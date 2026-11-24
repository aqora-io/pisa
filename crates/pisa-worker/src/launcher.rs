//! Starting REPL processes, optionally inside a bubblewrap sandbox and a
//! per-slot cgroup.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use pisa_protocol::repl::Message;
use pisa_repl::Repl;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::cgroup::{Cgroups, Peak};
use crate::config::{Sandbox, WorkerConfig};

pub struct Launcher {
    repl: PathBuf,
    lean: PathBuf,
    lake: PathBuf,
    /// `--lean-export`, if set; else the toolchain's own.
    lean_export: Option<PathBuf>,
    tool: PathBuf,
    project: Option<PathBuf>,
    /// Where the project's own code comes from, if it is configured.
    project_source: Option<crate::runs::ProjectSource>,
    /// The environment `lake env` sets up for the project (LEAN_PATH and friends),
    /// captured once so each spawn skips Lake.
    env: Option<Vec<(OsString, OsString)>>,
    /// The project's module search path, before any root of snapshot modules.
    base_path: Vec<PathBuf>,
    /// The project's module search path plus the snapshot cache and the
    /// `Pisa` library, for the REPL.
    lean_path: OsString,
    /// The same without the `Pisa` library, for building submitted code,
    /// which has no use for it, and re-checking it.
    build_path: OsString,
    sandbox: Sandbox,
    work_dir: PathBuf,
    snapshot_root: PathBuf,
    /// The pool version the worker serves.
    version: String,
    /// The project's toolchain's prefix, once [`toolchain_prefix`](Self::toolchain_prefix) found it.
    prefix: tokio::sync::OnceCell<PathBuf>,
    cgroups: Option<Cgroups>,
}

/// What `pisa build` reported for one module.
#[derive(Debug)]
pub struct Compiled {
    /// Lean's messages.
    pub messages: Vec<Message>,
    /// `None` when the time limit killed it.
    pub status: Option<std::process::ExitStatus>,
    pub stderr: String,
    /// OOM kills in the slot's cgroup while it ran.
    pub oom_kills: u64,
    /// The modules the written module imports (`PISA_IMPORTS`).
    pub imports: Option<Vec<String>>,
    /// Its declarations as the build describes them (`PISA_DECLS`), for code
    /// outside the module system: the JSON array the policy parses.
    pub report: Option<serde_json::Value>,
    /// Its declaration export (`PISA_EXPORT`).
    pub export: Option<serde_json::Value>,
}

impl Compiled {
    /// Parses `pisa build`'s output: Lean's messages as JSON lines, and its
    /// `PISA_*` lines, of which the last of each kind counts, as nothing the
    /// code prints comes after them.
    fn parse(stdout: &str) -> (Vec<Message>, [Option<serde_json::Value>; 3]) {
        let mut messages = Vec::new();
        let mut found: [Option<serde_json::Value>; 3] = [None, None, None];
        for line in stdout.lines() {
            let marked = ["PISA_IMPORTS ", "PISA_DECLS ", "PISA_EXPORT "]
                .iter()
                .position(|m| line.starts_with(m));
            match marked {
                Some(i) => {
                    let json = line.split_once(' ').map_or("", |(_, j)| j);
                    found[i] = serde_json::from_str(json).ok();
                }
                None => messages.extend(serde_json::from_str::<Message>(line).ok()),
            }
        }
        (messages, found)
    }
}

/// What a process run to its end printed, and how it exited.
#[derive(Debug)]
pub struct Ran {
    /// None when a signal killed it: its time limit, if `timed_out`.
    pub code: Option<i32>,
    /// Whether its time limit killed it.
    pub timed_out: bool,
    /// Its standard output, then its standard error.
    pub output: String,
}

/// The variables of the worker's environment that `lake` keeps when it runs
/// outside the sandbox. Credentials, such as the database's URL, aren't
/// among them.
const LAKE_ENV: [&str; 5] = ["PATH", "HOME", "ELAN_HOME", "LANG", "TMPDIR"];

/// The kernel re-check's verdict on one module, from a `PISA_VERIFY` line of
/// `pisa verify`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Verdict {
    /// The module, e.g. `PisaSnap.s0f…`.
    pub module: String,
    /// False when the kernel rejected a declaration.
    pub ok: bool,
    /// Why the kernel rejected it.
    #[serde(default)]
    pub reason: Option<String>,
    /// Its declarations as the verifier describes them: the JSON array the
    /// policy parses.
    #[serde(default)]
    pub declarations: serde_json::Value,
}

/// Why a re-check gave no verdict on a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfinished {
    /// It ran out of time, loading the imports or on the module.
    Timeout(Duration),
    /// The slot's memory limit killed it.
    OutOfMemory(Option<(&'static str, u64)>),
    /// It couldn't start, failed (`PISA_FAILED`), or ended without a
    /// verdict.
    Crash(String),
}

impl std::fmt::Display for Unfinished {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unfinished::Timeout(limit) => {
                write!(f, "the re-check took more than {}s", limit.as_secs())
            }
            Unfinished::OutOfMemory(cap) => write!(
                f,
                "the re-check exceeded its memory limit{}",
                crate::limits::cap_suffix(*cap)
            ),
            Unfinished::Crash(why) => write!(f, "the re-check failed: {why}"),
        }
    }
}

