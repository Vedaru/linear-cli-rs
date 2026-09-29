//! `linear label create` — port of `src/commands/label/label-create.ts`.
//!
//! Interactive mode is guarded by [`crate::prompt::is_interactive`]; under an
//! agent harness the command never blocks — it either takes the flag values or
//! fails validation with the flag to pass.

use std::io::{BufRead, Write};

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output, prompt};

use super::support::is_valid_hex;

const CREATE_ISSUE_LABEL_MUTATION: &str = r#"
mutation CreateIssueLabel($input: IssueLabelCreateInput!) {
  issueLabelCreate(input: $input) {
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

/// Common label colors from Linear's palette; index 6 (Indigo) is the default.
const DEFAULT_COLORS: [(&str, &str); 10] = [
    ("Red", "#EB5757"),
    ("Orange", "#F2994A"),
    ("Yellow", "#F2C94C"),
    ("Green", "#27AE60"),
    ("Teal", "#0D9488"),
    ("Blue", "#2F80ED"),
    ("Indigo", "#5E6AD2"),
    ("Purple", "#8B5CF6"),
    ("Pink", "#BB6BD9"),
    ("Gray", "#6B6F76"),
];

const DEFAULT_COLOR: &str = DEFAULT_COLORS[6].1;

#[derive(Args, Debug)]
pub struct LabelCreateArgs {
    /// Label name (required)
    #[arg(short = 'n', long, value_name = "name")]
    pub name: Option<String>,
    /// Color hex code (e.g., #EB5757)
    #[arg(short = 'c', long, value_name = "color")]
    pub color: Option<String>,
    /// Label description
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// Team key, name, or ID for a team-specific label (omit for workspace label)
    #[arg(short = 't', long, value_name = "team")]
    pub team: Option<String>,
    /// Interactive mode (default if no flags provided)
    #[arg(short = 'i', long)]
    pub interactive: bool,
}

pub fn run(args: LabelCreateArgs) -> Result<()> {
    let mut name = args.name;
    let mut color = args.color;
    let mut description = args.description;
    let mut team_key = args.team;

    let no_flags_provided = name.is_none();
    let is_interactive = (no_flags_provided || args.interactive) && prompt::is_interactive();

    if is_interactive {
        output::blank();
        output::line("Create a new label");
        output::blank();

        if name.is_none() {
            name = Some(prompt_text_required("Label name:")?);
        }

        if color.is_none() {
            let mut labels: Vec<String> = DEFAULT_COLORS
                .iter()
                .map(|(color_name, value)| format!("{color_name} ({value})"))
                .collect();
            labels.push("Custom color".to_string());

            let selected = prompt_select("Color:", &labels, 6)?;
            color = if selected == DEFAULT_COLORS.len() {
                Some(prompt_hex("Enter hex color (e.g., #FF5733):")?)
            } else {
                Some(DEFAULT_COLORS[selected].1.to_string())
            };
        }

        if description.is_none() {
            let value = prompt_text("Description (optional):", None)?;
            description = if value.is_empty() { None } else { Some(value) };
        }

        if team_key.is_none() {
            let all_teams = linear::get_all_teams()?;
            let mut values: Vec<String> = vec!["__workspace__".to_string()];
            let mut labels: Vec<String> = vec!["Workspace (shared by all teams)".to_string()];
            for team in &all_teams {
                labels.push(format!("{} ({})", team.name, team.key));
                values.push(team.key.clone());
            }

            let default_index = linear::get_team_key()?
                .and_then(|default_team| values.iter().position(|value| *value == default_team))
                .unwrap_or(0);

            let selected = prompt_select("Team:", &labels, default_index)?;
            let selected_value = &values[selected];
            team_key = if selected_value == "__workspace__" {
                None
            } else {
                Some(selected_value.clone())
            };
        }
    }

    // Validate required fields.
    let name = name.ok_or_else(|| {
        CliError::validation("Label name is required")
            .suggestion("Use --name or -n flag to specify a label name.")
    })?;

    if let Some(provided) = &color {
        if !is_valid_hex(provided) {
            return Err(CliError::validation(
                "Color must be a valid hex code (e.g., #EB5757)",
            ));
        }
    }
    let color = color.unwrap_or_else(|| DEFAULT_COLOR.to_string());

    let team_id = match &team_key {
        Some(key) => Some(linear::resolve_team(key)?.id),
        None => None,
    };

    let mut input = Map::new();
    input.insert("name".to_string(), json!(name));
    input.insert("color".to_string(), json!(color));
    if let Some(description) = description.filter(|value| !value.is_empty()) {
        input.insert("description".to_string(), json!(description));
    }
    if let Some(team_id) = team_id {
        input.insert("teamId".to_string(), json!(team_id));
    }

    let client = graphql::client()?;
    let result = client.request(
        CREATE_ISSUE_LABEL_MUTATION,
        json!({ "input": Value::Object(input) }),
    )?;

    let created = result
        .get("issueLabelCreate")
        .cloned()
        .unwrap_or(Value::Null);
    if !created
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CliError::cli("Failed to create label"));
    }

    let label = created.get("issueLabel").cloned().unwrap_or(Value::Null);
    let label_name = label.get("name").and_then(Value::as_str).unwrap_or("");
    let label_color = label.get("color").and_then(Value::as_str).unwrap_or("");

    output::line(&format!("✓ Created label: {label_name}"));
    output::line(&format!("  Color: {label_color}"));
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
        .filter(|team_name| !team_name.is_empty())
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

/// Read one line from the terminal. Only called after
/// [`crate::prompt::is_interactive`] has confirmed stdin is a terminal.
fn prompt_text(message: &str, default: Option<&str>) -> Result<String> {
    eprint!("{message}");
    if let Some(default) = default {
        eprint!(" [{default}]");
    }
    eprint!(" ");
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
    if read == 0 {
        return Ok(default.unwrap_or("").to_string());
    }
    let value = line.trim().to_string();
    if value.is_empty() {
        Ok(default.unwrap_or("").to_string())
    } else {
        Ok(value)
    }
}

fn prompt_text_required(message: &str) -> Result<String> {
    loop {
        let value = prompt_text(message, None)?;
        if !value.is_empty() {
            return Ok(value);
        }
    }
}

fn prompt_hex(message: &str) -> Result<String> {
    loop {
        let value = prompt_text(message, None)?;
        if is_valid_hex(&value) {
            return Ok(value);
        }
        eprintln!("Please enter a valid hex color (e.g., #FF5733)");
    }
}

/// Numbered selection with a default. Only called in interactive mode.
fn prompt_select(message: &str, labels: &[String], default_index: usize) -> Result<usize> {
    eprintln!("{message}");
    for (index, label) in labels.iter().enumerate() {
        let marker = if index == default_index {
            " (default)"
        } else {
            ""
        };
        eprintln!("  {}. {label}{marker}", index + 1);
    }

    let stdin = std::io::stdin();
    loop {
        eprint!(
            "Enter a number (1-{}) [{}]: ",
            labels.len(),
            default_index + 1
        );
        let _ = std::io::stderr().flush();

        let mut line = String::new();
        let read = stdin
            .lock()
            .read_line(&mut line)
            .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
        if read == 0 {
            return Ok(default_index);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(default_index);
        }
        if let Ok(choice) = trimmed.parse::<usize>() {
            if choice >= 1 && choice <= labels.len() {
                return Ok(choice - 1);
            }
        }
        eprintln!("Please enter a number between 1 and {}.", labels.len());
    }
}
