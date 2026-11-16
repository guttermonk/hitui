//! Memory bandwidth and cache latency.

use std::hint::black_box;
use std::time::Instant;

use crate::bench::{Benchmark, Kind, Measurement};

/// Sequential copy bandwidth.
pub struct Bandwidth;

/// Comfortably larger than any current last-level cache, so this measures
/// DRAM rather than cache — but small enough that allocating and faulting it
/// in does not cost more than the copy being measured.
const BUFFER: usize = 64 * 1024 * 1024;
/// Enough passes that one timed region is a couple of hundred milliseconds;
/// at 12 the region was 65 ms and varied 14 % between runs.
const PASSES: usize = 36;

impl Benchmark for Bandwidth {
    fn name(&self) -> &'static str {
        "Memory bandwidth (copy)"
    }

    fn kind(&self) -> Kind {
        Kind::Memory
    }

    fn run(&self) -> Measurement {
        let words = BUFFER / std::mem::size_of::<u64>();
        let source: Vec<u64> = (0..words as u64).collect();
        let mut destination = vec![0u64; words];

        // The first touch of a fresh allocation faults in pages, which would
        // otherwise be timed as if it were bandwidth.
        destination.copy_from_slice(&source);

        let started = Instant::now();
        for _ in 0..PASSES {
            destination.copy_from_slice(black_box(&source));
            black_box(&destination);
        }
        let elapsed = started.elapsed();

        // A copy reads a byte and writes a byte, so the traffic over the bus
        // is twice the buffer per pass. Reporting only one side would halve
        // the figure against what the hardware is rated for.
        let bytes = (BUFFER * PASSES * 2) as f64;
        let gib = bytes / (1024.0 * 1024.0 * 1024.0);
        Measurement::throughput(gib / elapsed.as_secs_f64(), "GiB/s")
            .with_note("read + write traffic, single thread")
    }
}

/// Latency at each level of the cache hierarchy.
pub struct CacheLatency;

/// Working-set sizes, chosen to straddle the usual L1/L2/L3/DRAM boundaries.
const SIZES: [usize; 5] = [
    16 * 1024,
    128 * 1024,
    1024 * 1024,
    8 * 1024 * 1024,
    64 * 1024 * 1024,
];

impl Benchmark for CacheLatency {
    fn name(&self) -> &'static str {
        "Memory latency (pointer chase)"
    }

    fn kind(&self) -> Kind {
        Kind::Memory
    }

    // Repeats stay at the default three. Averaging millions of hops within a
    // single pass does not help when the whole pass is disturbed: measured
    // with one pass this swung 19 % between runs, against about 1 % with
    // best-of-three.

    fn run(&self) -> Measurement {
        let mut curve = Vec::with_capacity(SIZES.len());
        for size in SIZES {
            curve.push((size, chase(size)));
        }

        // The headline is the largest working set: main memory, uncached.
        let (_, dram) = *curve.last().expect("SIZES is not empty");

        // The curve is the interesting part — it shows where each cache ends.
        let note = curve
            .iter()
            .map(|(size, ns)| format!("{}: {ns:.1}", human(*size)))
            .collect::<Vec<String>>()
            .join(", ");

        Measurement::latency(dram, "ns").with_note(format!("by working set, ns — {note}"))
    }
}

