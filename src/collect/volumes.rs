//! Arrays and logical volumes: what sits on top of the physical disks.
//!
//! The Storage page walks disks upward — here is a drive, here is what its
//! partitions are used for. This one walks volumes downward, which is the
//! question that matters once there is more than one layer between a
//! filesystem and a disk. A btrfs filesystem on a LUKS mapping on a partition
//! on an NVMe drive is four deep, and an LV on a VG on a PV on a LUKS mapping
//! on a partition on a disk is six; from either end that reads as a list of
//! unrelated block devices unless something renders the stack as a stack.
//!
//! All of it comes from sysfs, which matters more here than elsewhere: LVM's
//! own tools want `/dev/mapper/control` and report almost nothing as an
//! ordinary user, while `/sys/block/*/dm/uuid` identifies the same logical
//! volumes with no privilege at all. The same holds for btrfs, whose RAID
//! profiles are a block-group flag word under `/sys/fs/btrfs` rather than
//! something only `btrfs filesystem df` can say.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use crate::collect::storage::mapping_kind;
use crate::collect::{Collector, Group};
use crate::model::{Node, Value, format_bytes};
use crate::sysfs;

const BLOCK: &str = "/sys/block";
const BTRFS: &str = "/sys/fs/btrfs";
/// Depth cap for the stack walk. Nothing legitimate nests this far; the cap
/// is here so a cycle in sysfs cannot hang a page.
const MAX_DEPTH: usize = 8;

pub struct Volumes;

impl Collector for Volumes {
    fn id(&self) -> &'static str {
        "volumes"
    }

    fn title(&self) -> &'static str {
        "Volumes"
    }

    fn group(&self) -> Group {
        Group::Devices
    }

    fn refresh_interval(&self) -> Option<Duration> {
        // An array rebuilding is the one thing on this page whose current
        // value is the reason you opened it.
        Some(Duration::from_secs(5))
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes = Vec::new();
        nodes.extend(md_arrays());
        nodes.extend(volume_groups());
        nodes.extend(btrfs_filesystems());
        nodes.extend(mappings());

        if nodes.is_empty() {
            let mut node = Node::group("Volumes", vec![]);
            node.push(Node::field(
                "Configured",
                "none — no arrays, logical volumes or mappings; storage is used directly",
            ));
            nodes.push(node);
        }
        Ok(nodes)
    }
}

// ─── MD arrays ───────────────────────────────────────────────────────────────

/// Software RAID arrays, read from `/sys/block/mdN/md`.
///
/// Not `/proc/mdstat`: it is a free-form text summary assembled for people,
/// and every fact in it is a separate sysfs attribute that needs no parsing.
fn md_arrays() -> Vec<Node> {
    md_arrays_in(Path::new(BLOCK))
}

/// The scan root is a parameter so the whole section can be rendered against
/// a synthetic sysfs. A degraded array is the most important thing this page
/// ever says and the hardest to arrange on a machine that has none.
fn md_arrays_in(block: &Path) -> Vec<Node> {
    let mut out = Vec::new();
    for device in sysfs::list_dir(block) {
        let md = device.join("md");
        if !md.is_dir() {
            continue;
        }
        let name = file_name(&device);
        let level = sysfs::read(md.join("level")).unwrap_or_else(|| "unknown".to_string());

        let mut node = Node::group(format!("{name}  {level}"), vec![]);
        node.push_opt("Size", size_of(&device).map(format_bytes));
        node.push_opt("Metadata", sysfs::read(md.join("metadata_version")));
        node.push_opt(
            "Chunk",
            sysfs::read_u64(md.join("chunk_size")).map(format_bytes),
        );

        // The headline: an array missing a member still works, which is
        // exactly why nobody notices until the second one goes.
        let degraded = sysfs::read_u64(md.join("degraded")).unwrap_or(0);
        let total = sysfs::read_u64(md.join("raid_disks")).unwrap_or(0);
        node.push(Node::field(
            "Health",
            if degraded > 0 {
                format!(
                    "DEGRADED — {degraded} of {total} member{} missing or failed",
                    if total == 1 { "" } else { "s" }
                )
            } else {
                format!("{total} of {total} members present")
            },
        ));
        node.push_opt("State", sysfs::read(md.join("array_state")));

        // Resync progress, which is the reason this page refreshes.
        if let Some(action) = sysfs::read(md.join("sync_action")).filter(|a| a != "idle") {
            node.push(Node::field("Rebuild", action));
            if let Some(progress) = sync_progress(&md) {
                node.push(Node::field(
                    "Progress",
                    Value::gauge(progress, format!("{progress:.1} %")),
                ));
            }
        }
        // Non-zero after a check means the two copies disagreed, which is the
        // only way a silent corruption on a mirror ever announces itself.
        node.push_opt(
            "Mismatches",
            sysfs::read_u64(md.join("mismatch_cnt")).map(|n| Value::Int(n as i64)),
        );

        let mut members = Node::group("Members", vec![]);
        for member in sysfs::list_dir(&md) {
            let Some(device_name) = file_name(&member).strip_prefix("dev-").map(str::to_string)
            else {
                continue;
            };
            let state = sysfs::read(member.join("state")).unwrap_or_default();
            let slot = sysfs::read(member.join("slot")).unwrap_or_default();
            members.push(Node::field(
                device_name,
                if slot == "none" || slot.is_empty() {
                    state
                } else {
                    format!("{state} (slot {slot})")
                },
            ));
        }
        if !members.children.is_empty() {
            node.push(members);
        }
        out.push(node);
    }
    out
}

