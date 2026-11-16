//! Processor information: identity, topology, cache, clocks, flags, errata.
//!
//! Sources: /proc/cpuinfo, /proc/loadavg, /sys/devices/system/cpu, and CPUID
//! for the one thing the kernel does not expose cleanly (hypervisor identity).

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::collect::{Collector, Group};
use crate::model::{Node, Value, format_bytes};
use crate::sysfs;

const CPU_ROOT: &str = "/sys/devices/system/cpu";

pub struct Cpu;

impl Collector for Cpu {
    fn id(&self) -> &'static str {
        "cpu"
    }

    fn title(&self) -> &'static str {
        "Processor"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn refresh_interval(&self) -> Option<Duration> {
        // Clocks and load average move; the rest is static but cheap to re-read.
        Some(Duration::from_secs(2))
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let raw = std::fs::read_to_string("/proc/cpuinfo").context("reading /proc/cpuinfo")?;
        let blocks = sysfs::parse_cpuinfo_blocks(&raw);
        let cpus: Vec<_> = blocks
            .iter()
            .filter(|b| sysfs::find(b, "processor").is_some())
            .collect();

        let mut nodes = vec![identity(&cpus)];

        if let Some(cache) = cache() {
            nodes.push(cache);
        }
        nodes.push(clocks(&cpus));
        if let Some(load) = load_average() {
            nodes.push(load);
        }
        if let Some(first) = cpus.first()
            && let Some(flags) = flags(first)
        {
            nodes.push(flags);
        }
        if let Some(vulns) = vulnerabilities() {
            nodes.push(vulns);
        }

        Ok(nodes)
    }
}

fn identity(cpus: &[&Vec<(String, String)>]) -> Node {
    let mut node = Node::group("Processor", vec![]);
    let Some(first) = cpus.first() else {
        node.push(Node::field("Model", "unknown"));
        return node;
    };

    // ARM and RISC-V name these differently from x86.
    let model = sysfs::find(first, "model name")
        .or_else(|| sysfs::find(first, "Model"))
        .or_else(|| sysfs::find(first, "uarch"))
        .or_else(|| sysfs::find(first, "cpu"));
    node.push_opt("Model", model);
    node.push_opt(
        "Vendor",
        sysfs::find(first, "vendor_id").or_else(|| sysfs::find(first, "mvendorid")),
    );
    node.push(Node::field("Architecture", std::env::consts::ARCH));

    if let (Some(family), Some(model), Some(stepping)) = (
        sysfs::find(first, "cpu family"),
        sysfs::find(first, "model"),
        sysfs::find(first, "stepping"),
    ) {
        node.push(Node::field(
            "Family / Model / Stepping",
            format!("{family} / {model} / {stepping}"),
        ));
    }
    node.push_opt("Microcode", sysfs::find(first, "microcode"));

    // Sockets and physical cores are derived from the topology keys, which only
    // x86 reports in /proc/cpuinfo; elsewhere fall back to the thread count.
    let sockets: BTreeSet<&str> = cpus
        .iter()
        .filter_map(|b| sysfs::find(b, "physical id"))
        .collect();
    let cores: BTreeSet<(&str, &str)> = cpus
        .iter()
        .filter_map(|b| {
            Some((
                sysfs::find(b, "physical id").unwrap_or("0"),
                sysfs::find(b, "core id")?,
            ))
        })
        .collect();

    if !sockets.is_empty() {
        node.push(Node::field("Sockets", sockets.len()));
    }
    if !cores.is_empty() {
        node.push(Node::field("Physical cores", cores.len()));
    }
    node.push(Node::field("Logical processors", cpus.len()));

    if let Some(online) = sysfs::read(format!("{CPU_ROOT}/online")) {
        let count = sysfs::parse_cpu_list(&online).len();
        if count != cpus.len() {
            node.push(Node::field("Online", format!("{count} of {}", cpus.len())));
        }
    }

    node.push_opt("Hypervisor", hypervisor());
    node.push_opt("BogoMIPS", sysfs::find(first, "bogomips"));
    node.push_opt("Address sizes", sysfs::find(first, "address sizes"));
    node
}

