//! `linear issue id`: the identifier implied by the working copy (VED-295).
//!
//! `get_issue_identifier(None)` used to return `None` and ignore the branch, so
//! every `[issueId]`-optional command failed in a repo whose branch named the
//! issue. This drives the compiled binary from a real git repo to pin the wiring.
//!
//! `issue id` makes no API call - it resolves the identifier from the branch - so
//! the mock server is only here to give `mock_env` an endpoint and keep the
//! developer's own `linear.toml` out of the run.

mod common;

use std::path::Path;
use std::process::Command;

use common::{mock_env, CliOutput, MockLinearServer};

/// A real git repo on `branch`, with one commit so the branch is attached.
fn repo_on(branch: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    // `-B` creates or resets, so a branch the init already picked (main) is fine.
    git(&["checkout", "-q", "-B", branch]);
    git(&["commit", "-q", "--allow-empty", "-m", "init"]);
    dir
}

fn issue_id_in(dir: &Path, args: &[&str]) -> CliOutput {
    let server = MockLinearServer::start(vec![]);
    let mut command = Command::new(env!("CARGO_BIN_EXE_linear"));
    command.args(args).current_dir(dir).env("NO_COLOR", "1");
    for (key, value) in mock_env(&server) {
        command.env(key, value);
    }
    let output = command.output().expect("linear runs");
    CliOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code(),
    }
}

#[test]
fn issue_id_reads_the_identifier_from_the_branch() {
    let repo = repo_on("ved-288-zzz-branch-state");
    let out = issue_id_in(repo.path(), &["issue", "id"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "VED-288");
}

#[test]
fn issue_id_json_reads_the_identifier_from_a_prefixed_branch() {
    // A `feature/` prefix and a lower-case key are both normal branch names.
    let repo = repo_on("feature/ved-295-thing");
    let out = issue_id_in(repo.path(), &["issue", "id", "--json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).expect("json output");
    assert_eq!(parsed["identifier"], "VED-295");
}

#[test]
fn a_branch_without_an_identifier_still_errors() {
    let repo = repo_on("main");
    let out = issue_id_in(repo.path(), &["issue", "id"]);
    assert!(!out.success(), "stdout: {}", out.stdout);
    assert!(
        format!("{}{}", out.stdout, out.stderr).contains("Could not determine issue ID"),
        "stderr: {}",
        out.stderr
    );
}
