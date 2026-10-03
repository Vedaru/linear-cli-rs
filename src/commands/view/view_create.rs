//! `linear view create` — a saved filter, named.
//!
//! The filter comes from `--filter <json>` or `--filter-file <path>` (`-` for stdin) and is sent
//! as the API's `filterData` verbatim: it *is* the `issues(filter:)` document, so translating it
//! here would only be a second place to get the filter language wrong. A view with no filter is
//! refused - it would select nothing, and a saved nothing is worse than an error.

use std::io::Read;

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct ViewCreateArgs {
    /// Name for the view
    #[arg(short = 'n', long, value_name = "name")]
    pub name: Option<String>,
    /// Description of what the view shows
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// Team key, name, or ID the view belongs to (defaults to the configured team)
    #[arg(short = 't', long, value_name = "team")]
    pub team: Option<String>,
    /// The filter the view saves, as JSON in the API's `issues(filter:)` shape
    #[arg(long = "filter", value_name = "JSON")]
    pub filter: Option<String>,
    /// Read the filter from a file (`-` for stdin)
    #[arg(long = "filter-file", value_name = "PATH")]
    pub filter_file: Option<String>,
    /// Share the view with the team
    #[arg(long, value_name = "true|false")]
    pub shared: Option<bool>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ViewCreateArgs) -> Result<()> {
    let name = args.name.ok_or_else(|| {
        CliError::validation("View name is required")
            .suggestion("Use --name or -n to name the view, e.g. --name \"My Sprint\".")
    })?;

    let filter = read_filter(args.filter.as_deref(), args.filter_file.as_deref())?;

    let team = match args.team.as_deref() {
        Some(team) => linear::resolve_team(team)?,
        None => {
            let key = linear::get_team_key()?.ok_or_else(|| {
                CliError::validation("No team configured and no --team provided").suggestion(
                    "Pass --team <key, name, or ID> to say which team the view belongs to.",
                )
            })?;
            linear::resolve_team(&key)?
        }
    };

    let mut input = Map::new();
    input.insert("name".to_string(), json!(name));
    input.insert("teamId".to_string(), json!(team.id));
    input.insert("filterData".to_string(), filter);
    if let Some(description) = args.description.filter(|value| !value.is_empty()) {
        input.insert("description".to_string(), json!(description));
    }
    if let Some(shared) = args.shared {
        input.insert("shared".to_string(), json!(shared));
    }

    let view = linear::create_view(Value::Object(input))?;

    if args.json {
        output::print_json(&view);
        return Ok(());
    }

    output::line(&format!(
        "✓ Created view: {}",
        view.get("name").and_then(Value::as_str).unwrap_or(&name)
    ));
    output::line(&format!("  Scope: {} ({})", team.name, team.key));
    if let Some(id) = view.get("id").and_then(Value::as_str) {
        output::line(&format!("  ID: {id}"));
    }
    output::line(&format!(
        "  Filter: {}",
        view.get("filterData")
            .map(|filter| serde_json::to_string(filter).unwrap_or_default())
            .unwrap_or_default()
    ));
    Ok(())
}

/// The `--filter`/`--filter-file` pair, as JSON, refusing both-or-neither.
pub(super) fn read_filter(filter: Option<&str>, file: Option<&str>) -> Result<Value> {
    let raw = match (filter, file) {
        (Some(_), Some(_)) => {
            return Err(
                CliError::validation("Cannot use both --filter and --filter-file").suggestion(
                    "Pass the filter inline with --filter, or from a file with --filter-file.",
                ),
            )
        }
        (Some(inline), None) => inline.to_string(),
        (None, Some(path)) => read_file(path)?,
        (None, None) => return Err(CliError::validation("A view needs a filter").suggestion(
            "Pass the filter as JSON: --filter '{\"state\": {\"type\": {\"eq\": \"started\"}}}'.",
        )),
    };

    serde_json::from_str(&raw).map_err(|error| {
        CliError::validation(format!("The filter is not valid JSON: {error}")).suggestion(
            "The filter is the API's `issues(filter:)` shape, e.g. '{\"state\": {\"type\": {\"eq\": \"started\"}}}'.",
        )
    })
}

/// The filter for an update, where "not given" is a legitimate answer and "both" is not.
pub(super) fn read_optional_filter(
    filter: Option<&str>,
    file: Option<&str>,
) -> Result<Option<Value>> {
    if filter.is_none() && file.is_none() {
        return Ok(None);
    }
    read_filter(filter, file).map(Some)
}

fn read_file(path: &str) -> Result<String> {
    if path == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|error| CliError::cli(format!("Failed to read stdin: {error}")))?;
        return Ok(buffer);
    }
    std::fs::read_to_string(path)
        .map_err(|error| CliError::cli(format!("Failed to read {path}: {error}")))
}
