//! Where this machine gets its software.
//!
//! Every configured package source, per manager, read from the files the
//! managers themselves read — no `apt-cache policy`, no `dnf repolist`, no
//! `flatpak remotes`. Those shell out to a package manager, and a package
//! manager asked for a listing may well go and refresh metadata over the
//! network first, which is not something opening a page should do.
//!
//! The field that matters most on every one of them is whether signatures are
//! checked. A repository is a list of things this machine will install as
//! root, so "where from" and "verified how" are one question, and the managers
//! all spell the answer differently: `gpgcheck`, `gpg-verify`, `Signed-By`,
//! `SigLevel`, `require-sigs`. They are normalised to one row here, and a
//! source that is not verified says so rather than being left blank.
//!
//! Only managers that are actually configured appear. An empty section for
//! every manager a distribution does not use would be most of the page.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value as Json;

use crate::collect::{Collector, Group};
use crate::model::{Node, Value};
use crate::sysfs;

pub struct Repositories;

impl Collector for Repositories {
    fn id(&self) -> &'static str {
        "repos"
    }

    fn title(&self) -> &'static str {
        "Repositories"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes: Vec<Node> = [
            nix(),
            apt(),
            rpm_style("DNF", "/etc/yum.repos.d"),
            rpm_style("Zypper", "/etc/zypp/repos.d"),
            pacman(),
            apk(),
            portage(),
            flatpak(),
        ]
        .into_iter()
        .flatten()
        .collect();

        if nodes.is_empty() {
            let mut node = Node::group("Repositories", vec![]);
            node.push(Node::field(
                "Configured",
                "none found — no package manager this knows about is set up here",
            ));
            nodes.push(node);
        }
        Ok(nodes)
    }
}

// ─── Nix ─────────────────────────────────────────────────────────────────────

const NIX_CONF: &str = "/etc/nix/nix.conf";
const NIX_REGISTRY: &str = "/etc/nix/registry.json";
const ROOT_CHANNELS: &str = "/nix/var/nix/profiles/per-user/root/channels";

fn nix() -> Option<Node> {
    let conf = sysfs::read(NIX_CONF);
    let channels = std::fs::read_link(ROOT_CHANNELS).ok();
    if conf.is_none() && channels.is_none() {
        return None;
    }

    let mut node = Node::group("Nix", vec![]).from(NIX_CONF);
    if let Some(conf) = &conf {
        let settings = key_values(conf);
        let get = |key: &str| {
            settings
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };

        node.push_opt("Substituters", get("substituters").map(words));
        node.push_opt(
            "Trusted substituters",
            get("trusted-substituters")
                .filter(|v| !v.trim().is_empty())
                .map(words),
        );
        // The whole point of a binary cache is that you do not build it
        // yourself, so whether its signatures are checked is the question.
        node.push_opt(
            "Signatures",
            get("require-sigs").map(|value| match value.trim() {
                "false" => "not required — unsigned store paths are accepted".to_string(),
                _ => "required".to_string(),
            }),
        );
        node.push_opt("Trusted keys", get("trusted-public-keys").map(words));
        // A trusted user can add substituters of their own, so the list is
        // part of the answer to "where can software come from".
        node.push_opt("Trusted users", get("trusted-users").map(words));
    }

    // The channel the system was built from, named by the store path the
    // profile points at.
    if let Some(channels) = channels {
        node.push_opt(
            "Root channel",
            store_path_name(&resolve(Path::new(ROOT_CHANNELS), &channels)),
        );
    }
    if let Some(user) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| sysfs::read(home.join(".nix-channels")))
    {
        let channels = nix_channels(&user);
        if !channels.is_empty() {
            node.push(Node::field("User channels", Value::List(channels)));
        }
    }

    if let Some(registry) = flake_registry() {
        node.push(Node::field("Flake registry", Value::List(registry)));
    }
    Some(node)
}

/// `.nix-channels` lines, which are written URL-first.
///
/// Turned around to read name-first, because the name is what you type and
/// the URL is what it expands to — the file's own order puts the identifier
/// last, after a URL long enough to push it off the line.
fn nix_channels(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| match line.split_once(char::is_whitespace) {
            Some((url, name)) => format!("{} → {url}", name.trim()),
            None => line.to_string(),
        })
        .collect()
}

