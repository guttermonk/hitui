//! Operating system identity: distribution, kernel, init, uptime.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::{Node, format_duration};
use crate::sysfs;

pub struct OperatingSystem;

impl Collector for OperatingSystem {
    fn id(&self) -> &'static str {
        "os"
    }

    fn title(&self) -> &'static str {
        "Operating System"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        Ok(vec![distribution(), kernel(), runtime()])
    }
}

fn distribution() -> Node {
    let mut node = Node::group("Distribution", vec![]);
    node.push_opt("Name", os_release("PRETTY_NAME").or(os_release("NAME")));
    node.push_opt(
        "Version",
        os_release("VERSION").or(os_release("VERSION_ID")),
    );
    node.push_opt("Identifier", os_release("ID"));
    node.push_opt("Build", os_release("BUILD_ID"));
    node.push_opt("Variant", os_release("VARIANT"));
    node.push_opt("Home page", os_release("HOME_URL"));
    node
}

fn kernel() -> Node {
    let mut node = Node::group("Kernel", vec![]);
    node.push_opt("Release", sysfs::read("/proc/sys/kernel/osrelease"));
    node.push_opt("Version", sysfs::read("/proc/sys/kernel/version"));
    node.push(Node::field("Architecture", std::env::consts::ARCH));
    node.push_opt("Host name", sysfs::read("/proc/sys/kernel/hostname"));
    node.push_opt("Domain name", domain_name());

    // The command line is where boot-time configuration actually lives, and
    // the usual reason a kernel behaves unlike its defaults.
    node.push_opt("Command line", sysfs::read("/proc/cmdline"));
    node
}

/// NIS/DNS domain, skipping the `(none)` the kernel reports when unset.
fn domain_name() -> Option<String> {
    sysfs::read("/proc/sys/kernel/domainname").filter(|name| name != "(none)")
}

fn runtime() -> Node {
    let mut node = Node::group("Runtime", vec![]);

    // PID 1's own name is the reliable answer; the presence of
    // /run/systemd/system additionally distinguishes "systemd is installed"
    // from "systemd booted this machine".
    node.push_opt("Init system", sysfs::read("/proc/1/comm"));
    if std::path::Path::new("/run/systemd/system").is_dir() {
        node.push(Node::field("systemd", "booted this session"));
    }

    if let Some(raw) = sysfs::read("/proc/uptime") {
        let mut fields = raw.split_whitespace();
        if let Some(seconds) = fields.next().and_then(|s| s.parse::<f64>().ok()) {
            node.push(Node::field("Uptime", format_duration(seconds as u64)));
        }
    }

    // Processes and threads currently alive, from the same line as load.
    if let Some(load) = sysfs::read("/proc/loadavg") {
        if let Some(entities) = load.split_whitespace().nth(3)
            && let Some((runnable, total)) = entities.split_once('/')
        {
            node.push(Node::field("Runnable entities", runnable));
            node.push(Node::field("Total entities", total));
        }
        let averages: Vec<&str> = load.split_whitespace().take(3).collect();
        if averages.len() == 3 {
            node.push(Node::field("Load average", averages.join(" · ")));
        }
    }

    // A container or VM changes how everything else on this page should be
    // read, so it is worth stating plainly.
    node.push(Node::field("Containerised", container().unwrap_or("no")));
    node
}

/// Detect the obvious container runtimes.
///
/// These are the markers the runtimes themselves document; there is no kernel
/// interface that answers the question directly.
fn container() -> Option<&'static str> {
    if std::path::Path::new("/.dockerenv").exists() {
        return Some("yes — Docker");
    }
    if std::path::Path::new("/run/.containerenv").exists() {
        return Some("yes — Podman");
    }
    // systemd-nspawn and most OCI runtimes set this for PID 1.
    if let Some(environ) = std::fs::read("/proc/1/environ").ok()
        && String::from_utf8_lossy(&environ).contains("container=")
    {
        return Some("yes");
    }
    None
}

/// Read a key out of os-release, which is shell-quoted.
pub fn os_release(key: &str) -> Option<String> {
    let text = sysfs::read("/etc/os-release").or_else(|| sysfs::read("/usr/lib/os-release"))?;
    let prefix = format!("{key}=");
    let value = text.lines().find_map(|line| line.strip_prefix(&prefix))?;
    Some(value.trim_matches('"').to_string()).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_quoted_os_release_values() {
        // Every Linux system ships one of the two paths.
        if let Some(name) = os_release("PRETTY_NAME") {
            assert!(!name.starts_with('"'), "quotes should be stripped: {name}");
            assert!(!name.is_empty());
        }
        assert!(os_release("HITUI_NOT_A_REAL_KEY").is_none());
    }

    #[test]
    fn unset_domain_name_is_omitted() {
        // The kernel reports the literal "(none)", which is not a domain.
        if let Some(domain) = domain_name() {
            assert_ne!(domain, "(none)");
        }
    }

    #[test]
    fn collects_on_this_machine() {
        let nodes = OperatingSystem.collect().expect("collection failed");
        let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labels, vec!["Distribution", "Kernel", "Runtime"]);

        let kernel = &nodes[1];
        assert!(kernel.children.iter().any(|c| c.label == "Release"));
        assert!(kernel.children.iter().any(|c| c.label == "Architecture"));

        // Containerisation is always stated, never left to inference.
        assert!(
            nodes[2].children.iter().any(|c| c.label == "Containerised"),
            "container status must always be reported"
        );
    }
}
