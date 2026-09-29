//! `linear initiative delete` — port of
//! `src/commands/initiative/initiative-delete.ts`.
//!
//! Deletes one initiative, or many in bulk via `--bulk` / `--bulk-file` /
//! `--bulk-stdin` (the rows and the summary come from the shared
//! [`super::bulk`] helpers, mirroring upstream's `utils/bulk.ts`).
//!
//! The single path warns about the projects that will be unlinked, then
//! requires both a yes/no confirmation and the initiative's name typed back
//! before it deletes, as upstream does for a permanent action. Confirmation is
//! only offered on a real terminal; a non-interactive caller must pass
//! `--force`.
//!
//! The single and bulk paths both resolve through
//! [`linear::resolve_initiative_id_including_archived`] (URL, UUID, slug ID, or
//! exact name, archived entities included), matching upstream's local
//! `resolveInitiativeId` — a delete has to be able to name an archived
//! initiative, which the shared [`linear::resolve_initiative_id`] cannot.
//!
//! Error context (`Failed to delete initiative`) is supplied by the group
//! `mod.rs`, mirroring upstream's single `handleError` wrapper; the inner
//! `Failed to fetch initiative details` context is upstream's own.

use std::io::{BufRead, Write};

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, ErrorKind, Result};
use crate::{graphql, linear, output, prompt};

use super::bulk::{
    collect_bulk_ids, execute_bulk_operations, print_bulk_summary, BulkOperationResult,
};

const GET_INITIATIVE_FOR_DELETE_QUERY: &str = r#"
query GetInitiativeForDelete($id: String!) {
  initiative(id: $id) {
    id
    slugId
    name
    projects {
      nodes {
        id
      }
    }
  }
}
"#;

const DELETE_INITIATIVE_MUTATION: &str = r#"
mutation DeleteInitiative($id: String!) {
  initiativeDelete(id: $id) {
    success
  }
}
"#;

const GET_INITIATIVE_NAME_FOR_BULK_DELETE_QUERY: &str = r#"
query GetInitiativeNameForBulkDelete($id: String!) {
  initiative(id: $id) {
    id
    name
  }
}
"#;

const BULK_DELETE_INITIATIVE_MUTATION: &str = r#"
mutation BulkDeleteInitiative($id: String!) {
  initiativeDelete(id: $id) {
    success
  }
}
"#;

#[derive(Args, Debug)]
pub struct InitiativeDeleteArgs {
    /// Initiative ID, slug ID, URL, or exact name
    #[arg(value_name = "initiativeId")]
    pub initiative_id: Option<String>,
    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    pub force: bool,
    /// Delete multiple initiatives by ID, slug, or name
    #[arg(long, num_args = 1.., value_name = "ids")]
    pub bulk: Vec<String>,
    /// Read initiative IDs from a file (one per line)
    #[arg(long = "bulk-file", value_name = "file")]
    pub bulk_file: Option<String>,
    /// Read initiative IDs from stdin
    #[arg(long = "bulk-stdin")]
    pub bulk_stdin: bool,
}

pub fn run(args: InitiativeDeleteArgs) -> Result<()> {
    let client = graphql::client()?;

    // Upstream's `isBulkMode({ bulk, bulkFile, bulkStdin })`.
    if !args.bulk.is_empty() || args.bulk_file.is_some() || args.bulk_stdin {
        return handle_bulk_delete(&client, &args);
    }

    // Single mode requires a reference.
    let Some(initiative_id) = args.initiative_id.as_deref() else {
        return Err(CliError::validation(
            "Initiative ID required. Use --bulk for multiple initiatives.",
        ));
    };

    handle_single_delete(&client, initiative_id, args.force)
}