/// A run of `pisa verify` over modules, imports first, whose verdicts are
/// read as they come: see [`next`](Self::next). The process is killed when
/// the run is dropped.
pub struct VerifyRun<'a> {
    launcher: &'a Launcher,
    slot: usize,
    child: tokio::process::Child,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    /// The end of what it writes to stderr, read as it runs.
    stderr: Option<tokio::task::JoinHandle<String>>,
    oom_before: u64,
    /// The time left for the imports, until `PISA_IMPORTED`.
    imports: Option<Duration>,
    per_module: Duration,
    ended: bool,
}

impl VerifyRun<'_> {
    /// The verdict on the next module, an [`Unfinished`] if there is none,
    /// or `None` once the run has ended without fault. A rejection ends it.
    pub async fn next(&mut self) -> Option<Result<Verdict, Unfinished>> {
        if self.ended {
            return None;
        }
        let verdict = self.read().await;
        if !matches!(verdict, Some(Ok(Verdict { ok: true, .. }))) {
            self.ended = true;
        }
        verdict
    }

    async fn read(&mut self) -> Option<Result<Verdict, Unfinished>> {
        loop {
            let limit = self.imports.unwrap_or(Duration::ZERO) + self.per_module;
            let line = match tokio::time::timeout(limit, self.lines.next_line()).await {
                Err(_) => return Some(Err(Unfinished::Timeout(limit))),
                Ok(Err(e)) => return Some(Err(Unfinished::Crash(e.to_string()))),
                Ok(Ok(line)) => line,
            };
            let Some(line) = line else {
                let status = self.child.wait().await;
                if self.launcher.oom_kills(self.slot) > self.oom_before {
                    let cap = self.launcher.memory_max_mb(self.slot);
                    return Some(Err(Unfinished::OutOfMemory(cap)));
                }
                return match status {
                    Ok(s) if s.success() => None,
                    Ok(s) => {
                        let tail = self.stderr_tail().await;
                        Some(Err(Unfinished::Crash(with_tail(
                            format!("it exited with {s}"),
                            &tail,
                        ))))
                    }
                    Err(e) => Some(Err(Unfinished::Crash(e.to_string()))),
                };
            };
            if line.starts_with("PISA_IMPORTED ") {
                self.imports = None;
            } else if let Some(json) = line.strip_prefix("PISA_FAILED ") {
                let reason = serde_json::from_str::<serde_json::Value>(json)
                    .ok()
                    .and_then(|v| v["reason"].as_str().map(String::from))
                    .unwrap_or_else(|| format!("an unreadable failure: {json}"));
                return Some(Err(Unfinished::Crash(reason)));
            } else if let Some(json) = line.strip_prefix("PISA_VERIFY ") {
                return Some(
                    serde_json::from_str(json)
                        .map_err(|e| Unfinished::Crash(format!("an unreadable verdict: {e}"))),
                );
            }
        }
    }
}

impl VerifyRun<'_> {
    /// What the process wrote to stderr, at most [`STDERR_TAIL`] bytes of its
    /// end, once it has ended.
    async fn stderr_tail(&mut self) -> String {
        match self.stderr.take() {
            Some(read) => read.await.unwrap_or_default(),
            None => String::new(),
        }
    }
}

/// How much of a failed process's stderr its error keeps, from the end.
const STDERR_TAIL: usize = 2000;

/// Reads `stderr` to its end as the process runs, so that a full pipe never
/// stops it, and keeps the last [`STDERR_TAIL`] bytes.
fn read_tail(mut stderr: tokio::process::ChildStderr) -> tokio::task::JoinHandle<String> {
    use tokio::io::AsyncReadExt;
    tokio::spawn(async move {
        let mut kept: Vec<u8> = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            kept.extend_from_slice(&buf[..n]);
            if kept.len() > STDERR_TAIL {
                kept.drain(..kept.len() - STDERR_TAIL);
            }
        }
        String::from_utf8_lossy(&kept).trim().to_string()
    })
}

/// `message`, then what the process wrote to stderr, if anything.
fn with_tail(message: String, tail: &str) -> String {
    match tail.is_empty() {
        true => message,
        false => format!("{message}: {tail}"),
    }
}

/// Why [`Launcher::lean_export`] exported nothing.
#[derive(Debug)]
pub enum LeanExportError {
    /// The worker couldn't run the exporter or watch it run: nothing about
    /// the module.
    Worker(String),
    /// It ran past its time.
    Timeout,
    /// The slot's memory limit killed it.
    OutOfMemory,
    /// It exited with an error, as it does when the module lacks a target:
    /// how, and the end of what it wrote to stderr.
    Failed(String),
}

