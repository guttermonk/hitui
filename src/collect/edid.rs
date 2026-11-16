//! EDID decoding.
//!
//! Every display reports its identity and capabilities as a 128-byte EDID
//! block, which the kernel exposes verbatim at
//! /sys/class/drm/*/edid. Note that sysfs reports the file's *size* as zero
//! while still returning the data on read, so it has to be read rather than
//! statted.
//!
//! Layout references are VESA E-EDID 1.4. Only the base block is decoded;
//! CTA-861 extension blocks carry audio and HDR capability and are not needed
//! to identify a panel.

use std::path::Path;

/// EDID blocks always begin with this fixed pattern.
const MAGIC: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];

/// Offset of the first of four 18-byte descriptor slots.
const DESCRIPTORS: usize = 54;
const DESCRIPTOR_LEN: usize = 18;

#[derive(Debug, Default, PartialEq)]
pub struct Edid {
    /// Three-letter PNP manufacturer code, e.g. `LGD`.
    pub manufacturer: String,
    pub product_code: u16,
    pub serial: u32,
    /// Name from a product-name descriptor, when the display provides one.
    pub name: Option<String>,
    /// Serial as an ASCII string, when provided.
    pub serial_text: Option<String>,
    pub week: Option<u8>,
    pub year: Option<u16>,
    pub version: String,
    pub digital: bool,
    /// Physical size in centimetres.
    pub size_cm: Option<(u8, u8)>,
    /// Preferred timing: width, height, and refresh rate in Hz.
    pub preferred_mode: Option<(u32, u32, f64)>,
}

impl Edid {
    /// Screen diagonal in inches, from the physical size.
    pub fn diagonal_inches(&self) -> Option<f64> {
        let (w, h) = self.size_cm?;
        if w == 0 || h == 0 {
            return None;
        }
        let diagonal_cm = ((w as f64).powi(2) + (h as f64).powi(2)).sqrt();
        Some(diagonal_cm / 2.54)
    }
}

/// Read and decode a connector's EDID.
pub fn read(path: &Path) -> Option<Edid> {
    // Read rather than stat: sysfs reports size 0 for this attribute.
    parse(&std::fs::read(path).ok()?)
}

/// Decode an EDID block, or `None` if it is not one.
pub fn parse(bytes: &[u8]) -> Option<Edid> {
    // A disconnected connector yields an empty file; a flaky one can yield
    // garbage. Both must be rejected rather than decoded into nonsense.
    if bytes.len() < 128 || bytes[..8] != MAGIC {
        return None;
    }

    let mut edid = Edid {
        manufacturer: manufacturer(bytes),
        product_code: u16::from_le_bytes([bytes[10], bytes[11]]),
        serial: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        version: format!("{}.{}", bytes[18], bytes[19]),
        // Byte 20 bit 7 distinguishes a digital input from analog VGA.
        digital: bytes[20] & 0x80 != 0,
        ..Default::default()
    };

    // Week 0 and year 0 mean unspecified; the year is stored as an offset.
    if bytes[16] > 0 && bytes[16] <= 54 {
        edid.week = Some(bytes[16]);
    }
    if bytes[17] > 0 {
        edid.year = Some(1990 + bytes[17] as u16);
    }
    if bytes[21] > 0 && bytes[22] > 0 {
        edid.size_cm = Some((bytes[21], bytes[22]));
    }

    for slot in 0..4 {
        let start = DESCRIPTORS + slot * DESCRIPTOR_LEN;
        let Some(descriptor) = bytes.get(start..start + DESCRIPTOR_LEN) else {
            break;
        };
        decode_descriptor(descriptor, &mut edid);
    }
    Some(edid)
}

/// The PNP ID packed into bytes 8-9 as three 5-bit letters, big-endian.
///
/// A value of 1 is 'A', so each field is offset by 64 to reach ASCII.
fn manufacturer(bytes: &[u8]) -> String {
    let packed = u16::from_be_bytes([bytes[8], bytes[9]]);
    [(packed >> 10) & 0x1f, (packed >> 5) & 0x1f, packed & 0x1f]
        .iter()
        .map(|letter| (b'A' + (*letter as u8).saturating_sub(1)) as char)
        .collect()
}

/// A descriptor slot is either a detailed timing or a tagged display
/// descriptor, told apart by whether its first two bytes are a pixel clock.
fn decode_descriptor(descriptor: &[u8], edid: &mut Edid) {
    let pixel_clock = u16::from_le_bytes([descriptor[0], descriptor[1]]);
    if pixel_clock != 0 {
        // Detailed timing. Keep only the first, which VESA defines as the
        // display's preferred mode.
        if edid.preferred_mode.is_none() {
            edid.preferred_mode = timing(descriptor, pixel_clock);
        }
        return;
    }

    // Display descriptor: byte 3 is the tag.
    let text = || {
        String::from_utf8_lossy(&descriptor[5..18])
            .trim_end_matches(['\n', ' ', '\0'])
            .trim()
            .to_string()
    };
    match descriptor[3] {
        0xfc => {
            let name = text();
            if !name.is_empty() {
                // Long names span two descriptors and are concatenated.
                match &mut edid.name {
                    Some(existing) => existing.push_str(&name),
                    None => edid.name = Some(name),
                }
            }
        }
        0xff => {
            let serial = text();
            if !serial.is_empty() {
                edid.serial_text = Some(serial);
            }
        }
        _ => {}
    }
}

