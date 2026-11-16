//! Optional external tools.
//!
//! hitui reads the kernel's own interfaces for everything it can. A few facts
//! are not there to be read — drive health, GPU renderer strings, package
//! counts — and for those it will parse the output of the established tool if
//! that tool happens to be installed. Nothing here is required: every caller
//! must degrade to the kernel-only answer when `run` returns `None`.
//!
//! Fields obtained this way are tagged with their source (see
//! [`crate::model::Node::from`]) so the page says where a number came from.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::Duration;

/// How long to let a tool run before giving up on it.
///
/// `smartctl` against a dying disk can block for a long time, and a page that
/// hangs is worse than a page missing a column.
const TIMEOUT: Duration = Duration::from_secs(3);

/// Resolve a program in `PATH`, caching the answer for the process lifetime.
pub fn which(program: &str) -> Option<PathBuf> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Ok(cache) = cache.lock()
        && let Some(hit) = cache.get(program)
    {
        return hit.clone();
    }

    let found = search_path(program);
    if let Ok(mut cache) = cache.lock() {
        cache.insert(program.to_string(), found.clone());
    }
    found
}

fn search_path(program: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| {
            candidate
                .metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Run a tool and capture its stdout, or `None` if it is absent, fails, or
/// outruns [`TIMEOUT`].
///
/// stderr is discarded and stdin is closed: these are informational queries,
/// and a tool that decides to prompt should get EOF rather than block.
pub fn run(program: &str, args: &[&str]) -> Option<String> {
    let path = which(program)?;

    let mut child = Command::new(path)
        .args(args)
        // Several tools colour their output when these say to, regardless of
        // whether stdout is a terminal. `journalctl` with SYSTEMD_COLORS set
        // emits escape sequences into its own JSON, which then fails to parse.
        .env("NO_COLOR", "1")
        .env("SYSTEMD_COLORS", "0")
        .env("CLICOLOR", "0")
        .env("TERM", "dumb")
        // A tool that localises its output would break every string match
        // here, so ask for the C locale.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // Drain the pipe on another thread. Waiting on the child first would
    // deadlock as soon as a tool writes more than the pipe buffer holds.
    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = String::new();
        let mut stdout = stdout;
        let _ = stdout.read_to_string(&mut buffer);
        let _ = tx.send(buffer);
    });

    match rx.recv_timeout(TIMEOUT) {
        Ok(output) => {
            // A non-zero exit means the output is not trustworthy, except that
            // smartctl sets bits in its status for drive warnings while still
            // printing a valid report, so callers check content not status.
            let _ = child.wait();
            (!output.trim().is_empty()).then_some(output)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Whether a tool is available, for reporting what a page could have shown.
pub fn available(program: &str) -> bool {
    which(program).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_program_that_must_exist() {
        // `sh` is on every POSIX system and in PATH in any sane environment.
        assert!(which("sh").is_some(), "sh should resolve in PATH");
        assert!(available("sh"));
    }

    #[test]
    fn missing_programs_resolve_to_none() {
        assert!(which("hitui-definitely-not-a-real-program").is_none());
        assert!(run("hitui-definitely-not-a-real-program", &[]).is_none());
    }

    #[test]
    fn captures_stdout() {
        let output = run("sh", &["-c", "echo hello"]).expect("sh should run");
        assert_eq!(output.trim(), "hello");
    }

    #[test]
    fn empty_output_is_treated_as_no_answer() {
        // A tool that prints nothing has told us nothing.
        assert!(run("sh", &["-c", "exit 0"]).is_none());
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_does_not_deadlock() {
        // The classic failure: waiting on the child before draining stdout.
        // 256 KiB comfortably exceeds the 64 KiB default pipe capacity.
        let output = run("sh", &["-c", "yes abcdefgh | head -c 262144"]).expect("sh should run");
        assert_eq!(output.len(), 262_144);
    }

    #[test]
    fn colour_and_locale_are_forced_off_for_the_child() {
        // The bug this guards: journalctl honours SYSTEMD_COLORS even when
        // piped, putting escape sequences inside its JSON output.
        let env =
            run("sh", &["-c", "echo $NO_COLOR:$SYSTEMD_COLORS:$LC_ALL"]).expect("sh should run");
        assert_eq!(env.trim(), "1:0:C");
    }

    #[test]
    fn a_hanging_tool_is_killed_rather_than_waited_on() {
        let start = std::time::Instant::now();
        assert!(run("sh", &["-c", "sleep 60"]).is_none());
        assert!(
            start.elapsed() < TIMEOUT + Duration::from_secs(2),
            "should have given up after the timeout, took {:?}",
            start.elapsed()
        );
    }
}
