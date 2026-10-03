//! `linear view update` — rename, redescribe, refilter or share a view.
//!
//! Only the fields given are sent: the API's update input is partial, so an omitted field is
//! "leave it" rather than "clear it". An invocation with nothing to change is refused instead of
//! sending an empty input, which would report success for a no-op.

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{linear, output};

use super::view_create::read_optional_filter;

#[derive(Args, Debug)]
pub struct ViewUpdateArgs {
    /// View name or ID
    pub name_or_id: String,
    /// New name
    #[arg(short = 'n', long, value_name = "name")]
    pub name: Option<String>,
    /// New description (an empty string clears it)
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// New filter, as JSON in the API's `issues(filter:)` shape
    #[arg(long = "filter", value_name = "JSON")]
    pub filter: Option<String>,
    /// Read the new filter from a file (`-` for stdin)
    #[arg(long = "filter-file", value_name = "PATH")]
    pub filter_file: Option<String>,
    /// Share the view with the team
    #[arg(long, value_name = "true|false")]
    pub shared: Option<bool>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ViewUpdateArgs) -> Result<()> {
    let filter = read_optional_filter(args.filter.as_deref(), args.filter_file.as_deref())?;

    if args.name.is_none()
        && args.description.is_none()
        && filter.is_none()
        && args.shared.is_none()
    {
        return Err(CliError::validation("Nothing to update").suggestion(
            "Pass --name, --description, --filter/--filter-file or --shared; the fields you leave out are left alone.",
        ));
    }

    let view = linear::resolve_view(&args.name_or_id)?;
    let id = view
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::cli("The view has no id"))?
        .to_string();

    // `description` is sent even when empty: the API treats it as "clear it", which is a
    // different request from omitting it, and the flag was given explicitly.
    let mut input = Map::new();
    if let Some(name) = &args.name {
        input.insert("name".to_string(), json!(name));
    }
    if let Some(description) = &args.description {
        input.insert("description".to_string(), json!(description));
    }
    if let Some(filter) = filter {
        input.insert("filterData".to_string(), filter);
    }
    if let Some(shared) = args.shared {
        input.insert("shared".to_string(), json!(shared));
    }

    let updated = linear::update_view(&id, Value::Object(input))?;

    if args.json {
        output::print_json(&updated);
        return Ok(());
    }

    output::line(&format!(
        "✓ Updated view: {}",
        updated
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&args.name_or_id)
    ));
    let changed: Vec<&str> = [
        args.name.as_ref().map(|_| "name"),
        args.description.as_ref().map(|_| "description"),
        args.shared.map(|_| "sharing"),
    ]
    .into_iter()
    .flatten()
    .chain(if args.filter.is_some() || args.filter_file.is_some() {
        Some("filter")
    } else {
        None
    })
    .collect();
    output::line(&format!("  Changed: {}", changed.join(", ")));
    Ok(())
}
