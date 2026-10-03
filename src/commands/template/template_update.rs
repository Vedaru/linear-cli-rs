//! `linear template update` — change a template's fields.
//!
//! A local template is a file, so an edit is a rewrite of the fields given; a workspace one goes
//! through the API's `templateUpdate`, and its `templateData` can only be replaced wholesale (the
//! document is Linear's own shape), which is why `--data-file` is the flag for it rather than a set
//! of fields this command would have to translate.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, output};

use super::local;

const UPDATE_TEMPLATE_MUTATION: &str = r#"
mutation UpdateWorkspaceTemplate($id: String!, $input: TemplateUpdateInput!) {
  templateUpdate(id: $id, input: $input) {
    success
    template {
      id
      name
      type
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct TemplateUpdateArgs {
    /// Template name (local) or name/ID (workspace)
    pub template: String,
    /// New name
    #[arg(short = 'n', long, value_name = "name")]
    pub name: Option<String>,
    /// New description
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// New title pre-fill (local only)
    #[arg(short = 't', long, value_name = "title")]
    pub title: Option<String>,
    /// Priority to pre-fill (local only)
    #[arg(short = 'p', long, value_name = "priority")]
    pub priority: Option<i64>,
    /// Label to pre-fill, replacing the template's labels (local only; repeatable)
    #[arg(short = 'l', long, value_name = "label")]
    pub label: Vec<String>,
    /// Replace the API's `templateData` document from this file (`--workspace` only)
    #[arg(long = "data-file", value_name = "path")]
    pub data_file: Option<String>,
    /// Update the workspace template, not the local one
    #[arg(long)]
    pub workspace: bool,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: TemplateUpdateArgs) -> Result<()> {
    let nothing = args.name.is_none()
        && args.description.is_none()
        && args.title.is_none()
        && args.priority.is_none()
        && args.label.is_empty()
        && args.data_file.is_none();
    if nothing {
        return Err(CliError::validation("Nothing to update").suggestion(
            "Pass --name, --description, --title, --priority, --label or --data-file; the fields you leave out are left alone.",
        ));
    }

    if args.workspace {
        return update_workspace(&args);
    }
    update_local(&args)
}

fn update_local(args: &TemplateUpdateArgs) -> Result<()> {
    if args.data_file.is_some() {
        return Err(
            CliError::validation("--data-file updates a workspace template")
                .suggestion("Add --workspace, or edit the local file directly."),
        );
    }
    let Some(previous) = local::find(&args.template)? else {
        return Err(CliError::not_found("Local template", &args.template).suggestion(
            "Create it with `linear template create <name> ...`, or read the workspace one with --workspace.",
        ));
    };

    let mut fields = previous.fields;
    let mut changed: Vec<&str> = Vec::new();
    if let Some(title) = &args.title {
        fields.insert("title".to_string(), json!(title));
        changed.push("title");
    }
    if let Some(description) = &args.description {
        fields.insert("description".to_string(), json!(description));
        changed.push("description");
    }
    if let Some(priority) = args.priority {
        fields.insert("priority".to_string(), json!(priority));
        changed.push("priority");
    }
    if !args.label.is_empty() {
        fields.insert("labels".to_string(), json!(args.label));
        changed.push("labels");
    }

    let name = args.name.clone().unwrap_or_else(|| args.template.clone());
    let path = local::write(&name, &fields)?;
    // A rename writes the new file and removes the old one, in that order: a failure between the
    // two leaves the template readable under both names rather than gone.
    if name != args.template {
        local::remove(&args.template)?;
        changed.push("name");
    }

    if args.json {
        output::print_json(&json!({
            "kind": "local",
            "name": name,
            "path": path.display().to_string(),
            "fields": Value::Object(fields),
        }));
        return Ok(());
    }
    output::line(&format!("✓ Updated local template {name}"));
    output::line(&format!("  Changed: {}", changed.join(", ")));
    Ok(())
}

fn update_workspace(args: &TemplateUpdateArgs) -> Result<()> {
    let template = super::resolve_template(&args.template)?;
    let id = super::template_id(&template);

    let mut input = serde_json::Map::new();
    if let Some(name) = &args.name {
        input.insert("name".to_string(), json!(name));
    }
    if let Some(description) = &args.description {
        input.insert("description".to_string(), json!(description));
    }
    if let Some(path) = &args.data_file {
        input.insert("templateData".to_string(), super::read_json_file(path)?);
    }
    if input.is_empty() {
        return Err(CliError::validation(
            "Nothing to update in the workspace template",
        )
        .suggestion("--workspace accepts --name, --description and --data-file; the other fields are local-only."));
    }

    let client = graphql::client()?;
    let document = client.request(
        UPDATE_TEMPLATE_MUTATION,
        json!({ "id": id, "input": Value::Object(input) }),
    )?;
    let updated = document
        .get("templateUpdate")
        .ok_or_else(|| CliError::cli("Linear API response did not contain templateUpdate"))?;
    if updated.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli("Failed to update the workspace template"));
    }

    if args.json {
        output::print_json(&document);
        return Ok(());
    }
    output::line(&format!(
        "✓ Updated workspace template: {}",
        updated
            .pointer("/template/name")
            .and_then(Value::as_str)
            .unwrap_or(&args.template)
    ));
    Ok(())
}
