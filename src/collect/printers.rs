//! Printers known to CUPS.
//!
//! There is no kernel interface for this: printers are a CUPS concept, and
//! CUPS answers over IPP. Rather than speak IPP, hitui asks `lpstat`, which
//! ships with CUPS itself — so if there is a print system to query, the tool
//! to query it is present too.
//!
//! This is the slowest page, and unavoidably so where cupsd is running:
//! `lpstat -p` browses for network printers and takes about a second to
//! finish doing it, even with no queues configured. Machines with no cupsd
//! are short-circuited on the socket check and cost nothing.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::probe;

pub struct Printers;

impl Collector for Printers {
    fn id(&self) -> &'static str {
        "printers"
    }

    fn title(&self) -> &'static str {
        "Printers"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        // Asking lpstat anything when no daemon is listening costs a second
        // per call while it waits for a connection that will never come, so
        // the socket is checked first.
        if !daemon_running() {
            return Ok(vec![Node::group(
                "Print system",
                vec![Node::field(
                    "CUPS",
                    if probe::available("lpstat") {
                        "client tools installed, but cupsd is not running"
                    } else {
                        "not installed"
                    },
                )],
            )]);
        }

        let mut nodes = vec![print_system()];
        if let Some(queues) = queues() {
            nodes.push(queues);
        }
        Ok(nodes)
    }
}

/// Whether there is a cupsd to talk to.
///
/// A listening socket is the precondition for every lpstat query; without one
/// the tool blocks until it gives up.
fn daemon_running() -> bool {
    ["/run/cups/cups.sock", "/var/run/cups/cups.sock"]
        .iter()
        .any(|socket| std::path::Path::new(socket).exists())
}

/// Whether a print system is installed and running at all.
fn print_system() -> Node {
    let mut node = Node::group("Print system", vec![]);

    let configured = ["/etc/cups/cupsd.conf", "/etc/cups/client.conf"]
        .iter()
        .any(|path| std::path::Path::new(path).exists());

    node.push(Node::field(
        "CUPS",
        if configured {
            "configured"
        } else if probe::available("lpstat") {
            "client tools present, no local configuration"
        } else {
            "not installed"
        },
    ));
    node.push_opt(
        "Default destination",
        probe::run("lpstat", &["-d"]).and_then(|output| {
            // "system default destination: name", or a line saying there is none.
            let line = output.lines().next()?.trim().to_string();
            line.split_once(": ")
                .map(|(_, name)| name.to_string())
                .or(Some(line))
        }),
    );
    node
}

/// Configured queues and their state.
fn queues() -> Option<Node> {
    let Some(output) = probe::run("lpstat", &["-p"]) else {
        return Some(Node::group(
            "Queues",
            vec![Node::field(
                "Status",
                if probe::available("lpstat") {
                    // lpstat prints nothing at all when no queue exists.
                    "none configured"
                } else {
                    "unknown — install CUPS for printer queues"
                },
            )],
        ));
    };

    let mut node = Node::group("Queues", vec![]).from("lpstat");
    for line in output.lines() {
        let line = line.trim();
        // "printer NAME is idle.  enabled since ..." — the name is the second
        // word, and the rest is the state.
        let Some(rest) = line.strip_prefix("printer ") else {
            continue;
        };
        let Some((name, state)) = rest.split_once(" is ") else {
            continue;
        };
        node.push(Node::field(
            name,
            state.trim_end_matches('.').replace(".  ", " · "),
        ));
    }

    if node.children.is_empty() {
        node.push(Node::field("Status", "none configured"));
    }
    Some(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_system_state_is_always_reported() {
        let node = print_system();
        let cups = node
            .children
            .iter()
            .find(|c| c.label == "CUPS")
            .expect("CUPS state must be stated");
        assert!(!cups.value.as_ref().unwrap().to_string().is_empty());
    }

    #[test]
    fn an_absent_print_system_is_not_an_error() {
        // Most machines have no printer, which must render as a statement
        // rather than an empty page or a failure.
        let nodes = Printers.collect().expect("collection failed");
        assert!(!nodes.is_empty());
        for node in &nodes {
            assert!(!node.children.is_empty(), "{} is empty", node.label);
        }
    }

    #[test]
    fn no_daemon_means_no_lpstat_calls() {
        // The page must not spend a second per query waiting on a connection
        // that cannot succeed.
        let start = std::time::Instant::now();
        let _ = Printers.collect().expect("collection failed");
        if !daemon_running() {
            assert!(
                start.elapsed() < std::time::Duration::from_millis(200),
                "took {:?} with no daemon to talk to",
                start.elapsed()
            );
        }
    }

    #[test]
    fn parses_an_lpstat_queue_line() {
        // The shape lpstat -p emits, which has to survive the split.
        let line = "printer Brother_HL_L2340D is idle.  enabled since Mon 01 Jan 2024";
        let rest = line.strip_prefix("printer ").unwrap();
        let (name, state) = rest.split_once(" is ").unwrap();

        assert_eq!(name, "Brother_HL_L2340D");
        assert!(state.starts_with("idle"), "{state}");
    }
}
