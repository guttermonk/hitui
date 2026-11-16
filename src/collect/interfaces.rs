//! Network interfaces.
//!
//! Link-layer facts — MAC, MTU, state, driver, counters — all come from
//! /sys/class/net. Addresses do not: the kernel exposes IPv6 through
//! /proc/net/if_inet6 but offers no equivalent for IPv4, which is readable
//! only over netlink. Rather than take a netlink stack as a dependency, hitui
//! asks `ip -j addr`, whose JSON mode is stable and which ships with iproute2
//! on essentially every Linux system. Without it the page still describes
//! every link and its IPv6 addresses.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde_json::Value as Json;

use crate::collect::netutil;
use crate::collect::{Collector, Group};
use crate::model::{Node, Value};
use crate::probe;
use crate::sysfs;

const NET: &str = "/sys/class/net";

pub struct Interfaces;

impl Collector for Interfaces {
    fn id(&self) -> &'static str {
        "interfaces"
    }

    fn title(&self) -> &'static str {
        "Interfaces"
    }

    fn group(&self) -> Group {
        Group::Network
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let names = interface_names();
        let addresses = ip_addresses();
        let fallback = inet6_addresses();

        let mut overview = Node::group("Network", vec![]);
        overview.push(Node::field("Interfaces", names.len()));
        overview.push(Node::field(
            "Addresses",
            if !addresses.is_empty() {
                "resolved by ip"
            } else if probe::available("ip") {
                "unavailable — ip produced no usable output"
            } else {
                "IPv6 only — install iproute2 for IPv4 addresses"
            },
        ));

        let mut nodes = vec![overview];
        for name in &names {
            nodes.push(interface(name, addresses.get(name), fallback.get(name)));
        }
        Ok(nodes)
    }
}

