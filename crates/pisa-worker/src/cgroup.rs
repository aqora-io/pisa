//! Per-process resource limits with cgroup v2.
//!
//! The worker needs a delegated subtree. It moves itself into `<root>/agent`
//! (cgroup v2 forbids processes in a cgroup that has controllers enabled for
//! its children), enables the memory and pids controllers, and puts each REPL
//! in `<root>/repl-<slot>`. A child joins its cgroup between fork and exec, so
//! everything it starts is limited too.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use tokio::process::Command;

pub struct Cgroups {
    root: PathBuf,
    memory_max: Option<u64>,
    pids_max: Option<u64>,
}

impl Cgroups {
    pub fn init(root: &Path, memory_max: Option<u64>, pids_max: Option<u64>) -> io::Result<Self> {
        let agent = root.join("agent");
        std::fs::create_dir_all(&agent)?;
        std::fs::write(agent.join("cgroup.procs"), std::process::id().to_string())?;
        std::fs::write(root.join("cgroup.subtree_control"), "+memory +pids")?;
        Ok(Cgroups {
            root: root.to_path_buf(),
            memory_max,
            pids_max,
        })
    }

    fn slot_dir(&self, slot: usize) -> PathBuf {
        self.root.join(format!("repl-{slot}"))
    }

    /// Creates (or resets) the slot's cgroup and makes `cmd` join it on spawn.
    pub fn apply(&self, slot: usize, cmd: &mut Command) -> io::Result<()> {
        let dir = self.slot_dir(slot);
        std::fs::create_dir_all(&dir)?;
        let limit = |v: Option<u64>| v.map_or("max".to_string(), |v| v.to_string());
        std::fs::write(dir.join("memory.max"), limit(self.memory_max))?;
        // Without swap, memory.max is a hard ceiling. Absent when swap accounting is off.
        let _ = std::fs::write(dir.join("memory.swap.max"), "0");
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

    pub fn memory_current(&self, slot: usize) -> Option<u64> {
        std::fs::read_to_string(self.slot_dir(slot).join("memory.current"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}
