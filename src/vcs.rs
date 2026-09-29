//! Version-control integration. Port of `src/utils/vcs.ts`.
//!
//! Two backends are supported, selected by the `vcs` config option (default
//! `git`):
//!
//! * **git** — the issue identifier is read from the branch name, and starting
//!   work means checking out a branch.
//! * **jj** (Jujutsu) — the identifier is read from the `Linear-issue`
//!   trailer, and starting work prepares a change and writes its description.
//!
//! Upstream asks the user a `Select.prompt` when a git branch already exists.
//! This port routes that through [`prompt::select`], which refuses to block in
//! a non-interactive environment and returns an actionable error instead.

use crate::config::{self, Vcs};
use crate::errors::{CliError, Result};
use crate::git;
use crate::issue_identifier::find_issue_identifier_in_text;
use crate::jj;
use crate::output;
use crate::proc::{self, RunOptions, DEFAULT_TIMEOUT};
use crate::prompt;

/// The configured VCS, defaulting to git.
pub fn get_vcs() -> Vcs {
    config::vcs().unwrap_or(Vcs::Git)
}

/// The message shown when no issue identifier can be found for the active VCS.
pub fn get_no_issue_found_message() -> &'static str {
    match get_vcs() {
        Vcs::Git => "The current branch does not contain a valid linear issue id.",
        Vcs::Jj => "No Linear-issue trailer found in current or ancestor commits.",
    }
}

/// Like [`git::branch_exists`], but a missing/unspawnable `git` is an error
/// rather than `false` — this is the variant upstream wraps in a `CliError`.
fn git_branch_exists(branch: &str) -> Result<bool> {
    match proc::run(
        "git",
        &["rev-parse", "--verify", branch],
        &RunOptions::default(),
        DEFAULT_TIMEOUT,
    ) {
        Some(output) => Ok(output.success),
        None => Err(CliError::cli(
            "Failed to check if branch exists: git is not available",
        )),
    }
}

/// The issue identifier implied by the working copy: the branch name for git,
/// the `Linear-issue` trailer for jj. `None` when neither yields one.
///
/// A failure to even read the VCS state (git present but erroring) propagates,
/// matching upstream, which lets `getCurrentBranch`'s error escape.
pub fn get_current_issue_from_vcs() -> Result<Option<String>> {
    match get_vcs() {
        Vcs::Git => {
            let Some(branch) = git::get_current_branch()? else {
                return Ok(None);
            };
            Ok(find_issue_identifier_in_text(&branch).map(|parsed| parsed.identifier))
        }
        Vcs::Jj => Ok(jj::get_jj_linear_issue()),
    }
}

/// Start work on `issue_id` under the active VCS.
pub fn start_vcs_work(
    issue_id: &str,
    branch_name: &str,
    git_source_ref: Option<&str>,
) -> Result<()> {
    match get_vcs() {
        Vcs::Git => start_git_work(issue_id, branch_name, git_source_ref),
        Vcs::Jj => start_jj_work(issue_id),
    }
}

fn start_git_work(issue_id: &str, branch_name: &str, git_source_ref: Option<&str>) -> Result<()> {
    let _ = issue_id;
    let source_ref = git_source_ref.unwrap_or("HEAD");

    if git_branch_exists(branch_name)? {
        let labels = vec![
            "Switch to existing branch".to_string(),
            "Create new branch with suffix".to_string(),
        ];
        let choice = prompt::select(
            &format!("Branch {branch_name} already exists. What would you like to do?"),
            &labels,
        )?;

        if choice == 0 {
            let output = proc::run(
                "git",
                &["checkout", branch_name],
                &RunOptions::default(),
                DEFAULT_TIMEOUT,
            )
            .ok_or_else(|| CliError::cli("Failed to run git: git is not available"))?;
            if !output.success {
                let error_msg = output.stderr_string().trim().to_string();
                return Err(CliError::cli(format!(
                    "Failed to switch to branch '{branch_name}': {error_msg}"
                )));
            }
            output::line(&format!("✓ Switched to '{branch_name}'"));
        } else {
            // Find the next free `<branch>-N` suffix.
            let mut suffix = 1;
            let mut new_branch = format!("{branch_name}-{suffix}");
            while git_branch_exists(&new_branch)? {
                suffix += 1;
                new_branch = format!("{branch_name}-{suffix}");
            }
            create_branch(&new_branch, source_ref)?;
        }
    } else {
        create_branch(branch_name, source_ref)?;
    }
    Ok(())
}

fn create_branch(branch_name: &str, source_ref: &str) -> Result<()> {
    let output = proc::run(
        "git",
        &["checkout", "-b", branch_name, source_ref],
        &RunOptions::default(),
        DEFAULT_TIMEOUT,
    )
    .ok_or_else(|| CliError::cli("Failed to run git: git is not available"))?;
    if !output.success {
        let error_msg = output.stderr_string().trim().to_string();
        return Err(CliError::cli(format!(
            "Failed to create branch '{branch_name}': {error_msg}"
        )));
    }
    output::line(&format!("✓ Created and switched to branch '{branch_name}'"));
    Ok(())
}

fn start_jj_work(issue_id: &str) -> Result<()> {
    jj::prepare_jj_working_state()?;

    let details = crate::linear::fetch_issue_details(issue_id, false)?;
    let title = details
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let url = details
        .get("url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let description = jj::format_issue_description(issue_id, title, url, "Fixes");
    jj::set_jj_description(&description)?;

    output::line(&format!("✓ Prepared jj change for issue {issue_id}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_issue_message_matches_the_configured_vcs() {
        // `get_no_issue_found_message` reads config; without a `vcs` setting
        // the default is git.
        assert_eq!(
            get_no_issue_found_message(),
            "The current branch does not contain a valid linear issue id."
        );
    }
}
