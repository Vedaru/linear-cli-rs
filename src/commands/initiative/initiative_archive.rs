//! `linear initiative archive` — port of
//! `src/commands/initiative/initiative-archive.ts`.
//!
//! Archives one initiative, or many in bulk via `--bulk` / `--bulk-file` /
//! `--bulk-stdin`. The bulk path is driven by the shared [`super::bulk`]
//! helpers, which mirror upstream's `src/utils/bulk.ts` (the port has no
//! `utils/bulk.ts` module of its own; `issue archive` carries an inline copy).
//!
//! Initiative references are resolved through [`crate::linear::resolve_initiative_id`],
//! which covers an initiative URL, UUID, slug ID, or exact name — the same
//! contract as upstream's per-file `resolveInitiativeId`.
//!
//! Error context (`Failed to archive initiative`) is supplied by the group
//! `mod.rs`, matching upstream's top-level `handleError` wrapper.

use clap::Args;
use serde_json::{json, Value};

use super::bulk::{
    collect_bulk_ids, execute_bulk_operations, print_bulk_summary, BulkOperationResult,
};
use crate::errors::{CliError, Result};
use crate::{graphql, linear, output, prompt};

const GET_INITIATIVE_FOR_ARCHIVE_QUERY: &str = r#"
query GetInitiativeForArchive($id: String!) {
  initiative(id: $id) {
    id
    slugId
    name
    archivedAt
  }
}
"#;

const ARCHIVE_INITIATIVE_MUTATION: &str = r#"
mutation ArchiveInitiative($id: String!) {
  initiativeArchive(id: $id) {
    success
  }
}
"#;

const GET_INITIATIVE_NAME_FOR_BULK_ARCHIVE_QUERY: &str = r#"
query GetInitiativeNameForBulkArchive($id: String!) {
  initiative(id: $id) {
    id
    name
    archivedAt
  }
}
"#;

const BULK_ARCHIVE_INITIATIVE_MUTATION: &str = r#"
mutation BulkArchiveInitiative($id: String!) {
  initiativeArchive(id: $id) {
    success
  }
}
"#;

/// Archive a Linear initiative
#[derive(Args, Debug)]
pub struct InitiativeArchiveArgs {
    /// Initiative ID, slug ID, URL, or exact name
    #[arg(value_name = "initiativeId")]
    pub initiative_id: Option<String>,
    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    pub force: bool,
    /// Archive multiple initiatives by ID, slug, or name
    #[arg(long, num_args = 1.., value_name = "ids")]
    pub bulk: Vec<String>,
    /// Read initiative IDs from a file (one per line)
    #[arg(long = "bulk-file", value_name = "file")]
    pub bulk_file: Option<String>,
    /// Read initiative IDs from stdin
    #[arg(long = "bulk-stdin")]
    pub bulk_stdin: bool,
}

pub fn run(args: InitiativeArchiveArgs) -> Result<()> {
    let client = graphql::client()?;

    if is_bulk_mode(&args) {
        return handle_bulk_archive(&client, &args);
    }

    // Single mode requires the positional initiative ID.
    let Some(initiative_id) = args.initiative_id.as_deref() else {
        return Err(CliError::validation(
            "Initiative ID required. Use --bulk for multiple initiatives.",
        ));
    };

    handle_single_archive(&client, initiative_id, args.force)
}

fn is_bulk_mode(args: &InitiativeArchiveArgs) -> bool {
    !args.bulk.is_empty() || args.bulk_file.is_some() || args.bulk_stdin
}

