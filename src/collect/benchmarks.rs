//! The Benchmarks page.
//!
//! Nothing runs until the user presses Enter. hardinfo2 starts a benchmark
//! when you select its page, which means opening the wrong page pegs a core
//! for half a minute — the one behaviour of the original worth refusing to
//! copy.
//!
//! Results are in real units rather than scores. hardinfo2's scores are only
//! interpretable against its shared results database; hitui does not upload
//! anything anywhere, so a score would be a number with nothing to compare it
//! to. "412 MiB/s" compares against any other machine, and against the
//! hardware's own datasheet.

use std::time::Duration;

use anyhow::Result;

use crate::bench::{Kind, Measurement, Progress, Runner, history, registry};
use crate::collect::{Collector, Group};
use crate::config::Benchmarks as Settings;
use crate::model::Node;

#[derive(Default)]
pub struct Benchmarks {
    runner: Runner,
    settings: Settings,
    /// Whether `configure` has been called.
    ///
    /// An unconfigured collector writes nothing. `registry()` builds these
    /// with `Default` and the binary configures every page immediately, so
    /// the only caller that can skip it is a test — and a test suite must not
    /// leave files in the state directory of whoever ran it.
    configured: bool,
    /// Runs already on disk, read once when a run finishes so that every
    /// tick of a live page does not re-read the file.
    previous: Vec<history::Run>,
    /// Whether the run now showing has been written to the history. The page
    /// is re-collected four times a second while running and once more after
    /// it finishes, and a run must be recorded exactly once.
    recorded: bool,
}

impl Collector for Benchmarks {
    fn id(&self) -> &'static str {
        "benchmarks"
    }

    fn title(&self) -> &'static str {
        "Benchmarks"
    }

    fn group(&self) -> Group {
        Group::Benchmarks
    }

    fn action(&self) -> Option<&'static str> {
        Some(if self.runner.is_running() {
            "running…"
        } else {
            "run benchmarks"
        })
    }

    fn configure(&mut self, config: &crate::config::Config) {
        self.settings = config.benchmarks.clone();
        self.configured = true;
    }

    fn activate(&mut self) {
        // Read before the run, so the comparison is against what was on disk
        // beforehand rather than against the run being started.
        self.previous = if self.keeps_history() {
            history::load()
        } else {
            Vec::new()
        };
        self.recorded = false;
        self.runner.start();
    }

    fn refresh_interval(&self) -> Option<Duration> {
        // Only while a run is in flight, so an idle page is not re-collected
        // four times a second for nothing.
        self.runner
            .is_running()
            .then_some(Duration::from_millis(250))
    }

    fn collect(&mut self) -> Result<Vec<Node>> {
        // Writing happens outside the lock: the worker publishes into it four
        // times a second and file I/O has no business holding it.
        let finished = self.runner.with_state(|suite| {
            matches!(suite.progress, Progress::Finished { .. }).then(|| suite.results.clone())
        });
        if let Some(Some(results)) = finished
            && self.keeps_history()
            && !self.recorded
        {
            self.recorded = history::record(&results, self.settings.history_limit);
        }

        let previous = &self.previous;
        let settings = &self.settings;
        let Some(nodes) = self.runner.with_state(|suite| {
            let mut nodes = vec![status(&suite.progress)];

            // Group finished results by what they exercise, in a fixed order
            // so the page does not reshuffle as results arrive.
            for kind in Kind::ALL {
                let mut section = Node::group(kind.title(), vec![]);
                for (name, _, measurement) in suite.results.iter().filter(|(_, k, _)| *k == kind) {
                    section.push(Node::field(
                        *name,
                        describe(name, kind, measurement, previous),
                    ));
                }
                if !section.children.is_empty() {
                    nodes.push(section);
                }
            }
            if let Some(section) = comparison(previous, settings) {
                nodes.push(section);
            }
            nodes
        }) else {
            // The lock is only ever held to publish progress, so this means a
            // worker panicked mid-update and poisoned it.
            return Ok(vec![Node::group(
                "Benchmarks",
                vec![Node::field(
                    "Status",
                    "a benchmark panicked — restart hitui to run them again",
                )],
            )]);
        };

        let mut nodes = nodes;
        nodes.push(about());
        Ok(nodes)
    }
}

