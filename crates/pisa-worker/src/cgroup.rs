//! Per-process resource limits with cgroup v2.
//!
//! The worker needs a delegated subtree. It moves the processes in it, itself
//! among them, into `<root>/agent` (cgroup v2 forbids processes in a cgroup
//! that has controllers enabled for its children), enables the memory and
//! pids controllers, and puts each REPL in `<root>/repl-<slot>`. A child joins its cgroup between fork and exec, so
//! everything it starts is limited too. A Lean Beam slot's processes are
//! killed together when they run out of memory (`memory.oom.group`).

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use tokio::process::Command;

pub struct Cgroups {
    root: PathBuf,
    memory_max: Option<u64>,
    pids_max: Option<u64>,
    /// The first Beam slot and Beam's memory.max, which slots from it get.
    beam: Option<(usize, Option<u64>)>,
}

impl Cgroups {
    pub fn init(root: &Path, memory_max: Option<u64>, pids_max: Option<u64>) -> io::Result<Self> {
        let agent = root.join("agent");
        std::fs::create_dir_all(&agent)?;
        // Every process in the root moves, not only the worker: the init
        // that runs it as PID 1 too.
        for pid in std::fs::read_to_string(root.join("cgroup.procs"))?.split_whitespace() {
            match std::fs::write(agent.join("cgroup.procs"), pid) {
                // Gone since it was listed.
                Err(e) if e.raw_os_error() == Some(libc::ESRCH) => {}
                moved => moved?,
            }
        }
        std::fs::write(root.join("cgroup.subtree_control"), "+memory +pids")?;
        Ok(Cgroups {
            root: root.to_path_buf(),
            memory_max,
            pids_max,
            beam: None,
        })
    }

    /// Gives slots from `first_slot` on, which run Lean Beam, `memory_max`
    /// rather than the REPL's.
    pub fn with_beam(self, first_slot: usize, memory_max: Option<u64>) -> Self {
        Cgroups {
            beam: Some((first_slot, memory_max)),
            ..self
        }
    }

    fn slot_dir(&self, slot: usize) -> PathBuf {
        self.root.join(format!("repl-{slot}"))
    }

    /// Creates (or resets) the slot's cgroup and makes `cmd` join it on spawn.
    pub fn apply(&self, slot: usize, cmd: &mut Command) -> io::Result<()> {
        let dir = self.slot_dir(slot);
        std::fs::create_dir_all(&dir)?;
        let limit = |v: Option<u64>| v.map_or("max".to_string(), |v| v.to_string());
        let memory_max = match self.beam {
            Some((first, beam)) if slot >= first => beam,
            _ => self.memory_max,
        };
        std::fs::write(dir.join("memory.max"), limit(memory_max))?;
        // Without swap, memory.max is a hard ceiling. Absent when swap accounting is off.
        let _ = std::fs::write(dir.join("memory.swap.max"), "0");
        // Lean Beam runs a server and workers: running out of memory stops
        // them all, so the slot starts afresh and says why, rather than going
        // on without the worker the kernel chose.
        if matches!(self.beam, Some((first, _)) if slot >= first) {
            std::fs::write(dir.join("memory.oom.group"), "1")?;
        }
        std::fs::write(dir.join("pids.max"), limit(self.pids_max))?;
        let procs = CString::new(dir.join("cgroup.procs").as_os_str().as_bytes())?;
        // SAFETY: only async-signal-safe calls (open, write, close) between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                let fd = libc::open(procs.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                // "0" moves the writing process, i.e. this child.
                let n = libc::write(fd, b"0".as_ptr().cast(), 1);
                libc::close(fd);
                if n != 1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }

    /// The setting that limits the slot's memory and its value in MiB, `None`
    /// without a limit.
    pub fn memory_max_mb(&self, slot: usize) -> Option<(&'static str, u64)> {
        let (setting, limit) = match self.beam {
            Some((first, beam)) if slot >= first => ("PISA_BEAM_MEMORY_MAX_MB", beam),
            _ => ("PISA_MEMORY_MAX_MB", self.memory_max),
        };
        Some((setting, limit? / (1024 * 1024)))
    }

    /// The memory limits of slots `0..slots` and the slots from there on
    /// that are `beam_slots` many, summed in MiB; `None` if any has none.
    pub fn memory_caps_mb(&self, slots: usize, beam_slots: usize) -> Option<u64> {
        (0..slots + beam_slots).try_fold(0, |sum, slot| Some(sum + self.memory_max_mb(slot)?.1))
    }