fn handle_single_delete(
    client: &graphql::Client,
    initiative_id: &str,
    force: bool,
) -> Result<()> {
    // Upstream's local `resolveInitiativeId`: archived entities included, so an
    // archived initiative can be addressed by URL, slug ID, or name.
    let resolved_id = linear::resolve_initiative_id_including_archived(initiative_id)?;

    // Details for the confirmation message.
    let details = client
        .request(GET_INITIATIVE_FOR_DELETE_QUERY, json!({ "id": &resolved_id }))
        .map_err(|error| error.with_context("Failed to fetch initiative details"))?;
    let initiative = details
        .get("initiative")
        .filter(|value| !value.is_null())
        .ok_or_else(|| CliError::not_found("Initiative", initiative_id))?;

    let name = str_at(initiative, "name");
    let project_count = initiative
        .pointer("/projects/nodes")
        .and_then(Value::as_array)
        .map_or(0, |nodes| nodes.len());

    // Warn about the links that disappear with the initiative.
    if project_count > 0 {
        output::line(&format!(
            "\n⚠️  Initiative \"{name}\" has {project_count} linked project(s)."
        ));
        output::line("Deleting the initiative will unlink these projects.\n");
    }

    // Typed confirmation, for safety, unless --force.
    if !force {
        if !prompt::is_interactive() {
            return Err(CliError::validation(
                "Interactive confirmation required. Use --force to skip.",
            ));
        }
        output::line("\n⚠️  This action is PERMANENT and cannot be undone.\n");

        let confirmed = prompt::confirm(
            &format!("Are you sure you want to permanently delete \"{name}\"?"),
            false,
        )?;
        if !confirmed {
            output::line("Delete cancelled.");
            return Ok(());
        }

        // Typing the name back is the extra confirmation upstream requires.
        let typed_name = prompt_text("Type the initiative name to confirm deletion")?;
        if typed_name != name {
            output::line("Name does not match. Delete cancelled.");
            return Ok(());
        }
    }

    let result = client.request(DELETE_INITIATIVE_MUTATION, json!({ "id": &resolved_id }))?;
    let deleted = result
        .pointer("/initiativeDelete/success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !deleted {
        return Err(CliError::cli("Failed to delete initiative"));
    }

    output::line(&format!("✓ Permanently deleted initiative: {name}"));
    Ok(())
}

fn handle_bulk_delete(client: &graphql::Client, args: &InitiativeDeleteArgs) -> Result<()> {
    let ids = collect_bulk_ids(&args.bulk, args.bulk_file.as_deref(), args.bulk_stdin)?;

    if ids.is_empty() {
        return Err(CliError::validation(
            "No initiative IDs provided for bulk delete.",
        ));
    }

    output::line(&format!("Found {} initiative(s) to delete.", ids.len()));
    output::line("\n⚠️  This action is PERMANENT and cannot be undone.\n");

    if !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation(
                "Interactive confirmation required. Use --force to skip.",
            ));
        }
        let confirmed = prompt::confirm(
            &format!("Permanently delete {} initiative(s)?", ids.len()),
            false,
        )?;
        if !confirmed {
            output::line("Bulk delete cancelled.");
            return Ok(());
        }
    }

    let operation =
        |reference: &str| -> Result<BulkOperationResult> { bulk_delete_one(client, reference) };
    let summary = execute_bulk_operations(&ids, operation);
    print_bulk_summary(&summary, "initiative", "deleted");

    // Any failed row makes the run exit non-zero, as upstream's `Deno.exit(1)`.
    if summary.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn bulk_delete_one(client: &graphql::Client, reference: &str) -> Result<BulkOperationResult> {
    // A reference that resolves to nothing is a per-row failure ("Initiative
    // not found"); every other error propagates and is recorded by the runner.
    // Same archived-aware resolver as the single path, as upstream shares one.
    let resolved_id = match linear::resolve_initiative_id_including_archived(reference) {
        Ok(id) => id,
        Err(error) if error.kind == ErrorKind::NotFound => {
            return Ok(BulkOperationResult::failure(
                reference,
                Some(reference.to_string()),
                "Initiative not found",
            ));
        }
        Err(error) => return Err(error),
    };

    // Best-effort name lookup for the summary; the raw reference stands in when
    // it fails, matching upstream's try/catch around this request.
    let mut name = reference.to_string();
    if let Ok(details) =
        client.request(GET_INITIATIVE_NAME_FOR_BULK_DELETE_QUERY, json!({ "id": &resolved_id }))
    {
        if let Some(initiative) = details.get("initiative").filter(|value| !value.is_null()) {
            if let Some(found) = initiative.get("name").and_then(Value::as_str) {
                name = found.to_string();
            }
        }
    }

    let result = client.request(BULK_DELETE_INITIATIVE_MUTATION, json!({ "id": &resolved_id }))?;
    let deleted = result
        .pointer("/initiativeDelete/success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !deleted {
        return Ok(BulkOperationResult::failure(
            &resolved_id,
            Some(name),
            "Delete operation failed",
        ));
    }

    Ok(BulkOperationResult::success(&resolved_id, Some(name)))
}

/// `value.get(key).and_then(Value::as_str).unwrap_or("")`.
fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `Input.prompt`: one line from the terminal. EOF reads as a blank answer, so
/// the typed-name check cancels rather than blocking.
fn prompt_text(message: &str) -> Result<String> {
    eprint!("{message}: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
    if read == 0 {
        return Ok(String::new());
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}
