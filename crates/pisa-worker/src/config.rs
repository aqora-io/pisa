use std::path::{Path, PathBuf};

use anyhow::Context;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, clap::Parser)]
#[command(about = "Pisa worker: runs Lean REPLs for the router")]
pub struct WorkerConfig {
    /// Router address that workers dial.
    #[arg(long, env = "PISA_ROUTER", default_value = "127.0.0.1:7070")]
    pub router: String,
    #[arg(long, env = "PISA_WORKER_ID")]
    pub worker_id: Option<String>,
    /// Pool label. Defaults to the project's toolchain plus a hash of its lake-manifest.json.
    #[arg(long, env = "PISA_VERSION")]
    pub version: Option<String>,
    /// Number of Lean processes. Defaults to the CPU count minus one.
    #[arg(long, env = "PISA_SLOTS")]
    pub slots: Option<usize>,
    /// The REPL executable.
    #[arg(long, env = "PISA_REPL", default_value = "repl")]
    pub repl: PathBuf,
    /// Lake project whose dependencies the REPL imports (run through `lake env`).
    #[arg(long, env = "PISA_PROJECT")]
    pub project: Option<PathBuf>,
    /// Replace a Lean process after this many jobs. The REPL never frees environments.
    #[arg(long, env = "PISA_MAX_USES", default_value_t = 200)]
    pub max_uses: u64,
    /// Replace a Lean process once its resident memory exceeds this (0 disables).
    #[arg(long, env = "PISA_MAX_RSS_MB", default_value_t = 0)]
    pub max_rss_mb: u64,
    /// Timeout for importing a header into a fresh process.
    #[arg(long, env = "PISA_IMPORT_TIMEOUT_SECS", default_value_t = 600)]
    pub import_timeout_secs: u64,
    /// Headers to import at startup, e.g. "import Mathlib". Separate several with ';'.
    #[arg(long, env = "PISA_PREWARM", value_delimiter = ';')]
    pub prewarm: Vec<String>,
}

impl WorkerConfig {
    pub fn slots(&self) -> usize {
        self.slots.unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1).max(1))
        })
    }

    pub fn worker_id(&self) -> String {
        self.worker_id.clone().unwrap_or_else(|| {
            let host = std::env::var("HOSTNAME")
                .ok()
                .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
                .map(|h| h.trim().to_string())
                .unwrap_or_else(|| "worker".into());
            format!("{host}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
        })
    }

    pub fn version(&self) -> anyhow::Result<String> {
        match (&self.version, &self.project) {
            (Some(v), _) => Ok(v.clone()),
            (None, Some(project)) => project_version(project),
            (None, None) => Ok("dev".into()),
        }
    }
}

/// `<toolchain tag>-<first 8 hex of sha256(lake-manifest.json)>`, e.g. `v4.35.0-1a2b3c4d`.
pub fn project_version(project: &Path) -> anyhow::Result<String> {
    let toolchain = std::fs::read_to_string(project.join("lean-toolchain"))
        .context("reading lean-toolchain")?;
    let toolchain = toolchain.trim();
    let tag = toolchain.rsplit(':').next().unwrap_or(toolchain);
    let manifest = std::fs::read(project.join("lake-manifest.json")).unwrap_or_default();
    let hash = hex::encode(&Sha256::digest(&manifest)[..4]);
    Ok(format!("{tag}-{hash}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_from_project_files() {
        let dir = std::env::temp_dir().join(format!("pisa-v-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lean-toolchain"), "leanprover/lean4:v4.35.0\n").unwrap();
        std::fs::write(dir.join("lake-manifest.json"), "{}").unwrap();
        let v = project_version(&dir).unwrap();
        assert!(v.starts_with("v4.35.0-"), "{v}");
        assert_eq!(v.len(), "v4.35.0-".len() + 8);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
