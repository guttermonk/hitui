//! Storage throughput.
//!
//! Only writes are measured, and deliberately so. A read-back benchmark
//! without `O_DIRECT` or a cache drop measures the page cache — it reports
//! several GiB/s on a mechanical disk, which is true of the cache and
//! worthless as a statement about the drive. Dropping caches needs root, and
//! `O_DIRECT` needs alignment handling that would not make the number any
//! more honest. So this writes and fsyncs, which really does reach the
//! device, and says what it did not measure.

use std::fs::File;
use std::io::Write;
use std::time::Instant;

use crate::bench::{Benchmark, Kind, Measurement};

pub struct SequentialWrite;

/// Total written per run. Large enough to outlast any write cache, small
/// enough not to abuse the disk.
const TOTAL: usize = 64 * 1024 * 1024;
const CHUNK: usize = 1024 * 1024;

impl Benchmark for SequentialWrite {
    fn name(&self) -> &'static str {
        "Sequential write"
    }

    fn kind(&self) -> Kind {
        Kind::Storage
    }

    fn repeats(&self) -> usize {
        // Deliberately not repeated. Best-of-N removes *measurement* noise,
        // but a drive's run-to-run variation is the device's own caching: an
        // SSD absorbs the first burst into its SLC cache and slows once that
        // fills. Measured across three passes this reported 326 then 829
        // MiB/s, and taking the best would have published the cache speed as
        // though it were sustained throughput. One honest pass instead — and
        // a third of the write wear.
        1
    }

    fn run(&self) -> Measurement {
        // Written into the temp directory, so it lands on whichever
        // filesystem that is — often tmpfs, which is memory and not a disk at
        // all. Say which, because it changes what the number means.
        let directory = std::env::temp_dir();
        let path = directory.join(format!("hitui-bench-{}", std::process::id()));

        let buffer = vec![0xa5u8; CHUNK];
        let started = Instant::now();

        let result = (|| -> std::io::Result<()> {
            let mut file = File::create(&path)?;
            for _ in 0..(TOTAL / CHUNK) {
                file.write_all(&buffer)?;
            }
            // Without this the timing ends when the last byte reaches the page
            // cache, not the device.
            file.sync_all()?;
            Ok(())
        })();

        let elapsed = started.elapsed();
        // Clean up whether or not the write succeeded.
        let _ = std::fs::remove_file(&path);

        if let Err(error) = result {
            return Measurement::throughput(0.0, "MiB/s").with_note(format!("failed: {error}"));
        }

        let mib = TOTAL as f64 / (1024.0 * 1024.0);
        Measurement::throughput(mib / elapsed.as_secs_f64(), "MiB/s").with_note(format!(
            "{} MiB fsynced to {}; read speed not measured, it would hit the page cache",
            TOTAL / (1024 * 1024),
            backing_filesystem(&directory)
        ))
    }
}

/// Which filesystem a path sits on, so the reader knows whether this was a
/// disk at all.
fn backing_filesystem(path: &std::path::Path) -> String {
    let target = path.to_string_lossy();

    // The longest matching mount point is the one that governs the path.
    let mut best: Option<(usize, String, String)> = None;
    for mount in crate::collect::filesystems::mounts() {
        if target.starts_with(&mount.mount_point) {
            let length = mount.mount_point.len();
            if best
                .as_ref()
                .is_none_or(|(existing, _, _)| length > *existing)
            {
                best = Some((length, mount.mount_point.clone(), mount.fs_type.clone()));
            }
        }
    }

    match best {
        Some((_, mount_point, fs_type)) if fs_type == "tmpfs" || fs_type == "ramfs" => {
            format!("{mount_point} ({fs_type} — this is RAM, not a disk)")
        }
        Some((_, mount_point, fs_type)) => format!("{mount_point} ({fs_type})"),
        None => target.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_the_filesystem_behind_a_path() {
        let described = backing_filesystem(&std::env::temp_dir());
        assert!(!described.is_empty());
        // Either a mount point with its type, or the raw path as a fallback.
        assert!(
            described.contains('(') || described.starts_with('/'),
            "{described}"
        );
    }

    #[test]
    fn a_memory_backed_temp_dir_is_called_out() {
        // The honesty requirement: writing to tmpfs at 5 GiB/s is a true
        // measurement of memory and a false one about storage, so the note
        // has to say so.
        let described = backing_filesystem(std::path::Path::new("/dev/shm"));
        if described.contains("tmpfs") {
            assert!(described.contains("not a disk"), "{described}");
        }
    }

    #[test]
    fn root_is_resolved_to_the_longest_matching_mount() {
        // /nix/store is a longer match than / on this machine; the governing
        // mount is the longest prefix, not the first one found.
        let described = backing_filesystem(std::path::Path::new("/"));
        assert!(described.starts_with('/'), "{described}");
    }

    #[test]
    fn the_benchmark_cleans_up_after_itself() {
        // Run the real thing and confirm no file is left behind: a benchmark
        // that leaks 128 MiB per run would be a bug worth catching.
        let measurement = SequentialWrite.run();
        assert!(measurement.higher_is_better);

        let leftover = std::env::temp_dir().join(format!("hitui-bench-{}", std::process::id()));
        assert!(!leftover.exists(), "left {leftover:?} behind");
    }
}
