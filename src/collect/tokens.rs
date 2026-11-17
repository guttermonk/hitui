//! Hardware security tokens and the firmware they report.
//!
//! Detected by what a device *does*, not by who made it. A USB device is a
//! token here if it declares a CCID interface — USB class 0x0b, the smart
//! card class — or a HID interface whose report descriptor opens with usage
//! page 0xF1D0, which the FIDO Alliance owns and nothing else uses. No vendor
//! or product id appears anywhere in this file, so a Nitrokey, a SoloKey, a
//! Feitian, a Titan and a YubiKey are all found by the same rule; on the
//! machine this was written against the rule also turns up a plain EMV
//! smartcard reader, which is the point rather than a false positive.
//!
//! fwupd does not see these at all — its device list on that machine has the
//! SSD, the ME, the TPM and the Secure Boot certificates, and no token — so
//! this reads USB sysfs directly and needs no daemon and no privilege.

use std::path::Path;

use crate::model::{Node, Value};
use crate::sysfs;

const USB: &str = "/sys/bus/usb/devices";
/// Usage Page (0xF1D0), as a report descriptor encodes it: item 0x06 is a
/// two-byte usage page, little-endian.
const FIDO_USAGE_PAGE: [u8; 3] = [0x06, 0xd0, 0xf1];
/// USB class 0x0b, "Smart Card" — what CCID readers and the smartcard half of
/// a security key declare.
const CCID_CLASS: &str = "0b";
const HID_CLASS: &str = "03";

/// The "Security tokens" section, or nothing when the machine has none.
pub fn section() -> Option<Node> {
    let mut node = Node::group("Security tokens", vec![]).from(USB);
    let mut found = 0;

    for device in sysfs::list_dir(USB) {
        // Interfaces are siblings of their device in this directory, named
        // with a colon; skip them, and skip the root hubs.
        let name = file_name(&device);
        if name.contains(':') || name.starts_with("usb") {
            continue;
        }

        let roles = roles(&device, &name);
        if roles.is_empty() {
            continue;
        }
        node.push(token(&device, &roles));
        found += 1;
    }
    (found > 0).then_some(node)
}

/// What makes this device a token, if anything does.
fn roles(device: &Path, name: &str) -> Vec<&'static str> {
    let mut roles = Vec::new();
    for interface in sysfs::list_dir(device) {
        // An interface of this device, not a nested hub or endpoint.
        if !file_name(&interface).starts_with(&format!("{name}:")) {
            continue;
        }
        match sysfs::read(interface.join("bInterfaceClass")).as_deref() {
            Some(CCID_CLASS) => roles.push("smart card (CCID)"),
            Some(HID_CLASS) if declares_fido(&interface) => roles.push("FIDO/CTAP authenticator"),
            _ => {}
        }
    }
    roles.sort_unstable();
    roles.dedup();
    roles
}

/// Whether a HID interface's report descriptor claims the FIDO usage page.
///
/// The descriptor is the device's own statement of what it speaks, which is
/// why this needs no table of product ids to keep up to date.
fn declares_fido(interface: &Path) -> bool {
    for entry in sysfs::list_dir(interface) {
        let descriptor = entry.join("report_descriptor");
        if let Ok(bytes) = std::fs::read(&descriptor)
            && bytes.starts_with(&FIDO_USAGE_PAGE)
        {
            return true;
        }
    }
    false
}

