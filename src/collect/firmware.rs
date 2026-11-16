//! Platform firmware: UEFI, Secure Boot, and the TPM.
//!
//! Secure Boot state is an EFI variable, not a sysfs attribute. efivarfs
//! prefixes every variable with a 4-byte attribute word, so the value is the
//! *fifth* byte — reading the first byte gives the attribute flags and a
//! confidently wrong answer.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::collect::{dmi, fwupd};
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
        let mut nodes = vec![platform(), fwupd::section(), secure_boot()];
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
        assert_eq!(
            labels,
            vec!["Platform", "Firmware updates", "Secure Boot", "TPM"]
        );

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
