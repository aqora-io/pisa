//! Starting REPL processes, optionally inside a bubblewrap sandbox and a
//! per-slot cgroup.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use pisa_protocol::repl::Message;
use pisa_repl::Repl;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::cgroup::Cgroups;
use crate::config::{Sandbox, WorkerConfig};

pub struct Launcher {
    repl: PathBuf,
    lean: PathBuf,
    lake: PathBuf,
    tool: PathBuf,
    project: Option<PathBuf>,
    /// The environment `lake env` sets up for the project (LEAN_PATH and friends),
    /// captured once so each spawn skips Lake.
    env: Option<Vec<(OsString, OsString)>>,
    /// The project's module search path plus the snapshot cache and the
    /// `Pisa` library, for the REPL.
    lean_path: OsString,
    /// The same without the `Pisa` library, for building submitted code,
    /// which has no use for it, and re-checking it.
    build_path: OsString,
    sandbox: Sandbox,
    work_dir: PathBuf,
    snapshot_root: PathBuf,
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
    /// None when its time limit killed it.
    pub code: Option<i32>,
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
    OutOfMemory,
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
            Unfinished::OutOfMemory => write!(f, "the re-check exceeded its memory limit"),
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
                    return Some(Err(Unfinished::OutOfMemory));
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
        // Runs comparator judged, kept for the next judgment of the same run;
        // see `runs`.
        let _ = std::fs::remove_dir_all(work_dir.join("runs"));
        std::fs::create_dir_all(snapshot_root.join(pisa_protocol::snapshot::ROOT))
            .with_context(|| format!("creating {}", snapshot_root.display()))?;
        let base = env
            .as_ref()
            .and_then(|env| env.iter().find(|(k, _)| k == "LEAN_PATH"))
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var_os("LEAN_PATH"));
        let build_paths: Vec<PathBuf> = base
            .iter()
            .flat_map(std::env::split_paths)
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
        Ok(Launcher {
            repl: std::path::absolute(&cfg.repl).unwrap_or_else(|_| cfg.repl.clone()),
            // Usually a bare `lean`, found on the project environment's PATH.
            lean: cfg.lean.clone(),
            lake: cfg.lake.clone(),
            tool: cfg.tool.clone(),
            project: cfg.project.clone(),
            env,
            lean_path,
            build_path,
            sandbox: cfg.sandbox,
            work_dir,
            snapshot_root,
            cgroups,
        })
    }

    /// The Lake project the REPL imports from, if there is one.
    pub fn project(&self) -> Option<&Path> {
        self.project.as_deref()
    }

    /// Where runs exported for comparator are kept between judgments, each
    /// built once.
    pub fn runs_dir(&self) -> PathBuf {
        self.work_dir.join("runs")
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
        let mut cmd = match self.sandbox {
            Sandbox::None => {
                let mut cmd = Command::new(program);
                cmd.current_dir(&cwd);
                cmd
            }
            Sandbox::Bwrap => {
                let mut cmd = Command::new("bwrap");
                cmd.args(bwrap_args(&dir, &self.snapshot_root, &cwd))
                    .arg("--")
                    .arg(program);
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

    /// Runs `lake` with `args` in `dir`, in `slot`'s cgroup but outside the
    /// sandbox, for at most `timeout`: `lake comparator` sandboxes the code it
    /// judges itself.
    pub async fn lake(
        &self,
        slot: usize,
        args: &[&str],
        dir: &Path,
        timeout: Duration,
    ) -> std::io::Result<Ran> {
        let mut cmd = Command::new(&self.lake);
        cmd.args(args)
            .current_dir(dir)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for key in LAKE_ENV {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        if let Some(cg) = &self.cgroups {
            cg.apply(slot, &mut cmd)?;
        }
        let child = cmd.spawn()?;
        let ran = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(out) => {
                let out = out?;
                let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
                output.push_str(&String::from_utf8_lossy(&out.stderr));
                Ran {
                    code: out.status.code(),
                    output,
                }
            }
            Err(_) => Ran {
                code: None,
                output: format!("lake ran for more than {}s", timeout.as_secs()),
            },
        };
        self.wait_idle(slot).await;
        Ok(ran)
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

/// No network, no view of other processes, a read-only filesystem except
/// for the slot directory, and a fresh /tmp. The snapshot cache is bound
/// read-only after the tmpfs, since it usually lives under /tmp.
fn bwrap_args(slot_dir: &Path, snapshot_root: &Path, cwd: &Path) -> Vec<OsString> {
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
    args.extend(["--bind".into(), slot_dir.into(), slot_dir.into()]);
    args.extend([
        "--ro-bind".into(),
        snapshot_root.into(),
        snapshot_root.into(),
    ]);
    args.extend(["--chdir".into(), cwd.into()]);
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

#[cfg(test)]
mod tests {
    use std::path::Path;

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

    #[test]
    fn parses_nul_separated_env() {
        let env = super::parse_env0(b"LEAN_PATH=/a:/b\0X=y=z\0");
        assert_eq!(env.len(), 2);
        assert_eq!(env[0].0, "LEAN_PATH");
        assert_eq!(env[1].1, "y=z");
    }
}
