//! Firmware versions and pending updates, as fwupd sees them.
//!
//! Answers what a version number on its own cannot: is this firmware current,
//! and if not, how far behind is it. Three facts come from here that sysfs has
//! no concept of — when the installed build was released, what the newest build
//! is called, and how many releases separate the two.
//!
//! Everything is read through `fwupdmgr --json`, which is a supported
//! interface; the human-readable output is not. Nothing here can install
//! anything: these are the four read-only subcommands, and `fwupdmgr refresh`
//! is deliberately never run, because it downloads metadata over the network
//! and opening a page must not do that.
//!
//! The dates are vendor metadata, not measurements of this machine. A vendor
//! that stamps a batch of releases with one date — Lenovo does — shows the same
//! date for several versions, which is what the metadata says rather than
//! something to smooth over. For the same reason the metadata's own age is
//! reported: every "up to date" here is only as current as the last refresh.

use std::collections::BTreeMap;

use serde_json::Value as Json;

use crate::model::{Node, Value, format_date, format_duration, unix_now};
use crate::probe;

/// Cap on per-device release queries, so a machine with a dock full of
/// updatable peripherals cannot turn opening this page into a stall.
const MAX_DEVICES: usize = 16;

/// Everything read from fwupd in one pass, so that shaping it is pure.
#[derive(Default)]
struct Report {
    devices: Json,
    /// `None` if the query failed — distinct from "fwupd says nothing is
    /// pending", which is an empty device list.
    pending: Option<Json>,
    remotes: Option<Json>,
    /// Releases per device id, only for the devices actually listed.
    releases: BTreeMap<String, Json>,
}

/// The "Firmware updates" section, or a note saying why there isn't one.
pub fn section() -> Node {
    if !probe::available("fwupdmgr") {
        return note("unavailable — install fwupd for release dates and update checks");
    }
    let Some(devices) = query(&["get-devices"]) else {
        // fwupdmgr is installed but produced nothing usable. Almost always the
        // daemon: every subcommand used here goes through it.
        return note("fwupd is installed but did not answer — is fwupd.service running?");
    };

    let pending = query(&["get-updates"]);
    let mut report = Report {
        devices,
        pending,
        remotes: query(&["get-remotes"]),
        releases: BTreeMap::new(),
    };

    // One query per listed device, and it stops at the first one that does not
    // answer. probe::run kills a hung tool after three seconds, so without
    // this a wedged daemon would cost three seconds per device before the page
    // drew anything — and a machine with a dock full of updatable peripherals
    // has plenty of devices. Losing some release dates beats a page that
    // takes a minute.
    for id in listed(&report)
        .iter()
        .filter_map(|device| string(device, "DeviceId"))
        .collect::<Vec<_>>()
    {
        match query(&["get-releases", &id]) {
            Some(releases) => {
                report.releases.insert(id, releases);
            }
            None => break,
        }
    }
    build(&report)
}

fn note(message: &str) -> Node {
    let mut node = Node::group("Firmware updates", vec![]).from("fwupdmgr");
    node.push(Node::field("Status", message));
    node
}

/// Shape a gathered report into the page section.
fn build(report: &Report) -> Node {
    let mut node = Node::group("Firmware updates", vec![]).from("fwupdmgr");

    if report.pending.is_none() {
        node.push(Node::field(
            "Updates",
            "could not be checked — fwupdmgr get-updates failed",
        ));
    }
    node.push_opt(
        "Metadata",
        report
            .remotes
            .as_ref()
            .and_then(newest_metadata)
            .map(describe_age),
    );

    let devices = listed(report);
    if devices.is_empty() {
        // Every machine fwupd supports has a System Firmware device, so an
        // empty list means no metadata rather than no hardware.
        node.push(Node::field(
            "Status",
            "no device has firmware metadata — run `fwupdmgr refresh`",
        ));
        return node;
    }

    for device in &devices {
        node.push(device_node(device, report));
    }
    if interesting(report).count() > MAX_DEVICES {
        node.push(Node::field(
            "Note",
            format!("showing the first {MAX_DEVICES} of this machine's updatable devices"),
        ));
    }
    node
}

