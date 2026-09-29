//! End-to-end tests for `linear completions`.
//!
//! The case of interest is the closed reader: `linear completions bash | head`
//! is a pipeline a user actually types. It used to exit 101 with a panic,
//! because `clap_complete`'s shell generators `.expect("failed to write
//! completion file")` on any write error and Rust's runtime turns a closed pipe
//! into exactly that error. The fix belongs to the whole binary rather than to
//! this command (`main` restores the default `SIGPIPE` disposition, so a closed
//! reader ends the process the way it does for `ls | head`), and this suite
//! checks the pipeline is quiet either way: the script comes out, and a reader
//! that stops early produces neither a panic nor a complaint on stderr.

mod common;

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};

use common::run_cli;

/// A closed reader must not produce a panic or stderr noise. Dying by
/// `SIGPIPE` is the expected Unix outcome and is accepted here along with a
/// clean exit: which one happens is a race with the writer, not a contract.
#[cfg(unix)]
#[test]
fn completions_survive_a_closed_reader() {
    use std::os::unix::process::ExitStatusExt;

    let mut child = Command::new(env!("CARGO_BIN_EXE_linear"))
        .args(["completions", "bash"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn linear completions bash");

    {
        // Read a little, then drop the reader. The script is far larger than a
        // pipe buffer, so the child is still writing when the read end closes.
        let stdout = child.stdout.take().expect("piped stdout");
        let mut reader = BufReader::new(stdout);
        let mut first = String::new();
        let _ = reader.read_line(&mut first);
        assert!(
            first.starts_with("_linear()"),
            "expected the script to start with _linear(), got {first:?}"
        );
        let mut rest = [0u8; 32];
        let _ = reader.read(&mut rest);
    }

    let out = child.wait_with_output().expect("wait for linear");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "a closed reader must not panic: {stderr}"
    );
    assert!(
        stderr.is_empty(),
        "a closed reader must stay quiet: {stderr}"
    );
    assert!(
        out.status.success() || out.status.signal() == Some(13),
        "expected exit 0 or death by SIGPIPE, got {:?} (stderr: {stderr})",
        out.status
    );
}

/// The script itself, with the reader allowed to drain it.
#[test]
fn completions_bash_emits_a_script() {
    let out = run_cli(&["completions", "bash"], &[]);
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("_linear()"),
        "stdout should hold a bash function: {:?}",
        out.stdout.chars().take(80).collect::<String>()
    );
    assert!(
        out.stdout.lines().count() > 100,
        "expected a full script, got {} lines",
        out.stdout.lines().count()
    );
}

/// Every shell cliffy accepted is still accepted, and an unknown one is a
/// validation error rather than a panic.
#[test]
fn completions_accepts_the_upstream_shells_and_rejects_others() {
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let out = run_cli(&["completions", shell], &[]);
        assert!(out.success(), "{shell}: {}", out.stderr);
        assert!(!out.stdout.is_empty(), "{shell} produced nothing");
    }

    let bad = run_cli(&["completions", "tcsh"], &[]);
    assert!(!bad.success(), "tcsh should be rejected");
    assert!(
        bad.stderr.contains("Unsupported shell") || bad.stderr.contains("tcsh"),
        "stderr: {}",
        bad.stderr
    );
}
