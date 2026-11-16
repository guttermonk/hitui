//! Block devices, their partitions, and drive health.
//!
//! Geometry, queue settings and partition layout come from /sys/block. Drive
//! health does not live in sysfs at all — it is behind an ATA pass-through or
//! an NVMe admin command — so where `smartctl` is installed hitui asks it, in
//! its JSON mode rather than by scraping the human-readable report.
//!
//! SMART normally needs root. When it is refused, the page says so and still
//! shows everything sysfs knows, because "no health section" and "this drive
//! has no health data" are different statements.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use serde_json::Value as Json;

use crate::collect::filesystems;
use crate::collect::{Collector, Group};
use crate::model::{Node, Value, format_bytes, format_duration};
use crate::probe;
use crate::sysfs;

/// Every block device reports `size` in 512-byte sectors, whatever its own
/// logical block size happens to be.
const SECTOR: u64 = 512;

pub struct Storage;

impl Collector for Storage {
    fn id(&self) -> &'static str {
        "storage"
    }

    fn title(&self) -> &'static str {
        "Storage"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let devices = devices();
        let usage = Usage::read();

        // Query each drive exactly once. smartctl talks to the hardware, so
        // asking twice — once for the summary line, once for the health
        // section — is a real cost, not just a duplicated function call.
        let mut reports: BTreeMap<String, Smart> = devices
            .iter()
            .map(|name| (name.clone(), smart(name)))
            .collect();

        let mut overview = Node::group("Storage", vec![]);
        overview.push(Node::field("Devices", devices.len()));
        overview.push(Node::field(
            "Drive health",
            smart_availability(reports.values().next()),
        ));

        let mut nodes = vec![overview];
        for name in &devices {
            nodes.push(device(name, &usage, reports.remove(name)));
        }
        Ok(nodes)
    }
}

/// Physical block devices, excluding kernel plumbing.
///
/// A device sitting behind a driver is real hardware; loop, ram, zram and the
/// device-mapper targets are not, and belong on a (future) volumes page.
fn devices() -> Vec<String> {
    sysfs::list_dir("/sys/block")
        .into_iter()
        .filter(|path| path.join("device").is_dir())
        .filter_map(|path| path.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect()
}

/// What the kernel says is using each block device.
struct Usage {
    /// Mount points per device, keyed as /proc/self/mounts names it
    /// (`nvme0n1p1`, `mapper/luks-…`).
    ///
    /// Several per device rather than one: btrfs subvolumes put `/`, `/home`
    /// and `/nix` on a single device, and reporting only one of them would
    /// understate what the partition is doing.
    mounts: BTreeMap<String, Vec<String>>,
    /// Devices in use as swap, by kernel name (`dm-1`, `nvme0n1p3`). Swap is
    /// not a mount, so it is absent from the mount table entirely.
    swaps: BTreeSet<String>,
}

impl Usage {
    fn read() -> Self {
        let mut mounts: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for mount in filesystems::mounts() {
            if let Some(device) = mount.device.strip_prefix("/dev/") {
                mounts
                    .entry(device.to_string())
                    .or_default()
                    .push(mount.mount_point);
            }
        }
        for points in mounts.values_mut() {
            // Shortest first: the top of a subvolume tree is the useful label.
            points.sort_by_key(|p| (p.len(), p.clone()));
            points.dedup();
        }

        Self {
            mounts,
            swaps: swap_devices(),
        }
    }
}

/// Parse /proc/swaps, which names devices by their kernel path.
fn swap_devices() -> BTreeSet<String> {
    let Some(text) = sysfs::read("/proc/swaps") else {
        return BTreeSet::new();
    };
    text.lines()
        .skip(1) // Header row.
        .filter_map(|line| {
            let path = line.split_whitespace().next()?;
            Some(path.strip_prefix("/dev/")?.to_string())
        })
        .collect()
}

/// One line explaining whether health data could be read, and why not.
///
/// Judged from one drive's outcome rather than reported per-drive: the usual
/// answer is uniform because it is a permission question, not a device one.
fn smart_availability(sample: Option<&Smart>) -> &'static str {
    if !probe::available("smartctl") {
        return "unavailable — install smartmontools for drive health";
    }
    match sample {
        Some(Smart::Ok(_)) => "read by smartctl",
        Some(Smart::Denied) => "unavailable — smartctl needs root for this drive",
        Some(Smart::Failed) => "unavailable — smartctl could not read this drive",
        None => "no drives to query",
    }
}

