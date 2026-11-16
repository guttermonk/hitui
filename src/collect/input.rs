//! Input devices, from /proc/bus/input/devices.
//!
//! The kernel writes one record per device as `X: key=value` lines separated
//! by blank lines, where the leading letter is the field class — `I` for
//! identity, `N` for name, `H` for the handlers userspace talks to.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::sysfs;

pub struct Input;

impl Collector for Input {
    fn id(&self) -> &'static str {
        "input"
    }

    fn title(&self) -> &'static str {
        "Input Devices"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let Some(text) = sysfs::read("/proc/bus/input/devices") else {
            return Ok(vec![Node::group(
                "Input devices",
                vec![Node::field(
                    "Status",
                    "/proc/bus/input/devices is unavailable — CONFIG_INPUT may be absent",
                )],
            )]);
        };

        let devices = parse(&text);
        let mut nodes = vec![Node::group(
            "Input",
            vec![Node::field("Devices", devices.len())],
        )];

        for device in devices {
            let mut node = Node::group(
                match &device.name {
                    Some(name) => name.clone(),
                    None => "unnamed device".to_string(),
                },
                vec![],
            );
            node.push_opt("Kind", Some(device.kind()));
            node.push_opt("Bus", device.bus_name().map(str::to_string));
            node.push_opt("Vendor / product", device.ids());
            node.push_opt("Version", device.get('I', "Version"));
            node.push_opt("Physical path", device.get('P', "Phys"));
            node.push_opt("Sysfs path", device.get('S', "Sysfs"));
            node.push_opt("Unique identifier", device.get('U', "Uniq"));
            // The handlers are the device nodes userspace opens, which is what
            // you need when working out which /dev/input/eventN is which.
            node.push_opt("Handlers", device.handlers());
            nodes.push(node);
        }
        Ok(nodes)
    }
}

struct Device {
    /// `(class letter, key, value)` in file order.
    fields: Vec<(char, String, String)>,
    name: Option<String>,
}

impl Device {
    fn get(&self, class: char, key: &str) -> Option<String> {
        self.fields
            .iter()
            .find(|(c, k, _)| *c == class && k == key)
            .map(|(_, _, v)| v.trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
    }

    /// `Vendor=046d Product=c52b` as a single pair.
    fn ids(&self) -> Option<String> {
        let vendor = self.get('I', "Vendor")?;
        let product = self.get('I', "Product")?;
        Some(format!("{vendor}:{product}"))
    }

    /// The bus a device is attached to, named rather than numbered.
    ///
    /// Values are the `BUS_*` constants from input.h, in hex.
    fn bus_name(&self) -> Option<&'static str> {
        Some(match self.get('I', "Bus")?.as_str() {
            "0001" => "PCI",
            "0003" => "USB",
            "0005" => "Bluetooth",
            "0010" => "ISA",
            "0011" => "i8042 (PS/2)",
            "0017" => "parallel port",
            "0018" => "ADB",
            "0019" => "I2C",
            "001f" => "host",
            "0006" => "virtual",
            _ => return None,
        })
    }

    /// What the device is, inferred from the handlers the kernel bound.
    ///
    /// The kernel does not label devices by type; the handler set is how
    /// userspace works it out too.
    fn kind(&self) -> String {
        let handlers = self.get('H', "Handlers").unwrap_or_default();
        let mut kinds = Vec::new();

        if handlers.contains("kbd") {
            kinds.push("keyboard");
        }
        if handlers.contains("mouse") {
            kinds.push("pointer");
        }
        if handlers.contains("js") {
            kinds.push("joystick");
        }
        if handlers.contains("leds") {
            kinds.push("LEDs");
        }
        if kinds.is_empty() {
            // An event-only device is real but unclassifiable from here:
            // lid switches, power buttons and accelerometers all land here.
            return "event source".to_string();
        }
        kinds.join(", ")
    }

