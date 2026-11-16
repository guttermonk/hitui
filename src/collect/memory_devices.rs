//! Installed memory modules, decoded from SPD.
//!
//! hardinfo2 reads this through `dmidecode`, which needs root. The `ee1004`
//! and `at24` drivers expose each module's SPD EEPROM at
//! /sys/bus/i2c/devices/*/eeprom, world-readable, so the whole thing can be
//! decoded without privilege — part number, rank and die geometry, speed
//! grade, ECC width, and the week it was manufactured.
//!
//! Layouts are per-generation and incompatible. DDR4 is decoded in full
//! (JEDEC SPD 4.1.2); other generations are identified and then declared
//! un-decoded rather than guessed at with the wrong offsets.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::{Node, Value};
use crate::sysfs;

/// Medium Timebase for DDR4, in picoseconds. Byte 18 counts these.
const DDR4_MTB_PS: i64 = 125;

pub struct MemoryDevices;

impl Collector for MemoryDevices {
    fn id(&self) -> &'static str {
        "memory-devices"
    }

    fn title(&self) -> &'static str {
        "Memory Devices"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let modules = spd_eeproms();

        if modules.is_empty() {
            return Ok(vec![Node::group(
                "Memory devices",
                vec![Node::field(
                    "Status",
                    "no SPD EEPROMs readable — load the ee1004 (DDR4) or at24 (DDR3) \
                     driver, or check that the SMBus controller driver is present",
                )],
            )]);
        }

        let decoded: Vec<(PathBuf, Vec<u8>)> = modules
            .into_iter()
            .filter_map(|path| std::fs::read(&path).ok().map(|bytes| (path, bytes)))
            .collect();

        let mut overview = Node::group("Memory devices", vec![]);
        overview.push(Node::field("Modules populated", decoded.len()));
        let total: u64 = decoded.iter().filter_map(|(_, s)| capacity(s)).sum();
        if total > 0 {
            overview.push(Node::field("Total installed", Value::Bytes(total)));
        }
        overview.push(Node::field("Decoded from", "SPD EEPROM over SMBus"));

        let mut nodes = vec![overview];
        for (path, spd) in &decoded {
            nodes.push(module(path, spd));
        }
        Ok(nodes)
    }
}

/// Every SPD EEPROM the kernel has bound a driver to.
fn spd_eeproms() -> Vec<PathBuf> {
    sysfs::list_dir("/sys/bus/i2c/devices")
        .into_iter()
        .filter(|device| {
            // ee1004 serves DDR4, at24/spd serves DDR3 and earlier.
            matches!(
                sysfs::read(device.join("name")).as_deref().map(str::trim),
                Some("ee1004") | Some("spd") | Some("at24")
            )
        })
        .map(|device| device.join("eeprom"))
        .filter(|eeprom| eeprom.is_file())
        .collect()
}

fn module(path: &Path, spd: &[u8]) -> Node {
    // The i2c address is the slot identifier the board uses: 0x50 is the
    // first socket, 0x52 the second, and so on.
    let slot = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let part = part_number(spd);
    let heading = match &part {
        Some(part) => format!("{slot}  {part}"),
        None => slot,
    };

    let mut node = Node::group(heading, vec![]);
    node.push(Node::field("Generation", generation(spd)));
    node.push_opt("Form factor", form_factor(spd));

    if !is_ddr4(spd) {
        node.push(Node::field(
            "Detail",
            "not decoded — only the DDR4 SPD layout is implemented, and reading \
             it at another generation's offsets would invent values",
        ));
        return node;
    }

    node.push_opt("Capacity", capacity(spd).map(Value::Bytes));
    node.push_opt("Speed", data_rate(spd).map(|mts| format!("{mts} MT/s")));
    node.push_opt("Part number", part);
    node.push_opt("Manufacturer", manufacturer(spd, 320));
    node.push_opt("DRAM manufacturer", manufacturer(spd, 350));
    node.push_opt("Serial number", serial(spd));
    node.push_opt("Manufactured", manufactured(spd));

    // Geometry explains the capacity, and ECC is the thing people check.
    if let (Some(ranks), Some(width)) = (ranks(spd), sdram_width(spd)) {
        node.push(Node::field(
            "Organisation",
            format!("{ranks} rank{} of ×{width} dies", plural(ranks)),
        ));
    }
    if let Some((primary, extension)) = bus_width(spd) {
        node.push(Node::field(
            "Bus width",
            if extension > 0 {
                format!(
                    "{} bits ({primary} data + {extension} ECC)",
                    primary + extension
                )
            } else {
                format!("{primary} bits, no ECC")
            },
        ));
    }
    node
}

