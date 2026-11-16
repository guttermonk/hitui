//! Development toolchains present on `PATH`.
//!
//! Presence is cheap — resolving a name in `PATH` is a few `stat` calls — but
//! a version requires running the tool. Each one is asked only if it is
//! actually installed, so the cost is proportional to what you have rather
//! than to the length of the list.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;
use crate::probe;

/// Tools worth asking about, grouped as a reader would look for them.
///
/// Every entry uses `--version`; tools that want a different flag are
/// reported as present with an unknown version rather than being special
/// cased into a mess.
const TOOLCHAINS: [(&str, &[&str]); 5] = [
    (
        "Compilers and languages",
        &[
            "gcc", "g++", "clang", "rustc", "go", "python3", "node", "ruby", "perl", "php", "lua",
            "zig",
        ],
    ),
    (
        "Build systems",
        &[
            "make", "cmake", "ninja", "meson", "cargo", "npm", "pnpm", "bazel",
        ],
    ),
    ("Version control", &["git", "hg", "svn", "jj", "fossil"]),
    (
        "Debuggers and analysis",
        &["gdb", "lldb", "valgrind", "strace", "ltrace", "perf"],
    ),
    (
        "Containers and packaging",
        &["docker", "podman", "nix", "flatpak", "rpm", "dpkg"],
    ),
];

pub struct Development;

impl Collector for Development {
    fn id(&self) -> &'static str {
        "development"
    }

    fn title(&self) -> &'static str {
        "Development"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut nodes = Vec::new();
        let mut found = 0;
        let mut checked = 0;

        for (category, tools) in TOOLCHAINS {
            let mut section = Node::group(category, vec![]);
            for tool in tools {
                checked += 1;
                // Skip the spawn entirely for tools that are not installed.
                if !probe::available(tool) {
                    continue;
                }
                found += 1;
                section.push(Node::field(*tool, version(tool)));
            }
            if !section.children.is_empty() {
                nodes.push(section);
            }
        }

        let mut overview = Node::group("Development tools", vec![]);
        overview.push(Node::field(
            "Installed",
            format!("{found} of {checked} checked"),
        ));
        overview.push(Node::field(
            "Versions",
            "read by running each tool with --version",
        ));
        nodes.insert(0, overview);

        if nodes.len() == 1 {
            nodes.push(Node::group(
                "Toolchains",
                vec![Node::field(
                    "Status",
                    "none of the tools hitui knows about are on PATH",
                )],
            ));
        }
        Ok(nodes)
    }
}

/// A tool's version, reduced to the one useful line.
fn version(tool: &str) -> String {
    let Some(output) = probe::run(tool, &["--version"]) else {
        // Installed but uncooperative: java, for one, wants `-version` and
        // writes to stderr. Saying so beats inventing a version.
        return "installed, version not reported for --version".to_string();
    };

    // Banners run to several lines of licence text, and perl's begins with a
    // blank one, so take the first line that actually says something.
    let Some(first) = output.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return "installed, version unknown".to_string();
    };
    strip_banner(first, tool)
}

