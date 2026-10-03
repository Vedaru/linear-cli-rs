//! `linear issue id` — print the issue id for the current branch.
//! Port of `src/commands/issue/issue-id.ts`.

use serde_json::json;

use crate::errors::{CliError, Result};
use crate::linear;
use crate::output;

#[derive(clap::Args, Debug)]
pub struct IssueIdArgs {
    /// Output issue data as JSON (an addition to upstream; the shape is
    /// `{"identifier": ...}` and is pinned by a fixture)
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: IssueIdArgs) -> Result<()> {
    let result = (|| -> Result<()> {
        let Some(resolved_id) = linear::get_issue_identifier(None)? else {
            return Err(
                CliError::validation("Could not determine issue ID").suggestion(
                    "Please provide an issue ID or run from a branch with an issue identifier.",
                ),
            );
        };
        if args.json {
            // No API call: this command resolves the identifier from the branch,
            // and a machine-readable form must not change what it costs.
            output::print_json(&json!({ "identifier": resolved_id }));
            return Ok(());
        }
        output::line(&resolved_id);
        Ok(())
    })();
    result.map_err(|error| error.with_context("Failed to get issue ID"))
}
