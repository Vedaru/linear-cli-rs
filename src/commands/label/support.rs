//! Shared label helpers for the `label` group: the name/UUID resolver used by
//! `label delete` and `label update`, and the colour validator `label create`
//! and `label update` both need.
//!
//! Behaviour lives here rather than in one command because both callers must
//! agree: a label resolved by `delete` has to be the same label `update` would
//! touch, including the team/workspace preference and the multi-match rules.

use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, prompt};

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

/// Resolve a label by UUID or by name, the way upstream's `resolveLabelId` does.
///
/// `None` means no such label. With a team key, that team's label wins and a
/// workspace label is the fallback; several same-named labels with no team ask
/// an interactive caller to choose, and tell a non-interactive one to pass
/// `--team`. A failed lookup is surfaced as itself, never as "not found".
pub(crate) fn resolve_label(
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

/// `#RRGGBB`, the only colour form Linear's API accepts on a label.
pub(crate) fn is_valid_hex(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(|byte| byte.is_ascii_hexdigit())
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
