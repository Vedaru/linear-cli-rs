//! `linear project-update create` — port of
//! `src/commands/project-update/project-update-create.ts`.
//!
//! Two modes: an interactive wizard (only reachable when prompting is
//! possible) and a flag-driven path. Headless runs take the flag path and
//! fail validation naming the flag to pass instead of blocking on a prompt.
//!
//! Error context (`Failed to create project update`) is supplied by the group
//! `mod.rs`, mirroring upstream's single `handleError` wrapper.

use std::io::{BufRead, IsTerminal, Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{editor, graphql, linear, output, prompt};

const CREATE_PROJECT_UPDATE_MUTATION: &str = r#"
mutation CreateProjectUpdate($input: ProjectUpdateCreateInput!) {
  projectUpdateCreate(input: $input) {
    success
    projectUpdate {
      id
      body
      health
      url
      createdAt
      project {
        name
        slugId
      }
    }
  }
}
"#;

const VALID_HEALTH_VALUES: [&str; 3] = ["onTrack", "atRisk", "offTrack"];

#[derive(Args, Debug)]
pub struct ProjectUpdateCreateArgs {
    /// Project ID, slug ID, URL, or exact name
    pub project_id: String,
    /// Update content (inline)
    #[arg(long, value_name = "body")]
    pub body: Option<String>,
    /// Read content from file
    #[arg(long = "body-file", value_name = "path")]
    pub body_file: Option<String>,
    /// Project health status (onTrack, atRisk, offTrack)
    #[arg(long, value_name = "health")]
    pub health: Option<String>,
    /// Interactive mode with prompts
    #[arg(short = 'i', long)]
    pub interactive: bool,
}

pub fn run(args: ProjectUpdateCreateArgs) -> Result<()> {
    // Resolve the project before prompting or reading content, so a bad
    // reference fails with upstream's `Project not found: ...`.
    let resolved_project_id = linear::resolve_project_id(&args.project_id)?;

    // Determine whether to use interactive mode. The port gates on
    // `prompt::is_interactive()` (stdin + stderr), which also guarantees a
    // headless run can never block waiting for a prompt.
    let mut use_interactive = args.interactive && prompt::is_interactive();

    let no_flags_provided =
        args.body.is_none() && args.body_file.is_none() && args.health.is_none();
    if no_flags_provided && prompt::is_interactive() {
        use_interactive = true;
    }

    if use_interactive {
        let (body, health) = prompt_interactive_create()?;
        let mut input = Map::new();
        input.insert("projectId".to_string(), json!(resolved_project_id));
        if let Some(body) = body {
            input.insert("body".to_string(), json!(body));
        }
        if let Some(health) = health {
            input.insert("health".to_string(), json!(health));
        }
        return create_project_update(Value::Object(input));
    }

    // Non-interactive mode: resolve content from the various sources.
    let mut final_body: Option<String> = None;

    if let Some(body) = &args.body {
        final_body = Some(body.clone());
    } else if let Some(body_file) = &args.body_file {
        match std::fs::read_to_string(body_file) {
            Ok(content) => final_body = Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CliError::not_found("File", body_file));
            }
            Err(error) => {
                return Err(CliError::cli(format!("Failed to read body file: {error}")));
            }
        }
    } else if !std::io::stdin().is_terminal() {
        // Try reading from stdin if piped.
        if let Some(stdin_content) = read_content_from_stdin() {
            final_body = Some(stdin_content);
        }
    } else if std::io::stdout().is_terminal() {
        // No content provided, open editor.
        output::line("Opening editor for update content...");
        final_body = editor::open_editor();
        if final_body.is_none() {
            output::line("No content entered.");
        }
    }

    // Validate health against the allowed set.
    let validated_health = match &args.health {
        Some(health) => {
            if !VALID_HEALTH_VALUES.contains(&health.as_str()) {
                return Err(
                    CliError::validation(format!("Invalid health value: {health}")).suggestion(
                        format!("Must be one of: {}", VALID_HEALTH_VALUES.join(", ")),
                    ),
                );
            }
            Some(health.clone())
        }
        None => None,
    };

    let mut input = Map::new();
    input.insert("projectId".to_string(), json!(resolved_project_id));
    if let Some(body) = final_body {
        input.insert("body".to_string(), json!(body));
    }
    if let Some(health) = validated_health {
        input.insert("health".to_string(), json!(health));
    }

    create_project_update(Value::Object(input))
}

