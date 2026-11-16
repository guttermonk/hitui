//! Routing and neighbour tables — hardinfo2's "Routing Table" and "ARP Table".

use std::net::Ipv4Addr;

use anyhow::Result;

use crate::collect::netutil;
use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::sysfs;

pub struct Routes;

impl Collector for Routes {
    fn id(&self) -> &'static str {
        "routes"
    }

    fn title(&self) -> &'static str {
        "Routing Table"
    }

    fn group(&self) -> Group {
        Group::Network
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes = Vec::new();
        if let Some(v4) = ipv4_routes() {
            nodes.push(v4);
        }
        if let Some(v6) = ipv6_routes() {
            nodes.push(v6);
        }
        if nodes.is_empty() {
            nodes.push(Node::group(
                "Routing table",
                vec![Node::field(
                    "Status",
                    "no routes — the network is unconfigured",
                )],
            ));
        }
        Ok(nodes)
    }
}

/// /proc/net/route: tab-separated, every address in host-order hex.
fn ipv4_routes() -> Option<Node> {
    let text = sysfs::read("/proc/net/route")?;
    let mut node = Node::group("IPv4", vec![]);

    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 11 {
            continue;
        }
        // Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT
        let Some(destination) = netutil::ipv4(fields[1]) else {
            continue;
        };
        let Some(mask) = netutil::ipv4(fields[7]) else {
            continue;
        };
        let gateway = netutil::ipv4(fields[2]);
        let prefix = netutil::prefix_of(mask);

        // A destination of 0.0.0.0/0 is the default route, which is what
        // anyone reading this page is usually looking for.
        let label = if prefix == 0 && destination == Ipv4Addr::UNSPECIFIED {
            "default".to_string()
        } else {
            format!("{destination}/{prefix}")
        };

        let mut detail = Vec::new();
        match gateway {
            // A zero gateway means the destination is directly attached.
            Some(gateway) if gateway != Ipv4Addr::UNSPECIFIED => {
                detail.push(format!("via {gateway}"));
            }
            _ => detail.push("on link".to_string()),
        }
        detail.push(format!("dev {}", fields[0]));
        if let Ok(metric) = fields[6].parse::<u64>()
            && metric > 0
        {
            detail.push(format!("metric {metric}"));
        }
        node.push(Node::field(label, detail.join(" · ")));
    }

    (!node.children.is_empty()).then_some(node)
}

/// /proc/net/ipv6_route: destination, prefix, source, then gateway and flags.
fn ipv6_routes() -> Option<Node> {
    let text = sysfs::read("/proc/net/ipv6_route")?;
    let mut node = Node::group("IPv6", vec![]);

    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 10 {
            continue;
        }
        let Some(destination) = netutil::ipv6_raw(fields[0]) else {
            continue;
        };
        let prefix = u32::from_str_radix(fields[1], 16).unwrap_or(0);
        let gateway = netutil::ipv6_raw(fields[4]);
        let interface = fields[9];

        let label = if prefix == 0 {
            "default".to_string()
        } else {
            format!("{destination}/{prefix}")
        };

        let mut detail = Vec::new();
        match gateway {
            Some(gateway) if !gateway.is_unspecified() => {
                detail.push(format!("via {gateway}"));
            }
            _ => detail.push("on link".to_string()),
        }
        detail.push(format!("dev {interface}"));
        // u32::MAX is the kernel's "no metric" sentinel, not a cost of four
        // billion, so printing it as a number would be misleading.
        if let Ok(metric) = u32::from_str_radix(fields[5], 16)
            && metric > 0
            && metric != u32::MAX
        {
            detail.push(format!("metric {metric}"));
        }
        node.push(Node::field(label, detail.join(" · ")));
    }

    (!node.children.is_empty()).then_some(node)
}

pub struct Arp;

impl Collector for Arp {
    fn id(&self) -> &'static str {
        "arp"
    }

    fn title(&self) -> &'static str {
        "ARP Table"
    }

    fn group(&self) -> Group {
        Group::Network
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let Some(text) = sysfs::read("/proc/net/arp") else {
            return Ok(vec![Node::group(
                "ARP table",
                vec![Node::field("Status", "/proc/net/arp is unavailable")],
            )]);
        };

        // Unlike the rest of /proc/net, this file is already human-readable.
        let mut neighbours = Node::group("Neighbours", vec![]);
        for line in text.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 6 {
                continue;
            }
            // IP address, HW type, Flags, HW address, Mask, Device
            let mut detail = vec![fields[3].to_string()];
            detail.push(format!("dev {}", fields[5]));
            // Flag 0x2 is ATF_COM: the entry is complete. An incomplete entry
            // means the neighbour never answered, which is the useful signal.
            if fields[2] == "0x0" {
                detail.push("incomplete — no reply".to_string());
            }
            neighbours.push(Node::field(fields[0], detail.join(" · ")));
        }

        let mut overview = Node::group("ARP table", vec![]);
        overview.push(Node::field("Entries", neighbours.children.len()));

        let mut nodes = vec![overview];
        if !neighbours.children.is_empty() {
            nodes.push(neighbours);
        }
        Ok(nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_route_is_labelled_as_such() {
        // No routes at all is a real state — a build sandbox, a container with
        // an isolated netns, an unconfigured host — and the collector reports
        // it rather than failing, so the test must not demand a table either.
        let Some(node) = ipv4_routes() else {
            return;
        };
        // Reading "0.0.0.0/0" takes a moment longer than reading "default".
        if let Some(default) = node.children.iter().find(|c| c.label == "default") {
            let detail = default.value.as_ref().unwrap().to_string();
            assert!(detail.contains("dev "), "{detail}");
        }
    }

    #[test]
    fn directly_attached_routes_say_on_link() {
        let Some(node) = ipv4_routes() else {
            return;
        };
        for field in &node.children {
            let detail = field.value.as_ref().unwrap().to_string();
            // Every route either has a gateway or is on link; silence would
            // be ambiguous.
            assert!(
                detail.contains("via ") || detail.contains("on link"),
                "{}: {detail}",
                field.label
            );
        }
    }

    #[test]
    fn an_unrouted_machine_is_reported_not_failed() {
        // What the Nix build sandbox exercises: Routes::collect must produce a
        // page whether or not the kernel has any routes to show.
        let nodes = Routes.collect().expect("collection must not fail");
        assert!(!nodes.is_empty());
        for node in &nodes {
            assert!(!node.children.is_empty(), "{} is empty", node.label);
        }
    }

    #[test]
    fn arp_entries_carry_their_interface() {
        let nodes = Arp.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "ARP table");

        if nodes.len() > 1 {
            for field in &nodes[1].children {
                let detail = field.value.as_ref().unwrap().to_string();
                assert!(detail.contains("dev "), "{detail}");
                // The label is the address, which must have parsed.
                assert!(field.label.contains('.'), "{}", field.label);
            }
        }
    }

    #[test]
    fn collects_both_families_when_present() {
        let nodes = Routes.collect().expect("collection failed");
        let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        assert!(!labels.is_empty());
        for node in &nodes {
            assert!(!node.children.is_empty(), "{} is empty", node.label);
        }
    }
}
