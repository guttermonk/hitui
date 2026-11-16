//! USB devices, as the bus topology rather than a flat list.
//!
//! Unlike PCI, USB devices describe themselves: sysfs carries the
//! `manufacturer` and `product` strings the device reported at enumeration, so
//! no external table is needed for the common case. `lsusb` is consulted only
//! for devices that supplied no strings of their own, where it can still name
//! them from `usb.ids`.
//!
//! Devices are nested by port path — `1-5.2` sits under the hub at `1-5` —
//! which is how the hardware is actually wired. `lsusb` needs `-t` for this
//! and then loses the names.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::probe;
use crate::sysfs;

const USB_DEVICES: &str = "/sys/bus/usb/devices";

pub struct Usb;

impl Collector for Usb {
    fn id(&self) -> &'static str {
        "usb"
    }

    fn title(&self) -> &'static str {
        "USB Devices"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let entries = entries();
        let fallback_names = lsusb_names();

        // Parent port path for each device, so the tree can be rebuilt.
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for device in &entries.devices {
            children
                .entry(parent_of(device))
                .or_default()
                .push(device.clone());
        }

        let mut nodes = vec![Node::group(
            "USB",
            vec![
                Node::field("Buses", entries.roots.len()),
                Node::field("Devices", entries.devices.len()),
            ],
        )];

        for root in &entries.roots {
            let mut bus = describe(root, &fallback_names);
            // A root hub is the controller, not a peripheral; its own identity
            // matters less than what hangs off it.
            bus.label = match sysfs::read(path(root).join("product")) {
                Some(product) => format!("{root}  {product}"),
                None => root.clone(),
            };
            attach(&mut bus, root, &children, &fallback_names);
            nodes.push(bus);
        }

        // A device whose hub is absent from sysfs is never reached by the walk
        // above, so list it at the top level rather than losing it.
        for device in &entries.devices {
            let parent = parent_of(device);
            if !entries.roots.contains(&parent) && !entries.devices.contains(&parent) {
                nodes.push(describe(device, &fallback_names));
            }
        }
        Ok(nodes)
    }
}

/// Recursively add a hub's downstream devices.
fn attach(
    node: &mut Node,
    port: &str,
    children: &BTreeMap<String, Vec<String>>,
    names: &BTreeMap<(u32, u32), String>,
) {
    let Some(downstream) = children.get(port) else {
        return;
    };
    for device in downstream {
        let mut child = describe(device, names);
        attach(&mut child, device, children, names);
        node.push(child);
    }
}

struct Entries {
    /// Root hubs, named `usbN`.
    roots: Vec<String>,
    /// Peripherals, named by port path (`1-5`, `1-5.2`).
    devices: Vec<String>,
}

fn entries() -> Entries {
    let mut roots = Vec::new();
    let mut devices = Vec::new();

    for entry in sysfs::list_dir(USB_DEVICES) {
        let Some(name) = entry.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        // `1-5:1.0` is an interface of `1-5`, not a device in its own right.
        if name.contains(':') {
            continue;
        }
        if name.starts_with("usb") {
            roots.push(name);
        } else {
            devices.push(name);
        }
    }
    // Shallower paths first, so a hub is always described before its children.
    devices.sort_by_key(|d| (d.matches('.').count(), d.clone()));
    Entries { roots, devices }
}

/// The port path of the hub a device is plugged into.
///
/// `1-5.2` hangs off `1-5`; `1-5` hangs off root hub `usb1`.
fn parent_of(device: &str) -> String {
    match device.rsplit_once('.') {
        Some((parent, _)) => parent.to_string(),
        None => {
            let bus = device.split('-').next().unwrap_or("1");
            format!("usb{bus}")
        }
    }
}

fn path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(USB_DEVICES).join(name)
}