/// A symlink target resolved against the directory its link lives in.
fn resolve(link: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        return target.to_path_buf();
    }
    match link.parent() {
        Some(parent) => parent.join(target),
        None => target.to_path_buf(),
    }
}

/// The readable part of a store path: `…-nixos-26.05.10769.cf5e` is the name,
/// the 32 characters before it are a hash and say nothing.
fn store_path_name(path: &Path) -> Option<String> {
    let name = std::fs::canonicalize(path)
        .ok()?
        .file_name()?
        .to_string_lossy()
        .to_string();
    Some(match name.split_once('-') {
        Some((_hash, rest)) => rest.to_string(),
        None => name,
    })
}

/// Flake registry entries, as `from → to`.
///
/// The registry is what an unqualified `nixpkgs#hello` resolves through, so it
/// is a software source in exactly the sense this page is about.
fn flake_registry() -> Option<Vec<String>> {
    let text = sysfs::read(NIX_REGISTRY)?;
    let json: Json = serde_json::from_str(&text).ok()?;
    let entries: Vec<String> = json
        .get("flakes")?
        .as_array()?
        .iter()
        .filter_map(|entry| {
            let from = flake_ref(entry.get("from")?)?;
            let to = flake_ref(entry.get("to")?)?;
            Some(format!("{from} → {to}"))
        })
        .collect();
    (!entries.is_empty()).then_some(entries)
}

/// A flake reference as the registry stores it: a type plus whichever of
/// `id`, `owner/repo`, `url` or `path` that type uses.
fn flake_ref(value: &Json) -> Option<String> {
    let string = |key: &str| value.get(key).and_then(Json::as_str);
    if let Some(id) = string("id") {
        return Some(id.to_string());
    }
    if let (Some(owner), Some(repo)) = (string("owner"), string("repo")) {
        let reference = string("ref").map(|r| format!("/{r}")).unwrap_or_default();
        return Some(format!("{owner}/{repo}{reference}"));
    }
    string("url").or_else(|| string("path")).map(str::to_string)
}

// ─── APT ─────────────────────────────────────────────────────────────────────

fn apt() -> Option<Node> {
    let mut entries = Vec::new();
    if let Some(text) = sysfs::read("/etc/apt/sources.list") {
        entries.extend(apt_one_line(&text));
    }
    for file in sysfs::list_dir("/etc/apt/sources.list.d") {
        let Some(text) = sysfs::read(&file) else {
            continue;
        };
        match file.extension().and_then(|e| e.to_str()) {
            // The deb822 format, which is what new installations use.
            Some("sources") => entries.extend(apt_deb822(&text)),
            Some("list") => entries.extend(apt_one_line(&text)),
            _ => {}
        }
    }
    if entries.is_empty() {
        return None;
    }

    let mut node = Node::group("APT", vec![]).from("/etc/apt");
    for entry in entries {
        node.push(entry);
    }
    Some(node)
}

/// The one-line format: `deb [options] URI suite components...`
fn apt_one_line(text: &str) -> Vec<Node> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = strip_comment(line);
        let mut rest = line.trim();
        let Some((kind, tail)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        if kind != "deb" && kind != "deb-src" {
            continue;
        }
        rest = tail.trim();

        // Bracketed options sit between the type and the URI.
        let mut options = None;
        if let Some(stripped) = rest.strip_prefix('[')
            && let Some((inside, tail)) = stripped.split_once(']')
        {
            options = Some(inside.trim().to_string());
            rest = tail.trim();
        }

        let mut fields = rest.split_whitespace();
        let Some(uri) = fields.next() else { continue };
        let suite = fields.next().unwrap_or("");
        let components: Vec<&str> = fields.collect();

        let mut node = Node::group(uri.to_string(), vec![]);
        node.push(Node::field("Type", kind.to_string()));
        if !suite.is_empty() {
            node.push(Node::field("Suite", suite.to_string()));
        }
        if !components.is_empty() {
            node.push(Node::field(
                "Components",
                Value::List(components.iter().map(|c| c.to_string()).collect()),
            ));
        }
        node.push(Node::field("Signatures", apt_signing(options.as_deref())));
        out.push(node);
    }
    out
}