/// Rebuild progress as a percentage.
///
/// `sync_completed` reads `<done> / <total>` in sectors, or `none` when
/// nothing is running.
fn sync_progress(md: &Path) -> Option<f64> {
    let raw = sysfs::read(md.join("sync_completed"))?;
    let (done, total) = raw.split_once('/')?;
    let done: f64 = done.trim().parse().ok()?;
    let total: f64 = total.trim().parse().ok()?;
    (total > 0.0).then(|| done / total * 100.0)
}

// ─── LVM ─────────────────────────────────────────────────────────────────────

/// Volume groups, assembled from the logical volumes the kernel exposes.
///
/// There is no volume group in sysfs — only the LVs, each carrying its group
/// in its device-mapper name. Grouping them back up is what makes "this VG
/// spans these two disks" visible without `vgs`, which wants root.
fn volume_groups() -> Vec<Node> {
    volume_groups_in(Path::new(BLOCK))
}

fn volume_groups_in(block: &Path) -> Vec<Node> {
    // Volume group name -> (logical volume name, device path).
    let mut groups: Vec<(String, Vec<(String, PathBuf)>)> = Vec::new();

    for device in sysfs::list_dir(block) {
        let Some(uuid) = sysfs::read(device.join("dm/uuid")) else {
            continue;
        };
        if !uuid.starts_with("LVM") {
            continue;
        }
        let Some(name) = sysfs::read(device.join("dm/name")) else {
            continue;
        };
        let Some((group, volume)) = split_dm_name(&name) else {
            continue;
        };
        match groups.iter_mut().find(|(existing, _)| *existing == group) {
            Some((_, volumes)) => volumes.push((volume, device)),
            None => groups.push((group, vec![(volume, device)])),
        }
    }

    groups
        .into_iter()
        .map(|(group, volumes)| {
            let mut node = Node::group(format!("{group}  volume group"), vec![]);

            // The physical volumes are whatever the logical volumes sit on,
            // counted once each however many LVs touch them.
            let mut physical: Vec<String> = Vec::new();
            for (_, device) in &volumes {
                for slave in sysfs::list_dir(device.join("slaves")) {
                    let name = file_name(&slave);
                    if !physical.contains(&name) {
                        physical.push(name);
                    }
                }
            }
            if !physical.is_empty() {
                node.push(Node::field("Physical volumes", Value::List(physical)));
            }

            for (volume, device) in volumes {
                let mut logical = Node::group(volume, vec![]);
                logical.push_opt("Size", size_of(&device).map(format_bytes));
                logical.push(Node::field("Device", file_name(&device)));
                for layer in underneath(&device) {
                    logical.push(layer);
                }
                node.push(logical);
            }
            node
        })
        .collect()
}

/// Split a device-mapper name into its volume group and logical volume.
///
/// LVM joins the two with a hyphen and escapes any hyphen in either by
/// doubling it, so `vg--one-lv--two` is `vg-one` and `lv-two`. Splitting on
/// the first hyphen gets that wrong in exactly the case where a reader would
/// not notice.
fn split_dm_name(name: &str) -> Option<(String, String)> {
    let characters: Vec<char> = name.chars().collect();
    let mut group = String::new();
    let mut index = 0;

    while index < characters.len() {
        if characters[index] == '-' {
            if characters.get(index + 1) == Some(&'-') {
                group.push('-');
                index += 2;
                continue;
            }
            let volume: String = characters[index + 1..].iter().collect();
            return Some((group, volume.replace("--", "-")));
        }
        group.push(characters[index]);
        index += 1;
    }
    None
}

