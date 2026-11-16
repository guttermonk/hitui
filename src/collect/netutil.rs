//! Address parsing shared by the /proc/net collectors.
//!
//! These files do not agree on byte order, and the difference is invisible
//! until the addresses come out wrong:
//!
//! - /proc/net/tcp, udp, tcp6, udp6 and route print 32-bit words with
//!   `%08X`, which on a little-endian machine reverses each word's bytes.
//!   `0100007F` is 127.0.0.1; read big-endian it would be 1.0.0.127.
//! - /proc/net/ipv6_route and if_inet6 print the address bytes in order,
//!   with no swapping at all.
//!
//! So an IPv6 address needs [`ipv6_words`] in the first case and [`ipv6_raw`]
//! in the second. Using one for the other silently yields a valid-looking
//! address belonging to somebody else.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Parse an IPv4 address from the 8-hex-digit word form.
pub fn ipv4(hex: &str) -> Option<Ipv4Addr> {
    if hex.len() != 8 {
        return None;
    }
    let raw = u32::from_str_radix(hex, 16).ok()?;
    // Host order to network order.
    Some(Ipv4Addr::from(raw.swap_bytes()))
}

/// Parse an IPv6 address written as four byte-swapped 32-bit words.
///
/// This is the socket-table form: /proc/net/tcp6 and udp6.
pub fn ipv6_words(hex: &str) -> Option<Ipv6Addr> {
    if hex.len() != 32 {
        return None;
    }
    let mut octets = [0u8; 16];
    for word in 0..4 {
        let raw = u32::from_str_radix(hex.get(word * 8..word * 8 + 8)?, 16).ok()?;
        octets[word * 4..word * 4 + 4].copy_from_slice(&raw.swap_bytes().to_be_bytes());
    }
    Some(Ipv6Addr::from(octets))
}

/// Parse an IPv6 address written as 16 bytes in network order.
///
/// This is the form /proc/net/ipv6_route and /proc/net/if_inet6 use.
pub fn ipv6_raw(hex: &str) -> Option<Ipv6Addr> {
    if hex.len() != 32 {
        return None;
    }
    let mut segments = [0u16; 8];
    for (index, segment) in segments.iter_mut().enumerate() {
        *segment = u16::from_str_radix(hex.get(index * 4..index * 4 + 4)?, 16).ok()?;
    }
    Some(Ipv6Addr::from(segments))
}

/// Parse either family from the socket-table form, chosen by hex length.
pub fn address(hex: &str) -> Option<IpAddr> {
    match hex.len() {
        8 => ipv4(hex).map(IpAddr::V4),
        32 => ipv6_words(hex).map(IpAddr::V6),
        _ => None,
    }
}

/// Parse an `address:port` pair, as /proc/net/tcp writes endpoints.
pub fn endpoint(field: &str) -> Option<String> {
    let (host, port) = field.rsplit_once(':')?;
    let address = address(host)?;
    let port = u16::from_str_radix(port, 16).ok()?;

    // Bracket IPv6 so the port stays unambiguous.
    Some(match address {
        IpAddr::V6(v6) => format!("[{v6}]:{port}"),
        IpAddr::V4(v4) => format!("{v4}:{port}"),
    })
}

/// Convert an IPv4 netmask to its prefix length.
pub fn prefix_of(mask: Ipv4Addr) -> u8 {
    u32::from(mask).count_ones() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_is_byte_swapped_from_host_order() {
        // The bug this guards: reading 0100007F big-endian gives 1.0.0.127.
        assert_eq!(ipv4("0100007F").unwrap(), Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(ipv4("F401A8C0").unwrap(), Ipv4Addr::new(192, 168, 1, 244));
        assert_eq!(ipv4("00000000").unwrap(), Ipv4Addr::UNSPECIFIED);
        // Lower case is equally valid in these files.
        assert_eq!(ipv4("0101a8c0").unwrap(), Ipv4Addr::new(192, 168, 1, 1));
    }

    #[test]
    fn socket_tables_swap_within_each_word() {
        // How ::1 appears in /proc/net/tcp6.
        assert_eq!(
            ipv6_words("00000000000000000000000001000000").unwrap(),
            Ipv6Addr::LOCALHOST
        );
    }

    #[test]
    fn route_and_interface_tables_are_not_swapped() {
        // A real link-local prefix from /proc/net/ipv6_route on this machine,
        // and a real address from /proc/net/if_inet6.
        assert_eq!(
            ipv6_raw("fe800000000000000000000000000000")
                .unwrap()
                .to_string(),
            "fe80::"
        );
        assert_eq!(
            ipv6_raw("fe80000000000000eb8c658f52de1b08")
                .unwrap()
                .to_string(),
            "fe80::eb8c:658f:52de:1b08"
        );
        assert_eq!(
            ipv6_raw("00000000000000000000000000000001").unwrap(),
            Ipv6Addr::LOCALHOST
        );
    }

    #[test]
    fn the_two_forms_disagree_and_must_not_be_interchanged() {
        // The bug this guards: fe80:: decoded with the wrong function gives
        // 0:80fe::, which is a syntactically valid address somewhere else.
        let hex = "fe800000000000000000000000000000";
        assert_ne!(ipv6_raw(hex).unwrap(), ipv6_words(hex).unwrap());
    }

    #[test]
    fn endpoints_keep_the_port_unambiguous() {
        assert_eq!(endpoint("0100007F:2350").unwrap(), "127.0.0.1:9040");
        assert_eq!(endpoint("00000000:0000").unwrap(), "0.0.0.0:0");

        // IPv6 is bracketed, or "::1:80" would be unreadable.
        let v6 = endpoint("00000000000000000000000001000000:0050").unwrap();
        assert!(v6.starts_with('[') && v6.ends_with(":80"), "{v6}");
    }

    #[test]
    fn malformed_input_is_rejected() {
        assert!(ipv4("").is_none());
        assert!(ipv4("0100").is_none());
        assert!(ipv4("zzzzzzzz").is_none());
        assert!(ipv6_words("0100007F").is_none());
        assert!(ipv6_raw("0100007F").is_none());
        assert!(endpoint("nocolon").is_none());
        assert!(endpoint("0100007F:zzzz").is_none());
    }

    #[test]
    fn netmasks_become_prefix_lengths() {
        assert_eq!(prefix_of(Ipv4Addr::new(255, 255, 255, 0)), 24);
        assert_eq!(prefix_of(Ipv4Addr::new(255, 255, 0, 0)), 16);
        assert_eq!(prefix_of(Ipv4Addr::UNSPECIFIED), 0);
        assert_eq!(prefix_of(Ipv4Addr::new(255, 255, 255, 255)), 32);
    }
}
