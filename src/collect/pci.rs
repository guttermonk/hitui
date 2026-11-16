//! PCI devices.
//!
//! The device list, addresses, numeric IDs, class codes, bound drivers and
//! IRQs all come from /sys/bus/pci. What sysfs cannot give is the *names*:
//! turning `8086:591d` into "Intel Corporation Kaby Lake-DT GT2" needs the
//! hwdata `pci.ids` table, which plenty of systems do not ship as a readable
//! file. Where `lspci` is installed it resolves names against its own copy, so
//! hitui asks it and says so; without it the page still lists every device,
//! just by number.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::probe;
use crate::sysfs;

const PCI_DEVICES: &str = "/sys/bus/pci/devices";

pub struct Pci;

impl Collector for Pci {
    fn id(&self) -> &'static str {
        "pci"
    }

    fn title(&self) -> &'static str {
        "PCI Devices"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let names = lspci_names();
        let addresses = addresses();

        let mut nodes = vec![overview(
            addresses.len(),
            !names.is_empty(),
            probe::available("lspci"),
        )];
        for address in addresses {
            nodes.push(device(&address, names.get(&address)));
        }
        Ok(nodes)
    }
}

/// Bus totals, and whether names could be resolved.
///
/// Split out so both the enriched and the sysfs-only wording are testable
/// without having to manipulate `PATH`, which is process-global and would race
/// the other tests.
fn overview(devices: usize, named: bool, lspci_present: bool) -> Node {
    let names = match (named, lspci_present) {
        (true, _) => "resolved by lspci",
        // Installed but silent usually means an lspci too old for `-vmm -D`.
        (false, true) => "unavailable — lspci produced no usable output",
        (false, false) => "unavailable — install pciutils for vendor and device names",
    };
    Node::group(
        "PCI bus",
        vec![
            Node::field("Devices", devices),
            Node::field("Device names", names),
        ],
    )
}

