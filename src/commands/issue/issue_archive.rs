//! `linear issue archive` — port of `src/commands/issue/issue-archive.ts`.
//!
//! Archives a single issue, or many in bulk via `--bulk` / `--bulk-file` /
//! `--bulk-stdin`. The confirmation prompt is guarded by
//! [`crate::prompt::is_interactive`]; non-interactive runs must pass
//! `--confirm` instead of blocking.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{self, CliError, Result};
use crate::linear;
use crate::{graphql, output, prompt};
use std::collections::HashSet;

/// Archive an issue
#[derive(Args, Debug)]
#[command(
    long_about = "Archive an issue\n\nLinear archives closed issues on its own, and its docs say \"archiving happens automatically with no option to manually archive items\". Prefer closing (issue update --state) and letting auto-archive run, or issue delete to trash. This command calls the issueArchive mutation, which the Linear app and its official MCP server do not expose; archived issues drop out of list, query, and search results unless --include-archived is passed. See https://linear.app/docs/delete-archive-issues"
)]
pub struct IssueArchiveArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    pub confirm: bool,
    /// Archive multiple issues by identifier (e.g., TC-123 TC-124)
    #[arg(long, num_args = 1.., value_name = "ids")]
    pub bulk: Vec<String>,
    /// Read issue identifiers from a file (one per line)
    #[arg(long = "bulk-file", value_name = "file")]
    pub bulk_file: Option<String>,
    /// Read issue identifiers from stdin
    #[arg(long = "bulk-stdin")]
    pub bulk_stdin: bool,
}

const GET_ISSUE_ARCHIVE_DETAILS_QUERY: &str = r#"
query GetIssueArchiveDetails($id: String!) {
  issue(id: $id) {
    identifier
    title
    archivedAt
  }
}
"#;

const ARCHIVE_ISSUE_MUTATION: &str = r#"
mutation ArchiveIssue($id: String!) {
  issueArchive(id: $id) {
    success
  }
}
"#;

const GET_ISSUE_DETAILS_FOR_BULK_ARCHIVE_QUERY: &str = r#"
query GetIssueDetailsForBulkArchive($id: String!) {
  issue(id: $id) {
    identifier
    title
    archivedAt
  }
}
"#;

const BULK_ARCHIVE_ISSUE_MUTATION: &str = r#"
mutation BulkArchiveIssue($id: String!) {
  issueArchive(id: $id) {
    success
  }
}
"#;

pub fn run(args: IssueArchiveArgs) -> Result<()> {
    run_inner(args).map_err(|error| error.with_context("Failed to archive issue"))
}

fn run_inner(args: IssueArchiveArgs) -> Result<()> {
    let client = graphql::client()?;

    if is_bulk_mode(&args) {
        if args.issue_id.is_some() {
            return Err(CliError::validation(
                "Cannot combine a positional issue ID with --bulk",
            )
            .suggestion(
                "Pass every identifier through --bulk (or --bulk-file / --bulk-stdin), or drop the positional one.",
            ));
        }
        return handle_bulk_archive(&client, &args);
    }

    archive_issue(&client, args.issue_id.as_deref(), args.confirm)
}

fn archive_issue(
    client: &graphql::Client,
    issue_id: Option<&str>,
    confirm: bool,
) -> Result<()> {
    let resolved_id = linear::get_issue_identifier(issue_id)?;
    let Some(resolved_id) = resolved_id else {
        return Err(CliError::validation("Could not determine issue ID")
            .suggestion("Please provide an issue ID like 'ENG-123'."));
    };

    // Linear answers an unknown identifier with a GraphQL not-found error
    // rather than a null issue; translate both into the same clean error.
    let issue_details = errors::translate_not_found("Issue", &resolved_id, || {
        client.request(GET_ISSUE_ARCHIVE_DETAILS_QUERY, json!({ "id": resolved_id }))
    })?;

    let Some(issue) = issue_details
        .get("issue")
        .filter(|value| !value.is_null())
    else {
        return Err(CliError::not_found("Issue", &resolved_id));
    };

    let identifier = issue.get("identifier").and_then(Value::as_str).unwrap_or("");
    let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
    let archived_at = issue.get("archivedAt").filter(|value| !value.is_null());

    // Linear's issueArchive reports success on an already-archived issue, so
    // say so instead of prompting for (and reporting) a no-op.
    if archived_at.is_some() {
        output::line(&format!(
            "Issue \"{identifier}: {title}\" is already archived."
        ));
        return Ok(());
    }

    if !confirm {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --confirm to skip."));
        }
        let confirmed = prompt::confirm(
            &format!("Are you sure you want to archive \"{identifier}: {title}\"?"),
            false,
        )?;
        if !confirmed {
            output::line("Archive cancelled.");
            return Ok(());
        }
    }

    let result = client.request(ARCHIVE_ISSUE_MUTATION, json!({ "id": resolved_id }))?;
    let success = result
        .get("issueArchive")
        .and_then(|archive| archive.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Err(CliError::cli("Linear reported the archive as unsuccessful"));
    }

    output::line(&format!(
        "✓ Successfully archived issue: {identifier}: {title}"
    ));
    Ok(())
}

