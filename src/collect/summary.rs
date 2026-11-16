//! Summary — one page answering "what is this machine".
//!
//! An inventory snapshot, matching what hardinfo2's Summary is for: identity,
//! installed hardware, and capacity. Deliberately not a monitor — no
//! utilisation gauges, no history, no graphs. The few live figures here
//! (memory in use, link state) are ones hardinfo2 also refreshes.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::{Node, Value, format_bytes, format_duration};
use crate::sysfs;

pub struct Summary;

impl Collector for Summary {
    fn id(&self) -> &'static str {
        "summary"
    }

    fn title(&self) -> &'static str {
        "Summary"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn refresh_interval(&self) -> Option<Duration> {
        // hardinfo2 refreshes its Summary memory figure every second.
        Some(Duration::from_secs(1))
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes = vec![system(), processor(), memory()];
        nodes.extend([storage(), network()].into_iter().flatten());
        Ok(nodes)
    }
}

fn system() -> Node {
    let mut node = Node::group("System", vec![]);
    node.push_opt("Host", sysfs::read("/etc/hostname"));
    node.push_opt("Machine", machine_type());
    node.push_opt(
        "Operating system",
        os_release("PRETTY_NAME").or_else(|| os_release("NAME")),
    );

    if let Some(release) = sysfs::read("/proc/sys/kernel/osrelease") {
        node.push(Node::field(
            "Kernel",
            format!("Linux {release} {}", std::env::consts::ARCH),
        ));
    }
    node.push_opt("Uptime", uptime().map(format_duration));
    node.push_opt("User", user_name());
    node
}

/// Vendor, model, and form factor from DMI.
///
/// `product_version` carries the marketing name on Lenovo machines
/// ("ThinkPad P51") while `product_name` is the bare model code, so prefer it
/// and fall back the other way for vendors that leave it empty.
fn machine_type() -> Option<String> {
    const DMI: &str = "/sys/class/dmi/id";

    let vendor = sysfs::read(format!("{DMI}/sys_vendor"));
    let model = sysfs::read(format!("{DMI}/product_version"))
        .filter(|v| !v.is_empty() && !is_dmi_placeholder(v))
        .or_else(|| sysfs::read(format!("{DMI}/product_name")));

    let mut parts: Vec<String> = [vendor, model]
        .into_iter()
        .flatten()
        .filter(|p| !p.is_empty() && !is_dmi_placeholder(p))
        .collect();
    if parts.is_empty() {
        // Boards without DMI (most ARM) describe themselves in the device tree.
        return sysfs::read("/proc/device-tree/model");
    }

    if let Some(chassis) = sysfs::read_u64(format!("{DMI}/chassis_type")).and_then(chassis_name) {
        parts.push(format!("({chassis})"));
    }
    Some(parts.join(" "))
}

/// Firmware that was never filled in at the factory.
fn is_dmi_placeholder(value: &str) -> bool {
    const PLACEHOLDERS: [&str; 6] = [
        "To be filled by O.E.M.",
        "To Be Filled By O.E.M.",
        "System Product Name",
        "System manufacturer",
        "Default string",
        "None",
    ];
    PLACEHOLDERS.iter().any(|p| p.eq_ignore_ascii_case(value))
}

/// SMBIOS chassis types (spec table 7.4.1), abridged to the ones seen in life.
fn chassis_name(code: u64) -> Option<&'static str> {
    Some(match code {
        3 | 4 | 6 | 7 | 15 => "Desktop",
        8 | 9 | 10 | 14 => "Notebook",
        11 => "Handheld",
        13 => "All-in-one",
        17 | 23 | 28 | 29 => "Server",
        24 => "Sealed-case PC",
        30 => "Tablet",
        31 | 32 => "Convertible",
        34 => "Embedded PC",
        35 => "Mini PC",
        36 => "Stick PC",
        // 1 Other and 2 Unknown carry no information worth printing.
        _ => return None,
    })
}

fn os_release(key: &str) -> Option<String> {
    let text = sysfs::read("/etc/os-release").or_else(|| sysfs::read("/usr/lib/os-release"))?;
    let prefix = format!("{key}=");
    let value = text.lines().find_map(|line| line.strip_prefix(&prefix))?;
    // Values may be quoted when they contain spaces.
    Some(value.trim_matches('"').to_string())
}

fn uptime() -> Option<u64> {
    // "10157.83 62501.45" — uptime then summed idle time across CPUs.
    let raw = sysfs::read("/proc/uptime")?;
    let seconds: f64 = raw.split_whitespace().next()?.parse().ok()?;
    Some(seconds as u64)
}

fn user_name() -> Option<String> {
    if let Ok(name) = std::env::var("USER").or_else(|_| std::env::var("LOGNAME"))
        && !name.is_empty()
    {
        return Some(name);
    }
    // No environment to read (a service, say): resolve the uid through passwd
    // rather than giving up. Avoids linking libc's NSS machinery.
    let uid = rustix::process::getuid().as_raw().to_string();
    let passwd = sysfs::read("/etc/passwd")?;
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let found = fields.nth(1)? == uid;
        found.then(|| name.to_string())
    })
}

