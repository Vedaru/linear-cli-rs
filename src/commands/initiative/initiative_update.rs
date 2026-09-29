//! `linear initiative update` — port of
//! `src/commands/initiative/initiative-update.ts`.
//!
//! The initiative reference is resolved with the shared
//! [`linear::resolve_initiative_id`] (URL, UUID, slug ID, or exact name)
//! before anything is read or written, matching upstream's local
//! `resolveInitiativeId`. A reference that resolves to nothing fails with
//! upstream's `Initiative not found: ...`.
//!
//! The wizard is opt-in (`--interactive`) and only opens on a real terminal,
//! and then only when no update flag was given; every prompt is prefilled with
//! the initiative's current values and an unchanged answer is not sent.
//! An update with nothing to change prints `No changes specified` and exits.
//!
//! Error context (`Failed to update initiative`) is supplied by the group
//! `mod.rs`, mirroring upstream's single `handleError` wrapper; the inner
//! `Failed to fetch initiative details` context is upstream's own.

use std::io::{BufRead, IsTerminal, Write};

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output, prompt};

const GET_INITIATIVE_FOR_UPDATE_QUERY: &str = r#"
query GetInitiativeForUpdate($id: String!) {
  initiative(id: $id) {
    id
    slugId
    name
    description
    status
    targetDate
    color
    icon
    owner {
      id
      displayName
    }
  }
}
"#;

const UPDATE_INITIATIVE_MUTATION: &str = r#"
mutation UpdateInitiative($id: String!, $input: InitiativeUpdateInput!) {
  initiativeUpdate(id: $id, input: $input) {
    success
    initiative {
      id
      slugId
      name
      url
    }
  }
}
"#;

/// Linear's `InitiativeStatus` enum values as (API value, label), in the order
/// the wizard offers them. The enum is case-sensitive and, as the live API's
/// introspection reports, is `Proposed | Planned | Active | Completed |
/// Canceled` — there is no `paused` initiative status.
const INITIATIVE_STATUSES: [(&str, &str); 5] = [
    ("Proposed", "Proposed"),
    ("Planned", "Planned"),
    ("Active", "Active"),
    ("Completed", "Completed"),
    ("Canceled", "Canceled"),
];

/// Canonicalise a status the user typed (or the wizard picked) to the enum
/// spelling the API expects, so any casing is accepted rather than forwarded.
///
/// Deliberate deviation: upstream lower-cases the value on the way out
/// (`input.status = status.toLowerCase()`), which sends `active` for
/// `--status Active` and makes every update fail, and the wizard's own
/// lowercase values fail the same way. An unrecognised value is forwarded
/// unchanged so the API's own error surfaces. See AGENTS.md.
fn canonical_status(status: &str) -> String {
    let wanted = status.to_lowercase();
    INITIATIVE_STATUSES
        .iter()
        .find(|(value, _)| value.to_lowercase() == wanted)
        .map(|(value, _)| (*value).to_string())
        .unwrap_or_else(|| status.to_string())
}

#[derive(Args, Debug)]
pub struct InitiativeUpdateArgs {
    /// Initiative ID, slug ID, URL, or exact name
    #[arg(value_name = "initiativeId")]
    pub initiative_id: String,
    /// New name for the initiative
    #[arg(short = 'n', long, value_name = "name")]
    pub name: Option<String>,
    /// New description
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// New status (proposed, planned, active, completed, canceled)
    #[arg(long, value_name = "status")]
    pub status: Option<String>,
    /// New owner (username, email, or @me)
    #[arg(long, value_name = "owner")]
    pub owner: Option<String>,
    /// Target completion date (YYYY-MM-DD)
    #[arg(long = "target-date", value_name = "targetDate")]
    pub target_date: Option<String>,
    /// Initiative color (hex, e.g., #5E6AD2)
    #[arg(long, value_name = "color")]
    pub color: Option<String>,
    /// Initiative icon name
    #[arg(long, value_name = "icon")]
    pub icon: Option<String>,
    /// Interactive mode for updates
    #[arg(short = 'i', long)]
    pub interactive: bool,
}