/// Run the whole suite synchronously and return the same sections the page
/// shows, for `--json --bench`.
///
/// Blocks for as long as the suite takes. That is acceptable here precisely
/// because it is not the interactive path: a script asked for numbers and will
/// wait for them.
pub fn run_now(settings: &Settings) -> Vec<Node> {
    // Read before the run, so the comparison is against what was on disk
    // beforehand rather than against this run.
    let previous = if settings.history {
        history::load()
    } else {
        Vec::new()
    };
    let started = std::time::Instant::now();
    let results: Vec<_> = registry()
        .iter()
        .map(|benchmark| {
            (
                benchmark.name(),
                benchmark.kind(),
                crate::bench::best_of(benchmark.as_ref()),
            )
        })
        .collect();

    if settings.history {
        history::record(&results, settings.history_limit);
    }

    let mut nodes = vec![status(&Progress::Finished {
        elapsed: started.elapsed(),
    })];
    for kind in Kind::ALL {
        let mut section = Node::group(kind.title(), vec![]);
        for (name, _, measurement) in results.iter().filter(|(_, k, _)| *k == kind) {
            section.push(Node::field(
                *name,
                describe(name, kind, measurement, &previous),
            ));
        }
        if !section.children.is_empty() {
            nodes.push(section);
        }
    }
    if let Some(section) = comparison(&previous, settings) {
        nodes.push(section);
    }
    nodes.push(about());
    nodes
}

fn status(progress: &Progress) -> Node {
    let mut node = Node::group("Benchmarks", vec![]);

    match progress {
        Progress::Idle => {
            node.push(Node::field("Status", "not run yet"));
            node.push(Node::field("Start", "press Enter to run the suite"));
            node.push(Node::field("Benchmarks", registry().len()));
        }
        Progress::Running {
            current,
            done,
            total,
        } => {
            node.push(Node::field("Status", "running"));
            node.push(Node::field("Current", *current));
            node.push(Node::field(
                "Progress",
                crate::model::Value::gauge(
                    *done as f64 / *total as f64 * 100.0,
                    format!("{done} of {total} complete"),
                ),
            ));
        }
        Progress::Finished { elapsed } => {
            node.push(Node::field("Status", "complete"));
            node.push(Node::field(
                "Suite duration",
                format!("{:.1} s", elapsed.as_secs_f64()),
            ));
            node.push(Node::field("Re-run", "press Enter"));
        }
    }
    node
}

/// What these numbers do and do not mean.
fn about() -> Node {
    Node::group(
        "About these results",
        vec![
            Node::field(
                "Units",
                "real physical units, not scores — comparable against any other \
                 machine running the same hitui build",
            ),
            Node::field(
                "Uploads",
                "none — results stay on this machine. hardinfo2 compares against \
                 a shared database; hitui sends nothing anywhere",
            ),
            Node::field(
                "Accuracy",
                "best of three passes per benchmark, which holds most figures \
                 within a few percent between runs; close the rest of your \
                 desktop for comparable numbers",
            ),
            Node::field(
                "Sequential write",
                "run once rather than three times — repeating it would report \
                 the drive's SLC cache speed as sustained throughput",
            ),
            Node::field(
                "Not measured",
                "GPU throughput, which needs a graphics context hitui has no way \
                 to create from a terminal",
            ),
        ],
    )
}

#[cfg(test)]
mod sharing_tests {
    use super::*;
    use crate::config::Benchmarks as Settings;

    fn sharing(settings: &Settings) -> String {
        let node = comparison(&[], settings).expect("the section is always present");
        node.children
            .iter()
            .find(|child| child.label == "Sharing")
            .and_then(|child| child.value.as_ref())
            .map(|value| value.to_string())
            .expect("sharing is always stated")
    }

