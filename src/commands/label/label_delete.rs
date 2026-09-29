//! `linear label delete` — port of `src/commands/label/label-delete.ts`.
//!
//! Confirmation and disambiguation prompts are guarded by
//! [`crate::prompt::is_interactive`]; non-interactive runs must pass `--force`
//! and `--team` instead of blocking.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output, prompt};

const DELETE_ISSUE_LABEL_MUTATION: &str = r#"
mutation DeleteIssueLabel($id: String!) {
  issueLabelDelete(id: $id) {
    success
  }
}
"#;

// The document declares exactly one variable. Linear validates an operation
// before running it and rejects one that declares a variable it never uses
// ("Variable \"$teamKey\" is never used in operation \"GetLabelByName\""), so
// the unused `$teamKey` upstream's codegen left behind must not be declared:
// the team filter below is applied client-side, exactly as upstream applies it.
const GET_LABEL_BY_NAME_QUERY: &str = r#"
query GetLabelByName($name: String!) {
  issueLabels(
    filter: {
      name: { eqIgnoreCase: $name }
    }
  ) {
    nodes {
      id
      name
      color
      team {
        key
        name
      }
    }
  }
}
"#;

const GET_LABEL_BY_ID_QUERY: &str = r#"
query GetLabelById($id: String!) {
  issueLabel(id: $id) {
    id
    name
    color
    team {
      key
      name
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct LabelDeleteArgs {
    /// Label name or UUID
    pub name_or_id: String,
    /// Team key, name, or ID to disambiguate labels with the same name
    #[arg(short = 't', long, value_name = "team")]
    pub team: Option<String>,
    /// Skip confirmation prompt
    #[arg(short = 'f', long)]
    pub force: bool,
}

pub fn run(args: LabelDeleteArgs) -> Result<()> {
    let client = graphql::client()?;

    // An explicit team may be a key, name, or ID; labels are matched on the
    // canonical key. The configured default is already a key.
    let effective_team_key = match &args.team {
        Some(team) => Some(linear::resolve_team(team)?.key),
        None => linear::get_team_key()?,
    };

    let label = resolve_label_id(&client, &args.name_or_id, effective_team_key.as_deref())?;

    let Some(label) = label else {
        let suggestion = effective_team_key
            .as_ref()
            .map(|key| format!("Searched in team {key} and workspace."));
        return Err(CliError::not_found("Label", &args.name_or_id).maybe_suggestion(suggestion));
    };

    let label_name = label.get("name").and_then(Value::as_str).unwrap_or("");
    let team_key = label
        .get("team")
        .filter(|team| !team.is_null())
        .and_then(|team| team.get("key"))
        .and_then(Value::as_str)
        .unwrap_or("Workspace");
    let label_display = format!("{label_name} ({team_key})");

    // Confirmation prompt unless --force is used.
    if !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --force to skip confirmation."));
        }
        let confirmed = prompt::confirm(
            &format!("Are you sure you want to delete label \"{label_display}\"?"),
            false,
        )?;
        if !confirmed {
            output::line("Deletion canceled");
            return Ok(());
        }
    }

    let id = label.get("id").and_then(Value::as_str).unwrap_or("");
    let result = client.request(DELETE_ISSUE_LABEL_MUTATION, json!({ "id": id }))?;

    let success = result
        .get("issueLabelDelete")
        .and_then(|delete| delete.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if success {
        output::line(&format!("✓ Deleted label: {label_display}"));
    } else {
        return Err(CliError::cli("Failed to delete label"));
    }

    Ok(())
}

fn resolve_label_id(
    client: &graphql::Client,
    name_or_id: &str,
    team_key: Option<&str>,
) -> Result<Option<Value>> {
    linear_url_guard(name_or_id)?;

    // Try as UUID first.
    if is_uuid(name_or_id) {
        if let Ok(result) = client.request(GET_LABEL_BY_ID_QUERY, json!({ "id": name_or_id })) {
            if let Some(label) = result.get("issueLabel").filter(|value| !value.is_null()) {
                return Ok(Some(label.clone()));
            }
        }
        // Fall through to name lookup.
    }

    // Try as name. A request that fails is surfaced as itself: Linear's
    // validation errors and transport failures are not "not found", and
    // reporting them as a missing label hides the real cause. (Deliberate
    // deviation — upstream's `catch` turns any failure into "not found"; see
    // AGENTS.md.)
    let result = client.request(GET_LABEL_BY_NAME_QUERY, json!({ "name": name_or_id }))?;
    let labels: Vec<Value> = result
        .get("issueLabels")
        .and_then(|connection| connection.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if labels.is_empty() {
        return Ok(None);
    }

    // If team is specified, filter by team.
    if let Some(team_key) = team_key {
        let wanted = team_key.to_lowercase();
        if let Some(team_label) = labels.iter().find(|label| {
            label
                .get("team")
                .filter(|team| !team.is_null())
                .and_then(|team| team.get("key"))
                .and_then(Value::as_str)
                .map(|key| key.to_lowercase() == wanted)
                .unwrap_or(false)
        }) {
            return Ok(Some(team_label.clone()));
        }
        // Also check for workspace label.
        if let Some(workspace_label) = labels
            .iter()
            .find(|label| label.get("team").map(Value::is_null).unwrap_or(true))
        {
            return Ok(Some(workspace_label.clone()));
        }
        return Ok(None);
    }

    // If multiple labels with same name exist, let user choose.
    if labels.len() > 1 {
        if !prompt::is_interactive() {
            return Err(CliError::validation(format!(
                "Multiple labels named \"{name_or_id}\" found"
            ))
            .suggestion("Use --team to disambiguate."));
        }
        let options: Vec<String> = labels
            .iter()
            .map(|label| {
                let name = label.get("name").and_then(Value::as_str).unwrap_or("");
                let key = label
                    .get("team")
                    .filter(|team| !team.is_null())
                    .and_then(|team| team.get("key"))
                    .and_then(Value::as_str)
                    .unwrap_or("Workspace");
                let color = label.get("color").and_then(Value::as_str).unwrap_or("");
                format!("{name} ({key}) - {color}")
            })
            .collect();

        let selected = prompt::select(
            &format!("Multiple labels named \"{name_or_id}\" found. Which one?"),
            &options,
        )?;

        return Ok(Some(labels[selected].clone()));
    }

    // Return first match (workspace labels typically).
    Ok(Some(labels[0].clone()))
}

/// Reject a Linear URL with the entity label upstream uses.
fn linear_url_guard(value: &str) -> Result<()> {
    crate::linear_url::reject_linear_url(value, "a label name or UUID")
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}
