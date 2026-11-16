//! Benchmarks.
//!
//! Unlike every other page, these *do* work rather than reading it, which the
//! synchronous collector model cannot accommodate: a run takes seconds and
//! would freeze the interface. So a suite runs on a worker thread and
//! publishes progress into shared state, which the Benchmarks collector reads
//! on each tick. Nothing starts until the user asks for it.
//!
//! Results are reported in real physical units — MiB/s, nanoseconds, millions
//! of operations per second — not as an invented score. A score is only
//! comparable against a database of other people's scores; "412 MiB/s" is
//! comparable against any other machine you run this on, and against the
//! hardware's own specifications.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub mod cpu;
pub mod fpu;
pub mod history;
pub mod memory;
pub mod storage;

/// Which part of the machine a benchmark exercises.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Cpu,
    Fpu,
    Memory,
    Storage,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Cpu, Kind::Fpu, Kind::Memory, Kind::Storage];

    pub fn title(self) -> &'static str {
        match self {
            Kind::Cpu => "Processor",
            Kind::Fpu => "Floating point",
            Kind::Memory => "Memory",
            Kind::Storage => "Storage",
        }
    }
}

/// One measurement.
#[derive(Debug, Clone)]
pub struct Measurement {
    /// The figure, already in its unit.
    pub value: f64,
    pub unit: &'static str,
    /// Higher is better for throughput, lower for latency. Needed because the
    /// two read in opposite directions and a bare number does not say which.
    pub higher_is_better: bool,
    /// What was actually measured, where that is not obvious.
    pub note: Option<String>,
}

impl Measurement {
    pub fn throughput(value: f64, unit: &'static str) -> Self {
        Self {
            value,
            unit,
            higher_is_better: true,
            note: None,
        }
    }

