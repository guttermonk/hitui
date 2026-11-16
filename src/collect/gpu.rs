//! Graphics processors.
//!
//! Each DRM device maps back to a PCI function, so identity comes from the
//! same sysfs attributes and the same optional `lspci` enrichment as the PCI
//! page. The OpenGL renderer string is not in sysfs at all — it requires
//! making a GL context — so `glxinfo` supplies it when present.

use anyhow::Result;

use crate::collect::pci;
use crate::collect::{Collector, Group};
use crate::model::{Node, Value};
use crate::probe;
use crate::sysfs;

const DRM: &str = "/sys/class/drm";

pub struct Gpu;

impl Collector for Gpu {
    fn id(&self) -> &'static str {
        "gpu"
    }

    fn title(&self) -> &'static str {
        "Graphics Processors"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let cards = cards();
        let names = pci::lspci_names();

        let mut overview = Node::group("Graphics", vec![]);
        overview.push(Node::field("Devices", cards.len()));
        overview.push_opt("DRM version", sysfs::read(format!("{DRM}/version")));

        let mut nodes = vec![overview];
        for card in &cards {
            nodes.push(card_node(card, &names));
        }
        if let Some(renderer) = renderer() {
            nodes.push(renderer);
        }
        Ok(nodes)
    }
}

/// DRM card devices, excluding connectors and render nodes.
fn cards() -> Vec<String> {
    sysfs::list_dir(DRM)
        .into_iter()
        .filter_map(|path| path.file_name().map(|n| n.to_string_lossy().to_string()))
        .filter(|name| {
            name.starts_with("card")
                && !name.contains('-')
                && name["card".len()..].chars().all(|c| c.is_ascii_digit())
        })
        .collect()
}

fn card_node(card: &str, names: &std::collections::BTreeMap<String, pci::DeviceNames>) -> Node {
    let dir = std::path::Path::new(DRM).join(card);
    let device = dir.join("device");

    // The PCI address is the link target's name, and the key into lspci.
    let address = std::fs::read_link(&device)
        .ok()
        .and_then(|target| target.file_name().map(|n| n.to_string_lossy().to_string()));
    let pci_names = address.as_deref().and_then(|a| names.get(a));

    let heading = match pci_names.map(|n| n.device.clone()) {
        Some(name) => format!("{card}  {name}"),
        None => card.to_string(),
    };
    let mut node = Node::group(heading, vec![]);
    if pci_names.is_some() {
        node = node.from("lspci");
    }

    node.push_opt("PCI address", address.clone());
    node.push_opt("Vendor", pci_names.map(|n| n.vendor.clone()));
    node.push_opt("Device", pci_names.map(|n| n.device.clone()));
    node.push_opt(
        "Driver",
        std::fs::read_link(device.join("driver"))
            .ok()
            .and_then(|target| target.file_name().map(|n| n.to_string_lossy().to_string())),
    );
    // Which card the firmware used to boot — the one that showed the POST
    // screen, and usually the one driving the internal panel.
    node.push_opt(
        "Primary display",
        sysfs::read_u64(device.join("boot_vga")).map(|v| Value::Flag(v != 0)),
    );

    // Only amdgpu publishes VRAM totals in sysfs; Intel shares system memory
    // and NVIDIA's proprietary driver keeps it in /proc.
    node.push_opt(
        "Video memory",
        sysfs::read_u64(device.join("mem_info_vram_total")).map(Value::Bytes),
    );

    let attached = connectors_of(card);
    if !attached.is_empty() {
        let connected = attached
            .iter()
            .filter(|(_, status)| status == "connected")
            .count();
        let mut section = Node::group(
            format!("Connectors ({connected} of {} in use)", attached.len()),
            vec![],
        );
        for (name, status) in attached {
            section.push(Node::field(name, status));
        }
        node.push(section);
    }
    node
}

/// Connectors belonging to a card, with their link status.
fn connectors_of(card: &str) -> Vec<(String, String)> {
    let prefix = format!("{card}-");
    sysfs::list_dir(DRM)
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().to_string();
            let output = name.strip_prefix(&prefix)?.to_string();
            let status = sysfs::read(path.join("status")).unwrap_or_else(|| "unknown".into());
            Some((output, status))
        })
        .collect()
}

/// The OpenGL implementation actually in use.
///
/// Reported by whichever card the current session's GL context lands on, so
/// on a hybrid-graphics machine this is one of the cards above, not all of
/// them.
fn renderer() -> Option<Node> {
    let Some(output) = probe::run("glxinfo", &["-B"]) else {
        return Some(Node::group(
            "OpenGL",
            vec![Node::field(
                "Renderer",
                if probe::available("glxinfo") {
                    "unavailable — glxinfo could not open a display"
                } else {
                    "unavailable — install mesa-utils or glxinfo for the renderer string"
                },
            )],
        ));
    };

    let mut node = Node::group("OpenGL", vec![]).from("glxinfo");
    for line in output.lines() {
        let line = line.trim();
        // glxinfo -B prints a handful of "Label: value" lines among prose.
        for (label, prefix) in [
            ("Vendor", "OpenGL vendor string:"),
            ("Renderer", "OpenGL renderer string:"),
            ("Version", "OpenGL version string:"),
            (
                "Shading language",
                "OpenGL shading language version string:",
            ),
            (
                "Core profile version",
                "OpenGL core profile version string:",
            ),
        ] {
            if let Some(value) = line.strip_prefix(prefix) {
                node.push(Node::field(label, value.trim()));
            }
        }
        if let Some(value) = line.strip_prefix("Device:") {
            node.push(Node::field("Device", value.trim()));
        }
    }

    if node.children.is_empty() {
        node.push(Node::field(
            "Renderer",
            "glxinfo ran but reported no OpenGL strings",
        ));
    }
    Some(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_exclude_connectors_and_render_nodes() {
        for card in cards() {
            assert!(card.starts_with("card"), "{card}");
            assert!(!card.contains('-'), "{card} is a connector");
            assert!(!card.starts_with("renderD"), "{card} is a render node");
        }
    }

    #[test]
    fn connectors_are_attributed_to_their_card() {
        for card in cards() {
            for (output, status) in connectors_of(&card) {
                // The card prefix is stripped, leaving the output name.
                assert!(!output.starts_with(&card), "{output} kept its prefix");
                assert!(!output.is_empty());
                assert!(!status.is_empty());
            }
        }
    }

    #[test]
    fn every_card_is_described() {
        let nodes = Gpu.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Graphics");

        let count = nodes[0].children[0].value.as_ref().unwrap().to_string();
        assert_eq!(count, cards().len().to_string());

        // Each card reports a driver binding or says none is bound.
        for node in nodes.iter().skip(1) {
            if node.label == "OpenGL" {
                continue;
            }
            assert!(
                node.children.iter().any(|c| c.label == "PCI address"),
                "{} has no PCI address",
                node.label
            );
        }
    }

    #[test]
    fn opengl_section_always_states_something() {
        // Either the renderer strings, or why they are missing.
        let node = renderer().expect("a section either way");
        assert!(!node.children.is_empty());
    }
}