    #[test]
    fn sharing_is_off_by_default() {
        let settings = Settings::default();
        assert!(!settings.share);
        assert!(settings.share_endpoint.is_empty());
        assert_eq!(sharing(&settings), "off — results stay on this machine");
    }

    #[test]
    fn a_sharing_switch_that_does_nothing_says_which_nothing_it_does() {
        // Three states, and none may look like any other. The failure this
        // guards is a setting that reads as on while nothing is sent and
        // nothing says so — which is the whole reason the switch exists
        // before the feature does.
        let mut settings = Settings {
            share: true,
            ..Settings::default()
        };
        let no_endpoint = sharing(&settings);
        assert!(no_endpoint.contains("nothing can be sent"), "{no_endpoint}");
        assert!(no_endpoint.contains("share_endpoint"), "{no_endpoint}");

        settings.share_endpoint = "https://example.test/results".to_string();
        let unimplemented = sharing(&settings);
        assert!(unimplemented.contains("not implemented"), "{unimplemented}");
        assert!(unimplemented.contains("nothing is sent"), "{unimplemented}");
        // And it names where it would have gone, so the setting is visibly
        // taking effect even though the upload does not exist.
        assert!(
            unimplemented.contains("https://example.test/results"),
            "{unimplemented}"
        );

        // All three readings are distinct.
        let off = sharing(&Settings::default());
        assert_ne!(off, no_endpoint);
        assert_ne!(no_endpoint, unimplemented);
    }

    #[test]
    fn an_unconfigured_collector_writes_nothing() {
        // `registry()` builds these with Default and the binary configures
        // every page before it draws, so the only caller that can skip
        // configure is a test — and running the suite must not leave a file
        // in the state directory of whoever ran it. This is the invariant
        // that makes that structural rather than a thing each test remembers.
        let page = Benchmarks::default();
        assert!(!page.configured);
        assert!(
            !page.keeps_history(),
            "an unconfigured page must not write to the history"
        );

        // Configured, the config's own default applies — which is on.
        let mut page = Benchmarks::default();
        page.configure(&crate::config::Config::default());
        assert!(page.keeps_history());
    }

    #[test]
    fn history_off_is_stated_rather_than_implied_by_an_absent_row() {
        let settings = Settings {
            history: false,
            ..Settings::default()
        };
        let node = comparison(&[], &settings).expect("the section is always present");
        let history = node
            .children
            .iter()
            .find(|child| child.label == "History")
            .and_then(|child| child.value.as_ref())
            .map(|value| value.to_string())
            .expect("history is always stated");
        assert_eq!(history, "off — results are not saved");
    }

