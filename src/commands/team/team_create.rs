//! `linear team create` — create a Linear team.

use std::io::{BufRead, Write};

use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::graphql;
use crate::output;
use crate::prompt;

const CREATE_TEAM_MUTATION: &str = r#"
mutation CreateTeam($input: TeamCreateInput!) {
  teamCreate(input: $input) {
    success
    team { id, name, key }
  }
}
"#;

#[derive(clap::Args, Debug)]
pub struct CreateArgs {
    /// Name of the team
    #[arg(short = 'n', long)]
    pub name: Option<String>,
    /// Description of the team
    #[arg(short = 'd', long)]
    pub description: Option<String>,
    /// Team key (if not provided, will be generated from name)
    #[arg(short = 'k', long)]
    pub key: Option<String>,
    /// Make the team private
    #[arg(long)]
    pub private: bool,
    /// Disable interactive prompts
    #[arg(long)]
    pub no_interactive: bool,
    /// Output as JSON (the API's own `teamCreate` payload)
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: CreateArgs) -> Result<()> {
    let interactive = !args.no_interactive && prompt::is_interactive();
    let no_flags_provided =
        args.name.is_none() && args.description.is_none() && args.key.is_none() && !args.private;

    if no_flags_provided && interactive {
        output::line("Creating a new team...\n");

        let name = loop {
            let value = input("Team name:")?;
            if !value.trim().is_empty() {
                break value;
            }
            eprintln!("Team name is required");
        };

        let description = input("Team description (optional):")?;
        let key = group_key(input(
            "Team key (optional, will be generated from name if not provided):",
        )?);
        let description = group_key(description);

        let options = vec!["Public".to_string(), "Private".to_string()];
        let privacy = prompt::select("Team visibility:", &options)?;
        let is_private = privacy == 1;

        output::line(&format!("\nCreating team \"{name}\"..."));

        return create_and_report(
            &name,
            description.as_deref(),
            key.as_deref(),
            is_private,
            false,
        );
    }

    // Fallback to flag-based mode.
    let Some(name) = args.name.as_deref() else {
        return Err(
            CliError::validation("Team name is required when not using interactive mode")
                .suggestion("Use --name or run without any flags for interactive mode."),
        );
    };

    if !args.json {
        output::line(&format!("Creating team \"{name}\""));
    }

    create_and_report(
        name,
        args.description.as_deref(),
        args.key.as_deref(),
        args.private,
        args.json,
    )
}

fn create_and_report(
    name: &str,
    description: Option<&str>,
    key: Option<&str>,
    is_private: bool,
    json: bool,
) -> Result<()> {
    let mut input = Map::new();
    input.insert("name".to_string(), json!(name));
    if let Some(description) = description.filter(|value| !value.is_empty()) {
        input.insert("description".to_string(), json!(description));
    }
    if let Some(key) = key.filter(|value| !value.is_empty()) {
        input.insert("key".to_string(), json!(key));
    }
    if is_private {
        input.insert("private".to_string(), json!(true));
    }

    let client = graphql::client()?;
    let data = client.request(
        CREATE_TEAM_MUTATION,
        json!({ "input": Value::Object(input) }),
    )?;
    let team_create = data
        .get("teamCreate")
        .ok_or_else(|| CliError::cli("Team creation failed"))?;

    if !team_create
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CliError::cli("Team creation failed"));
    }

    if json {
        // The API's own payload, like every other `--json` here: the caller gets the id, the key
        // and the name without a second query.
        output::print_json(&data);
        return Ok(());
    }

    let team = team_create
        .get("team")
        .filter(|team| !team.is_null())
        .ok_or_else(|| CliError::cli("Team creation failed - no team returned"))?;

    let team_key = team.get("key").and_then(Value::as_str).unwrap_or("");
    let team_name = team.get("name").and_then(Value::as_str).unwrap_or("");
    output::line(&format!("✓ Created team {team_key}: {team_name}"));
    Ok(())
}

/// Prompt for a free-text value. Port of cliffy's `Input.prompt`, guarded by
/// [`prompt::is_interactive`] so a headless run never blocks.
fn input(message: &str) -> Result<String> {
    if !prompt::is_interactive() {
        return Err(CliError::cli(format!(
            "Cannot read {message} in a non-interactive environment"
        )));
    }

    eprint!("{message} ");
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
    if read == 0 {
        return Ok(String::new());
    }
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// Upstream treats an empty prompt answer as `undefined`.
fn group_key(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}