fn device(name: &str, usage: &Usage, report: Option<Smart>) -> Node {
    let dir = Path::new("/sys/block").join(name);
    let trimmed = |file: &str| {
        sysfs::read(dir.join(file))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    let sectors = sysfs::read_u64(dir.join("size")).unwrap_or(0);
    let model = trimmed("device/model");
    let heading = match &model {
        Some(model) => format!("{name}  {model}"),
        None => name.to_string(),
    };

    let mut node = Node::group(heading, vec![]);
    node.push(Node::field("Size", Value::Bytes(sectors * SECTOR)));
    node.push_opt("Model", model);
    // SATA splits the name across vendor and model; NVMe puts it all in model.
    node.push_opt("Vendor", trimmed("device/vendor"));
    node.push_opt(
        "Firmware",
        trimmed("device/firmware_rev").or_else(|| trimmed("device/rev")),
    );
    node.push_opt("Serial", trimmed("device/serial"));

    match sysfs::read_u64(dir.join("queue/rotational")) {
        Some(1) => node.push(Node::field("Type", "rotational")),
        Some(0) => node.push(Node::field("Type", "solid state")),
        _ => {}
    }
    node.push_opt(
        "Removable",
        sysfs::read_u64(dir.join("removable")).map(|v| Value::Flag(v != 0)),
    );
    node.push_opt(
        "Logical block",
        sysfs::read_u64(dir.join("queue/logical_block_size")).map(Value::Bytes),
    );
    node.push_opt(
        "Physical block",
        sysfs::read_u64(dir.join("queue/physical_block_size")).map(Value::Bytes),
    );
    node.push_opt(
        "Scheduler",
        trimmed("queue/scheduler").map(|s| active_scheduler(&s)),
    );

    if let Some(partitions) = partitions(&dir, usage) {
        node.push(partitions);
    }
    if let Some(Smart::Ok(report)) = report {
        node.push(health(&report).from("smartctl"));
    }
    node
}

/// The partition table as sysfs sees it, annotated with mount points.
fn partitions(dir: &Path, usage: &Usage) -> Option<Node> {
    let mut node = Node::group("Partitions", vec![]);

    for entry in sysfs::list_dir(dir) {
        // A partition directory is a child of the disk and carries `partition`.
        if !entry.join("partition").is_file() {
            continue;
        }
        let Some(name) = entry.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        let sectors = sysfs::read_u64(entry.join("size")).unwrap_or(0);

        let mut detail = vec![format_bytes(sectors * SECTOR)];
        detail.push(usage_of(&entry, &name, usage));
        node.push(Node::field(name, detail.join(" · ")));
    }

    (!node.children.is_empty()).then_some(node)
}

/// What a partition is actually being used for.
///
/// A partition can be in use without being mounted: an encrypted or LVM volume
/// is consumed by a device-mapper target, and the filesystem is mounted from
/// *that*. Reporting the raw partition as "not mounted" would be true but
/// badly misleading, so the mapping is followed through `holders/`.
fn usage_of(partition: &Path, name: &str, usage: &Usage) -> String {
    if let Some(points) = usage.mounts.get(name) {
        return format!("mounted at {}", points.join(", "));
    }
    if usage.swaps.contains(name) {
        return "in use as swap".to_string();
    }

    for holder in sysfs::list_dir(partition.join("holders")) {
        let Some(holder_name) = holder.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        // `holders/dm-0` is a kernel name; the mapped name is what shows up in
        // /dev/mapper and therefore in the mount table.
        let mapped = sysfs::read(Path::new("/sys/block").join(&holder_name).join("dm/name"));
        let kind = sysfs::read(Path::new("/sys/block").join(&holder_name).join("dm/uuid"))
            .map(|uuid| mapping_kind(&uuid))
            .unwrap_or("device-mapper");

        let target = mapped.clone().unwrap_or_else(|| holder_name.clone());
        // The mapped device is what is mounted or swapped on, so resolve its
        // use too — otherwise an encrypted root looks idle.
        let downstream = mapped
            .and_then(|name| usage.mounts.get(&format!("mapper/{name}")).cloned())
            .map(|points| format!(", mounted at {}", points.join(", ")))
            .or_else(|| {
                usage
                    .swaps
                    .contains(&holder_name)
                    .then(|| ", in use as swap".to_string())
            })
            .unwrap_or_default();
        return format!("{kind} → {target}{downstream}");
    }

    // Genuinely idle, which is often exactly what you were looking for.
    "not mounted".to_string()
}

/// Classify a device-mapper target from its UUID prefix.
///
/// Shared with the Volumes page, which classifies the same mappings from the
/// other direction.
pub(crate) fn mapping_kind(uuid: &str) -> &'static str {
    // dm encodes the subsystem in the UUID: CRYPT-LUKS2-…, LVM-…, mpath-….
    if uuid.starts_with("CRYPT-LUKS2") {
        "LUKS2"
    } else if uuid.starts_with("CRYPT-LUKS1") {
        "LUKS1"
    } else if uuid.starts_with("CRYPT") {
        "dm-crypt"
    } else if uuid.starts_with("LVM") {
        "LVM"
    } else if uuid.starts_with("mpath") {
        "multipath"
    } else {
        "device-mapper"
    }
}