fn handle_single_archive(
    client: &graphql::Client,
    initiative_id: &str,
    force: bool,
) -> Result<()> {
    // Resolve first so a bad reference fails with upstream's
    // `Initiative not found: ...`.
    let resolved_id = linear::resolve_initiative_id(initiative_id)?;

    // Get initiative details for the confirmation message.
    let details = client
        .request(
            GET_INITIATIVE_FOR_ARCHIVE_QUERY,
            json!({ "id": resolved_id }),
        )
        .map_err(|error| error.with_context("Failed to fetch initiative details"))?;

    let Some(initiative) = details.get("initiative").filter(|value| !value.is_null()) else {
        return Err(CliError::not_found("Initiative", initiative_id));
    };

    let name = initiative.get("name").and_then(Value::as_str).unwrap_or("");

    // Already archived counts as done: say so instead of prompting for a
    // no-op.
    if initiative
        .get("archivedAt")
        .filter(|value| !value.is_null())
        .is_some()
    {
        output::line(&format!("Initiative \"{name}\" is already archived."));
        return Ok(());
    }

    if !force {
        if !prompt::is_interactive() {
            return Err(CliError::validation(
                "Interactive confirmation required. Use --force to skip.",
            ));
        }
        let confirmed = prompt::confirm(&format!("Archive initiative \"{name}\"?"), true)?;
        if !confirmed {
            output::line("Archive cancelled.");
            return Ok(());
        }
    }

    let result = client.request(ARCHIVE_INITIATIVE_MUTATION, json!({ "id": resolved_id }))?;
    let success = result
        .pointer("/initiativeArchive/success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Err(CliError::cli("Failed to archive initiative"));
    }

    output::line(&format!("✓ Archived initiative: {name}"));
    Ok(())
}

fn handle_bulk_archive(client: &graphql::Client, args: &InitiativeArchiveArgs) -> Result<()> {
    let ids = collect_bulk_ids(&args.bulk, args.bulk_file.as_deref(), args.bulk_stdin)?;

    if ids.is_empty() {
        return Err(CliError::validation(
            "No initiative IDs provided for bulk archive.",
        ));
    }

    output::line(&format!("Found {} initiative(s) to archive.", ids.len()));

    if !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation(
                "Interactive confirmation required. Use --force to skip.",
            ));
        }
        let confirmed = prompt::confirm(
            &format!("Archive {} initiative(s)?", ids.len()),
            false,
        )?;
        if !confirmed {
            output::line("Bulk archive cancelled.");
            return Ok(());
        }
    }

    let operation =
        |id_or_slug_or_name: &str| -> Result<BulkOperationResult> {
            bulk_archive_one(client, id_or_slug_or_name)
        };

    let summary = execute_bulk_operations(&ids, operation);
    print_bulk_summary(&summary, "initiative", "archived");

    // Exit with an error code if any row failed, matching upstream's
    // `Deno.exit(1)`.
    if summary.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn bulk_archive_one(
    client: &graphql::Client,
    id_or_slug_or_name: &str,
) -> Result<BulkOperationResult> {
    // Upstream reports an unresolvable reference as a per-row failure rather
    // than an error, so one bad ID does not abort the batch.
    let Ok(resolved_id) = linear::resolve_initiative_id(id_or_slug_or_name) else {
        return Ok(BulkOperationResult::failure(
            id_or_slug_or_name,
            Some(id_or_slug_or_name.to_string()),
            "Initiative not found",
        ));
    };

    // Best-effort name and archive state for display; a failed lookup keeps
    // the default name, matching upstream's empty `catch`.
    let mut name = id_or_slug_or_name.to_string();
    let mut already_archived = false;
    if let Ok(details) = client.request(
        GET_INITIATIVE_NAME_FOR_BULK_ARCHIVE_QUERY,
        json!({ "id": resolved_id }),
    ) {
        if let Some(initiative) = details.get("initiative").filter(|value| !value.is_null()) {
            if let Some(fetched) = initiative.get("name").and_then(Value::as_str) {
                name = fetched.to_string();
            }
            already_archived = initiative
                .get("archivedAt")
                .filter(|value| !value.is_null())
                .is_some();
        }
    }

    if already_archived {
        return Ok(BulkOperationResult::success(&resolved_id, Some(name)));
    }

    let result = client.request(BULK_ARCHIVE_INITIATIVE_MUTATION, json!({ "id": resolved_id }))?;
    let success = result
        .pointer("/initiativeArchive/success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Ok(BulkOperationResult::failure(
            &resolved_id,
            Some(name),
            "Archive operation failed",
        ));
    }

    Ok(BulkOperationResult::success(&resolved_id, Some(name)))
}
