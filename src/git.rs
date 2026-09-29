//! Git helpers. Port of `src/utils/git.ts`.
//!
//! Every call goes through [`proc::run`], so a missing or wedged `git` can
//! never block the CLI: the process has a deadline and never inherits the
//! parent terminal. The two "best-effort" predicates (`is_inside_git_repo`,
//! `branch_exists`) swallow every failure as `false`, matching the upstream
//! `try`/`catch` wrappers — those callers use the result only for optional
//! guidance and must not turn into a crash.

use std::path::Path;

use crate::errors::{CliError, Result};
use crate::proc::{self, RunOptions, DEFAULT_TIMEOUT};

fn git(args: &[&str]) -> Option<proc::ProcOutput> {
    proc::run("git", args, &RunOptions::default(), DEFAULT_TIMEOUT)
}

/// The current branch, or `None` in a detached-HEAD state.
///
/// `git symbolic-ref --short HEAD` fails on a detached HEAD with "fatal: ref
/// HEAD is not a symbolic ref"; upstream treats that as "no branch" rather
/// than an error, so we do too.
pub fn get_current_branch() -> Result<Option<String>> {
    let output = git(&["symbolic-ref", "--short", "HEAD"])
        .ok_or_else(|| CliError::cli("Failed to get current branch: git is not available"))?;

    if !output.success {
        let error_msg = output.stderr_string().trim().to_string();
        if error_msg.contains("not a symbolic ref") {
            return Ok(None);
        }
        return Err(CliError::cli(format!(
            "Failed to get current branch: {error_msg}"
        )));
    }

    let branch = output.stdout_trimmed();
    Ok(if branch.is_empty() { None } else { Some(branch) })
}

/// The base name of the repository's top-level directory (`path.basename` of
/// `git rev-parse --show-toplevel`).
pub fn get_repo_dir() -> Result<String> {
    let output = git(&["rev-parse", "--show-toplevel"]).ok_or_else(|| {
        CliError::cli("Failed to get repository directory: git is not available")
    })?;

    if !output.success {
        let error_msg = output.stderr_string().trim().to_string();
        return Err(CliError::cli(format!(
            "Failed to get repository directory: {error_msg}"
        )));
    }

    let full_path = output.stdout_trimmed();
    Ok(Path::new(&full_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default())
}

/// Best-effort check for whether the working directory is inside a git work
/// tree. Any failure — git absent, not a repository, dubious ownership — is
/// `false`.
///
/// `git rev-parse --is-inside-work-tree` prints `false` with exit status 0
/// inside a bare repo or `.git` directory, so the exit code alone is not
/// enough; the output must be exactly `true`.
pub fn is_inside_git_repo() -> bool {
    match git(&["rev-parse", "--is-inside-work-tree"]) {
        Some(output) => output.success && output.stdout_trimmed() == "true",
        None => false,
    }
}

/// Whether a local branch (or any rev) resolves. Swallows every failure.
pub fn branch_exists(branch: &str) -> bool {
    match git(&["rev-parse", "--verify", branch]) {
        Some(output) => output.success,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_exists_is_false_for_a_nonexistent_rev() {
        // Runs inside whatever repository (or not) we happen to be in; either
        // way this nonsense rev cannot resolve.
        assert!(!branch_exists(
            "linear-cli-definitely-not-a-real-branch-xyz"
        ));
    }

    #[test]
    fn is_inside_git_repo_returns_a_bool_without_panicking() {
        // The project may or may not be checked out under git; both answers
        // are valid — the only contract is that it never panics.
        let _ = is_inside_git_repo();
    }
}
