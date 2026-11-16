//! Open sockets, from /proc/net/{tcp,tcp6,udp,udp6}.
//!
//! These files list sockets system-wide, but the owning process is not in
//! them — only the socket's inode, which would have to be matched against
//! every /proc/*/fd entry to name a process. That requires privilege for
//! other users' processes, so this page reports the sockets and their owning
//! uid and leaves process attribution to `ss -p`.

use anyhow::Result;

use crate::collect::netutil;
use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::sysfs;

pub struct Connections;

impl Collector for Connections {
    fn id(&self) -> &'static str {
        "connections"
    }

    fn title(&self) -> &'static str {
        "IP Connections"
    }

    fn group(&self) -> Group {
        Group::Network
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let tcp: Vec<Socket> = ["/proc/net/tcp", "/proc/net/tcp6"]
            .iter()
            .flat_map(|path| sockets(path, true))
            .collect();
        let udp: Vec<Socket> = ["/proc/net/udp", "/proc/net/udp6"]
            .iter()
            .flat_map(|path| sockets(path, false))
            .collect();

        let listening = tcp.iter().filter(|s| s.state == "LISTEN").count();
        let established = tcp.iter().filter(|s| s.state == "ESTABLISHED").count();

        let mut overview = Node::group("Connections", vec![]);
        overview.push(Node::field("TCP sockets", tcp.len()));
        overview.push(Node::field("Listening", listening));
        overview.push(Node::field("Established", established));
        overview.push(Node::field("UDP sockets", udp.len()));

        let mut nodes = vec![overview];

        // Listening sockets are the interesting ones — they are the machine's
        // attack surface — so they lead rather than being mixed in by number.
        let mut listeners = Node::group("Listening (TCP)", vec![]);
        for socket in tcp.iter().filter(|s| s.state == "LISTEN") {
            listeners.push(Node::field(&socket.local, socket.summary()));
        }
        if !listeners.children.is_empty() {
            nodes.push(listeners);
        }

        let mut active = Node::group("Established (TCP)", vec![]);
        for socket in tcp.iter().filter(|s| s.state == "ESTABLISHED") {
            active.push(Node::field(
                format!("{} → {}", socket.local, socket.remote),
                socket.summary(),
            ));
        }
        if !active.children.is_empty() {
            nodes.push(active);
        }

        // Everything else: TIME_WAIT, CLOSE_WAIT and the other transient
        // states, which matter when diagnosing socket exhaustion.
        let mut other = Node::group("Other TCP states", vec![]);
        for socket in tcp
            .iter()
            .filter(|s| s.state != "LISTEN" && s.state != "ESTABLISHED")
        {
            other.push(Node::field(
                format!("{} → {}", socket.local, socket.remote),
                socket.summary(),
            ));
        }
        if !other.children.is_empty() {
            nodes.push(other);
        }

        let mut datagrams = Node::group("UDP", vec![]);
        for socket in &udp {
            datagrams.push(Node::field(&socket.local, socket.summary()));
        }
        if !datagrams.children.is_empty() {
            nodes.push(datagrams);
        }

        nodes.push(Node::group(
            "Source",
            vec![Node::field(
                "Process names",
                "not shown — /proc/net lists socket inodes, and matching them to \
                 processes needs privilege. Use `ss -p` for that.",
            )],
        ));
        Ok(nodes)
    }
}

struct Socket {
    local: String,
    remote: String,
    state: &'static str,
    uid: String,
    /// Bytes queued for transmit and receive.
    queues: Option<(u64, u64)>,
}

impl Socket {
    fn summary(&self) -> String {
        let mut parts = vec![self.state.to_string()];
        parts.push(format!("uid {}", self.uid));

        // A non-empty queue is a symptom; two zeroes every line is noise.
        if let Some((tx, rx)) = self.queues
            && (tx > 0 || rx > 0)
        {
            parts.push(format!("tx queue {tx}, rx queue {rx}"));
        }
        parts.join(" · ")
    }
}

