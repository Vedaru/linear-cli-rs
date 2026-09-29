//! `linear document create` — port of `src/commands/document/document-create.ts`.
//!
//! Two modes: an interactive wizard (only reachable when prompting is
//! possible) and a flag-driven path. The wizard is gated on
//! [`crate::prompt::is_interactive`]; headless runs take the flag path and
//! fail validation naming the flag they should pass instead.
//!
//! Error context mirrors upstream: the whole action sits in `handleError(error,
//! "Failed to create document")`, and the group `mod.rs` supplies that prefix.

use std::io::{BufRead, IsTerminal, Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use clap::Args;
use serde_json::{json, Map, Value};

use crate::commands::document::attachment_target::{
    parse_document_target_options, resolve_document_target, to_document_target_input,
    DocumentTarget, DocumentTargetOptions, DocumentTargetSelector, TargetRequirement,
};
use crate::errors::{CliError, Result};
use crate::{editor, graphql, linear, output, prompt};

const CREATE_DOCUMENT_MUTATION: &str = r#"
mutation CreateDocument($input: DocumentCreateInput!) {
  documentCreate(input: $input) {
    success
    document {
      id
      slugId
      title
      url
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct DocumentCreateArgs {
    /// Document title (required)
    #[arg(short = 't', long, value_name = "title")]
    pub title: Option<String>,
    /// Markdown content (inline)
    #[arg(short = 'c', long, value_name = "content")]
    pub content: Option<String>,
    /// Read content from file
    #[arg(short = 'f', long = "content-file", value_name = "path")]
    pub content_file: Option<String>,
    /// Attach to project (UUID, slug ID, or name)
    #[arg(long, value_name = "project")]
    pub project: Option<String>,
    /// Attach to issue (identifier like TC-123)
    #[arg(long, value_name = "issue")]
    pub issue: Option<String>,
    /// Attach to initiative (UUID, slug ID, or name)
    #[arg(long, value_name = "initiative")]
    pub initiative: Option<String>,
    /// Attach to team (key, name, or ID); with --cycle, scopes the cycle lookup instead
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Attach to cycle: name, number, 'active'/'now', 'next', 'previous', or a relative offset like +1 (team from --team or config)
    #[arg(long, value_name = "cycle")]
    pub cycle: Option<String>,
    /// Attach to release (UUID, name, or version)
    #[arg(long, value_name = "release")]
    pub release: Option<String>,
    /// Document icon (emoji)
    #[arg(long, value_name = "icon")]
    pub icon: Option<String>,
    /// Interactive mode with prompts
    #[arg(short = 'i', long)]
    pub interactive: bool,
}

pub fn run(args: DocumentCreateArgs) -> Result<()> {
    let target_options = DocumentTargetOptions {
        project: args.project.clone(),
        issue: args.issue.clone(),
        initiative: args.initiative.clone(),
        team: args.team.clone(),
        cycle: args.cycle.clone(),
        release: args.release.clone(),
    };
    let any_target_flag = args.project.is_some()
        || args.issue.is_some()
        || args.initiative.is_some()
        || args.team.is_some()
        || args.cycle.is_some()
        || args.release.is_some();

    // Determine whether to use interactive mode.
    let mut use_interactive = args.interactive && prompt::is_interactive();

    let no_flags_provided = args.title.is_none()
        && args.content.is_none()
        && args.content_file.is_none()
        && !any_target_flag
        && args.icon.is_none();
    if no_flags_provided && prompt::is_interactive() {
        use_interactive = true;
    }

    if use_interactive {
        // Interactive mode picks its target via prompts; mixing in target flags
        // would silently lose one of the two, so reject up front.
        if any_target_flag {
            return Err(CliError::validation(
                "Attachment target flags cannot be combined with interactive mode",
            )
            .suggestion(
                "Drop the target flags to choose the attachment interactively, or drop -i/--interactive to use the flags.",
            ));
        }

        let result = prompt_interactive_create()?;
        let Some(title) = result.title.clone().filter(|value| !value.is_empty()) else {
            return Err(CliError::validation("Title is required"));
        };

        let mut input = Map::new();
        input.insert("title".to_string(), json!(title));
        merge_target(&mut input, &result.target);
        if let Some(content) = &result.content {
            input.insert("content".to_string(), json!(content));
        }
        if let Some(icon) = &result.icon {
            input.insert("icon".to_string(), json!(icon));
        }
        return create_document(Value::Object(input));
    }

    // Non-interactive mode requires a title.
    let Some(title) = args.title.clone().filter(|value| !value.is_empty()) else {
        return Err(CliError::validation("Title is required")
            .suggestion("Use --title or run with -i for interactive mode."));
    };

    // Validate target cardinality before any content work so a bad flag
    // combination fails before an editor is opened or stdin is read.
    let selector = parse_document_target_options(&target_options, TargetRequirement::ExactlyOne)?;
    let selector = selector.expect("exactly-one validated by parse_document_target_options");

    // Resolve content from various sources.
    let mut final_content: Option<String> = None;

    if let Some(content) = &args.content {
        // Content provided inline via --content.
        final_content = Some(content.clone());
    } else if let Some(content_file) = &args.content_file {
        // Content from file via --content-file.
        match std::fs::read_to_string(content_file) {
            Ok(content) => final_content = Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CliError::not_found("File", content_file));
            }
            Err(error) => {
                return Err(CliError::cli(format!(
                    "Failed to read content file: {error}"
                )));
            }
        }
    } else if !std::io::stdin().is_terminal() {
        // Try reading from stdin if piped.
        if let Some(stdin_content) = read_content_from_stdin() {
            final_content = Some(stdin_content);
        }
    } else if std::io::stdout().is_terminal() {
        // No content provided, open editor.
        output::line("Opening editor for document content...");
        final_content = editor::open_editor();
        if final_content.is_none() {
            output::line("No content entered. Creating document without content.");
        }
    }

    let target = resolve_document_target(&selector)?;

    let mut input = Map::new();
    input.insert("title".to_string(), json!(title));
    merge_target(&mut input, &target);
    if let Some(content) = &final_content {
        input.insert("content".to_string(), json!(content));
    }
    if let Some(icon) = &args.icon {
        input.insert("icon".to_string(), json!(icon));
    }

    create_document(Value::Object(input))
}

fn merge_target(input: &mut Map<String, Value>, target: &DocumentTarget) {
    if let Value::Object(fields) = to_document_target_input(target) {
        for (key, value) in fields {
            input.insert(key, value);
        }
    }
}

fn create_document(input: Value) -> Result<()> {
    let client = graphql::client()?;
    let result = client.request(CREATE_DOCUMENT_MUTATION, json!({ "input": input }))?;

    let created = result
        .get("documentCreate")
        .and_then(|value| value.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !created {
        return Err(CliError::cli("Document creation failed"));
    }

    let document = result
        .get("documentCreate")
        .and_then(|value| value.get("document"))
        .filter(|value| !value.is_null())
        .ok_or_else(|| CliError::cli("Document creation failed - no document returned"))?;

    let title = document.get("title").and_then(Value::as_str).unwrap_or("");
    let url = document.get("url").and_then(Value::as_str).unwrap_or("");
    output::line(&format!("✓ Created document: {title}"));
    output::line(url);
    Ok(())
}

/// Read content from stdin if available, mirroring upstream's 100ms race.
/// Upstream joins the parsed "ids" back with newlines; the reader trims each
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
    receiver.recv_timeout(Duration::from_millis(100)).ok().flatten()
}

struct InteractiveResult {
    title: Option<String>,
    content: Option<String>,
    icon: Option<String>,
    target: DocumentTarget,
}

fn prompt_interactive_create() -> Result<InteractiveResult> {
    let title = prompt_text("Document title")?;

    let editor_name = editor::get_editor();
    let editor_display_name = editor_name
        .as_deref()
        .and_then(|value| value.rsplit('/').next())
        .map(str::to_string);

    let mut labels = vec![
        "Skip (no content)".to_string(),
        "Enter inline".to_string(),
    ];
    if let Some(name) = &editor_display_name {
        labels.push(format!("Open {name}"));
    }
    labels.push("Read from file".to_string());

    let selection = prompt_select("How would you like to enter content?", &labels, 0)?;
    let mut content: Option<String> = None;
    match labels[selection].as_str() {
        "Skip (no content)" => {}
        "Enter inline" => {
            let inline = prompt_text_with_default("Content (markdown)", "")?;
            let trimmed = inline.trim();
            if !trimmed.is_empty() {
                content = Some(trimmed.to_string());
            }
        }
        "Read from file" => {
            let file_path = prompt_text("File path")?;
            match std::fs::read_to_string(&file_path) {
                Ok(text) => content = Some(text),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(CliError::not_found("File", &file_path));
                }
                Err(error) => {
                    return Err(CliError::cli(format!("Failed to read file: {error}")));
                }
            }
        }
        _ => {
            // The only remaining label is "Open {editor}".
            if let Some(name) = &editor_display_name {
                output::line(&format!("Opening {name}..."));
                content = editor::open_editor();
                if let Some(text) = &content {
                    output::line(&format!("Content entered ({} characters)", text.len()));
                }
            }
        }
    }

    let icon = prompt_text_with_default("Icon (emoji, leave blank for none)", "")?;
    let icon = {
        let trimmed = icon.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    let target = prompt_for_target()?;

    Ok(InteractiveResult {
        title: Some(title),
        content,
        icon,
        target,
    })
}

fn prompt_for_target() -> Result<DocumentTarget> {
    let labels = [
        "Project",
        "Issue",
        "Team",
        "Initiative",
        "Cycle",
        "Release",
    ]
    .iter()
    .map(|value| value.to_string())
    .collect::<Vec<_>>();

    let selection = prompt_select("Attach document to", &labels, 0)?;

    let selector = match selection {
        0 => {
            let project = prompt_text("Project (UUID, slug ID, or name)")?;
            DocumentTargetSelector::Project(project)
        }
        1 => {
            let issue = prompt_text("Issue identifier (e.g., TC-123)")?;
            DocumentTargetSelector::Issue(issue)
        }
        2 => {
            let default = linear::get_team_key().unwrap_or_default();
            let team = prompt_text_with_default("Team key (e.g., ENG)", &default)?;
            DocumentTargetSelector::Team(team)
        }
        3 => {
            let initiative = prompt_text("Initiative (UUID, slug ID, or name)")?;
            DocumentTargetSelector::Initiative(initiative)
        }
        4 => {
            let default = linear::get_team_key().unwrap_or_default();
            let team = prompt_text_with_default("Team key for the cycle (e.g., ENG)", &default)?;
            let cycle = prompt_text("Cycle (name, number, 'active', 'next', or 'previous')")?;
            DocumentTargetSelector::Cycle {
                cycle,
                team: Some(team),
            }
        }
        5 => {
            let release = prompt_text("Release (UUID, name, or version)")?;
            DocumentTargetSelector::Release(release)
        }
        other => {
            return Err(CliError::validation(format!(
                "Unknown attachment target: {}",
                labels.get(other).map(String::as_str).unwrap_or("")
            )));
        }
    };

    resolve_document_target(&selector)
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
