//! Benchmark results kept between runs, so a run can be compared with the
//! machine's own past.
//!
//! This is the local half of what hardinfo2 does by uploading to a shared
//! database. It answers the question that database is usually reached for —
//! "is this machine slower than it was" — without a third party, because the
//! most useful comparison for a given machine is against itself: same
//! hardware, same workload, one variable changed. A number from someone
//! else's laptop differs for reasons nobody has recorded.
//!
//! Nothing is written unless a benchmark was explicitly run, and the file is
//! plain JSON under `$XDG_STATE_HOME` that can be deleted at any time without
//! consequence.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::bench::{Kind, Measurement};
use crate::model::unix_now;

/// Below this, a difference is run-to-run noise rather than a change.
///
/// Per kind, because the kinds are not equally repeatable and a single floor
/// for all of them has to be either useless or wrong. Measured while building
/// the suite: before best-of-three, Fibonacci swung 77 % between runs and copy
/// bandwidth 14 %. Best-of-three brought those down but not to nothing, and
/// two consecutive runs on an otherwise busy machine still put bandwidth 35 %
/// apart. Storage is wider again, and is why that benchmark takes no
/// best-of-N at all: an SSD's write cache makes the first pass the fastest, so
/// the spread there is a property of the hardware rather than of the
/// measurement.
///
/// Reporting noise as a trend is worse than saying nothing: it sends someone
/// looking for a regression that was never there.
fn noise_floor(kind: Kind) -> f64 {
    match kind {
        Kind::Cpu | Kind::Fpu => 5.0,
        Kind::Memory => 15.0,
        Kind::Storage => 25.0,
    }
}

/// One stored run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    /// Unix seconds. Stored rather than derived so an old entry keeps its own
    /// date after the file is copied to another machine.
    pub at: i64,
    /// Enough of the machine to tell whether a comparison is apples to
    /// apples: a kernel upgrade between two runs is usually the explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    /// Benchmark name to the figure in its own unit.
    pub results: BTreeMap<String, f64>,
}

/// Where runs are kept.
///
/// `$XDG_STATE_HOME` rather than the config or cache directory: this is state
/// that should survive a reboot but is not precious, which is exactly what
/// that directory is specified for.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(base.join("hitui").join("benchmarks.json"))
}

/// Every stored run, oldest first.
///
/// A missing, unreadable or malformed file is an empty history. It is a
/// convenience, and refusing to run benchmarks because a cache file was
/// truncated would be the tail wagging the dog.
pub fn load() -> Vec<Run> {
    match path() {
        Some(path) => load_from(&path),
        None => Vec::new(),
    }
}