fn interface_names() -> Vec<String> {
    sysfs::list_dir(NET)
        .into_iter()
        .filter_map(|path| path.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect()
}

/// One assigned address: its family, and `address/prefix (scope)`.
pub struct Address {
    family: &'static str,
    text: String,
}

fn interface(
    name: &str,
    addresses: Option<&Vec<Address>>,
    fallback: Option<&Vec<Address>>,
) -> Node {
    let dir = Path::new(NET).join(name);
    let read = |file: &str| sysfs::read(dir.join(file)).map(|v| v.trim().to_string());

    let state = read("operstate").unwrap_or_else(|| "unknown".to_string());
    let mut node = Node::group(format!("{name}  {state}"), vec![]);

    node.push(Node::field("State", state.clone()));
    node.push(Node::field("Kind", link_kind(&dir)));
    node.push_opt(
        "MAC address",
        read("address").filter(|mac| mac != "00:00:00:00:00:00"),
    );
    node.push_opt("MTU", read("mtu"));

    // Speed and duplex are only populated by wired drivers with a live link;
    // reading them on a down or wireless interface yields -1 or nothing.
    if state == "up" {
        node.push_opt(
            "Speed",
            sysfs::read_u64(dir.join("speed")).map(|mbps| format!("{mbps} Mb/s")),
        );
        node.push_opt("Duplex", read("duplex"));
    }
    node.push_opt(
        "Carrier",
        sysfs::read_u64(dir.join("carrier")).map(|c| Value::Flag(c != 0)),
    );
    node.push_opt(
        "Driver",
        std::fs::read_link(dir.join("device/driver"))
            .ok()
            .and_then(|target| target.file_name().map(|n| n.to_string_lossy().to_string())),
    );

    // `ip` reports both families; the /proc fallback only knows IPv6.
    match addresses.or(fallback) {
        Some(list) if !list.is_empty() => {
            let mut group = Node::group(
                // Say when the answer is partial, so a reader does not take a
                // missing IPv4 address to mean there isn't one.
                if addresses.is_some() {
                    "Addresses"
                } else {
                    "Addresses (IPv6 only — install iproute2 for IPv4)"
                },
                vec![],
            );
            for address in list {
                // Keyed by family so the label column carries meaning rather
                // than repeating the address.
                group.push(Node::field(address.family, address.text.clone()));
            }
            node.push(group);
        }
        _ => node.push(Node::field("Addresses", "none assigned")),
    }

    if let Some(counters) = traffic(&dir) {
        node.push(counters);
    }
    node
}

/// What kind of link this is, since the name alone does not always say.
fn link_kind(dir: &Path) -> String {
    // A `wireless` directory is the reliable marker; `phy80211` appears only
    // for cfg80211 drivers.
    if dir.join("wireless").is_dir() || dir.join("phy80211").exists() {
        return "wireless".to_string();
    }
    if dir.join("bridge").is_dir() {
        return "bridge".to_string();
    }
    if dir.join("bonding").is_dir() {
        return "bond".to_string();
    }
    if dir.join("tun_flags").exists() {
        return "tun/tap".to_string();
    }
    // ARPHRD_LOOPBACK, from if_arp.h.
    if sysfs::read_u64(dir.join("type")) == Some(772) {
        return "loopback".to_string();
    }
    // A device symlink means real hardware behind a driver.
    if dir.join("device").exists() {
        "wired".to_string()
    } else {
        "virtual".to_string()
    }
}

/// Byte and packet counters, plus errors if there are any.
fn traffic(dir: &Path) -> Option<Node> {
    let stat = |name: &str| sysfs::read_u64(dir.join("statistics").join(name));

    let mut node = Node::group("Traffic", vec![]);
    node.push_opt("Received", stat("rx_bytes").map(Value::Bytes));
    node.push_opt("Sent", stat("tx_bytes").map(Value::Bytes));

    // Only surface error counters that are actually non-zero: four permanent
    // zeroes per interface is noise, a non-zero drop count is a finding.
    for (label, name) in [
        ("Receive errors", "rx_errors"),
        ("Transmit errors", "tx_errors"),
        ("Receive drops", "rx_dropped"),
        ("Transmit drops", "tx_dropped"),
        ("Collisions", "collisions"),
    ] {
        node.push_opt(label, stat(name).filter(|count| *count > 0));
    }

    (!node.children.is_empty()).then_some(node)
}

/// Addresses per interface from `ip -j addr show`.
fn ip_addresses() -> BTreeMap<String, Vec<Address>> {
    let Some(output) = probe::run("ip", &["-j", "addr", "show"]) else {
        return BTreeMap::new();
    };
    let Ok(links) = serde_json::from_str::<Json>(&output) else {
        return BTreeMap::new();
    };
    let Some(links) = links.as_array() else {
        return BTreeMap::new();
    };

    let mut by_interface = BTreeMap::new();
    for link in links {
        let Some(name) = link.get("ifname").and_then(Json::as_str) else {
            continue;
        };
        let addresses: Vec<Address> = link
            .get("addr_info")
            .and_then(Json::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        let local = entry.get("local").and_then(Json::as_str)?;
                        let prefix = entry.get("prefixlen").and_then(Json::as_u64)?;
                        let scope = entry
                            .get("scope")
                            .and_then(Json::as_str)
                            .unwrap_or("global");
                        Some(Address {
                            family: match entry.get("family").and_then(Json::as_str) {
                                Some("inet6") => "IPv6",
                                _ => "IPv4",
                            },
                            // Scope distinguishes a routable address from a
                            // link-local one, which matters when reading this.
                            text: format!("{local}/{prefix} ({scope})"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        by_interface.insert(name.to_string(), addresses);
    }
    by_interface
}

/// IPv6 addresses from /proc/net/if_inet6, for when `ip` is absent.
///
/// Columns are: address as 32 hex digits, interface index, prefix length,
/// scope, flags, device name.
fn inet6_addresses() -> BTreeMap<String, Vec<Address>> {
    let Some(text) = sysfs::read("/proc/net/if_inet6") else {
        return BTreeMap::new();
    };

    let mut by_interface: BTreeMap<String, Vec<Address>> = BTreeMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let Some(address) = netutil::ipv6_raw(fields[0]) else {
            continue;
        };
        let prefix = u32::from_str_radix(fields[2], 16).unwrap_or(0);
        let scope = match u32::from_str_radix(fields[3], 16).unwrap_or(0) {
            0x00 => "global",
            0x10 => "host",
            0x20 => "link",
            0x40 => "site",
            _ => "unknown",
        };
        by_interface
            .entry(fields[5].to_string())
            .or_default()
            .push(Address {
                family: "IPv6",
                text: format!("{address}/{prefix} ({scope})"),
            });
    }
    by_interface
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ipv6_from_proc_as_a_fallback() {
        // The fallback must work regardless of whether `ip` is installed,
        // because it is what covers the case where it is not.
        let fallback = inet6_addresses();
        for (interface, addresses) in &fallback {
            assert!(!interface.is_empty());
            for address in addresses {
                assert_eq!(address.family, "IPv6", "if_inet6 holds only IPv6");
                assert!(
                    address.text.contains('/') && address.text.contains('('),
                    "expected address/prefix (scope), got {}",
                    address.text
                );
            }
        }
        // Loopback always has ::1, so the parser is always exercised here.
        if let Some(loopback) = fallback.get("lo") {
            assert!(
                loopback.iter().any(|a| a.text.starts_with("::1/128")),
                "expected ::1 on lo, got {:?}",
                loopback.iter().map(|a| &a.text).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn identifies_loopback_and_wireless() {
        for name in interface_names() {
            let kind = link_kind(&Path::new(NET).join(&name));
            if name == "lo" {
                assert_eq!(kind, "loopback");
            }
            assert!(!kind.is_empty());
        }
    }

    #[test]
    fn collects_every_interface() {
        let nodes = Interfaces.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Network");
        assert_eq!(nodes.len(), interface_names().len() + 1);

        // Link-layer facts come from sysfs and must be present either way.
        for node in nodes.iter().skip(1) {
            assert!(
                node.children.iter().any(|c| c.label == "State"),
                "{} should report a state",
                node.label
            );
            assert!(
                node.children.iter().any(|c| c.label == "Kind"),
                "{} should report a kind",
                node.label
            );
        }
    }
}
