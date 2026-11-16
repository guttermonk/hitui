//! Users and groups, from /etc/passwd and /etc/group.
//!
//! Read as files rather than through NSS, so this shows local accounts only.
//! On a machine joined to LDAP or Active Directory the directory's users will
//! not appear, and the page says so rather than implying the list is complete.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::sysfs;

/// Below this uid, accounts exist to own files and run daemons.
///
/// The threshold is a convention set in /etc/login.defs rather than anything
/// the kernel enforces, but 1000 is near-universal on Linux.
const FIRST_REGULAR_UID: u64 = 1000;

pub struct Users;

impl Collector for Users {
    fn id(&self) -> &'static str {
        "users"
    }

    fn title(&self) -> &'static str {
        "Users"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let users = passwd();
        let groups = group_names();

        let mut nodes = Vec::new();

        // People first, service accounts second: a list sorted only by uid
        // buries the three accounts you care about under fifty you do not.
        let (regular, system): (Vec<&User>, Vec<&User>) =
            users.iter().partition(|u| u.uid >= FIRST_REGULAR_UID);

        for (title, members) in [("Regular users", regular), ("System accounts", system)] {
            if members.is_empty() {
                continue;
            }
            let mut section = Node::group(title, vec![]);
            for user in members {
                section.push(Node::field(&user.name, user.summary(&groups)));
            }
            nodes.push(section);
        }

        nodes.push(Node::group(
            "Source",
            vec![Node::field(
                "Accounts",
                "/etc/passwd — local accounts only, not LDAP or AD directories",
            )],
        ));
        Ok(nodes)
    }
}

pub struct Groups;

impl Collector for Groups {
    fn id(&self) -> &'static str {
        "groups"
    }

    fn title(&self) -> &'static str {
        "Groups"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let Some(text) = sysfs::read("/etc/group") else {
            return Ok(vec![Node::group(
                "Groups",
                vec![Node::field("Status", "/etc/group is not readable")],
            )]);
        };

        let mut populated = Node::group("Groups with members", vec![]);
        let mut empty = Node::group("Groups with no members", vec![]);

        for line in text.lines() {
            // name:password:gid:member,member
            let mut fields = line.split(':');
            let (Some(name), Some(_), Some(gid)) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let members: Vec<&str> = fields
                .next()
                .unwrap_or("")
                .split(',')
                .filter(|m| !m.is_empty())
                .collect();

            if members.is_empty() {
                // A gid alone is all there is to say about these.
                empty.push(Node::field(name, format!("gid {gid}")));
            } else {
                populated.push(Node::field(
                    name,
                    format!("gid {gid} · {}", members.join(", ")),
                ));
            }
        }

        let mut nodes = Vec::new();
        for section in [populated, empty] {
            if !section.children.is_empty() {
                nodes.push(section);
            }
        }
        Ok(nodes)
    }
}

struct User {
    name: String,
    uid: u64,
    gid: u64,
    comment: String,
    home: String,
    shell: String,
}

impl User {
    fn summary(&self, groups: &BTreeMap<u64, String>) -> String {
        let mut parts = vec![format!("uid {}", self.uid)];

        // Resolve the primary group: "gid 100" means less than "users".
        match groups.get(&self.gid) {
            Some(group) => parts.push(format!("group {group}")),
            None => parts.push(format!("gid {}", self.gid)),
        }
        if !self.comment.is_empty() {
            parts.push(self.comment.clone());
        }
        if !self.home.is_empty() {
            parts.push(self.home.clone());
        }
        // Flag accounts that cannot log in, which is most of the system ones.
        if is_nologin(&self.shell) {
            parts.push("no login".to_string());
        } else if !self.shell.is_empty() {
            parts.push(self.shell.clone());
        }
        parts.join(" · ")
    }
}

/// Shells that exist specifically to refuse a login.
fn is_nologin(shell: &str) -> bool {
    let name = shell.rsplit('/').next().unwrap_or(shell);
    matches!(name, "nologin" | "false" | "true" | "sync") || shell.is_empty()
}

fn passwd() -> Vec<User> {
    let Some(text) = sysfs::read("/etc/passwd") else {
        return Vec::new();
    };
    let mut users: Vec<User> = text
        .lines()
        .filter_map(|line| {
            // name:password:uid:gid:comment:home:shell
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < 7 {
                return None;
            }
            Some(User {
                name: fields[0].to_string(),
                uid: fields[2].parse().ok()?,
                gid: fields[3].parse().ok()?,
                comment: fields[4].to_string(),
                home: fields[5].to_string(),
                shell: fields[6].to_string(),
            })
        })
        .collect();
    users.sort_by_key(|u| (u.uid, u.name.clone()));
    users
}

/// gid to group name, for resolving primary groups.
fn group_names() -> BTreeMap<u64, String> {
    let Some(text) = sysfs::read("/etc/group") else {
        return BTreeMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            Some((fields.get(2)?.parse().ok()?, fields.first()?.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_shells_that_refuse_login() {
        assert!(is_nologin("/run/current-system/sw/bin/nologin"));
        assert!(is_nologin("/sbin/nologin"));
        assert!(is_nologin("/bin/false"));
        assert!(is_nologin(""), "an empty shell is not a login");
        assert!(!is_nologin("/bin/bash"));
        assert!(!is_nologin("/run/current-system/sw/bin/zsh"));
    }

    #[test]
    fn parses_this_machines_passwd() {
        let users = passwd();
        assert!(!users.is_empty(), "/etc/passwd should parse");

        // root is uid 0 on every Unix, and sorting puts it first.
        assert_eq!(users[0].uid, 0);
        assert_eq!(users[0].name, "root");
    }

    #[test]
    fn resolves_primary_groups_to_names() {
        let groups = group_names();
        let users = passwd();
        let root = users.iter().find(|u| u.uid == 0).expect("root exists");
        let summary = root.summary(&groups);

        assert!(summary.contains("uid 0"), "{summary}");
        // gid 0 is "root" on every Linux, so the name must be resolved rather
        // than falling back to the number.
        if groups.contains_key(&0) {
            assert!(summary.contains("group root"), "{summary}");
        }
    }

    #[test]
    fn separates_people_from_service_accounts() {
        let nodes = Users.collect().expect("collection failed");
        let titles: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        assert!(titles.contains(&"System accounts"), "{titles:?}");
        // The last section states where the list came from, since reading it
        // as a complete account list would be wrong on a domain-joined host.
        assert_eq!(nodes.last().unwrap().label, "Source");
    }

    #[test]
    fn groups_are_split_by_whether_anyone_is_in_them() {
        let nodes = Groups.collect().expect("collection failed");
        assert!(!nodes.is_empty());
        for node in &nodes {
            assert!(!node.children.is_empty(), "{} is empty", node.label);
        }
    }
}