/// `[none] mq-deadline kyber` — the active scheduler is the bracketed one.
fn active_scheduler(raw: &str) -> String {
    for word in raw.split_whitespace() {
        if let Some(active) = word.strip_prefix('[').and_then(|w| w.strip_suffix(']')) {
            return active.to_string();
        }
    }
    raw.trim().to_string()
}

/// Outcome of asking smartctl about a drive.
enum Smart {
    Ok(Json),
    /// smartctl ran but was not allowed to open the device.
    Denied,
    /// Absent, timed out, or produced nothing usable.
    Failed,
}

/// Query SMART data in smartctl's JSON mode.
///
/// `-j` gives a documented object instead of a report meant for humans, and it
/// reports its own failures *inside* that object — a permission error comes
/// back as a message with `severity: "error"` alongside `exit_status: 2`, not
/// as empty output. So the JSON has to be inspected rather than trusted.
fn smart(device: &str) -> Smart {
    let node = format!("/dev/{device}");
    let Some(output) = probe::run("smartctl", &["--json", "-H", "-A", "-i", &node]) else {
        return Smart::Failed;
    };
    let Ok(report) = serde_json::from_str::<Json>(&output) else {
        return Smart::Failed;
    };

    let messages = report
        .pointer("/smartctl/messages")
        .and_then(Json::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|m| m.get("severity").and_then(Json::as_str) == Some("error"))
                .filter_map(|m| m.get("string").and_then(Json::as_str))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();

    if messages.contains("Permission denied") || messages.contains("requires") {
        return Smart::Denied;
    }
    if !messages.is_empty() {
        return Smart::Failed;
    }
    Smart::Ok(report)
}

