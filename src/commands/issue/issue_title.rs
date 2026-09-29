//! `linear issue title` — print the issue title for the current branch.
//! Port of `src/commands/issue/issue-title.ts`.

use serde_json::Value;

use crate::errors::{CliError, Result};
use crate::linear;
use crate::output;

#[derive(clap::Args, Debug)]
pub struct IssueTitleArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
}

pub fn run(args: IssueTitleArgs) -> Result<()> {
    let result = (|| -> Result<()> {
        let Some(resolved_id) = linear::get_issue_identifier(args.issue_id.as_deref())? else {
            return Err(CliError::validation("Could not determine issue ID")
                .suggestion("Please provide an issue ID like 'ENG-123'."));
        };
        let details = linear::fetch_issue_details(&resolved_id, false)?;
        let title = details.get("title").and_then(Value::as_str).unwrap_or("");
        output::line(title);
        Ok(())
    })();
    result.map_err(|error| error.with_context("Failed to get issue title"))
}
