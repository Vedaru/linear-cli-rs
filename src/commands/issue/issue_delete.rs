//! `linear issue delete` — port of `src/commands/issue/issue-delete.ts`.
//!
//! Deletes a single issue, or many in bulk via `--bulk` / `--bulk-file` /
//! `--bulk-stdin`. The confirmation prompt is guarded by
//! [`crate::prompt::is_interactive`]; non-interactive runs must pass
//! `--confirm` instead of blocking.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::linear;
use crate::{graphql, output, prompt};
use std::collections::HashSet;

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

    if is_bulk_mode(&args) {
        return handle_bulk_delete(&client, &args);
    }

    // Single mode requires issueId.
    let Some(issue_id) = args.issue_id.as_deref() else {
        return Err(CliError::validation("Issue ID required")
            .suggestion("Use --bulk for multiple issues."));
    };

    handle_single_delete(&client, issue_id, args.confirm)
}

fn handle_single_delete(
    client: &graphql::Client,
    issue_id: &str,
    confirm: bool,
) -> Result<()> {
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
    let identifier = issue.get("identifier").and_then(Value::as_str).unwrap_or("");

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
    let ids = collect_bulk_ids(args)?;

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
        let confirmed = prompt::confirm(
            &format!("Delete {} issue(s)?", ids.len()),
            false,
        )?;
        if !confirmed {
            output::line("Bulk delete cancelled.");
            return Ok(());
        }
    }

    let operation =
        |issue_id_input: &str| -> Result<BulkOperationResult> {
            bulk_delete_one(client, issue_id_input)
        };

    let summary = execute_bulk_operations(&ids, operation);
    print_bulk_summary(&summary, "issue", "deleted");

    if summary.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn bulk_delete_one(
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

    // Best-effort details lookup: on any failure fall back to the resolved id.
    let mut identifier = resolved_id.clone();
    let mut title = String::new();
    if let Ok(details) =
        client.request(GET_ISSUE_DETAILS_FOR_BULK_DELETE_QUERY, json!({ "id": resolved_id }))
    {
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

// ---------------------------------------------------------------------------
// Bulk helpers
//
// Upstream keeps these in `src/utils/bulk.ts`; this port has no shared module
// for them, so the same logic lives inline here (and in `issue_archive.rs`).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(crate) struct BulkOperationResult {
    pub id: String,
    pub name: Option<String>,
    pub success: bool,
    pub error: Option<String>,
}

impl BulkOperationResult {
    fn success(id: &str, name: Option<String>) -> Self {
        BulkOperationResult {
            id: id.to_string(),
            name,
            success: true,
            error: None,
        }
    }

    fn failure(id: &str, name: Option<String>, error: impl Into<String>) -> Self {
        BulkOperationResult {
            id: id.to_string(),
            name,
            success: false,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BulkOperationSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub results: Vec<BulkOperationResult>,
}

pub(crate) fn is_bulk_mode(args: &IssueDeleteArgs) -> bool {
    !args.bulk.is_empty() || args.bulk_file.is_some() || args.bulk_stdin
}

/// Parse IDs from text input, splitting on newlines, commas, and whitespace.
pub(crate) fn parse_ids(input: &str) -> Vec<String> {
    input
        .split(|c: char| c == '\n' || c == '\r' || c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn collect_bulk_ids(args: &IssueDeleteArgs) -> Result<Vec<String>> {
    let mut all_ids: Vec<String> = Vec::new();

    if !args.bulk.is_empty() {
        all_ids.extend(args.bulk.iter().cloned());
    }

    if let Some(path) = &args.bulk_file {
        match std::fs::read_to_string(path) {
            Ok(content) => all_ids.extend(parse_ids(&content)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CliError::not_found("File", path));
            }
            Err(error) => return Err(error.into()),
        }
    }

    if args.bulk_stdin {
        let mut buffer = String::new();
        use std::io::Read;
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(CliError::from)?;
        all_ids.extend(parse_ids(&buffer));
    }

    let mut seen = HashSet::new();
    all_ids.retain(|id| seen.insert(id.clone()));
    Ok(all_ids)
}

pub(crate) fn execute_bulk_operations<F>(
    ids: &[String],
    operation: F,
) -> BulkOperationSummary
where
    F: Fn(&str) -> Result<BulkOperationResult>,
{
    let mut results = Vec::with_capacity(ids.len());
    for id in ids {
        match operation(id) {
            Ok(result) => results.push(result),
            Err(error) => results.push(BulkOperationResult {
                id: id.clone(),
                name: None,
                success: false,
                error: Some(error.user_message.clone()),
            }),
        }
    }

    let succeeded = results.iter().filter(|result| result.success).count();
    BulkOperationSummary {
        total: ids.len(),
        succeeded,
        failed: ids.len() - succeeded,
        results,
    }
}

pub(crate) fn print_bulk_summary(
    summary: &BulkOperationSummary,
    entity_name: &str,
    operation_name: &str,
) {
    output::blank();

    if summary.failed == 0 {
        let plural = if summary.succeeded != 1 { "s" } else { "" };
        output::line(&format!(
            "✓ Successfully {operation_name} {} {entity_name}{plural}",
            summary.succeeded
        ));
    } else if summary.succeeded == 0 {
        let verb = operation_name.strip_suffix("ed").unwrap_or(operation_name);
        let plural = if summary.total != 1 { "s" } else { "" };
        output::line(&format!(
            "✗ Failed to {verb} all {} {entity_name}{plural}",
            summary.total
        ));
    } else {
        let plural = if summary.total != 1 { "s" } else { "" };
        output::line(&format!(
            "Completed: {}/{} {entity_name}{plural} {operation_name}",
            summary.succeeded, summary.total
        ));
        output::line(&format!("  ✓ Succeeded: {}", summary.succeeded));
        output::line(&format!("  ✗ Failed: {}", summary.failed));
    }

    if summary.failed > 0 {
        output::line("\nFailed operations:");
        for result in &summary.results {
            if !result.success {
                let name = result
                    .name
                    .as_deref()
                    .map(|name| format!(" ({name})"))
                    .unwrap_or_default();
                let error = result.error.as_deref().unwrap_or("Unknown error");
                output::line(&format!("  - {}{name}: {error}", result.id));
            }
        }
    }
}
