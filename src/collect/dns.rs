//! Name resolution configuration.
//!
//! /etc/resolv.conf is often not the real answer any more: systemd-resolved,
//! dnsmasq and NetworkManager all install a stub resolver there pointing at
//! themselves. The page reports what the file says *and* notices when it is a
//! stub, because "nameserver 127.0.0.53" is not the server your queries
//! actually reach.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::probe;
use crate::sysfs;

pub struct Dns;

impl Collector for Dns {
    fn id(&self) -> &'static str {
        "dns"
    }

    fn title(&self) -> &'static str {
        "DNS Servers"
    }

    fn group(&self) -> Group {
        Group::Network
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes = vec![resolv_conf()];
        if let Some(upstream) = resolved() {
            nodes.push(upstream);
        }
        Ok(nodes)
    }
}

fn resolv_conf() -> Node {
    let Some(text) = sysfs::read("/etc/resolv.conf") else {
        return Node::group(
            "Resolver configuration",
            vec![Node::field(
                "Status",
                "/etc/resolv.conf is absent — resolution may go through nsswitch only",
            )],
        );
    };

    let mut node = Node::group("Resolver configuration", vec![]);
    let mut servers = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((directive, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let value = value.trim();

        match directive {
            "nameserver" => servers.push(value.to_string()),
            "search" | "domain" => node.push(Node::field("Search domains", value)),
            "options" => node.push(Node::field("Options", value)),
            "sortlist" => node.push(Node::field("Sort list", value)),
            _ => {}
        }
    }

    for (index, server) in servers.iter().enumerate() {
        // Order is significant: the resolver tries them in sequence.
        node.push(Node::field(
            format!("Nameserver {}", index + 1),
            server.clone(),
        ));
    }

    // A loopback nameserver means a local stub is in the path, and the
    // addresses above are not where queries end up.
    let stub = servers.iter().any(|s| is_loopback(s));
    node.push(Node::field(
        "Local stub resolver",
        if stub {
            "yes — queries go to a resolver on this machine, not to these addresses"
        } else {
            "no — these addresses are queried directly"
        },
    ));
    node
}

fn is_loopback(address: &str) -> bool {
    address
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// The upstream servers a local stub is actually forwarding to.
fn resolved() -> Option<Node> {
    let output = probe::run("resolvectl", &["status"])?;

    let mut node = Node::group("Upstream servers", vec![]).from("resolvectl");
    let mut link = None;

    for line in output.lines() {
        let trimmed = line.trim();

        // Output is grouped by link, with indented key/value pairs beneath.
        if let Some(name) = trimmed.strip_prefix("Link ") {
            link = Some(name.trim().to_string());
            continue;
        }
        for (label, prefix) in [
            ("Current DNS server", "Current DNS Server:"),
            ("DNS servers", "DNS Servers:"),
            ("DNS domain", "DNS Domain:"),
            ("DNSSEC", "DNSSEC setting:"),
            ("DNS over TLS", "DefaultRoute setting:"),
        ] {
            if let Some(value) = trimmed.strip_prefix(prefix) {
                let value = value.trim();
                if value.is_empty() {
                    continue;
                }
                node.push(Node::field(
                    match &link {
                        Some(link) => format!("{label} ({link})"),
                        None => format!("{label} (global)"),
                    },
                    value,
                ));
            }
        }
    }

    if node.children.is_empty() {
        return None;
    }
    Some(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_loopback_nameservers() {
        // systemd-resolved's stub, and the other common local resolvers.
        assert!(is_loopback("127.0.0.53"));
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("::1"));
        assert!(!is_loopback("192.168.1.1"));
        assert!(!is_loopback("8.8.8.8"));
        // Not an address at all.
        assert!(!is_loopback("example.com"));
    }

    #[test]
    fn stub_presence_is_always_stated() {
        let node = resolv_conf();
        let stub = node
            .children
            .iter()
            .find(|c| c.label == "Local stub resolver" || c.label == "Status")
            .expect("either the stub verdict or why the file is missing");
        assert!(!stub.value.as_ref().unwrap().to_string().is_empty());
    }

    #[test]
    fn nameservers_are_numbered_in_order() {
        let node = resolv_conf();
        let numbered: Vec<&str> = node
            .children
            .iter()
            .filter(|c| c.label.starts_with("Nameserver"))
            .map(|c| c.label.as_str())
            .collect();

        // Order is how the resolver tries them, so it has to be preserved.
        for (index, label) in numbered.iter().enumerate() {
            assert_eq!(*label, format!("Nameserver {}", index + 1));
        }
    }

    #[test]
    fn collects_on_this_machine() {
        let nodes = Dns.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Resolver configuration");
        assert!(!nodes[0].children.is_empty());
    }
}