fn describe(name: &str, fallback_names: &BTreeMap<(u32, u32), String>) -> Node {
    let dir = path(name);
    let read = |file: &str| sysfs::read(dir.join(file)).map(|v| v.trim().to_string());
    let nonempty = |file: &str| read(file).filter(|v| !v.is_empty());

    let vendor_id = nonempty("idVendor");
    let product_id = nonempty("idProduct");
    let ids = match (&vendor_id, &product_id) {
        (Some(vendor), Some(product)) => Some(format!("{vendor}:{product}")),
        _ => None,
    };

    // The device's own strings come first; lsusb's usb.ids lookup is the
    // fallback for devices that reported none.
    let product = nonempty("product");
    let manufacturer = nonempty("manufacturer");
    let fallback = match (nonempty("busnum"), nonempty("devnum")) {
        (Some(bus), Some(dev)) => bus
            .parse()
            .ok()
            .zip(dev.parse().ok())
            .and_then(|key| fallback_names.get(&key))
            .cloned(),
        _ => None,
    };

    let title = product
        .clone()
        .or_else(|| fallback.clone())
        .or_else(|| ids.clone())
        .unwrap_or_else(|| "unknown device".to_string());

    let mut node = Node::group(format!("{name}  {title}"), vec![]);
    if product.is_none() && fallback.is_some() {
        node = node.from("lsusb");
    }

    node.push_opt("Product", product.or(fallback));
    node.push_opt("Manufacturer", manufacturer);
    node.push_opt("ID", ids);
    node.push_opt("Serial", nonempty("serial"));
    node.push_opt("Speed", nonempty("speed").map(|s| speed_name(&s)));
    node.push_opt("USB version", nonempty("version"));

    // Class 0 means "look at the interfaces", which is the common case for
    // anything that does more than one thing.
    if let Some(class) = nonempty("bDeviceClass") {
        node.push(Node::field(
            "Class",
            match class_name(&class) {
                Some(name) => format!("{name} [{class}]"),
                None => format!("defined per interface [{class}]"),
            },
        ));
    }
    if let Some(interfaces) = interfaces(name) {
        node.push(interfaces);
    }
    node
}

/// The interfaces a device exposes, with the driver bound to each.
///
/// This is what actually says what a device does when its own class is 0.
fn interfaces(device: &str) -> Option<Node> {
    let mut node = Node::group("Interfaces", vec![]);

    for entry in sysfs::list_dir(USB_DEVICES) {
        let Some(name) = entry.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        // Interfaces of `1-5` are `1-5:1.0`, `1-5:1.1`, and so on.
        if !name.starts_with(&format!("{device}:")) {
            continue;
        }

        let class = sysfs::read(entry.join("bInterfaceClass")).map(|c| c.trim().to_string());
        let described = class
            .as_deref()
            .and_then(class_name)
            .map(str::to_string)
            .or(class)
            .unwrap_or_else(|| "unknown".to_string());
        let driver = std::fs::read_link(entry.join("driver"))
            .ok()
            .and_then(|target| target.file_name().map(|n| n.to_string_lossy().to_string()));

        node.push(Node::field(
            name,
            match driver {
                Some(driver) => format!("{described} · {driver}"),
                None => format!("{described} · no driver bound"),
            },
        ));
    }

    (!node.children.is_empty()).then_some(node)
}

/// Name the USB signalling rate behind a raw Mb/s figure.
fn speed_name(raw: &str) -> String {
    let Ok(mbps) = raw.parse::<f64>() else {
        return raw.to_string();
    };
    // The marketing names are what documentation and lsusb both use.
    let name = match mbps {
        m if m <= 1.5 => "Low Speed",
        m if m <= 12.0 => "Full Speed",
        m if m <= 480.0 => "High Speed",
        m if m <= 5_000.0 => "SuperSpeed",
        _ => "SuperSpeed+",
    };
    if mbps >= 1000.0 {
        format!("{name} ({} Gb/s)", concise(mbps / 1000.0))
    } else {
        format!("{name} ({} Mb/s)", concise(mbps))
    }
}

/// Drop a pointless `.0` — rates read as "12 Mb/s" and "1.5 Mb/s".
fn concise(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// USB base class codes, per the device class definitions.
fn class_name(code: &str) -> Option<&'static str> {
    Some(match u8::from_str_radix(code.trim(), 16).ok()? {
        0x01 => "Audio",
        0x02 => "Communications",
        0x03 => "Human interface device",
        0x05 => "Physical",
        0x06 => "Still imaging",
        0x07 => "Printer",
        0x08 => "Mass storage",
        0x09 => "Hub",
        0x0a => "CDC data",
        0x0b => "Smart card",
        0x0d => "Content security",
        0x0e => "Video",
        0x0f => "Personal healthcare",
        0x10 => "Audio/video",
        0x11 => "Billboard",
        0x12 => "USB Type-C bridge",
        0xdc => "Diagnostic",
        0xe0 => "Wireless controller",
        0xef => "Miscellaneous",
        0xfe => "Application specific",
        0xff => "Vendor specific",
        // 0x00 is the "see the interfaces" sentinel, not a real class.
        _ => return None,
    })
}

