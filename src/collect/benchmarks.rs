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

use crate::bench::{Kind, Progress, Runner, registry};
use crate::collect::{Collector, Group};
use crate::model::Node;

#[derive(Default)]
pub struct Benchmarks {
    runner: Runner,
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

    fn activate(&mut self) {
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
        let Some(nodes) = self.runner.with_state(|suite| {
            let mut nodes = vec![status(&suite.progress)];

            // Group finished results by what they exercise, in a fixed order
            // so the page does not reshuffle as results arrive.
            for kind in Kind::ALL {
                let mut section = Node::group(kind.title(), vec![]);
                for (name, _, measurement) in suite.results.iter().filter(|(_, k, _)| *k == kind) {
                    section.push(Node::field(*name, measurement.display()));
                }
                if !section.children.is_empty() {
                    nodes.push(section);
                }
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
pub fn run_now() -> Vec<Node> {
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

    let mut nodes = vec![status(&Progress::Finished {
        elapsed: started.elapsed(),
    })];
    for kind in Kind::ALL {
        let mut section = Node::group(kind.title(), vec![]);
        for (name, _, measurement) in results.iter().filter(|(_, k, _)| *k == kind) {
            section.push(Node::field(*name, measurement.display()));
        }
        if !section.children.is_empty() {
            nodes.push(section);
        }
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
        let reported: usize = nodes[1..nodes.len() - 1]
            .iter()
            .map(|section| section.children.len())
            .sum();
        assert_eq!(reported, registry().len());
        for section in &nodes[1..nodes.len() - 1] {
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
