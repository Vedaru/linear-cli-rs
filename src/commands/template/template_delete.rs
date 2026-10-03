//! `linear template delete` — remove a template, local or workspace.
//!
//! Local by default, and a file removal cannot be undone, so it confirms off a terminal the way the
//! rest of this CLI's destructive commands do. `--workspace` goes through the API's
//! `templateDelete`, which is the team's template rather than this machine's file - hence the
//! explicit flag rather than a fallback that guesses which one was meant.

use clap::Args;
use serde_json::json;

use crate::errors::{CliError, Result};
use crate::{graphql, output, prompt};

use super::local;

const DELETE_TEMPLATE_MUTATION: &str = r#"
mutation DeleteWorkspaceTemplate($id: String!) {
  templateDelete(id: $id) {
    success
  }
}
"#;

#[derive(Args, Debug)]
pub struct TemplateDeleteArgs {
    /// Template name (local) or name/ID (workspace)
    pub template: String,
    /// Delete the workspace template, not the local one
    #[arg(long)]
    pub workspace: bool,
    /// Skip confirmation prompt
    #[arg(short = 'f', long)]
    pub force: bool,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: TemplateDeleteArgs) -> Result<()> {
    if args.workspace {
        return delete_workspace(&args);
    }

    let Some(found) = local::find(&args.template)? else {
        return Err(CliError::not_found("Local template", &args.template).suggestion(
            "Run `linear template show <name>` to see what this machine has, or add --workspace for Linear's own template.",
        ));
    };

    if !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --force to skip confirmation."));
        }
        let confirmed = prompt::confirm(
            &format!(
                "Delete local template \"{}\" ({} field(s))?",
                found.name,
                found.fields.len()
            ),
            false,
        )?;
        if !confirmed {
            output::line("Deletion canceled");
            return Ok(());
        }
    }

    let path = local::remove(&args.template)?;
    if args.json {
        output::print_json(&json!({
            "kind": "local",
            "name": args.template,
            "path": path.display().to_string(),
            "deleted": true,
        }));
        return Ok(());
    }
    output::line(&format!("✓ Deleted local template {}", args.template));
    Ok(())
}

fn delete_workspace(args: &TemplateDeleteArgs) -> Result<()> {
    let template = super::resolve_template(&args.template)?;
    let id = super::template_id(&template);

    if !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --force to skip confirmation."));
        }
        let confirmed = prompt::confirm(
            &format!(
                "Delete workspace template \"{}\"?",
                super::template_name(&template)
            ),
            false,
        )?;
        if !confirmed {
            output::line("Deletion canceled");
            return Ok(());
        }
    }

    let client = graphql::client()?;
    let document = client.request(DELETE_TEMPLATE_MUTATION, json!({ "id": id }))?;
    let deleted = document
        .get("templateDelete")
        .ok_or_else(|| CliError::cli("Linear API response did not contain templateDelete"))?;
    if deleted.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(CliError::cli("Failed to delete the workspace template"));
    }

    if args.json {
        output::print_json(&document);
        return Ok(());
    }
    output::line(&format!(
        "✓ Deleted workspace template {}",
        super::template_name(&template)
    ));
    Ok(())
}
