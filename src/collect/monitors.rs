//! Displays attached to each DRM connector, identified from their EDID.

use anyhow::Result;

use crate::collect::edid;
use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::sysfs;

const DRM: &str = "/sys/class/drm";

pub struct Monitors;

impl Collector for Monitors {
    fn id(&self) -> &'static str {
        "monitors"
    }

    fn title(&self) -> &'static str {
        "Monitors"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let connectors = connectors();

        let connected = connectors
            .iter()
            .filter(|c| c.status.as_deref() == Some("connected"))
            .count();

        let mut overview = Node::group("Displays", vec![]);
        overview.push(Node::field("Connectors", connectors.len()));
        overview.push(Node::field("Connected", connected));

        let mut nodes = vec![overview];

        // Connected outputs first, then the empty ports: a laptop exposes a
        // dozen connectors and one display, and the display is the point.
        let (attached, idle): (Vec<&Connector>, Vec<&Connector>) = connectors
            .iter()
            .partition(|c| c.status.as_deref() == Some("connected"));

        for connector in attached {
            nodes.push(describe(connector));
        }
        if !idle.is_empty() {
            let mut section = Node::group("Unused connectors", vec![]);
            for connector in idle {
                section.push(Node::field(
                    &connector.name,
                    connector.status.clone().unwrap_or_else(|| "unknown".into()),
                ));
            }
            nodes.push(section);
        }
        Ok(nodes)
    }
}

struct Connector {
    name: String,
    path: std::path::PathBuf,
    status: Option<String>,
}

fn connectors() -> Vec<Connector> {
    sysfs::list_dir(DRM)
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().to_string();
            // Connectors are `cardN-<output>`; `cardN` alone is the device and
            // `renderDN` is the render node.
            if !name.contains('-') {
                return None;
            }
            let status = sysfs::read(path.join("status"));
            Some(Connector { name, path, status })
        })
        .collect()
}

fn describe(connector: &Connector) -> Node {
    let decoded = edid::read(&connector.path.join("edid"));

    // Prefer the display's own name for the heading; fall back to the
    // connector, which at least says which port it is plugged into.
    let heading = match decoded.as_ref().and_then(|e| e.name.clone()) {
        Some(name) => format!("{}  {name}", connector.name),
        None => connector.name.clone(),
    };
    let mut node = Node::group(heading, vec![]);

    node.push_opt("Status", connector.status.clone());
    node.push_opt("Enabled", sysfs::read(connector.path.join("enabled")));
    node.push_opt("Power state", sysfs::read(connector.path.join("dpms")));

    // Modes the driver is willing to set, highest first in the kernel's order.
    if let Some(modes) = sysfs::read(connector.path.join("modes")) {
        let list: Vec<&str> = modes.lines().collect();
        if let Some(current) = list.first() {
            node.push(Node::field("Current mode", *current));
        }
        if list.len() > 1 {
            // Deduplicate: a connector commonly lists the same resolution at
            // several refresh rates, which the modes file does not show.
            let mut unique: Vec<&str> = list.clone();
            unique.dedup();
            node.push(Node::field("Available modes", unique.len()));
        }
    }

    let Some(decoded) = decoded else {
        // A connected output with no EDID is a real and awkward state: a KVM
        // or a bad cable will do it, and the mode list comes from the driver's
        // fallback rather than the display.
        node.push(Node::field(
            "EDID",
            "not provided by the display — modes above are the driver's defaults",
        ));
        return node;
    };

    node.push(Node::field("Manufacturer", decoded.manufacturer.clone()));
    node.push_opt("Model name", decoded.name.clone());
    node.push(Node::field(
        "Product code",
        format!("0x{:04x}", decoded.product_code),
    ));
    node.push_opt("Serial number", decoded.serial_text.clone());
    node.push(Node::field("EDID version", decoded.version.clone()));
    node.push(Node::field(
        "Input",
        if decoded.digital { "digital" } else { "analog" },
    ));

    if let Some((width, height, hz)) = decoded.preferred_mode {
        node.push(Node::field(
            "Preferred mode",
            format!("{width}×{height} at {hz:.0} Hz"),
        ));
    }
    if let Some((w, h)) = decoded.size_cm {
        let size = match decoded.diagonal_inches() {
            Some(inches) => format!("{w} × {h} cm ({inches:.1}″ diagonal)"),
            None => format!("{w} × {h} cm"),
        };
        node.push(Node::field("Physical size", size));

        // Pixel density is what actually decides whether scaling is needed.
        if let Some((pixels_w, pixels_h, _)) = decoded.preferred_mode
            && w > 0
            && h > 0
        {
            let dpi = pixels_w as f64 / (w as f64 / 2.54);
            let dpi_v = pixels_h as f64 / (h as f64 / 2.54);
            node.push(Node::field(
                "Pixel density",
                format!("{:.0} dpi", (dpi + dpi_v) / 2.0),
            ));
        }
    }
    match (decoded.week, decoded.year) {
        (Some(week), Some(year)) => {
            node.push(Node::field(
                "Manufactured",
                format!("week {week} of {year}"),
            ));
        }
        (None, Some(year)) => node.push(Node::field("Manufactured", year.to_string())),
        _ => {}
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connectors_exclude_cards_and_render_nodes() {
        for connector in connectors() {
            assert!(
                connector.name.contains('-'),
                "{} is not a connector",
                connector.name
            );
            assert!(!connector.name.starts_with("renderD"));
        }
    }

    #[test]
    fn connected_outputs_come_before_idle_ones() {
        let nodes = Monitors.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Displays");

        // Once the unused-connector section appears, nothing follows it.
        if let Some(index) = nodes.iter().position(|n| n.label == "Unused connectors") {
            assert_eq!(index, nodes.len() - 1, "idle connectors should be last");
        }
    }

    #[test]
    fn a_connected_display_is_identified() {
        let connected: Vec<Connector> = connectors()
            .into_iter()
            .filter(|c| c.status.as_deref() == Some("connected"))
            .collect();

        for connector in &connected {
            let node = describe(connector);
            // Either the EDID was decoded, or the page says it was absent.
            let has_manufacturer = node.children.iter().any(|c| c.label == "Manufacturer");
            let explains = node.children.iter().any(|c| c.label == "EDID");
            assert!(
                has_manufacturer || explains,
                "{} neither identified nor explained",
                connector.name
            );
        }
    }
}
