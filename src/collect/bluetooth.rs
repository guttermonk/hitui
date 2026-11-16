//! Bluetooth adapters and the devices paired with them.
//!
//! The kernel exposes almost nothing here. `/sys/class/bluetooth/hciN` holds
//! `reset` and `uevent` and that is all — the adapter's own address was
//! removed from sysfs years ago — so everything worth reporting lives in
//! BlueZ, behind D-Bus. It is read through `busctl --json`, which returns the
//! whole object tree in one non-interactive call; `bluetoothctl` is a REPL
//! that happens to accept arguments, and asking a REPL for machine-readable
//! output is how pages come to hang.
//!
//! What sysfs *does* own is the part BlueZ cannot see: the rfkill state and
//! the transport the adapter is attached to. A radio that is blocked is the
//! single most common reason Bluetooth "stops working", and the block lives in
//! the kernel whether or not bluetoothd is running — so those are read
//! directly, and reported even when BlueZ is unreachable.
//!
//! Several of BlueZ's fields are numbers that only mean something decoded, and
//! are decoded: the HCI version is the Bluetooth specification the adapter
//! implements, the class of device is a packed major/minor/service triple, and
//! the Modalias carries bluetoothd's own version.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value as Json;

use crate::collect::{Collector, Group};
use crate::model::{Node, Value};
use crate::probe;
use crate::sysfs;

const SYS: &str = "/sys/class/bluetooth";

pub struct Bluetooth;

impl Collector for Bluetooth {
    fn id(&self) -> &'static str {
        "bluetooth"
    }

    fn title(&self) -> &'static str {
        "Bluetooth"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn refresh_interval(&self) -> Option<Duration> {
        // Whether a headset is connected, and what its battery is down to, are
        // the kind of fact whose current value is the reason you opened the
        // page. The busctl call is a single D-Bus round trip.
        Some(Duration::from_secs(5))
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let adapters = sysfs::list_dir(SYS);
        if adapters.is_empty() {
            let mut node = Node::group("Bluetooth", vec![]);
            node.push(Node::field(
                "Present",
                "no — no Bluetooth adapter exposed by the kernel",
            ));
            return Ok(vec![node]);
        }

        let objects = managed_objects();
        let mut nodes = Vec::new();
        for path in &adapters {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            nodes.push(adapter(&name, path, objects.as_ref()));
        }

        // One section for the paired and connected devices, rather than one
        // per adapter: almost every machine has a single adapter, and nesting
        // the list under it buys an indent and nothing else.
        if let Some(objects) = objects.as_ref() {
            nodes.push(devices(objects));
        }
        Ok(nodes)
    }
}

