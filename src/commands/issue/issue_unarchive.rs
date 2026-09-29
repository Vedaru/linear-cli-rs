//! `linear issue unarchive` — restore an issue from the archive or the trash.
//!
//! Neither upstream (`schpet/linear-cli`) nor Linear's own app and MCP server
//! expose a way back from `issue archive` / `issue delete`, but the API does:
//! the `issueUnarchive` mutation restores both. An archived issue and an issue
//! sitting in the trash are the same state to Linear — deleting sets
//! `archivedAt` and `trashed`, and restoring clears both — so this command takes
//! either as its input and is the missing half of `issue archive` and
//! `issue delete`.
//!
//! The confirmation prompt is guarded by [`crate::prompt::is_interactive`];
//! non-interactive runs must pass `--confirm` instead of blocking.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{self, CliError, Result};
use crate::linear;
use crate::{graphql, output, prompt};

use crate::bulk::{
    collect_bulk_ids, execute_bulk_operations, is_bulk_mode, print_bulk_summary,
    BulkOperationResult,
};

/// Unarchive an issue
#[derive(Args, Debug)]
#[command(
    long_about = "Restore an issue from Linear's archive or its trash\n\nLinear keeps archived and deleted (trashed) issues out of list, query, and search results; both states are cleared by the issueUnarchive mutation this command calls, so it is the inverse of `issue archive` and of `issue delete`. An issue that is neither archived nor trashed is left alone. See https://linear.app/docs/delete-archive-issues"
)]
pub struct IssueUnarchiveArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    pub confirm: bool,
    /// Unarchive multiple issues by identifier (e.g., TC-123 TC-124)
    #[arg(long, num_args = 1.., value_name = "ids")]
    pub bulk: Vec<String>,
    /// Read issue identifiers from a file (one per line)
    #[arg(long = "bulk-file", value_name = "file")]
    pub bulk_file: Option<String>,
    /// Read issue identifiers from stdin
    #[arg(long = "bulk-stdin")]
    pub bulk_stdin: bool,
}

const GET_ISSUE_UNARCHIVE_DETAILS_QUERY: &str = r#"
query GetIssueUnarchiveDetails($id: String!) {
  issue(id: $id) {
    identifier
    title
    archivedAt
    trashed
  }
}
"#;

const UNARCHIVE_ISSUE_MUTATION: &str = r#"
mutation UnarchiveIssue($id: String!) {
  issueUnarchive(id: $id) {
    success
  }
}
"#;

const GET_ISSUE_DETAILS_FOR_BULK_UNARCHIVE_QUERY: &str = r#"
query GetIssueDetailsForBulkUnarchive($id: String!) {
  issue(id: $id) {
    identifier
    title
    archivedAt
    trashed
  }
}
"#;

const BULK_UNARCHIVE_ISSUE_MUTATION: &str = r#"
mutation BulkUnarchiveIssue($id: String!) {
  issueUnarchive(id: $id) {
    success
  }
}
"#;

pub fn run(args: IssueUnarchiveArgs) -> Result<()> {
    run_inner(args).map_err(|error| error.with_context("Failed to unarchive issue"))
}

fn run_inner(args: IssueUnarchiveArgs) -> Result<()> {
    let client = graphql::client()?;

    if is_bulk_mode(&args.bulk, args.bulk_file.as_deref(), args.bulk_stdin) {
        if args.issue_id.is_some() {
            return Err(CliError::validation(
                "Cannot combine a positional issue ID with --bulk",
            )
            .suggestion(
                "Pass every identifier through --bulk (or --bulk-file / --bulk-stdin), or drop the positional one.",
            ));
        }
        return handle_bulk_unarchive(&client, &args);
    }

    unarchive_issue(&client, args.issue_id.as_deref(), args.confirm)
}

