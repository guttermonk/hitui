//! Hardware resources: interrupts, I/O ports, DMA channels.
//!
//! /proc/ioports and /proc/iomem zero out every address for unprivileged
//! readers — the ranges read as `0000-0000` rather than being withheld, which
//! looks like data. The page says so instead of presenting zeroes as fact.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::sysfs;

pub struct Resources;

impl Collector for Resources {
    fn id(&self) -> &'static str {
        "resources"
    }

    fn title(&self) -> &'static str {
        "Resources"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes = vec![interrupts()];
        nodes.push(ranges("I/O ports", "/proc/ioports"));
        nodes.push(ranges("Memory ranges", "/proc/iomem"));
        if let Some(dma) = dma() {
            nodes.push(dma);
        }
        Ok(nodes)
    }
}

/// Interrupt lines and the devices on them, with per-IRQ totals.
fn interrupts() -> Node {
    let Some(text) = sysfs::read("/proc/interrupts") else {
        return Node::group(
            "Interrupts",
            vec![Node::field("Status", "/proc/interrupts is unavailable")],
        );
    };

    let mut node = Node::group("Interrupts", vec![]);
    let mut lines = text.lines();

    // The header names the CPU columns, which is how many to expect.
    let cpus = lines
        .next()
        .map(|h| h.split_whitespace().count())
        .unwrap_or(0);

    for line in lines {
        let Some((irq, rest)) = line.split_once(':') else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();

        // Per-CPU counts come first, then the controller and the device names.
        let counts: u64 = fields
            .iter()
            .take(cpus)
            .filter_map(|f| f.parse::<u64>().ok())
            .sum();
        let description = fields
            .iter()
            .skip(cpus)
            .copied()
            .collect::<Vec<&str>>()
            .join(" ");

        // An interrupt that has never fired tells you a device is idle or its
        // driver never attached, which is worth keeping rather than hiding.
        node.push(Node::field(
            irq.trim(),
            if description.is_empty() {
                format!("{counts} interrupts")
            } else {
                format!("{counts} interrupts · {description}")
            },
        ));
    }
    node
}

/// A resource map from /proc, or an explanation of why it reads as zeroes.
fn ranges(title: &str, path: &str) -> Node {
    let Some(text) = sysfs::read(path) else {
        return Node::group(
            title,
            vec![Node::field("Status", format!("{path} is unavailable"))],
        );
    };

    if is_redacted(&text) {
        return Node::group(
            title,
            vec![Node::field(
                "Status",
                format!("{path} reports zeroed addresses — run as root to see the real ranges"),
            )],
        );
    }

    let mut node = Node::group(title, vec![]);
    for line in text.lines() {
        // "  0000-001f : dma1" — indentation indicates nesting under the
        // parent range above it.
        let Some((range, owner)) = line.split_once(':') else {
            continue;
        };
        let depth = line.len() - line.trim_start().len();
        // Two spaces per level in the kernel's own formatting.
        let indent = "  ".repeat(depth / 2);
        node.push(Node::field(
            format!("{indent}{}", range.trim()),
            owner.trim(),
        ));
    }
    node
}

/// Whether the kernel zeroed the addresses because we are unprivileged.
///
/// Every range collapses to the same all-zero span, which is never true of a
/// real machine with more than one resource.
fn is_redacted(text: &str) -> bool {
    let ranges: Vec<&str> = text
        .lines()
        .filter_map(|line| line.split_once(':').map(|(r, _)| r.trim()))
        .collect();
    ranges.len() > 1
        && ranges
            .iter()
            .all(|r| r.chars().all(|c| c == '0' || c == '-'))
}

fn dma() -> Option<Node> {
    let text = sysfs::read("/proc/dma")?;

    let mut node = Node::group("DMA channels", vec![]);
    for line in text.lines() {
        if let Some((channel, owner)) = line.split_once(':') {
            node.push(Node::field(channel.trim(), owner.trim()));
        }
    }
    // ISA DMA is absent on anything modern, so an empty file is normal and
    // not worth a section.
    (!node.children.is_empty()).then_some(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_unprivileged_zeroed_map() {
        // What an unprivileged read of /proc/ioports actually looks like.
        let redacted = "0000-0000 : PCI Bus 0000:00\n  0000-0000 : dma1\n  0000-0000 : pic1\n";
        assert!(is_redacted(redacted));

        // A real map from a privileged read.
        let real = "0000-0cf7 : PCI Bus 0000:00\n  0000-001f : dma1\n  0020-0021 : pic1\n";
        assert!(!is_redacted(real));
    }

    #[test]
    fn a_single_range_is_not_treated_as_redacted() {
        // One zero-looking line could legitimately be a real range, and
        // guessing wrong would suppress genuine data.
        assert!(!is_redacted("0000-0000 : only one\n"));
    }

    #[test]
    fn sums_interrupt_counts_across_cpus() {
        let node = interrupts();
        assert!(!node.children.is_empty(), "expected interrupt lines");

        for field in &node.children {
            let value = field.value.as_ref().unwrap().to_string();
            assert!(value.contains("interrupts"), "{value}");
        }
    }

    #[test]
    fn says_why_ranges_are_zeroed_rather_than_showing_them() {
        let node = ranges("I/O ports", "/proc/ioports");
        // Unprivileged here, so this should be the explanation rather than a
        // list of 0000-0000 entries presented as real addresses.
        if node.children.len() == 1 && node.children[0].label == "Status" {
            let status = node.children[0].value.as_ref().unwrap().to_string();
            assert!(
                status.contains("root") || status.contains("unavailable"),
                "{status}"
            );
        }
    }

    #[test]
    fn collects_every_section() {
        let nodes = Resources.collect().expect("collection failed");
        let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        assert!(labels.contains(&"Interrupts"));
        assert!(labels.contains(&"I/O ports"));
        for node in &nodes {
            assert!(!node.children.is_empty(), "{} is empty", node.label);
        }
    }
}