/// Active resolution and refresh rate from a detailed timing descriptor.
///
/// Horizontal and vertical values are split across a low byte and a nibble of
/// a shared high byte, so each needs reassembling before it means anything.
fn timing(descriptor: &[u8], pixel_clock: u16) -> Option<(u32, u32, f64)> {
    let h_active = descriptor[2] as u32 | ((descriptor[4] as u32 & 0xf0) << 4);
    let h_blank = descriptor[3] as u32 | ((descriptor[4] as u32 & 0x0f) << 8);
    let v_active = descriptor[5] as u32 | ((descriptor[7] as u32 & 0xf0) << 4);
    let v_blank = descriptor[6] as u32 | ((descriptor[7] as u32 & 0x0f) << 8);

    let h_total = h_active + h_blank;
    let v_total = v_active + v_blank;
    if h_active == 0 || v_active == 0 || h_total == 0 || v_total == 0 {
        return None;
    }

    // The clock is stored in units of 10 kHz.
    let hz = pixel_clock as f64 * 10_000.0 / (h_total as f64 * v_total as f64);
    Some((h_active, v_active, hz))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first 32 bytes of this machine's panel, verbatim, padded out to a
    /// full block with a detailed timing descriptor for 1920x1080.
    fn panel() -> Vec<u8> {
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&MAGIC);
        edid[8] = 0x30; // "LGD" packed
        edid[9] = 0xe4;
        edid[10] = 0x38; // product code 0x0538
        edid[11] = 0x05;
        edid[16] = 0x00; // week unspecified
        edid[17] = 0x1a; // 1990 + 26 = 2016
        edid[18] = 0x01; // EDID 1.4
        edid[19] = 0x04;
        edid[20] = 0x95; // digital input
        edid[21] = 0x22; // 34 cm wide
        edid[22] = 0x13; // 19 cm tall

        // A 1920x1080 detailed timing: 148.5 MHz clock, 2200x1125 total.
        let timing = &mut edid[DESCRIPTORS..DESCRIPTORS + DESCRIPTOR_LEN];
        timing[0..2].copy_from_slice(&14850u16.to_le_bytes());
        timing[2] = 0x80; // h active low byte (1920 & 0xff)
        timing[3] = 0x18; // h blank low byte (280 & 0xff)
        timing[4] = 0x71; // h active high nibble 7, h blank high nibble 1
        timing[5] = 0x38; // v active low byte (1080 & 0xff)
        timing[6] = 0x2d; // v blank low byte (45)
        timing[7] = 0x40; // v active high nibble 4, v blank high 0
        edid
    }

    #[test]
    fn unpacks_the_pnp_manufacturer_code() {
        // 0x30e4 is three 5-bit letters: 01100 00111 00100 = L, G, D.
        assert_eq!(manufacturer(&panel()), "LGD");
    }

    #[test]
    fn decodes_this_machines_panel() {
        let edid = parse(&panel()).expect("valid EDID");

        assert_eq!(edid.manufacturer, "LGD");
        assert_eq!(edid.product_code, 0x0538);
        assert_eq!(edid.version, "1.4");
        assert!(edid.digital);
        assert_eq!(edid.year, Some(2016));
        assert_eq!(edid.size_cm, Some((34, 19)));

        // 34 x 19 cm is a 15.4" panel, which is what a P51 has.
        let inches = edid.diagonal_inches().unwrap();
        assert!((15.0..15.8).contains(&inches), "got {inches}");
    }

    #[test]
    fn reassembles_split_timing_fields() {
        // The bug this guards: reading only the low bytes gives 128x56
        // instead of 1920x1080, because the high bits live in a shared nibble.
        let edid = parse(&panel()).expect("valid EDID");
        let (width, height, hz) = edid.preferred_mode.expect("a preferred mode");

        assert_eq!((width, height), (1920, 1080));
        assert!((59.5..60.5).contains(&hz), "expected ~60 Hz, got {hz}");
    }

    #[test]
    fn rejects_anything_that_is_not_an_edid() {
        // A disconnected connector returns an empty file.
        assert!(parse(&[]).is_none());
        // A truncated read is not a block.
        assert!(parse(&[0u8; 64]).is_none());
        // Right length, wrong header: garbage must not decode.
        assert!(parse(&[0x42u8; 128]).is_none());
    }

    #[test]
    fn reads_product_name_and_serial_descriptors() {
        let mut edid = panel();
        // Second slot: a product-name descriptor.
        let slot = DESCRIPTORS + DESCRIPTOR_LEN;
        edid[slot + 3] = 0xfc;
        edid[slot + 5..slot + 13].copy_from_slice(b"TestPane");
        // Third slot: an ASCII serial.
        let slot = DESCRIPTORS + 2 * DESCRIPTOR_LEN;
        edid[slot + 3] = 0xff;
        edid[slot + 5..slot + 11].copy_from_slice(b"SN1234");

        let decoded = parse(&edid).expect("valid EDID");
        assert_eq!(decoded.name.as_deref(), Some("TestPane"));
        assert_eq!(decoded.serial_text.as_deref(), Some("SN1234"));
    }

    #[test]
    fn implausible_dates_are_dropped() {
        let mut edid = panel();
        edid[16] = 99; // there is no week 99
        assert!(parse(&edid).unwrap().week.is_none());
    }
}