/// Reduce a version banner to just the version.
///
/// Tools prefix their own name, sometimes a parenthesised product name, and
/// sometimes the word "version" — `gcc (GCC) 15.3.0`, `nix (Nix) 2.34.9`,
/// `strace -- version 7.3`, `git version 2.54.0`. The label already says which
/// tool this is, so all of that is noise.
fn strip_banner(line: &str, tool: &str) -> String {
    let mut rest = line.trim();

    // The tool's own name, however it capitalises it.
    if rest.len() > tool.len() && rest[..tool.len()].eq_ignore_ascii_case(tool) {
        rest = rest[tool.len()..].trim_start();
    }
    // strace writes "-- version", others just "version".
    for prefix in ["-- version", "--version", "version"] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped.trim_start();
            break;
        }
    }
    // A parenthesised product name: "(GCC)", "(Nix)", "(GNU coreutils)".
    if rest.starts_with('(')
        && let Some(close) = rest.find(')')
    {
        let after = rest[close + 1..].trim_start();
        // Only drop it if a version follows; "(cli)" in php's banner comes
        // after the version and must survive.
        if after.starts_with(|c: char| c.is_ascii_digit()) {
            rest = after;
        }
    }

    // Never reduce the line to nothing: a tool whose banner is only its name
    // has still told us it is installed.
    if rest.is_empty() {
        let whole = line.trim();
        return if whole.is_empty() {
            "installed, version unknown".to_string()
        } else {
            whole.to_string()
        };
    }
    rest.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduces_real_banners_to_just_the_version() {
        // Every one of these is a banner observed from the tool itself.
        assert_eq!(strip_banner("git version 2.54.0", "git"), "2.54.0");
        assert_eq!(strip_banner("cmake version 4.1.6", "cmake"), "4.1.6");
        assert_eq!(
            strip_banner("rustc 1.99.0 (b940084d7 2026-09-28)", "rustc"),
            "1.99.0 (b940084d7 2026-09-28)"
        );
        // A parenthesised product name before the version.
        assert_eq!(strip_banner("gcc (GCC) 15.3.0", "gcc"), "15.3.0");
        assert_eq!(strip_banner("nix (Nix) 2.34.9", "nix"), "2.34.9");
        // strace spells it "-- version".
        assert_eq!(strip_banner("strace -- version 7.3", "strace"), "7.3");
        // Case differs from the command name.
        assert_eq!(strip_banner("Flatpak 1.18.4", "flatpak"), "1.18.4");
    }

    #[test]
    fn keeps_parenthesised_text_that_is_not_a_product_name() {
        // php's "(cli)" follows the version and is meaningful; dropping any
        // parenthesised group unconditionally would eat it.
        assert_eq!(
            strip_banner("PHP 8.4.27 (cli) (built: Oct 19 2026)", "php"),
            "8.4.27 (cli) (built: Oct 19 2026)"
        );
    }

    #[test]
    fn never_reduces_a_banner_to_nothing() {
        // The guarantee is non-emptiness: a tool whose banner is only its own
        // name has still told us it is installed.
        assert_eq!(strip_banner("git", "git"), "git");
        for banner in ["gcc (GCC)", "nix", "  ", "version", "-- version"] {
            let stripped = strip_banner(banner, "gcc");
            assert!(!stripped.is_empty(), "{banner:?} reduced to nothing");
        }
    }

    #[test]
    fn only_installed_tools_are_executed() {
        // The guarantee that keeps this page fast: a list of forty tools on a
        // machine with six installed costs six spawns, not forty.
        let nodes = Development.collect().expect("collection failed");
        let listed: usize = nodes
            .iter()
            .skip(1)
            .map(|section| section.children.len())
            .sum();

        for section in nodes.iter().skip(1) {
            for tool in &section.children {
                if tool.label == "Status" {
                    continue;
                }
                assert!(
                    probe::available(&tool.label),
                    "{} was listed but is not on PATH",
                    tool.label
                );
            }
        }
        assert!(listed <= TOOLCHAINS.iter().map(|(_, t)| t.len()).sum::<usize>());
    }

    #[test]
    fn every_listed_tool_reports_something() {
        let nodes = Development.collect().expect("collection failed");
        for section in nodes.iter().skip(1) {
            for tool in &section.children {
                let value = tool.value.as_ref().unwrap().to_string();
                assert!(!value.is_empty(), "{} has no version line", tool.label);
            }
        }
    }

    #[test]
    fn the_overview_counts_what_was_checked() {
        let nodes = Development.collect().expect("collection failed");
        let installed = nodes[0]
            .children
            .iter()
            .find(|c| c.label == "Installed")
            .and_then(|c| c.value.clone())
            .unwrap()
            .to_string();
        let total: usize = TOOLCHAINS.iter().map(|(_, t)| t.len()).sum();
        assert!(
            installed.ends_with(&format!("of {total} checked")),
            "{installed}"
        );
    }
}
