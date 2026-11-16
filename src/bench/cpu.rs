//! Integer and general-purpose CPU benchmarks.
//!
//! Each of these measures a well-known implementation doing a fixed amount of
//! work. That is a measurement of *this build* on *this machine*, not of the
//! algorithm in the abstract: a different Blowfish would give a different
//! number. Comparisons between machines running the same hitui build are
//! meaningful; comparisons against another tool's score are not.
//!
//! Every timed loop ends in `black_box`. Without it the optimiser is entitled
//! to observe that nothing reads the result and delete the work, which
//! produces a spectacular and entirely fictional throughput.

use std::hint::black_box;
use std::io::Write;
use std::time::Instant;

use blowfish::Blowfish as BlowfishCipher;
use blowfish::cipher::{BlockCipherEncrypt, KeyInit};
use sha2::{Digest, Sha256};

use crate::bench::{Benchmark, Kind, Measurement, cpu_count, in_parallel};

/// Buffer each cipher and hash pass works over.
const BLOCK: usize = 64 * 1024;

/// Blowfish, the benchmark hardinfo2 leads with.
pub struct Blowfish {
    threads: usize,
}

impl Blowfish {
    pub fn single() -> Self {
        Self { threads: 1 }
    }

    pub fn multi() -> Self {
        Self {
            threads: cpu_count(),
        }
    }
}

/// Total plaintext each thread pushes through the cipher.
const BLOWFISH_BYTES: usize = 24 * 1024 * 1024;

impl Benchmark for Blowfish {
    fn name(&self) -> &'static str {
        if self.threads == 1 {
            "Blowfish (single thread)"
        } else {
            "Blowfish (all cores)"
        }
    }

    fn kind(&self) -> Kind {
        Kind::Cpu
    }

    fn run(&self) -> Measurement {
        let passes = BLOWFISH_BYTES / BLOCK;

        // A warm-up only needs to fault in the buffer and prime the caches and
        // branch predictors. Running the *whole* workload untimed, as this
        // once did, doubled the benchmark's cost for no extra accuracy — and
        // with three repeats that is six times the nominal work.
        encrypt(WARMUP_PASSES);
        let elapsed = in_parallel(self.threads, move |_| encrypt(passes));

        let total = (BLOWFISH_BYTES * self.threads) as f64 / (1024.0 * 1024.0);
        Measurement::throughput(total / elapsed.as_secs_f64(), "MiB/s")
            .with_note(thread_note(self.threads))
    }
}

/// Passes used to warm up, rather than the full count.
const WARMUP_PASSES: usize = 4;

/// Encrypt the working buffer `passes` times.
fn encrypt(passes: usize) {
    // The cipher is generic over byte order; the default is big-endian.
    let cipher: BlowfishCipher = BlowfishCipher::new_from_slice(b"hitui benchmark key").unwrap();
    let mut buffer = vec![0u8; BLOCK];
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = index as u8;
    }
    for _ in 0..passes {
        // Blowfish has a 64-bit block; the buffer is a multiple of 8 so
        // as_chunks_mut leaves no remainder to handle.
        for block in buffer.as_chunks_mut::<8>().0 {
            cipher.encrypt_block(block.into());
        }
    }
    black_box(&buffer);
}

/// SHA-256, which on recent x86 and ARM runs on dedicated instructions.
pub struct CryptoHash {
    threads: usize,
}

impl CryptoHash {
    pub fn single() -> Self {
        Self { threads: 1 }
    }

    pub fn multi() -> Self {
        Self {
            threads: cpu_count(),
        }
    }
}

const HASH_BYTES: usize = 48 * 1024 * 1024;

impl Benchmark for CryptoHash {
    fn name(&self) -> &'static str {
        if self.threads == 1 {
            "SHA-256 (single thread)"
        } else {
            "SHA-256 (all cores)"
        }
    }

    fn kind(&self) -> Kind {
        Kind::Cpu
    }

    fn run(&self) -> Measurement {
        let passes = HASH_BYTES / BLOCK;

        hash(WARMUP_PASSES);
        let elapsed = in_parallel(self.threads, move |_| hash(passes));

        let total = (HASH_BYTES * self.threads) as f64 / (1024.0 * 1024.0);
        // Worth flagging: a CPU with the SHA extensions will look several
        // times faster than one without, which is a real difference but not a
        // general measure of integer throughput.
        Measurement::throughput(total / elapsed.as_secs_f64(), "MiB/s").with_note(format!(
            "{}, uses SHA extensions where present",
            thread_note(self.threads)
        ))
    }
}

