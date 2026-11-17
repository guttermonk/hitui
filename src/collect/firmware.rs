//! Platform firmware: UEFI, Secure Boot, and the TPM.
//!
//! Secure Boot state is an EFI variable, not a sysfs attribute. efivarfs
//! prefixes every variable with a 4-byte attribute word, so the value is the
//! *fifth* byte — reading the first byte gives the attribute flags and a
//! confidently wrong answer.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::collect::{dmi, fwupd, tokens};
use crate::model::{Node, Value};
use crate::sysfs;

const EFI: &str = "/sys/firmware/efi";
/// EFI global variable namespace, from the UEFI specification.
const GLOBAL_GUID: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";

pub struct Firmware;

impl Collector for Firmware {
    fn id(&self) -> &'static str {
        "firmware"
    }

    fn title(&self) -> &'static str {
        "Firmware"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        // Updates come second: whether this firmware is current is the first
        // thing worth knowing after what it is.
        let mut nodes = vec![platform(), fwupd::section()];
        // Firmware that fwupd does not see. A GPU runs its own and nothing
        // reports it; security tokens are absent from fwupd's device list
        // entirely, so they are read from USB sysfs instead.
        nodes.extend(graphics());
        nodes.extend(tokens::section());
        nodes.push(secure_boot());
        nodes.push(tpm());
        Ok(nodes)
    }
}

fn platform() -> Node {
    let mut node = Node::group("Platform", vec![]);
    let uefi = std::path::Path::new(EFI).is_dir();

    node.push(Node::field(
        "Boot interface",
        if uefi { "UEFI" } else { "legacy BIOS" },
    ));
    if uefi {
        node.push_opt(
            "Firmware word size",
            sysfs::read(format!("{EFI}/fw_platform_size")).map(|bits| format!("{bits}-bit")),
        );
        // `fw_vendor` is deliberately not read: it holds the physical address
        // of the vendor string, not the string, so it renders as a useless
        // "0x6ef31698". The vendor name comes from DMI below.
        //
        // efivarfs has to be mounted for any variable to be readable, which is
        // the usual reason Secure Boot state comes back unknown.
        node.push(Node::field(
            "efivarfs",
            if std::path::Path::new(EFI).join("efivars").is_dir() {
                "mounted"
            } else {
                "not mounted — EFI variables unreadable"
            },
        ));
    }

    // The BIOS/firmware build, which is what a vendor asks for.
    node.push_opt("Vendor", dmi::field("bios_vendor"));
    node.push_opt("Version", dmi::field("bios_version"));
    node.push_opt("Release", dmi::field("bios_release"));
    node.push_opt("Date", dmi::field("bios_date"));
    node.push_opt("EC firmware", dmi::field("ec_firmware_release"));
    node
}

/// Firmware the graphics hardware runs.
///
/// A GPU is a computer with its own firmware: a video BIOS at minimum, and on
/// Intel parts the GuC and HuC microcontroller images the driver uploads at
/// initialisation. Neither appears in fwupd's device list, and the GPU page
/// reports what the card *is* rather than what it is running.
///
/// NVIDIA publishes the video BIOS version in `/proc`. i915 keeps its GuC,
/// HuC and DMC versions in debugfs, which is root-only, so that is stated
/// rather than left looking like a card with no firmware at all.
fn graphics() -> Option<Node> {
    let mut node = Node::group("Graphics firmware", vec![]);
    let mut found = 0;

    for gpu in sysfs::list_dir("/proc/driver/nvidia/gpus") {
        let Some(information) = sysfs::read(gpu.join("information")) else {
            continue;
        };
        let fields = sysfs::parse_colon_pairs(&information);
        let get = |key: &str| {
            fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
                .filter(|value| !value.is_empty())
        };

        let label = get("Model").unwrap_or_else(|| {
            gpu.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "NVIDIA GPU".to_string())
        });
        let mut card = Node::group(label, vec![]).from("/proc/driver/nvidia");
        card.push_opt("Video BIOS", get("Video BIOS"));
        card.push_opt("Bus", get("Bus Location"));
        node.push(card);
        found += 1;
    }

    // Intel's firmware versions are behind debugfs. Saying so beats a card
    // that looks like it runs none.
    for card in sysfs::list_dir("/sys/class/drm") {
        let driver = std::fs::canonicalize(card.join("device/driver"))
            .ok()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
            });
        if driver.as_deref() != Some("i915") && driver.as_deref() != Some("xe") {
            continue;
        }
        let name = card
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        // Connectors are also drm devices; only the cards themselves matter.
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let mut intel = Node::group(name, vec![]);
        intel.push(Node::field(
            "GuC and HuC",
            "versions are in debugfs — needs root to read",
        ));
        node.push(intel);
        found += 1;
    }

    (found > 0).then_some(node)
}

