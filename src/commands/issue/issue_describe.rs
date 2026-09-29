//! `linear issue describe` — print the issue title and Linear-issue trailer.
//! Port of `src/commands/issue/issue-describe.ts`.

use serde_json::Value;

use crate::errors::{CliError, Result};
use crate::jj;
use crate::linear;
use crate::output;

#[derive(clap::Args, Debug)]
pub struct IssueDescribeArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
    /// Use 'References' instead of 'Fixes' for the Linear issue link
    #[arg(short = 'r', long = "references", visible_alias = "ref")]
    pub references: bool,
}

pub fn run(args: IssueDescribeArgs) -> Result<()> {
    let result = (|| -> Result<()> {
        let Some(resolved_id) = linear::get_issue_identifier(args.issue_id.as_deref())? else {
            return Err(CliError::validation("Could not determine issue ID")
                .suggestion("Please provide an issue ID like 'ENG-123'."));
        };

        let details = linear::fetch_issue_details(&resolved_id, false)?;
        let title = details.get("title").and_then(Value::as_str).unwrap_or("");
        let url = details.get("url").and_then(Value::as_str).unwrap_or("");

        let magic_word = if args.references { "References" } else { "Fixes" };
        output::line(&jj::format_issue_description(
            &resolved_id,
            title,
            url,
            magic_word,
        ));
        Ok(())
    })();
    result.map_err(|error| error.with_context("Failed to get issue description"))
}