/// Translate a SMART report into the handful of figures that predict failure.
fn health(report: &Json) -> Node {
    let mut node = Node::group("Health", vec![]);

    let string = |pointer: &str| {
        report
            .pointer(pointer)
            .and_then(Json::as_str)
            .map(str::to_string)
    };
    let number = |pointer: &str| report.pointer(pointer).and_then(Json::as_u64);

    node.push_opt(
        "SMART status",
        report
            .pointer("/smart_status/passed")
            .and_then(Json::as_bool)
            .map(|passed| if passed { "passed" } else { "FAILING" }),
    );
    node.push_opt("Protocol", string("/device/protocol"));
    node.push_opt(
        "Temperature",
        number("/temperature/current").map(|c| format!("{c} °C")),
    );
    // NVMe controllers often carry several sensors; the hottest is not always
    // the one reported as "current".
    if let Some(sensors) = report
        .pointer("/nvme_smart_health_information_log/temperature_sensors")
        .and_then(Json::as_array)
        .filter(|sensors| sensors.len() > 1)
    {
        let readings: Vec<String> = sensors
            .iter()
            .filter_map(Json::as_u64)
            .map(|c| format!("{c} °C"))
            .collect();
        node.push(Node::field("Sensors", readings.join(" · ")));
    }

    // NVMe publishes a fixed health log; ATA exposes the same ideas as
    // numbered attributes, so each is read from where it actually lives.
    let nvme = "/nvme_smart_health_information_log";
    let power_on = number(&format!("{nvme}/power_on_hours"))
        .or_else(|| number("/power_on_time/hours"))
        .or_else(|| ata_attribute(report, 9));
    node.push_opt("Power-on hours", power_on);
    node.push_opt(
        "Power cycles",
        number(&format!("{nvme}/power_cycles"))
            .or_else(|| number("/power_cycle_count"))
            .or_else(|| ata_attribute(report, 12)),
    );

    // Wear: how much of the drive's rated endurance is gone. The single most
    // useful number on an SSD, and a gauge makes it readable at a glance.
    if let Some(used) = number(&format!("{nvme}/percentage_used")) {
        node.push(Node::field(
            "Endurance used",
            Value::gauge(used as f64, format!("{used} % of rated writes")),
        ));
    }
    // Spare blocks are a reserve that depletes, so an empty bar is the alarm.
    // The drive publishes the threshold it considers critical, which is better
    // context than any figure hitui could pick.
    if let Some(spare) = number(&format!("{nvme}/available_spare")) {
        let caption = match number(&format!("{nvme}/available_spare_threshold")) {
            Some(threshold) => format!("{spare} % remaining, critical below {threshold} %"),
            None => format!("{spare} % remaining"),
        };
        node.push(Node::field(
            "Spare blocks",
            Value::reserve_gauge(spare as f64, caption),
        ));
    }

    // Written and read volumes are reported in 512 KB units for NVMe.
    for (label, field) in [
        ("Data written", "data_units_written"),
        ("Data read", "data_units_read"),
    ] {
        node.push_opt(
            label,
            number(&format!("{nvme}/{field}")).map(|units| Value::Bytes(units * 512 * 1000)),
        );
    }

    for (label, field) in [
        ("Media errors", "media_errors"),
        ("Unsafe shutdowns", "unsafe_shutdowns"),
        ("Critical warnings", "critical_warning"),
        ("Error log entries", "num_err_log_entries"),
    ] {
        node.push_opt(label, number(&format!("{nvme}/{field}")));
    }

    // Time spent above the drive's own warning and critical thresholds, in
    // minutes. Zero is the normal reading, so these only appear when the drive
    // has actually been throttling — which is the whole point of showing them.
    for (label, field) in [
        ("Time over warning temp", "warning_temp_time"),
        ("Time over critical temp", "critical_comp_time"),
    ] {
        node.push_opt(
            label,
            number(&format!("{nvme}/{field}"))
                .filter(|minutes| *minutes > 0)
                .map(|minutes| format_duration(minutes * 60)),
        );
    }
    node.push_opt(
        "Controller busy time",
        number(&format!("{nvme}/controller_busy_time"))
            .filter(|minutes| *minutes > 0)
            .map(|minutes| format_duration(minutes * 60)),
    );

    // Reallocated sectors are the classic early sign of a dying spinning disk.
    node.push_opt("Reallocated sectors", ata_attribute(report, 5));
    node.push_opt("Pending sectors", ata_attribute(report, 197));

    if node.children.is_empty() {
        node.push(Node::field(
            "Status",
            "smartctl returned no health data for this drive",
        ));
    }
    node
}