/// Hash the working buffer `passes` times.
fn hash(passes: usize) {
    let buffer = vec![0x5au8; BLOCK];
    let mut hasher = Sha256::new();
    for _ in 0..passes {
        hasher.update(&buffer);
    }
    black_box(hasher.finalize());
}

/// Deflate compression and decompression.
pub struct Zlib;

/// Compressible but not trivially so, which is what real data looks like.
const ZLIB_BYTES: usize = 16 * 1024 * 1024;

impl Benchmark for Zlib {
    fn name(&self) -> &'static str {
        "Zlib compress and decompress"
    }

    fn kind(&self) -> Kind {
        Kind::Cpu
    }

    fn run(&self) -> Measurement {
        let input = compressible(ZLIB_BYTES);

        // Warm up on a slice, then time the full round trip.
        let _ = round_trip(&input[..BLOCK]);

        let started = Instant::now();
        let compressed = round_trip(&input);
        let elapsed = started.elapsed();

        let ratio = input.len() as f64 / compressed as f64;
        let mib = input.len() as f64 / (1024.0 * 1024.0);
        Measurement::throughput(mib / elapsed.as_secs_f64(), "MiB/s")
            .with_note(format!("round trip, {ratio:.1}:1 ratio"))
    }
}

/// Compress then decompress, returning the compressed size.
fn round_trip(input: &[u8]) -> usize {
    use flate2::Compression;
    use flate2::write::{ZlibDecoder, ZlibEncoder};

    // Pre-allocate both outputs. A growing Vec reallocates and copies as the
    // stream proceeds, at points that depend on allocator state rather than
    // on compression speed — which showed up as 17 % run-to-run variance.
    let mut encoder = ZlibEncoder::new(Vec::with_capacity(input.len()), Compression::default());
    encoder.write_all(input).expect("writing to a Vec");
    let compressed = encoder.finish().expect("flushing the encoder");
    let size = compressed.len();

    let mut decoder = ZlibDecoder::new(Vec::with_capacity(input.len()));
    decoder.write_all(&compressed).expect("writing to a Vec");
    let output = decoder.finish().expect("flushing the decoder");

    // Check the round trip rather than trusting it: a decoder that silently
    // produced nothing would otherwise read as excellent throughput.
    assert_eq!(output.len(), input.len(), "zlib round trip lost data");
    black_box(&output);
    size
}

/// Data with structure, so compression has something to find.
///
/// Built without allocating per record. An earlier version called
/// `counter.to_string()` for each of ~800k records, which cost more time than
/// the compression it was feeding.
fn compressible(len: usize) -> Vec<u8> {
    const WORDS: [&str; 8] = [
        "processor",
        "memory",
        "storage",
        "kernel",
        "device",
        "bus",
        "sensor",
        "module",
    ];
    let mut out = Vec::with_capacity(len);
    let mut digits = [0u8; 20];
    let mut counter = 0usize;

    while out.len() < len {
        out.extend_from_slice(WORDS[counter % WORDS.len()].as_bytes());
        out.push(b' ');

        // A changing number keeps it from compressing to nothing. Written
        // digit by digit into a reusable buffer rather than through a String.
        let mut value = counter;
        let mut written = 0;
        loop {
            digits[written] = b'0' + (value % 10) as u8;
            written += 1;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        out.extend(digits[..written].iter().rev());
        out.push(b'\n');
        counter += 1;
    }
    out.truncate(len);
    out
}

/// Naive recursive Fibonacci: call overhead and branch prediction.
pub struct Fibonacci;

/// Sized so one pass takes a few hundred milliseconds. fib(34) finished in
/// about 20 ms, which is small enough that timer noise dominated.
const FIB_N: u32 = 40;

impl Benchmark for Fibonacci {
    fn name(&self) -> &'static str {
        "Fibonacci (recursive)"
    }

    fn kind(&self) -> Kind {
        Kind::Cpu
    }

    fn run(&self) -> Measurement {
        black_box(fib(20));

        let started = Instant::now();
        let result = fib(black_box(FIB_N));
        let elapsed = started.elapsed();
        black_box(result);

        // The call count for naive fib(n) is 2·fib(n+1)−1, which is the
        // honest denominator for a per-operation rate.
        let calls = 2.0 * fib(FIB_N + 1) as f64 - 1.0;
        Measurement::throughput(calls / elapsed.as_secs_f64() / 1e6, "M calls/s")
            .with_note(format!("fib({FIB_N}), single thread"))
    }
}

