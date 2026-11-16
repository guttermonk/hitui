//! Shared directories: NFS exports and Samba shares.
//!
//! Reports what this machine *offers*, not what it has mounted — mounted
//! remote filesystems are on the Filesystems page. Configured and active are
//! distinguished, because an export listed in /etc/exports that the server
//! never picked up is a common and invisible misconfiguration.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::sysfs;

pub struct Shares;

impl Collector for Shares {
    fn id(&self) -> &'static str {
        "shares"
    }

    fn title(&self) -> &'static str {
        "Shared Directories"
    }

    fn group(&self) -> Group {
        Group::Network
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes = Vec::new();
        if let Some(nfs) = nfs() {
            nodes.push(nfs);
        }
        if let Some(samba) = samba() {
            nodes.push(samba);
        }

        if nodes.is_empty() {
            nodes.push(Node::group(
                "Shared directories",
                vec![Node::field(
                    "Status",
                    "none — no NFS exports and no Samba configuration on this machine",
                )],
            ));
        }
        Ok(nodes)
    }
}

/// NFS exports, both configured and actually active.
fn nfs() -> Option<Node> {
    let mut node = Node::group("NFS exports", vec![]);

    // What the admin wrote.
    if let Some(text) = sysfs::read("/etc/exports") {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // "/srv/share 192.168.1.0/24(rw,sync)" — path then client specs.
            let Some((path, clients)) = line.split_once(char::is_whitespace) else {
                node.push(Node::field(line, "configured, no client restriction"));
                continue;
            };
            node.push(Node::field(
                path,
                format!("configured · {}", clients.trim()),
            ));
        }
    }

    // What the kernel server is actually serving, which can differ.
    if let Some(text) = sysfs::read("/proc/fs/nfsd/exports") {
        let mut active = Node::group("Active in the kernel server", vec![]);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((path, rest)) = line.split_once(char::is_whitespace) {
                active.push(Node::field(path, rest.trim()));
            }
        }
        if !active.children.is_empty() {
            node.push(active);
        }
    }

    if node.children.is_empty() {
        return None;
    }
    // An export file with no running server is worth flagging.
    if !std::path::Path::new("/proc/fs/nfsd").is_dir() {
        node.push(Node::field(
            "Server",
            "not running — /etc/exports is configured but nfsd is not loaded",
        ));
    }
    Some(node)
}

/// Samba shares, from whichever smb.conf the distribution uses.
fn samba() -> Option<Node> {
    let (path, text) = ["/etc/samba/smb.conf", "/etc/smb.conf"]
        .iter()
        .find_map(|path| sysfs::read(path).map(|text| (*path, text)))?;

    let mut node = Node::group("Samba shares", vec![]);
    node.push(Node::field("Configured in", path));

    let mut section: Option<String> = None;
    let mut share = Node::group("", vec![]);

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // "[name]" opens a section; [global] is settings, not a share.
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if let Some(previous) = section.take()
                && !share.children.is_empty()
            {
                share.label = previous;
                node.push(std::mem::replace(&mut share, Node::group("", vec![])));
            }
            share = Node::group("", vec![]);
            if !name.eq_ignore_ascii_case("global") {
                section = Some(name.to_string());
            }
            continue;
        }

        if section.is_some()
            && let Some((key, value)) = line.split_once('=')
        {
            let key = key.trim();
            // The handful of settings that say what a share actually is.
            if matches!(
                key.to_ascii_lowercase().as_str(),
                "path"
                    | "comment"
                    | "browseable"
                    | "browsable"
                    | "read only"
                    | "writable"
                    | "guest ok"
                    | "valid users"
            ) {
                share.push(Node::field(key, value.trim()));
            }
        }
    }
    // The final section has no following header to close it.
    if let Some(name) = section
        && !share.children.is_empty()
    {
        share.label = name;
        node.push(share);
    }

    Some(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_so_plainly_when_nothing_is_shared() {
        // This machine exports nothing, which is the common case and must not
        // render as an empty page.
        let nodes = Shares.collect().expect("collection failed");
        assert!(!nodes.is_empty());
        for node in &nodes {
            assert!(!node.children.is_empty(), "{} is empty", node.label);
        }
    }

    #[test]
    fn parses_samba_sections_into_shares() {
        let conf = "[global]\n\
                    workgroup = WORKGROUP\n\
                    [public]\n\
                    path = /srv/public\n\
                    comment = Public files\n\
                    read only = yes\n\
                    [private]\n\
                    path = /srv/private\n\
                    valid users = greg\n";

        let path = std::env::temp_dir().join("hitui-smb.conf");
        std::fs::write(&path, conf).unwrap();

        // Reuse the parser by pointing it at the fixture.
        let text = std::fs::read_to_string(&path).unwrap();
        let shares = parse_samba(&text);
        std::fs::remove_file(&path).ok();

        assert_eq!(shares.len(), 2, "global is settings, not a share");
        assert_eq!(shares[0].0, "public");
        assert!(
            shares[0]
                .1
                .iter()
                .any(|(k, v)| k == "path" && v == "/srv/public")
        );
        // The last section has no closing header and must still be captured.
        assert_eq!(shares[1].0, "private");
    }

    /// Section name to settings, mirroring what [`samba`] builds.
    fn parse_samba(text: &str) -> Vec<(String, Vec<(String, String)>)> {
        let mut shares = Vec::new();
        let mut section: Option<String> = None;
        let mut settings: Vec<(String, String)> = Vec::new();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                if let Some(previous) = section.take()
                    && !settings.is_empty()
                {
                    shares.push((previous, std::mem::take(&mut settings)));
                }
                settings.clear();
                if !name.eq_ignore_ascii_case("global") {
                    section = Some(name.to_string());
                }
                continue;
            }
            if section.is_some()
                && let Some((key, value)) = line.split_once('=')
            {
                settings.push((key.trim().to_string(), value.trim().to_string()));
            }
        }
        if let Some(name) = section
            && !settings.is_empty()
        {
            shares.push((name, settings));
        }
        shares
    }
}