/// One device: what is installed, when it was released, and what is newer.
fn device_node(device: &Json, report: &Report) -> Node {
    let name = string(device, "Name").unwrap_or_else(|| "Unknown device".to_string());
    let mut node = Node::group(name, vec![]);

    let installed = string(device, "Version");
    node.push_opt("Installed", installed.clone());

    // A device does not carry its own release date: it has to be looked up in
    // the metadata by matching the version string. No match means the running
    // build is not in the metadata at all, which is reported by saying nothing
    // rather than by picking a nearby date.
    let releases = string(device, "DeviceId").and_then(|id| report.releases.get(&id));
    if let (Some(installed), Some(releases)) = (&installed, releases) {
        node.push_opt(
            "Released",
            installed_date(releases, installed).map(format_date),
        );
    }

    match upgrades(device, report.pending.as_ref()) {
        Some(releases) if !releases.is_empty() => {
            // fwupd lists upgrades newest first.
            let newest = releases[0];
            node.push_opt("Update", string(newest, "Version"));
            node.push_opt("Update released", created(newest).map(format_date));
            // What the vendor calls it. Summary is the descriptive one
            // ("Lenovo ThinkPad P51 System Firmware"); Name is often just the
            // model, which the page says already.
            node.push_opt(
                "Update name",
                string(newest, "Summary").or_else(|| string(newest, "Name")),
            );
            node.push(Node::field(
                "Releases ahead",
                Value::Int(releases.len() as i64),
            ));
        }
        // An empty list is fwupd saying it looked and found nothing.
        Some(_) => node.push(Node::field("Up to date", Value::Flag(true))),
        None => {}
    }
    node
}

/// The release date of the installed version, from the metadata.
fn installed_date(releases: &Json, installed: &str) -> Option<i64> {
    releases_of(releases)
        .find(|release| string(release, "Version").as_deref() == Some(installed))
        .and_then(created)
}

/// The devices worth a row, capped.
fn listed(report: &Report) -> Vec<&Json> {
    interesting(report).take(MAX_DEVICES).collect()
}

/// Which devices earn a row.
///
/// fwupd reports every Secure Boot certificate as an updatable "device", which
/// would bury the handful that matter. A device earns its place by having an
/// update pending, or by fwupd holding metadata for it — `supported` — which is
/// also the condition for being able to say anything about its releases.
fn interesting(report: &Report) -> impl Iterator<Item = &Json> {
    devices_of(&report.devices).filter(move |device| {
        if string(device, "Version").is_none() {
            return false;
        }
        upgrades(device, report.pending.as_ref()).is_some_and(|releases| !releases.is_empty())
            || flags(device).any(|flag| flag == "supported")
    })
}

/// The pending upgrade releases for a device, newest first.
///
/// `None` means fwupd was never asked or could not say; an empty slice means it
/// was asked and there is nothing — the two must not collapse, or a failed
/// query would render as "up to date".
fn upgrades<'a>(device: &Json, pending: Option<&'a Json>) -> Option<Vec<&'a Json>> {
    let id = string(device, "DeviceId")?;
    let devices = pending?.get("Devices")?.as_array()?;
    let entry = devices
        .iter()
        .find(|candidate| string(candidate, "DeviceId").as_deref() == Some(id.as_str()));
    match entry {
        Some(entry) => Some(releases_of(entry).collect()),
        None => Some(Vec::new()),
    }
}

/// When the newest enabled remote last fetched its metadata.
fn newest_metadata(remotes: &Json) -> Option<i64> {
    remotes
        .get("Remotes")?
        .as_array()?
        .iter()
        .filter(|remote| remote.get("Enabled").and_then(Json::as_bool) == Some(true))
        // Only a download remote has metadata that goes stale; a directory
        // remote is local files.
        .filter(|remote| string(remote, "Kind").as_deref() == Some("download"))
        .filter_map(|remote| remote.get("Mtime").and_then(Json::as_i64))
        // A remote that has never been fetched reports -1.
        .filter(|mtime| *mtime > 0)
        .max()
}

fn describe_age(mtime: i64) -> String {
    match unix_now() {
        Some(now) => format!(
            "{} ({} old)",
            format_date(mtime),
            format_duration((now - mtime).max(0) as u64)
        ),
        None => format_date(mtime),
    }
}

fn devices_of(json: &Json) -> impl Iterator<Item = &Json> {
    array(json, "Devices")
}

fn releases_of(json: &Json) -> impl Iterator<Item = &Json> {
    array(json, "Releases")
}

fn array<'a>(json: &'a Json, key: &str) -> impl Iterator<Item = &'a Json> {
    json.get(key)
        .and_then(Json::as_array)
        .map(|items| items.iter())
        .unwrap_or_default()
}

fn flags(device: &Json) -> impl Iterator<Item = &str> {
    array(device, "Flags").filter_map(Json::as_str)
}

