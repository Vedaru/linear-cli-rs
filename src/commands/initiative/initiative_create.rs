//! `linear initiative create` — port of
//! `src/commands/initiative/initiative-create.ts`.
//!
//! Two modes: an interactive wizard (only reachable when prompting is possible)
//! and a flag-driven path. Headless runs take the flag path and fail validation
//! naming the flag to pass instead of blocking on a prompt.
//!
//! Interactive mode opens when no `--name` was given (or `--interactive` was
//! passed) and stdout is a real terminal, matching upstream's
//! `Deno.stdout.isTerminal()` guard. There is no `--json` flag upstream, so
//! this module prints the human summary only.
//!
//! Error context (`Failed to create initiative`) is supplied by the group
//! `mod.rs`, mirroring upstream's single `handleError` wrapper.

use std::io::{BufRead, IsTerminal, Write};

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output, prompt};

const CREATE_INITIATIVE_MUTATION: &str = r#"
mutation CreateInitiative($input: InitiativeCreateInput!) {
  initiativeCreate(input: $input) {
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

/// Initiative statuses (enum values: Planned, Active, Completed).
const INITIATIVE_STATUSES: [&str; 3] = ["Planned", "Active", "Completed"];

/// Common initiative colors from Linear's palette.
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

#[derive(Args, Debug)]
pub struct InitiativeCreateArgs {
    /// Initiative name (required)
    #[arg(short = 'n', long, value_name = "name")]
    pub name: Option<String>,
    /// Initiative description
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// Status: planned, active, completed (default: planned)
    #[arg(short = 's', long, value_name = "status")]
    pub status: Option<String>,
    /// Owner (username, email, or @me for yourself)
    #[arg(short = 'o', long, value_name = "owner")]
    pub owner: Option<String>,
    /// Target completion date (YYYY-MM-DD)
    #[arg(long = "target-date", value_name = "targetDate")]
    pub target_date: Option<String>,
    /// Color hex code (e.g., #5E6AD2)
    #[arg(short = 'c', long, value_name = "color")]
    pub color: Option<String>,
    /// Icon name
    #[arg(long, value_name = "icon")]
    pub icon: Option<String>,
    /// Interactive mode (default if no flags provided)
    #[arg(short = 'i', long)]
    pub interactive: bool,
}

pub fn run(args: InitiativeCreateArgs) -> Result<()> {
    // Upstream builds the client before prompting, so a missing credential
    // fails before any wizard opens.
    let client = graphql::client()?;

    let mut name = args.name.clone();
    let mut description = args.description.clone();
    let mut status = args.status.clone();
    let mut owner = args.owner.clone();
    let mut target_date = args.target_date.clone();
    let mut color = args.color.clone();
    let icon = args.icon.clone();

    // Interactive mode: no `--name` (or an explicit `--interactive`) on a real
    // terminal. `prompt::is_interactive()` also guarantees a headless run can
    // never block waiting for a prompt.
    let no_flags_provided = name.is_none();
    let is_interactive = (no_flags_provided || args.interactive)
        && std::io::stdout().is_terminal()
        && prompt::is_interactive();

    if is_interactive {
        output::blank();
        output::line("Create a new initiative");
        output::blank();

        // Name (required).
        if name.is_none() {
            name = Some(prompt_text_required("Initiative name")?);
        }

        // Description (optional).
        if description.is_none() {
            let value = prompt_text("Description (optional)", "")?;
            description = if value.is_empty() { None } else { Some(value) };
        }

        // Status selection.
        if status.is_none() {
            let options: Vec<(String, String)> = INITIATIVE_STATUSES
                .iter()
                .map(|value| (value.to_string(), value.to_string()))
                .collect();
            let default_index = options
                .iter()
                .position(|(value, _)| value.to_lowercase() == "planned")
                .unwrap_or(0);
            status = Some(prompt_select("Status", &options, default_index)?);
        }

        // Owner (optional).
        if owner.is_none() {
            let value = prompt_text("Owner (username, email, or @me - press Enter to skip)", "")?;
            owner = if value.is_empty() { None } else { Some(value) };
        }

        // Target date (optional).
        if target_date.is_none() {
            let value = prompt_text("Target date (YYYY-MM-DD - press Enter to skip)", "")?;
            target_date = if value.is_empty() { None } else { Some(value) };
        }

        // Color selection (optional).
        if color.is_none() {
            color = prompt_color()?;
        }
    }

    // Validate required fields.
    let Some(name) = name else {
        return Err(CliError::validation(
            "Initiative name is required. Use --name or -n flag.",
        ));
    };

    // Validate the status if provided; a lowercase answer is canonicalised to
    // the enum value the API expects.
    let status = match non_empty(status.as_deref()) {
        Some(status) => {
            let wanted = status.to_lowercase();
            match INITIATIVE_STATUSES
                .iter()
                .find(|entry| entry.to_lowercase() == wanted)
            {
                Some(entry) => Some((*entry).to_string()),
                None => {
                    return Err(CliError::validation(format!(
                        "Invalid status: {status}. Valid values: {}",
                        INITIATIVE_STATUSES
                            .iter()
                            .map(|entry| entry.to_lowercase())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
            }
        }
        None => None,
    };

    // Validate the color format if provided.
    if let Some(color) = non_empty(color.as_deref()) {
        if !is_hex_color(color) {
            return Err(CliError::validation(
                "Color must be a valid hex code (e.g., #5E6AD2)",
            ));
        }
    }

    // Validate the target date format if provided.
    if let Some(target_date) = non_empty(target_date.as_deref()) {
        if !is_iso_date(target_date) {
            return Err(CliError::validation(
                "Target date must be in YYYY-MM-DD format",
            ));
        }
    }

    // Build the input. Upstream spreads each option only when it is truthy, so
    // a blank value is dropped rather than sent.
    let mut owner_id: Option<String> = None;
    if let Some(owner) = non_empty(owner.as_deref()) {
        let Some(resolved) = linear::lookup_user_id(owner)? else {
            return Err(CliError::not_found("Owner", owner));
        };
        owner_id = Some(resolved);
    }

    let mut input = Map::new();
    input.insert("name".to_string(), json!(name));
    if let Some(description) = non_empty(description.as_deref()) {
        input.insert("description".to_string(), json!(description));
    }
    if let Some(status) = &status {
        input.insert("status".to_string(), json!(status));
    }
    if let Some(owner_id) = &owner_id {
        input.insert("ownerId".to_string(), json!(owner_id));
    }
    if let Some(target_date) = non_empty(target_date.as_deref()) {
        input.insert("targetDate".to_string(), json!(target_date));
    }
    if let Some(color) = non_empty(color.as_deref()) {
        input.insert("color".to_string(), json!(color));
    }
    if let Some(icon) = non_empty(icon.as_deref()) {
        input.insert("icon".to_string(), json!(icon));
    }

    let result = client.request(
        CREATE_INITIATIVE_MUTATION,
        json!({ "input": Value::Object(input) }),
    )?;

    let created = result
        .get("initiativeCreate")
        .and_then(|value| value.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !created {
        return Err(CliError::cli("Failed to create initiative"));
    }

    let initiative = result
        .get("initiativeCreate")
        .and_then(|value| value.get("initiative"))
        .filter(|value| !value.is_null())
        .ok_or_else(|| CliError::cli("Failed to create initiative"))?;

    let created_name = initiative.get("name").and_then(Value::as_str).unwrap_or("");
    output::line(&format!("✓ Created initiative: {created_name}"));
    let slug = initiative
        .get("slugId")
        .and_then(Value::as_str)
        .unwrap_or("");
    output::line(&format!("  Slug: {slug}"));
    if let Some(url) = initiative.get("url").and_then(Value::as_str) {
        if !url.is_empty() {
            output::line(&format!("  URL: {url}"));
        }
    }

    Ok(())
}

/// Upstream spreads optional strings only when they are truthy, so a blank
/// value never reaches the API.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// `#RRGGBB`, mirroring upstream's `/^#[0-9A-Fa-f]{6}$/`.
fn is_hex_color(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(|byte| byte.is_ascii_hexdigit())
}

/// `YYYY-MM-DD`, mirroring upstream's `/^\d{4}-\d{2}-\d{2}$/`.
fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
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

/// `Input.prompt`: the answer, or `default` when the line is blank or EOF.
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

/// `Input.prompt` with `minLength: 1`: re-ask until something is entered.
fn prompt_text_required(message: &str) -> Result<String> {
    loop {
        eprint!("{message}: ");
        let _ = std::io::stderr().flush();
        match read_line()? {
            None => return Err(CliError::cli("Failed to read a name from the terminal")),
            Some(value) if !value.is_empty() => return Ok(value),
            Some(_) => eprintln!("Please enter a value."),
        }
    }
}

/// The color select: the palette, "skip", and a custom-hex escape hatch.
fn prompt_color() -> Result<Option<String>> {
    let mut options: Vec<(String, String)> =
        vec![("__skip__".to_string(), "Skip (use default)".to_string())];
    options.extend(
        DEFAULT_COLORS
            .iter()
            .map(|(name, value)| (value.to_string(), format!("{name} ({value})"))),
    );
    options.push(("__custom__".to_string(), "Custom color".to_string()));

    let selected = prompt_select("Color (optional)", &options, 0)?;
    if selected == "__custom__" {
        return Ok(Some(prompt_hex_color("Enter hex color (e.g., #FF5733)")?));
    }
    if selected == "__skip__" {
        return Ok(None);
    }
    Ok(Some(selected))
}

/// `Input.prompt` with the hex-color `validate` callback: re-ask until the
/// value matches `#RRGGBB`.
fn prompt_hex_color(message: &str) -> Result<String> {
    loop {
        eprint!("{message}: ");
        let _ = std::io::stderr().flush();
        match read_line()? {
            None => return Err(CliError::cli("Failed to read a color from the terminal")),
            Some(value) if is_hex_color(&value) => return Ok(value),
            Some(_) => eprintln!("Please enter a valid hex color (e.g., #FF5733)"),
        }
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
