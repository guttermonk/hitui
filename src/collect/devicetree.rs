//! Device tree, from /proc/device-tree.
//!
//! The canonical hardware description on ARM and RISC-V. It also turns up on
//! x86 under EFI, where the kernel exposes a small stub rather than a full
//! tree — so "present" does not mean "this is a device-tree platform", and the
//! page distinguishes the two.

use std::path::Path;

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::sysfs;

const DT: &str = "/proc/device-tree";

/// Depth to walk before stopping.
///
/// A full tree on a server board runs to thousands of nodes; three levels
/// covers the machine identity and its buses, which is what the page is for.
const MAX_DEPTH: usize = 3;

pub struct DeviceTree;

impl Collector for DeviceTree {
    fn id(&self) -> &'static str {
        "devicetree"
    }

    fn title(&self) -> &'static str {
        "Device Tree"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        if !Path::new(DT).is_dir() {
            return Ok(vec![Node::group(
                "Device tree",
                vec![Node::field(
                    "Status",
                    "not present — this platform describes itself through ACPI and SMBIOS",
                )],
            )]);
        }

        let mut overview = Node::group("Device tree", vec![]);
        overview.push_opt("Model", property("model"));
        overview.push_opt("Compatible", property("compatible"));
        overview.push_opt("Serial number", property("serial-number"));

        // An EFI stub has no `model` and only a handful of children; a real
        // device-tree platform describes the whole board.
        let children = sysfs::list_dir(DT).iter().filter(|p| p.is_dir()).count();
        overview.push(Node::field(
            "Kind",
            if property("model").is_some() {
                "full tree — this is a device-tree platform"
            } else {
                "stub only — exposed by EFI, not a device-tree platform"
            },
        ));
        overview.push(Node::field("Top-level nodes", children));

        let mut nodes = vec![overview];
        if let Some(tree) = walk(Path::new(DT), 0) {
            nodes.push(Node::group("Nodes", tree));
        }
        Ok(nodes)
    }
}

/// Read a top-level property as text.
///
/// Device-tree strings are NUL-terminated, and `compatible` holds several
/// concatenated together.
fn property(name: &str) -> Option<String> {
    let bytes = std::fs::read(Path::new(DT).join(name)).ok()?;
    let text = String::from_utf8_lossy(&bytes)
        .split('\0')
        .filter(|s| !s.is_empty())
        .collect::<Vec<&str>>()
        .join(", ");
    (!text.is_empty()).then_some(text)
}

/// Recursively describe the tree, bounded by [`MAX_DEPTH`].
fn walk(dir: &Path, depth: usize) -> Option<Vec<Node>> {
    if depth >= MAX_DEPTH {
        return None;
    }
    let mut nodes = Vec::new();

    for entry in sysfs::list_dir(dir) {
        let Some(name) = entry.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        if !entry.is_dir() {
            continue;
        }

        let mut node = Node::group(name, vec![]);
        // Name the node's type and identity before recursing, so a collapsed
        // subtree still says what it is.
        for property in ["compatible", "device_type", "status", "reg"] {
            if let Some(value) = readable(&entry.join(property)) {
                node.push(Node::field(property, value));
            }
        }
        if let Some(children) = walk(&entry, depth + 1) {
            node.children.extend(children);
        } else if depth + 1 >= MAX_DEPTH {
            let deeper = sysfs::list_dir(&entry)
                .iter()
                .filter(|p| p.is_dir())
                .count();
            if deeper > 0 {
                node.push(Node::field(
                    "Child nodes",
                    format!("{deeper} — not shown, past the depth limit"),
                ));
            }
        }
        nodes.push(node);
    }
    (!nodes.is_empty()).then_some(nodes)
}

/// A property as text, if it is text at all.
///
/// Device-tree properties are untyped bytes. Printable ones are strings;
/// `reg` and friends are big-endian integers and are shown as hex rather than
/// as mojibake.
fn readable(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }

    let printable = bytes
        .iter()
        .all(|b| *b == 0 || (*b >= 0x20 && *b < 0x7f) || *b == b'\n');
    if printable {
        let text = String::from_utf8_lossy(&bytes)
            .split('\0')
            .filter(|s| !s.is_empty())
            .collect::<Vec<&str>>()
            .join(", ");
        return (!text.is_empty()).then_some(text);
    }

    // Cap the hex dump: `reg` on a big node can run to hundreds of bytes.
    let hex: String = bytes
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<String>>()
        .join(" ");
    Some(if bytes.len() > 16 {
        format!("{hex} … ({} bytes)", bytes.len())
    } else {
        hex
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_nul_separated_strings() {
        // `compatible` holds several NUL-terminated strings back to back.
        let dir = std::env::temp_dir().join("hitui-dt-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("compatible");
        std::fs::write(&path, b"brand,board-v2\0brand,board\0").unwrap();

        assert_eq!(readable(&path).unwrap(), "brand,board-v2, brand,board");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn binary_properties_render_as_hex_not_mojibake() {
        let dir = std::env::temp_dir().join("hitui-dt-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reg");
        std::fs::write(&path, [0x00, 0x00, 0x80, 0xff, 0x01]).unwrap();

        let value = readable(&path).unwrap();
        assert_eq!(value, "00 00 80 ff 01");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn long_binary_properties_are_capped() {
        let dir = std::env::temp_dir().join("hitui-dt-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big");
        std::fs::write(&path, vec![0xff; 64]).unwrap();

        let value = readable(&path).unwrap();
        assert!(value.contains("64 bytes"), "{value}");
        assert!(value.len() < 100, "dump should be truncated: {value}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn distinguishes_a_stub_from_a_real_tree() {
        // x86 under EFI exposes a stub with no `model`, which must not be
        // reported as a device-tree platform.
        let nodes = DeviceTree.collect().expect("collection failed");
        let kind = nodes[0]
            .children
            .iter()
            .find(|c| c.label == "Kind" || c.label == "Status")
            .expect("the tree's nature must be stated");
        let value = kind.value.as_ref().unwrap().to_string();

        if Path::new(DT).is_dir() && property("model").is_none() {
            assert!(value.contains("stub"), "{value}");
        }
    }
}