fn secure_boot() -> Node {
    let mut node = Node::group("Secure Boot", vec![]);

    match efi_bool("SecureBoot") {
        Some(true) => node.push(Node::field("Enabled", Value::Flag(true))),
        Some(false) => node.push(Node::field("Enabled", Value::Flag(false))),
        None => node.push(Node::field(
            "Enabled",
            "unknown — no SecureBoot variable (not a UEFI boot, or efivarfs absent)",
        )),
    }

    // Setup mode means the platform is accepting new keys, so Secure Boot is
    // not actually protecting anything even if it reports enabled.
    if let Some(setup) = efi_bool("SetupMode") {
        node.push(Node::field(
            "Setup mode",
            if setup {
                "yes — the platform is accepting new keys"
            } else {
                "no — keys are enrolled"
            },
        ));
    }
    node.push_opt("Audit mode", efi_bool("AuditMode").map(Value::Flag));
    node.push_opt("Deployed mode", efi_bool("DeployedMode").map(Value::Flag));
    node
}

/// Read a boolean EFI variable from efivarfs.
///
/// The file layout is a 4-byte little-endian attribute mask followed by the
/// data, so a one-byte boolean lives at offset 4.
fn efi_bool(name: &str) -> Option<bool> {
    let path = format!("{EFI}/efivars/{name}-{GLOBAL_GUID}");
    let bytes = std::fs::read(path).ok()?;
    bytes.get(4).map(|value| *value != 0)
}

fn tpm() -> Node {
    let mut node = Node::group("TPM", vec![]);

    let Some(device) = sysfs::list_dir("/sys/class/tpm")
        .into_iter()
        .find(|path| path.file_name().is_some())
    else {
        node.push(Node::field(
            "Present",
            "no — no TPM device exposed by the kernel",
        ));
        return node;
    };

    let name = device
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    node.push(Node::field("Device", name));
    node.push_opt(
        "Specification",
        sysfs::read(device.join("tpm_version_major")).map(|major| match major.trim() {
            // The major version is the thing software actually branches on:
            // 2.0 and 1.2 have incompatible command sets.
            "2" => "TPM 2.0".to_string(),
            "1" => "TPM 1.2".to_string(),
            other => format!("major version {other}"),
        }),
    );
    node.push_opt(
        "Description",
        sysfs::read(device.join("description")).filter(|d| !d.trim().is_empty()),
    );
    // Which hashes the TPM supports determines what can be sealed against it.
    node.push_opt(
        "Active PCR banks",
        sysfs::read(device.join("pcr-sha256/0"))
            .map(|_| "SHA-256".to_string())
            .or_else(|| sysfs::read(device.join("pcr-sha1/0")).map(|_| "SHA-1".to_string())),
    );
    node
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn efi_variables_are_read_past_the_attribute_word() {
        // The bug this guards: efivarfs prefixes a 4-byte attribute mask, so
        // reading byte 0 returns the flags. On this machine SecureBoot reads
        // "6 0 0 0 0" — attributes 6, value 0. Byte 0 would say "enabled".
        if let Some(enabled) = efi_bool("SecureBoot") {
            let path = format!("{EFI}/efivars/SecureBoot-{GLOBAL_GUID}");
            let bytes = std::fs::read(&path).expect("variable was just read");
            assert!(bytes.len() >= 5, "expected attributes plus a value");
            assert_eq!(
                enabled,
                bytes[4] != 0,
                "the value byte is at offset 4, not 0"
            );
            // The attribute word is non-zero for every real variable, which is
            // exactly why reading byte 0 would be wrong.
            assert_ne!(bytes[0], 0, "attribute flags should be set");
        }
    }

    #[test]
    fn a_missing_variable_is_unknown_rather_than_false() {
        // Reporting Secure Boot as disabled when the variable is simply
        // unreadable would be a false statement about the machine.
        assert!(efi_bool("HituiNotARealVariable").is_none());
    }

    #[test]
    fn collects_every_section() {
        let nodes = Firmware.collect().expect("collection failed");
        let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        // Two of the sections depend on the hardware: a machine with no
        // discrete GPU and nothing plugged into it has no graphics firmware
        // to report and no tokens to find. So the always-present ones are
        // asserted by name and order, and the optional ones are allowed
        // between them rather than pinned into a list that only matches the
        // machine this was written on.
        assert_eq!(labels.first(), Some(&"Platform"));
        assert_eq!(labels.last(), Some(&"TPM"));
        for required in ["Platform", "Firmware updates", "Secure Boot", "TPM"] {
            assert!(labels.contains(&required), "{required} missing: {labels:?}");
        }
        let position = |label: &str| labels.iter().position(|l| *l == label);
        assert!(position("Firmware updates") < position("Secure Boot"));
        for label in &labels {
            assert!(
                [
                    "Platform",
                    "Firmware updates",
                    "Graphics firmware",
                    "Security tokens",
                    "Secure Boot",
                    "TPM",
                ]
                .contains(label),
                "unexpected section {label:?}"
            );
        }

        for node in &nodes {
            assert!(!node.children.is_empty(), "{} is empty", node.label);
        }
        // Secure Boot always states something, even when it cannot be read.
        // Found by label rather than by index: a section inserted ahead of it
        // should not be able to make this assertion test the wrong one.
        let secure_boot = nodes
            .iter()
            .find(|node| node.label == "Secure Boot")
            .expect("Secure Boot section");
        assert!(
            secure_boot.children.iter().any(|c| c.label == "Enabled"),
            "Secure Boot state must always be reported"
        );
    }
}
