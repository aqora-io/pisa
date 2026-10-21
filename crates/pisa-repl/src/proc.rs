//! Memory accounting from /proc, used to recycle REPLs at a high-water mark.

use std::fs;

/// Resident set size of `pid` and all its descendants, in bytes. Under a
/// sandbox the REPL is a grandchild of the process we spawned, so the whole
/// tree has to be counted. Returns 0 where /proc is unavailable.
pub fn tree_rss_bytes(pid: u32) -> u64 {
    let mut total = 0;
    let mut stack = vec![pid];
    while let Some(p) = stack.pop() {
        total += rss_bytes(p);
        stack.extend(children(p));
    }
    total
}

fn rss_bytes(pid: u32) -> u64 {
    let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) else {
        return 0;
    };
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        .map_or(0, |kb| kb * 1024)
}

fn children(pid: u32) -> Vec<u32> {
    let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    tasks
        .flatten()
        .filter_map(|t| fs::read_to_string(t.path().join("children")).ok())
        .flat_map(|c| {
            c.split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn own_rss_is_positive() {
        if cfg!(target_os = "linux") {
            assert!(super::tree_rss_bytes(std::process::id()) > 0);
        }
    }
}