// ─── btrfs ───────────────────────────────────────────────────────────────────

/// btrfs filesystems, which carry their own redundancy and span their own
/// devices rather than sitting on an array.
fn btrfs_filesystems() -> Vec<Node> {
    btrfs_filesystems_in(Path::new(BTRFS))
}

fn btrfs_filesystems_in(root: &Path) -> Vec<Node> {
    let mut out = Vec::new();
    for filesystem in sysfs::list_dir(root) {
        // `features` sits alongside the filesystems and is not one.
        if !filesystem.join("devices").is_dir() {
            continue;
        }
        let uuid = file_name(&filesystem);
        let label = sysfs::read(filesystem.join("label")).filter(|l| !l.is_empty());

        let title = match &label {
            Some(label) => format!("btrfs \"{label}\""),
            None => format!("btrfs {uuid}"),
        };
        let mut node = Node::group(title, vec![]).from(BTRFS);
        node.push(Node::field("UUID", uuid));

        // The profiles are the redundancy: a two-disk filesystem with data
        // single and metadata raid1 loses half its data to one failure, and
        // nothing else on this page would say so.
        for group in ["data", "metadata", "system"] {
            node.push_opt(
                group_label(group),
                sysfs::read_u64(filesystem.join("allocation").join(group).join("flags"))
                    .map(|flags| profile(flags).to_string()),
            );
        }
        node.push_opt(
            "Allocated",
            sysfs::read_u64(filesystem.join("allocation/data/total_bytes")).map(format_bytes),
        );
        node.push_opt(
            "Node size",
            sysfs::read_u64(filesystem.join("nodesize")).map(format_bytes),
        );
        node.push_opt("Checksum", sysfs::read(filesystem.join("checksum")));

        let mut devices = Node::group("Devices", vec![]);
        for device in sysfs::list_dir(filesystem.join("devices")) {
            let resolved = std::fs::canonicalize(&device).unwrap_or(device);
            // The member itself, not just what is under it: on an encrypted
            // filesystem the member *is* the mapping, and leaving it out
            // showed btrfs sitting straight on a partition it never touches.
            devices.push(device_tree(&resolved, 0));
        }
        if !devices.children.is_empty() {
            node.push(devices);
        }
        out.push(node);
    }
    out
}

fn group_label(group: &str) -> String {
    let mut label = group.to_string();
    label[..1].make_ascii_uppercase();
    format!("{label} profile")
}

/// The RAID profile encoded in a btrfs block-group flag word.
///
/// The low three bits say which kind of block group it is; the profile is the
/// bit above them, and no profile bit at all means `single`.
fn profile(flags: u64) -> &'static str {
    const RAID0: u64 = 1 << 3;
    const RAID1: u64 = 1 << 4;
    const DUP: u64 = 1 << 5;
    const RAID10: u64 = 1 << 6;
    const RAID5: u64 = 1 << 7;
    const RAID6: u64 = 1 << 8;
    const RAID1C3: u64 = 1 << 9;
    const RAID1C4: u64 = 1 << 10;

    match flags {
        f if f & RAID1C4 != 0 => "raid1c4",
        f if f & RAID1C3 != 0 => "raid1c3",
        f if f & RAID6 != 0 => "raid6",
        f if f & RAID5 != 0 => "raid5",
        f if f & RAID10 != 0 => "raid10",
        f if f & DUP != 0 => "dup",
        f if f & RAID1 != 0 => "raid1",
        f if f & RAID0 != 0 => "raid0",
        _ => "single",
    }
}

// ─── Device mapper ───────────────────────────────────────────────────────────

/// Mappings that are not logical volumes: LUKS containers, plain dm-crypt,
/// multipath. LVM's own mappings are left to the volume group section rather
/// than listed twice.
fn mappings() -> Vec<Node> {
    mappings_in(Path::new(BLOCK))
}

