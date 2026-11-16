//! Memory information, derived from /proc/meminfo.
//!
//! The overview section mirrors how `free(1)` accounts for memory — "used" is
//! total minus *available*, not total minus free, so reclaimable page cache is
//! not counted against you. Every raw /proc/meminfo key is also exposed
//! verbatim under its own section.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::collect::{Collector, Group};
use crate::model::{Node, Value};
use crate::sysfs;

pub struct Memory;

impl Collector for Memory {
    fn id(&self) -> &'static str {
        "memory"
    }

    fn title(&self) -> &'static str {
        "Memory Usage"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn refresh_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(2))
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let raw = std::fs::read_to_string("/proc/meminfo").context("reading /proc/meminfo")?;
        let fields = parse_meminfo(&raw);
        let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| *v);

        let mut nodes = Vec::new();

        let mut overview = Node::group("Overview", vec![]);
        let total = get("MemTotal");
        let available = get("MemAvailable");
        overview.push_opt("Total", total.map(Value::Bytes));
        if let (Some(total), Some(available)) = (total, available) {
            let used = total.saturating_sub(available);
            overview.push(Node::field("Used", Value::Bytes(used)));
            overview.push(Node::field("Available", Value::Bytes(available)));
            if total > 0 {
                overview.push(Node::field(
                    "Usage",
                    Value::Percent(used as f64 / total as f64 * 100.0),
                ));
            }
        }
        overview.push_opt("Free", get("MemFree").map(Value::Bytes));
        overview.push_opt("Buffers", get("Buffers").map(Value::Bytes));
        overview.push_opt("Cached", get("Cached").map(Value::Bytes));
        overview.push_opt("Shared", get("Shmem").map(Value::Bytes));
        overview.push_opt("Reclaimable slab", get("SReclaimable").map(Value::Bytes));
        nodes.push(overview);

        let mut swap = Node::group("Swap", vec![]);
        let swap_total = get("SwapTotal");
        swap.push_opt("Total", swap_total.map(Value::Bytes));
        if let (Some(total), Some(free)) = (swap_total, get("SwapFree")) {
            let used = total.saturating_sub(free);
            swap.push(Node::field("Used", Value::Bytes(used)));
            swap.push(Node::field("Free", Value::Bytes(free)));
            if total > 0 {
                swap.push(Node::field(
                    "Usage",
                    Value::Percent(used as f64 / total as f64 * 100.0),
                ));
            }
        }
        swap.push_opt("Cached", get("SwapCached").map(Value::Bytes));
        swap.push_opt(
            "Swappiness",
            sysfs::read("/proc/sys/vm/swappiness").map(Value::Text),
        );
        nodes.push(swap);

        // Huge pages are counts, not byte sizes, apart from the page size itself.
        let mut huge = Node::group("Huge pages", vec![]);
        huge.push_opt("Size", get("Hugepagesize").map(Value::Bytes));
        for key in [
            "HugePages_Total",
            "HugePages_Free",
            "HugePages_Rsvd",
            "HugePages_Surp",
        ] {
            huge.push_opt(
                key.replace("HugePages_", ""),
                get(key).map(|n| Value::Int(n as i64)),
            );
        }
        huge.push_opt("Anonymous", get("AnonHugePages").map(Value::Bytes));
        if huge.children.len() > 1 {
            nodes.push(huge);
        }

        let mut commit = Node::group("Commit", vec![]);
        commit.push_opt("Committed", get("Committed_AS").map(Value::Bytes));
        commit.push_opt("Commit limit", get("CommitLimit").map(Value::Bytes));
        commit.push_opt(
            "Overcommit mode",
            sysfs::read("/proc/sys/vm/overcommit_memory").map(Value::Text),
        );
        if !commit.children.is_empty() {
            nodes.push(commit);
        }

        // Everything /proc/meminfo reported, so nothing is hidden from the
        // report even before a dedicated section exists for it.
        let raw_fields = fields
            .iter()
            .map(|(key, value)| Node::field(key.clone(), Value::Bytes(*value)))
            .collect();
        nodes.push(Node::group("All /proc/meminfo", raw_fields));

        Ok(nodes)
    }
}

/// Parse /proc/meminfo into `(key, bytes)` pairs.
///
/// Values carrying a `kB` suffix are scaled to bytes; bare counts (the
/// `HugePages_*` fields) are passed through unscaled.
fn parse_meminfo(text: &str) -> Vec<(String, u64)> {
    sysfs::parse_colon_pairs(text)
        .into_iter()
        .filter_map(|(key, value)| {
            let (number, scale) = match value.strip_suffix(" kB") {
                Some(n) => (n, 1024),
                None => (value.as_str(), 1),
            };
            let parsed: u64 = number.trim().parse().ok()?;
            Some((key, parsed * scale))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_kb_suffixed_values_to_bytes() {
        let fields = parse_meminfo("MemTotal:       32768 kB\nHugePages_Total:    0\n");
        assert_eq!(fields[0], ("MemTotal".to_string(), 32768 * 1024));
        assert_eq!(fields[1], ("HugePages_Total".to_string(), 0));
    }

    #[test]
    fn collects_on_this_machine() {
        let nodes = Memory.collect().expect("memory collection failed");
        assert_eq!(nodes[0].label, "Overview");
        assert!(
            nodes[0].children.iter().any(|c| c.label == "Total"),
            "expected a total memory field"
        );
    }
}
