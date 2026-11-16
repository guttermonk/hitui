//! Loaded kernel modules.
//!
//! /proc/modules is the authoritative list — what `lsmod` reads — and
//! /sys/module adds per-module state.
//!
//! Descriptions are deliberately absent. They live in the module files, which
//! only `modinfo` can read, and the cost is decompressing every `.ko.xz` on
//! disk rather than spawning a process: batching all 213 modules of this
//! machine into one `modinfo` call still measured 32 seconds. That is a page
//! that hangs, which is worse than a page without a description column.
//! Adding them back needs background collection or an on-disk cache, not a
//! longer timeout.
//!
//! Also decodes /proc/sys/kernel/tainted, which is where the consequences of
//! loading modules show up. "4097" tells a reader nothing; "proprietary
//! module loaded, out-of-tree module loaded" tells them everything.

use anyhow::{Context, Result};

use crate::collect::{Collector, Group};
use crate::model::{Node, Value, format_bytes};
use crate::sysfs;

pub struct Modules;

impl Collector for Modules {
    fn id(&self) -> &'static str {
        "modules"
    }

    fn title(&self) -> &'static str {
        "Kernel Modules"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let raw = std::fs::read_to_string("/proc/modules").context("reading /proc/modules")?;
        let modules = parse_modules(&raw);

        let mut overview = Node::group("Kernel modules", vec![]);
        overview.push(Node::field("Loaded", modules.len()));
        overview.push(Node::field(
            "Total size",
            Value::Bytes(modules.iter().map(|m| m.size).sum()),
        ));
        overview.push_opt(
            "Signature enforcement",
            // Absent unless CONFIG_MODULE_SIG_FORCE or the parameter is set.
            sysfs::read("/sys/module/module/parameters/sig_enforce")
                .map(|v| Value::Flag(v.trim() == "Y" || v.trim() == "1")),
        );
        if let Some(taint) = taint() {
            overview.push(taint);
        }

        let mut nodes = vec![overview];

        // One line per module rather than a section each: 200 collapsible
        // groups is not navigable, and `/` already filters by any field.
        let mut listing = Node::group("Loaded modules", vec![]);
        for module in &modules {
            listing.push(Node::field(&module.name, module.summary()));
        }
        nodes.push(listing);

        Ok(nodes)
    }
}

struct Module {
    name: String,
    size: u64,
    /// How many things hold a reference to this module.
    refcount: u64,
    /// Modules that depend on this one.
    used_by: Vec<String>,
    /// "Live", "Loading" or "Unloading".
    state: String,
}

impl Module {
    /// A one-line description: size, state, and what is holding it loaded.
    fn summary(&self) -> String {
        let mut parts = vec![format_bytes(self.size)];

        // Only mention state when it is not the ordinary one; a list where
        // every row says "live" wastes the column.
        if !self.state.eq_ignore_ascii_case("live") {
            parts.push(self.state.to_lowercase());
        }
        if self.used_by.is_empty() {
            // A zero refcount means it could be unloaded, which is the useful
            // reading of this column.
            if self.refcount == 0 {
                parts.push("unused".to_string());
            } else {
                parts.push(format!("{} references", self.refcount));
            }
        } else {
            parts.push(format!("used by {}", self.used_by.join(", ")));
        }
        parts.join(" · ")
    }
}

/// Parse /proc/modules: `name size refcount deps state address`.
fn parse_modules(text: &str) -> Vec<Module> {
    let mut modules: Vec<Module> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?.to_string();
            let size = fields.next()?.parse().ok()?;
            let refcount = fields.next()?.parse().unwrap_or(0);

            // The dependency field is a comma-terminated list, or "-" when
            // nothing depends on the module.
            let used_by = match fields.next() {
                Some("-") | None => Vec::new(),
                Some(deps) => deps
                    .split(',')
                    .filter(|d| !d.is_empty())
                    .map(str::to_string)
                    .collect(),
            };
            let state = fields.next().unwrap_or("unknown").to_string();

            Some(Module {
                name,
                size,
                refcount,
                used_by,
                state,
            })
        })
        .collect();

    // /proc/modules is in load order, which is near-random to read.
    modules.sort_by(|a, b| a.name.cmp(&b.name));
    modules
}