impl Launcher {
    pub async fn new(cfg: &WorkerConfig, worker_id: &str, version: &str) -> anyhow::Result<Self> {
        let env = match &cfg.project {
            Some(project) => Some(lake_env(project).await?),
            None => None,
        };
        let work_dir = cfg
            .work_dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join(format!("pisa-{worker_id}")));
        std::fs::create_dir_all(&work_dir)
            .with_context(|| format!("creating {}", work_dir.display()))?;
        // Snapshot modules live under `<root>/PisaSnap/`. Lean looks a module up
        // only in the first search-path root that has its top-level directory, so
        // this is the only root with one. It starts empty: modules are fetched
        // again from the store, so nothing stale survives a restart.
        let snapshot_root = work_dir.join("snapshots").join(version);
        let _ = std::fs::remove_dir_all(&snapshot_root);
        // What judgments left, which none will finish now; see `runs`.
        let _ = std::fs::remove_dir_all(work_dir.join("judging"));
        std::fs::create_dir_all(snapshot_root.join(pisa_protocol::snapshot::ROOT))
            .with_context(|| format!("creating {}", snapshot_root.display()))?;
        let base = env
            .as_ref()
            .and_then(|env| env.iter().find(|(k, _)| k == "LEAN_PATH"))
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var_os("LEAN_PATH"));
        let base_path: Vec<PathBuf> = base.iter().flat_map(std::env::split_paths).collect();
        let build_paths: Vec<PathBuf> = (base_path.iter().cloned())
            .chain([snapshot_root.clone()])
            .collect();
        let build_path = std::env::join_paths(&build_paths).context("building LEAN_PATH")?;
        let lean_path = std::env::join_paths(build_paths.iter().chain([&cfg.lean_lib]))
            .context("building LEAN_PATH")?;
        let cgroups = match &cfg.cgroup_root {
            None => None,
            Some(root) => {
                let memory = (cfg.memory_max_mb > 0).then_some(cfg.memory_max_mb * 1024 * 1024);
                let pids = (cfg.pids_max > 0).then_some(cfg.pids_max);
                let beam =
                    (cfg.beam_memory_max_mb > 0).then_some(cfg.beam_memory_max_mb * 1024 * 1024);
                // Asked for, the limits hold or the worker doesn't start:
                // without them, Lean would run with no memory or process
                // limit and an out-of-memory kill would read as a crash.
                let c = Cgroups::init(root, memory, pids).with_context(|| {
                    format!(
                        "setting up cgroups in {} (PISA_CGROUP_ROOT; unset it to run without \
                         per-process limits)",
                        root.display()
                    )
                })?;
                Some(c.with_beam(cfg.slots(), beam))
            }
        };
        if cgroups.is_none() {
            tracing::warn!(
                "PISA_CGROUP_ROOT is unset: Lean runs without memory or process limits, and an \
                 out-of-memory kill reads as a crash"
            );
        }
        // Comparator's kernels would each fail here, which pisa can only
        // tell from a kernel's rejection by what bwrap printed.
        if let Err(why) = probe_comparator_sandbox().await {
            tracing::warn!(
                "bwrap can't make comparator's sandbox, so acceptances can't run here \
                 (it needs unprivileged user namespaces and its own /proc): {why}"
            );
        }
        Ok(Launcher {
            repl: std::path::absolute(&cfg.repl).unwrap_or_else(|_| cfg.repl.clone()),
            // Usually a bare `lean`, found on the project environment's PATH.
            lean: cfg.lean.clone(),
            lake: cfg.lake.clone(),
            lean_export: cfg.lean_export.clone(),
            tool: cfg.tool.clone(),
            project: cfg.project.clone(),
            // An empty setting, as a container's environment can pass, is none.
            project_source: (cfg.project_git.clone().filter(|g| !g.is_empty()))
                .zip(cfg.project_rev.clone().filter(|r| !r.is_empty()))
                .map(|(git, rev)| crate::runs::ProjectSource { git, rev }),
            env,
            base_path,
            lean_path,
            build_path,
            sandbox: cfg.sandbox,
            work_dir,
            snapshot_root,
            version: version.to_string(),
            prefix: tokio::sync::OnceCell::new(),
            cgroups,
        })
    }

    /// The toolchain the project pins and the packages its manifest pins,
    /// which the worker registers for `/health`; empty without a project.
    pub fn project_pins(&self) -> (String, Vec<pisa_protocol::api::PoolPackage>) {
        match &self.project {
            Some(project) => pisa_protocol::version::project_pins(project),
            None => (String::new(), Vec::new()),
        }
    }

    /// The project's files that define its environment, which the worker
    /// records as it registers; none without a project.
    pub fn project_files(&self) -> BTreeMap<String, String> {
        match &self.project {
            Some(project) => pisa_protocol::version::project_files(project),
            None => BTreeMap::new(),
        }
    }

    /// The setting that limits the slot's memory (`PISA_MEMORY_MAX_MB`, or
    /// `PISA_BEAM_MEMORY_MAX_MB` for a Beam slot) and its value in MiB, `None`
    /// without a limit.
    pub fn memory_max_mb(&self, slot: usize) -> Option<(&'static str, u64)> {
        self.cgroups.as_ref()?.memory_max_mb(slot)
    }

    /// What the memory limits of `slots` REPL slots and `beam_slots` Beam
    /// slots add up to in MiB; `None` if Lean runs without limits, or any of
    /// those slots has none.
    pub fn memory_caps_mb(&self, slots: usize, beam_slots: usize) -> Option<u64> {
        self.cgroups.as_ref()?.memory_caps_mb(slots, beam_slots)
    }

    /// Whether Lean runs with the memory and process limits of cgroups.
    pub fn limited(&self) -> bool {
        self.cgroups.is_some()
    }

    /// The Lake project the REPL imports from, if there is one.
    pub fn project(&self) -> Option<&Path> {
        self.project.as_deref()
    }

    /// Where the project's own code comes from, if it is configured.
    pub fn project_source(&self) -> Option<&crate::runs::ProjectSource> {
        self.project_source.as_ref()
    }

    /// Where acceptance judges runs, each judgment in a private directory of
    /// its own: emptied when the worker starts.
    pub fn judging_dir(&self) -> PathBuf {
        self.work_dir.join("judging")
    }

    /// Where acceptance keeps the exports of modules for comparator, which
    /// outlive the worker.
    pub fn exports_dir(&self) -> PathBuf {
        self.work_dir.join("exports")
    }

    /// The pool version the worker serves.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// A private writable directory for the slot, visible inside the sandbox
    /// at the same path.
    pub fn slot_dir(&self, slot: usize) -> PathBuf {
        self.work_dir.join(format!("slot-{slot}"))
    }

    /// The search-path root of snapshot modules, read-only in the sandbox.
    pub fn snapshot_root(&self) -> &Path {
        &self.snapshot_root
    }

    pub fn spawn(&self, slot: usize) -> std::io::Result<Repl> {
        Repl::spawn(self.command(slot, &self.repl, &[])?)
    }

    /// `program` set up like a REPL for `slot`: same environment, sandbox and
    /// cgroup.
    pub fn command(&self, slot: usize, program: &Path, args: &[&str]) -> std::io::Result<Command> {
        self.command_on(slot, program, args, &self.lean_path)
    }

    /// [`command`](Self::command) with `lean_path` as LEAN_PATH.
    fn command_on(
        &self,
        slot: usize,
        program: &Path,
        args: &[&str],
        lean_path: &OsString,
    ) -> std::io::Result<Command> {
        let dir = self.slot_dir(slot);
        std::fs::create_dir_all(&dir)?;
        let cwd = self.project.clone().unwrap_or_else(|| dir.clone());
        let place = Place {
            cwd: &cwd,
            writable: Some(&dir),
            read_only: &[&self.snapshot_root],
        };
        self.command_at(slot, program, args, lean_path, &place)
    }

    /// `program` with `args`, set up like a REPL for `slot` but in `place`.
    fn command_at(
        &self,
        slot: usize,
        program: &Path,
        args: &[&str],
        lean_path: &OsString,
        place: &Place,
    ) -> std::io::Result<Command> {
        let mut cmd = match self.sandbox {
            Sandbox::None => {
                let mut cmd = Command::new(program);
                cmd.current_dir(place.cwd);
                cmd
            }
            Sandbox::Bwrap => {
                let mut cmd = Command::new("bwrap");
                cmd.args(bwrap_args(place)).arg("--").arg(program);
                cmd
            }
        };
        cmd.args(args);
        if let Some(env) = &self.env {
            cmd.env_clear().envs(env.iter().cloned());
        }
        cmd.env("LEAN_PATH", lean_path);
        if let Some(cg) = &self.cgroups {
            cg.apply(slot, &mut cmd)?;
        }
        Ok(cmd)
    }

    /// Lean's own messages for a header, from `lean --stdin --json`. The REPL
    /// reports nothing when imports fail to load together; plain Lean does.
    /// Returns no messages if Lean can't be run.
    pub async fn diagnose_header(
        &self,
        slot: usize,
        header: &str,
        timeout: Duration,
    ) -> Vec<Message> {
        let run = async {
            let mut cmd = self.command(slot, &self.lean, &["--stdin", "--json"])?;
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let mut child = cmd.spawn()?;
            let mut stdin = child.stdin.take().expect("piped stdin");
            stdin.write_all(header.as_bytes()).await?;
            drop(stdin);
            child.wait_with_output().await
        };
        let output = match tokio::time::timeout(timeout, run).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                tracing::warn!("running lean to diagnose a header failed: {e}");
                return Vec::new();
            }
            Err(_) => return Vec::new(),
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Compiles `file` to the module `olean`, named by its path below `root`,
    /// with `pisa build`, set up like the slot's REPL (same environment,
    /// sandbox and cgroup), and has it describe the module.
    pub async fn compile(
        &self,
        slot: usize,
        root: &Path,
        file: &Path,
        olean: &Path,
        timeout: Duration,
    ) -> std::io::Result<Compiled> {
        let before = self.oom_kills(slot);
        let args = compile_args(root, file, olean);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut cmd = self.command_on(slot, &self.tool, &args, &self.build_path)?;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = cmd.spawn()?;
        let output = tokio::time::timeout(timeout, child.wait_with_output()).await;
        let oom_kills = self.oom_kills(slot).saturating_sub(before);
        let Ok(output) = output else {
            return Ok(Compiled {
                messages: Vec::new(),
                status: None,
                stderr: String::new(),
                oom_kills,
                imports: None,
                report: None,
                export: None,
            });
        };
        let output = output?;
        let (messages, [imports, report, export]) =
            Compiled::parse(&String::from_utf8_lossy(&output.stdout));
        Ok(Compiled {
            messages,
            status: Some(output.status),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            oom_kills,
            imports: imports.and_then(|i| serde_json::from_value(i).ok()),
            report,
            export,
        })
    }

    /// Starts `pisa verify` on `modules`, imports first, in the slot's
    /// sandbox and cgroup. It has `imports` to load what they import, and
    /// then `per_module` for each module.
    pub fn verify(
        &self,
        slot: usize,
        modules: &[String],
        imports: Duration,
        per_module: Duration,
    ) -> std::io::Result<VerifyRun<'_>> {
        use tokio::io::AsyncBufReadExt;
        let oom_before = self.oom_kills(slot);
        let mut args = vec!["verify"];
        args.extend(modules.iter().map(String::as_str));
        let mut cmd = self.command_on(slot, &self.tool, &args, &self.build_path)?;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().map(read_tail);
        Ok(VerifyRun {
            launcher: self,
            slot,
            child,
            lines: tokio::io::BufReader::new(stdout).lines(),
            stderr,
            oom_before,
            imports: Some(imports),
            per_module,
            ended: false,
        })
    }

    /// The prefix of the toolchain that builds the project, where Lean's
    /// bundled exporter and kernels are, as `lean --print-prefix` in the
    /// project says. It is found once.
    pub async fn toolchain_prefix(&self) -> std::io::Result<PathBuf> {
        let found = self.prefix.get_or_try_init(|| async {
            // `lean` resolves through elan by the project's `lean-toolchain`,
            // as `lake` does.
            let dir = self.project.as_deref().unwrap_or(&self.work_dir);
            let mut cmd = Command::new(&self.lean);
            cmd.arg("--print-prefix")
                .current_dir(dir)
                .env_clear()
                .stdin(Stdio::null());
            for key in LAKE_ENV {
                if let Some(value) = std::env::var_os(key) {
                    cmd.env(key, value);
                }
            }
            let out = cmd.output().await?;
            let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !out.status.success() || prefix.is_empty() {
                return Err(std::io::Error::other(format!(
                    "`lean --print-prefix` gave no prefix: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            Ok(PathBuf::from(prefix))
        });
        found.await.cloned()
    }

    /// The exporter acceptance runs: `--lean-export`, or the toolchain's own
    /// `leanexport`, which writes the export format of the compiler that
    /// built the modules.
    pub async fn lean_exporter(&self) -> std::io::Result<PathBuf> {
        match &self.lean_export {
            Some(exporter) => Ok(exporter.clone()),
            None => Ok(self.toolchain_prefix().await?.join("bin/leanexport")),
        }
    }

    /// Exports `targets` of `module`, a snapshot module under `root`, to
    /// `output`, with Lean's bundled exporter, in the slot's sandbox and
    /// cgroup, for at most `timeout`. `root` takes the place of the snapshot
    /// cache on its search path: Lean looks a module up only in the first
    /// root that has its top-level directory. The exporter can write nothing
    /// but its output, and has `LEAN_SYSROOT`, so that it never runs a
    /// `lean` it finds to ask for the toolchain, and `LEAN_ABORT_ON_PANIC`,
    /// so that a target it can't find fails it instead of leaving it out.
    pub async fn lean_export(
        &self,
        slot: usize,
        root: &Path,
        module: &str,
        targets: &[String],
        output: std::fs::File,
        timeout: Duration,
    ) -> Result<(), LeanExportError> {
        let worker = |e: std::io::Error| LeanExportError::Worker(e.to_string());
        let prefix = self.toolchain_prefix().await.map_err(worker)?;
        let exporter = self.lean_exporter().await.map_err(worker)?;
        let before = self.oom_kills(slot);
        let mut cmd = self
            .lean_export_command(slot, root, (module, targets), &prefix, &exporter)
            .map_err(worker)?;
        cmd.stdin(Stdio::null())
            .stdout(output)
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(worker)?;
        drop(cmd);
        let stderr = child.stderr.take().map(read_tail);
        let status = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(status) => status.map_err(worker)?,
            Err(_) => {
                let _ = child.kill().await;
                self.wait_idle(slot).await;
                return Err(LeanExportError::Timeout);
            }
        };
        if self.oom_kills(slot) > before {
            return Err(LeanExportError::OutOfMemory);
        }
        if status.success() {
            return Ok(());
        }
        let tail = match stderr {
            Some(read) => read.await.unwrap_or_default(),
            None => String::new(),
        };
        // A panic's backtrace says nothing of the module.
        let tail = tail.split("\nbacktrace:").next().unwrap_or_default();
        Err(LeanExportError::Failed(with_tail(
            format!("it exited with {status}"),
            tail,
        )))
    }

    /// The command of [`lean_export`](Self::lean_export): `exporter` on
    /// `export`, a module and its targets, with `prefix` as the sysroot.
    fn lean_export_command(
        &self,
        slot: usize,
        root: &Path,
        export: (&str, &[String]),
        prefix: &Path,
        exporter: &Path,
    ) -> std::io::Result<Command> {
        let lean_path =
            std::env::join_paths(self.base_path.iter().map(PathBuf::as_path).chain([root]))
                .map_err(std::io::Error::other)?;
        let mut args = vec![export.0, "--"];
        args.extend(export.1.iter().map(String::as_str));
        let place = Place {
            cwd: self.project.as_deref().unwrap_or(root),
            writable: None,
            read_only: &[root],
        };
        let mut cmd = self.command_at(slot, exporter, &args, &lean_path, &place)?;
        cmd.env("LEAN_SYSROOT", prefix)
            .env("LEAN_ABORT_ON_PANIC", "1");
        Ok(cmd)
    }

    /// Runs `lake` with `args` in `dir`, in `slot`'s cgroup but outside the
    /// sandbox, for at most `timeout`, after which it is killed, `timed_out`:
    /// `lake comparator` sandboxes what it runs itself. Its stdout and stderr
    /// share one pipe, so that the output keeps the order they were written
    /// in.
    pub async fn lake(
        &self,
        slot: usize,
        args: &[&str],
        dir: &Path,
        timeout: Duration,
    ) -> std::io::Result<Ran> {
        let (reader, writer) = std::io::pipe()?;
        let mut cmd = Command::new(&self.lake);
        cmd.args(args)
            .current_dir(dir)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(writer.try_clone()?)
            .stderr(writer)
            .kill_on_drop(true);
        for key in LAKE_ENV {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        if let Some(cg) = &self.cgroups {
            cg.apply(slot, &mut cmd)?;
        }
        let mut child = cmd.spawn()?;
        // The command holds the pipe's writing ends until it is dropped.
        drop(cmd);
        // Read as it runs, so that what it printed before a timeout stays.
        let output = read_all(Some(tokio::net::unix::pipe::Receiver::from_owned_fd(
            reader.into(),
        )?));
        let (code, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(status) => (status?.code(), false),
            Err(_) => {
                let _ = child.kill().await;
                (None, true)
            }
        };
        // A process Lake started may outlive it and keep the pipes open, so
        // take what was read by then.
        let output = String::from_utf8_lossy(&output.finish().await).into_owned();
        self.wait_idle(slot).await;
        Ok(Ran {
            code,
            timed_out,
            output,
        })
    }

    /// Waits (up to two seconds) until no process is left in the slot's
    /// cgroup, e.g. a sandboxed REPL that outlives the killed bwrap briefly.
    pub async fn wait_idle(&self, slot: usize) {
        let Some(cg) = &self.cgroups else { return };
        for _ in 0..40 {
            if cg.is_empty(slot) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tracing::warn!(slot, "processes linger in the slot's cgroup");
    }

    /// Kills what is left in the slot's cgroup, and waits for it to go
    /// ([`wait_idle`](Self::wait_idle)); nothing without cgroups.
    pub async fn kill(&self, slot: usize) {
        if let Some(cg) = &self.cgroups {
            cg.kill(slot);
            self.wait_idle(slot).await;
        }
    }

    /// The executables of the processes in the slot's cgroup, of those the
    /// worker may see; none without cgroups.
    pub fn slot_executables(&self, slot: usize) -> Vec<PathBuf> {
        let Some(cg) = &self.cgroups else {
            return Vec::new();
        };
        (cg.procs(slot).into_iter())
            .filter_map(|pid| std::fs::read_link(format!("/proc/{pid}/exe")).ok())
            .collect()
    }

    /// The slot's peak memory from now on (see [`Peak`]); `None` without
    /// cgroups, or a kernel that can't reset it.
    pub fn peak(&self, slot: usize) -> Option<Peak> {
        self.cgroups.as_ref()?.peak(slot)
    }

    /// OOM kills so far in the slot's cgroup (0 without cgroups).
    pub fn oom_kills(&self, slot: usize) -> u64 {
        self.cgroups.as_ref().map_or(0, |c| c.oom_kills(slot))
    }

    /// Memory charged to the slot: its cgroup if there is one, else the RSS of
    /// the process tree.
    pub fn memory(&self, slot: usize, repl: &Repl) -> u64 {
        self.cgroups
            .as_ref()
            .and_then(|c| c.memory_current(slot))
            .or_else(|| repl.pid().map(pisa_repl::proc::tree_rss_bytes))
            .unwrap_or(0)
    }
}

/// The `pisa` arguments that compile `file` to `olean`, a module named by its
/// path below `root`. `-D` sets `autoImplicit` off for the whole file, as the
/// header command does in the REPL; code can turn it back on with
/// `set_option`. Definitions' values are exported up to
/// [`VALUE_CHARS`](crate::catalog::VALUE_CHARS) characters.
pub fn compile_args(root: &Path, file: &Path, olean: &Path) -> Vec<String> {
    let path = |p: &Path| p.to_string_lossy().into_owned();
    vec![
        "build".into(),
        "-R".into(),
        path(root),
        "-o".into(),
        path(olean),
        "-D".into(),
        "autoImplicit=false".into(),
        "--value-chars".into(),
        crate::catalog::VALUE_CHARS.to_string(),
        path(file),
    ]
}

/// Where a process runs, and what of the worker's files it may write and
/// read besides a read-only `/`.
struct Place<'a> {
    cwd: &'a Path,
    /// The one directory it may write, the slot's for Lean; none for the
    /// exporter.
    writable: Option<&'a Path>,
    /// Bound read-only after the sandbox's fresh `/tmp`, which may cover
    /// them: the snapshot cache, or the modules of a judgment.
    read_only: &'a [&'a Path],
}

/// The sandbox `lake comparator` runs each kernel in, as Lake's
/// `Lake/CLI/Check.lean` builds it, around `true`. When bwrap can't make it,
/// it exits 1, which Lake reports as the kernel rejecting the solution.
const COMPARATOR_SANDBOX: [&str; 20] = [
    "--ro-bind",
    "/",
    "/",
    "--tmpfs",
    "/home",
    "--tmpfs",
    "/root",
    "--tmpfs",
    "/run/user",
    "--tmpfs",
    "/tmp",
    "--dev",
    "/dev",
    "--proc",
    "/proc",
    "--clearenv",
    "--unshare-all",
    "--die-with-parent",
    "--new-session",
    "true",
];

/// Runs `bwrap` from `PATH`, which `lake comparator` runs too, with
/// [`COMPARATOR_SANDBOX`]: `Err` with what it said if it fails.
async fn probe_comparator_sandbox() -> Result<(), String> {
    let out = Command::new("bwrap")
        .args(COMPARATOR_SANDBOX)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("running bwrap: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(match stderr.lines().find(|l| !l.trim().is_empty()) {
        Some(line) => line.trim().to_string(),
        None => format!("bwrap exited with {}", out.status),
    })
}

/// No network, no view of other processes, a read-only filesystem except
/// for `place`'s writable directory, if it has one, and a fresh /tmp. Its
/// read-only paths are bound after the tmpfs, since they usually live under
/// /tmp.
fn bwrap_args(place: &Place) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    if let Some(dir) = place.writable {
        args.extend(["--bind".into(), dir.into(), dir.into()]);
    }
    for path in place.read_only {
        args.extend(["--ro-bind".into(), path.into(), path.into()]);
    }
    args.extend(["--chdir".into(), place.cwd.into()]);
    args
}