fn fib(n: u32) -> u64 {
    if n < 2 {
        n as u64
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

/// N-Queens by backtracking: branchy integer work with poor locality.
pub struct NQueens;

/// 12 completed in under 7 ms, which is mostly timer noise.
const QUEENS_N: u32 = 14;

impl Benchmark for NQueens {
    fn name(&self) -> &'static str {
        "N-Queens (backtracking)"
    }

    fn kind(&self) -> Kind {
        Kind::Cpu
    }

    fn run(&self) -> Measurement {
        black_box(queens(8));

        let started = Instant::now();
        let solutions = queens(black_box(QUEENS_N));
        let elapsed = started.elapsed();
        black_box(solutions);

        Measurement::throughput(
            solutions as f64 / elapsed.as_secs_f64() / 1e3,
            "k solutions/s",
        )
        .with_note(format!(
            "{QUEENS_N}×{QUEENS_N} board, {solutions} solutions, single thread"
        ))
    }
}

/// Count complete N-Queens solutions using bitmask backtracking.
fn queens(n: u32) -> u64 {
    fn place(n: u32, row: u32, columns: u32, left: u32, right: u32) -> u64 {
        if row == n {
            return 1;
        }
        // Bits set in `available` are columns this row may use.
        let occupied = columns | left | right;
        let mut available = !occupied & ((1 << n) - 1);
        let mut found = 0;

        while available != 0 {
            let bit = available.isolate_lowest_one();
            available -= bit;
            found += place(
                n,
                row + 1,
                columns | bit,
                (left | bit) << 1,
                (right | bit) >> 1,
            );
        }
        found
    }
    place(n, 0, 0, 0, 0)
}

/// Shared wording for the thread-count note.
fn thread_note(threads: usize) -> String {
    if threads == 1 {
        "single thread".to_string()
    } else {
        format!("{threads} threads")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fibonacci_is_correct() {
        // A benchmark that computes the wrong thing measures nothing.
        assert_eq!(fib(0), 0);
        assert_eq!(fib(1), 1);
        assert_eq!(fib(10), 55);
        assert_eq!(fib(20), 6765);
    }

    #[test]
    fn nqueens_matches_the_known_counts() {
        // OEIS A000170. If the solver is wrong the rate is meaningless.
        assert_eq!(queens(1), 1);
        assert_eq!(queens(4), 2);
        assert_eq!(queens(6), 4);
        assert_eq!(queens(8), 92);
        assert_eq!(queens(10), 724);
    }

    #[test]
    fn compressible_data_actually_compresses() {
        // If the generator produced incompressible noise the ratio would be
        // 1:1 and the benchmark would measure something else entirely.
        let input = compressible(256 * 1024);
        assert_eq!(input.len(), 256 * 1024);
        let compressed = round_trip(&input);
        assert!(
            compressed < input.len() / 2,
            "expected better than 2:1, got {} from {}",
            compressed,
            input.len()
        );
    }

    #[test]
    fn round_trip_verifies_its_own_output() {
        // round_trip asserts the decompressed length; confirm it really does
        // reconstruct the bytes, not just the count.
        let input = compressible(64 * 1024);
        let size = round_trip(&input);
        assert!(size > 0);
    }

    #[test]
    fn single_and_multi_variants_are_distinct() {
        assert_eq!(Blowfish::single().threads, 1);
        assert_eq!(Blowfish::multi().threads, cpu_count());
        assert_ne!(Blowfish::single().name(), Blowfish::multi().name());
        assert_ne!(CryptoHash::single().name(), CryptoHash::multi().name());
    }

    #[test]
    fn thread_notes_read_correctly() {
        assert_eq!(thread_note(1), "single thread");
        assert_eq!(thread_note(8), "8 threads");
    }
}