fn mappings_in(block: &Path) -> Vec<Node> {
    let mut out = Vec::new();
    for device in sysfs::list_dir(block) {
        let Some(uuid) = sysfs::read(device.join("dm/uuid")) else {
            continue;
        };
        if uuid.starts_with("LVM") {
            continue;
        }
        let name = sysfs::read(device.join("dm/name")).unwrap_or_else(|| file_name(&device));
        let kind = mapping_kind(&uuid);

        let mut node = Node::group(format!("{name}  {kind}"), vec![]);
        node.push(Node::field("Device", file_name(&device)));
        node.push_opt("Size", size_of(&device).map(format_bytes));
        if sysfs::read(device.join("dm/suspended")).as_deref() == Some("1") {
            node.push(Node::field("Suspended", Value::Flag(true)));
        }
        // What is stacked on top, which is the direction this page does not
        // otherwise show.
        let holders: Vec<String> = sysfs::list_dir(device.join("holders"))
            .iter()
            .map(|holder| file_name(holder))
            .collect();
        if !holders.is_empty() {
            node.push(Node::field("Held by", Value::List(holders)));
        }
        for layer in underneath(&device) {
            node.push(layer);
        }
        out.push(node);
    }
    out
}

// ─── The stack ───────────────────────────────────────────────────────────────

/// A device described, with every layer underneath it nested inside.
///
/// The nesting is the statement: a row indented under another sits on it.
/// An explicit "on" level per step would double the depth of a stack that is
/// already four or six deep by the time it is worth drawing at all.
fn device_tree(device: &Path, depth: usize) -> Node {
    let mut node = Node::group(describe(device), vec![]);
    if depth < MAX_DEPTH {
        for child in beneath(device) {
            node.push(device_tree(&child, depth + 1));
        }
    }
    node
}

/// The layers underneath a device, without describing the device itself —
/// for callers whose own heading already names it.
fn underneath(device: &Path) -> Vec<Node> {
    beneath(device)
        .iter()
        .map(|child| device_tree(child, 1))
        .collect()
}

/// The devices one layer below this one.
fn beneath(device: &Path) -> Vec<PathBuf> {
    let slaves: Vec<PathBuf> = sysfs::list_dir(device.join("slaves"))
        .iter()
        .map(|slave| std::fs::canonicalize(slave).unwrap_or_else(|_| slave.clone()))
        .collect();
    if !slaves.is_empty() {
        return slaves;
    }
    // A partition has no slaves; what it sits on is the disk it belongs to,
    // which in sysfs is its parent directory.
    if device.join("partition").is_file()
        && let Some(disk) = device.parent()
        && disk.join("dev").is_file()
    {
        return vec![disk.to_path_buf()];
    }
    Vec::new()
}

/// A one-line description of a block device, in the terms that matter at the
/// layer it sits at.
fn describe(device: &Path) -> String {
    let name = file_name(device);
    let size = size_of(device).map(format_bytes).unwrap_or_default();

    if let Some(uuid) = sysfs::read(device.join("dm/uuid")) {
        let mapped = sysfs::read(device.join("dm/name")).unwrap_or_else(|| name.clone());
        return format!("{name}  {} \"{mapped}\"  {size}", mapping_kind(&uuid));
    }
    if device.join("md/level").is_file() {
        let level = sysfs::read(device.join("md/level")).unwrap_or_default();
        return format!("{name}  {level} array  {size}");
    }
    if device.join("partition").is_file() {
        return format!("{name}  partition  {size}");
    }

    // A whole disk: the model is what identifies it to a human.
    let model = sysfs::read(device.join("device/model"))
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    match model {
        Some(model) => format!("{name}  {model}  {size}"),
        None => format!("{name}  {size}"),
    }
}