fn handle_bulk_archive(client: &graphql::Client, args: &IssueArchiveArgs) -> Result<()> {
    let ids = collect_bulk_ids(args)?;

    if ids.is_empty() {
        return Err(CliError::validation(
            "No issue identifiers provided for bulk archive",
        ));
    }

    output::line(&format!("Found {} issue(s) to archive.", ids.len()));

    if !args.confirm {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --confirm to skip."));
        }
        let confirmed = prompt::confirm(
            &format!("Archive {} issue(s)?", ids.len()),
            false,
        )?;
        if !confirmed {
            output::line("Bulk archive cancelled.");
            return Ok(());
        }
    }

    let operation = |issue_id_input: &str| -> Result<BulkOperationResult> {
        bulk_archive_one(client, issue_id_input)
    };

    let summary = execute_bulk_operations(&ids, operation);
    print_bulk_summary(&summary, "issue", "archived");

    if summary.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn bulk_archive_one(
    client: &graphql::Client,
    issue_id_input: &str,
) -> Result<BulkOperationResult> {
    let Some(resolved_id) = linear::get_issue_identifier(Some(issue_id_input))? else {
        return Ok(BulkOperationResult::failure(
            issue_id_input,
            issue_id_input,
            None,
            "Issue not found",
        ));
    };

    let details = client.request(
        GET_ISSUE_DETAILS_FOR_BULK_ARCHIVE_QUERY,
        json!({ "id": resolved_id }),
    );
    let details = match details {
        Ok(details) => details,
        Err(error) if error.is_not_found() => {
            return Ok(BulkOperationResult::failure(
                &resolved_id,
                &resolved_id,
                None,
                "Issue not found",
            ));
        }
        Err(error) => return Err(error),
    };

    let Some(issue) = details.get("issue").filter(|value| !value.is_null()) else {
        return Ok(BulkOperationResult::failure(
            &resolved_id,
            &resolved_id,
            None,
            "Issue not found",
        ));
    };

    let identifier = issue.get("identifier").and_then(Value::as_str).unwrap_or("");
    let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
    let archived_at = issue.get("archivedAt").filter(|value| !value.is_null());
    let name = format!("{identifier}: {title}");

    // Already archived counts as done: the requested end state holds.
    if archived_at.is_some() {
        return Ok(BulkOperationResult::success(
            &resolved_id,
            identifier,
            Some(name),
        ));
    }

    let result = client.request(BULK_ARCHIVE_ISSUE_MUTATION, json!({ "id": resolved_id }))?;
    let success = result
        .get("issueArchive")
        .and_then(|archive| archive.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Ok(BulkOperationResult::failure(
            &resolved_id,
            identifier,
            Some(name),
            "Archive operation failed",
        ));
    }

    Ok(BulkOperationResult::success(
        &resolved_id,
        identifier,
        Some(name),
    ))
}

// ---------------------------------------------------------------------------
// Bulk helpers
//
// Upstream keeps these in `src/utils/bulk.ts`; this port has no shared module
// for them, so the same logic lives inline here (and in `issue_delete.rs`).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(crate) struct BulkOperationResult {
    pub id: String,
    pub name: Option<String>,
    pub success: bool,
    pub error: Option<String>,
}

impl BulkOperationResult {
    fn success(id: &str, _identifier: &str, name: Option<String>) -> Self {
        BulkOperationResult {
            id: id.to_string(),
            name,
            success: true,
            error: None,
        }
    }

    fn failure(
        id: &str,
        _identifier: &str,
        name: Option<String>,
        error: impl Into<String>,
    ) -> Self {
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

pub(crate) fn is_bulk_mode(args: &IssueArchiveArgs) -> bool {
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

pub(crate) fn collect_bulk_ids(args: &IssueArchiveArgs) -> Result<Vec<String>> {
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

    // Deduplicate, preserving first-seen order.
    let mut seen = HashSet::new();
    all_ids.retain(|id| seen.insert(id.clone()));
    Ok(all_ids)
}

/// Run the operation for every ID, preserving input order. Errors thrown by
/// the operation become failed results carrying the error message, matching
/// `executeBulkOperations` upstream.
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