/// One adapter: what the kernel knows, then what BlueZ adds.
fn adapter(name: &str, path: &Path, objects: Option<&Json>) -> Node {
    let properties = objects.and_then(|objects| {
        interface_of(objects, &format!("/org/bluez/{name}"), "org.bluez.Adapter1")
    });

    let mut node = Node::group(name.to_string(), vec![]);
    if properties.is_some() {
        node = node.from("busctl");
    }

    if let Some(properties) = properties {
        node.push_opt("Address", string(properties, "Address"));
        node.push_opt("Address type", string(properties, "AddressType"));
        node.push_opt("Name", string(properties, "Name"));
        // Only when it has actually been renamed: BlueZ defaults the alias to
        // the name, and a row repeating the row above it is noise.
        let alias = string(properties, "Alias");
        if alias != string(properties, "Name") {
            node.push_opt("Alias", alias);
        }
        node.push_opt(
            "Bluetooth",
            integer(properties, "Version").and_then(|v| hci_version(v as u8)),
        );
        node.push_opt(
            "Chipset vendor",
            integer(properties, "Manufacturer").map(|id| match company(id as u16) {
                Some(name) => name.to_string(),
                // Naming the wrong vendor is worse than naming none: the SIG
                // list runs to thousands of entries and only a handful are
                // ever a Bluetooth controller.
                None => format!("SIG company {id}"),
            }),
        );
        node.push_opt(
            "BlueZ",
            string(properties, "Modalias")
                .as_deref()
                .and_then(bluez_version),
        );

        if let Some(class) = integer(properties, "Class") {
            let class = class as u32;
            node.push_opt("Class", device_class(class));
            let services = service_classes(class);
            if !services.is_empty() {
                node.push(Node::field(
                    "Services",
                    Value::List(services.iter().map(|s| s.to_string()).collect()),
                ));
            }
        }

        node.push_opt("Roles", strings(properties, "Roles").map(Value::List));
        node.push_opt("Powered", boolean(properties, "Powered").map(Value::Flag));
        node.push_opt(
            "Discoverable",
            boolean(properties, "Discoverable").map(Value::Flag),
        );
        node.push_opt("Pairable", boolean(properties, "Pairable").map(Value::Flag));
        node.push_opt(
            "Discovering",
            boolean(properties, "Discovering").map(Value::Flag),
        );
        if let Some(profiles) = profiles(properties) {
            node.push(Node::field("Profiles", profiles));
        }
    } else {
        // The adapter exists as far as the kernel is concerned, so say what is
        // missing rather than leaving the section bare.
        node.push(Node::field(
            "BlueZ",
            if probe::available("busctl") {
                "not reachable — is bluetooth.service running?"
            } else {
                "not queried — busctl (systemd) is not installed"
            },
        ));
    }

    // From the kernel, and reported whether or not BlueZ answered: a blocked
    // radio is the usual reason none of the above works.
    for (label, value) in rfkill(path) {
        node.push(Node::field(label, value));
    }
    node.push_opt("Transport", transport(path));
    node
}

/// Paired and connected devices, newest-looking first is not a thing BlueZ
/// offers, so they come out in object-path order.
fn devices(objects: &Json) -> Node {
    let mut node = Node::group("Devices", vec![]).from("busctl");
    let Some(map) = objects.as_object() else {
        return node;
    };

    let mut found = 0;
    for (path, interfaces) in map {
        let Some(properties) = interfaces.get("org.bluez.Device1") else {
            continue;
        };
        found += 1;

        let label = string(properties, "Alias")
            .or_else(|| string(properties, "Name"))
            .or_else(|| string(properties, "Address"))
            .unwrap_or_else(|| path.clone());
        let mut device = Node::group(label, vec![]);

        device.push_opt("Address", string(properties, "Address"));
        device.push_opt("Address type", string(properties, "AddressType"));
        // BlueZ's icon name is the most legible statement of what a device is,
        // and unlike the class of device it is present on LE-only peripherals.
        device.push_opt("Kind", string(properties, "Icon"));
        if let Some(class) = integer(properties, "Class") {
            device.push_opt("Class", device_class(class as u32));
        }
        device.push_opt(
            "Connected",
            boolean(properties, "Connected").map(Value::Flag),
        );
        device.push_opt("Paired", boolean(properties, "Paired").map(Value::Flag));
        device.push_opt("Bonded", boolean(properties, "Bonded").map(Value::Flag));
        device.push_opt("Trusted", boolean(properties, "Trusted").map(Value::Flag));
        // Only when it is set: "Blocked: no" on every device is fourteen rows
        // of nothing, but a blocked device that will not connect is the answer.
        if boolean(properties, "Blocked") == Some(true) {
            device.push(Node::field("Blocked", Value::Flag(true)));
        }

        // Battery lives on its own interface, not on Device1.
        if let Some(battery) = interfaces
            .get("org.bluez.Battery1")
            .and_then(|battery| integer(battery, "Percentage"))
        {
            device.push(Node::field(
                "Battery",
                // A reserve: it reddens as it empties, not as it fills.
                Value::reserve_gauge(battery as f64, format!("{battery} %")),
            ));
        }
        // Only meaningful while the device is in range and advertising.
        device.push_opt(
            "Signal",
            integer(properties, "RSSI").map(|rssi| format!("{rssi} dBm")),
        );
        device.push_opt(
            "Transmit power",
            integer(properties, "TxPower").map(|power| format!("{power} dBm")),
        );
        device.push_opt(
            "Modalias",
            string(properties, "Modalias")
                .as_deref()
                .map(describe_modalias),
        );
        if let Some(profiles) = profiles(properties) {
            device.push(Node::field("Profiles", profiles));
        }
        node.push(device);
    }

    if found == 0 {
        node.push(Node::field(
            "Paired",
            "none — BlueZ has no device records for this adapter",
        ));
    }
    node
}

