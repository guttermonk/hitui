//! Batteries and power supplies, from /sys/class/power_supply.

use std::path::Path;

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::{Node, Value};
use crate::sysfs;

pub struct Battery;

impl Collector for Battery {
    fn id(&self) -> &'static str {
        "battery"
    }

    fn title(&self) -> &'static str {
        "Battery"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes = Vec::new();

        for supply in sysfs::list_dir("/sys/class/power_supply") {
            let name = supply
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "power supply".to_string());
            let kind = sysfs::read(supply.join("type")).unwrap_or_default();

            let mut node = Node::group(format!("{name} ({kind})"), vec![]);
            match kind.as_str() {
                "Battery" => battery_fields(&supply, &mut node),
                // Mains and USB-PD supplies only report whether they are live.
                _ => node.push_opt(
                    "Online",
                    sysfs::read_u64(supply.join("online")).map(|v| Value::Flag(v != 0)),
                ),
            }
            if !node.children.is_empty() {
                nodes.push(node);
            }
        }

        if nodes.is_empty() {
            nodes.push(Node::group(
                "Power supplies",
                vec![Node::field("Status", "none reported — likely a desktop")],
            ));
        }
        Ok(nodes)
    }
}

fn battery_fields(supply: &Path, node: &mut Node) {
    node.push_opt("Status", sysfs::read(supply.join("status")));

    if let Some(percent) = charge_percent(supply) {
        node.push(Node::field(
            "Charge",
            Value::gauge(percent, format!("{percent:.0} %")),
        ));
    }

    // Design capacity versus what the cells now hold full: the usual measure
    // of how worn a battery is.
    let Capacity {
        full,
        design,
        unit,
        scale,
    } = capacity(supply);
    if let (Some(full), Some(design)) = (full, design)
        && design > 0
    {
        let health = full as f64 / design as f64 * 100.0;
        node.push(Node::field(
            "Health",
            Value::gauge(health, format!("{health:.0} % of design capacity")),
        ));
    }
    let quantity = |raw: u64| format!("{:.1} {unit}", raw as f64 / scale);
    node.push_opt("Capacity when full", full.map(quantity));
    node.push_opt("Design capacity", design.map(quantity));

    node.push_opt("Technology", sysfs::read(supply.join("technology")));
    node.push_opt("Manufacturer", sysfs::read(supply.join("manufacturer")));
    node.push_opt("Model", sysfs::read(supply.join("model_name")));
    node.push_opt("Serial", sysfs::read(supply.join("serial_number")));
    node.push_opt(
        "Cycle count",
        sysfs::read_u64(supply.join("cycle_count")).filter(|c| *c > 0),
    );

    // Microvolts and microwatts.
    if let Some(microvolts) = sysfs::read_u64(supply.join("voltage_now")) {
        node.push(Node::field(
            "Voltage",
            format!("{:.2} V", microvolts as f64 / 1_000_000.0),
        ));
    }
    if let Some(microwatts) = sysfs::read_u64(supply.join("power_now")).filter(|p| *p > 0) {
        node.push(Node::field(
            "Draw",
            format!("{:.2} W", microwatts as f64 / 1_000_000.0),
        ));
    }
}

/// The firmware's own percentage, or the charge/energy pair if it has none.
fn charge_percent(supply: &Path) -> Option<f64> {
    if let Some(capacity) = sysfs::read_u64(supply.join("capacity")) {
        return Some(capacity as f64);
    }
    let full = capacity(supply).full?;
    let now = sysfs::read_u64(supply.join("energy_now"))
        .or_else(|| sysfs::read_u64(supply.join("charge_now")))?;
    (full > 0).then(|| now as f64 / full as f64 * 100.0)
}

struct Capacity {
    full: Option<u64>,
    design: Option<u64>,
    /// Display unit for the raw readings.
    unit: &'static str,
    /// Divisor taking the raw reading to `unit`.
    scale: f64,
}

/// Battery capacity as the driver reports it.
///
/// Drivers expose either energy (µWh) or charge (µAh) depending on what the
/// hardware actually measures, so the unit travels with the numbers rather
/// than being assumed.
fn capacity(supply: &Path) -> Capacity {
    if let Some(full) = sysfs::read_u64(supply.join("energy_full")) {
        return Capacity {
            full: Some(full),
            design: sysfs::read_u64(supply.join("energy_full_design")),
            unit: "Wh",
            scale: 1_000_000.0,
        };
    }
    Capacity {
        full: sysfs::read_u64(supply.join("charge_full")),
        design: sysfs::read_u64(supply.join("charge_full_design")),
        unit: "Ah",
        scale: 1_000_000.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_without_panicking() {
        let nodes = Battery.collect().expect("collection failed");
        assert!(!nodes.is_empty(), "expected a section or a placeholder");
    }

    #[test]
    fn charge_is_a_percentage() {
        for supply in sysfs::list_dir("/sys/class/power_supply") {
            if sysfs::read(supply.join("type")).as_deref() != Some("Battery") {
                continue;
            }
            if let Some(percent) = charge_percent(&supply) {
                assert!(
                    (0.0..=100.0).contains(&percent),
                    "{supply:?} reported {percent} %"
                );
            }
        }
    }
}