/// The path is a parameter so the persistence can be tested without reaching
/// for `XDG_STATE_HOME`: the environment is process-global and one of the
/// other tests enumerates it on purpose.
fn load_from(path: &std::path::Path) -> Vec<Run> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Append a run and trim the history to `limit`, keeping the newest.
///
/// Returns whether it was written, so the caller can say so rather than
/// claiming a save that silently failed.
pub fn record(results: &[(&'static str, Kind, Measurement)], limit: usize) -> bool {
    match path() {
        Some(path) => record_at(&path, results, limit),
        None => false,
    }
}

fn record_at(
    path: &std::path::Path,
    results: &[(&'static str, Kind, Measurement)],
    limit: usize,
) -> bool {
    let run = Run {
        at: unix_now().unwrap_or_default(),
        kernel: crate::sysfs::read("/proc/sys/kernel/osrelease"),
        cpu: cpu_model(),
        results: results
            .iter()
            .map(|(name, _, measurement)| (name.to_string(), measurement.value))
            .collect(),
    };

    let mut runs = load_from(path);
    runs.push(run);
    // Keep the newest: an old run is only useful as a comparison, and the
    // comparison that matters is with the recent past.
    if runs.len() > limit {
        let excess = runs.len() - limit;
        runs.drain(..excess);
    }

    let Ok(text) = serde_json::to_string_pretty(&runs) else {
        return false;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Written beside and renamed, so an interrupted write cannot leave a
    // half-file where the next run expects JSON.
    let temporary = path.with_extension("json.new");
    if std::fs::write(&temporary, text).is_err() {
        return false;
    }
    std::fs::rename(&temporary, path).is_ok()
}

fn cpu_model() -> Option<String> {
    crate::sysfs::read("/proc/cpuinfo")?
        .lines()
        .find_map(|line| line.strip_prefix("model name"))
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, model)| model.trim().to_string())
}

/// How a figure compares with the most recent earlier run that measured the
/// same thing.
///
/// `None` when there is nothing to compare against, which is the normal state
/// for a first run and must not render as "0 % change".
pub fn compare(runs: &[Run], name: &str, kind: Kind, measurement: &Measurement) -> Option<String> {
    let previous = runs
        .iter()
        .rev()
        .find_map(|run| run.results.get(name).map(|value| (run, *value)))?;
    let (run, before) = previous;
    if before == 0.0 {
        return None;
    }

    let when = crate::model::format_date(run.at);
    let change = (measurement.value - before) / before * 100.0;
    if change.abs() < noise_floor(kind) {
        return Some(format!("unchanged since {when}"));
    }

    // Which direction is an improvement depends on the unit, not on the sign:
    // a latency going up is a regression and a throughput going up is not.
    let better = if measurement.higher_is_better {
        change > 0.0
    } else {
        change < 0.0
    };
    Some(format!(
        "{:.1} % {} than {when}",
        change.abs(),
        if better { "faster" } else { "slower" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(at: i64, name: &str, value: f64) -> Run {
        Run {
            at,
            kernel: None,
            cpu: None,
            results: [(name.to_string(), value)].into_iter().collect(),
        }
    }

    /// 2026-01-01, so the rendered date is stable.
    const WHEN: i64 = 1_767_225_600;

    #[test]
    fn a_first_run_has_nothing_to_compare_with() {
        // Which must not render as "0 % change": there is no previous figure,
        // not an identical one.
        assert_eq!(
            compare(
                &[],
                "Blowfish",
                Kind::Cpu,
                &Measurement::throughput(100.0, "MiB/s")
            ),
            None
        );
        // Nor when the history exists but never measured this benchmark.
        let runs = vec![run(WHEN, "Zlib", 50.0)];
        assert_eq!(
            compare(
                &runs,
                "Blowfish",
                Kind::Cpu,
                &Measurement::throughput(100.0, "MiB/s")
            ),
            None
        );
    }

    #[test]
    fn a_throughput_going_up_is_faster_and_a_latency_going_up_is_slower() {
        // The sign does not say which direction is an improvement; the unit
        // does. This is the gauge-polarity problem again in another costume.
        let runs = vec![run(WHEN, "Copy bandwidth", 100.0)];
        assert_eq!(
            compare(
                &runs,
                "Copy bandwidth",
                Kind::Memory,
                &Measurement::throughput(150.0, "MiB/s")
            )
            .as_deref(),
            Some("50.0 % faster than 2026-01-01")
        );

        let runs = vec![run(WHEN, "L1 latency", 1.0)];
        assert_eq!(
            compare(
                &runs,
                "L1 latency",
                Kind::Memory,
                &Measurement::latency(1.5, "ns")
            )
            .as_deref(),
            Some("50.0 % slower than 2026-01-01")
        );
        // And the other way, so neither direction is hardcoded.
        assert_eq!(
            compare(
                &runs,
                "L1 latency",
                Kind::Memory,
                &Measurement::latency(0.5, "ns")
            )
            .as_deref(),
            Some("50.0 % faster than 2026-01-01")
        );
    }

    #[test]
    fn a_difference_inside_the_noise_floor_is_not_a_trend() {
        // Reporting noise as a change invites someone to go hunting for a
        // regression that is not there.
        let runs = vec![run(WHEN, "Zlib", 100.0)];
        assert_eq!(
            compare(
                &runs,
                "Zlib",
                Kind::Cpu,
                &Measurement::throughput(102.0, "MiB/s")
            )
            .as_deref(),
            Some("unchanged since 2026-01-01")
        );
        assert_eq!(
            compare(
                &runs,
                "Zlib",
                Kind::Cpu,
                &Measurement::throughput(96.0, "MiB/s")
            )
            .as_deref(),
            Some("unchanged since 2026-01-01")
        );
    }

    #[test]
    fn the_noise_floor_follows_what_the_benchmark_can_repeat() {
        // One floor for every kind has to be either useless or wrong. A 10 %
        // move is a real change in a CPU figure and nothing at all in a
        // bandwidth one, which two consecutive runs on a busy machine put
        // 35 % apart.
        let runs = vec![run(WHEN, "x", 100.0)];
        let ten_percent = Measurement::throughput(110.0, "MiB/s");

        assert_eq!(
            compare(&runs, "x", Kind::Cpu, &ten_percent).as_deref(),
            Some("10.0 % faster than 2026-01-01")
        );
        assert_eq!(
            compare(&runs, "x", Kind::Memory, &ten_percent).as_deref(),
            Some("unchanged since 2026-01-01")
        );
        // Storage is wider again: an SSD's write cache makes the first pass
        // the fastest, so a 20 % spread there says nothing about the machine.
        assert_eq!(
            compare(
                &runs,
                "x",
                Kind::Storage,
                &Measurement::throughput(120.0, "MiB/s")
            )
            .as_deref(),
            Some("unchanged since 2026-01-01")
        );
        assert!(noise_floor(Kind::Cpu) < noise_floor(Kind::Memory));
        assert!(noise_floor(Kind::Memory) < noise_floor(Kind::Storage));
    }

    #[test]
    fn the_most_recent_earlier_run_is_the_one_compared_against() {
        // Oldest first in the file, so the comparison has to search backwards.
        let runs = vec![run(WHEN, "Zlib", 50.0), run(WHEN + 86_400, "Zlib", 100.0)];
        assert_eq!(
            compare(
                &runs,
                "Zlib",
                Kind::Cpu,
                &Measurement::throughput(200.0, "MiB/s")
            )
            .as_deref(),
            Some("100.0 % faster than 2026-01-02")
        );
    }

    #[test]
    fn a_zero_baseline_is_not_divided_by() {
        let runs = vec![run(WHEN, "Zlib", 0.0)];
        assert_eq!(
            compare(
                &runs,
                "Zlib",
                Kind::Cpu,
                &Measurement::throughput(10.0, "MiB/s")
            ),
            None
        );
    }

    #[test]
    fn a_malformed_history_file_is_an_empty_history() {
        // It is a convenience. Refusing to benchmark because a cache file was
        // truncated would be the tail wagging the dog.
        assert!(serde_json::from_str::<Vec<Run>>("not json").is_err());
        assert_eq!(
            serde_json::from_str::<Vec<Run>>("not json")
                .unwrap_or_default()
                .len(),
            0
        );
    }

    #[test]
    fn the_limit_trims_the_oldest_runs() {
        // Untested until now, and the one thing in here that silently loses
        // data if it is wrong in the wrong direction.
        let path = std::env::temp_dir().join("hitui-history-limit.json");
        let _ = std::fs::remove_file(&path);

        let results = [("Zlib", Kind::Cpu, Measurement::throughput(100.0, "MiB/s"))];
        for _ in 0..5 {
            assert!(record_at(&path, &results, 2), "recording should succeed");
        }

        let runs = load_from(&path);
        assert_eq!(runs.len(), 2, "the limit is a cap, not a suggestion");
        // The newest are the ones kept: an old run is only useful as a
        // comparison, and the comparison that matters is with the recent past.
        assert!(runs[0].at <= runs[1].at);
        assert_eq!(runs[1].results.get("Zlib"), Some(&100.0));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_interrupted_write_cannot_leave_half_a_file() {
        // Written beside and renamed, so the next run never reads a partial
        // document where it expects JSON.
        let path = std::env::temp_dir().join("hitui-history-atomic.json");
        let _ = std::fs::remove_file(&path);
        let results = [("Zlib", Kind::Cpu, Measurement::throughput(1.0, "MiB/s"))];
        assert!(record_at(&path, &results, 5));

        // The temporary is gone once the rename has happened.
        assert!(!path.with_extension("json.new").exists());
        assert_eq!(load_from(&path).len(), 1);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_state_path_follows_xdg() {
        // Only the shape is asserted: the environment is process-global and
        // this runs alongside every other test.
        if let Some(path) = path() {
            assert!(
                path.ends_with("hitui/benchmarks.json"),
                "{}",
                path.display()
            );
            assert!(path.is_absolute());
        }
    }
}
