//! Boot history.
//!
//! The journal is the only practical source: `journalctl --list-boots` knows
//! every boot it has records for, with the boot id the kernel generated. The
//! alternative is parsing /var/log/wtmp, a binary `utmp` record array whose
//! layout is libc-specific and which only covers logins anyway — it would
//! answer a different question less reliably.

use anyhow::Result;
use serde_json::Value as Json;

use crate::collect::{Collector, Group};
use crate::model::{Node, format_duration, format_timestamp};
use crate::probe;
use crate::sysfs;

/// How many boots to list. The journal can hold hundreds.
const LIMIT: usize = 25;

pub struct Boots;

impl Collector for Boots {
    fn id(&self) -> &'static str {
        "boots"
    }

    fn title(&self) -> &'static str {
        "Boots"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut current = Node::group("Current boot", vec![]);
        current.push_opt("Boot ID", sysfs::read("/proc/sys/kernel/random/boot_id"));
        current.push_opt("Kernel", sysfs::read("/proc/sys/kernel/osrelease"));
        if let Some(uptime) = sysfs::read("/proc/uptime")
            .and_then(|raw| raw.split_whitespace().next()?.parse::<f64>().ok())
        {
            current.push(Node::field("Uptime", format_duration(uptime as u64)));
            // Derive the boot time rather than reading it: /proc/stat's btime
            // is absent on some kernels, and now-minus-uptime always works.
            if let Some(now) = unix_now() {
                current.push(Node::field(
                    "Booted at",
                    format_timestamp(now - uptime as i64),
                ));
            }
        }

        let mut nodes = vec![current];
        nodes.push(history());
        Ok(nodes)
    }
}

fn history() -> Node {
    let Some(output) = probe::run("journalctl", &["--list-boots", "-o", "json"]) else {
        return Node::group(
            "Boot history",
            vec![Node::field(
                "Status",
                if probe::available("journalctl") {
                    "unavailable — the journal holds no boot records, or is not persistent \
                     (set Storage=persistent in journald.conf)"
                } else {
                    "unavailable — boot history needs systemd's journal"
                },
            )],
        );
    };

    let Ok(Json::Array(boots)) = serde_json::from_str::<Json>(&output) else {
        return Node::group(
            "Boot history",
            vec![Node::field(
                "Status",
                "journalctl produced output that could not be parsed as JSON",
            )],
        );
    };

    let mut node = Node::group("Boot history", vec![]).from("journalctl");
    node.push(Node::field("Recorded boots", boots.len()));

    // Most recent first: index 0 is the running boot, negatives go backwards.
    let mut ordered = boots;
    ordered.sort_by_key(|boot| -boot.get("index").and_then(Json::as_i64).unwrap_or(0));

    for boot in ordered.iter().take(LIMIT) {
        let index = boot.get("index").and_then(Json::as_i64).unwrap_or(0);
        let id = boot
            .get("boot_id")
            .and_then(Json::as_str)
            .unwrap_or("unknown");

        // Journal timestamps are microseconds since the epoch.
        let first = boot.get("first_entry").and_then(Json::as_i64);
        let last = boot.get("last_entry").and_then(Json::as_i64);

        let mut detail = Vec::new();
        if let Some(first) = first {
            detail.push(format_timestamp(first / 1_000_000));
        }
        if let (Some(first), Some(last)) = (first, last)
            && last > first
        {
            detail.push(format!(
                "ran {}",
                format_duration(((last - first) / 1_000_000) as u64)
            ));
        }
        // The id is what `journalctl -b <id>` takes, so it earns its space.
        detail.push(format!("id {}", &id[..id.len().min(12)]));

        node.push(Node::field(
            if index == 0 {
                "current".to_string()
            } else {
                index.to_string()
            },
            detail.join(" · "),
        ));
    }

    if ordered.len() > LIMIT {
        node.push(Node::field(
            "Older boots",
            format!("{} not shown", ordered.len() - LIMIT),
        ));
    }
    node
}

/// Seconds since the Unix epoch.
fn unix_now() -> Option<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_boot_is_always_described() {
        let nodes = Boots.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Current boot");
        // The boot id comes from /proc and needs no external tool.
        assert!(
            nodes[0].children.iter().any(|c| c.label == "Boot ID"),
            "the running boot should always be identified"
        );
    }

    #[test]
    fn history_explains_itself_when_unavailable() {
        let node = history();
        assert!(!node.children.is_empty(), "either boots or a reason");
    }

    #[test]
    fn boots_are_listed_newest_first() {
        let node = history();
        let indices: Vec<i64> = node
            .children
            .iter()
            .filter_map(|c| {
                if c.label == "current" {
                    Some(0)
                } else {
                    c.label.parse().ok()
                }
            })
            .collect();

        // Descending: 0, -1, -2, … so the running boot leads.
        for pair in indices.windows(2) {
            assert!(pair[0] > pair[1], "{indices:?} is not newest-first");
        }
    }

    #[test]
    fn journal_output_parses_despite_inherited_colour_settings() {
        // journalctl colours its JSON when SYSTEMD_COLORS is set, which the
        // probe layer now suppresses. If that regressed, the parse fails and
        // the section says so instead of listing boots.
        if !probe::available("journalctl") {
            return;
        }
        let node = history();
        let status = node
            .children
            .iter()
            .find(|c| c.label == "Status")
            .map(|c| c.value.as_ref().unwrap().to_string());
        if let Some(status) = status {
            assert!(
                !status.contains("could not be parsed"),
                "journal JSON failed to parse: {status}"
            );
        }
    }
}