/// Soft and hard radio blocks, from the rfkill node the kernel hangs off the
/// adapter itself — no matching rfkill names to adapter names required.
fn rfkill(path: &Path) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    let Some(node) = sysfs::list_dir(path).into_iter().find(|entry| {
        entry
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("rfkill"))
    }) else {
        return out;
    };

    if let Some(soft) = sysfs::read(node.join("soft")) {
        out.push((
            "Soft blocked",
            if soft.trim() == "1" {
                "yes — disabled in software"
            } else {
                "no"
            },
        ));
    }
    if let Some(hard) = sysfs::read(node.join("hard")) {
        // A hardware block cannot be cleared by software, so saying which kind
        // it is saves someone an hour of `rfkill unblock`.
        out.push((
            "Hard blocked",
            if hard.trim() == "1" {
                "yes — disabled by a physical switch"
            } else {
                "no"
            },
        ));
    }
    out
}

/// What the adapter is plugged into, walked up from its sysfs path.
fn transport(path: &Path) -> Option<String> {
    let real = std::fs::canonicalize(path).ok()?;
    // hciN / bluetooth / <interface> / <device>: the USB device carrying the
    // controller is three levels up from the adapter.
    let device = real.parent()?.parent()?.parent()?;
    let vendor = sysfs::read(device.join("idVendor"))?;
    let product = sysfs::read(device.join("idProduct"))?;
    let port = device.file_name()?.to_string_lossy().to_string();
    Some(format!("USB {vendor}:{product} at {port}"))
}

/// `GetManagedObjects` on BlueZ: every adapter, device and their interfaces in
/// one call.
fn managed_objects() -> Option<Json> {
    let output = probe::run(
        "busctl",
        &[
            "--system",
            "--json=short",
            "call",
            "org.bluez",
            "/",
            "org.freedesktop.DBus.ObjectManager",
            "GetManagedObjects",
        ],
    )?;
    let json: Json = serde_json::from_str(&output).ok()?;
    // busctl wraps the reply in {"type": ..., "data": [ <the map> ]}.
    json.get("data")?.as_array()?.first().cloned()
}

fn interface_of<'a>(objects: &'a Json, path: &str, interface: &str) -> Option<&'a Json> {
    objects.get(path)?.get(interface)
}

/// busctl's JSON wraps every property as `{"type": ..., "data": ...}`.
fn property<'a>(properties: &'a Json, key: &str) -> Option<&'a Json> {
    properties.get(key)?.get("data")
}