/// Processor identity, not load: model, topology, and the clock ceiling.
fn processor() -> Node {
    let mut node = Node::group("Processor", vec![]);

    let cpuinfo = sysfs::read("/proc/cpuinfo").unwrap_or_default();
    let blocks = sysfs::parse_cpuinfo_blocks(&cpuinfo);
    let cpus: Vec<_> = blocks
        .iter()
        .filter(|b| sysfs::find(b, "processor").is_some())
        .collect();

    if let Some(first) = cpus.first() {
        node.push_opt(
            "Model",
            sysfs::find(first, "model name").or_else(|| sysfs::find(first, "Model")),
        );

        let cores: BTreeSet<(&str, &str)> = cpus
            .iter()
            .filter_map(|b| {
                Some((
                    sysfs::find(b, "physical id").unwrap_or("0"),
                    sysfs::find(b, "core id")?,
                ))
            })
            .collect();
        node.push(Node::field(
            "Topology",
            if cores.is_empty() {
                format!("{} threads", cpus.len())
            } else {
                format!("{} cores / {} threads", cores.len(), cpus.len())
            },
        ));
    }

    node.push_opt(
        "Maximum clock",
        sysfs::read_u64("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
            .map(|khz| Value::Hertz(khz * 1000)),
    );
    node
}

/// Installed memory and how much of it is in use.
///
/// hardinfo2 refreshes its Summary memory figure every second, so a live
/// reading here is faithful to the original — unlike a utilisation history,
/// which belongs in a monitor.
fn memory() -> Node {
    let mut node = Node::group("Memory", vec![]);
    let Some(raw) = sysfs::read("/proc/meminfo") else {
        return node;
    };
    let fields: BTreeMap<String, u64> = sysfs::parse_colon_pairs(&raw)
        .into_iter()
        .filter_map(|(key, value)| {
            let number = value.strip_suffix(" kB").unwrap_or(&value);
            Some((key, number.trim().parse::<u64>().ok()? * 1024))
        })
        .collect();
    let get = |key: &str| fields.get(key).copied();

    node.push_opt("Installed", get("MemTotal").map(Value::Bytes));
    // "In use" excludes reclaimable cache, matching free(1).
    if let (Some(total), Some(available)) = (get("MemTotal"), get("MemAvailable")) {
        node.push(Node::field(
            "In use",
            Value::bytes_gauge(total.saturating_sub(available), total),
        ));
    }
    match (get("SwapTotal"), get("SwapFree")) {
        (Some(total), Some(free)) if total > 0 => node.push(Node::field(
            "Swap",
            Value::bytes_gauge(total.saturating_sub(free), total),
        )),
        // Say so explicitly: an absent swap row reads as a collector bug.
        _ => node.push(Node::field("Swap", "none")),
    }
    node
}

/// Whole block devices with their model and size.
///
/// hardinfo2's Summary lists storage *hardware*; per-filesystem free space is
/// the Filesystems page's job.
fn storage() -> Option<Node> {
    let mut node = Node::group("Storage", vec![]);

    for device in sysfs::list_dir("/sys/block") {
        let Some(name) = device.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        // Only devices sitting behind a driver are installed hardware. This
        // drops loop, ram, zram, md, and the device-mapper targets — a LUKS
        // mapper is not a disk, and its name swamps the row.
        if !device.join("device").is_dir() {
            continue;
        }
        // `size` is in 512-byte sectors regardless of the device's block size.
        let Some(sectors) = sysfs::read_u64(device.join("size")).filter(|s| *s > 0) else {
            continue;
        };

        let mut detail = vec![format_bytes(sectors * 512)];
        if let Some(model) = sysfs::read(device.join("device/model"))
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
        {
            detail.push(model);
        }
        match sysfs::read_u64(device.join("queue/rotational")) {
            Some(1) => detail.push("rotational".to_string()),
            Some(0) => detail.push("solid state".to_string()),
            _ => {}
        }
        node.push(Node::field(name, detail.join(" · ")));
    }

    (!node.children.is_empty()).then_some(node)
}

/// Network interfaces present, with their link state.
fn network() -> Option<Node> {
    let mut node = Node::group("Network", vec![]);

    for interface in sysfs::list_dir("/sys/class/net") {
        let Some(name) = interface
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
        else {
            continue;
        };
        if name == "lo" {
            continue;
        }
        let state = sysfs::read(interface.join("operstate")).unwrap_or_else(|| "unknown".into());
        // Link speed is only meaningful while the link is up, and wireless
        // drivers leave it unset even then.
        let detail = match sysfs::read_u64(interface.join("speed")) {
            Some(mbps) if state == "up" => format!("{state} · {mbps} Mb/s"),
            _ => state,
        };
        node.push(Node::field(name, detail));
    }

    (!node.children.is_empty()).then_some(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_strips_quotes() {
        // Reads the real file; every Linux box has one of the two paths.
        if let Some(name) = os_release("PRETTY_NAME") {
            assert!(!name.starts_with('"'), "quotes should be trimmed: {name}");
            assert!(!name.is_empty());
        }
    }

    #[test]
    fn placeholder_dmi_strings_are_rejected() {
        assert!(is_dmi_placeholder("To be filled by O.E.M."));
        assert!(is_dmi_placeholder("Default string"));
        assert!(!is_dmi_placeholder("ThinkPad P51"));
    }

    #[test]
    fn collects_on_this_machine() {
        let nodes = Summary.collect().expect("summary collection failed");
        let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(&labels[..3], &["System", "Processor", "Memory"]);

        let system = &nodes[0];
        for expected in ["Kernel", "Uptime"] {
            assert!(
                system.children.iter().any(|c| c.label == expected),
                "expected a {expected} field"
            );
        }
    }

    #[test]
    fn summary_carries_no_utilisation_history() {
        // Guards the scope line: this page is an inventory snapshot, not a
        // monitor. Utilisation gauges and load history belong in a monitor.
        let nodes = Summary.collect().expect("summary collection failed");
        let processor = nodes
            .iter()
            .find(|n| n.label == "Processor")
            .expect("processor section");
        for child in &processor.children {
            assert!(
                !child.label.contains("Utilisation") && !child.label.contains("minute"),
                "unexpected monitoring field: {}",
                child.label
            );
        }
    }
}
