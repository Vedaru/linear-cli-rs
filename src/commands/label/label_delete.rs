//! `linear label delete` — port of `src/commands/label/label-delete.ts`.
//!
//! Confirmation and disambiguation prompts are guarded by
//! [`crate::prompt::is_interactive`]; non-interactive runs must pass `--force`
//! and `--team` instead of blocking. Label resolution (name or UUID, team vs
//! workspace) lives in [`super::support`], shared with `label update`.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output, prompt};

use super::support::resolve_label;

const DELETE_ISSUE_LABEL_MUTATION: &str = r#"
mutation DeleteIssueLabel($id: String!) {
  issueLabelDelete(id: $id) {
    success
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

    let label = resolve_label(&client, &args.name_or_id, effective_team_key.as_deref())?;

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
