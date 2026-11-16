//! Hardware monitoring sensors, grouped by chip.
//!
//! Read straight from /sys/class/hwmon rather than through libsensors. The
//! kernel already exposes every reading and its label there; libsensors adds
//! `sensors.conf` handling — renaming and scaling per board — which is worth
//! linking for later, but not for showing what the chips report.

use std::path::Path;

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::sysfs;

pub struct Sensors;

impl Collector for Sensors {
    fn id(&self) -> &'static str {
        "sensors"
    }

    fn title(&self) -> &'static str {
        "Sensors"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes = Vec::new();

        for chip in sysfs::list_dir("/sys/class/hwmon") {
            let name = sysfs::read(chip.join("name")).unwrap_or_else(|| "sensor".to_string());
            let node = chip
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            // Chips of the same kind share a name, so qualify every heading
            // with its sysfs node rather than guessing which are ambiguous.
            let mut section = Node::group(format!("{name} ({node})"), vec![]);
            for kind in SensorKind::ALL {
                section.children.extend(readings(&chip, kind));
            }
            if !section.children.is_empty() {
                nodes.push(section);
            }
        }

        if nodes.is_empty() {
            // An empty page is indistinguishable from a broken one, so say why.
            nodes.push(Node::group(
                "Sensors",
                vec![Node::field(
                    "Status",
                    "no hwmon devices — the sensor drivers for this board may not be loaded",
                )],
            ));
        }
        Ok(nodes)
    }
}

/// The hwmon classes worth surfacing, with their sysfs prefix and scaling.
#[derive(Clone, Copy)]
enum SensorKind {
    Temperature,
    Fan,
    Voltage,
    Current,
    Power,
    Energy,
}

impl SensorKind {
    const ALL: [SensorKind; 6] = [
        SensorKind::Temperature,
        SensorKind::Fan,
        SensorKind::Voltage,
        SensorKind::Current,
        SensorKind::Power,
        SensorKind::Energy,
    ];

    /// The `tempN_input` style prefix this class uses.
    fn prefix(self) -> &'static str {
        match self {
            SensorKind::Temperature => "temp",
            SensorKind::Fan => "fan",
            SensorKind::Voltage => "in",
            SensorKind::Current => "curr",
            SensorKind::Power => "power",
            SensorKind::Energy => "energy",
        }
    }

    /// Fallback name when the chip publishes no `_label`.
    fn noun(self) -> &'static str {
        match self {
            SensorKind::Temperature => "Temperature",
            SensorKind::Fan => "Fan",
            SensorKind::Voltage => "Voltage",
            SensorKind::Current => "Current",
            SensorKind::Power => "Power",
            SensorKind::Energy => "Energy",
        }
    }

    /// Convert a raw hwmon reading into a display string.
    ///
    /// hwmon reports millidegrees, millivolts, milliamps, and microwatts; fan
    /// speeds are already in RPM.
    fn format(self, raw: u64) -> String {
        match self {
            SensorKind::Temperature => format!("{:.1} °C", raw as f64 / 1000.0),
            SensorKind::Fan => format!("{raw} RPM"),
            SensorKind::Voltage => format!("{:.3} V", raw as f64 / 1000.0),
            SensorKind::Current => format!("{:.3} A", raw as f64 / 1000.0),
            SensorKind::Power => format!("{:.2} W", raw as f64 / 1_000_000.0),
            SensorKind::Energy => format!("{:.2} J", raw as f64 / 1_000_000.0),
        }
    }
}

/// Every reading of one class on one chip, in sysfs order.
fn readings(chip: &Path, kind: SensorKind) -> Vec<Node> {
    let prefix = kind.prefix();
    let mut inputs: Vec<_> = sysfs::list_dir(chip)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .map(|n| n.to_string_lossy())
                .is_some_and(|n| n.starts_with(prefix) && n.ends_with("_input"))
        })
        .collect();
    inputs.sort();

    inputs
        .into_iter()
        .filter_map(|input| {
            let raw = sysfs::read_u64(&input)?;
            let stem = input.to_string_lossy().to_string();
            let label = sysfs::read(stem.replace("_input", "_label")).unwrap_or_else(|| {
                // "temp2_input" with no label becomes "Temperature 2".
                let index = input
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .and_then(|n| {
                        n.trim_start_matches(prefix)
                            .trim_end_matches("_input")
                            .parse::<u32>()
                            .ok()
                    })
                    .unwrap_or(0);
                format!("{} {index}", kind.noun())
            });

            let mut node = Node::field(label, kind.format(raw));
            // Thresholds give the reading context: 60 °C means little without
            // knowing the chip trips at 100.
            for (suffix, name) in [
                ("_max", "High"),
                ("_crit", "Critical"),
                ("_min", "Low"),
                ("_alarm", "Alarm"),
            ] {
                if let Some(limit) = sysfs::read_u64(stem.replace("_input", suffix)) {
                    if suffix == "_alarm" {
                        // An alarm flag is a boolean, not a reading.
                        node.push(Node::field(name, limit != 0));
                    } else {
                        node.push(Node::field(name, kind.format(limit)));
                    }
                }
            }
            Some(node)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_raw_hwmon_units() {
        assert_eq!(SensorKind::Temperature.format(57_850), "57.9 °C");
        assert_eq!(SensorKind::Fan.format(2_400), "2400 RPM");
        assert_eq!(SensorKind::Voltage.format(1_216), "1.216 V");
        assert_eq!(SensorKind::Power.format(15_500_000), "15.50 W");
    }

    #[test]
    fn collects_without_panicking() {
        // Machines in CI often have no hwmon at all, so this must produce the
        // explanatory placeholder rather than an empty page or an error.
        let nodes = Sensors.collect().expect("collection failed");
        assert!(!nodes.is_empty());
    }

    #[test]
    fn temperature_readings_are_plausible() {
        for chip in sysfs::list_dir("/sys/class/hwmon") {
            for node in readings(&chip, SensorKind::Temperature) {
                let text = node.value.expect("a reading").to_string();
                let celsius: f64 = text
                    .trim_end_matches(" °C")
                    .parse()
                    .unwrap_or_else(|_| panic!("unparseable reading: {text}"));
                assert!(
                    (-50.0..200.0).contains(&celsius),
                    "{} reported {celsius} °C",
                    node.label
                );
            }
        }
    }
}