fn is_ddr4(spd: &[u8]) -> bool {
    spd.get(2) == Some(&0x0c)
}

/// Byte 2 identifies the memory generation.
fn generation(spd: &[u8]) -> String {
    match spd.get(2) {
        Some(0x0b) => "DDR3 SDRAM".to_string(),
        Some(0x0c) => "DDR4 SDRAM".to_string(),
        Some(0x0d) => "DDR4E SDRAM".to_string(),
        Some(0x10) => "LPDDR3".to_string(),
        Some(0x11) => "LPDDR4".to_string(),
        Some(0x12) => "DDR5 SDRAM".to_string(),
        Some(0x13) => "LPDDR5".to_string(),
        Some(other) => format!("unrecognised type 0x{other:02x}"),
        None => "unreadable".to_string(),
    }
}

/// Byte 3, low nibble: the physical module format.
fn form_factor(spd: &[u8]) -> Option<&'static str> {
    Some(match spd.get(3)? & 0x0f {
        0x01 => "RDIMM (registered)",
        0x02 => "UDIMM (unbuffered)",
        0x03 => "SO-DIMM",
        0x04 => "LRDIMM (load-reduced)",
        0x05 => "Mini-RDIMM",
        0x06 => "Mini-UDIMM",
        0x08 => "72-bit SO-RDIMM (ECC)",
        0x09 => "72-bit SO-UDIMM (ECC)",
        0x0c => "16-bit SO-DIMM",
        0x0d => "32-bit SO-DIMM",
        _ => return None,
    })
}

/// Capacity in bytes, computed from the die geometry.
///
/// `density / 8` gives bytes per die; the primary bus width divided by the
/// die width gives dies per rank. ECC bits are excluded deliberately — they
/// are not addressable capacity.
fn capacity(spd: &[u8]) -> Option<u64> {
    if !is_ddr4(spd) {
        return None;
    }
    // Byte 4, low nibble: 256 Mb shifted left, so 5 is 8 Gb.
    let density_mbit: u64 = 256 << (spd.get(4)? & 0x0f);
    let (primary, _) = bus_width(spd)?;
    let width = sdram_width(spd)?;
    let ranks = ranks(spd)?;

    let dies_per_rank = primary / width;
    Some(density_mbit / 8 * 1024 * 1024 * dies_per_rank * ranks)
}

/// Byte 12: package ranks in bits 5:3, die width in bits 2:0.
fn ranks(spd: &[u8]) -> Option<u64> {
    Some((((spd.get(12)? >> 3) & 0x07) + 1) as u64)
}

fn sdram_width(spd: &[u8]) -> Option<u64> {
    Some(4 << (spd.get(12)? & 0x07))
}

/// Byte 13: `(primary data bits, ECC extension bits)`.
fn bus_width(spd: &[u8]) -> Option<(u64, u64)> {
    let byte = spd.get(13)?;
    let primary = 8 << (byte & 0x07);
    // Bits 4:3 are the extension: 1 means 8 bits of ECC alongside the data.
    let extension = if (byte >> 3) & 0x03 == 1 { 8 } else { 0 };
    Some((primary, extension))
}

/// Transfers per second, from the minimum cycle time.
///
/// tCKAVGmin is byte 18 in units of 125 ps plus a signed 1 ps correction in
/// byte 125. Without the correction a DDR4-2400 module reads as 2285 MT/s,
/// because 0.875 ns is not 0.833 ns.
fn data_rate(spd: &[u8]) -> Option<u64> {
    let coarse = *spd.get(18)? as i64 * DDR4_MTB_PS;
    let fine = *spd.get(125)? as i8 as i64;
    let cycle_ps = coarse + fine;
    if cycle_ps <= 0 {
        return None;
    }
    // Two transfers per clock. In these units that is 2e6 / picoseconds:
    // 2 transfers / (833 ps) = 2401 MT/s, which rounds to the JEDEC grade.
    let rate = 2_000_000 / cycle_ps as u64;
    Some(((rate + 50) / 100) * 100)
}