/// Every PCI address on the bus, in sysfs order.
fn addresses() -> Vec<String> {
    sysfs::list_dir(PCI_DEVICES)
        .into_iter()
        .filter_map(|path| path.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect()
}

fn device(address: &str, names: Option<&DeviceNames>) -> Node {
    let path = std::path::Path::new(PCI_DEVICES).join(address);
    let id = |file: &str| sysfs::read(path.join(file)).map(|v| strip_hex(&v));

    // The heading carries the class so the list can be skimmed, and names it
    // in words when lspci supplied them.
    let class_code = id("class");
    let class_name = names.map(|n| n.class.clone()).or_else(|| {
        class_code
            .as_deref()
            .and_then(base_class_name)
            .map(str::to_string)
    });
    let heading = match &class_name {
        Some(class) => format!("{address}  {class}"),
        None => address.to_string(),
    };

    let mut node = Node::group(heading, vec![]);
    if names.is_some() {
        node = node.from("lspci");
    }

    // Pair each name with its number rather than choosing: the number is what
    // you need for a modprobe option or a VFIO bind, the name is what you need
    // to know what you are looking at.
    node.push_opt("Vendor", labelled(names.map(|n| &n.vendor), id("vendor")));
    node.push_opt("Device", labelled(names.map(|n| &n.device), id("device")));

    let subsystem = match (id("subsystem_vendor"), id("subsystem_device")) {
        (Some(vendor), Some(device)) => Some(format!("{vendor}:{device}")),
        _ => None,
    };
    let subsystem_name = names.and_then(|n| match (&n.subsystem_vendor, &n.subsystem_device) {
        (Some(vendor), Some(device)) => Some(format!("{vendor} {device}")),
        (Some(vendor), None) => Some(vendor.clone()),
        _ => None,
    });
    node.push_opt(
        "Subsystem",
        labelled(subsystem_name.as_ref(), subsystem.clone()),
    );

    node.push_opt("Class code", class_code);
    node.push_opt("Revision", id("revision"));
    // An unbound device is worth noticing: it is why a card does nothing.
    node.push(Node::field(
        "Driver",
        driver(&path).unwrap_or_else(|| "none bound".to_string()),
    ));
    node.push_opt(
        "IRQ",
        sysfs::read_u64(path.join("irq")).filter(|irq| *irq > 0),
    );
    node.push_opt("Modalias", sysfs::read(path.join("modalias")));
    node
}

/// `Name [hex]`, or whichever of the two is available.
fn labelled(name: Option<&String>, hex: Option<String>) -> Option<String> {
    match (name, hex) {
        (Some(name), Some(hex)) => Some(format!("{name} [{hex}]")),
        (Some(name), None) => Some(name.clone()),
        (None, Some(hex)) => Some(hex),
        (None, None) => None,
    }
}

fn driver(path: &std::path::Path) -> Option<String> {
    let target = std::fs::read_link(path.join("driver")).ok()?;
    // The link points into /sys/bus/pci/drivers/<name>.
    Some(target.file_name()?.to_string_lossy().to_string())
}

/// sysfs writes these as `0x8086`; the conventional notation drops the prefix.
fn strip_hex(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("0x")
        .to_ascii_lowercase()
        .to_string()
}

/// Names for one device, as reported by lspci.
#[derive(Default)]
pub struct DeviceNames {
    pub class: String,
    pub vendor: String,
    pub device: String,
    pub subsystem_vendor: Option<String>,
    pub subsystem_device: Option<String>,
}

/// Ask lspci for human-readable names, keyed by full PCI address.
///
/// `-vmm` is lspci's machine-readable format: `key:\tvalue` lines in
/// blank-line-separated blocks, which is the same shape as /proc/cpuinfo.
/// `-D` forces the domain into every address so the keys match sysfs.
pub fn lspci_names() -> BTreeMap<String, DeviceNames> {
    let Some(output) = probe::run("lspci", &["-vmm", "-D"]) else {
        return BTreeMap::new();
    };

    let mut names = BTreeMap::new();
    for block in sysfs::parse_cpuinfo_blocks(&output) {
        let Some(slot) = sysfs::find(&block, "Slot") else {
            continue;
        };
        names.insert(
            normalise_slot(slot),
            DeviceNames {
                class: sysfs::find(&block, "Class").unwrap_or_default().to_string(),
                vendor: sysfs::find(&block, "Vendor")
                    .unwrap_or_default()
                    .to_string(),
                device: sysfs::find(&block, "Device")
                    .unwrap_or_default()
                    .to_string(),
                subsystem_vendor: sysfs::find(&block, "SVendor").map(str::to_string),
                subsystem_device: sysfs::find(&block, "SDevice").map(str::to_string),
            },
        );
    }
    names
}

/// Pad a domain-less `bus:dev.func` out to the full form sysfs uses.
///
/// `-D` should make this unnecessary, but older lspci builds ignore it when
/// combined with `-vmm`, and a silently empty name column is worse than a
/// cheap fix-up.
fn normalise_slot(slot: &str) -> String {
    if slot.matches(':').count() == 1 {
        format!("0000:{slot}")
    } else {
        slot.to_string()
    }
}

/// The base class byte of a `class` code, named per the PCI specification.
///
/// Only the top-level classes: this exists so the page is still skimmable
/// without lspci, not to replace the full `pci.ids` class table.
fn base_class_name(class_code: &str) -> Option<&'static str> {
    // Six hex digits: base class, subclass, programming interface.
    let base = u8::from_str_radix(class_code.get(..2)?, 16).ok()?;
    Some(match base {
        0x00 => "Unclassified device",
        0x01 => "Mass storage controller",
        0x02 => "Network controller",
        0x03 => "Display controller",
        0x04 => "Multimedia controller",
        0x05 => "Memory controller",
        0x06 => "Bridge",
        0x07 => "Communication controller",
        0x08 => "Generic system peripheral",
        0x09 => "Input device controller",
        0x0a => "Docking station",
        0x0b => "Processor",
        0x0c => "Serial bus controller",
        0x0d => "Wireless controller",
        0x0e => "Intelligent controller",
        0x0f => "Satellite communications controller",
        0x10 => "Encryption controller",
        0x11 => "Signal processing controller",
        0x12 => "Processing accelerator",
        0x13 => "Non-essential instrumentation",
        0x40 => "Coprocessor",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_sysfs_hex_prefix() {
        assert_eq!(strip_hex("0x8086"), "8086");
        assert_eq!(strip_hex("0x591D"), "591d");
        assert_eq!(strip_hex("  0x04\n"), "04");
    }

    #[test]
    fn names_base_classes_from_the_class_code() {
        assert_eq!(base_class_name("030000"), Some("Display controller"));
        assert_eq!(base_class_name("060000"), Some("Bridge"));
        assert_eq!(base_class_name("0c0330"), Some("Serial bus controller"));
        // Reserved ranges have no name worth inventing.
        assert_eq!(base_class_name("f00000"), None);
        assert_eq!(base_class_name(""), None);
    }

    #[test]
    fn pads_domainless_slots() {
        assert_eq!(normalise_slot("00:02.0"), "0000:00:02.0");
        assert_eq!(normalise_slot("0000:00:02.0"), "0000:00:02.0");
        // A non-zero domain must pass through untouched.
        assert_eq!(normalise_slot("0001:00:02.0"), "0001:00:02.0");
    }

    #[test]
    fn pairs_names_with_numbers() {
        let vendor = "Intel Corporation".to_string();
        assert_eq!(
            labelled(Some(&vendor), Some("8086".into())).unwrap(),
            "Intel Corporation [8086]"
        );
        // Either half alone still produces something useful.
        assert_eq!(labelled(None, Some("8086".into())).unwrap(), "8086");
        assert_eq!(labelled(Some(&vendor), None).unwrap(), "Intel Corporation");
        assert!(labelled(None, None).is_none());
    }

    #[test]
    fn collects_every_device_on_the_bus() {
        let nodes = Pci.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "PCI bus");

        // One overview section plus one section per device.
        let expected = addresses().len();
        assert_eq!(nodes.len(), expected + 1, "a section per device");

        if expected > 0 {
            let first = &nodes[1];
            assert!(
                first.children.iter().any(|c| c.label == "Driver"),
                "every device should report its driver binding"
            );
            assert!(
                first.children.iter().any(|c| c.label == "Class code"),
                "every device should report a class code"
            );
        }
    }

    #[test]
    fn every_device_is_identified_from_sysfs_alone() {
        // lspci is an enrichment, not a dependency: the numeric identity of
        // each device comes from sysfs and must always be present.
        let nodes = Pci.collect().expect("collection failed");
        for node in nodes.iter().skip(1) {
            assert!(
                node.children.iter().any(|c| c.label == "Vendor"),
                "{} should have a vendor from sysfs alone",
                node.label
            );
        }
    }

    #[test]
    fn overview_says_why_names_are_missing() {
        let message = |node: Node| {
            node.children
                .iter()
                .find(|c| c.label == "Device names")
                .and_then(|c| c.value.clone())
                .map(|v| v.to_string())
                .expect("a device-names field")
        };

        assert!(message(overview(24, true, true)).contains("lspci"));
        // Absent tool: tell the user what to install, not just that it failed.
        let missing = message(overview(24, false, false));
        assert!(missing.contains("pciutils"), "should name the package");
        // Present but useless: a different problem, so a different message.
        let silent = message(overview(24, false, true));
        assert_ne!(missing, silent);
    }
}
