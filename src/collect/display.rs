//! The graphical session: display server, compositor, and active outputs.
//!
//! Everything here is read from the environment and from DRM. There is no
//! kernel interface that reports "which compositor is running", so the
//! conventional `XDG_*` variables are the source — which means this describes
//! the session hitui was launched from, not the machine as a whole.

use anyhow::Result;

use crate::collect::edid;
use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::sysfs;

const DRM: &str = "/sys/class/drm";

pub struct Display;

impl Collector for Display {
    fn id(&self) -> &'static str {
        "display"
    }

    fn title(&self) -> &'static str {
        "Display"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes = vec![session(), outputs()];
        if let Some(backlight) = backlight() {
            nodes.push(backlight);
        }
        Ok(nodes)
    }
}

fn session() -> Node {
    let mut node = Node::group("Session", vec![]);
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());

    // XDG_SESSION_TYPE is authoritative when set; otherwise the presence of
    // WAYLAND_DISPLAY or DISPLAY says which server is reachable.
    let kind = var("XDG_SESSION_TYPE").or_else(|| {
        if var("WAYLAND_DISPLAY").is_some() {
            Some("wayland".to_string())
        } else if var("DISPLAY").is_some() {
            Some("x11".to_string())
        } else {
            None
        }
    });
    node.push(Node::field(
        "Display server",
        kind.unwrap_or_else(|| "none — this is a text console or remote shell".to_string()),
    ));

    node.push_opt("Desktop", var("XDG_CURRENT_DESKTOP"));
    node.push_opt("Session desktop", var("XDG_SESSION_DESKTOP"));
    node.push_opt("Wayland display", var("WAYLAND_DISPLAY"));
    node.push_opt("X display", var("DISPLAY"));
    node.push_opt("Terminal", var("TERM"));
    node.push_opt("Terminal program", var("TERM_PROGRAM"));

    // Scaling factors explain a surprising rendered size more often than the
    // panel's own resolution does.
    node.push_opt("GDK scale", var("GDK_SCALE"));
    node.push_opt("Qt scale factor", var("QT_SCALE_FACTOR"));
    node
}

/// Active outputs and the mode each is running.
fn outputs() -> Node {
    let mut node = Node::group("Active outputs", vec![]);

    for path in sysfs::list_dir(DRM) {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        if !name.contains('-') {
            continue;
        }
        if sysfs::read(path.join("status")).as_deref() != Some("connected") {
            continue;
        }

        // The first entry in `modes` is what the driver has set.
        let mode = sysfs::read(path.join("modes"))
            .and_then(|modes| modes.lines().next().map(str::to_string));
        let decoded = edid::read(&path.join("edid"));

        let mut detail = Vec::new();
        if let Some(mode) = mode {
            detail.push(mode);
        }
        if let Some(inches) = decoded.as_ref().and_then(|e| e.diagonal_inches()) {
            detail.push(format!("{inches:.1}″"));
        }
        if let Some(panel) = decoded.as_ref().and_then(|e| e.name.clone()) {
            detail.push(panel);
        } else if let Some(maker) = decoded.as_ref().map(|e| e.manufacturer.clone()) {
            detail.push(maker);
        }

        node.push(Node::field(
            name,
            if detail.is_empty() {
                "connected, mode unknown".to_string()
            } else {
                detail.join(" · ")
            },
        ));
    }

    if node.children.is_empty() {
        node.push(Node::field(
            "Status",
            "no connected outputs — headless, or the display is driven by a \
             framebuffer the kernel does not expose through DRM",
        ));
    }
    node
}

/// Panel backlights, which only exist on built-in displays.
fn backlight() -> Option<Node> {
    let mut node = Node::group("Backlight", vec![]);

    for device in sysfs::list_dir("/sys/class/backlight") {
        let Some(name) = device.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        let current = sysfs::read_u64(device.join("brightness"));
        let max = sysfs::read_u64(device.join("max_brightness"));

        let detail = match (current, max) {
            // Raw values are driver-specific and meaningless alone; the
            // proportion is what a reader can use.
            (Some(current), Some(max)) if max > 0 => format!(
                "{:.0}% ({current} of {max})",
                current as f64 / max as f64 * 100.0
            ),
            (Some(current), _) => format!("{current} (scale unknown)"),
            _ => continue,
        };
        node.push(Node::field(name, detail));
    }

    (!node.children.is_empty()).then_some(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_server_is_always_stated() {
        let node = session();
        let server = node
            .children
            .iter()
            .find(|c| c.label == "Display server")
            .expect("the session type must be reported");
        assert!(!server.value.as_ref().unwrap().to_string().is_empty());
    }

    #[test]
    fn outputs_section_is_never_silently_empty() {
        let node = outputs();
        assert!(
            !node.children.is_empty(),
            "either outputs or an explanation"
        );
    }

    #[test]
    fn backlight_is_a_proportion_not_a_raw_value() {
        // 4437 of 4437 means nothing; 100% does.
        if let Some(node) = backlight() {
            for field in &node.children {
                let value = field.value.as_ref().unwrap().to_string();
                assert!(
                    value.contains('%') || value.contains("scale unknown"),
                    "{value}"
                );
            }
        }
    }

    #[test]
    fn collects_on_this_machine() {
        let nodes = Display.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Session");
        assert_eq!(nodes[1].label, "Active outputs");
    }
}