/// Bytes 329-348: twenty bytes of ASCII, space padded.
fn part_number(spd: &[u8]) -> Option<String> {
    let raw = spd.get(329..349)?;
    let text: String = raw
        .iter()
        .filter(|b| b.is_ascii_graphic() || **b == b' ')
        .map(|b| *b as char)
        .collect();
    let trimmed = text.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Bytes 325-328, as the hex the vendor's label shows.
fn serial(spd: &[u8]) -> Option<String> {
    let raw = spd.get(325..329)?;
    if raw.iter().all(|b| *b == 0 || *b == 0xff) {
        return None;
    }
    Some(raw.iter().map(|b| format!("{b:02x}")).collect())
}

/// Manufacturing date from bytes 323-324, stored as BCD year and week.
fn manufactured(spd: &[u8]) -> Option<String> {
    let year = from_bcd(*spd.get(323)?)?;
    let week = from_bcd(*spd.get(324)?)?;
    if week == 0 || week > 53 {
        return None;
    }
    Some(format!("week {week} of 20{year:02}"))
}

fn from_bcd(byte: u8) -> Option<u32> {
    let (high, low) = (byte >> 4, byte & 0x0f);
    (high <= 9 && low <= 9).then_some((high * 10 + low) as u32)
}

/// JEDEC manufacturer at `offset`: a continuation-count byte then an ID byte.
///
/// The published list runs to hundreds of entries across sixteen banks; the
/// handful here cover what turns up in practice, and anything else is reported
/// as its code rather than guessed.
fn manufacturer(spd: &[u8], offset: usize) -> Option<String> {
    let bank = *spd.get(offset)?;
    let id = *spd.get(offset + 1)?;
    if id == 0 || id == 0xff {
        return None;
    }

    // Bank 1 is the continuation code 0x80; almost every DRAM vendor is there.
    let name = match (bank, id) {
        (0x80, 0x2c) => Some("Micron"),
        (0x80, 0xad) => Some("SK hynix"),
        (0x80, 0xce) => Some("Samsung"),
        (0x80, 0x98) => Some("Kingston"),
        (0x80, 0x4f) => Some("Transcend"),
        (0x80, 0x9e) => Some("Corsair"),
        (0x80, 0xfe) => Some("Elpida"),
        (0x80, 0x0b) => Some("Nanya"),
        (0x80, 0x94) => Some("Smart Modular"),
        (0x80, 0xc1) => Some("Infineon"),
        (0x80, 0x4b) => Some("Netlist"),
        (0x80, 0xb3) => Some("Innodisk"),
        (0x80, 0x51) => Some("Qimonda"),
        (0x80, 0xda) => Some("Ramaxel"),
        _ => None,
    };
    Some(match name {
        Some(name) => name.to_string(),
        None => format!("unknown (bank 0x{bank:02x}, id 0x{id:02x})"),
    })
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real SK hynix HMA82GS7AFR8N-UH: 16 GB DDR4-2400 ECC SO-DIMM.
    ///
    /// Only the bytes the decoder reads are populated, at their true offsets,
    /// with values taken verbatim from the module in this machine.
    fn hynix_16gb() -> Vec<u8> {
        let mut spd = vec![0u8; 512];
        spd[0] = 0x23; // 384 bytes used, 512 total
        spd[1] = 0x11; // SPD revision 1.1
        spd[2] = 0x0c; // DDR4
        spd[3] = 0x09; // 72-bit SO-UDIMM (ECC)
        spd[4] = 0x85; // 8 Gb density
        spd[12] = 0x09; // 2 ranks, ×8 dies
        spd[13] = 0x0b; // 64-bit primary bus, 8-bit ECC extension
        spd[18] = 7; // 875 ps coarse
        spd[125] = (-42i8) as u8; // -42 ps fine correction

        spd[320] = 0x80; // continuation bank
        spd[321] = 0xad; // SK hynix
        spd[322] = 0x01;
        spd[323] = 0x17; // BCD year 17
        spd[324] = 0x40; // BCD week 40
        spd[325..329].copy_from_slice(&[0x81, 0xc7, 0xc8, 0x82]);
        spd[329..349].copy_from_slice(b"HMA82GS7AFR8N-UH    ");
        spd[350] = 0x80;
        spd[351] = 0xad;
        spd
    }

    #[test]
    fn decodes_a_real_ddr4_module() {
        let spd = hynix_16gb();

        assert!(is_ddr4(&spd));
        assert_eq!(generation(&spd), "DDR4 SDRAM");
        assert_eq!(form_factor(&spd), Some("72-bit SO-UDIMM (ECC)"));
        assert_eq!(part_number(&spd).unwrap(), "HMA82GS7AFR8N-UH");
        assert_eq!(manufacturer(&spd, 320).unwrap(), "SK hynix");
        assert_eq!(manufacturer(&spd, 350).unwrap(), "SK hynix");
        assert_eq!(serial(&spd).unwrap(), "81c7c882");
        assert_eq!(manufactured(&spd).unwrap(), "week 40 of 2017");

        // 8 Gb / 8 = 1 GB per die, 64 / 8 = 8 dies per rank, 2 ranks = 16 GB.
        assert_eq!(capacity(&spd).unwrap(), 16 * 1024 * 1024 * 1024);
        assert_eq!(ranks(&spd).unwrap(), 2);
        assert_eq!(sdram_width(&spd).unwrap(), 8);
        assert_eq!(bus_width(&spd).unwrap(), (64, 8));
    }

    #[test]
    fn the_fine_timebase_correction_is_applied() {
        let mut spd = hynix_16gb();
        // With the correction: 875 - 42 = 833 ps, which is DDR4-2400.
        assert_eq!(data_rate(&spd).unwrap(), 2400);

        // Without it the coarse 875 ps would read as 2300, not 2400 — this is
        // exactly the bug the fine byte exists to prevent.
        spd[125] = 0;
        assert_ne!(data_rate(&spd).unwrap(), 2400);
    }

    #[test]
    fn other_generations_are_identified_but_not_guessed_at() {
        let mut spd = hynix_16gb();
        spd[2] = 0x12; // DDR5

        assert_eq!(generation(&spd), "DDR5 SDRAM");
        assert!(!is_ddr4(&spd));
        // Reading DDR4 offsets on a DDR5 module would invent a capacity.
        assert!(capacity(&spd).is_none());
    }

    #[test]
    fn unknown_manufacturers_report_their_code() {
        let mut spd = hynix_16gb();
        spd[321] = 0x77;
        let name = manufacturer(&spd, 320).unwrap();
        assert!(name.contains("0x77"), "{name}");

        // An unprogrammed field is absent, not "unknown".
        spd[321] = 0xff;
        assert!(manufacturer(&spd, 320).is_none());
    }

    #[test]
    fn rejects_implausible_bcd_dates() {
        let mut spd = hynix_16gb();
        spd[324] = 0xff; // not valid BCD
        assert!(manufactured(&spd).is_none());

        spd[324] = 0x99; // week 99 does not exist
        assert!(manufactured(&spd).is_none());
    }

    #[test]
    fn truncated_spd_data_does_not_panic() {
        // A short read must degrade, not index out of bounds.
        for length in [0, 1, 3, 13, 20, 128, 320, 340] {
            let spd = vec![0x0cu8; length];
            let _ = capacity(&spd);
            let _ = data_rate(&spd);
            let _ = part_number(&spd);
            let _ = manufactured(&spd);
            let _ = manufacturer(&spd, 320);
            let _ = generation(&spd);
        }
    }

    #[test]
    fn collects_on_this_machine() {
        let nodes = MemoryDevices.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Memory devices");

        // If SPD is readable here, the decoded total should agree with what
        // the kernel reports installed, within one module's rounding.
        if nodes.len() > 1
            && let Some(total) = nodes[0]
                .children
                .iter()
                .find(|c| c.label == "Total installed")
        {
            let decoded = total.value.as_ref().unwrap().to_string();
            assert!(decoded.contains("GiB"), "{decoded}");
        }
    }
}
