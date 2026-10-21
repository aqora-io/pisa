//! Starting REPL processes.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::Context;
use pisa_repl::Repl;
use tokio::process::Command;

use crate::config::WorkerConfig;

pub struct Launcher {
    repl: PathBuf,
    project: Option<PathBuf>,
    /// The environment `lake env` sets up for the project (LEAN_PATH and friends),
    /// captured once so each spawn skips Lake.
    env: Option<Vec<(OsString, OsString)>>,
}

impl Launcher {
    pub async fn new(cfg: &WorkerConfig) -> anyhow::Result<Self> {
        let env = match &cfg.project {
            Some(project) => Some(lake_env(project).await?),
            None => None,
        };
        Ok(Launcher {
            repl: cfg.repl.clone(),
            project: cfg.project.clone(),
            env,
        })
    }

    pub fn spawn(&self, _slot: usize) -> std::io::Result<Repl> {
        let mut cmd = Command::new(&self.repl);
        if let Some(project) = &self.project {
            cmd.current_dir(project);
        }
        if let Some(env) = &self.env {
            cmd.env_clear().envs(env.iter().cloned());
        }
        Repl::spawn(cmd)
    }
}

async fn lake_env(project: &PathBuf) -> anyhow::Result<Vec<(OsString, OsString)>> {
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