    pub fn latency(value: f64, unit: &'static str) -> Self {
        Self {
            value,
            unit,
            higher_is_better: false,
            note: None,
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// The figure as it should be shown, with the unit and direction.
    pub fn display(&self) -> String {
        let magnitude = if self.value >= 1000.0 {
            format!("{:.0}", self.value)
        } else if self.value >= 10.0 {
            format!("{:.1}", self.value)
        } else {
            format!("{:.2}", self.value)
        };
        let direction = if self.higher_is_better {
            "higher is better"
        } else {
            "lower is better"
        };
        match &self.note {
            Some(note) => format!("{magnitude} {} · {direction} · {note}", self.unit),
            None => format!("{magnitude} {} · {direction}", self.unit),
        }
    }
}

/// A single benchmark.
pub trait Benchmark: Send + Sync {
    fn name(&self) -> &'static str;
    fn kind(&self) -> Kind;

    /// Do the work and measure it.
    ///
    /// Implementations must warm up before timing and must consume their own
    /// result, or the optimiser is entitled to delete the whole computation.
    fn run(&self) -> Measurement;

    /// How many times to repeat, keeping the best result.
    ///
    /// A single timed pass picks up whatever else the scheduler was doing:
    /// measured across two runs, Fibonacci varied by 77 % and Zlib by 22 %.
    /// Taking the best of several is the usual remedy — the fastest run is
    /// the one least disturbed by other work.
    ///
    /// Benchmarks that already average over millions of iterations are stable
    /// on their own and override this to 1.
    fn repeats(&self) -> usize {
        3
    }
}

/// Run a benchmark its requested number of times and keep the best result.
///
/// "Best" depends on direction: the highest throughput, or the lowest latency.
pub fn best_of(benchmark: &dyn Benchmark) -> Measurement {
    let mut best = benchmark.run();
    for _ in 1..benchmark.repeats().max(1) {
        let next = benchmark.run();
        let improved = if next.higher_is_better {
            next.value > best.value
        } else {
            next.value < best.value
        };
        if improved {
            best = next;
        }
    }
    best
}

/// How far a suite has got.
pub enum Progress {
    /// Nothing has been asked for yet.
    Idle,
    Running {
        /// Name of the benchmark currently executing.
        current: &'static str,
        done: usize,
        total: usize,
    },
    Finished {
        elapsed: Duration,
    },
}

/// Shared state between the worker thread and the collector.
pub struct Suite {
    pub progress: Progress,
    /// Completed measurements, in the order they were run.
    pub results: Vec<(&'static str, Kind, Measurement)>,
}

impl Suite {
    fn new() -> Self {
        Self {
            progress: Progress::Idle,
            results: Vec::new(),
        }
    }
}

/// Owns the worker thread and the state it publishes into.
pub struct Runner {
    state: Arc<Mutex<Suite>>,
    /// Set while a worker is alive, so a second Enter cannot start a
    /// concurrent run and interleave its results.
    busy: Arc<AtomicBool>,
}

impl Default for Runner {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(Suite::new())),
            busy: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Runner {
    /// Start the suite, unless one is already running.
    pub fn start(&self) {
        // compare_exchange rather than load-then-store: two keypresses in the
        // same tick would otherwise both see "not busy" and spawn.
        if self
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let state = Arc::clone(&self.state);
        let busy = Arc::clone(&self.busy);

        std::thread::spawn(move || {
            let benchmarks = registry();
            let total = benchmarks.len();
            let started = Instant::now();

            if let Ok(mut suite) = state.lock() {
                suite.results.clear();
                suite.progress = Progress::Running {
                    current: benchmarks[0].name(),
                    done: 0,
                    total,
                };
            }

            for (index, benchmark) in benchmarks.iter().enumerate() {
                // Publish what is about to run before running it, so the
                // interface names the benchmark you are waiting on.
                if let Ok(mut suite) = state.lock() {
                    suite.progress = Progress::Running {
                        current: benchmark.name(),
                        done: index,
                        total,
                    };
                }

                let measurement = best_of(benchmark.as_ref());

                if let Ok(mut suite) = state.lock() {
                    suite
                        .results
                        .push((benchmark.name(), benchmark.kind(), measurement));
                }
            }

            if let Ok(mut suite) = state.lock() {
                suite.progress = Progress::Finished {
                    elapsed: started.elapsed(),
                };
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    pub fn is_running(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// Read the shared state. Never blocks for long: the worker holds the lock
    /// only to publish, never while benchmarking.
    pub fn with_state<T>(&self, f: impl FnOnce(&Suite) -> T) -> Option<T> {
        self.state.lock().ok().map(|suite| f(&suite))
    }
}

/// Every benchmark, in run order.
pub fn registry() -> Vec<Box<dyn Benchmark>> {
    vec![
        Box::new(cpu::Blowfish::single()),
        Box::new(cpu::Blowfish::multi()),
        Box::new(cpu::CryptoHash::single()),
        Box::new(cpu::CryptoHash::multi()),
        Box::new(cpu::Zlib),
        Box::new(cpu::Fibonacci),
        Box::new(cpu::NQueens),
        Box::new(fpu::Fft),
        Box::new(fpu::Raytrace),
        Box::new(memory::Bandwidth),
        Box::new(memory::CacheLatency),
        Box::new(storage::SequentialWrite),
    ]
}

/// Run a closure on every logical CPU and return the wall-clock time.
///
/// Used by the multi-core variants. Threads are spawned per run rather than
/// pooled: the work items are seconds long, so spawn cost is immaterial and a
/// pool would be state to get wrong.
pub fn in_parallel(
    threads: usize,
    work: impl Fn(usize) + Send + Sync + Clone + 'static,
) -> Duration {
    let started = Instant::now();
    let mut handles = Vec::with_capacity(threads);

    for id in 0..threads {
        let work = work.clone();
        handles.push(std::thread::spawn(move || work(id)));
    }
    for handle in handles {
        let _ = handle.join();
    }
    started.elapsed()
}

/// Logical CPUs available for the multi-core variants.
pub fn cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurements_state_their_direction() {
        let up = Measurement::throughput(412.5, "MiB/s");
        assert!(up.display().contains("412"), "{}", up.display());
        assert!(up.display().contains("higher is better"));

        let down = Measurement::latency(1.23, "ns");
        assert!(down.display().contains("1.23"), "{}", down.display());
        assert!(down.display().contains("lower is better"));
    }

    #[test]
    fn display_scales_precision_to_magnitude() {
        // Three significant figures regardless of size, so a 4-digit
        // throughput does not carry meaningless decimals.
        assert!(
            Measurement::throughput(12345.6, "MiB/s")
                .display()
                .starts_with("12346 ")
        );
        assert!(
            Measurement::throughput(42.42, "MiB/s")
                .display()
                .starts_with("42.4 ")
        );
        assert!(
            Measurement::throughput(1.234, "GHz")
                .display()
                .starts_with("1.23 ")
        );
    }

    #[test]
    fn notes_are_appended_when_present() {
        let m = Measurement::throughput(100.0, "MiB/s").with_note("8 threads");
        assert!(m.display().ends_with("8 threads"), "{}", m.display());
    }

    #[test]
    fn registry_names_are_unique() {
        // Results are keyed by name in the display, so a duplicate would make
        // two different measurements indistinguishable.
        let names: Vec<&str> = registry().iter().map(|b| b.name()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate name in {names:?}");
    }

    #[test]
    fn every_kind_has_at_least_one_benchmark() {
        let kinds: Vec<Kind> = registry().iter().map(|b| b.kind()).collect();
        for kind in Kind::ALL {
            assert!(kinds.contains(&kind), "nothing exercises {kind:?}");
        }
    }

    #[test]
    fn a_runner_starts_idle_and_refuses_to_double_start() {
        let runner = Runner::default();
        assert!(!runner.is_running());
        assert!(
            runner
                .with_state(|s| matches!(s.progress, Progress::Idle))
                .unwrap()
        );

        runner.start();
        // A second start while busy must not spawn a second worker, or the two
        // would interleave results into the same vector.
        runner.start();
        assert!(runner.is_running());

        // Let it finish so the thread does not outlive the test process.
        while runner.is_running() {
            std::thread::sleep(Duration::from_millis(50));
        }
        let count = runner.with_state(|s| s.results.len()).unwrap();
        assert_eq!(
            count,
            registry().len(),
            "every benchmark should report once"
        );
    }

    #[test]
    fn parallel_work_runs_on_every_thread() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&counter);
        in_parallel(4, move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(counter.load(Ordering::SeqCst), 4);
    }
}
