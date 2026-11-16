//! SMBIOS/DMI tables — what the firmware says the machine is.
//!
//! Read from /sys/class/dmi/id, which the kernel populates from the SMBIOS
//! entry point at boot. Everything there is world-readable *except* the
//! serial numbers and the system UUID, which are mode 0400 because they
//! identify the machine; those appear only when hitui runs as root.
//!
//! `dmidecode` is deliberately not used here. It reads the same tables and
//! needs the same privilege, so it would add a dependency without adding a
//! fact.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::sysfs;

const DMI: &str = "/sys/class/dmi/id";

pub struct Dmi;

impl Collector for Dmi {
    fn id(&self) -> &'static str {
        "dmi"
    }

    fn title(&self) -> &'static str {
        "System DMI"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        if !std::path::Path::new(DMI).is_dir() {
            // No SMBIOS at all: normal on ARM and most virtualised boards.
            return Ok(vec![Node::group(
                "System DMI",
                vec![Node::field(
                    "Status",
                    "no SMBIOS tables — this platform describes itself through the device tree",
                )],
            )]);
        }

        let mut nodes = Vec::new();

        let mut system = Node::group("System", vec![]);
        system.push_opt("Manufacturer", field("sys_vendor"));
        system.push_opt("Product name", field("product_name"));
        system.push_opt("Version", field("product_version"));
        system.push_opt("Family", field("product_family"));
        system.push_opt("SKU", field("product_sku"));
        system.push_opt("Serial number", field("product_serial"));
        system.push_opt("UUID", field("product_uuid"));
        nodes.push(system);

        let mut board = Node::group("Base board", vec![]);
        board.push_opt("Manufacturer", field("board_vendor"));
        board.push_opt("Product name", field("board_name"));
        board.push_opt("Version", field("board_version"));
        board.push_opt("Serial number", field("board_serial"));
        board.push_opt("Asset tag", field("board_asset_tag"));
        nodes.push(board);

        let mut chassis = Node::group("Chassis", vec![]);
        chassis.push_opt("Manufacturer", field("chassis_vendor"));
        chassis.push_opt(
            "Type",
            sysfs::read_u64(format!("{DMI}/chassis_type")).map(|code| match chassis_type(code) {
                Some(name) => format!("{name} [{code}]"),
                None => format!("unspecified [{code}]"),
            }),
        );
        chassis.push_opt("Version", field("chassis_version"));
        chassis.push_opt("Serial number", field("chassis_serial"));
        chassis.push_opt("Asset tag", field("chassis_asset_tag"));
        nodes.push(chassis);

        let mut bios = Node::group("Firmware", vec![]);
        bios.push_opt("Vendor", field("bios_vendor"));
        bios.push_opt("Version", field("bios_version"));
        bios.push_opt("Release", field("bios_release"));
        bios.push_opt("Date", field("bios_date"));
        // Embedded controller firmware, present on most laptops.
        bios.push_opt("EC firmware release", field("ec_firmware_release"));
        nodes.push(bios);

        // Say why the identifying fields are blank rather than leaving the
        // reader to wonder whether the firmware withheld them.
        if restricted_fields_hidden() {
            nodes.push(Node::group(
                "Restricted fields",
                vec![Node::field(
                    "Serial numbers and UUID",
                    "hidden — these are root-readable only (mode 0400)",
                )],
            ));
        }

        nodes.retain(|node| !node.children.is_empty());
        Ok(nodes)
    }
}

/// Read a DMI field, discarding the placeholders firmware ships unfilled.
pub fn field(name: &str) -> Option<String> {
    sysfs::read(format!("{DMI}/{name}"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && !is_placeholder(value))
}

/// Strings that mean "the vendor never filled this in".
///
/// Printing them verbatim is worse than omitting the field: "To be filled by
/// O.E.M." reads as data.
pub fn is_placeholder(value: &str) -> bool {
    const PLACEHOLDERS: [&str; 10] = [
        "To be filled by O.E.M.",
        "To Be Filled By O.E.M.",
        "System Product Name",
        "System Version",
        "System manufacturer",
        "Default string",
        "Not Available",
        "Not Specified",
        "No Asset Information",
        "None",
    ];
    PLACEHOLDERS.iter().any(|p| p.eq_ignore_ascii_case(value))
}

/// Whether the root-only fields exist but could not be read.
fn restricted_fields_hidden() -> bool {
    ["product_serial", "board_serial", "product_uuid"]
        .iter()
        .any(|name| {
            let path = std::path::Path::new(DMI).join(name);
            // Present on disk but unreadable: that is the permission case, as
            // opposed to firmware that simply does not provide the field.
            path.exists() && sysfs::read(&path).is_none()
        })
}

/// SMBIOS chassis types, specification table 7.4.1.
pub fn chassis_type(code: u64) -> Option<&'static str> {
    Some(match code {
        3 => "Desktop",
        4 => "Low-profile desktop",
        5 => "Pizza box",
        6 => "Mini tower",
        7 => "Tower",
        8 => "Portable",
        9 => "Laptop",
        10 => "Notebook",
        11 => "Handheld",
        12 => "Docking station",
        13 => "All-in-one",
        14 => "Sub-notebook",
        15 => "Space-saving",
        16 => "Lunch box",
        17 => "Main server chassis",
        18 => "Expansion chassis",
        21 => "Peripheral chassis",
        22 => "RAID chassis",
        23 => "Rack-mount chassis",
        24 => "Sealed-case PC",
        25 => "Multi-system chassis",
        28 => "Blade",
        29 => "Blade enclosure",
        30 => "Tablet",
        31 => "Convertible",
        32 => "Detachable",
        33 => "IoT gateway",
        34 => "Embedded PC",
        35 => "Mini PC",
        36 => "Stick PC",
        // 1 "Other" and 2 "Unknown" carry no information worth printing.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unfilled_firmware_strings() {
        assert!(is_placeholder("To be filled by O.E.M."));
        assert!(is_placeholder("Default string"));
        assert!(is_placeholder("Not Available"));
        assert!(is_placeholder("No Asset Information"));
        // Case-insensitive, since vendors are inconsistent about it.
        assert!(is_placeholder("NOT SPECIFIED"));
        assert!(!is_placeholder("ThinkPad P51"));
        assert!(!is_placeholder("LENOVO"));
    }

    #[test]
    fn placeholder_fields_are_dropped_not_printed() {
        // board_asset_tag reads "Not Available" on this machine.
        if let Some(value) = field("board_asset_tag") {
            assert!(!is_placeholder(&value), "placeholder leaked: {value}");
        }
    }

    #[test]
    fn names_chassis_types() {
        assert_eq!(chassis_type(10), Some("Notebook"));
        assert_eq!(chassis_type(3), Some("Desktop"));
        assert_eq!(chassis_type(23), Some("Rack-mount chassis"));
        assert_eq!(chassis_type(1), None, "\"Other\" is not information");
        assert_eq!(chassis_type(2), None, "\"Unknown\" is not information");
    }

    #[test]
    fn collects_without_panicking() {
        let nodes = Dmi.collect().expect("collection failed");
        assert!(!nodes.is_empty(), "expected sections or a placeholder");
        // Empty sections are removed rather than rendered as bare headings.
        for node in &nodes {
            assert!(!node.children.is_empty(), "{} is empty", node.label);
        }
    }
}