/// Capacity in bytes. sysfs reports it in 512-byte sectors regardless of the
/// device's own sector size.
fn size_of(device: &Path) -> Option<u64> {
    sysfs::read_u64(device.join("size")).map(|sectors| sectors * 512)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic sysfs under a directory named for the caller, so two
    /// tests running in parallel cannot write each other's fixtures.
    fn fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("hitui-volumes-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("fixture root");
        root
    }

    fn write(path: PathBuf, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture directory");
        }
        std::fs::write(path, contents).expect("writing the fixture");
    }

    fn field(node: &Node, label: &str) -> Option<String> {
        node.children
            .iter()
            .find(|child| child.label == label)
            .and_then(|child| child.value.as_ref())
            .map(|value| value.to_string())
    }

    fn child<'a>(node: &'a Node, label: &str) -> &'a Node {
        node.children
            .iter()
            .find(|child| child.label == label)
            .unwrap_or_else(|| panic!("{label} is not present"))
    }

    #[test]
    fn a_degraded_array_says_so_rather_than_counting_members() {
        // The thing this page exists to say. An array missing a member still
        // works, which is exactly why nobody notices until the second one
        // goes, so "2 of 2 present" and "DEGRADED" must not look alike.
        let root = fixture("degraded");
        let md = root.join("md0/md");
        write(root.join("md0/size"), "2097152\n");
        write(md.join("level"), "raid1\n");
        write(md.join("raid_disks"), "2\n");
        write(md.join("degraded"), "1\n");
        write(md.join("array_state"), "clean\n");
        write(md.join("sync_action"), "recover\n");
        write(md.join("sync_completed"), "262144 / 2097152\n");
        write(md.join("mismatch_cnt"), "0\n");
        write(md.join("dev-sda1/state"), "in_sync\n");
        write(md.join("dev-sda1/slot"), "0\n");
        write(md.join("dev-sdb1/state"), "faulty\n");
        write(md.join("dev-sdb1/slot"), "none\n");

        let arrays = md_arrays_in(&root);
        assert_eq!(arrays.len(), 1);
        let array = &arrays[0];
        assert_eq!(array.label, "md0  raid1");
        assert_eq!(
            field(array, "Health").as_deref(),
            Some("DEGRADED — 1 of 2 members missing or failed")
        );
        assert_eq!(field(array, "Size").as_deref(), Some("1.00 GiB"));
        assert_eq!(field(array, "Rebuild").as_deref(), Some("recover"));

        // Members are named with their state, and a spare has no slot to
        // report rather than the word "none".
        let members = child(array, "Members");
        assert_eq!(field(members, "sda1").as_deref(), Some("in_sync (slot 0)"));
        assert_eq!(field(members, "sdb1").as_deref(), Some("faulty"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_healthy_array_reports_no_rebuild() {
        let root = fixture("healthy");
        let md = root.join("md1/md");
        write(root.join("md1/size"), "1024\n");
        write(md.join("level"), "raid10\n");
        write(md.join("raid_disks"), "4\n");
        write(md.join("degraded"), "0\n");
        write(md.join("sync_action"), "idle\n");
        write(md.join("chunk_size"), "524288\n");

        let arrays = md_arrays_in(&root);
        let array = &arrays[0];
        assert_eq!(
            field(array, "Health").as_deref(),
            Some("4 of 4 members present")
        );
        // An idle array has nothing to say about rebuilding, so it says
        // nothing rather than "idle".
        assert!(field(array, "Rebuild").is_none());
        assert!(field(array, "Progress").is_none());
        assert_eq!(field(array, "Chunk").as_deref(), Some("512.00 KiB"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rebuild_progress_is_a_fraction_of_sectors() {
        let root = fixture("progress");
        write(root.join("sync_completed"), "262144 / 2097152\n");
        assert_eq!(sync_progress(&root), Some(12.5));

        // While nothing is running the kernel writes "none", which is not a
        // fraction and must not read as zero percent.
        write(root.join("sync_completed"), "none\n");
        assert_eq!(sync_progress(&root), None);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_volume_group_is_assembled_from_its_logical_volumes() {
        // sysfs has no volume group — only LVs, each carrying its group in
        // its device-mapper name. Grouping them back up is what makes "this
        // VG spans these two disks" visible without root.
        let root = fixture("lvm");
        for (device, name) in [("dm-0", "vg0-root"), ("dm-1", "vg0-swap")] {
            write(root.join(device).join("dm/uuid"), "LVM-abcdef123456\n");
            write(root.join(device).join("dm/name"), &format!("{name}\n"));
            write(root.join(device).join("size"), "2048\n");
            std::fs::create_dir_all(root.join(device).join("slaves/sda2"))
                .expect("slave directory");
        }

        let groups = volume_groups_in(&root);
        assert_eq!(groups.len(), 1, "both volumes belong to one group");
        let group = &groups[0];
        assert_eq!(group.label, "vg0  volume group");
        // Counted once however many logical volumes touch it.
        assert_eq!(field(group, "Physical volumes").as_deref(), Some("sda2"));

        let names: Vec<&str> = group
            .children
            .iter()
            .filter(|child| child.value.is_none())
            .map(|child| child.label.as_str())
            .collect();
        assert_eq!(names, vec!["root", "swap"]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_hyphen_in_a_volume_name_is_doubled_not_a_separator() {
        // LVM escapes a hyphen in either name by doubling it. Splitting on
        // the first hyphen gets this wrong in exactly the case where a reader
        // would not notice the answer is wrong.
        assert_eq!(
            split_dm_name("vg0-root"),
            Some(("vg0".into(), "root".into()))
        );
        assert_eq!(
            split_dm_name("vg--one-lv--two"),
            Some(("vg-one".into(), "lv-two".into()))
        );
        assert_eq!(
            split_dm_name("my--vg-my--lv--name"),
            Some(("my-vg".into(), "my-lv-name".into()))
        );
        // A name with no separator at all is not a logical volume.
        assert_eq!(
            split_dm_name("luks-abc"),
            Some(("luks".into(), "abc".into()))
        );
        assert_eq!(split_dm_name("plain"), None);
    }

    #[test]
    fn a_btrfs_profile_is_the_bit_above_the_block_group_kind() {
        // The low three bits say which kind of block group it is; the profile
        // is the bit above them, and no profile bit at all means single.
        assert_eq!(profile(0x1), "single"); // data, no profile
        assert_eq!(profile(0x4), "single"); // metadata, no profile
        assert_eq!(profile(0x1 | (1 << 4)), "raid1");
        assert_eq!(profile(0x4 | (1 << 5)), "dup");
        assert_eq!(profile(0x1 | (1 << 3)), "raid0");
        assert_eq!(profile(0x1 | (1 << 6)), "raid10");
        assert_eq!(profile(0x1 | (1 << 7)), "raid5");
        assert_eq!(profile(0x1 | (1 << 8)), "raid6");
        assert_eq!(profile(0x4 | (1 << 9)), "raid1c3");
        assert_eq!(profile(0x4 | (1 << 10)), "raid1c4");
    }

    #[test]
    fn a_btrfs_filesystem_reports_the_redundancy_of_each_block_group() {
        // A two-disk filesystem with data single and metadata raid1 loses
        // half its data to one failure, and nothing else on this page would
        // say so.
        let root = fixture("btrfs");
        let fs = root.join("546b587f-0000-0000-0000-000000000000");
        write(fs.join("label"), "root\n");
        write(fs.join("nodesize"), "16384\n");
        write(fs.join("allocation/data/flags"), "1\n");
        write(fs.join("allocation/data/total_bytes"), "1073741824\n");
        write(fs.join("allocation/metadata/flags"), "36\n"); // 4 metadata | 32 dup
        write(fs.join("allocation/system/flags"), "2\n");
        std::fs::create_dir_all(fs.join("devices")).expect("devices directory");

        let filesystems = btrfs_filesystems_in(&root);
        assert_eq!(filesystems.len(), 1);
        let filesystem = &filesystems[0];
        assert_eq!(filesystem.label, "btrfs \"root\"");
        assert_eq!(field(filesystem, "Data profile").as_deref(), Some("single"));
        assert_eq!(
            field(filesystem, "Metadata profile").as_deref(),
            Some("dup")
        );
        assert_eq!(field(filesystem, "Allocated").as_deref(), Some("1.00 GiB"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_logical_volume_is_not_listed_twice() {
        // LVM's mappings belong to the volume group section; listing them
        // again as bare device-mapper targets would double the page on any
        // machine that uses LVM at all.
        let root = fixture("not-twice");
        write(root.join("dm-0/dm/uuid"), "LVM-abcdef\n");
        write(root.join("dm-0/dm/name"), "vg0-root\n");
        write(root.join("dm-1/dm/uuid"), "CRYPT-LUKS2-abc-luks-abc\n");
        write(root.join("dm-1/dm/name"), "luks-abc\n");

        let mapped = mappings_in(&root);
        assert_eq!(mapped.len(), 1, "only the LUKS mapping");
        assert!(
            mapped[0].label.starts_with("luks-abc  LUKS2"),
            "{}",
            mapped[0].label
        );
        assert_eq!(volume_groups_in(&root).len(), 1);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_stack_walks_down_to_the_physical_device() {
        // The whole point of the page: four layers deep reads as a list of
        // unrelated block devices unless something renders it as a stack.
        let nodes = Volumes.collect().expect("collection failed");
        assert!(!nodes.is_empty());

        // Whatever this machine has, no section may be empty and the walk
        // must terminate.
        for node in &nodes {
            assert!(!node.children.is_empty(), "{} is empty", node.label);
            assert!(
                depth(node) <= MAX_DEPTH + 3,
                "{} nests too deep",
                node.label
            );
        }
    }

    fn depth(node: &Node) -> usize {
        1 + node.children.iter().map(depth).max().unwrap_or(0)
    }
}