fn string(properties: &Json, key: &str) -> Option<String> {
    property(properties, key)?
        .as_str()
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

fn integer(properties: &Json, key: &str) -> Option<i64> {
    property(properties, key)?.as_i64()
}

fn boolean(properties: &Json, key: &str) -> Option<bool> {
    property(properties, key)?.as_bool()
}

fn strings(properties: &Json, key: &str) -> Option<Vec<String>> {
    let values: Vec<String> = property(properties, key)?
        .as_array()?
        .iter()
        .filter_map(Json::as_str)
        .map(str::to_string)
        .collect();
    (!values.is_empty()).then_some(values)
}

/// The profiles an adapter or device advertises, as names rather than UUIDs.
///
/// Vendor UUIDs are counted rather than printed: they are 36 characters that
/// identify nothing to a reader, and a device with eight of them would push
/// everything else off the page.
fn profiles(properties: &Json) -> Option<Value> {
    let uuids = strings(properties, "UUIDs")?;
    let mut named: Vec<String> = Vec::new();
    let mut vendor = 0;
    for uuid in &uuids {
        match assigned_number(uuid).and_then(profile_name) {
            Some(name) => named.push(name.to_string()),
            None => vendor += 1,
        }
    }
    named.sort();
    named.dedup();
    if vendor > 0 {
        named.push(format!(
            "and {vendor} vendor-specific service{}",
            if vendor == 1 { "" } else { "s" }
        ));
    }
    (!named.is_empty()).then_some(Value::List(named))
}

/// The 16-bit assigned number inside a Bluetooth base UUID.
///
/// Everything the SIG assigns is `0000XXXX-0000-1000-8000-00805f9b34fb`; a
/// UUID outside that range belongs to a vendor and means nothing here.
fn assigned_number(uuid: &str) -> Option<u16> {
    let uuid = uuid.to_ascii_lowercase();
    let rest = uuid.strip_prefix("0000")?;
    let (short, tail) = rest.split_once('-')?;
    if tail != "0000-1000-8000-00805f9b34fb" {
        return None;
    }
    u16::from_str_radix(short, 16).ok()
}

fn profile_name(uuid: u16) -> Option<&'static str> {
    Some(match uuid {
        0x1101 => "Serial port",
        0x1105 => "Object push",
        0x1106 => "File transfer",
        0x1108 => "Headset",
        0x110a => "Audio source (A2DP)",
        0x110b => "Audio sink (A2DP)",
        0x110c => "Remote control target",
        0x110e => "Remote control (AVRCP)",
        0x110f => "Remote control controller",
        0x1112 => "Headset audio gateway",
        0x1115 => "Personal area network user",
        0x1116 => "Network access point",
        0x1117 => "Group ad-hoc network",
        0x111e => "Hands-free",
        0x111f => "Hands-free audio gateway",
        0x1124 => "Human interface device",
        0x112d => "SIM access",
        0x112f => "Phonebook access",
        0x1132 => "Message access",
        0x1200 => "PnP information",
        0x1800 => "Generic access",
        0x1801 => "Generic attribute",
        0x180a => "Device information",
        0x180f => "Battery",
        0x1812 => "HID over GATT",
        _ => return None,
    })
}

/// The Bluetooth specification an HCI version number corresponds to.
///
/// The number on its own says nothing; "Bluetooth 4.2" is the fact anyone
/// actually wants from this field.
fn hci_version(version: u8) -> Option<String> {
    let spec = match version {
        0 => "1.0b",
        1 => "1.1",
        2 => "1.2",
        3 => "2.0 + EDR",
        4 => "2.1 + EDR",
        5 => "3.0 + HS",
        6 => "4.0",
        7 => "4.1",
        8 => "4.2",
        9 => "5.0",
        10 => "5.1",
        11 => "5.2",
        12 => "5.3",
        13 => "5.4",
        14 => "6.0",
        _ => return None,
    };
    Some(format!("{spec} (HCI version {version})"))
}

/// Bluetooth SIG company identifiers, for the handful that make controllers.
///
/// Deliberately not the whole list, which runs to thousands of entries and
/// would be stale within a release.
fn company(id: u16) -> Option<&'static str> {
    Some(match id {
        0 => "Ericsson",
        1 => "Nokia",
        2 => "Intel",
        3 => "IBM",
        4 => "Toshiba",
        5 => "3Com",
        6 => "Microsoft",
        7 => "Lucent",
        8 => "Motorola",
        9 => "Infineon",
        10 => "Cambridge Silicon Radio",
        13 => "Texas Instruments",
        15 => "Broadcom",
        29 => "Qualcomm",
        72 => "MediaTek",
        93 => "Realtek",
        _ => return None,
    })
}