fn unarchive_issue(client: &graphql::Client, issue_id: Option<&str>, confirm: bool) -> Result<()> {
    let resolved_id = linear::get_issue_identifier(issue_id)?;
    let Some(resolved_id) = resolved_id else {
        return Err(CliError::validation("Could not determine issue ID")
            .suggestion("Please provide an issue ID like 'ENG-123'."));
    };

    // Linear answers an unknown identifier with a GraphQL not-found error
    // rather than a null issue; translate both into the same clean error.
    let issue_details = errors::translate_not_found("Issue", &resolved_id, || {
        client.request(
            GET_ISSUE_UNARCHIVE_DETAILS_QUERY,
            json!({ "id": resolved_id }),
        )
    })?;

    let Some(issue) = issue_details.get("issue").filter(|value| !value.is_null()) else {
        return Err(CliError::not_found("Issue", &resolved_id));
    };

    let identifier = issue
        .get("identifier")
        .and_then(Value::as_str)
        .unwrap_or("");
    let title = issue.get("title").and_then(Value::as_str).unwrap_or("");

    // Nothing to restore: say so instead of prompting for (and reporting) a
    // no-op, the way `issue archive` reports an already-archived issue.
    if !is_archived_or_trashed(issue) {
        output::line(&format!("Issue \"{identifier}: {title}\" is not archived."));
        return Ok(());
    }

    if !confirm {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --confirm to skip."));
        }
        let confirmed = prompt::confirm(
            &format!("Are you sure you want to unarchive \"{identifier}: {title}\"?"),
            false,
        )?;
        if !confirmed {
            output::line("Unarchive cancelled.");
            return Ok(());
        }
    }

    let result = client.request(UNARCHIVE_ISSUE_MUTATION, json!({ "id": resolved_id }))?;
    let success = result
        .get("issueUnarchive")
        .and_then(|unarchive| unarchive.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Err(CliError::cli(
            "Linear reported the unarchive as unsuccessful",
        ));
    }

    output::line(&format!(
        "✓ Successfully unarchived issue: {identifier}: {title}"
    ));
    Ok(())
}

fn handle_bulk_unarchive(client: &graphql::Client, args: &IssueUnarchiveArgs) -> Result<()> {
    let ids = collect_bulk_ids(&args.bulk, args.bulk_file.as_deref(), args.bulk_stdin)?;

    if ids.is_empty() {
        return Err(CliError::validation(
            "No issue identifiers provided for bulk unarchive",
        ));
    }

    output::line(&format!("Found {} issue(s) to unarchive.", ids.len()));

    if !args.confirm {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --confirm to skip."));
        }
        let confirmed = prompt::confirm(&format!("Unarchive {} issue(s)?", ids.len()), false)?;
        if !confirmed {
            output::line("Bulk unarchive cancelled.");
            return Ok(());
        }
    }

    let operation = |issue_id_input: &str| -> Result<BulkOperationResult> {
        bulk_unarchive_one(client, issue_id_input)
    };

    let summary = execute_bulk_operations(&ids, operation);
    print_bulk_summary(&summary, "issue", "unarchived");

    if summary.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn bulk_unarchive_one(
    client: &graphql::Client,
    issue_id_input: &str,
) -> Result<BulkOperationResult> {
    let Some(resolved_id) = linear::get_issue_identifier(Some(issue_id_input))? else {
        return Ok(BulkOperationResult::failure(
            issue_id_input,
            None,
            "Issue not found",
        ));
    };

    let details = client.request(
        GET_ISSUE_DETAILS_FOR_BULK_UNARCHIVE_QUERY,
        json!({ "id": resolved_id }),
    );
    let details = match details {
        Ok(details) => details,
        Err(error) if error.is_not_found() => {
            return Ok(BulkOperationResult {
                id: resolved_id,
                name: None,
                success: false,
                error: Some("Issue not found".to_string()),
            });
        }
        Err(error) => return Err(error),
    };

    let Some(issue) = details.get("issue").filter(|value| !value.is_null()) else {
        return Ok(BulkOperationResult::failure(
            &resolved_id,
            None,
            "Issue not found",
        ));
    };

    let identifier = issue
        .get("identifier")
        .and_then(Value::as_str)
        .unwrap_or("");
    let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
    let name = format!("{identifier}: {title}");

    // Already live counts as done: the requested end state holds.
    if !is_archived_or_trashed(issue) {
        return Ok(BulkOperationResult::success(&resolved_id, Some(name)));
    }

    let result = client.request(BULK_UNARCHIVE_ISSUE_MUTATION, json!({ "id": resolved_id }))?;
    let success = result
        .get("issueUnarchive")
        .and_then(|unarchive| unarchive.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Ok(BulkOperationResult::failure(
            &resolved_id,
            Some(name),
            "Unarchive operation failed",
        ));
    }

    Ok(BulkOperationResult::success(&resolved_id, Some(name)))
}

/// Linear represents an archived issue and a trashed (deleted) one with the
/// same two fields: `issue delete` sets `archivedAt` as well as `trashed`, and
/// `issueUnarchive` clears both.
fn is_archived_or_trashed(issue: &Value) -> bool {
    let archived = issue
        .get("archivedAt")
        .map(|value| !value.is_null())
        .unwrap_or(false);
    let trashed = issue
        .get("trashed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    archived || trashed
}
