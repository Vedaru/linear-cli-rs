//! `linear label update` — rename, recolour, or redescribe a label.
//!
//! The API's `issueLabelUpdate`; upstream has no equivalent (its `label` group
//! is create/list/delete only), so a label that exists can otherwise never be
//! corrected. Resolution, and the hex-colour rule, are shared with
//! [`super::support`] so `update` touches exactly the label `delete` would.

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output};

use super::support::{is_valid_hex, resolve_label};

const UPDATE_ISSUE_LABEL_MUTATION: &str = r#"
mutation UpdateIssueLabel($id: String!, $input: IssueLabelUpdateInput!) {
  issueLabelUpdate(id: $id, input: $input) {
    success
    issueLabel {
      id
      name
      color
      description
      team {
        key
        name
      }
    }
  }
}
"#;

/// Update a label
#[derive(Args, Debug)]
pub struct LabelUpdateArgs {
    /// Label name or UUID
    pub name_or_id: String,
    /// New name for the label
    #[arg(short = 'n', long, value_name = "name")]
    pub name: Option<String>,
    /// New color hex code (e.g., #EB5757)
    #[arg(short = 'c', long, value_name = "color")]
    pub color: Option<String>,
    /// New description (an empty string clears it)
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// Team key, name, or ID to disambiguate labels with the same name
    #[arg(short = 't', long, value_name = "team")]
    pub team: Option<String>,
}

pub fn run(args: LabelUpdateArgs) -> Result<()> {
    if args.name.is_none() && args.color.is_none() && args.description.is_none() {
        return Err(CliError::validation("Nothing to update")
            .suggestion("Pass --name, --color, or --description."));
    }
    if let Some(color) = &args.color {
        if !is_valid_hex(color) {
            return Err(CliError::validation(
                "Color must be a valid hex code (e.g., #EB5757)",
            ));
        }
    }

    let client = graphql::client()?;

    // An explicit team may be a key, name, or ID; labels are matched on the
    // canonical key. The configured default is already a key.
    let effective_team_key = match &args.team {
        Some(team) => Some(linear::resolve_team(team)?.key),
        None => linear::get_team_key()?,
    };

    let label = resolve_label(&client, &args.name_or_id, effective_team_key.as_deref())?;
    let Some(label) = label else {
        let suggestion = effective_team_key
            .as_ref()
            .map(|key| format!("Searched in team {key} and workspace."));
        return Err(CliError::not_found("Label", &args.name_or_id).maybe_suggestion(suggestion));
    };
    let id = label.get("id").and_then(Value::as_str).unwrap_or("");

    let mut input = Map::new();
    if let Some(name) = &args.name {
        input.insert("name".to_string(), json!(name));
    }
    if let Some(color) = &args.color {
        input.insert("color".to_string(), json!(color));
    }
    if let Some(description) = &args.description {
        input.insert("description".to_string(), json!(description));
    }

    let result = client.request(
        UPDATE_ISSUE_LABEL_MUTATION,
        json!({ "id": id, "input": Value::Object(input) }),
    )?;

    let updated = result
        .get("issueLabelUpdate")
        .cloned()
        .unwrap_or(Value::Null);
    if !updated
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CliError::cli("Failed to update label"));
    }

    // Same shape as `label create`, so a created and an updated label read the
    // same way.
    let label = updated.get("issueLabel").cloned().unwrap_or(Value::Null);
    let name = label.get("name").and_then(Value::as_str).unwrap_or("");
    output::line(&format!("✓ Updated label: {name}"));
    output::line(&format!(
        "  Color: {}",
        label.get("color").and_then(Value::as_str).unwrap_or("")
    ));
    if let Some(description) = label
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        output::line(&format!("  Description: {description}"));
    }

    let scope = label
        .get("team")
        .filter(|team| !team.is_null())
        .and_then(|team| team.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(|team_name| {
            let key = label
                .get("team")
                .and_then(|team| team.get("key"))
                .and_then(Value::as_str)
                .unwrap_or("");
            format!("{team_name} ({key})")
        })
        .unwrap_or_else(|| "Workspace".to_string());
    output::line(&format!("  Scope: {scope}"));

    Ok(())
}
