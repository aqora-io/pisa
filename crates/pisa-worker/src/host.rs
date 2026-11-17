//! What the machine under the worker has: its memory and its boot.
//!
//! Each reads a file of the kernel, and gives `None` where it isn't there,
//! as on a system without `/proc`.

/// `MemTotal` of `/proc/meminfo`, in MiB.
pub fn parse_meminfo(text: &str) -> Option<u64> {
    let kib: u64 = text
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()?;
    Some(kib / 1024)
}

/// A cgroup v2 `memory.max`, in MiB; `None` for `max`, which sets no limit.
pub fn parse_memory_max(text: &str) -> Option<u64> {
    Some(text.trim().parse::<u64>().ok()? / (1024 * 1024))
}

/// The memory the worker's Lean processes share, in MiB: the machine's, or
/// the memory limit of the container the worker runs in, whichever is less.
/// The container's limit is read at `/sys/fs/cgroup/memory.max`, the root of
/// the cgroup namespace a container has, whether or not `PISA_CGROUP_ROOT`
/// places the Lean processes in cgroups below it. `None` if the machine's
/// memory can't be read.
pub fn memory_mb() -> Option<u64> {
    let machine = parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)?;
    let container = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .ok()
        .and_then(|text| parse_memory_max(&text));
    Some(container.map_or(machine, |c| c.min(machine)))
}

/// The id of this boot of the machine, which tells the workers on one
/// machine from those on others; `None` if it can't be read.
pub fn boot_id() -> Option<String> {
    let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_gives_the_total_in_mebibytes() {
        let text = "MemTotal:       48167364 kB\nMemFree:         1000 kB\n";
        assert_eq!(parse_meminfo(text), Some(47038));
        assert_eq!(parse_meminfo("MemFree: 1 kB\n"), None);
        assert_eq!(parse_meminfo(""), None);
    }

    #[test]
    fn memory_max_is_a_number_or_nothing() {
        assert_eq!(parse_memory_max("max\n"), None);
        assert_eq!(parse_memory_max("17179869184\n"), Some(16384));
        assert_eq!(parse_memory_max("nonsense"), None);
    }
}