/// bluetoothd's version, which it reports as the device release of a virtual
/// USB modalias — `usb:v1D6Bp0246d0556` is BlueZ 5.86.
fn bluez_version(modalias: &str) -> Option<String> {
    let release = modalias.rsplit_once('d')?.1;
    let packed = u16::from_str_radix(release, 16).ok()?;
    Some(format!("{}.{}", packed >> 8, packed & 0xff))
}

/// A device's modalias, with its vendor named where the identifier resolves.
///
/// Left as-is otherwise: it is the string people paste into a search when a
/// peripheral misbehaves, so mangling it would cost more than it buys.
fn describe_modalias(modalias: &str) -> String {
    if let Some(rest) = modalias.strip_prefix("bluetooth:v")
        && let Some(vendor) = rest.get(..4).and_then(|v| u16::from_str_radix(v, 16).ok())
        && let Some(name) = company(vendor)
    {
        return format!("{modalias} ({name})");
    }
    modalias.to_string()
}

/// The major and minor halves of a class of device.
///
/// A packed 24-bit field: format in bits 0-1, minor class in 2-7, major in
/// 8-12, service classes above that.
fn device_class(class: u32) -> Option<String> {
    let major = (class >> 8) & 0x1f;
    let minor = (class >> 2) & 0x3f;

    let major_name = match major {
        0 => "Miscellaneous",
        1 => "Computer",
        2 => "Phone",
        3 => "Network access point",
        4 => "Audio/video",
        5 => "Peripheral",
        6 => "Imaging",
        7 => "Wearable",
        8 => "Toy",
        9 => "Health",
        31 => "Uncategorised",
        _ => return None,
    };

    // Minor classes are per-major, and only defined for some of them. An
    // undecoded minor is left off rather than guessed at.
    let minor_name = match (major, minor) {
        (1, 1) => Some("Desktop"),
        (1, 2) => Some("Server"),
        (1, 3) => Some("Laptop"),
        (1, 4) => Some("Handheld"),
        (1, 5) => Some("Palm-size"),
        (1, 6) => Some("Wearable computer"),
        (1, 7) => Some("Tablet"),
        (2, 1) => Some("Cellular"),
        (2, 2) => Some("Cordless"),
        (2, 3) => Some("Smartphone"),
        (2, 4) => Some("Wired modem"),
        (4, 1) => Some("Headset"),
        (4, 2) => Some("Hands-free"),
        (4, 4) => Some("Microphone"),
        (4, 5) => Some("Loudspeaker"),
        (4, 6) => Some("Headphones"),
        (4, 7) => Some("Portable audio"),
        (4, 8) => Some("Car audio"),
        (4, 9) => Some("Set-top box"),
        (4, 10) => Some("HiFi audio"),
        (4, 12) => Some("Video camera"),
        (4, 13) => Some("Camcorder"),
        (4, 18) => Some("Gaming"),
        (7, 1) => Some("Wristwatch"),
        (7, 2) => Some("Pager"),
        (7, 3) => Some("Jacket"),
        (7, 4) => Some("Helmet"),
        (7, 5) => Some("Glasses"),
        // The peripheral minor is itself split: the top two bits say keyboard
        // or pointer, the rest says what kind of thing it is.
        (5, _) => return Some(format!("{major_name} / {}", peripheral(minor))),
        _ => None,
    };

    Some(match minor_name {
        Some(minor_name) => format!("{major_name} / {minor_name}"),
        None => major_name.to_string(),
    })
}

fn peripheral(minor: u32) -> String {
    let mut parts = Vec::new();
    match (minor >> 4) & 0x3 {
        1 => parts.push("Keyboard"),
        2 => parts.push("Pointing device"),
        3 => parts.push("Keyboard and pointing device"),
        _ => {}
    }
    if let Some(kind) = match minor & 0xf {
        1 => Some("joystick"),
        2 => Some("gamepad"),
        3 => Some("remote control"),
        4 => Some("sensor"),
        5 => Some("digitiser tablet"),
        6 => Some("card reader"),
        7 => Some("digital pen"),
        8 => Some("handheld scanner"),
        9 => Some("gestural input"),
        _ => None,
    } {
        parts.push(kind);
    }
    if parts.is_empty() {
        return "Peripheral".to_string();
    }
    parts.join(", ")
}

