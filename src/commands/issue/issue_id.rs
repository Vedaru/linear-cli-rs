//! `linear issue id` — print the issue id for the current branch.
//! Port of `src/commands/issue/issue-id.ts`.

use crate::errors::{CliError, Result};
use crate::linear;
use crate::output;

#[derive(clap::Args, Debug)]
pub struct IssueIdArgs {}

pub fn run(_args: IssueIdArgs) -> Result<()> {
    let result = (|| -> Result<()> {
        let Some(resolved_id) = linear::get_issue_identifier(None)? else {
            return Err(
                CliError::validation("Could not determine issue ID").suggestion(
                    "Please provide an issue ID or run from a branch with an issue identifier.",
                ),
            );
        };
        output::line(&resolved_id);
        Ok(())
    })();
    result.map_err(|error| error.with_context("Failed to get issue ID"))
}