    pub fn oom_kills(&self, slot: usize) -> u64 {
        std::fs::read_to_string(self.slot_dir(slot).join("memory.events"))
            .ok()
            .and_then(|events| {
                events
                    .lines()
                    .find_map(|l| l.strip_prefix("oom_kill "))
                    .and_then(|n| n.trim().parse().ok())
            })
            .unwrap_or(0)
    }

    /// Whether no process is in the slot's cgroup (true if it doesn't exist).
    pub fn is_empty(&self, slot: usize) -> bool {
        std::fs::read_to_string(self.slot_dir(slot).join("cgroup.procs"))
            .map_or(true, |procs| procs.trim().is_empty())
    }

    pub fn memory_current(&self, slot: usize) -> Option<u64> {
        std::fs::read_to_string(self.slot_dir(slot).join("memory.current"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// The processes in the slot's cgroup, by pid; none if it doesn't exist.
    pub fn procs(&self, slot: usize) -> Vec<u32> {
        let procs = std::fs::read_to_string(self.slot_dir(slot).join("cgroup.procs"));
        let procs = procs.unwrap_or_default();
        procs
            .split_whitespace()
            .filter_map(|p| p.parse().ok())
            .collect()
    }

    /// Kills every process in the slot's cgroup (`cgroup.kill`), those that
    /// fork into it meanwhile too. They are gone soon after, not at once.
    pub fn kill(&self, slot: usize) {
        match std::fs::write(self.slot_dir(slot).join("cgroup.kill"), "1") {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                tracing::warn!(slot, "killing the slot's processes failed: {e}")
            }
            _ => {}
        }
    }

    /// The slot's peak memory from now on, which [`Peak::reset`] starts
    /// again; `None` if the kernel can't reset it (before Linux 6.12).
    pub fn peak(&self, slot: usize) -> Option<Peak> {
        let dir = self.slot_dir(slot);
        std::fs::create_dir_all(&dir).ok()?;
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .open(dir.join("memory.peak"))
            .ok()?;
        let peak = Peak(file);
        peak.reset().then_some(peak)
    }
}

/// An open `memory.peak` of a slot's cgroup. Writing to it resets the peak
/// that reads through the same file see, and no other reader's.
pub struct Peak(std::fs::File);

impl Peak {
    /// Starts the peak again from the slot's memory now; whether it could.
    pub fn reset(&self) -> bool {
        use std::os::unix::fs::FileExt;
        self.0.write_at(b"0", 0).is_ok()
    }

    /// The most memory the slot held since the last reset, in MiB.
    pub fn mb(&self) -> Option<u64> {
        use std::os::unix::fs::FileExt;
        let mut buf = [0u8; 32];
        let n = self.0.read_at(&mut buf, 0).ok()?;
        let bytes: u64 = std::str::from_utf8(&buf[..n]).ok()?.trim().parse().ok()?;
        Some(bytes / (1024 * 1024))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slots_cap_is_the_setting_that_gives_it() {
        let cgroups = Cgroups {
            root: PathBuf::new(),
            memory_max: Some(16000 * 1024 * 1024),
            pids_max: None,
            beam: Some((2, Some(8000 * 1024 * 1024))),
        };
        assert_eq!(
            cgroups.memory_max_mb(0),
            Some(("PISA_MEMORY_MAX_MB", 16000))
        );
        assert_eq!(
            cgroups.memory_max_mb(1),
            Some(("PISA_MEMORY_MAX_MB", 16000))
        );
        assert_eq!(
            cgroups.memory_max_mb(2),
            Some(("PISA_BEAM_MEMORY_MAX_MB", 8000))
        );
        let unlimited = Cgroups {
            root: PathBuf::new(),
            memory_max: None,
            pids_max: None,
            beam: Some((2, None)),
        };
        assert_eq!(cgroups.memory_caps_mb(2, 1), Some(2 * 16000 + 8000));
        assert_eq!(cgroups.memory_caps_mb(2, 0), Some(32000));
        assert_eq!(unlimited.memory_caps_mb(2, 1), None);
        // A pool without slots needs no limit.
        let beamless = Cgroups {
            root: PathBuf::new(),
            memory_max: cgroups.memory_max,
            pids_max: None,
            beam: Some((2, None)),
        };
        assert_eq!(beamless.memory_caps_mb(2, 0), Some(32000));
        assert_eq!(beamless.memory_caps_mb(2, 1), None);
        assert_eq!(unlimited.memory_max_mb(0), None);
        assert_eq!(unlimited.memory_max_mb(2), None);
    }
}