async fn lake_env(project: &Path) -> anyhow::Result<Vec<(OsString, OsString)>> {
    let out = Command::new("lake")
        .args(["env", "env", "-0"])
        .current_dir(project)
        .output()
        .await
        .context("running `lake env`")?;
    anyhow::ensure!(
        out.status.success(),
        "`lake env` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(parse_env0(&out.stdout))
}

fn parse_env0(bytes: &[u8]) -> Vec<(OsString, OsString)> {
    use std::os::unix::ffi::OsStrExt;
    bytes
        .split(|b| *b == 0)
        .filter_map(|kv| {
            let eq = kv.iter().position(|b| *b == b'=')?;
            Some((
                std::ffi::OsStr::from_bytes(&kv[..eq]).to_owned(),
                std::ffi::OsStr::from_bytes(&kv[eq + 1..]).to_owned(),
            ))
        })
        .collect()
}

/// A pipe read in a task of its own, into a buffer that keeps what it read.
struct Reading {
    task: tokio::task::JoinHandle<()>,
    read: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl Reading {
    /// What was read: all of it once the pipe closes, or what was read
    /// two seconds after it is asked for if it doesn't.
    async fn finish(self) -> Vec<u8> {
        // On a timeout the task is left to end with the pipe.
        let _ = tokio::time::timeout(Duration::from_secs(2), self.task).await;
        std::mem::take(&mut *self.read.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Reads `pipe` in a task of its own.
fn read_all<R>(pipe: Option<R>) -> Reading
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let read = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let into = read.clone();
    let task = tokio::spawn(async move {
        let Some(mut pipe) = pipe else { return };
        let mut chunk = [0u8; 8192];
        loop {
            match tokio::io::AsyncReadExt::read(&mut pipe, &mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => into
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend_from_slice(&chunk[..n]),
            }
        }
    });
    Reading { task, read }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn compile_turns_auto_implicit_off_before_the_file() {
        let args = super::compile_args(
            Path::new("/root"),
            Path::new("/root/s.lean"),
            Path::new("/root/s.olean"),
        );
        assert_eq!(
            args,
            [
                "build",
                "-R",
                "/root",
                "-o",
                "/root/s.olean",
                "-D",
                "autoImplicit=false",
                "--value-chars",
                "4000",
                "/root/s.lean"
            ]
        );
    }

    #[test]
    fn a_builds_last_marker_lines_count() {
        let stdout = "{\"severity\":\"warning\",\"pos\":{\"line\":1,\"column\":0},\"data\":\"w\"}\n\
                      PISA_DECLS [1]\nPISA_IMPORTS [\"Init\"]\nPISA_DECLS [2]\nPISA_EXPORT oops\n";
        let (messages, [imports, report, export]) = super::Compiled::parse(stdout);
        assert_eq!(messages.len(), 1);
        assert_eq!(imports, Some(serde_json::json!(["Init"])));
        assert_eq!(report, Some(serde_json::json!([2])));
        assert_eq!(export, None);
    }

    /// The exporter runs in the slot's sandbox with nothing of the worker's
    /// writable, the judgment's modules in the snapshot cache's place on its
    /// search path, a sysroot, so that it never asks a `lean` it finds for
    /// one, and aborting on a panic, so that a missing target fails it.
    #[tokio::test]
    async fn the_exporter_runs_with_sysroot_and_abort_on_panic_and_no_writable_path() {
        use clap::Parser;
        use std::path::PathBuf;
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().to_str().unwrap().to_string();
        let args = [
            "pisa-worker",
            "--database-url",
            "x",
            "--sandbox",
            "bwrap",
            "--work-dir",
            &work,
        ];
        let cfg = crate::config::WorkerConfig::try_parse_from(args).unwrap();
        let launcher = super::Launcher::new(&cfg, "w", "v").await.unwrap();
        let root = launcher.judging_dir().join("j");
        let targets = ["t".to_string(), "Nat".to_string()];
        let cmd = launcher
            .lean_export_command(
                0,
                &root,
                ("PisaSnap.sa", &targets),
                Path::new("/tc"),
                Path::new("/tc/bin/leanexport"),
            )
            .unwrap();
        let cmd = cmd.as_std();
        assert_eq!(cmd.get_program(), "bwrap");
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        let (sandbox, run) = args.split_at(args.iter().position(|a| *a == "--").unwrap());
        assert_eq!(
            run,
            ["--", "/tc/bin/leanexport", "PisaSnap.sa", "--", "t", "Nat"]
        );
        assert!(!sandbox.contains(&"--bind"), "{sandbox:?}");
        let root = root.to_str().unwrap();
        for bound in [["--ro-bind", "/", "/"], ["--ro-bind", root, root]] {
            assert!(sandbox.windows(3).any(|w| w == bound), "{sandbox:?}");
        }
        assert!(sandbox.windows(2).any(|w| w == ["--tmpfs", "/tmp"]));
        let env = |key: &str| {
            let value = cmd.get_envs().find(|(k, _)| *k == key).and_then(|(_, v)| v);
            value.map(|v| v.to_str().unwrap().to_string())
        };
        assert_eq!(env("LEAN_SYSROOT").as_deref(), Some("/tc"));
        assert_eq!(env("LEAN_ABORT_ON_PANIC").as_deref(), Some("1"));
        let roots: Vec<PathBuf> = std::env::split_paths(&env("LEAN_PATH").unwrap()).collect();
        assert_eq!(roots.last(), Some(&PathBuf::from(root)), "{roots:?}");
        assert!(
            !roots.iter().any(|r| r == launcher.snapshot_root()),
            "{roots:?}"
        );
    }

    /// A `lake` its time limit killed is told from one a signal of its own
    /// killed, which has no code either.
    #[tokio::test]
    async fn lake_says_whether_its_time_limit_killed_it() {
        use clap::Parser;
        let dir = tempfile::tempdir().unwrap();
        let lake = dir.path().join("lake");
        std::fs::write(
            &lake,
            "#!/bin/sh\ncase $1 in\n  crash) kill -SEGV $$ ;;\n  slow) sleep 10 ;;\nesac\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&lake).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&lake, perms).unwrap();
        let work = dir.path().join("work");
        let args = [
            "pisa-worker",
            "--database-url",
            "x",
            "--lake",
            lake.to_str().unwrap(),
            "--work-dir",
            work.to_str().unwrap(),
        ];
        let cfg = crate::config::WorkerConfig::try_parse_from(args).unwrap();
        let launcher = super::Launcher::new(&cfg, "w", "v").await.unwrap();
        let run = |what: &'static str, timeout: Duration| {
            let launcher = &launcher;
            let dir = dir.path();
            async move { launcher.lake(0, &[what], dir, timeout).await.unwrap() }
        };
        let crashed = run("crash", Duration::from_secs(10)).await;
        assert_eq!((crashed.code, crashed.timed_out), (None, false));
        let slow = run("slow", Duration::from_millis(200)).await;
        assert_eq!((slow.code, slow.timed_out), (None, true));
        let done = run("done", Duration::from_secs(10)).await;
        assert_eq!((done.code, done.timed_out), (Some(0), false));
    }

    #[test]
    fn parses_nul_separated_env() {
        let env = super::parse_env0(b"LEAN_PATH=/a:/b\0X=y=z\0");
        assert_eq!(env.len(), 2);
        assert_eq!(env[0].0, "LEAN_PATH");
        assert_eq!(env[1].1, "y=z");
    }
}