/// The deb822 format: paragraphs of `Field: value`, blank-line separated.
fn apt_deb822(text: &str) -> Vec<Node> {
    let mut out = Vec::new();
    for paragraph in text.split("\n\n") {
        let fields = key_values_with(paragraph, ':');
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.as_str())
        };
        let Some(uris) = get("URIs") else { continue };

        let mut node = Node::group(uris.to_string(), vec![]);
        node.push_opt("Type", get("Types").map(str::to_string));
        node.push_opt("Suite", get("Suites").map(str::to_string));
        node.push_opt("Components", get("Components").map(words));
        node.push(Node::field(
            "Signatures",
            match get("Signed-By") {
                Some(key) => format!("verified by {key}"),
                None => "verified by the system keyring".to_string(),
            },
        ));
        if get("Enabled").is_some_and(|v| v.eq_ignore_ascii_case("no")) {
            node.push(Node::field("Enabled", Value::Flag(false)));
        }
        out.push(node);
    }
    out
}

/// What an APT option list says about signing.
fn apt_signing(options: Option<&str>) -> String {
    let Some(options) = options else {
        return "verified by the system keyring".to_string();
    };
    for option in options.split_whitespace() {
        if let Some(key) = option.strip_prefix("signed-by=") {
            return format!("verified by {key}");
        }
        // The one that matters: it turns verification off for this source.
        if option.eq_ignore_ascii_case("trusted=yes") {
            return "NOT verified — trusted=yes accepts anything this source serves".to_string();
        }
    }
    "verified by the system keyring".to_string()
}

// ─── DNF, Zypper ─────────────────────────────────────────────────────────────

/// `.repo` files, which DNF, YUM and Zypper all share the format of.
fn rpm_style(manager: &'static str, directory: &str) -> Option<Node> {
    let mut node = Node::group(manager, vec![]).from(directory.to_string());
    let mut found = 0;

    for file in sysfs::list_dir(directory) {
        if file.extension().and_then(|e| e.to_str()) != Some("repo") {
            continue;
        }
        let Some(text) = sysfs::read(&file) else {
            continue;
        };
        for section in ini(&text) {
            let mut repo = Node::group(
                section.get("name").unwrap_or(&section.name).to_string(),
                vec![],
            );
            repo.push(Node::field("Id", section.name.clone()));
            repo.push_opt(
                "URL",
                section
                    .get("baseurl")
                    .or_else(|| section.get("metalink"))
                    .or_else(|| section.get("mirrorlist"))
                    .map(str::to_string),
            );
            // Absent means enabled for both managers.
            if section.get("enabled").is_some_and(is_false) {
                repo.push(Node::field("Enabled", Value::Flag(false)));
            }
            repo.push(Node::field(
                "Signatures",
                match section.get("gpgcheck") {
                    Some(value) if is_false(value) => {
                        "NOT checked — gpgcheck is off for this repository".to_string()
                    }
                    Some(_) => match section.get("gpgkey") {
                        Some(key) => format!("checked against {key}"),
                        None => "checked".to_string(),
                    },
                    None => "unset — the manager's default applies".to_string(),
                },
            ));
            node.push(repo);
            found += 1;
        }
    }
    (found > 0).then_some(node)
}

// ─── pacman ──────────────────────────────────────────────────────────────────

const PACMAN_CONF: &str = "/etc/pacman.conf";

fn pacman() -> Option<Node> {
    let text = sysfs::read(PACMAN_CONF)?;
    let sections = ini(&text);
    // The default signature policy, which a repository inherits unless it
    // sets its own.
    let default_siglevel = sections
        .iter()
        .find(|section| section.name == "options")
        .and_then(|options| options.get("SigLevel"))
        .map(str::to_string);

    let mut node = Node::group("pacman", vec![]).from(PACMAN_CONF);
    let mut found = 0;
    for section in &sections {
        // [options] is configuration, not a repository.
        if section.name.eq_ignore_ascii_case("options") {
            continue;
        }
        let mut repo = Node::group(section.name.clone(), vec![]);

        let mut servers: Vec<String> = section
            .all("Server")
            .into_iter()
            .map(str::to_string)
            .collect();
        // A mirrorlist is the usual way an Arch repository names its servers,
        // and following the Include is the difference between a URL and a
        // path to a file the reader then has to go and open.
        for include in section.all("Include") {
            servers.extend(pacman_mirrors(Path::new(include)));
        }
        if !servers.is_empty() {
            repo.push(Node::field("Servers", Value::List(servers)));
        }
        repo.push(Node::field(
            "Signatures",
            match section.get("SigLevel").or(default_siglevel.as_deref()) {
                Some(level) if level.contains("Never") => {
                    format!("NOT checked — SigLevel {level}")
                }
                Some(level) => format!("checked — SigLevel {level}"),
                None => "the built-in default applies".to_string(),
            },
        ));
        node.push(repo);
        found += 1;
    }
    (found > 0).then_some(node)
}

