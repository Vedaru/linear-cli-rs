//! `linear project delete` — port of
//! `src/commands/project/project-delete.ts`.
//!
//! The group `mod.rs` supplies the `Failed to delete project` context, so this
//! module returns bare errors. Confirmation is only offered on a real terminal;
//! a non-interactive caller must pass `--force`.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output, prompt};

const DELETE_PROJECT_MUTATION: &str = r#"
mutation DeleteProject($id: String!) {
  projectDelete(id: $id) {
    success
    entity {
      id
      name
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct ProjectDeleteArgs {
    /// Project ID, slug, or name
    #[arg(value_name = "projectId")]
    pub project_id: String,
    /// Skip confirmation prompt
    #[arg(short = 'f', long)]
    pub force: bool,
}

pub fn run(args: ProjectDeleteArgs) -> Result<()> {
    if !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --force to skip confirmation."));
        }
        let confirmed = prompt::confirm(
            &format!(
                "Are you sure you want to delete project {}?",
                args.project_id
            ),
            false,
        )?;
        if !confirmed {
            output::line("Deletion canceled");
            return Ok(());
        }
    }

    let client = graphql::client()?;
    let resolved_id = linear::resolve_project_id(&args.project_id)?;

    let result = client.request(DELETE_PROJECT_MUTATION, json!({ "id": resolved_id }))?;
    let project_delete = result
        .get("projectDelete")
        .cloned()
        .ok_or_else(|| CliError::cli("Failed to delete project"))?;

    if project_delete.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli("Failed to delete project"));
    }

    let display_name = project_delete
        .pointer("/entity/name")
        .and_then(Value::as_str)
        .unwrap_or(&args.project_id);
    output::line(&format!("✓ Deleted project: {display_name}"));
    Ok(())
}