pub fn run(args: InitiativeUpdateArgs) -> Result<()> {
    // Resolve the reference first, so a bad one fails with upstream's
    // `Initiative not found: ...` before any request of our own.
    let resolved_id = linear::resolve_initiative_id(&args.initiative_id)?;

    let mut name = args.name.clone();
    let mut description = args.description.clone();
    let mut status = args.status.clone();
    let owner = args.owner.clone();
    let mut target_date = args.target_date.clone();
    let mut color_hex = args.color.clone();
    let icon = args.icon.clone();

    let client = graphql::client()?;

    // Current values: upstream always fetches these, and the wizard uses them
    // as the prefilled defaults.
    let details = client
        .request(
            GET_INITIATIVE_FOR_UPDATE_QUERY,
            json!({ "id": &resolved_id }),
        )
        .map_err(|error| error.with_context("Failed to fetch initiative details"))?;
    let initiative = details
        .get("initiative")
        .filter(|value| !value.is_null())
        .ok_or_else(|| CliError::not_found("Initiative", &args.initiative_id))?;

    // Upstream opens the wizard only when `--interactive` was passed, the
    // terminal is real, and no update flag was given.
    let is_interactive =
        args.interactive && std::io::stdout().is_terminal() && prompt::is_interactive();
    let no_flags_provided = name.is_none()
        && description.is_none()
        && status.is_none()
        && owner.is_none()
        && target_date.is_none()
        && color_hex.is_none()
        && icon.is_none();

    if no_flags_provided && is_interactive {
        output::line(&format!(
            "\nUpdating initiative: {}\n",
            str_at(initiative, "name")
        ));

        // Name
        let current_name = str_at(initiative, "name");
        let new_name = prompt_text("Name", current_name)?;
        if new_name != current_name {
            name = Some(new_name);
        }

        // Description
        let current_description = str_at(initiative, "description");
        let new_description = prompt_text("Description", current_description)?;
        if new_description != current_description {
            description = if new_description.is_empty() {
                None
            } else {
                Some(new_description)
            };
        }

        // Status
        let current_status = str_at(initiative, "status").to_lowercase();
        let options: Vec<(String, String)> = INITIATIVE_STATUSES
            .iter()
            .map(|(value, label)| ((*value).to_string(), (*label).to_string()))
            .collect();
        let default_index = options
            .iter()
            .position(|(value, _)| value.to_lowercase() == current_status)
            .unwrap_or(0);
        let new_status = prompt_select("Status", &options, default_index)?;
        if new_status.to_lowercase() != current_status {
            status = Some(new_status);
        }

        // Target date
        let current_target_date = str_at(initiative, "targetDate");
        let new_target_date = prompt_text("Target date (YYYY-MM-DD)", current_target_date)?;
        if new_target_date != current_target_date {
            target_date = if new_target_date.is_empty() {
                None
            } else {
                Some(new_target_date)
            };
        }

        // Color
        let current_color = str_at(initiative, "color");
        let new_color = prompt_text("Color (hex, e.g., #5E6AD2)", current_color)?;
        if new_color != current_color {
            color_hex = if new_color.is_empty() {
                None
            } else {
                Some(new_color)
            };
        }
    }

    // Build the update input. Unlike create, upstream tests each option for
    // `undefined`, so an explicitly blank value is forwarded; the status is
    // canonicalised (`--status Active` and `--status active` both send `Active`)
    // instead of lower-cased.
    let mut input = Map::new();
    if let Some(name) = &name {
        input.insert("name".to_string(), json!(name));
    }
    if let Some(description) = &description {
        input.insert("description".to_string(), json!(description));
    }
    if let Some(status) = &status {
        input.insert("status".to_string(), json!(canonical_status(status)));
    }
    if let Some(target_date) = &target_date {
        input.insert("targetDate".to_string(), json!(target_date));
    }
    if let Some(color) = &color_hex {
        input.insert("color".to_string(), json!(color));
    }
    if let Some(icon) = &icon {
        input.insert("icon".to_string(), json!(icon));
    }
    if let Some(owner) = &owner {
        let Some(owner_id) = linear::lookup_user_id(owner)? else {
            return Err(CliError::not_found("Owner", owner));
        };
        input.insert("ownerId".to_string(), json!(owner_id));
    }

    // Nothing to update.
    if input.is_empty() {
        output::line("No changes specified");
        return Ok(());
    }

    let result = client.request(
        UPDATE_INITIATIVE_MUTATION,
        json!({ "id": &resolved_id, "input": Value::Object(input) }),
    )?;

    let updated_successfully = result
        .pointer("/initiativeUpdate/success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !updated_successfully {
        return Err(CliError::cli("Failed to update initiative"));
    }

    let updated = result
        .pointer("/initiativeUpdate/initiative")
        .filter(|value| !value.is_null())
        .ok_or_else(|| CliError::cli("Failed to update initiative"))?;

    output::line(&format!(
        "✓ Updated initiative: {}",
        str_at(updated, "name")
    ));
    if let Some(url) = updated.get("url").and_then(Value::as_str) {
        if !url.is_empty() {
            output::line(url);
        }
    }

    Ok(())
}

/// `value.get(key).and_then(Value::as_str).unwrap_or("")`, re-declared per file
/// (there is no shared version).
fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

// ---------------------------------------------------------------------------
// Interactive prompts (mirroring @cliffy/prompt's Input/Select).
// ---------------------------------------------------------------------------

/// Read one line from the terminal. `None` is EOF (Ctrl-D), which no prompt can
/// answer, so callers stop rather than loop forever.
fn read_line() -> Result<Option<String>> {
    let mut line = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
    if read == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
}

/// `Input.prompt`: the answer, or `default` when the line is blank or EOF. The
/// default is shown in brackets, as cliffy does.
fn prompt_text(message: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        eprint!("{message}: ");
    } else {
        eprint!("{message} [{default}]: ");
    }
    let _ = std::io::stderr().flush();
    match read_line()? {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Ok(default.to_string()),
    }
}

/// `Select.prompt`: a numbered single-choice prompt whose answer is the
/// option's value. A blank line or EOF accepts the default index.
fn prompt_select(
    message: &str,
    options: &[(String, String)],
    default_index: usize,
) -> Result<String> {
    if options.is_empty() {
        return Err(CliError::cli("No options available"));
    }
    eprintln!("{message}");
    for (index, (_, label)) in options.iter().enumerate() {
        let marker = if index == default_index {
            " (default)"
        } else {
            ""
        };
        eprintln!("  {}. {label}{marker}", index + 1);
    }

    loop {
        eprint!(
            "Enter a number (1-{}) [{}]: ",
            options.len(),
            default_index + 1
        );
        let _ = std::io::stderr().flush();
        match read_line()? {
            None => return Ok(options[default_index].0.clone()),
            Some(value) if value.is_empty() => return Ok(options[default_index].0.clone()),
            Some(value) => {
                if let Ok(choice) = value.parse::<usize>() {
                    if choice >= 1 && choice <= options.len() {
                        return Ok(options[choice - 1].0.clone());
                    }
                }
                eprintln!("Please enter a number between 1 and {}.", options.len());
            }
        }
    }
}
