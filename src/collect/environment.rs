//! Environment variables of the hitui process.
//!
//! Values that look like credentials are replaced with their length rather
//! than printed. hardinfo2 prints everything, but hitui has a `--json` mode
//! whose output is meant to be pasted into bug reports, and an environment
//! routinely holds API tokens and session keys. "Is it set, and how long is
//! it" answers the debugging question without putting the secret on a
//! pastebin.

use anyhow::Result;

use crate::collect::{Collector, Group};
use crate::model::Node;

pub struct Environment;

impl Collector for Environment {
    fn id(&self) -> &'static str {
        "environment"
    }

    fn title(&self) -> &'static str {
        "Environment Variables"
    }

    fn group(&self) -> Group {
        Group::Computer
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        let mut variables: Vec<(String, String)> = std::env::vars().collect();
        variables.sort();

        let redacted = variables
            .iter()
            .filter(|(name, _)| is_sensitive(name))
            .count();

        let mut listing = Node::group("Variables", vec![]);
        for (name, value) in &variables {
            listing.push(Node::field(name, present(name, value)));
        }

        let mut nodes = vec![Node::group(
            "Environment",
            vec![
                Node::field("Variables", variables.len()),
                Node::field(
                    "Redacted",
                    if redacted == 0 {
                        "none — no variable names looked like credentials".to_string()
                    } else {
                        format!("{redacted} — names matching credential patterns")
                    },
                ),
            ],
        )];
        nodes.push(listing);
        Ok(nodes)
    }
}

/// The value as it should be shown: verbatim, or a redaction notice.
fn present(name: &str, value: &str) -> String {
    if !is_sensitive(name) {
        return value.to_string();
    }
    if value.is_empty() {
        return "<empty>".to_string();
    }
    // Length is the useful part of a secret for debugging: it distinguishes
    // "unset" from "set to the wrong thing" from "truncated".
    format!("<redacted, {} characters>", value.chars().count())
}

/// Whether a variable's *name* suggests its value is a credential.
///
/// Matched on the name only. Scanning values for secret-shaped strings would
/// be guesswork, and the conventions around these names are strong enough
/// that this catches what matters.
fn is_sensitive(name: &str) -> bool {
    const MARKERS: [&str; 12] = [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "APIKEY",
        "API_KEY",
        "ACCESS_KEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
        "SESSION_KEY",
        "AUTH_KEY",
        "_PAT",
    ];
    let upper = name.to_ascii_uppercase();
    MARKERS.iter().any(|marker| upper.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_credential_names() {
        for name in [
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "ANTHROPIC_API_KEY",
            "MYSQL_PASSWORD",
            "npm_token",
            "GH_PAT",
        ] {
            assert!(is_sensitive(name), "{name} should be redacted");
        }
    }

    #[test]
    fn leaves_ordinary_variables_alone() {
        for name in ["PATH", "HOME", "LANG", "TERM", "SHELL", "EDITOR", "PWD"] {
            assert!(!is_sensitive(name), "{name} should not be redacted");
        }
        assert_eq!(present("PATH", "/usr/bin:/bin"), "/usr/bin:/bin");
    }

    #[test]
    fn redaction_keeps_the_length_and_drops_the_value() {
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let shown = present("GITHUB_TOKEN", secret);

        assert!(!shown.contains("ghp_"), "the secret leaked: {shown}");
        assert!(shown.contains(&secret.len().to_string()), "{shown}");
        // An unset-but-present variable is a distinct and useful state.
        assert_eq!(present("GITHUB_TOKEN", ""), "<empty>");
    }

    #[test]
    fn no_sensitive_value_appears_in_the_report() {
        // The guarantee this page makes, checked against the real environment.
        let nodes = Environment.collect().expect("collection failed");
        let listing = &nodes[1];

        for field in &listing.children {
            if !is_sensitive(&field.label) {
                continue;
            }
            let shown = field.value.as_ref().unwrap().to_string();
            let actual = std::env::var(&field.label).unwrap_or_default();
            if !actual.is_empty() {
                assert_ne!(shown, actual, "{} was printed verbatim", field.label);
                assert!(shown.starts_with("<redacted"), "{shown}");
            }
        }
    }
}
