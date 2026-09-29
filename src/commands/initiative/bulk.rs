//! Bulk-operation helpers for `initiative archive` / `initiative delete`.
//!
//! Upstream keeps these in `src/utils/bulk.ts`; this port has no shared module
//! for them, so the same logic lives here (mirroring the copies inline in
//! `src/commands/issue/issue_delete.rs` and `issue_archive.rs`).

use std::collections::HashSet;

use crate::errors::{CliError, Result};
use crate::output;

#[derive(Debug, Clone, Default)]
pub(crate) struct BulkOperationResult {
    pub id: String,
    pub name: Option<String>,
    pub success: bool,
    pub error: Option<String>,
}

impl BulkOperationResult {
    pub(crate) fn success(id: &str, name: Option<String>) -> Self {
        BulkOperationResult {
            id: id.to_string(),
            name,
            success: true,
            error: None,
        }
    }

    pub(crate) fn failure(id: &str, name: Option<String>, error: impl Into<String>) -> Self {
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

/// Parse IDs from text input, splitting on newlines, commas, and whitespace.
pub(crate) fn parse_ids(input: &str) -> Vec<String> {
    input
        .split(|c: char| c == '\n' || c == '\r' || c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

/// Gather IDs from `--bulk`, `--bulk-file`, and `--bulk-stdin`, de-duplicated
/// while preserving first-seen order.
pub(crate) fn collect_bulk_ids(
    bulk: &[String],
    bulk_file: Option<&str>,
    bulk_stdin: bool,
) -> Result<Vec<String>> {
    let mut all_ids: Vec<String> = Vec::new();

    all_ids.extend(bulk.iter().cloned());

    if let Some(path) = bulk_file {
        match std::fs::read_to_string(path) {
            Ok(content) => all_ids.extend(parse_ids(&content)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CliError::not_found("File", path));
            }
            Err(error) => return Err(error.into()),
        }
    }

    if bulk_stdin {
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

pub(crate) fn execute_bulk_operations<F>(ids: &[String], operation: F) -> BulkOperationSummary
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
