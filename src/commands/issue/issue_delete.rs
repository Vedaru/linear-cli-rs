//! `linear issue delete` — port of `src/commands/issue/issue-delete.ts`.
//!
//! Deletes a single issue, or many in bulk via `--bulk` / `--bulk-file` /
//! `--bulk-stdin`. The confirmation prompt is guarded by
//! [`crate::prompt::is_interactive`]; non-interactive runs must pass
//! `--confirm` instead of blocking.

use clap::Args;
use serde_json::{json, Value};

use crate::bulk::{
    collect_bulk_ids, execute_bulk_operations, is_bulk_mode, print_bulk_summary,
    BulkOperationResult,
};
use crate::errors::{CliError, Result};
use crate::linear;
use crate::{graphql, output, prompt};

/// Delete an issue
#[derive(Args, Debug)]
pub struct IssueDeleteArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    pub confirm: bool,
    /// Delete multiple issues by identifier (e.g., TC-123 TC-124)
    #[arg(long, num_args = 1.., value_name = "ids")]
    pub bulk: Vec<String>,
    /// Read issue identifiers from a file (one per line)
    #[arg(long = "bulk-file", value_name = "file")]
    pub bulk_file: Option<String>,
    /// Read issue identifiers from stdin
    #[arg(long = "bulk-stdin")]
    pub bulk_stdin: bool,
}

const GET_ISSUE_DELETE_DETAILS_QUERY: &str = r#"
query GetIssueDeleteDetails($id: String!) {
  issue(id: $id) { title, identifier }
}
"#;

const DELETE_ISSUE_MUTATION: &str = r#"
mutation DeleteIssue($id: String!) {
  issueDelete(id: $id) {
    success
    entity {
      identifier
      title
    }
  }
}
"#;

const GET_ISSUE_DETAILS_FOR_BULK_DELETE_QUERY: &str = r#"
query GetIssueDetailsForBulkDelete($id: String!) {
  issue(id: $id) { title, identifier }
}
"#;

const BULK_DELETE_ISSUE_MUTATION: &str = r#"
mutation BulkDeleteIssue($id: String!) {
  issueDelete(id: $id) {
    success
  }
}
"#;

pub fn run(args: IssueDeleteArgs) -> Result<()> {
    run_inner(args).map_err(|error| error.with_context("Failed to delete issue"))
}

fn run_inner(args: IssueDeleteArgs) -> Result<()> {
    let client = graphql::client()?;

    if is_bulk_mode(&args.bulk, args.bulk_file.as_deref(), args.bulk_stdin) {
        return handle_bulk_delete(&client, &args);
    }

    // Single mode requires issueId.
    let Some(issue_id) = args.issue_id.as_deref() else {
        return Err(
            CliError::validation("Issue ID required").suggestion("Use --bulk for multiple issues.")
        );
    };

    handle_single_delete(&client, issue_id, args.confirm)
}

fn handle_single_delete(client: &graphql::Client, issue_id: &str, confirm: bool) -> Result<()> {
    // First resolve the issue ID to get the issue details.
    let Some(resolved_id) = linear::get_issue_identifier(Some(issue_id))? else {
        return Err(CliError::not_found("Issue", issue_id));
    };

    // Get issue details to show title in confirmation.
    let issue_details =
        client.request(GET_ISSUE_DELETE_DETAILS_QUERY, json!({ "id": resolved_id }))?;
    let Some(issue) = issue_details.get("issue").filter(|value| !value.is_null()) else {
        return Err(CliError::not_found("Issue", &resolved_id));
    };

    let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
    let identifier = issue
        .get("identifier")
        .and_then(Value::as_str)
        .unwrap_or("");

    // Show confirmation prompt unless --confirm flag is used.
    if !confirm {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --confirm to skip."));
        }
        let confirmed = prompt::confirm(
            &format!("Are you sure you want to delete \"{identifier}: {title}\"?"),
            false,
        )?;
        if !confirmed {
            output::line("Delete cancelled.");
            return Ok(());
        }
    }

    let result = client.request(DELETE_ISSUE_MUTATION, json!({ "id": resolved_id }))?;
    let success = result
        .get("issueDelete")
        .and_then(|delete| delete.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if success {
        output::line(&format!(
            "✓ Successfully deleted issue: {identifier}: {title}"
        ));
    } else {
        return Err(CliError::cli("Failed to delete issue"));
    }
    Ok(())
}

fn handle_bulk_delete(client: &graphql::Client, args: &IssueDeleteArgs) -> Result<()> {
    let ids = collect_bulk_ids(&args.bulk, args.bulk_file.as_deref(), args.bulk_stdin)?;

    if ids.is_empty() {
        return Err(CliError::validation(
            "No issue identifiers provided for bulk delete",
        ));
    }

    output::line(&format!("Found {} issue(s) to delete.", ids.len()));

    if !args.confirm {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --confirm to skip."));
        }
        let confirmed = prompt::confirm(&format!("Delete {} issue(s)?", ids.len()), false)?;
        if !confirmed {
            output::line("Bulk delete cancelled.");
            return Ok(());
        }
    }

    let operation = |issue_id_input: &str| -> Result<BulkOperationResult> {
        bulk_delete_one(client, issue_id_input)
    };

    let summary = execute_bulk_operations(&ids, operation);
    print_bulk_summary(&summary, "issue", "deleted");

    if summary.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn bulk_delete_one(client: &graphql::Client, issue_id_input: &str) -> Result<BulkOperationResult> {
    let Some(resolved_id) = linear::get_issue_identifier(Some(issue_id_input))? else {
        return Ok(BulkOperationResult::failure(
            issue_id_input,
            None,
            "Issue not found",
        ));
    };

    // Best-effort details lookup: on any failure fall back to the resolved id.
    let mut identifier = resolved_id.clone();
    let mut title = String::new();
    if let Ok(details) = client.request(
        GET_ISSUE_DETAILS_FOR_BULK_DELETE_QUERY,
        json!({ "id": resolved_id }),
    ) {
        if let Some(issue) = details.get("issue").filter(|value| !value.is_null()) {
            identifier = issue
                .get("identifier")
                .and_then(Value::as_str)
                .unwrap_or(&resolved_id)
                .to_string();
            title = issue
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
        }
    }

    let name = if title.is_empty() {
        identifier.clone()
    } else {
        format!("{identifier}: {title}")
    };

    let result = client.request(BULK_DELETE_ISSUE_MUTATION, json!({ "id": resolved_id }))?;
    let success = result
        .get("issueDelete")
        .and_then(|delete| delete.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if !success {
        return Ok(BulkOperationResult::failure(
            &resolved_id,
            Some(name),
            "Delete operation failed",
        ));
    }

    Ok(BulkOperationResult::success(&resolved_id, Some(name)))
}
