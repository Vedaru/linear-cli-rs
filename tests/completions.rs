//! End-to-end tests for `linear completions`.
//!
//! The registry of interest is the closed-reader case: `clap_complete`'s shell
//! generators `.expect("failed to write completion file")` on any write error,
//! so generating straight into stdout panicked (exit 101) whenever a reader
//! stopped early - the exact pipeline a user types (`linear completions bash |
//! head`). `output.rs` promises a closed stdout is a normal end of output; this
//! suite holds the completions path to that promise.

mod common;

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};

use common::run_cli;

/// Generate into memory, so a closed reader cannot reach the generator.
#[test]
fn completions_survive_a_closed_reader() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_linear"))
        .args(["completions", "bash"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn linear completions bash");

    {
        // Read a little, then drop the reader: with a script far larger than a
        // pipe buffer the child is still writing, so closing the read end
        // raises EPIPE inside it.
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
        out.status.success(),
        "a closed reader must not fail the command: status={:?} stderr={stderr}",
        out.status
    );
    assert!(
        !stderr.contains("panicked"),
        "a closed reader must not panic: {stderr}"
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