/// The service class bits, which say what the device offers rather than what
/// it is.
fn service_classes(class: u32) -> Vec<&'static str> {
    const SERVICES: [(u32, &str); 9] = [
        (13, "Limited discoverable"),
        (16, "Positioning"),
        (17, "Networking"),
        (18, "Rendering"),
        (19, "Capturing"),
        (20, "Object transfer"),
        (21, "Audio"),
        (22, "Telephony"),
        (23, "Information"),
    ];
    SERVICES
        .iter()
        .filter(|(bit, _)| class & (1 << bit) != 0)
        .map(|(_, name)| *name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped exactly as `busctl --json=short` returns it, with invented
    /// addresses: this file is public and the real ones identify a machine.
    const OBJECTS: &str = r#"{
      "/org/bluez/hci0": {
        "org.bluez.Adapter1": {
          "Address": {"type": "s", "data": "00:11:22:33:44:55"},
          "AddressType": {"type": "s", "data": "public"},
          "Name": {"type": "s", "data": "host"},
          "Alias": {"type": "s", "data": "host"},
          "Class": {"type": "u", "data": 7078156},
          "Powered": {"type": "b", "data": true},
          "Discoverable": {"type": "b", "data": false},
          "Pairable": {"type": "b", "data": false},
          "Discovering": {"type": "b", "data": false},
          "Modalias": {"type": "s", "data": "usb:v1D6Bp0246d0556"},
          "Roles": {"type": "as", "data": ["central", "peripheral"]},
          "Manufacturer": {"type": "q", "data": 2},
          "Version": {"type": "y", "data": 8},
          "UUIDs": {"type": "as", "data": [
            "0000110b-0000-1000-8000-00805f9b34fb",
            "0000110a-0000-1000-8000-00805f9b34fb",
            "03b80e5a-ede8-4b33-a751-6ce34ec4c700"
          ]}
        }
      },
      "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF": {
        "org.bluez.Device1": {
          "Address": {"type": "s", "data": "AA:BB:CC:DD:EE:FF"},
          "AddressType": {"type": "s", "data": "public"},
          "Name": {"type": "s", "data": "WH-1000XM4"},
          "Alias": {"type": "s", "data": "Headphones"},
          "Class": {"type": "u", "data": 2360344},
          "Icon": {"type": "s", "data": "audio-headset"},
          "Connected": {"type": "b", "data": true},
          "Paired": {"type": "b", "data": true},
          "Bonded": {"type": "b", "data": true},
          "Trusted": {"type": "b", "data": true},
          "Blocked": {"type": "b", "data": false},
          "RSSI": {"type": "n", "data": -56},
          "Modalias": {"type": "s", "data": "bluetooth:v004Cp6006d0F70"},
          "UUIDs": {"type": "as", "data": ["0000110b-0000-1000-8000-00805f9b34fb"]}
        },
        "org.bluez.Battery1": {
          "Percentage": {"type": "y", "data": 15}
        }
      },
      "/org/bluez/hci0/dev_11_22_33_44_55_66": {
        "org.bluez.Device1": {
          "Address": {"type": "s", "data": "11:22:33:44:55:66"},
          "Name": {"type": "s", "data": "Keyboard K380"},
          "Class": {"type": "u", "data": 1344},
          "Connected": {"type": "b", "data": false},
          "Paired": {"type": "b", "data": true},
          "Blocked": {"type": "b", "data": true}
        }
      }
    }"#;

    fn objects() -> Json {
        serde_json::from_str(OBJECTS).expect("fixture is valid JSON")
    }

    fn field(node: &Node, label: &str) -> Option<String> {
        node.children
            .iter()
            .find(|child| child.label == label)
            .and_then(|child| child.value.as_ref())
            .map(|value| value.to_string())
    }

    fn child<'a>(node: &'a Node, label: &str) -> &'a Node {
        node.children
            .iter()
            .find(|child| child.label == label)
            .unwrap_or_else(|| panic!("{label} is not listed"))
    }

    #[test]
    fn an_hci_version_names_the_specification_it_implements() {
        // The number alone says nothing. "Bluetooth 4.2" is the fact wanted
        // from this field, and this machine's adapter reports version 8.
        assert_eq!(hci_version(8).as_deref(), Some("4.2 (HCI version 8)"));
        assert_eq!(hci_version(11).as_deref(), Some("5.2 (HCI version 11)"));
        // A version newer than the table is left undecoded rather than
        // extrapolated: the mapping is a list, not an arithmetic sequence.
        assert_eq!(hci_version(200), None);
    }

    #[test]
    fn a_class_of_device_unpacks_into_what_the_thing_is() {
        // 7078156 is this machine's own adapter, cross-checked against the
        // fact that it is a laptop.
        assert_eq!(device_class(7078156).as_deref(), Some("Computer / Laptop"));
        // 2360344 = major 4 minor 6: a pair of headphones.
        assert_eq!(
            device_class(2360344).as_deref(),
            Some("Audio/video / Headphones")
        );
        // 1344 = major 5 minor 64>>4 = keyboard.
        assert_eq!(device_class(1344).as_deref(), Some("Peripheral / Keyboard"));
        // An undefined minor leaves the major standing rather than inventing
        // a name for it.
        assert_eq!(device_class(0x000100).as_deref(), Some("Computer"));
    }

    #[test]
    fn service_class_bits_say_what_a_device_offers() {
        // The same adapter: it renders and captures audio, and does telephony
        // through hands-free.
        assert_eq!(
            service_classes(7078156),
            vec!["Rendering", "Capturing", "Audio", "Telephony"]
        );
        assert!(service_classes(0).is_empty());
    }

    #[test]
    fn only_sig_assigned_uuids_become_profile_names() {
        assert_eq!(
            assigned_number("0000110b-0000-1000-8000-00805f9b34fb"),
            Some(0x110b)
        );
        assert_eq!(profile_name(0x110b), Some("Audio sink (A2DP)"));
        // A vendor UUID is not in the SIG's base range and means nothing to
        // this table — it must not be read as an assigned number.
        assert_eq!(
            assigned_number("03b80e5a-ede8-4b33-a751-6ce34ec4c700"),
            None
        );
        // Nor may a UUID that merely starts with the right four zeroes.
        assert_eq!(
            assigned_number("0000110b-0000-1000-8000-ffffffffffff"),
            None
        );
    }

    #[test]
    fn bluetoothd_reports_its_own_version_as_a_modalias() {
        // BlueZ presents itself as a virtual USB device whose release field
        // is its version; this one is 5.86, confirmed against
        // `bluetoothctl --version`.
        assert_eq!(
            bluez_version("usb:v1D6Bp0246d0556").as_deref(),
            Some("5.86")
        );
        assert_eq!(bluez_version("nonsense"), None);
    }

    #[test]
    fn an_adapter_decodes_every_packed_field() {
        let objects = objects();
        let node = adapter("hci0", Path::new("/nonexistent/hci0"), Some(&objects));

        assert_eq!(node.source.as_deref(), Some("busctl"));
        assert_eq!(
            field(&node, "Bluetooth").as_deref(),
            Some("4.2 (HCI version 8)")
        );
        assert_eq!(field(&node, "Chipset vendor").as_deref(), Some("Intel"));
        assert_eq!(field(&node, "BlueZ").as_deref(), Some("5.86"));
        assert_eq!(field(&node, "Class").as_deref(), Some("Computer / Laptop"));
        // Alias equals Name here, so it is not repeated as its own row.
        assert!(field(&node, "Alias").is_none());

        // The vendor UUID is counted, not printed: 36 characters that identify
        // nothing to a reader.
        let profiles = field(&node, "Profiles").expect("profiles are listed");
        assert!(profiles.contains("Audio sink (A2DP)"), "{profiles}");
        assert!(profiles.contains("1 vendor-specific service"), "{profiles}");
        assert!(!profiles.contains("03b80e5a"), "{profiles}");
    }

    #[test]
    fn an_unknown_company_id_is_numbered_rather_than_named() {
        // The SIG list runs to thousands of entries and only a handful are
        // ever a controller, so naming the wrong vendor is the likelier error.
        let mut objects = objects();
        objects["/org/bluez/hci0"]["org.bluez.Adapter1"]["Manufacturer"]["data"] =
            serde_json::json!(31337);
        let node = adapter("hci0", Path::new("/nonexistent/hci0"), Some(&objects));
        assert_eq!(
            field(&node, "Chipset vendor").as_deref(),
            Some("SIG company 31337")
        );
    }

    #[test]
    fn a_connected_device_reports_its_battery_as_a_reserve() {
        let objects = objects();
        let node = devices(&objects);
        let headphones = child(&node, "Headphones");

        assert_eq!(field(headphones, "Kind").as_deref(), Some("audio-headset"));
        assert_eq!(
            field(headphones, "Class").as_deref(),
            Some("Audio/video / Headphones")
        );
        assert_eq!(field(headphones, "Signal").as_deref(), Some("-56 dBm"));
        assert_eq!(
            field(headphones, "Modalias").as_deref(),
            Some("bluetooth:v004Cp6006d0F70")
        );

        // A battery is a reserve, so it reddens as it empties. The bug this
        // guards is the one the gauges already had once: 15 % charge drawn as
        // if 15 % were a healthy low number.
        let battery = headphones
            .children
            .iter()
            .find(|child| child.label == "Battery")
            .and_then(|child| child.value.clone())
            .expect("battery is reported");
        match battery {
            Value::Gauge {
                percent, polarity, ..
            } => {
                assert_eq!(percent, 15.0);
                assert_eq!(polarity, crate::model::Polarity::HighIsGood);
            }
            other => panic!("expected a gauge, got {other:?}"),
        }
    }

    #[test]
    fn a_blocked_device_says_so_and_an_unblocked_one_stays_quiet() {
        let objects = objects();
        let node = devices(&objects);

        // Blocked is why a device will not connect, so it is stated.
        assert_eq!(
            field(child(&node, "Keyboard K380"), "Blocked").as_deref(),
            Some("yes")
        );
        // And left off everything else, rather than a row of "no" per device.
        assert!(field(child(&node, "Headphones"), "Blocked").is_none());
    }

    #[test]
    fn an_adapter_without_bluez_still_reports_what_the_kernel_knows() {
        // bluetoothd not running is a normal state, and the rfkill block that
        // usually explains it is readable regardless.
        let node = adapter("hci0", Path::new("/nonexistent/hci0"), None);
        assert_eq!(node.source, None);
        let bluez = field(&node, "BlueZ").expect("the gap is named");
        assert!(
            bluez.contains("not reachable") || bluez.contains("not queried"),
            "{bluez}"
        );
    }

    #[test]
    fn a_machine_with_no_adapter_says_so() {
        // Collected against the real machine: this asserts the shape of the
        // answer, whichever it is.
        let nodes = Bluetooth.collect().expect("collection failed");
        assert!(!nodes.is_empty());
        if std::path::Path::new(SYS)
            .read_dir()
            .is_ok_and(|mut d| d.next().is_none())
        {
            assert_eq!(
                field(&nodes[0], "Present").as_deref(),
                Some("no — no Bluetooth adapter exposed by the kernel")
            );
        }
    }
}