fn create_project_update(input: Value) -> Result<()> {
    let client = graphql::client()?;
    let result = client.request(CREATE_PROJECT_UPDATE_MUTATION, json!({ "input": input }))?;

    let created = result
        .get("projectUpdateCreate")
        .and_then(|value| value.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !created {
        return Err(CliError::cli("Failed to create project update"));
    }

    let project_update = result
        .get("projectUpdateCreate")
        .and_then(|value| value.get("projectUpdate"))
        .filter(|value| !value.is_null())
        .ok_or_else(|| CliError::cli("Project update creation failed - no update returned"))?;

    let project_name = project_update
        .pointer("/project/name")
        .and_then(Value::as_str)
        .unwrap_or("Unknown project");
    output::line(&format!("Created status update for: {project_name}"));
    if let Some(health) = project_update.get("health").and_then(Value::as_str) {
        if !health.is_empty() {
            output::line(&format!("Health: {health}"));
        }
    }
    let url = project_update
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("");
    output::line(url);
    Ok(())
}

/// Read content from stdin if available, mirroring upstream's 100ms race.
/// Upstream joins the parsed ids back with newlines; this reader trims each
/// line and drops blanks, so both agree on the resulting content.
fn read_content_from_stdin() -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = String::new();
        if std::io::stdin().read_to_string(&mut buffer).is_err() {
            let _ = sender.send(None);
            return;
        }
        let content = buffer
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = sender.send(if content.is_empty() {
            None
        } else {
            Some(content)
        });
    });
    receiver
        .recv_timeout(Duration::from_millis(100))
        .ok()
        .flatten()
}

fn prompt_interactive_create() -> Result<(Option<String>, Option<String>)> {
    let health_labels = ["On Track", "At Risk", "Off Track", "No change"]
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    let health_index = prompt_select("Project health status", &health_labels, 3)?;
    let health = match health_index {
        0 => Some("onTrack".to_string()),
        1 => Some("atRisk".to_string()),
        2 => Some("offTrack".to_string()),
        _ => None,
    };

    let editor_name = editor::get_editor();
    let editor_display_name = editor_name
        .as_deref()
        .and_then(|value| value.rsplit('/').next())
        .map(str::to_string);

    let mut method_labels = vec!["Skip (no content)".to_string(), "Enter inline".to_string()];
    if let Some(name) = &editor_display_name {
        method_labels.push(format!("Open {name}"));
    }
    method_labels.push("Read from file".to_string());

    let method = prompt_select(
        "How would you like to enter the update content?",
        &method_labels,
        0,
    )?;
    let selected = method_labels[method].as_str();

    let mut body: Option<String> = None;
    if selected == "Enter inline" {
        let inline = prompt_text_with_default("Update content (markdown)", "")?;
        let trimmed = inline.trim();
        if !trimmed.is_empty() {
            body = Some(trimmed.to_string());
        }
    } else if selected.starts_with("Open ") {
        if let Some(name) = &editor_display_name {
            output::line(&format!("Opening {name}..."));
            body = editor::open_editor();
            if let Some(text) = &body {
                output::line(&format!("Content entered ({} characters)", text.len()));
            }
        }
    } else if selected == "Read from file" {
        let file_path = prompt_text("File path")?;
        match std::fs::read_to_string(&file_path) {
            Ok(content) => body = Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CliError::not_found("File", &file_path));
            }
            Err(error) => {
                return Err(CliError::cli(format!("Failed to read file: {error}")));
            }
        }
    }

    Ok((body, health))
}

// ---------------------------------------------------------------------------
// Local prompt helpers (mirroring @cliffy/prompt's Input/Select).
// ---------------------------------------------------------------------------

fn prompt_text(message: &str) -> Result<String> {
    eprint!("{message}: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Prompt for text with an editable default shown in brackets; a blank line
/// accepts the default.
fn prompt_text_with_default(message: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        eprint!("{message}: ");
    } else {
        eprint!("{message} [{default}]: ");
    }
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
    if read == 0 {
        return Ok(default.to_string());
    }
    let value = line.trim_end_matches(['\r', '\n']).to_string();
    if value.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(value)
    }
}

/// Numbered single-choice prompt with a default index (0-based).
fn prompt_select(message: &str, labels: &[String], default: usize) -> Result<usize> {
    eprintln!("{message}");
    for (index, label) in labels.iter().enumerate() {
        eprintln!("  {}. {label}", index + 1);
    }
    let stdin = std::io::stdin();
    loop {
        eprint!("Enter a number (1-{}): ", labels.len());
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let read = stdin
            .lock()
            .read_line(&mut line)
            .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
        if read == 0 {
            return Ok(default);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(default);
        }
        if let Ok(choice) = trimmed.parse::<usize>() {
            if choice >= 1 && choice <= labels.len() {
                return Ok(choice - 1);
            }
        }
        eprintln!("Please enter a number between 1 and {}.", labels.len());
    }
}