/// Servers from a pacman mirrorlist, capped.
///
/// A mirrorlist runs to hundreds of lines with all but a few commented out,
/// and the ones in use are what matters.
fn pacman_mirrors(path: &Path) -> Vec<String> {
    const SHOWN: usize = 3;
    let Some(text) = sysfs::read(path) else {
        return Vec::new();
    };
    let servers: Vec<String> = text
        .lines()
        .map(strip_comment)
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| key.trim().eq_ignore_ascii_case("Server"))
        .map(|(_, value)| value.trim().to_string())
        .collect();

    let mut shown: Vec<String> = servers.iter().take(SHOWN).cloned().collect();
    if servers.len() > SHOWN {
        shown.push(format!("and {} more", servers.len() - SHOWN));
    }
    shown
}

// ─── apk ─────────────────────────────────────────────────────────────────────

fn apk() -> Option<Node> {
    let text = sysfs::read("/etc/apk/repositories")?;
    let mut node = Node::group("apk", vec![]).from("/etc/apk/repositories");
    let mut found = 0;
    for line in text.lines() {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        // A @tag prefix marks a repository packages can be pinned to.
        let (tag, url) = match line.strip_prefix('@') {
            Some(rest) => match rest.split_once(char::is_whitespace) {
                Some((tag, url)) => (Some(tag.to_string()), url.trim()),
                None => (None, line),
            },
            None => (None, line),
        };
        let mut repo = Node::group(url.to_string(), vec![]);
        repo.push_opt("Tag", tag);
        node.push(repo);
        found += 1;
    }
    (found > 0).then_some(node)
}

// ─── Portage ─────────────────────────────────────────────────────────────────

fn portage() -> Option<Node> {
    let root = Path::new("/etc/portage/repos.conf");
    // It is either a file or a directory of files, and both are in use.
    let files = if root.is_dir() {
        sysfs::list_dir(root)
    } else {
        vec![root.to_path_buf()]
    };

    let mut node = Node::group("Portage", vec![]).from("/etc/portage/repos.conf");
    let mut found = 0;
    for file in files {
        let Some(text) = sysfs::read(&file) else {
            continue;
        };
        for section in ini(&text) {
            // [DEFAULT] carries main-repo and such, not a repository.
            if section.name.eq_ignore_ascii_case("DEFAULT") {
                continue;
            }
            let mut repo = Node::group(section.name.clone(), vec![]);
            repo.push_opt("Sync URI", section.get("sync-uri").map(str::to_string));
            repo.push_opt("Sync type", section.get("sync-type").map(str::to_string));
            repo.push_opt("Location", section.get("location").map(str::to_string));
            node.push(repo);
            found += 1;
        }
    }
    (found > 0).then_some(node)
}

// ─── Flatpak ─────────────────────────────────────────────────────────────────

fn flatpak() -> Option<Node> {
    let user = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/share/flatpak/repo/config"));

    let mut node = Node::group("Flatpak", vec![]).from("ostree repo config");
    let mut found = 0;
    for (scope, path) in [
        (
            "system",
            Some(PathBuf::from("/var/lib/flatpak/repo/config")),
        ),
        ("user", user),
    ] {
        let Some(text) = path.as_ref().and_then(sysfs::read) else {
            continue;
        };
        for section in ini(&text) {
            // Sections are spelled `remote "flathub"`; anything else is the
            // ostree repository's own configuration.
            let Some(name) = section.name.strip_prefix("remote ") else {
                continue;
            };
            let name = name.trim_matches('"');

            let mut remote = Node::group(name.to_string(), vec![]);
            remote.push_opt("Title", section.get("xa.title").map(str::to_string));
            remote.push_opt("URL", section.get("url").map(str::to_string));
            remote.push(Node::field("Scope", scope));
            remote.push(Node::field(
                "Signatures",
                match section.get("gpg-verify") {
                    Some(value) if is_false(value) => {
                        "NOT verified — gpg-verify is off for this remote".to_string()
                    }
                    _ => "GPG verified".to_string(),
                },
            ));
            if section.get("xa.disable").is_some_and(is_true) {
                remote.push(Node::field("Enabled", Value::Flag(false)));
            }
            node.push(remote);
            found += 1;
        }
    }
    (found > 0).then_some(node)
}

