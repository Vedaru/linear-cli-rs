//! `linear issue commits` — show all commits for a Linear issue (jj only).
//! Port of `src/commands/issue/issue-commits.ts`.

use crate::config::Vcs;
use crate::errors::{CliError, Result};
use crate::linear;
use crate::proc::{self, RunOptions, DEFAULT_TIMEOUT};
use crate::vcs;

#[derive(clap::Args, Debug)]
pub struct IssueCommitsArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
}

pub fn run(args: IssueCommitsArgs) -> Result<()> {
    let result = (|| -> Result<()> {
        if vcs::get_vcs() != Vcs::Jj {
            return Err(
                CliError::validation("commits is only supported with jj-vcs")
                    .suggestion("This command requires jujutsu (jj) version control."),
            );
        }

        let Some(resolved_id) = linear::get_issue_identifier(args.issue_id.as_deref())? else {
            return Err(CliError::validation("Could not determine issue ID")
                .suggestion("Please provide an issue ID like 'ENG-123'."));
        };

        // Verify the issue exists in Linear. A lookup failure that means
        // "not found" is reported against the issue identifier; anything else
        // propagates.
        let linear_issue_id = match linear::get_issue_id(&resolved_id) {
            Ok(id) => id,
            Err(error) if error.is_not_found() => {
                return Err(CliError::not_found("Issue", &resolved_id));
            }
            Err(error) => return Err(error),
        };
        if linear_issue_id.is_none() {
            return Err(CliError::not_found("Issue", &resolved_id));
        }

        // Build the revset to find all commits with this Linear issue.
        let revset = format!(r#"description(regex:"(?m)^Linear-issue:.*{resolved_id}")"#);

        // First check if any commits exist.
        let check = proc::run(
            "jj",
            &["log", "-r", &revset, "-T", "commit_id", "--no-graph"],
            &RunOptions::default(),
            DEFAULT_TIMEOUT,
        )
        .ok_or_else(|| CliError::cli("Failed to run jj: jj is not available"))?;
        let commit_ids = check.stdout_trimmed();

        if commit_ids.is_empty() {
            return Err(CliError::not_found("Commits", &resolved_id));
        }

        // Show the commits with full details, inheriting the terminal.
        match proc::run_inherit(
            "jj",
            &[
                "log",
                "-r",
                &revset,
                "-p",
                "--git",
                "--no-graph",
                "-T",
                "builtin_log_compact_full_description",
            ],
            None,
            crate::proc::EDITOR_TIMEOUT,
        ) {
            Some(true) => {}
            Some(false) => std::process::exit(1),
            None => return Err(CliError::cli("Failed to run jj: jj is not available")),
        }

        Ok(())
    })();
    result.map_err(|error| error.with_context("Failed to show commits"))
}
