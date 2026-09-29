//! `linear initiative add-project` — port of
//! `src/commands/initiative/initiative-add-project.ts`.
//!
//! Links one project to one initiative through the `initiativeToProjectCreate`
//! mutation. Both references are resolved to a UUID first (upstream's
//! per-file `resolveInitiativeId` / `resolveProjectId`), and their display
//! names are read back so the success line can name both sides.
//!
//! Initiative references go through [`crate::linear::resolve_initiative_id`];
//! projects go through [`crate::linear::resolve_project_id`], the same
//! resolver `project update` uses.
//!
//! Linear answers an existing link with an error whose message mentions
//! "already exists" / "duplicate" rather than a failed mutation, so that case
//! is reported as a no-op, exactly as upstream's `catch` does.
//!
//! Error context (`Failed to add project to initiative`) is supplied by the
//! group `mod.rs`, matching upstream's top-level `handleError` wrapper.

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output};

const ADD_PROJECT_TO_INITIATIVE_MUTATION: &str = r#"
mutation AddProjectToInitiative($input: InitiativeToProjectCreateInput!) {
  initiativeToProjectCreate(input: $input) {
    success
    initiativeToProject {
      id
    }
  }
}
"#;

const GET_INITIATIVE_NAME_BY_ID_QUERY: &str = r#"
query GetInitiativeNameById($id: String!) {
  initiative(id: $id) {
    id
    name
  }
}
"#;

const GET_PROJECT_NAME_BY_ID_QUERY: &str = r#"
query GetProjectNameById($id: String!) {
  project(id: $id) {
    id
    name
  }
}
"#;

/// Link a project to an initiative
#[derive(Args, Debug)]
pub struct InitiativeAddProjectArgs {
    /// Initiative ID, slug ID, URL, or exact name
    #[arg(value_name = "initiative")]
    pub initiative: String,
    /// Project ID, slug ID, URL, or exact name
    #[arg(value_name = "project")]
    pub project: String,
    /// Sort order within initiative
    #[arg(long = "sort-order", value_name = "sortOrder")]
    pub sort_order: Option<i64>,
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

pub fn run(args: InitiativeAddProjectArgs) -> Result<()> {
    let client = graphql::client()?;

    let initiative = resolve_with_name(
        &client,
        &args.initiative,
        GET_INITIATIVE_NAME_BY_ID_QUERY,
        "initiative",
        linear::resolve_initiative_id,
    )?;
    let project = resolve_with_name(
        &client,
        &args.project,
        GET_PROJECT_NAME_BY_ID_QUERY,
        "project",
        linear::resolve_project_id,
    )?;

    // Only provided fields are sent; `sortOrder` is included even at 0.
    let mut input = Map::new();
    input.insert("initiativeId".to_string(), json!(initiative.id));
    input.insert("projectId".to_string(), json!(project.id));
    if let Some(sort_order) = args.sort_order {
        input.insert("sortOrder".to_string(), json!(sort_order));
    }

    match client.request(
        ADD_PROJECT_TO_INITIATIVE_MUTATION,
        json!({ "input": Value::Object(input) }),
    ) {
        Ok(result) => {
            let success = result
                .pointer("/initiativeToProjectCreate/success")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !success {
                return Err(CliError::cli("Failed to add project to initiative"));
            }

            output::line(&format!(
                "✓ Added \"{}\" to initiative \"{}\"",
                project.name, initiative.name
            ));
            Ok(())
        }
        Err(error) => {
            // An existing link surfaces as a GraphQL error, not a failed
            // mutation; treat it as the requested end state.
            if error.user_message.contains("already exists")
                || error.user_message.contains("duplicate")
            {
                output::line(&format!(
                    "Project \"{}\" is already linked to initiative \"{}\"",
                    project.name, initiative.name
                ));
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}
