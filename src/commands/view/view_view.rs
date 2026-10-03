//! `linear view view` — one custom view, with the filter it saves.
//!
//! The filter is printed as the JSON the API takes, not paraphrased: it is the one field worth
//! copying into `issue query --view` or back into `view update --filter`.

use clap::Args;
use serde_json::Value;

use crate::errors::Result;
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct ViewViewArgs {
    /// View name or ID
    pub name_or_id: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ViewViewArgs) -> Result<()> {
    let view = linear::resolve_view(&args.name_or_id)?;

    if args.json {
        output::print_json(&view);
        return Ok(());
    }

    output::line(&format!("Name:     {}", field(&view, "name")));
    output::line(&format!("ID:       {}", field(&view, "id")));
    if let Some(slug) = optional(&view, "slugId") {
        output::line(&format!("Slug:     {slug}"));
    }
    output::line(&format!("Scope:    {}", scope_of(&view)));
    if let Some(owner) = owner_of(&view) {
        output::line(&format!("Owner:    {owner}"));
    }
    output::line(&format!(
        "Shared:   {}",
        if view.get("shared").and_then(Value::as_bool) == Some(true) {
            "yes"
        } else {
            "no"
        }
    ));
    for (label, key) in [("Created", "createdAt"), ("Updated", "updatedAt")] {
        if let Some(value) = optional(&view, key) {
            output::line(&format!("{label}:  {value}"));
        }
    }
    if let Some(description) = optional(&view, "description") {
        if !description.is_empty() {
            output::line(&format!("About:    {description}"));
        }
    }

    output::blank();
    output::line("Filter:");
    match view.get("filterData") {
        Some(filter) if !filter.is_null() => {
            output::line(&serde_json::to_string_pretty(filter).unwrap_or_default())
        }
        // A view with no filter is a view that selects nothing; saying so beats an empty block.
        _ => output::line("(none - this view saves no filter)"),
    }

    Ok(())
}

fn field(view: &Value, key: &str) -> String {
    view.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn optional(view: &Value, key: &str) -> Option<String> {
    view.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn scope_of(view: &Value) -> String {
    view.get("team")
        .filter(|team| !team.is_null())
        .map(|team| {
            let key = team.get("key").and_then(Value::as_str).unwrap_or("");
            let name = team.get("name").and_then(Value::as_str).unwrap_or("");
            format!("{key} ({name})")
        })
        .unwrap_or_else(|| "Workspace".to_string())
}

fn owner_of(view: &Value) -> Option<String> {
    let owner = view.get("owner").filter(|owner| !owner.is_null())?;
    owner
        .get("displayName")
        .or_else(|| owner.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
}
