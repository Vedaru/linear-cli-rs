//! `linear initiative unarchive` — port of
//! `src/commands/initiative/initiative-unarchive.ts`.
//!
//! Restores a single archived initiative. The details lookup opts into
//! archived entities (`includeArchived: true`), since Linear hides them from
//! the default `initiatives` filter.
//!
//! Initiative references are resolved through
//! [`crate::linear::resolve_initiative_id_including_archived`], which covers an
//! initiative URL, UUID, slug ID, or exact name. Upstream's unarchive command
//! carries its own local resolver purely so that every lookup opts into
//! archived entities (`includeArchived: true`); the shared
//! [`crate::linear::resolve_initiative_id`] would exclude the very initiatives
//! this command exists to restore.
//!
//! Error context (`Failed to unarchive initiative`) is supplied by the group
//! `mod.rs`, matching upstream's top-level `handleError` wrapper.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output, prompt};

const GET_INITIATIVE_FOR_UNARCHIVE_QUERY: &str = r#"
query GetInitiativeForUnarchive($id: ID!) {
  initiatives(filter: { id: { eq: $id } }, includeArchived: true) {
    nodes {
      id
      slugId
      name
      archivedAt
    }
  }
}
"#;

const UNARCHIVE_INITIATIVE_MUTATION: &str = r#"
mutation UnarchiveInitiative($id: String!) {
  initiativeUnarchive(id: $id) {
    success
    entity {
      id
      slugId
      name
      url
    }
  }
}
"#;

/// Unarchive a Linear initiative
#[derive(Args, Debug)]
pub struct InitiativeUnarchiveArgs {
    /// Initiative ID, slug ID, URL, or exact name
    #[arg(value_name = "initiativeId")]
    pub initiative_id: String,
    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    pub force: bool,
}

pub fn run(args: InitiativeUnarchiveArgs) -> Result<()> {
    let client = graphql::client()?;

    // Resolve first so a bad reference fails with upstream's
    // `Initiative not found: ...`. This uses the archived-aware resolver:
    // upstream's unarchive command has its own local `resolveInitiativeId`
    // precisely so a URL, slug ID, or name can name an archived initiative.
    let resolved_id = linear::resolve_initiative_id_including_archived(&args.initiative_id)?;

    // The details lookup must include archived initiatives; the default
    // filter would return nothing for the very entity being unarchived.
    let details = client
        .request(
            GET_INITIATIVE_FOR_UNARCHIVE_QUERY,
            json!({ "id": resolved_id }),
        )
        .map_err(|error| error.with_context("Failed to fetch initiative details"))?;

    let Some(initiative) = details
        .pointer("/initiatives/nodes")
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.first())
    else {
        return Err(CliError::not_found("Initiative", &args.initiative_id));
    };

    let name = initiative.get("name").and_then(Value::as_str).unwrap_or("");

    // Already unarchived: report it instead of prompting for a no-op.
    if initiative
        .get("archivedAt")
        .filter(|value| !value.is_null())
        .is_none()
    {
        output::line(&format!("Initiative \"{name}\" is not archived."));
        return Ok(());
    }

    if !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation(
                "Interactive confirmation required. Use --force to skip.",
            ));
        }
        let confirmed = prompt::confirm(
            &format!("Are you sure you want to unarchive \"{name}\"?"),
            true,
        )?;
        if !confirmed {
            output::line("Unarchive cancelled.");
            return Ok(());
        }
    }

    let result = client.request(UNARCHIVE_INITIATIVE_MUTATION, json!({ "id": resolved_id }))?;
    let success = result
        .pointer("/initiativeUnarchive/success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Err(CliError::cli("Failed to unarchive initiative"));
    }

    let unarchived = result.pointer("/initiativeUnarchive/entity");
    let unarchived_name = unarchived
        .and_then(|entity| entity.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    output::line(&format!("✓ Unarchived initiative: {unarchived_name}"));
    if let Some(url) = unarchived
        .and_then(|entity| entity.get("url"))
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
    {
        output::line(url);
    }

    Ok(())
}