fn sockets(path: &str, tcp: bool) -> Vec<Socket> {
    let Some(text) = sysfs::read(path) else {
        return Vec::new();
    };

    text.lines()
        .skip(1) // Column header.
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 8 {
                return None;
            }
            // sl local rem st tx:rx tr tm retrnsmt uid …
            let local = netutil::endpoint(fields[1])?;
            let remote = netutil::endpoint(fields[2])?;
            let state = if tcp {
                tcp_state(fields[3])
            } else {
                // UDP is connectionless; the state column is meaningful only
                // in that 07 means unconnected.
                if fields[3] == "07" {
                    "unconnected"
                } else {
                    "connected"
                }
            };
            let queues = fields[4].split_once(':').and_then(|(tx, rx)| {
                Some((
                    u64::from_str_radix(tx, 16).ok()?,
                    u64::from_str_radix(rx, 16).ok()?,
                ))
            });
            Some(Socket {
                local,
                remote,
                state,
                uid: fields[7].to_string(),
                queues,
            })
        })
        .collect()
}

/// TCP state names, from the kernel's `tcp_states.h` enumeration.
fn tcp_state(hex: &str) -> &'static str {
    match hex {
        "01" => "ESTABLISHED",
        "02" => "SYN_SENT",
        "03" => "SYN_RECV",
        "04" => "FIN_WAIT1",
        "05" => "FIN_WAIT2",
        "06" => "TIME_WAIT",
        "07" => "CLOSE",
        "08" => "CLOSE_WAIT",
        "09" => "LAST_ACK",
        "0A" | "0a" => "LISTEN",
        "0B" | "0b" => "CLOSING",
        "0C" | "0c" => "NEW_SYN_RECV",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_every_tcp_state() {
        assert_eq!(tcp_state("01"), "ESTABLISHED");
        assert_eq!(tcp_state("0A"), "LISTEN");
        // The kernel writes these upper case, but be tolerant either way.
        assert_eq!(tcp_state("0a"), "LISTEN");
        assert_eq!(tcp_state("06"), "TIME_WAIT");
        assert_eq!(tcp_state("ff"), "unknown");
    }

    #[test]
    fn parses_a_real_tcp_line() {
        let text = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid\n   \
                    0: 0100007F:2350 00000000:0000 0A 00000000:00000000 00:00000000 00000000    35\n";
        let parsed = sockets_from("listen-line", text, true);

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].local, "127.0.0.1:9040");
        assert_eq!(parsed[0].state, "LISTEN");
        assert_eq!(parsed[0].uid, "35");
        // Both queues empty, so the summary should not mention them.
        assert!(
            !parsed[0].summary().contains("queue"),
            "{}",
            parsed[0].summary()
        );
    }

    #[test]
    fn non_empty_queues_are_surfaced() {
        let text = "header\n 0: 0100007F:2350 00000000:0000 01 0000002A:00000010 00:0 0 1000\n";
        let parsed = sockets_from("queue-depth", text, true);
        let summary = parsed[0].summary();

        // 0x2A is 42 and 0x10 is 16; a backed-up queue is a symptom worth
        // seeing rather than hiding behind hex.
        assert!(summary.contains("tx queue 42"), "{summary}");
        assert!(summary.contains("rx queue 16"), "{summary}");
    }

    /// Parse from text rather than a path, for the tests above.
    /// `fixture` must be unique per test: these run in parallel, so a shared
    /// path means one test overwrites another's input and both then assert
    /// against whichever write landed last.
    fn sockets_from(fixture: &str, text: &str, tcp: bool) -> Vec<Socket> {
        let path = std::env::temp_dir().join(format!("hitui-net-{fixture}"));
        std::fs::write(&path, text).unwrap();
        let parsed = sockets(path.to_str().unwrap(), tcp);
        std::fs::remove_file(&path).ok();
        parsed
    }

    #[test]
    fn collects_on_this_machine() {
        let nodes = Connections.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Connections");
        // The last section explains the absent process names rather than
        // leaving the reader to wonder.
        assert_eq!(nodes.last().unwrap().label, "Source");
    }

    #[test]
    fn listening_sockets_come_first() {
        let nodes = Connections.collect().expect("collection failed");
        let order: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();

        if let (Some(listen), Some(established)) = (
            order.iter().position(|l| *l == "Listening (TCP)"),
            order.iter().position(|l| *l == "Established (TCP)"),
        ) {
            assert!(listen < established, "{order:?}");
        }
    }
}