/// Device descriptions from `lsusb`, keyed by `(bus, device)` number.
///
/// Only used for devices that reported no strings themselves.
fn lsusb_names() -> BTreeMap<(u32, u32), String> {
    let Some(output) = probe::run("lsusb", &[]) else {
        return BTreeMap::new();
    };

    let mut names = BTreeMap::new();
    for line in output.lines() {
        // "Bus 001 Device 005: ID 058f:9540 Generic EMV Smartcard Reader"
        let mut words = line.split_whitespace();
        if words.next() != Some("Bus") {
            continue;
        }
        let Some(bus) = words.next().and_then(|b| b.parse().ok()) else {
            continue;
        };
        if words.next() != Some("Device") {
            continue;
        }
        let Some(device) = words
            .next()
            .and_then(|d| d.trim_end_matches(':').parse().ok())
        else {
            continue;
        };
        // Skip "ID" and the vendor:product pair to reach the description.
        let description = words.skip(2).collect::<Vec<_>>().join(" ");
        if !description.is_empty() {
            names.insert((bus, device), description);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_paths_resolve_to_their_hub() {
        assert_eq!(parent_of("1-5"), "usb1");
        assert_eq!(parent_of("1-5.2"), "1-5");
        assert_eq!(parent_of("1-5.2.1"), "1-5.2");
        assert_eq!(parent_of("2-3"), "usb2");
    }

    #[test]
    fn names_signalling_rates() {
        assert_eq!(speed_name("1.5"), "Low Speed (1.5 Mb/s)");
        assert_eq!(speed_name("12"), "Full Speed (12 Mb/s)");
        assert_eq!(speed_name("480"), "High Speed (480 Mb/s)");
        assert_eq!(speed_name("5000"), "SuperSpeed (5 Gb/s)");
        assert_eq!(speed_name("10000"), "SuperSpeed+ (10 Gb/s)");
        // Unparseable input passes through rather than being invented.
        assert_eq!(speed_name("weird"), "weird");
    }

    #[test]
    fn names_usb_classes() {
        assert_eq!(class_name("09"), Some("Hub"));
        assert_eq!(class_name("03"), Some("Human interface device"));
        assert_eq!(class_name("e0"), Some("Wireless controller"));
        // Class 0 defers to the interfaces and must not be given a name.
        assert_eq!(class_name("00"), None);
    }

    #[test]
    fn parses_lsusb_lines() {
        // Exercised through the real tool when present; the shape is fixed.
        for ((bus, device), description) in lsusb_names() {
            assert!(bus > 0, "bus numbers start at 1");
            assert!(device > 0, "device numbers start at 1");
            assert!(!description.is_empty());
        }
    }

    #[test]
    fn interfaces_are_not_listed_as_devices() {
        let entries = entries();
        for name in entries.devices.iter().chain(entries.roots.iter()) {
            assert!(!name.contains(':'), "{name} is an interface, not a device");
        }
    }

    #[test]
    fn collects_the_bus_as_a_tree() {
        let nodes = Usb.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "USB");

        // Every root hub becomes a top-level section; peripherals nest inside
        // rather than appearing alongside their hub.
        let entries = entries();
        assert!(
            nodes.len() > entries.roots.len(),
            "expected a section per bus plus the overview"
        );
        let nested: usize = nodes[1..].iter().map(count_devices).sum();
        assert_eq!(
            nested,
            entries.devices.len(),
            "every device should appear exactly once in the tree"
        );
    }

    /// Count nested device sections, ignoring the field rows and the
    /// "Interfaces" grouping.
    fn count_devices(node: &Node) -> usize {
        node.children
            .iter()
            .filter(|c| c.is_group() && c.label != "Interfaces")
            .map(|c| 1 + count_devices(c))
            .sum()
    }
}