fn token(device: &Path, roles: &[&'static str]) -> Node {
    let product = sysfs::read(device.join("product")).filter(|p| !p.is_empty());
    let vendor = sysfs::read(device.join("manufacturer")).filter(|v| !v.is_empty());

    let label = match (&vendor, &product) {
        // Most products already name their maker, and "Yubico YubiKey 5" is
        // worse than "YubiKey 5".
        (Some(vendor), Some(product)) if !product.starts_with(vendor.as_str()) => {
            format!("{vendor} {product}")
        }
        (_, Some(product)) => product.clone(),
        (Some(vendor), None) => vendor.clone(),
        (None, None) => file_name(device),
    };

    let mut node = Node::group(label, vec![]);
    node.push(Node::field(
        "Provides",
        Value::List(roles.iter().map(|role| role.to_string()).collect()),
    ));
    node.push_opt(
        "Firmware",
        sysfs::read(device.join("bcdDevice"))
            .as_deref()
            .and_then(device_release),
    );
    node.push_opt(
        "USB ID",
        match (
            sysfs::read(device.join("idVendor")),
            sysfs::read(device.join("idProduct")),
        ) {
            (Some(vendor), Some(product)) => Some(format!("{vendor}:{product}")),
            _ => None,
        },
    );
    // Useful for telling two identical keys apart, and already on the USB
    // page, so leaving it out here would hide nothing.
    node.push_opt(
        "Serial",
        sysfs::read(device.join("serial")).filter(|s| !s.is_empty()),
    );
    node.push(Node::field("Port", file_name(device)));
    node
}

/// The USB device release number, which tokens conventionally use to report
/// their firmware version.
///
/// Binary-coded decimal in the form `0xJJMN` — major, minor, sub-minor — per
/// the USB specification, so a key reporting `0543` is running 5.4.3.
///
/// Reported as "firmware" because that is what it is on every token this was
/// tested against, but the field is a convention rather than a guarantee:
/// plenty of USB devices leave it at `0100` for life and mean nothing by it.
/// A device that reports exactly that is treated as having said nothing,
/// since a confident "firmware 1.0.0" is worse than a blank.
fn device_release(raw: &str) -> Option<String> {
    let value = u16::from_str_radix(raw.trim(), 16).ok()?;
    if value == 0x0100 || value == 0 {
        return None;
    }
    let high = (value >> 8) as u8;
    let low = (value & 0xff) as u8;
    // The high byte is two BCD digits; the low byte is one each.
    let major = (high >> 4) as u32 * 10 + (high & 0x0f) as u32;
    let minor = (low >> 4) & 0x0f;
    let sub = low & 0x0f;
    Some(format!("{major}.{minor}.{sub}"))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Exposed for the test, which builds a synthetic USB tree.
#[cfg(test)]
fn section_in(root: &Path) -> Option<Node> {
    let mut node = Node::group("Security tokens", vec![]).from(USB);
    let mut found = 0;
    for device in sysfs::list_dir(root) {
        let name = file_name(&device);
        if name.contains(':') || name.starts_with("usb") {
            continue;
        }
        let roles = roles(&device, &name);
        if roles.is_empty() {
            continue;
        }
        node.push(token(&device, &roles));
        found += 1;
    }
    (found > 0).then_some(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write(path: PathBuf, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture directory");
        }
        std::fs::write(path, contents).expect("writing the fixture");
    }

    fn fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("hitui-tokens-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("fixture root");
        root
    }

    /// A USB device with the given interface classes, and a FIDO report
    /// descriptor on the interface at `fido_on` if any.
    fn device(root: &Path, port: &str, classes: &[&str], fido_on: Option<usize>) -> PathBuf {
        let device = root.join(port);
        write(device.join("product"), b"Test Token\n");
        write(device.join("manufacturer"), b"Example\n");
        write(device.join("idVendor"), b"1234\n");
        write(device.join("idProduct"), b"5678\n");
        write(device.join("bcdDevice"), b"0543\n");
        for (index, class) in classes.iter().enumerate() {
            let interface = device.join(format!("{port}:1.{index}"));
            write(interface.join("bInterfaceClass"), class.as_bytes());
            if fido_on == Some(index) {
                write(
                    interface.join("0003:1234:5678.0001/report_descriptor"),
                    &[0x06, 0xd0, 0xf1, 0x09, 0x01],
                );
            }
        }
        device
    }

    #[test]
    fn a_fido_authenticator_is_found_by_its_usage_page() {
        // The report descriptor is the device's own statement of what it
        // speaks, which is why no table of product ids is needed.
        let root = fixture("fido");
        device(&root, "1-5", &["03"], Some(0));

        let section = section_in(&root).expect("a token was found");
        assert_eq!(section.children.len(), 1);
        let token = &section.children[0];
        assert_eq!(token.label, "Example Test Token");
        assert_eq!(
            field(token, "Provides").as_deref(),
            Some("FIDO/CTAP authenticator")
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_smartcard_reader_counts_even_with_no_fido_at_all() {
        // On the machine this was written against the rule turns up an EMV
        // reader alongside the security key. That is the rule working, not a
        // false positive: it is a device that holds keys and has firmware.
        let root = fixture("ccid");
        device(&root, "1-11", &["0b"], None);

        let section = section_in(&root).expect("a reader was found");
        assert_eq!(
            field(&section.children[0], "Provides").as_deref(),
            Some("smart card (CCID)")
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_key_doing_both_reports_both_once() {
        let root = fixture("both");
        // The real shape of a YubiKey: an OTP keyboard, a FIDO HID interface
        // and a CCID interface.
        device(&root, "1-5", &["03", "03", "0b"], Some(1));

        let section = section_in(&root).expect("a token was found");
        let provides = field(&section.children[0], "Provides").expect("roles");
        assert!(provides.contains("FIDO/CTAP authenticator"), "{provides}");
        assert!(provides.contains("smart card (CCID)"), "{provides}");
        // The keyboard interface is not a third role, and neither role is
        // listed twice.
        assert_eq!(provides.matches("CCID").count(), 1, "{provides}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_ordinary_device_is_not_a_token() {
        let root = fixture("ordinary");
        // A mouse: HID, but its report descriptor is Generic Desktop.
        let mouse = root.join("1-2");
        write(mouse.join("product"), b"Mouse\n");
        write(mouse.join("1-2:1.0/bInterfaceClass"), b"03");
        write(
            mouse.join("1-2:1.0/0003:0001:0002.0001/report_descriptor"),
            &[0x05, 0x01, 0x09, 0x06],
        );
        // And a mass storage device, which is not HID at all.
        let disk = root.join("1-3");
        write(disk.join("1-3:1.0/bInterfaceClass"), b"08");

        assert!(section_in(&root).is_none(), "nothing here is a token");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_device_release_is_binary_coded_decimal() {
        // 0x0543 is 5.4.3, not 5.67 and not 1347: the field is BCD in the
        // form JJ.M.N, and a YubiKey reporting 0543 is running firmware 5.4.3.
        assert_eq!(device_release("0543").as_deref(), Some("5.4.3"));
        assert_eq!(device_release("0100"), None);
        // The default nobody means anything by is treated as silence: a
        // confident "firmware 1.0.0" is worse than a blank.
        assert_eq!(device_release("0000"), None);
        assert_eq!(device_release("1234").as_deref(), Some("12.3.4"));
        assert_eq!(device_release("nonsense"), None);
    }

    #[test]
    fn the_real_machine_is_scanned_without_panicking() {
        // Whatever is plugged in, the walk must terminate and every token it
        // finds must say what makes it one.
        if let Some(section) = section() {
            assert_eq!(section.source.as_deref(), Some(USB));
            for token in &section.children {
                assert!(
                    field(token, "Provides").is_some(),
                    "{} says nothing about what it provides",
                    token.label
                );
            }
        }
    }

    fn field(node: &Node, label: &str) -> Option<String> {
        node.children
            .iter()
            .find(|child| child.label == label)
            .and_then(|child| child.value.as_ref())
            .map(|value| value.to_string())
    }
}