/// Decode /proc/sys/kernel/tainted into the reasons behind it.
fn taint() -> Option<Node> {
    // Bit positions are stable kernel ABI, documented in
    // Documentation/admin-guide/tainted-kernels.rst.
    const REASONS: [(u32, &str); 19] = [
        (0, "proprietary module loaded"),
        (1, "module force loaded"),
        (2, "SMP kernel oops"),
        (3, "module force unloaded"),
        (4, "machine check exception"),
        (5, "bad page encountered"),
        (6, "taint requested by userspace"),
        (7, "died recently"),
        (8, "ACPI table overridden"),
        (9, "kernel issued a warning"),
        (10, "staging driver loaded"),
        (11, "firmware workaround applied"),
        (12, "out-of-tree module loaded"),
        (13, "unsigned module loaded"),
        (14, "soft lockup occurred"),
        (15, "kernel live patched"),
        (16, "auxiliary taint"),
        (17, "struct randomisation mismatch"),
        (18, "in-kernel test executed"),
    ];

    let value = sysfs::read_u64("/proc/sys/kernel/tainted")?;
    if value == 0 {
        return Some(Node::field("Taint", "none"));
    }

    let reasons: Vec<String> = REASONS
        .iter()
        .filter(|(bit, _)| value & (1 << bit) != 0)
        .map(|(_, reason)| reason.to_string())
        .collect();

    let mut node = Node::group(format!("Taint ({value})"), vec![]);
    for reason in reasons {
        node.push(Node::field(reason, "yes"));
    }
    // An unrecognised bit is better flagged than silently dropped.
    if node.children.is_empty() {
        node.push(Node::field(
            "Reason",
            format!("unrecognised flags ({value})"),
        ));
    }
    Some(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_modules() {
        let text = "sd_mod 86016 0 - Live 0x0000000000000000\n\
                    scsi_mod 339968 1 sd_mod, Live 0x0000000000000000\n\
                    scsi_common 16384 2 sd_mod,scsi_mod, Live 0x0000000000000000\n";
        let modules = parse_modules(text);
        assert_eq!(modules.len(), 3);

        // Sorted by name, not load order.
        assert_eq!(modules[0].name, "scsi_common");
        assert_eq!(modules[0].used_by, vec!["sd_mod", "scsi_mod"]);
        assert_eq!(modules[0].refcount, 2);

        let sd_mod = modules.iter().find(|m| m.name == "sd_mod").unwrap();
        assert_eq!(sd_mod.size, 86016);
        assert!(
            sd_mod.used_by.is_empty(),
            "\"-\" means nothing depends on it"
        );
    }

    #[test]
    fn summarises_a_module_in_one_line() {
        let text = "scsi_mod 339968 1 sd_mod, Live 0x0\nsd_mod 86016 0 - Live 0x0\n";
        let modules = parse_modules(text);

        let sd_mod = modules.iter().find(|m| m.name == "sd_mod").unwrap();
        let summary = sd_mod.summary();
        assert!(summary.contains("84.00 KiB"), "{summary}");
        assert!(summary.contains("unused"), "{summary}");
        // "live" is the normal state and would be noise on every row.
        assert!(!summary.contains("live"), "{summary}");

        let scsi_mod = modules.iter().find(|m| m.name == "scsi_mod").unwrap();
        assert!(scsi_mod.summary().contains("used by sd_mod"));
    }

    #[test]
    fn an_unusual_state_is_reported() {
        let modules = parse_modules("weird_mod 4096 0 - Unloading 0x0\n");
        assert!(modules[0].summary().contains("unloading"));
    }

    #[test]
    fn decodes_taint_bits_into_reasons() {
        // This machine reports 4097: bit 0 and bit 12.
        let node = taint().expect("/proc/sys/kernel/tainted is readable");
        if node.label == "Taint" {
            assert_eq!(node.value.unwrap().to_string(), "none");
        } else {
            assert!(node.label.starts_with("Taint ("), "{}", node.label);
            assert!(!node.children.is_empty(), "a tainted kernel must say why");
            for reason in &node.children {
                assert!(
                    !reason.label.contains("unrecognised"),
                    "unknown taint bit: {}",
                    reason.label
                );
            }
        }
    }

    #[test]
    fn collects_on_this_machine() {
        let nodes = Modules.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Kernel modules");
        assert_eq!(nodes[1].label, "Loaded modules");

        // Every loaded module is listed, and the count agrees.
        let loaded = nodes[0]
            .children
            .iter()
            .find(|c| c.label == "Loaded")
            .and_then(|c| c.value.clone())
            .map(|v| v.to_string())
            .unwrap();
        assert_eq!(loaded, nodes[1].children.len().to_string());
    }
}
