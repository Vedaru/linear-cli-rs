//! `linear issue title` — print the issue title for the current branch.
//! Port of `src/commands/issue/issue-title.ts`.

use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::linear;
use crate::output;

#[derive(clap::Args, Debug)]
pub struct IssueTitleArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
    /// Output issue data as JSON (an addition to upstream; the shape is
    /// `{"identifier": ..., "title": ...}`)
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: IssueTitleArgs) -> Result<()> {
    let result = (|| -> Result<()> {
        let Some(resolved_id) = linear::get_issue_identifier(args.issue_id.as_deref())? else {
            return Err(CliError::validation("Could not determine issue ID")
                .suggestion("Please provide an issue ID like 'ENG-123'."));
        };
        let details = linear::fetch_issue_details(&resolved_id, false)?;
        let title = details.get("title").and_then(Value::as_str).unwrap_or("");
        if args.json {
            // The identifier travels with the title even though it is a bare
            // string in the text form: a caller parsing this has no branch to
            // re-resolve it from.
            output::print_json(&json!({ "identifier": resolved_id, "title": title }));
            return Ok(());
        }
        output::line(title);
        Ok(())
    })();
    result.map_err(|error| error.with_context("Failed to get issue title"))
}
