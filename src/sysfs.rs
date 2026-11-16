//! Small helpers for reading the kernel's text interfaces.
//!
//! Everything here treats a missing or unreadable file as `None` rather than an
//! error: which files exist varies by kernel config, architecture, and
//! privilege, and a collector should degrade to showing less rather than
//! failing the whole page.

use std::fs;
use std::path::Path;

/// Read a file, trimming trailing whitespace. `None` if it cannot be read.
pub fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim_end().to_string())
}

/// Read a file holding a single integer.
pub fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read(path)?.trim().parse().ok()
}

/// Parse `key: value` lines, as used by /proc/cpuinfo, /proc/meminfo and friends.
///
/// Order is preserved and duplicate keys are kept, because /proc/cpuinfo
/// repeats its whole key set once per logical CPU.
pub fn parse_colon_pairs(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

/// Split /proc/cpuinfo into one key/value set per logical CPU.
///
/// Blocks are separated by blank lines. On architectures that emit a trailing
/// machine-wide block (ARM's `Hardware`/`Revision`/`Serial`), that block comes
/// back as the final entry, so callers should match on keys rather than assume
/// every block describes a CPU.
pub fn parse_cpuinfo_blocks(text: &str) -> Vec<Vec<(String, String)>> {
    text.split("\n\n")
        .map(parse_colon_pairs)
        .filter(|block| !block.is_empty())
        .collect()
}

/// Look up the first value for `key` in a parsed block.
pub fn find<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

/// List the entries of a directory, sorted by name for stable output.
pub fn list_dir(path: impl AsRef<Path>) -> Vec<std::path::PathBuf> {
    let mut entries: Vec<_> = match fs::read_dir(path) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => return Vec::new(),
    };
    entries.sort();
    entries
}

/// Expand a kernel CPU list such as `0-3,8` into its members.
///
/// Used by sysfs for `shared_cpu_list`, `core_cpus_list`, and similar.
pub fn parse_cpu_list(list: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in list.trim().split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((start, end)) => {
                if let (Ok(a), Ok(b)) = (start.trim().parse::<u32>(), end.trim().parse::<u32>()) {
                    out.extend(a..=b);
                }
            }
            None => {
                if let Ok(n) = part.trim().parse() {
                    out.push(n);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_cpuinfo_into_per_cpu_blocks() {
        let text =
            "processor\t: 0\nmodel name\t: Test CPU\n\nprocessor\t: 1\nmodel name\t: Test CPU\n";
        let blocks = parse_cpuinfo_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(find(&blocks[1], "processor"), Some("1"));
        assert_eq!(find(&blocks[0], "model name"), Some("Test CPU"));
    }

    #[test]
    fn ignores_keys_with_empty_values() {
        // /proc/cpuinfo emits bare "power management:" lines on some AMD parts.
        let pairs = parse_colon_pairs("power management:\nmodel name\t: Test\n");
        assert_eq!(find(&pairs, "power management"), None);
        assert_eq!(find(&pairs, "model name"), Some("Test"));
    }

    #[test]
    fn expands_cpu_ranges() {
        assert_eq!(parse_cpu_list("0-3,8"), vec![0, 1, 2, 3, 8]);
        assert_eq!(parse_cpu_list("2"), vec![2]);
        assert!(parse_cpu_list("").is_empty());
    }
}
