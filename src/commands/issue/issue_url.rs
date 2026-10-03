//! `linear issue url` — print the issue URL for the current branch.
//! Port of `src/commands/issue/issue-url.ts`.

use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::linear;
use crate::output;

#[derive(clap::Args, Debug)]
pub struct IssueUrlArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
    /// Output issue data as JSON (an addition to upstream; the shape is
    /// `{"identifier": ..., "url": ...}`)
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: IssueUrlArgs) -> Result<()> {
    let result = (|| -> Result<()> {
        let Some(resolved_id) = linear::get_issue_identifier(args.issue_id.as_deref())? else {
            return Err(CliError::validation("Could not determine issue ID")
                .suggestion("Please provide an issue ID like 'ENG-123'."));
        };
        let details = linear::fetch_issue_details(&resolved_id, false)?;
        let url = details.get("url").and_then(Value::as_str).unwrap_or("");
        if args.json {
            output::print_json(&json!({ "identifier": resolved_id, "url": url }));
            return Ok(());
        }
        output::line(url);
        Ok(())
    })();
    result.map_err(|error| error.with_context("Failed to get issue URL"))
}
