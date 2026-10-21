//! Starting REPL processes, optionally inside a bubblewrap sandbox and a
//! per-slot cgroup.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Context;
use pisa_repl::Repl;
use tokio::process::Command;

use crate::cgroup::Cgroups;
use crate::config::{Sandbox, WorkerConfig};

pub struct Launcher {
    repl: PathBuf,
    project: Option<PathBuf>,
    /// The environment `lake env` sets up for the project (LEAN_PATH and friends),
    /// captured once so each spawn skips Lake.
    env: Option<Vec<(OsString, OsString)>>,
    sandbox: Sandbox,
    work_dir: PathBuf,
    cgroups: Option<Cgroups>,
}

impl Launcher {
    pub async fn new(cfg: &WorkerConfig, worker_id: &str) -> anyhow::Result<Self> {
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
        let cgroups = match &cfg.cgroup_root {
            None => None,
            Some(root) => {
                let memory = (cfg.memory_max_mb > 0).then_some(cfg.memory_max_mb * 1024 * 1024);
                let pids = (cfg.pids_max > 0).then_some(cfg.pids_max);
                match Cgroups::init(root, memory, pids) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        tracing::warn!(root = %root.display(), "cgroups unavailable, running without per-process limits: {e}");
                        None
                    }
                }
            }
        };
        Ok(Launcher {
            repl: std::path::absolute(&cfg.repl).unwrap_or_else(|_| cfg.repl.clone()),
            project: cfg.project.clone(),
            env,
            sandbox: cfg.sandbox,
            work_dir,
            cgroups,
        })
    }

    /// A private writable directory for the slot, visible inside the sandbox
    /// at the same path.
    pub fn slot_dir(&self, slot: usize) -> PathBuf {
        self.work_dir.join(format!("slot-{slot}"))
    }

    pub fn spawn(&self, slot: usize) -> std::io::Result<Repl> {
        let dir = self.slot_dir(slot);
        std::fs::create_dir_all(&dir)?;
        let cwd = self.project.clone().unwrap_or_else(|| dir.clone());
        let mut cmd = match self.sandbox {
            Sandbox::None => {
                let mut cmd = Command::new(&self.repl);
                cmd.current_dir(&cwd);
                cmd
            }
            Sandbox::Bwrap => {
                let mut cmd = Command::new("bwrap");
                cmd.args(bwrap_args(&dir, &cwd)).arg("--").arg(&self.repl);
                cmd
            }
        };
        if let Some(env) = &self.env {
            cmd.env_clear().envs(env.iter().cloned());
        }
        if let Some(cg) = &self.cgroups {
            cg.apply(slot, &mut cmd)?;
        }
        Repl::spawn(cmd)
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

/// No network, no view of other processes, a read-only filesystem except
/// for the slot directory, and a fresh /tmp.
fn bwrap_args(slot_dir: &Path, cwd: &Path) -> Vec<OsString> {
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
    #[test]
    fn parses_nul_separated_env() {
        let env = super::parse_env0(b"LEAN_PATH=/a:/b\0X=y=z\0");
        assert_eq!(env.len(), 2);
        assert_eq!(env[0].0, "LEAN_PATH");
        assert_eq!(env[1].1, "y=z");
    }
}
