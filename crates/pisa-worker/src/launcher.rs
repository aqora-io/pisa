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
    verifier: PathBuf,
    project: Option<PathBuf>,
    /// The environment `lake env` sets up for the project (LEAN_PATH and friends),
    /// captured once so each spawn skips Lake.
    env: Option<Vec<(OsString, OsString)>>,
    /// The project's module search path plus the snapshot cache and the
    /// `Pisa` library, for the REPL and the verifier.
    lean_path: OsString,
    /// The same without the `Pisa` library, for building submitted code,
    /// which has no use for it.
    build_path: OsString,
    sandbox: Sandbox,
    work_dir: PathBuf,
    snapshot_root: PathBuf,
    cgroups: Option<Cgroups>,
}

/// What `lean -o` reported for one module.
#[derive(Debug)]
pub struct Compiled {
    pub messages: Vec<Message>,
    /// `None` when the time limit killed it.
    pub status: Option<std::process::ExitStatus>,
    pub stderr: String,
    /// OOM kills in the slot's cgroup while it ran.
    pub oom_kills: u64,
}

/// The snapshot verifier's `PISA_VERIFY` verdict. `ok` is false when the
/// kernel rejected a declaration; `report` carries its axiom report (the
/// JSON array the policy already parses).
#[derive(Debug, serde::Deserialize)]
pub struct Verified {
    pub ok: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub declarations: serde_json::Value,
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
                match Cgroups::init(root, memory, pids) {
                    Ok(c) => Some(c.with_beam(cfg.slots(), beam)),
                    Err(e) => {
                        tracing::warn!(root = %root.display(), "cgroups unavailable, running without per-process limits: {e}");
                        None
                    }
                }
            }
        };
        Ok(Launcher {
            repl: std::path::absolute(&cfg.repl).unwrap_or_else(|_| cfg.repl.clone()),
            // Usually a bare `lean`, found on the project environment's PATH.
            lean: cfg.lean.clone(),
            verifier: cfg.verifier.clone(),
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
    /// set up like the slot's REPL (same environment, sandbox and cgroup).
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
        let mut cmd = self.command_on(slot, &self.lean, &args, &self.build_path)?;
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
            });
        };
        let output = output?;
        Ok(Compiled {
            messages: String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect(),
            status: Some(output.status),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            oom_kills,
        })
    }

    /// The `.olean` files of the modules `file` imports, as Lean's own header
    /// parser reads them (`lean --deps`), set up like
    /// [`compile`](Self::compile). The error is Lean's, or why it didn't run.
    pub async fn deps(
        &self,
        slot: usize,
        file: &Path,
        timeout: Duration,
    ) -> Result<Vec<PathBuf>, String> {
        let file = file.to_string_lossy().into_owned();
        let args = ["--deps", file.as_str()];
        let run = async {
            let mut cmd = self.command_on(slot, &self.lean, &args, &self.build_path)?;
            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            cmd.spawn()?.wait_with_output().await
        };
        let output = match tokio::time::timeout(timeout, run).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(format!("running Lean failed: {e}")),
            Err(_) => return Err("Lean took too long to read the imports".into()),
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("{} {}", stdout.trim(), stderr.trim())
                .trim()
                .to_string());
        }
        Ok(stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(PathBuf::from)
            .collect())
    }

    /// Re-verifies snapshot `module` through the kernel with `lean --run`, in
    /// the slot's sandbox and cgroup. A timeout or a missing `PISA_VERIFY`
    /// line is reported as `ok: false`.
    pub async fn verify(&self, slot: usize, module: &str, timeout: Duration) -> Verified {
        let not_ok = |reason: String| Verified {
            ok: false,
            reason: Some(reason),
            declarations: serde_json::Value::Null,
        };
        let verifier = self.verifier.to_string_lossy().into_owned();
        let args = ["--run", &verifier, module];
        let run = async {
            let mut cmd = self.command(slot, &self.lean, &args)?;
            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            cmd.spawn()?.wait_with_output().await
        };
        let output = match tokio::time::timeout(timeout, run).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return not_ok(format!("running the verifier failed: {e}")),
            Err(_) => return not_ok("the verifier timed out".into()),
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some(line) = stdout.lines().find_map(|l| l.strip_prefix("PISA_VERIFY ")) else {
            return not_ok("the verifier produced no verdict".into());
        };
        match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => not_ok(format!("unreadable verifier verdict: {e}")),
        }
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

/// The `lean` arguments that compile `file` to `olean`, a module named by its
/// path below `root`. `-D` sets `autoImplicit` off for the whole file, as the
/// header command does in the REPL; code can turn it back on with `set_option`.
pub fn compile_args(root: &Path, file: &Path, olean: &Path) -> Vec<String> {
    let path = |p: &Path| p.to_string_lossy().into_owned();
    vec![
        "-R".into(),
        path(root),
        "-o".into(),
        path(olean),
        "-DautoImplicit=false".into(),
        "--json".into(),
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
                "-R",
                "/root",
                "-o",
                "/root/s.olean",
                "-DautoImplicit=false",
                "--json",
                "/root/s.lean"
            ]
        );
    }

    #[test]
    fn parses_nul_separated_env() {
        let env = super::parse_env0(b"LEAN_PATH=/a:/b\0X=y=z\0");
        assert_eq!(env.len(), 2);
        assert_eq!(env[0].0, "LEAN_PATH");
        assert_eq!(env[1].1, "y=z");
    }
}