/// Hypervisor identity via CPUID's paravirtualisation leaf.
///
/// /proc/cpuinfo only carries a `hypervisor` feature flag, which says that we
/// are virtualised but not by what.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn hypervisor() -> Option<String> {
    let info = raw_cpuid::CpuId::new().get_hypervisor_info()?;
    Some(format!("{:?}", info.identify()))
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn hypervisor() -> Option<String> {
    None
}

/// Cache topology, deduplicated across logical CPUs that share a cache.
fn cache() -> Option<Node> {
    // (level, kind) -> (bytes per instance, instance count)
    let mut seen: BTreeSet<(u64, String, String)> = BTreeSet::new();
    let mut levels: std::collections::BTreeMap<(u64, String), (Option<u64>, usize)> =
        Default::default();

    for cpu_dir in sysfs::list_dir(CPU_ROOT) {
        // Match cpu0, cpu1, … and skip the siblings sharing that prefix
        // (cpufreq, cpuidle). A `?` here would abandon the whole scan.
        let Some(name) = cpu_dir.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        let Some(digits) = name.strip_prefix("cpu") else {
            continue;
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        for index in sysfs::list_dir(cpu_dir.join("cache")) {
            let Some(level) = sysfs::read_u64(index.join("level")) else {
                continue;
            };
            let Some(kind) = sysfs::read(index.join("type")) else {
                continue;
            };
            // Count each physical cache once: sibling threads report the same
            // shared_cpu_list for the cache they share.
            let shared = sysfs::read(index.join("shared_cpu_list")).unwrap_or_default();
            if !seen.insert((level, kind.clone(), shared)) {
                continue;
            }
            let size = sysfs::read(index.join("size")).and_then(|s| parse_cache_size(&s));
            let entry = levels.entry((level, kind)).or_insert((size, 0));
            entry.1 += 1;
        }
    }

    if levels.is_empty() {
        return None;
    }

    let mut node = Node::group("Cache", vec![]);
    for ((level, kind), (size, count)) in levels {
        let label = match kind.as_str() {
            "Data" => format!("L{level} Data"),
            "Instruction" => format!("L{level} Instruction"),
            _ => format!("L{level}"),
        };
        let value = match size {
            Some(bytes) if count > 1 => {
                format!("{} × {}", count, format_bytes(bytes))
            }
            Some(bytes) => format_bytes(bytes),
            None => format!("{count} instances"),
        };
        node.push(Node::field(label, value));
    }
    Some(node)
}

/// Parse sysfs cache sizes, which carry a `K`/`M`/`G` suffix (e.g. `8192K`).
fn parse_cache_size(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let (digits, scale) = match raw.chars().last()? {
        'K' | 'k' => (&raw[..raw.len() - 1], 1024),
        'M' | 'm' => (&raw[..raw.len() - 1], 1024 * 1024),
        'G' | 'g' => (&raw[..raw.len() - 1], 1024 * 1024 * 1024),
        _ => (raw, 1),
    };
    digits.trim().parse::<u64>().ok().map(|n| n * scale)
}

fn clocks(cpus: &[&Vec<(String, String)>]) -> Node {
    let mut node = Node::group("Clocks", vec![]);

    // Take the id from each block rather than its position: with CPUs offline,
    // /proc/cpuinfo's `processor` ids are not contiguous from zero.
    let current: Vec<(u64, u64)> = cpus
        .iter()
        .filter_map(|block| {
            let id: u64 = sysfs::find(block, "processor")?.parse().ok()?;
            // cpufreq reports kHz; /proc/cpuinfo reports MHz as a float.
            let hz = sysfs::read_u64(format!("{CPU_ROOT}/cpu{id}/cpufreq/scaling_cur_freq"))
                .map(|khz| khz * 1000)
                .or_else(|| {
                    let mhz: f64 = sysfs::find(block, "cpu MHz")?.parse().ok()?;
                    Some((mhz * 1_000_000.0) as u64)
                })?;
            Some((id, hz))
        })
        .collect();

    if !current.is_empty() {
        let sum: u64 = current.iter().map(|(_, hz)| hz).sum();
        node.push(Node::field(
            "Current (average)",
            Value::Hertz(sum / current.len() as u64),
        ));
    }

    // Policy limits are per-governor and identical across a policy's CPUs; read
    // them from the lowest online CPU rather than assuming cpu0 is present.
    let policy = format!(
        "{CPU_ROOT}/cpu{}/cpufreq",
        current.first().map(|(id, _)| *id).unwrap_or(0)
    );

    node.push_opt(
        "Base",
        sysfs::read_u64(format!("{policy}/base_frequency")).map(|k| Value::Hertz(k * 1000)),
    );
    node.push_opt(
        "Minimum",
        sysfs::read_u64(format!("{policy}/cpuinfo_min_freq")).map(|k| Value::Hertz(k * 1000)),
    );
    node.push_opt(
        "Maximum",
        sysfs::read_u64(format!("{policy}/cpuinfo_max_freq")).map(|k| Value::Hertz(k * 1000)),
    );
    node.push_opt("Driver", sysfs::read(format!("{policy}/scaling_driver")));
    node.push_opt(
        "Governor",
        sysfs::read(format!("{policy}/scaling_governor")),
    );

    if !current.is_empty() {
        let per_core = current
            .into_iter()
            .map(|(i, hz)| Node::field(format!("CPU {i}"), Value::Hertz(hz)))
            .collect();
        node.push(Node::group("Per processor", per_core));
    }
    node
}

fn load_average() -> Option<Node> {
    let raw = sysfs::read("/proc/loadavg")?;
    let fields: Vec<&str> = raw.split_whitespace().collect();
    if fields.len() < 4 {
        return None;
    }

    let mut node = Node::group("Load average", vec![]);
    for (label, field) in [("1 minute", 0), ("5 minutes", 1), ("15 minutes", 2)] {
        node.push_opt(label, fields.get(field).copied());
    }
    // Field 4 is "runnable/total" scheduling entities.
    if let Some((runnable, total)) = fields[3].split_once('/') {
        node.push(Node::field("Runnable entities", runnable));
        node.push(Node::field("Total entities", total));
    }
    Some(node)
}

fn flags(first: &[(String, String)]) -> Option<Node> {
    // x86 calls them `flags`; ARM calls them `Features`.
    let raw = sysfs::find(first, "flags").or_else(|| sysfs::find(first, "Features"))?;
    let list: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
    Some(Node::group(
        "Features",
        vec![
            Node::field("Count", list.len()),
            Node::field("Flags", Value::List(list)),
        ],
    ))
}

/// Per-errata mitigation status from sysfs, which is more specific than
/// /proc/cpuinfo's `bugs` line (it says what is mitigated, and how).
fn vulnerabilities() -> Option<Node> {
    let entries = sysfs::list_dir(format!("{CPU_ROOT}/vulnerabilities"));
    if entries.is_empty() {
        return None;
    }
    let mut node = Node::group("Vulnerabilities", vec![]);
    for path in entries {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        node.push_opt(name.replace('_', " "), sysfs::read(&path));
    }
    if node.children.is_empty() {
        None
    } else {
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sysfs_cache_sizes() {
        assert_eq!(parse_cache_size("32K"), Some(32 * 1024));
        assert_eq!(parse_cache_size("8192K"), Some(8192 * 1024));
        assert_eq!(parse_cache_size("1M"), Some(1024 * 1024));
        assert_eq!(parse_cache_size("512"), Some(512));
        assert_eq!(parse_cache_size("weird"), None);
    }

    #[test]
    fn collects_on_this_machine() {
        // /proc/cpuinfo exists on every Linux host, so this should always
        // produce an identity section with a model.
        let nodes = Cpu.collect().expect("cpu collection failed");
        assert!(!nodes.is_empty());
        assert_eq!(nodes[0].label, "Processor");
        assert!(!nodes[0].children.is_empty());
    }
}