fn string(json: &Json, key: &str) -> Option<String> {
    json.get(key)
        .and_then(Json::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

/// A release's publication date, as a Unix timestamp.
fn created(release: &Json) -> Option<i64> {
    release
        .get("Created")
        .and_then(Json::as_i64)
        // fwupd uses 0 for "no date in the metadata", which would otherwise
        // render as 1970.
        .filter(|created| *created > 0)
}

/// Run `fwupdmgr <args> --json` and parse the result.
fn query(args: &[&str]) -> Option<Json> {
    let mut args = args.to_vec();
    args.push("--json");
    parse(&probe::run("fwupdmgr", &args)?)
}

/// Parse one `--json` response.
///
/// With `--json`, fwupdmgr reports failure as a JSON object carrying an
/// `Error` key, on stdout, often while still exiting zero. The shape has to be
/// checked, because the alternative is treating an error message as a device
/// list and reporting a machine with no firmware.
fn parse(output: &str) -> Option<Json> {
    let json: Json = serde_json::from_str(output).ok()?;
    if json.get("Error").is_some() {
        return None;
    }
    Some(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from this machine's real `fwupdmgr get-devices --json`.
    const DEVICES: &str = r#"{
      "Devices": [
        {
          "Name": "System Firmware",
          "DeviceId": "2eaa",
          "Flags": ["internal", "updatable", "supported", "needs-reboot"],
          "Version": "0.1.60"
        },
        {
          "Name": "MZVLB512HAJQ-000L7",
          "DeviceId": "c829",
          "Flags": ["internal", "updatable", "supported"],
          "Version": "5L2QEXA7"
        },
        {
          "Name": "KEK CA",
          "DeviceId": "b7a1",
          "Flags": ["internal", "updatable", "signed-payload"],
          "Version": "2011"
        },
        {
          "Name": "GPIO controller",
          "DeviceId": "3f75",
          "Flags": ["can-emulation-tag"]
        }
      ]
    }"#;

    const UPDATES: &str = r#"{
      "Devices": [
        {
          "Name": "System Firmware",
          "DeviceId": "2eaa",
          "Version": "0.1.60",
          "Releases": [
            {"Version": "0.1.65", "Created": 1669680000, "Name": "ThinkPad P51",
             "Summary": "Lenovo ThinkPad P51 System Firmware"},
            {"Version": "0.1.64", "Created": 1669680000, "Name": "ThinkPad P51"},
            {"Version": "0.1.63", "Created": 1669680000, "Name": "ThinkPad P51"},
            {"Version": "0.1.62", "Created": 1669680000, "Name": "ThinkPad P51"}
          ]
        }
      ]
    }"#;

    const RELEASES: &str = r#"{
      "Releases": [
        {"Version": "0.1.65", "Created": 1669680000},
        {"Version": "0.1.60", "Created": 1669680000},
        {"Version": "0.1.59", "Created": 1658016000},
        {"Version": "0.1.52", "Created": 1592438400}
      ]
    }"#;

    fn json(text: &str) -> Json {
        serde_json::from_str(text).expect("fixture is valid JSON")
    }

    fn report() -> Report {
        let mut releases = BTreeMap::new();
        releases.insert("2eaa".to_string(), json(RELEASES));
        releases.insert(
            "c829".to_string(),
            json(r#"{"Releases": [{"Version": "5L2QEXA7", "Created": 1467946800}]}"#),
        );
        Report {
            devices: json(DEVICES),
            pending: Some(json(UPDATES)),
            remotes: None,
            releases,
        }
    }

    /// A device's fields as label/value pairs.
    fn fields(node: &Node) -> Vec<(String, String)> {
        node.children
            .iter()
            .map(|child| {
                (
                    child.label.clone(),
                    child
                        .value
                        .as_ref()
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                )
            })
            .collect()
    }

    fn device(section: &Node, name: &str) -> Node {
        section
            .children
            .iter()
            .find(|child| child.label == name)
            .unwrap_or_else(|| panic!("{name} is not listed"))
            .clone()
    }

    #[test]
    fn an_out_of_date_device_says_how_far_behind_it_is() {
        let section = build(&report());
        assert_eq!(
            fields(&device(&section, "System Firmware")),
            vec![
                ("Installed".to_string(), "0.1.60".to_string()),
                ("Released".to_string(), "2022-11-29".to_string()),
                ("Update".to_string(), "0.1.65".to_string()),
                ("Update released".to_string(), "2022-11-29".to_string()),
                (
                    "Update name".to_string(),
                    "Lenovo ThinkPad P51 System Firmware".to_string()
                ),
                ("Releases ahead".to_string(), "4".to_string()),
            ]
        );
    }

    #[test]
    fn a_current_device_is_marked_up_to_date() {
        let section = build(&report());
        assert_eq!(
            fields(&device(&section, "MZVLB512HAJQ-000L7")),
            vec![
                ("Installed".to_string(), "5L2QEXA7".to_string()),
                ("Released".to_string(), "2016-07-08".to_string()),
                ("Up to date".to_string(), "yes".to_string()),
            ]
        );
    }

    #[test]
    fn a_failed_update_check_is_not_reported_as_up_to_date() {
        // The bug this guards: collapsing "could not ask" into "nothing
        // pending" would tell you your firmware is current when nobody
        // checked.
        let mut report = report();
        report.pending = None;
        let section = build(&report);

        let firmware = device(&section, "System Firmware");
        let labels: Vec<&str> = firmware.children.iter().map(|c| c.label.as_str()).collect();
        assert!(!labels.contains(&"Up to date"), "{labels:?}");
        assert!(!labels.contains(&"Update"), "{labels:?}");
        // And the section says so out loud.
        assert!(
            section.children.iter().any(|c| c.label == "Updates"
                && c.value
                    .as_ref()
                    .is_some_and(|v| v.to_string().contains("could not be checked"))),
            "the failure should be stated"
        );
    }

    #[test]
    fn certificate_devices_without_metadata_are_left_out() {
        // fwupd calls every Secure Boot certificate an updatable device. With
        // no metadata and no pending update there is nothing to say about
        // them, and they outnumber the devices that matter.
        let section = build(&report());
        let listed: Vec<&str> = section
            .children
            .iter()
            .map(|child| child.label.as_str())
            .collect();
        assert!(!listed.contains(&"KEK CA"), "{listed:?}");
        // Nor a device with no version at all.
        assert!(!listed.contains(&"GPIO controller"), "{listed:?}");
    }

    #[test]
    fn an_installed_version_absent_from_the_metadata_has_no_date() {
        // UEFI CA 2011 is the real case: the running version predates the
        // metadata, so there is no release to date it from. Reporting the
        // nearest release would be inventing a fact.
        assert_eq!(installed_date(&json(RELEASES), "0.1.60"), Some(1669680000));
        assert_eq!(installed_date(&json(RELEASES), "2011"), None);
    }

    #[test]
    fn a_release_with_no_date_is_not_dated_to_1970() {
        assert_eq!(created(&json(r#"{"Created": 0}"#)), None);
        assert_eq!(created(&json("{}")), None);
        assert_eq!(
            created(&json(r#"{"Created": 1669680000}"#)),
            Some(1669680000)
        );
    }

    #[test]
    fn an_error_payload_is_not_mistaken_for_data() {
        // fwupdmgr --json reports failure as JSON on stdout, so a parse that
        // only checked for valid JSON would read an error as an empty machine.
        assert!(
            parse(r#"{"Error": {"Code": 8, "Message": "failed to find x"}}"#).is_none(),
            "an Error payload is not data"
        );
        assert!(parse("not json").is_none());
        assert!(
            parse(r#"{"Devices": []}"#).is_some(),
            "empty is still an answer"
        );
    }

    #[test]
    fn metadata_age_uses_the_newest_fetched_remote() {
        let remotes = json(
            r#"{"Remotes": [
                {"Id": "lvfs-testing", "Enabled": false, "Kind": "download", "Mtime": -1},
                {"Id": "vendor", "Enabled": true, "Kind": "directory", "Mtime": 1700000000},
                {"Id": "lvfs", "Enabled": true, "Kind": "download", "Mtime": 1794797269}
            ]}"#,
        );
        assert_eq!(newest_metadata(&remotes), Some(1794797269));

        // A remote that has never been refreshed reports -1, which must not
        // become a date in 1969.
        let never = json(r#"{"Remotes": [{"Enabled": true, "Kind": "download", "Mtime": -1}]}"#);
        assert_eq!(newest_metadata(&never), None);
    }

    #[test]
    fn no_metadata_at_all_says_to_refresh() {
        let mut report = report();
        report.devices = json(r#"{"Devices": []}"#);
        report.pending = Some(json(r#"{"Devices": []}"#));
        let section = build(&report);
        let text: String = fields(&section)
            .iter()
            .map(|(label, value)| format!("{label} {value}"))
            .collect();
        assert!(text.contains("fwupdmgr refresh"), "{text}");
    }

    #[test]
    fn the_section_names_where_it_came_from() {
        // A number read from the kernel and a number scraped from a tool are
        // not the same kind of fact.
        assert_eq!(build(&report()).source.as_deref(), Some("fwupdmgr"));
        assert_eq!(note("whatever").source.as_deref(), Some("fwupdmgr"));
    }
}
