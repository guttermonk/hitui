//! Mounted filesystems and their free space.
//!
//! Capacity is accounted as `df(1)` does: used is total minus free, but the
//! percentage is taken over used plus available-to-us, which excludes the
//! root reserve. That is why a full disk reads 100 % before `f_bfree` hits
//! zero.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::{Node, Value};
use crate::sysfs;

pub struct Filesystems;

impl Collector for Filesystems {
    fn id(&self) -> &'static str {
        "filesystems"
    }

    fn title(&self) -> &'static str {
        "Filesystems"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mounts = mounts();

        let mut real = Node::group("Mounted filesystems", vec![]);
        let mut pseudo = Node::group("Pseudo filesystems", vec![]);

        for mount in &mounts {
            // A filesystem backed by a path is a real one; this single rule
            // separates block devices from proc, sysfs, tmpfs and fuse portals.
            let target = if mount.device.starts_with('/') {
                &mut real
            } else {
                &mut pseudo
            };

            let mut entry = Node::group(&mount.mount_point, vec![]);
            entry.push(Node::field("Device", mount.device.clone()));
            entry.push(Node::field("Type", mount.fs_type.clone()));
            if let Some((used, capacity)) = usage(&mount.mount_point) {
                entry.push(Node::field("Usage", Value::bytes_gauge(used, capacity)));
            }
            // Read-only and the other headline flags, not the whole option list.
            entry.push(Node::field("Options", mount.options.clone()));
            target.push(entry);
        }

        let mut nodes = Vec::new();
        if !real.children.is_empty() {
            nodes.push(real);
        }
        if !pseudo.children.is_empty() {
            nodes.push(pseudo);
        }
        Ok(nodes)
    }
}

pub struct Mount {
    pub device: String,
    pub mount_point: String,
    pub fs_type: String,
    pub options: String,
}

/// Parse /proc/self/mounts.
pub fn mounts() -> Vec<Mount> {
    let Some(text) = sysfs::read("/proc/self/mounts") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some(Mount {
                device: unescape_mount(fields.next()?),
                mount_point: unescape_mount(fields.next()?),
                fs_type: fields.next()?.to_string(),
                options: fields.next().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

/// Used and chargeable capacity in bytes, or `None` if it cannot be statted.
pub fn usage(mount_point: &str) -> Option<(u64, u64)> {
    let stat = rustix::fs::statvfs(mount_point).ok()?;
    let block = stat.f_frsize;
    let used = stat.f_blocks.saturating_sub(stat.f_bfree) * block;
    let capacity = used + stat.f_bavail * block;
    (capacity > 0).then_some((used, capacity))
}

/// /proc/self/mounts octal-escapes the characters that would break its format.
fn unescape_mount(path: &str) -> String {
    if !path.contains('\\') {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let octal: String = chars.clone().take(3).collect();
        match u32::from_str_radix(&octal, 8).ok().and_then(char::from_u32) {
            Some(decoded) => {
                out.push(decoded);
                chars.nth(2);
            }
            None => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_octal_mount_points() {
        assert_eq!(unescape_mount("/mnt/my\\040disk"), "/mnt/my disk");
        assert_eq!(unescape_mount("/plain/path"), "/plain/path");
        // A stray backslash must survive rather than eat the next characters.
        assert_eq!(unescape_mount("/odd\\path"), "/odd\\path");
    }

    #[test]
    fn root_is_present_and_statbable() {
        let mounts = mounts();
        assert!(!mounts.is_empty(), "/proc/self/mounts should parse");
        assert!(
            mounts.iter().any(|m| m.mount_point == "/"),
            "expected a root mount"
        );
        let (used, capacity) = usage("/").expect("root should stat");
        assert!(capacity > 0 && used <= capacity);
    }

    #[test]
    fn collects_on_this_machine() {
        let nodes = Filesystems.collect().expect("collection failed");
        assert_eq!(nodes[0].label, "Mounted filesystems");
        assert!(!nodes[0].children.is_empty());

        // Real filesystems are separated from the pseudo ones, and the real
        // side is where a usage gauge can actually be computed.
        let root = nodes[0]
            .children
            .iter()
            .find(|m| m.label == "/")
            .expect("root should be listed as a real filesystem");
        assert!(root.children.iter().any(|f| f.label == "Usage"));
    }
}
