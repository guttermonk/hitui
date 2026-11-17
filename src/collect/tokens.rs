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

use std::path::{Path, PathBuf};

use crate::model::{Node, Value};
use crate::probe;
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
        node.push(token(&device, &roles, yubico_count(Path::new(USB))));
        found += 1;
    }
    (found > 0).then_some(node)
}

/// How many Yubico devices are plugged in, which decides whether `ykman` can
/// be asked about one without a serial.
fn yubico_count(root: &Path) -> usize {
    sysfs::list_dir(root)
        .iter()
        .filter(|device| {
            let name = file_name(device);
            !name.contains(':')
                && !name.starts_with("usb")
                && sysfs::read(device.join("idVendor")).as_deref() == Some(YUBICO)
        })
        .count()
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

fn token(device: &Path, roles: &[&'static str], keys: usize) -> Node {
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
    for detail in yubikey(device, keys) {
        node.push(detail);
    }
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

// ─── Vendor detail ───────────────────────────────────────────────────────────

/// Yubico's USB vendor id.
///
/// The only vendor id in this module, and it gates a vendor *tool* rather than
/// the detection above: reading a YubiKey's applets means speaking Yubico's
/// protocols, and `ykman` is the implementation of them. A Nitrokey is still
/// found and still reports its firmware; it just gets no applet breakdown,
/// because nothing here can ask it for one.
const YUBICO: &str = "1050";
/// Where pcscd puts its socket. Its absence is the reason three of the four
/// slot-bearing applets cannot be read, and saying which is more use than a
/// blank row.
const PCSCD_SOCKET: &str = "/run/pcscd/pcscd.comm";

/// Applet and slot detail for a YubiKey, via `ykman`.
///
/// `keys` is how many Yubico devices are plugged in: with more than one,
/// ykman needs a `--device <serial>` and nothing here can map a serial back to
/// a USB port, so the detail is skipped rather than guessed at.
fn yubikey(device: &Path, keys: usize) -> Vec<Node> {
    if sysfs::read(device.join("idVendor")).as_deref() != Some(YUBICO) {
        return Vec::new();
    }
    if keys > 1 {
        return vec![Node::field(
            "Applet detail",
            "skipped — more than one YubiKey is plugged in, and ykman needs a serial to \
             choose between them",
        )];
    }
    if !probe::available("ykman") {
        return vec![Node::field(
            "Applet detail",
            "unavailable — install yubikey-manager (ykman) for applet and slot state",
        )];
    }

    let mut out = Vec::new();
    if let Some(info) = probe::run("ykman", &["info"]) {
        let fields = sysfs::parse_colon_pairs(&info);
        let get = |key: &str| {
            fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
                .filter(|value| !value.is_empty())
        };
        // The model, which the USB product string does not give: it says
        // "YubiKey OTP+FIDO+CCID", the enabled interfaces, not "5 Nano".
        let mut node = Node::group("YubiKey", vec![]).from("ykman");
        node.push_opt("Model", get("Device type"));
        node.push_opt("Form factor", get("Form factor"));
        node.push_opt("Serial", get("Serial number"));
        node.push_opt(
            "USB interfaces",
            get("Enabled USB interfaces")
                .map(|v| Value::List(v.split(',').map(|part| part.trim().to_string()).collect())),
        );

        // Which applications are turned on at all. This is the coarse answer
        // to "what is available": a disabled applet has no slots to speak of.
        let applications = applications(&info);
        if !applications.children.is_empty() {
            node.push(applications);
        }
        out.push(node);
    }

    if let Some(fido) = probe::run("ykman", &["fido", "info"]) {
        out.push(fido2(&fido));
    }
    out.push(slots(device));
    out
}

/// The `Applications` block of `ykman info`: a name and a state per line,
/// after a bare "Applications" heading.
fn applications(info: &str) -> Node {
    let mut node = Node::group("Applications", vec![]);
    let mut inside = false;
    for line in info.lines() {
        let line = line.trim_end();
        if line.trim() == "Applications" {
            inside = true;
            continue;
        }
        if !inside || line.trim().is_empty() {
            continue;
        }
        // Name and state are column-aligned with tabs or runs of spaces, and
        // the name itself contains spaces ("Yubico OTP", "YubiHSM Auth"), so
        // the split is on the gap rather than on whitespace.
        let Some((name, state)) = split_columns(line) else {
            continue;
        };
        node.push(Node::field(name, state.to_ascii_lowercase()));
    }
    node
}

/// Split a column-aligned line on its gap: two or more spaces, or a tab.
fn split_columns(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    for index in 0..bytes.len() {
        let gap =
            bytes[index] == b'\t' || (bytes[index] == b' ' && bytes.get(index + 1) == Some(&b' '));
        if gap {
            let name = line[..index].trim();
            let state = line[index..].trim();
            if !name.is_empty() && !state.is_empty() {
                return Some((name, state));
            }
        }
    }
    None
}

/// FIDO2 state, which has no numbered slots to report.
///
/// Worth being explicit about, because "slot" is the word people reach for
/// here and FIDO2 does not have them: a credential is discoverable or it is
/// not, and the device has a capacity rather than an array of positions. What
/// it does have is a PIN, and whether one is set is the fact that matters —
/// a key with no PIN will hand over a resident credential to anyone holding
/// it.
///
/// Listing the credentials themselves needs the PIN, so it is not attempted:
/// a page that reports what a machine *is* must never ask for a secret.
fn fido2(info: &str) -> Node {
    let fields = sysfs::parse_colon_pairs(info);
    let get = |key: &str| {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty())
    };

    let mut node = Node::group("FIDO2", vec![]).from("ykman");
    node.push(Node::field(
        "PIN",
        match get("PIN") {
            // ykman phrases this as "8 attempt(s) remaining" when a PIN is
            // set and "not set" when it is not.
            Some(state) if state.contains("not set") => {
                "not set — a resident credential on this key is usable by anyone holding it"
                    .to_string()
            }
            Some(state) => format!("set, {state}"),
            None => "unknown".to_string(),
        },
    ));
    node.push_opt(
        "Minimum PIN length",
        get("Minimum PIN length").map(|n| format!("{n} characters")),
    );
    node.push_opt("AAGUID", get("AAGUID"));
    node.push(Node::field(
        "Credentials",
        "not listed — the device requires its PIN to enumerate them",
    ));
    node
}

/// The applets that do have numbered slots, and what each needs to be read.
///
/// Every one of these is behind an interface that an ordinary user does not
/// get by default, so the common case is that none can be read. Naming the
/// specific obstacle per applet is the whole value of the section in that
/// state: "needs pcscd" is actionable and a blank row is not.
fn slots(device: &Path) -> Node {
    let mut node = Node::group("Slots", vec![]);

    // Yubico OTP: two slots, short press and long press, spoken over HID on
    // the interface whose hidraw node gets no uaccess tag without Yubico's
    // udev rules. The FIDO interface gets one from systemd, which is why that
    // applet reads and this one does not.
    //
    // Checked by looking at the node rather than by trying and failing: the
    // attempt costs 0.4 s of someone's page load to learn what a permission
    // bit already said, and the node's own name is a better answer than
    // "unreadable" on its own.
    node.push(Node::field(
        "Yubico OTP",
        match otp_hidraw(device) {
            Some(node) if !readable(&node) => format!(
                "2 slots — unreadable: {} is not readable by this user; Yubico's udev rules \
                 grant it",
                node.display()
            ),
            _ => match probe::run("ykman", &["otp", "info"]) {
                Some(output) => otp_slots(&output),
                None => "2 slots — unreadable: ykman could not reach the OTP applet".to_string(),
            },
        },
    ));

    // PIV, OATH and OpenPGP all speak CCID, which goes through pcscd.
    let smartcard = if Path::new(PCSCD_SOCKET).exists() {
        None
    } else {
        Some("unreadable: pcscd is not running")
    };
    for (applet, arguments, count) in [
        ("PIV", ["piv", "info"], "4 slots (9a, 9c, 9d, 9e)"),
        ("OATH", ["oath", "info"], "credential slots"),
        ("OpenPGP", ["openpgp", "info"], "3 key slots"),
    ] {
        node.push(Node::field(
            applet,
            match smartcard {
                Some(reason) => format!("{count} — {reason}"),
                None => match probe::run("ykman", &arguments) {
                    Some(output) => populated(&output, count),
                    None => format!("{count} — unreadable: ykman could not reach the applet"),
                },
            },
        ));
    }
    node
}

/// The hidraw node of the OTP interface, if the device has one.
///
/// The OTP applet is a HID keyboard — class 3, boot subclass 1 — which is how
/// it types a one-time password into whatever has focus. The FIDO interface on
/// the same device is class 3 subclass 0, so the subclass is what tells them
/// apart.
fn otp_hidraw(device: &Path) -> Option<PathBuf> {
    let name = file_name(device);
    for interface in sysfs::list_dir(device) {
        if !file_name(&interface).starts_with(&format!("{name}:")) {
            continue;
        }
        if sysfs::read(interface.join("bInterfaceClass")).as_deref() != Some(HID_CLASS)
            || sysfs::read(interface.join("bInterfaceSubClass")).as_deref() != Some("01")
        {
            continue;
        }
        for entry in sysfs::list_dir(&interface) {
            // One hidraw node per HID interface, so the first is the only.
            if let Some(hidraw) = sysfs::list_dir(entry.join("hidraw")).first() {
                return Some(Path::new("/dev").join(file_name(hidraw)));
            }
        }
    }
    None
}

fn readable(node: &Path) -> bool {
    std::fs::File::open(node).is_ok()
}

/// `ykman otp info` reports one line per slot.
fn otp_slots(output: &str) -> String {
    let slots: Vec<String> = output
        .lines()
        .filter_map(split_columns_colon)
        .filter(|(name, _)| name.starts_with("Slot"))
        .map(|(name, state)| format!("{name} {state}"))
        .collect();
    if slots.is_empty() {
        return "2 slots — ykman reported no slot state".to_string();
    }
    slots.join(", ")
}

/// Which slots an applet's `info` output mentions as holding something.
///
/// Deliberately shallow: it reports the slot identifiers the output names and
/// nothing about their contents. The formats differ between ykman releases,
/// and a parser that finds nothing says so, where one that guessed at fields
/// would report a key that is not there.
fn populated(output: &str, count: &str) -> String {
    let named: Vec<String> = output
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_suffix(':').or(Some(line)))
        .filter(|line| line.starts_with("Slot "))
        .map(|line| line.trim_end_matches(':').to_string())
        .collect();
    if named.is_empty() {
        return format!("{count} — all empty");
    }
    format!("in use: {}", named.join(", "))
}

fn split_columns_colon(line: &str) -> Option<(&str, &str)> {
    let (name, value) = line.split_once(':')?;
    Some((name.trim(), value.trim()))
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
        node.push(token(&device, &roles, yubico_count(root)));
        found += 1;
    }
    (found > 0).then_some(node)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Verbatim from `ykman info` on the key this was written against.
    const YKMAN_INFO: &str = "Device type: YubiKey 5 Nano\n\
         Serial number: 13166271\n\
         Firmware version: 5.4.3\n\
         Form factor: Nano (USB-A)\n\
         Enabled USB interfaces: OTP, FIDO, CCID\n\
         \n\
         Applications\n\
         Yubico OTP  \tEnabled\n\
         FIDO U2F    \tEnabled\n\
         FIDO2       \tEnabled\n\
         OATH        \tEnabled\n\
         PIV         \tEnabled\n\
         OpenPGP     \tDisabled\n\
         YubiHSM Auth\tEnabled\n";

    /// Verbatim from `ykman fido info`.
    const YKMAN_FIDO: &str = "AAGUID:             ee882879-721c-4913-9775-3dfcce97072a\n\
         PIN:                8 attempt(s) remaining\n\
         Minimum PIN length: 4\n";

    #[test]
    fn applications_are_read_with_their_state() {
        // The coarse answer to "what is available": a disabled applet has no
        // slots to speak of.
        let node = applications(YKMAN_INFO);
        assert_eq!(node.children.len(), 7, "all seven applets");
        assert_eq!(field(&node, "Yubico OTP").as_deref(), Some("enabled"));
        assert_eq!(field(&node, "OpenPGP").as_deref(), Some("disabled"));
        // The names contain spaces, so the split is on the column gap and not
        // on whitespace — "YubiHSM Auth" must not become "YubiHSM".
        assert_eq!(field(&node, "YubiHSM Auth").as_deref(), Some("enabled"));
    }

    #[test]
    fn a_column_gap_is_two_spaces_or_a_tab() {
        assert_eq!(
            split_columns("Yubico OTP  \tEnabled"),
            Some(("Yubico OTP", "Enabled"))
        );
        assert_eq!(split_columns("PIV\tEnabled"), Some(("PIV", "Enabled")));
        // A single space is inside a name, not between columns.
        assert_eq!(split_columns("Yubico OTP"), None);
        assert_eq!(split_columns(""), None);
    }

    #[test]
    fn fido2_reports_whether_a_pin_is_set() {
        let node = fido2(YKMAN_FIDO);
        assert_eq!(
            field(&node, "PIN").as_deref(),
            Some("set, 8 attempt(s) remaining")
        );
        assert_eq!(
            field(&node, "Minimum PIN length").as_deref(),
            Some("4 characters")
        );

        // The state worth shouting about: with no PIN, a resident credential
        // is usable by whoever is holding the key.
        let node = fido2("PIN: not set\n");
        let pin = field(&node, "PIN").expect("PIN is always stated");
        assert!(pin.starts_with("not set"), "{pin}");
        assert!(pin.contains("anyone holding it"), "{pin}");
    }

    #[test]
    fn fido2_does_not_offer_to_enumerate_credentials() {
        // Listing them needs the device's PIN, and a page that reports what a
        // machine is must never ask for a secret.
        let node = fido2(YKMAN_FIDO);
        let credentials = field(&node, "Credentials").expect("the gap is named");
        assert!(credentials.contains("requires its PIN"), "{credentials}");
    }

    #[test]
    fn otp_slots_are_reported_per_slot() {
        let reported = otp_slots("Slot 1: programmed\nSlot 2: empty\n");
        assert_eq!(reported, "Slot 1 programmed, Slot 2 empty");

        // A format this does not recognise says so rather than claiming two
        // empty slots.
        let reported = otp_slots("something else entirely\n");
        assert!(reported.contains("no slot state"), "{reported}");
    }

    #[test]
    fn an_unrecognised_applet_format_claims_nothing() {
        // The formats differ between ykman releases. A parser that finds
        // nothing must say so; one that guessed at fields would report a key
        // that is not there.
        assert_eq!(
            populated("unexpected output\n", "4 slots"),
            "4 slots — all empty"
        );
        assert_eq!(
            populated("Slot 9A:\n  Algorithm: RSA2048\nSlot 9E:\n", "4 slots"),
            "in use: Slot 9A, Slot 9E"
        );
    }

    #[test]
    fn a_non_yubico_token_gets_no_vendor_detail() {
        // The detection is generic; this is not, and must not be run against
        // a key whose protocols ykman cannot speak.
        let root = fixture("nonyubico");
        let device = device(&root, "1-7", &["03"], Some(0));
        assert!(
            yubikey(&device, 1).is_empty(),
            "a non-Yubico key gets no ykman detail"
        );
        assert_eq!(yubico_count(&root), 0);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn two_keys_are_not_guessed_between() {
        // ykman needs a --device serial with more than one plugged in, and
        // nothing here can map a serial back to a USB port.
        let root = fixture("twokeys");
        let mut device = device(&root, "1-5", &["03"], Some(0));
        write(device.join("idVendor"), b"1050\n");
        device = device.clone();
        let detail = yubikey(&device, 2);
        assert_eq!(detail.len(), 1);
        let text = detail[0]
            .value
            .as_ref()
            .map(|value| value.to_string())
            .unwrap_or_default();
        assert!(text.contains("more than one"), "{text}");

        let _ = std::fs::remove_dir_all(root);
    }

    fn field(node: &Node, label: &str) -> Option<String> {
        node.children
            .iter()
            .find(|child| child.label == label)
            .and_then(|child| child.value.as_ref())
            .map(|value| value.to_string())
    }
}