    fn handlers(&self) -> Option<String> {
        self.get('H', "Handlers")
            .map(|h| h.split_whitespace().collect::<Vec<_>>().join(", "))
    }
}

/// Split the file into records and each record into classified fields.
fn parse(text: &str) -> Vec<Device> {
    text.split("\n\n")
        .filter_map(|record| {
            let mut fields = Vec::new();
            for line in record.lines() {
                // "I: Bus=0011 Vendor=0001" — a class letter, then pairs. The
                // N/P/S/U classes hold a single value that may contain spaces,
                // so those are not split further.
                let (class, rest) = line.split_once(": ")?;
                let class = class.chars().next()?;

                if matches!(class, 'N' | 'P' | 'S' | 'U') {
                    if let Some((key, value)) = rest.split_once('=') {
                        fields.push((class, key.trim().to_string(), value.trim().to_string()));
                    }
                    continue;
                }
                for pair in rest.split_whitespace() {
                    if let Some((key, value)) = pair.split_once('=') {
                        fields.push((class, key.to_string(), value.to_string()));
                    }
                }
                // Handlers are space-separated under one key, so the pair split
                // above kept only the first. Replace it with the whole list.
                if class == 'H'
                    && let Some(list) = rest.strip_prefix("Handlers=")
                {
                    fields.retain(|(c, k, _)| !(*c == 'H' && k == "Handlers"));
                    fields.push((class, "Handlers".to_string(), list.trim().to_string()));
                }
            }
            if fields.is_empty() {
                return None;
            }
            let name = fields
                .iter()
                .find(|(c, k, _)| *c == 'N' && k == "Name")
                .map(|(_, _, v)| v.trim_matches('"').to_string());
            Some(Device { fields, name })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD: &str = "I: Bus=0011 Vendor=0001 Product=0001 Version=ab83\n\
                          N: Name=\"AT Translated Set 2 keyboard\"\n\
                          P: Phys=isa0060/serio0/input0\n\
                          S: Sysfs=/devices/platform/i8042/serio0/input/input0\n\
                          U: Uniq=\n\
                          H: Handlers=sysrq kbd leds event0 \n\
                          B: EV=120013\n";

    #[test]
    fn parses_a_record_into_classified_fields() {
        let devices = parse(RECORD);
        assert_eq!(devices.len(), 1);
        let device = &devices[0];

        // Quoted names are unquoted, and names contain spaces.
        assert_eq!(device.name.as_deref(), Some("AT Translated Set 2 keyboard"));
        assert_eq!(device.ids().unwrap(), "0001:0001");
        assert_eq!(device.bus_name(), Some("i8042 (PS/2)"));
        assert_eq!(
            device.get('S', "Sysfs").unwrap(),
            "/devices/platform/i8042/serio0/input/input0"
        );
        // An empty value is absent, not an empty string.
        assert!(device.get('U', "Uniq").is_none());
    }

    #[test]
    fn handlers_stay_together_as_one_list() {
        let devices = parse(RECORD);
        // The naive split on whitespace would leave only "sysrq".
        assert_eq!(
            devices[0].handlers().unwrap(),
            "sysrq, kbd, leds, event0",
            "every handler should survive"
        );
    }

    #[test]
    fn infers_device_kind_from_handlers() {
        assert_eq!(parse(RECORD)[0].kind(), "keyboard, LEDs");

        let mouse = "N: Name=\"Mouse\"\nH: Handlers=mouse0 event1 \n";
        assert_eq!(parse(mouse)[0].kind(), "pointer");

        // A lid switch has only an event handler and cannot be classified.
        let lid = "N: Name=\"Lid Switch\"\nH: Handlers=event2 \n";
        assert_eq!(parse(lid)[0].kind(), "event source");
    }

    #[test]
    fn collects_on_this_machine() {
        let nodes = Input.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Input");

        let count = nodes[0].children[0].value.as_ref().unwrap().to_string();
        assert_eq!(count, (nodes.len() - 1).to_string(), "count must match");
    }
}