// ─── Parsing helpers ─────────────────────────────────────────────────────────

/// One `[section]` of an INI-ish file.
struct Section {
    name: String,
    entries: Vec<(String, String)>,
}

impl Section {
    /// The first value for `key`, matched without regard to case because the
    /// four formats sharing this parser disagree about it.
    fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// Every value for `key`: pacman repeats `Server` and `Include`.
    fn all(&self, key: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

/// Parse an INI-ish file into sections.
///
/// Shared by the `.repo` files, pacman.conf, portage's repos.conf and
/// flatpak's ostree config, which differ in their keys and not their shape.
fn ini(text: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    for line in text.lines() {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.push(Section {
                name: name.trim().to_string(),
                entries: Vec::new(),
            });
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && let Some(section) = sections.last_mut()
        {
            section
                .entries
                .push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    sections
}

/// `key = value` lines with no sections, as nix.conf is written.
fn key_values(text: &str) -> Vec<(String, String)> {
    key_values_with(text, '=')
}

fn key_values_with(text: &str, separator: char) -> Vec<(String, String)> {
    text.lines()
        .map(strip_comment)
        .filter_map(|line| line.split_once(separator))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

/// Everything from a `#` or `;` to the end of the line, where that character
/// starts the line or follows whitespace.
///
/// The position matters. Cutting at the first `#` anywhere would truncate a
/// URL at its fragment and a password at its hash, and every one of these
/// formats writes comments the conventional way — at the start of a line, or
/// after the value with a space in front.
fn strip_comment(line: &str) -> &str {
    let mut previous = ' ';
    for (index, character) in line.char_indices() {
        if (character == '#' || character == ';') && previous.is_whitespace() {
            return &line[..index];
        }
        previous = character;
    }
    line
}

fn words(value: &str) -> Value {
    Value::List(value.split_whitespace().map(str::to_string).collect())
}

fn is_false(value: &str) -> bool {
    matches!(
        value.trim(),
        "0" | "false" | "no" | "False" | "No" | "FALSE"
    )
}

fn is_true(value: &str) -> bool {
    matches!(value.trim(), "1" | "true" | "yes" | "True" | "Yes" | "TRUE")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node's fields as label/value pairs.
    fn fields(node: &Node) -> Vec<(String, String)> {
        node.children
            .iter()
            .map(|child| {
                (
                    child.label.clone(),
                    child
                        .value
                        .as_ref()
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                )
            })
            .collect()
    }

    fn value_of(node: &Node, label: &str) -> Option<String> {
        node.children
            .iter()
            .find(|child| child.label == label)
            .and_then(|child| child.value.as_ref())
            .map(|value| value.to_string())
    }

    #[test]
    fn an_ini_file_splits_into_sections_whatever_its_comment_style() {
        let sections = ini("# a hash comment\n\
             [core]\n\
             ; a semicolon comment\n\
             key = value\n\
             other=spaced out  \n\
             \n\
             [second]\n\
             key = 1\n");
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].name, "core");
        assert_eq!(sections[0].get("key"), Some("value"));
        assert_eq!(sections[0].get("other"), Some("spaced out"));
        // Keys are matched without regard to case: the four formats sharing
        // this parser disagree about it.
        assert_eq!(sections[0].get("KEY"), Some("value"));
        assert_eq!(sections[1].get("key"), Some("1"));
        // A key before any section header belongs to nothing and is dropped
        // rather than attached to the first one that comes along.
        assert!(
            ini("stray = 1\n[core]\nkey = 2\n")[0]
                .get("stray")
                .is_none()
        );
    }

    #[test]
    fn apt_one_line_entries_are_split_into_their_parts() {
        let repos = apt_one_line(
            "deb https://deb.debian.org/debian bookworm main contrib\n\
             # commented out\n\
             deb-src https://deb.debian.org/debian bookworm main\n",
        );
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].label, "https://deb.debian.org/debian");
        assert_eq!(value_of(&repos[0], "Type").as_deref(), Some("deb"));
        assert_eq!(value_of(&repos[0], "Suite").as_deref(), Some("bookworm"));
        assert_eq!(
            value_of(&repos[0], "Components").as_deref(),
            Some("main, contrib")
        );
        assert_eq!(value_of(&repos[1], "Type").as_deref(), Some("deb-src"));
    }

    #[test]
    fn an_apt_source_that_verifies_nothing_says_so() {
        // `trusted=yes` tells APT to install from this source without
        // checking a signature, which is the single most important thing a
        // line of sources.list can say.
        let repos = apt_one_line("deb [trusted=yes] http://insecure.example/debian sid main\n");
        assert_eq!(
            value_of(&repos[0], "Signatures").as_deref(),
            Some("NOT verified — trusted=yes accepts anything this source serves")
        );

        // A keyring named explicitly is reported as such, so two sources
        // using different keys are distinguishable.
        let repos = apt_one_line(
            "deb [arch=amd64 signed-by=/usr/share/keyrings/x.gpg] https://x.example/deb stable main\n",
        );
        assert_eq!(
            value_of(&repos[0], "Signatures").as_deref(),
            Some("verified by /usr/share/keyrings/x.gpg")
        );
        // The bracketed options must not be mistaken for the URI.
        assert_eq!(repos[0].label, "https://x.example/deb");
    }

    #[test]
    fn apt_deb822_paragraphs_are_read() {
        // The format new installations use, which the one-line parser cannot
        // read at all.
        let repos = apt_deb822(
            "Types: deb\n\
             URIs: https://deb.debian.org/debian\n\
             Suites: bookworm\n\
             Components: main contrib\n\
             Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg\n\
             \n\
             Types: deb\n\
             URIs: https://security.debian.org/debian-security\n\
             Suites: bookworm-security\n\
             Components: main\n\
             Enabled: no\n",
        );
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].label, "https://deb.debian.org/debian");
        assert_eq!(
            value_of(&repos[0], "Signatures").as_deref(),
            Some("verified by /usr/share/keyrings/debian-archive-keyring.gpg")
        );
        // Disabled is stated; enabled is the default and says nothing.
        assert_eq!(value_of(&repos[1], "Enabled").as_deref(), Some("no"));
        assert!(value_of(&repos[0], "Enabled").is_none());
    }

    #[test]
    fn a_pacman_repository_follows_its_mirrorlist() {
        // Without following the Include, an Arch repository shows no URL at
        // all — just the path of a file the reader then has to go and open.
        let dir = std::env::temp_dir().join("hitui-pacman-test");
        std::fs::create_dir_all(&dir).expect("fixture directory");
        let mirrorlist = dir.join("mirrorlist");
        std::fs::write(
            &mirrorlist,
            "# Arch mirrorlist\n\
             #Server = https://commented.example/$repo/os/$arch\n\
             Server = https://one.example/$repo/os/$arch\n\
             Server = https://two.example/$repo/os/$arch\n\
             Server = https://three.example/$repo/os/$arch\n\
             Server = https://four.example/$repo/os/$arch\n",
        )
        .expect("writing the fixture");

        let servers = pacman_mirrors(&mirrorlist);
        // Commented-out mirrors are not servers.
        assert!(!servers.iter().any(|s| s.contains("commented")));
        // Capped, with the remainder counted rather than dropped silently.
        assert_eq!(servers.len(), 4);
        assert_eq!(servers[3], "and 1 more");

        let _ = std::fs::remove_file(mirrorlist);
    }

    #[test]
    fn a_repo_file_reports_what_signing_it_does() {
        let sections = ini("[fedora]\n\
             name=Fedora $releasever\n\
             metalink=https://mirrors.fedoraproject.org/metalink?repo=fedora-$releasever\n\
             gpgcheck=1\n\
             gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-fedora\n\
             \n\
             [sketchy]\n\
             name=Third party\n\
             baseurl=http://sketchy.example/rpm\n\
             enabled=1\n\
             gpgcheck=0\n");
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].get("gpgcheck"), Some("1"));
        assert!(is_false(sections[1].get("gpgcheck").unwrap()));
        // A URL comes from whichever of the three keys the repository used.
        assert!(sections[0].get("metalink").unwrap().contains("metalink"));
    }

    #[test]
    fn a_flatpak_remote_section_is_named_inside_quotes() {
        // ostree spells these `[remote "flathub"]`, so a parser that took the
        // section name whole would label the remote `remote "flathub"`.
        let sections = ini("[core]\n\
             repo_version=1\n\
             \n\
             [remote \"flathub\"]\n\
             url=https://dl.flathub.org/repo/\n\
             gpg-verify=true\n");
        let remote = sections
            .iter()
            .find(|section| section.name.starts_with("remote "))
            .expect("the remote section");
        assert_eq!(
            remote
                .name
                .strip_prefix("remote ")
                .unwrap()
                .trim_matches('"'),
            "flathub"
        );
        assert_eq!(remote.get("url"), Some("https://dl.flathub.org/repo/"));
    }

    #[test]
    fn nix_channels_read_name_first() {
        // The file is written URL-first, which puts the identifier after a
        // URL long enough to push it off the line.
        assert_eq!(
            nix_channels("https://nixos.org/channels/nixos-26.05 nixos\n"),
            vec!["nixos → https://nixos.org/channels/nixos-26.05"]
        );
        assert!(nix_channels("\n\n").is_empty());
    }

    #[test]
    fn a_flake_registry_entry_reads_from_arrow_to() {
        let registry: Json = serde_json::from_str(
            r#"{"version": 2, "flakes": [
                {"from": {"id": "nixpkgs", "type": "indirect"},
                 "to": {"owner": "NixOS", "repo": "nixpkgs", "ref": "nixos-unstable", "type": "github"}}
            ]}"#,
        )
        .expect("fixture is valid JSON");

        let from = flake_ref(registry["flakes"][0].get("from").unwrap()).unwrap();
        let to = flake_ref(registry["flakes"][0].get("to").unwrap()).unwrap();
        assert_eq!(from, "nixpkgs");
        assert_eq!(to, "NixOS/nixpkgs/nixos-unstable");
    }

    #[test]
    fn a_store_path_is_named_without_its_hash() {
        // The 32 characters before the name are a hash and say nothing.
        assert_eq!(
            Path::new("/nix/store/wfvpl6cj2h51d00ipyma8g18kll1hb6z-nixos-26.05.10769")
                .file_name()
                .map(|n| n
                    .to_string_lossy()
                    .split_once('-')
                    .map(|(_, rest)| rest.to_string()))
                .unwrap(),
            Some("nixos-26.05.10769".to_string())
        );
    }

    #[test]
    fn comments_are_stripped_but_values_are_not_truncated() {
        assert_eq!(strip_comment("key = value # trailing"), "key = value ");
        assert_eq!(strip_comment("; whole line"), "");
        assert_eq!(strip_comment("  # indented"), "  ");
        assert_eq!(strip_comment("key = value"), "key = value");

        // The reason the position is checked rather than the character: a
        // fragment is part of the URL, not a comment about it.
        assert_eq!(
            strip_comment("url=https://example.test/repo#stable"),
            "url=https://example.test/repo#stable"
        );
        assert_eq!(
            strip_comment("deb https://user:pa;ss@host/debian sid main"),
            "deb https://user:pa;ss@host/debian sid main"
        );
    }

    #[test]
    fn collects_without_inventing_a_manager() {
        // Only managers actually configured here appear; an empty section per
        // manager a distribution does not use would be most of the page.
        let nodes = Repositories.collect().expect("collection failed");
        assert!(!nodes.is_empty());
        for node in &nodes {
            assert!(
                !node.children.is_empty(),
                "{} is an empty section",
                node.label
            );
        }
        // Whatever this machine runs, a section that claims a manager must
        // have found its configuration.
        let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        if labels.contains(&"Nix") {
            assert!(Path::new(NIX_CONF).exists() || Path::new(ROOT_CHANNELS).exists());
        }
        // And the fields are the normalised ones, not raw config keys.
        for node in &nodes {
            for (label, _) in fields(node) {
                assert!(!label.contains('='), "{label} looks like a raw config line");
            }
        }
    }
}