/// Average latency of a dependent load, in nanoseconds.
///
/// The accesses form a single cycle through the buffer, so each load's address
/// depends on the previous load's result. That dependency is the point: it
/// defeats prefetching and out-of-order execution, which is the only way to
/// measure latency rather than bandwidth.
fn chase(size: usize) -> f64 {
    // One slot per machine word, so the buffer is exactly `size` bytes. An
    // earlier version sized slots per *cache line* while still storing them
    // in a packed Vec<usize>, which made the real working set an eighth of
    // the label — and produced a curve where a 128 KiB "set" was as fast as a
    // 16 KiB one, because both actually fitted in L1.
    let slots = (size / std::mem::size_of::<usize>()).max(2);

    // Build a cycle visiting every slot exactly once, in a scattered order.
    // Random order is what defeats the prefetcher and spreads the accesses
    // over distinct cache lines; a stride would be predicted.
    let mut order: Vec<usize> = (0..slots).collect();
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    for index in (1..slots).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let pick = (seed % (index as u64 + 1)) as usize;
        order.swap(index, pick);
    }

    let mut next = vec![0usize; slots];
    for window in order.windows(2) {
        next[window[0]] = window[1];
    }
    // Close the cycle so the chase can run indefinitely.
    next[order[slots - 1]] = order[0];

    // Enough hops that timer overhead is negligible at every size, while
    // keeping one pass short enough to repeat three times: at DRAM latency
    // this is about 180 ms, and at L1 latency a few milliseconds.
    let hops = 2_000_000usize.max(slots * 2);

    // Warm up: at small sizes this loads the working set into cache, which is
    // exactly what we want to measure the latency of.
    let mut cursor = 0usize;
    for _ in 0..slots {
        cursor = next[cursor];
    }

    let started = Instant::now();
    for _ in 0..hops {
        cursor = next[black_box(cursor)];
    }
    let elapsed = started.elapsed();
    black_box(cursor);

    elapsed.as_secs_f64() * 1e9 / hops as f64
}

/// Compact size label for the curve note.
fn human(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{}M", bytes / (1024 * 1024))
    } else {
        format!("{}K", bytes / 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_labels_are_compact() {
        assert_eq!(human(16 * 1024), "16K");
        assert_eq!(human(8 * 1024 * 1024), "8M");
        assert_eq!(human(64 * 1024 * 1024), "64M");
    }

    #[test]
    fn latency_grows_with_the_working_set() {
        // The property that makes this a cache benchmark: a set that fits in
        // L1 must be markedly faster than one that cannot fit in any cache.
        // Only the two extremes are compared, since the middle depends on the
        // particular cache hierarchy.
        let small = chase(16 * 1024);
        let large = chase(64 * 1024 * 1024);

        assert!(small > 0.0, "L1 latency should be positive, got {small}");
        assert!(
            large > small * 2.0,
            "expected DRAM to be much slower than L1: {large} vs {small} ns"
        );
    }

    #[test]
    fn l1_latency_is_physically_plausible() {
        // The point of an absolute bound is to catch the dependent-load chain
        // being optimised away, which would report a latency far below what
        // any cache can achieve. That only means something in an optimised
        // build: unoptimised, with bounds checks on every hop, this same
        // chase measured 40.8 ns against 2.7 ns in release. So the ceiling
        // follows the profile, while the floor — the part that catches a
        // deleted chain — stays put.
        let ceiling = if cfg!(debug_assertions) { 500.0 } else { 20.0 };

        let small = chase(16 * 1024);
        assert!(
            (0.1..ceiling).contains(&small),
            "implausible L1 latency: {small} ns (ceiling {ceiling})"
        );
    }

    #[test]
    fn the_chase_visits_every_slot_exactly_once() {
        // If the permutation were not a single cycle, part of the working set
        // would never be touched and the measured latency would reflect a
        // smaller set than advertised.
        let size = 16 * 1024;
        let slots = size / std::mem::size_of::<usize>();

        // Rebuild the cycle the same way `chase` does.
        let mut order: Vec<usize> = (0..slots).collect();
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for index in (1..slots).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let pick = (seed % (index as u64 + 1)) as usize;
            order.swap(index, pick);
        }
        let mut next = vec![0usize; slots];
        for window in order.windows(2) {
            next[window[0]] = window[1];
        }
        next[order[slots - 1]] = order[0];

        let mut seen = vec![false; slots];
        let mut cursor = 0;
        for _ in 0..slots {
            assert!(!seen[cursor], "slot {cursor} visited twice before the end");
            seen[cursor] = true;
            cursor = next[cursor];
        }
        assert_eq!(cursor, 0, "the chase should close into a cycle");
        assert!(seen.iter().all(|&s| s), "some slots were never visited");
    }
}