    #[test]
    fn nothing_is_sent_because_nothing_can_send() {
        // The structural guarantee behind the claim on the page: there is no
        // network code in the crate to send anything with. If this ever
        // fails, the sentence on the Benchmarks page has become a lie.
        let source = include_str!("../../Cargo.toml");
        for client in ["reqwest", "ureq", "hyper", "curl", "isahc", "attohttpc"] {
            assert!(
                !source.contains(client),
                "{client} is a dependency now — the Benchmarks page claims nothing is sent"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unrun_suite_explains_how_to_start_it() {
        let mut page = Benchmarks::default();
        let nodes = page.collect().expect("collection failed");

        assert_eq!(nodes[0].label, "Benchmarks");
        let status = nodes[0]
            .children
            .iter()
            .find(|c| c.label == "Status")
            .unwrap()
            .value
            .as_ref()
            .unwrap()
            .to_string();
        assert_eq!(status, "not run yet");
        assert!(nodes[0].children.iter().any(|c| c.label == "Start"));
    }

    #[test]
    fn nothing_runs_without_being_asked() {
        // The behaviour this page exists to get right: merely collecting must
        // not start a benchmark.
        let mut page = Benchmarks::default();
        page.collect().expect("collection failed");
        assert!(!page.runner.is_running());
        assert_eq!(page.runner.with_state(|s| s.results.len()).unwrap(), 0);
    }

    #[test]
    fn the_action_label_tracks_the_run_state() {
        let page = Benchmarks::default();
        assert_eq!(page.action(), Some("run benchmarks"));
        // Idle pages are not re-collected on a timer.
        assert!(page.refresh_interval().is_none());
    }

    #[test]
    fn results_appear_grouped_once_the_suite_finishes() {
        let mut page = Benchmarks::default();
        page.activate();

        while page.runner.is_running() {
            std::thread::sleep(Duration::from_millis(100));
        }
        let nodes = page.collect().expect("collection failed");

        // Status, one section per kind exercised, then the explanation.
        let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labels[0], "Benchmarks");
        assert_eq!(labels.last(), Some(&"About these results"));
        for kind in Kind::ALL {
            assert!(
                labels.contains(&kind.title()),
                "{kind:?} missing: {labels:?}"
            );
        }

        // Every registered benchmark reported a figure with a direction.
        // Counted from the kind sections by name rather than from everything
        // between the status and the explanation: the comparison section sits
        // there too and its rows are not results.
        let results: Vec<&Node> = nodes
            .iter()
            .filter(|section| Kind::ALL.iter().any(|kind| kind.title() == section.label))
            .collect();
        let reported: usize = results.iter().map(|section| section.children.len()).sum();
        assert_eq!(reported, registry().len());
        for section in &results {
            for field in &section.children {
                let value = field.value.as_ref().unwrap().to_string();
                assert!(
                    value.contains("higher is better") || value.contains("lower is better"),
                    "{}: {value}",
                    field.label
                );
            }
        }
    }
}

impl Benchmarks {
    /// Whether this run should be written to the history.
    fn keeps_history(&self) -> bool {
        self.configured && self.settings.history
    }
}

/// A result, with how it compares against this machine's own past.
///
/// The comparison is appended to the figure rather than given a row of its
/// own: it is a property of that number, and a parallel list of deltas would
/// have to be read back against the results by eye.
fn describe(
    name: &str,
    kind: Kind,
    measurement: &Measurement,
    previous: &[history::Run],
) -> String {
    match history::compare(previous, name, kind, measurement) {
        Some(change) => format!("{}  ({change})", measurement.display()),
        None => measurement.display(),
    }
}

/// What is kept, and what is sent.
///
/// Stated on the page rather than left to the config file, because "nothing
/// leaves this machine" is a claim worth being able to check from inside the
/// program that would be doing the leaving.
fn comparison(previous: &[history::Run], settings: &Settings) -> Option<Node> {
    let mut node = Node::group("Comparison", vec![]);

    if settings.history {
        match history::path() {
            Some(path) => node.push(crate::model::Node::field(
                "History",
                format!("{} runs kept in {}", settings.history_limit, path.display()),
            )),
            None => node.push(Node::field(
                "History",
                "on, but no state directory could be determined — nothing is being kept",
            )),
        }
        if !previous.is_empty() {
            let runs: Vec<String> = previous
                .iter()
                .rev()
                .take(5)
                .map(|run| {
                    let when = crate::model::format_date(run.at);
                    match &run.kernel {
                        Some(kernel) => format!("{when} (Linux {kernel})"),
                        None => when,
                    }
                })
                .collect();
            node.push(Node::field("Earlier runs", crate::model::Value::List(runs)));
        }
    } else {
        node.push(Node::field("History", "off — results are not saved"));
    }

    // Three distinct states, and none of them may look like any other. A
    // setting that is on while nothing happens is the failure this spells out.
    node.push(Node::field(
        "Sharing",
        if !settings.share {
            "off — results stay on this machine".to_string()
        } else if settings.share_endpoint.is_empty() {
            "on, but benchmarks.share_endpoint is empty — nothing can be sent".to_string()
        } else {
            format!(
                "on for {}, but uploading is not implemented in this build — nothing is sent",
                settings.share_endpoint
            )
        },
    ));
    Some(node)
}
