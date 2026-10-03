//! `linear roadmap view` — one roadmap, and the projects on it.
//!
//! The projects are read from the roadmap's own relation (`roadmap.projects`) rather than by
//! listing every project and filtering client-side: the API already knows which projects belong to
//! the roadmap, and asking it is both shorter and the only version that stays right when a project
//! is attached by someone else.

use clap::Args;
use serde_json::Value;

use crate::errors::Result;
use crate::{colors, display, linear, output};

#[derive(Args, Debug)]
pub struct RoadmapViewArgs {
    /// Roadmap name or ID
    pub name_or_id: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: RoadmapViewArgs) -> Result<()> {
    let roadmap = linear::view_roadmap(&args.name_or_id)?;

    if args.json {
        output::print_json(&roadmap);
        return Ok(());
    }

    output::line(&format!("Name:     {}", field(&roadmap, "name")));
    output::line(&format!("ID:       {}", field(&roadmap, "id")));
    if let Some(slug) = optional(&roadmap, "slugId") {
        output::line(&format!("Slug:     {slug}"));
    }
    output::line(&format!("Owner:    {}", person(&roadmap, "owner")));
    output::line(&format!("Creator:  {}", person(&roadmap, "creator")));
    if let Some(color) = optional(&roadmap, "color") {
        output::line(&format!("Color:    {color}"));
    }
    for (label, key) in [("Created", "createdAt"), ("Updated", "updatedAt")] {
        if let Some(value) = optional(&roadmap, key) {
            output::line(&format!("{label}:  {value}"));
        }
    }
    if let Some(archived) = optional(&roadmap, "archivedAt") {
        output::line(&format!("Archived: {archived}"));
    }
    if let Some(description) = optional(&roadmap, "description") {
        if !description.is_empty() {
            output::line(&format!("About:    {description}"));
        }
    }

    let projects = roadmap
        .get("projects")
        .and_then(|projects| projects.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    output::blank();
    if projects.is_empty() {
        output::line("No projects on this roadmap.");
        return Ok(());
    }

    output::line(&colors::underline(&format!(
        "{}PROJECTS",
        display::pad_display("", 0)
    )));
    for project in &projects {
        let id = project.get("id").and_then(Value::as_str).unwrap_or("");
        let name = project.get("name").and_then(Value::as_str).unwrap_or("");
        let state = project.get("state").and_then(Value::as_str).unwrap_or("");
        output::line(&format!("  {id}  {name}  ({state})"));
    }
    output::blank();
    output::line(&format!("{} projects on this roadmap.", projects.len()));
    Ok(())
}

fn field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn optional(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn person(roadmap: &Value, key: &str) -> String {
    roadmap
        .get(key)
        .filter(|person| !person.is_null())
        .and_then(|person| person.get("displayName"))
        .and_then(Value::as_str)
        .unwrap_or("(none)")
        .to_string()
}
