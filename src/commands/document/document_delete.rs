//! `linear document delete` — port of `src/commands/document/document-delete.ts`.
//!
//! Deletes a single document, or many in bulk via `--bulk` / `--bulk-file` /
//! `--bulk-stdin` (moves them to trash). The confirmation prompt is guarded by
//! [`crate::prompt::is_interactive`]; non-interactive runs must pass `--yes`
//! instead of blocking.
//!
//! The group `mod.rs` supplies the `Failed to delete document` context, so this
//! module returns bare errors.

use std::collections::HashSet;

use clap::Args;
use serde_json::{json, Value};

use crate::commands::issue::issue_delete::{
    execute_bulk_operations, parse_ids, print_bulk_summary, BulkOperationResult,
};
use crate::errors::{CliError, Result};
use crate::{graphql, linear, output, prompt};

#[derive(Args, Debug)]
pub struct DocumentDeleteArgs {
    /// Document ID, URL, or slug ID
    #[arg(value_name = "documentId")]
    pub document_id: Option<String>,
    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Delete multiple documents by slug or ID
    #[arg(long, num_args = 1.., value_name = "ids")]
    pub bulk: Vec<String>,
    /// Read document slugs/IDs from a file (one per line)
    #[arg(long = "bulk-file", value_name = "file")]
    pub bulk_file: Option<String>,
    /// Read document slugs/IDs from stdin
    #[arg(long = "bulk-stdin")]
    pub bulk_stdin: bool,
}

const GET_DOCUMENT_FOR_DELETE_QUERY: &str = r#"
query GetDocumentForDelete($id: String!) {
  document(id: $id) {
    id
    slugId
    title
  }
}
"#;

const DELETE_DOCUMENT_MUTATION: &str = r#"
mutation DeleteDocument($id: String!) {
  documentDelete(id: $id) {
    success
  }
}
"#;

const GET_DOCUMENT_FOR_BULK_DELETE_QUERY: &str = r#"
query GetDocumentForBulkDelete($id: String!) {
  document(id: $id) {
    id
    slugId
    title
  }
}
"#;

const BULK_DELETE_DOCUMENT_MUTATION: &str = r#"
mutation BulkDeleteDocument($id: String!) {
  documentDelete(id: $id) {
    success
  }
}
"#;

pub fn run(args: DocumentDeleteArgs) -> Result<()> {
    let client = graphql::client()?;

    if is_bulk_mode(&args) {
        return handle_bulk_delete(&client, &args);
    }

    // Single mode requires documentId.
    let Some(document_id) = args.document_id.as_deref() else {
        return Err(CliError::validation("Document ID required")
            .suggestion("Use --bulk for multiple documents."));
    };

    handle_single_delete(&client, document_id, args.yes)
}

fn is_bulk_mode(args: &DocumentDeleteArgs) -> bool {
    !args.bulk.is_empty() || args.bulk_file.is_some() || args.bulk_stdin
}

fn collect_bulk_ids(args: &DocumentDeleteArgs) -> Result<Vec<String>> {
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

fn handle_single_delete(client: &graphql::Client, raw_document_id: &str, yes: bool) -> Result<()> {
    let document_id = linear::resolve_document_reference(raw_document_id)?;
    let details = client.request(GET_DOCUMENT_FOR_DELETE_QUERY, json!({ "id": document_id }))?;

    let Some(document) = details.get("document").filter(|value| !value.is_null()) else {
        return Err(CliError::not_found("Document", raw_document_id));
    };
    let document_uuid = document
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(&document_id);
    let title = document.get("title").and_then(Value::as_str).unwrap_or("");

    if !yes {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --yes to skip."));
        }
        let confirmed = prompt::confirm(
            &format!("Are you sure you want to delete \"{title}\"?"),
            false,
        )?;
        if !confirmed {
            output::line("Delete cancelled.");
            return Ok(());
        }
    }

    let result = client.request(DELETE_DOCUMENT_MUTATION, json!({ "id": document_uuid }))?;
    let success = result
        .get("documentDelete")
        .and_then(|delete| delete.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Err(CliError::cli("Delete operation failed"));
    }

    output::line(&format!("✓ Deleted document: {title}"));
    Ok(())
}

fn handle_bulk_delete(client: &graphql::Client, args: &DocumentDeleteArgs) -> Result<()> {
    let ids = collect_bulk_ids(args)?;

    if ids.is_empty() {
        return Err(CliError::validation(
            "No document IDs provided for bulk delete",
        ));
    }

    output::line(&format!("Found {} document(s) to delete.", ids.len()));

    if !args.yes {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --yes to skip."));
        }
        let confirmed = prompt::confirm(&format!("Delete {} document(s)?", ids.len()), false)?;
        if !confirmed {
            output::line("Bulk delete cancelled.");
            return Ok(());
        }
    }

    let operation = |doc_id_input: &str| -> Result<BulkOperationResult> {
        bulk_delete_one(client, doc_id_input)
    };

    let summary = execute_bulk_operations(&ids, operation);
    print_bulk_summary(&summary, "document", "deleted");

    if summary.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn bulk_delete_one(client: &graphql::Client, doc_id_input: &str) -> Result<BulkOperationResult> {
    let resolved_doc_id = linear::resolve_document_reference(doc_id_input)?;
    let mut document_uuid = resolved_doc_id.clone();
    let mut title = doc_id_input.to_string();

    // Best-effort details lookup: on any failure report "Document not found"
    // (upstream's catch branch); a null document falls through to the delete.
    match client.request(
        GET_DOCUMENT_FOR_BULK_DELETE_QUERY,
        json!({ "id": resolved_doc_id }),
    ) {
        Ok(details) => {
            if let Some(document) = details.get("document").filter(|value| !value.is_null()) {
                document_uuid = document
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(&resolved_doc_id)
                    .to_string();
                title = document
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(&title)
                    .to_string();
            }
        }
        Err(_) => {
            return Ok(BulkOperationResult {
                id: doc_id_input.to_string(),
                name: None,
                success: false,
                error: Some("Document not found".to_string()),
            });
        }
    }

    let result = client.request(
        BULK_DELETE_DOCUMENT_MUTATION,
        json!({ "id": document_uuid }),
    )?;
    let success = result
        .get("documentDelete")
        .and_then(|delete| delete.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if !success {
        return Ok(BulkOperationResult {
            id: document_uuid,
            name: Some(title),
            success: false,
            error: Some("Delete operation failed".to_string()),
        });
    }

    Ok(BulkOperationResult {
        id: document_uuid,
        name: Some(title),
        success: true,
        error: None,
    })
}
