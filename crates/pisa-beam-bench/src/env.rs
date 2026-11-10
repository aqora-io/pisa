//! What every phase shares: the worker's own launcher (so both backends run
//! under the worker's sandbox, environment and cgroups), a fresh slot per
//! measurement, and the slot cgroups' counters.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use pisa_beam::{Beam, Packages, Workspace};
use pisa_repl::Repl;
use pisa_worker::config::WorkerConfig;
use pisa_worker::launcher::Launcher;
use serde::Serialize;

/// The library a workspace's documents belong to.
pub const LIB: &str = "BeamDocs";

pub struct Env {
    pub launcher: Launcher,
    pub project: PathBuf,
    pub beam_bin: PathBuf,
    pub packages: Packages,
    pub toolchain: String,
    pub version: String,
    cgroup_root: Option<PathBuf>,
    next_slot: AtomicUsize,
}

/// A slot cgroup's counters at one moment.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Usage {
    pub memory_mb: Option<f64>,
    pub memory_peak_mb: Option<f64>,
    pub pids_peak: Option<u64>,
    pub cpu_secs: Option<f64>,
}

impl Env {
    pub async fn new(cfg: &WorkerConfig, beam_bin: PathBuf, packages: Packages) -> Result<Env> {
        let version = cfg.version()?;
        // Wipes this worker id's snapshot cache and joins the cgroup root: never
        // run it beside a live worker.
        let launcher = Launcher::new(cfg, "beam-bench", &version).await?;
        let project = cfg.project.clone().context("PISA_PROJECT is required")?;
        let toolchain = std::fs::read_to_string(project.join("lean-toolchain"))?
            .trim()
            .to_string();
        Ok(Env {
            launcher,
            project,
            beam_bin,
            packages,
            toolchain,
            version,
            cgroup_root: cfg.cgroup_root.clone(),
            next_slot: AtomicUsize::new(0),
        })
    }

    /// A slot not used before: its own directory and cgroup, so counters
    /// such as `memory.peak` belong to one measurement.
    pub fn slot(&self) -> usize {
        self.next_slot.fetch_add(1, Ordering::SeqCst)
    }

    pub fn slot_dir(&self, slot: usize) -> PathBuf {
        self.launcher.slot_dir(slot)
    }

    pub fn repl(&self, slot: usize) -> Result<Repl> {
        Ok(self.launcher.spawn(slot)?)
    }

    /// A workspace over the project, in the slot's directory (the only one
    /// the sandbox lets Lean write).
    pub fn workspace(&self, slot: usize, name: &str) -> Result<Workspace> {
        let dir = self.slot_dir(slot).join(name);
        Ok(Workspace::over(&dir, &self.project, LIB, self.packages)?)
    }

    /// `lean-beam-mcp` for `slot`, set up as the worker sets up a REPL, with a
    /// writable home and session directory in the slot.
    pub async fn beam(&self, slot: usize) -> Result<Beam> {
        let dir = self.slot_dir(slot);
        let home = dir.join("home");
        let session = dir.join("beam");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&session)?;
        // Beam refuses a session directory others may read.
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o700))?;
        let mut cmd = self.launcher.command(slot, &self.beam_bin, &[])?;
        cmd.env("HOME", &home).env("BEAM_SESSION_ROOT", &session);
        Beam::spawn(cmd, Duration::from_secs(120))
            .await
            .context("starting lean-beam-mcp")
    }

    fn cgroup(&self, slot: usize) -> Option<PathBuf> {
        Some(self.cgroup_root.as_ref()?.join(format!("repl-{slot}")))
    }

    fn read(&self, slot: usize, file: &str) -> Option<String> {
        std::fs::read_to_string(self.cgroup(slot)?.join(file)).ok()
    }

    pub fn usage(&self, slot: usize) -> Usage {
        let number = |file: &str| self.read(slot, file)?.trim().parse::<u64>().ok();
        let mb = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
        let cpu = self.read(slot, "cpu.stat").and_then(|stat| {
            stat.lines()
                .find_map(|l| l.strip_prefix("usage_usec "))
                .and_then(|v| v.trim().parse::<u64>().ok())
        });
        Usage {
            memory_mb: number("memory.current").map(mb),
            memory_peak_mb: number("memory.peak").map(mb),
            pids_peak: number("pids.peak"),
            cpu_secs: cpu.map(|us| us as f64 / 1e6),
        }
    }

    pub fn pids(&self, slot: usize) -> Option<u64> {
        self.read(slot, "pids.current")?.trim().parse().ok()
    }

    pub fn snapshot_root(&self) -> &Path {
        self.launcher.snapshot_root()
    }
}