/// The raw value of a numbered ATA SMART attribute.
fn ata_attribute(report: &Json, id: u64) -> Option<u64> {
    report
        .pointer("/ata_smart_attributes/table")?
        .as_array()?
        .iter()
        .find(|attribute| attribute.get("id").and_then(Json::as_u64) == Some(id))?
        .pointer("/raw/value")?
        .as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Polarity;

    #[test]
    fn picks_the_bracketed_scheduler() {
        assert_eq!(active_scheduler("[none] mq-deadline kyber"), "none");
        assert_eq!(active_scheduler("none [mq-deadline] kyber"), "mq-deadline");
        // Some kernels expose a single unbracketed value.
        assert_eq!(active_scheduler("none"), "none");
    }

    #[test]
    fn excludes_kernel_plumbing_from_the_device_list() {
        for name in devices() {
            assert!(
                !name.starts_with("loop") && !name.starts_with("dm-") && !name.starts_with("ram"),
                "{name} is not physical hardware"
            );
        }
    }

    /// A real `smartctl --json -H -A -i` report, verbatim from a Samsung
    /// MZVLB512HAJQ. Kept real so the field pointers are anchored to output
    /// smartctl actually produces rather than to the schema as I read it.
    const NVME_REPORT: &str = r#"{
        "smart_status": {"passed": true, "nvme": {"value": 0}},
        "temperature": {"current": 38},
        "nvme_smart_health_information_log": {
            "nsid": 1,
            "critical_warning": 0,
            "temperature": 38,
            "available_spare": 100,
            "available_spare_threshold": 10,
            "percentage_used": 5,
            "data_units_read": 32538942,
            "data_units_written": 112259745,
            "host_reads": 467754166,
            "host_writes": 2966775494,
            "controller_busy_time": 3558,
            "power_cycles": 1744,
            "power_on_hours": 8944,
            "unsafe_shutdowns": 143,
            "media_errors": 0,
            "num_err_log_entries": 2760,
            "warning_temp_time": 0,
            "critical_comp_time": 0,
            "temperature_sensors": [38, 57]
        }
    }"#;

    fn field_of(node: &Node, label: &str) -> Option<String> {
        node.children
            .iter()
            .find(|c| c.label == label)
            .and_then(|c| c.value.clone())
            .map(|v| v.to_string())
    }

    #[test]
    fn reads_health_out_of_a_real_nvme_report() {
        let node = health(&serde_json::from_str(NVME_REPORT).unwrap());
        let field = |label: &str| field_of(&node, label);

        assert_eq!(field("SMART status").unwrap(), "passed");
        assert_eq!(field("Temperature").unwrap(), "38 °C");
        assert_eq!(field("Power-on hours").unwrap(), "8944");
        assert_eq!(field("Power cycles").unwrap(), "1744");
        assert_eq!(field("Unsafe shutdowns").unwrap(), "143");
        assert_eq!(field("Error log entries").unwrap(), "2760");
        assert!(field("Endurance used").unwrap().contains("5 %"));

        // The drive's own critical threshold is better context than a guess.
        let spare = field("Spare blocks").unwrap();
        assert!(spare.contains("100 % remaining"), "{spare}");
        assert!(spare.contains("critical below 10 %"), "{spare}");

        // Both controller sensors, since "current" reports only the first.
        assert_eq!(field("Sensors").unwrap(), "38 °C · 57 °C");

        // NVMe data units are thousands of 512-byte blocks, so one unit is
        // 512 KB: 112259745 units is ~52 TiB.
        assert_eq!(field("Data written").unwrap(), "52.28 TiB");
        assert_eq!(field("Data read").unwrap(), "15.15 TiB");

        // Zero thermal-throttle time is the normal case and would be noise.
        assert!(field("Time over warning temp").is_none());
        assert!(field("Time over critical temp").is_none());
        assert_eq!(field("Controller busy time").unwrap(), "2 days, 11 hours");
    }

    #[test]
    fn reserves_and_consumption_are_coloured_oppositely() {
        // The bug this guards: 100 % spare blocks is perfect health, but a
        // gauge that assumes high-is-bad painted it red, as it did a fully
        // charged battery.
        let node = health(&serde_json::from_str(NVME_REPORT).unwrap());
        let polarity = |label: &str| {
            node.children
                .iter()
                .find(|c| c.label == label)
                .and_then(|c| c.value.clone())
                .and_then(|v| match v {
                    Value::Gauge { polarity, .. } => Some(polarity),
                    _ => None,
                })
        };

        assert_eq!(polarity("Spare blocks"), Some(Polarity::HighIsGood));
        assert_eq!(polarity("Endurance used"), Some(Polarity::HighIsBad));
    }

    #[test]
    fn thermal_throttling_is_reported_when_it_happened() {
        // Same drive, but having spent time above its warning threshold.
        let hot = NVME_REPORT.replace("\"warning_temp_time\": 0", "\"warning_temp_time\": 90");
        let node = health(&serde_json::from_str(&hot).unwrap());
        assert_eq!(
            field_of(&node, "Time over warning temp").unwrap(),
            "1 hour, 30 minutes"
        );
    }

    #[test]
    fn reads_health_out_of_an_ata_report() {
        // ATA drives carry the same facts as numbered attributes instead.
        let report: Json = serde_json::from_str(
            r#"{
                "device": {"protocol": "ATA"},
                "smart_status": {"passed": false},
                "ata_smart_attributes": {"table": [
                    {"id": 5, "name": "Reallocated_Sector_Ct", "raw": {"value": 8}},
                    {"id": 9, "name": "Power_On_Hours", "raw": {"value": 31000}},
                    {"id": 12, "name": "Power_Cycle_Count", "raw": {"value": 420}},
                    {"id": 197, "name": "Current_Pending_Sector", "raw": {"value": 3}}
                ]}
            }"#,
        )
        .unwrap();

        let node = health(&report);
        let field = |label: &str| {
            node.children
                .iter()
                .find(|c| c.label == label)
                .and_then(|c| c.value.clone())
                .map(|v| v.to_string())
        };

        assert_eq!(field("SMART status").unwrap(), "FAILING");
        assert_eq!(field("Power-on hours").unwrap(), "31000");
        assert_eq!(field("Power cycles").unwrap(), "420");
        assert_eq!(field("Reallocated sectors").unwrap(), "8");
        assert_eq!(field("Pending sectors").unwrap(), "3");
    }

    #[test]
    fn an_empty_report_says_so_rather_than_showing_nothing() {
        let node = health(&serde_json::from_str("{}").unwrap());
        assert_eq!(node.children.len(), 1);
        assert!(
            node.children[0]
                .value
                .as_ref()
                .unwrap()
                .to_string()
                .contains("no health data")
        );
    }

    #[test]
    fn classifies_device_mapper_targets() {
        assert_eq!(mapping_kind("CRYPT-LUKS2-abc-luks-abc"), "LUKS2");
        assert_eq!(mapping_kind("CRYPT-LUKS1-abc-luks-abc"), "LUKS1");
        assert_eq!(mapping_kind("CRYPT-PLAIN-abc"), "dm-crypt");
        assert_eq!(mapping_kind("LVM-abcdef"), "LVM");
        assert_eq!(mapping_kind("mpath-abcdef"), "multipath");
        assert_eq!(mapping_kind("something-else"), "device-mapper");
    }

    #[test]
    fn a_partition_backing_an_encrypted_volume_is_not_called_unmounted() {
        // The bug this guards: reporting the raw partition as "not mounted"
        // when it is the LUKS container the root filesystem lives in.
        let nodes = Storage.collect().expect("collection failed");
        let usage = Usage::read();

        for device in nodes.iter().skip(1) {
            let Some(partitions) = device.children.iter().find(|c| c.label == "Partitions") else {
                continue;
            };
            for partition in &partitions.children {
                let detail = partition.value.as_ref().unwrap().to_string();
                if !detail.contains("not mounted") {
                    continue;
                }
                // Anything claimed to be idle must genuinely have no holder,
                // no mount, and no swap use.
                let holders = sysfs::list_dir(
                    Path::new("/sys/block")
                        .join(device.label.split_whitespace().next().unwrap())
                        .join(&partition.label)
                        .join("holders"),
                );
                assert!(
                    holders.is_empty(),
                    "{} is claimed unmounted but is held by {holders:?}",
                    partition.label
                );
                assert!(!usage.mounts.contains_key(&partition.label));
                assert!(!usage.swaps.contains(&partition.label));
            }
        }
    }

    #[test]
    fn collects_every_physical_drive() {
        let nodes = Storage.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Storage");
        assert_eq!(nodes.len(), devices().len() + 1);

        // Geometry comes from sysfs and must be present with or without SMART.
        for node in nodes.iter().skip(1) {
            assert!(
                node.children.iter().any(|c| c.label == "Size"),
                "{} should report a size",
                node.label
            );
        }
    }

    #[test]
    fn says_why_health_is_missing() {
        // On an unprivileged run this is the permission message; the point is
        // that it is never silently absent.
        let sample = devices().first().map(|name| smart(name));
        let message = smart_availability(sample.as_ref());
        assert!(!message.is_empty());
        if !probe::available("smartctl") {
            assert!(message.contains("smartmontools"), "should name the package");
        }
    }

    #[test]
    fn each_drive_is_queried_once() {
        // smartctl talks to the hardware, so the summary line and the health
        // section must share one report rather than each asking for their own.
        let nodes = Storage.collect().expect("collection failed");
        assert_eq!(nodes.len(), devices().len() + 1);

        // A drive either has exactly one health section or none at all.
        for node in nodes.iter().skip(1) {
            let health_sections = node.children.iter().filter(|c| c.label == "Health").count();
            assert!(health_sections <= 1, "{} has duplicate health", node.label);
        }
    }
}
