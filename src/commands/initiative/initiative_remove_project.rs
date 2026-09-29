//! `linear initiative remove-project` — port of
//! `src/commands/initiative/initiative-remove-project.ts`.
//!
//! Unlinks one project from one initiative. Linear exposes the relationship
//! as a join row (`initiativeToProjects`), so the command lists the rows,
//! finds the one matching both resolved UUIDs client-side, and deletes it by
//! its own id.
//!
//! Initiative references go through [`crate::linear::resolve_initiative_id`];
//! projects go through [`crate::linear::resolve_project_id`], the same
//! resolver `project update` uses.
//!
//! Error context (`Failed to remove project from initiative`) is supplied by
//! the group `mod.rs`; the inner link lookup keeps upstream's
//! `Failed to find project link`.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output, prompt};

const GET_INITIATIVE_TO_PROJECTS_QUERY: &str = r#"
query GetInitiativeToProjects($first: Int) {
  initiativeToProjects(first: $first) {
    nodes {
      id
      initiative {
        id
      }
      project {
        id
      }
    }
  }
}
"#;

const REMOVE_PROJECT_FROM_INITIATIVE_MUTATION: &str = r#"
mutation RemoveProjectFromInitiative($id: String!) {
  initiativeToProjectDelete(id: $id) {
    success
  }
}
"#;

const GET_INITIATIVE_NAME_BY_ID_FOR_REMOVE_QUERY: &str = r#"
query GetInitiativeNameByIdForRemove($id: String!) {
  initiative(id: $id) {
    id
    name
  }
}
"#;

const GET_PROJECT_NAME_BY_ID_FOR_REMOVE_QUERY: &str = r#"
query GetProjectNameByIdForRemove($id: String!) {
  project(id: $id) {
    id
    name
  }
}
"#;

/// Unlink a project from an initiative
#[derive(Args, Debug)]
pub struct InitiativeRemoveProjectArgs {
    /// Initiative ID, slug ID, URL, or exact name
    #[arg(value_name = "initiative")]
    pub initiative: String,
    /// Project ID, slug ID, URL, or exact name
    #[arg(value_name = "project")]
    pub project: String,
    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    pub force: bool,
}

/// A resolved reference: the canonical UUID plus the name to show the user.
struct EntityRef {
    id: String,
    name: String,
}

/// Resolve `reference` to a UUID, then read its display name back through
/// `name_query`. A lookup that fails or returns nothing keeps the raw
/// reference as the name, mirroring upstream's resolver fallback.
fn resolve_with_name(
    client: &graphql::Client,
    reference: &str,
    name_query: &str,
    root: &str,
    resolve: impl FnOnce(&str) -> Result<String>,
) -> Result<EntityRef> {
    let id = resolve(reference)?;
    let name = client
        .request(name_query, json!({ "id": id }))
        .ok()
        .and_then(|data| {
            data.get(root)
                .and_then(|entity| entity.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| reference.to_string());
    Ok(EntityRef { id, name })
}

pub fn run(args: InitiativeRemoveProjectArgs) -> Result<()> {
    let client = graphql::client()?;

    let initiative = resolve_with_name(
        &client,
        &args.initiative,
        GET_INITIATIVE_NAME_BY_ID_FOR_REMOVE_QUERY,
        "initiative",
        linear::resolve_initiative_id,
    )?;
    let project = resolve_with_name(
        &client,
        &args.project,
        GET_PROJECT_NAME_BY_ID_FOR_REMOVE_QUERY,
        "project",
        linear::resolve_project_id,
    )?;

    // The join row carries its own id; find the one matching both sides.
    let links = client
        .request(GET_INITIATIVE_TO_PROJECTS_QUERY, json!({ "first": 250 }))
        .map_err(|error| error.with_context("Failed to find project link"))?;
    let link_id = links
        .pointer("/initiativeToProjects/nodes")
        .and_then(Value::as_array)
        .and_then(|nodes| {
            nodes.iter().find(|node| {
                node.pointer("/initiative/id").and_then(Value::as_str)
                    == Some(initiative.id.as_str())
                    && node.pointer("/project/id").and_then(Value::as_str)
                        == Some(project.id.as_str())
            })
        })
        .and_then(|node| node.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let Some(link_id) = link_id else {
        output::line(&format!(
            "Project \"{}\" is not linked to initiative \"{}\"",
            project.name, initiative.name
        ));
        return Ok(());
    };

    if !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation(
                "Interactive confirmation required. Use --force to skip.",
            ));
        }
        let confirmed = prompt::confirm(
            &format!(
                "Remove \"{}\" from initiative \"{}\"?",
                project.name, initiative.name
            ),
            true,
        )?;
        if !confirmed {
            output::line("Removal cancelled.");
            return Ok(());
        }
    }

    let result = client.request(
        REMOVE_PROJECT_FROM_INITIATIVE_MUTATION,
        json!({ "id": link_id }),
    )?;
    let success = result
        .pointer("/initiativeToProjectDelete/success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Err(CliError::cli("Failed to remove project from initiative"));
    }

    output::line(&format!(
        "✓ Removed \"{}\" from initiative \"{}\"",
        project.name, initiative.name
    ));
    Ok(())
}
