//! Locale configuration — hardinfo2's "Languages" page.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::probe;
use crate::sysfs;

pub struct Locales;

impl Collector for Locales {
    fn id(&self) -> &'static str {
        "locales"
    }

    fn title(&self) -> &'static str {
        "Languages"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes = vec![active(), system()];
        if let Some(available) = available() {
            nodes.push(available);
        }
        Ok(nodes)
    }
}

/// The locale this process is actually running under.
fn active() -> Node {
    let mut node = Node::group("Active locale", vec![]);

    // LC_ALL overrides everything; LANG is the fallback for any unset
    // category. Showing the precedence matters because a surprising
    // locale is nearly always one of these two overriding the other.
    for name in ["LC_ALL", "LANG"] {
        node.push_opt(name, std::env::var(name).ok().filter(|v| !v.is_empty()));
    }

    let mut categories = Node::group("Categories", vec![]);
    for name in [
        "LC_CTYPE",
        "LC_NUMERIC",
        "LC_TIME",
        "LC_COLLATE",
        "LC_MONETARY",
        "LC_MESSAGES",
        "LC_PAPER",
        "LC_NAME",
        "LC_ADDRESS",
        "LC_TELEPHONE",
        "LC_MEASUREMENT",
        "LC_IDENTIFICATION",
    ] {
        // Only the categories explicitly overridden; the rest follow LANG and
        // listing twelve copies of it says nothing.
        categories.push_opt(name, std::env::var(name).ok().filter(|v| !v.is_empty()));
    }
    if !categories.children.is_empty() {
        node.push(categories);
    } else {
        node.push(Node::field(
            "Categories",
            "all following LANG — none overridden individually",
        ));
    }
    node
}

/// System-wide defaults, as opposed to this shell's environment.
fn system() -> Node {
    let mut node = Node::group("System default", vec![]);

    // Where the default is configured varies by distribution; all of these
    // are `KEY=value` files.
    for path in [
        "/etc/locale.conf",
        "/etc/default/locale",
        "/etc/sysconfig/i18n",
    ] {
        if let Some(text) = sysfs::read(path) {
            node.push(Node::field("Configured in", path));
            for line in text.lines().filter(|l| l.contains('=')) {
                if let Some((key, value)) = line.split_once('=') {
                    node.push(Node::field(key.trim(), value.trim().trim_matches('"')));
                }
            }
            break;
        }
    }
    if node.children.is_empty() {
        node.push(Node::field(
            "Configured in",
            "no locale.conf — the default comes from the environment",
        ));
    }
    node
}

/// Locales built and installed on the system.
///
/// `locale -a` is the only portable way to ask: distributions store the
/// compiled data differently, and several (NixOS, Debian with locale-archive)
/// keep it packed in an archive with no per-locale directory to list.
fn available() -> Option<Node> {
    let Some(output) = probe::run("locale", &["-a"]) else {
        return Some(Node::group(
            "Available locales",
            vec![Node::field(
                "Status",
                "unknown — `locale` is not installed, and compiled locales have no fixed path",
            )],
        ));
    };

    let mut locales: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    locales.sort_unstable();

    let mut node = Node::group("Available locales", vec![]).from("locale");
    node.push(Node::field("Count", locales.len()));
    // UTF-8 variants are what anything modern should be using; legacy
    // single-byte locales are worth distinguishing.
    let utf8 = locales
        .iter()
        .filter(|l| {
            let upper = l.to_ascii_uppercase();
            upper.contains("UTF-8") || upper.contains("UTF8")
        })
        .count();
    node.push(Node::field("UTF-8 locales", utf8));
    node.push(Node::field(
        "Installed",
        locales
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<String>>(),
    ));
    Some(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_locale_always_reports_something() {
        let node = active();
        assert!(!node.children.is_empty());
        // Either individual overrides are listed, or the page says they all
        // follow LANG. Silence would be ambiguous.
        assert!(
            node.children.iter().any(|c| c.label == "Categories"),
            "categories must be accounted for either way"
        );
    }

    #[test]
    fn system_default_states_where_it_came_from() {
        let node = system();
        assert!(
            node.children.iter().any(|c| c.label == "Configured in"),
            "the source of the default must be named"
        );
    }

    #[test]
    fn collects_on_this_machine() {
        let nodes = Locales.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Active locale");
        assert_eq!(nodes[1].label, "System default");
        assert_eq!(nodes[2].label, "Available locales");
    }
}
