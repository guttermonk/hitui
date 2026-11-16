//! Network statistics: per-interface counters and per-protocol totals.
//!
//! The interface counters come from /proc/net/dev, the protocol counters from
//! /proc/net/snmp. Only non-zero error and drop counters are shown — a column
//! of permanent zeroes buries the one number that matters.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::{Node, Value};
use crate::sysfs;

pub struct NetStat;

impl Collector for NetStat {
    fn id(&self) -> &'static str {
        "netstat"
    }

    fn title(&self) -> &'static str {
        "Statistics"
    }

    fn group(&self) -> Group {
        Group::Network
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes = vec![interfaces()];
        nodes.extend(protocols());
        Ok(nodes)
    }
}

/// Per-interface totals from /proc/net/dev.
fn interfaces() -> Node {
    let Some(text) = sysfs::read("/proc/net/dev") else {
        return Node::group(
            "Interfaces",
            vec![Node::field("Status", "/proc/net/dev is unavailable")],
        );
    };

    let mut node = Node::group("Interfaces", vec![]);

    // Two header lines, then one line per interface.
    for line in text.lines().skip(2) {
        let Some((name, counters)) = line.split_once(':') else {
            continue;
        };
        let fields: Vec<u64> = counters
            .split_whitespace()
            .map(|f| f.parse().unwrap_or(0))
            .collect();
        if fields.len() < 16 {
            continue;
        }

        // Receive: bytes packets errs drop fifo frame compressed multicast
        // Transmit: bytes packets errs drop fifo colls carrier compressed
        let mut interface = Node::group(name.trim(), vec![]);
        interface.push(Node::field("Received", Value::Bytes(fields[0])));
        interface.push(Node::field("Received packets", fields[1]));
        interface.push(Node::field("Sent", Value::Bytes(fields[8])));
        interface.push(Node::field("Sent packets", fields[9]));

        for (label, value) in [
            ("Receive errors", fields[2]),
            ("Receive drops", fields[3]),
            ("Receive FIFO overruns", fields[4]),
            ("Receive frame errors", fields[5]),
            ("Multicast", fields[7]),
            ("Transmit errors", fields[10]),
            ("Transmit drops", fields[11]),
            ("Transmit FIFO overruns", fields[12]),
            ("Collisions", fields[13]),
            ("Carrier losses", fields[14]),
        ] {
            // Zero is the normal value for all of these; printing ten zeroes
            // per interface hides the one that is not.
            if value > 0 {
                interface.push(Node::field(label, value));
            }
        }
        node.push(interface);
    }
    node
}

/// Protocol counters from /proc/net/snmp.
///
/// The file pairs a header line of names with a value line, per protocol.
/// Only a curated set is surfaced: the full IP table alone runs to twenty
/// columns, most of which are zero on a healthy host.
fn protocols() -> Vec<Node> {
    const INTERESTING: [(&str, &[&str]); 4] = [
        (
            "Ip",
            &[
                "InReceives",
                "InHdrErrors",
                "InAddrErrors",
                "InDiscards",
                "InDelivers",
                "OutRequests",
                "OutDiscards",
                "OutNoRoutes",
                "ReasmFails",
                "FragFails",
            ],
        ),
        (
            "Tcp",
            &[
                "ActiveOpens",
                "PassiveOpens",
                "AttemptFails",
                "EstabResets",
                "CurrEstab",
                "InSegs",
                "OutSegs",
                "RetransSegs",
                "InErrs",
                "OutRsts",
            ],
        ),
        (
            "Udp",
            &[
                "InDatagrams",
                "NoPorts",
                "InErrors",
                "OutDatagrams",
                "RcvbufErrors",
                "SndbufErrors",
            ],
        ),
        (
            "Icmp",
            &[
                "InMsgs",
                "InErrors",
                "InDestUnreachs",
                "OutMsgs",
                "OutErrors",
            ],
        ),
    ];

    let Some(text) = sysfs::read("/proc/net/snmp") else {
        return Vec::new();
    };
    let table = parse_snmp(&text);

    INTERESTING
        .iter()
        .filter_map(|(protocol, wanted)| {
            let counters = table.iter().find(|(name, _)| name == protocol)?;
            let mut node = Node::group(*protocol, vec![]);

            for name in *wanted {
                if let Some((_, value)) = counters.1.iter().find(|(key, _)| key == name) {
                    node.push(Node::field(readable(name), *value));
                }
            }
            (!node.children.is_empty()).then_some(node)
        })
        .collect()
}

/// Pair each protocol's header line with its value line.
type Snmp = Vec<(String, Vec<(String, u64)>)>;

fn parse_snmp(text: &str) -> Snmp {
    let mut table = Snmp::new();
    let mut lines = text.lines();

    while let (Some(header), Some(values)) = (lines.next(), lines.next()) {
        let Some((protocol, names)) = header.split_once(':') else {
            continue;
        };
        let Some((value_protocol, numbers)) = values.split_once(':') else {
            continue;
        };
        // The two lines must belong to the same protocol, or the pairing has
        // slipped and every number would be attributed to the wrong name.
        if protocol != value_protocol {
            continue;
        }
        let counters = names
            .split_whitespace()
            .zip(numbers.split_whitespace())
            .filter_map(|(name, value)| Some((name.to_string(), value.parse().ok()?)))
            .collect();
        table.push((protocol.to_string(), counters));
    }
    table
}

/// Space out the kernel's CamelCase counter names.
fn readable(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, c) in name.char_indices() {
        if index > 0 && c.is_uppercase() {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_snmp_headers_with_their_values() {
        let text = "Ip: Forwarding DefaultTTL InReceives\n\
                    Ip: 2 64 2046714\n\
                    Tcp: ActiveOpens PassiveOpens\n\
                    Tcp: 100 200\n";
        let table = parse_snmp(text);

        assert_eq!(table.len(), 2);
        assert_eq!(table[0].0, "Ip");
        assert_eq!(
            table[0].1,
            vec![
                ("Forwarding".to_string(), 2),
                ("DefaultTTL".to_string(), 64),
                ("InReceives".to_string(), 2046714)
            ]
        );
        assert_eq!(table[1].1[1], ("PassiveOpens".to_string(), 200));
    }

    #[test]
    fn mismatched_protocol_lines_are_rejected() {
        // If the pairing slipped, every value would be filed under the wrong
        // name — better to drop the pair than to report fiction.
        let text = "Ip: Forwarding DefaultTTL\nTcp: 2 64\n";
        assert!(parse_snmp(text).is_empty());
    }

    #[test]
    fn counter_names_are_spaced_out() {
        assert_eq!(readable("InReceives"), "In receives");
        assert_eq!(readable("OutNoRoutes"), "Out no routes");
        assert_eq!(readable("RetransSegs"), "Retrans segs");
    }

    #[test]
    fn zero_error_counters_are_omitted() {
        let node = interfaces();
        // Loopback never has errors, so it should carry only the four totals.
        if let Some(lo) = node.children.iter().find(|c| c.label == "lo") {
            for field in &lo.children {
                if field.label.contains("error") || field.label.contains("drop") {
                    let value = field.value.as_ref().unwrap().to_string();
                    assert_ne!(value, "0", "{} should have been omitted", field.label);
                }
            }
        }
    }

    #[test]
    fn collects_on_this_machine() {
        let nodes = NetStat.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Interfaces");
        assert!(!nodes[0].children.is_empty());

        // Protocol sections follow, named for the protocols themselves.
        let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        assert!(
            labels.contains(&"Tcp") || labels.contains(&"Ip"),
            "{labels:?}"
        );
    }
}
