//! `linear initiative view` — port of
//! `src/commands/initiative/initiative-view.ts`.
//!
//! The initiative is rendered as raw Linear-flavored Markdown (see AGENTS.md —
//! there is no Rust equivalent of `@littletof/charmd`), so upstream's
//! non-terminal branch is the only branch here: the `**Status:**` line a
//! terminal would print with a truecolor style is folded into the markdown,
//! which is what upstream does whenever stdout is not a terminal.
//!
//! This module self-wraps every failure with `Failed to fetch initiative
//! details`, matching upstream's single `handleError`; the group `mod.rs` must
//! not wrap it again.
//!
//! The reference is resolved with `crate::linear::resolve_initiative_id` (URL,
//! UUID, slug ID, or exact name) — the same helper `initiative-update list`
//! uses — so `GetInitiativeDetails` only ever receives a UUID.

use clap::Args;
use serde_json::{json, Value};

use crate::display;
use crate::errors::{CliError, Result};
use crate::{actions, graphql, linear, output};

const GET_INITIATIVE_DETAILS_QUERY: &str = r#"
query GetInitiativeDetails($id: String!) {
  initiative(id: $id) {
    id
    slugId
    name
    description
    status
    targetDate
    health
    color
    icon
    url
    archivedAt
    createdAt
    updatedAt
    owner {
      id
      name
      displayName
    }
    projects {
      nodes {
        id
        slugId
        name
        status {
          name
          type
        }
      }
    }
  }
}
"#;

/// Upstream's `statusOrder`: the order projects are grouped in under
/// `## Projects`, with any status outside the list appended afterwards.
const PROJECT_STATUS_ORDER: [&str; 6] = [
    "started",
    "planned",
    "backlog",
    "paused",
    "completed",
    "canceled",
];

#[derive(Args, Debug)]
pub struct InitiativeViewArgs {
    /// Initiative URL, UUID, slug ID, or exact name
    #[arg(value_name = "initiativeId")]
    pub initiative_id: String,
    /// Open in web browser
    #[arg(short = 'w', long)]
    pub web: bool,
    /// Open in Linear.app
    #[arg(short = 'a', long)]
    pub app: bool,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: InitiativeViewArgs) -> Result<()> {
    view(&args).map_err(|error| error.with_context("Failed to fetch initiative details"))
}

fn view(args: &InitiativeViewArgs) -> Result<()> {
    let resolved_id = linear::resolve_initiative_id(&args.initiative_id)?;

    // `--web`/`--app` still needs the details request: the initiative's own
    // `url` is what opens, not a URL this port could rebuild.
    if args.web || args.app {
        let initiative = fetch_initiative_details(&resolved_id, &args.initiative_id)?;
        let Some(url) = non_empty(&initiative, "url") else {
            return Err(CliError::not_found("Initiative", &args.initiative_id));
        };
        let app = args.app;
        let destination = if app { "Linear.app" } else { "web browser" };
        output::line(&format!("Opening {url} in {destination}"));
        return actions::open_url(&url, app);
    }

    let initiative = fetch_initiative_details(&resolved_id, &args.initiative_id)?;

    if args.json {
        output::print_json(&initiative);
        return Ok(());
    }

    output::line(&format_as_markdown(&initiative));

    Ok(())
}

/// Fetch the initiative, reporting a missing one against the raw reference the
/// user typed (`NotFoundError("Initiative", initiativeId)`).
fn fetch_initiative_details(id: &str, original_input: &str) -> Result<Value> {
    let client = graphql::client()?;
    let result = client.request(GET_INITIATIVE_DETAILS_QUERY, json!({ "id": id }))?;

    result
        .get("initiative")
        .filter(|value| !value.is_null())
        .cloned()
        .ok_or_else(|| CliError::not_found("Initiative", original_input))
}

fn format_as_markdown(initiative: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();

    // Title, with the initiative's icon in front when it has one.
    let icon = match non_empty(initiative, "icon") {
        Some(icon) => format!("{icon} "),
        None => String::new(),
    };
    lines.push(format!("# {icon}{}", str_at(initiative, "/name")));
    lines.push(String::new());

    lines.push(format!("**Slug:** {}", str_at(initiative, "/slugId")));
    lines.push(format!("**URL:** {}", str_at(initiative, "/url")));
    lines.push(format!(
        "**Status:** {}",
        status_display(str_at(initiative, "/status"))
    ));

    if let Some(health) = non_empty(initiative, "health") {
        lines.push(format!("**Health:** {health}"));
    }
    if let Some(owner) = owner_display_name(initiative) {
        lines.push(format!("**Owner:** {owner}"));
    }
    if let Some(target_date) = non_empty(initiative, "targetDate") {
        lines.push(format!("**Target Date:** {target_date}"));
    }
    if let Some(archived_at) = non_empty(initiative, "archivedAt") {
        lines.push(format!(
            "**Archived:** {}",
            display::format_relative_time(&archived_at)
        ));
    }

    lines.push(String::new());
    lines.push(format!(
        "**Created:** {}",
        display::format_relative_time(str_at(initiative, "/createdAt"))
    ));
    lines.push(format!(
        "**Updated:** {}",
        display::format_relative_time(str_at(initiative, "/updatedAt"))
    ));

    if let Some(description) = non_empty(initiative, "description") {
        lines.push(String::new());
        lines.push("## Description".to_string());
        lines.push(String::new());
        lines.push(description);
    }

    let projects = project_nodes(initiative);
    if projects.is_empty() {
        lines.push(String::new());
        lines.push("## Projects".to_string());
        lines.push(String::new());
        lines.push("*No projects linked to this initiative.*".to_string());
    } else {
        lines.push(String::new());
        lines.push(format!("## Projects ({})", projects.len()));
        lines.push(String::new());

        // Grouped by status type, then emitted in upstream's priority order;
        // a status type the list does not name keeps its arrival order and is
        // emitted last, so a new Linear status can never hide a project.
        let mut grouped: Vec<(&str, Vec<&Value>)> = Vec::new();
        for project in &projects {
            let status_type = match str_at(project, "/status/type") {
                "" => "unknown",
                status_type => status_type,
            };
            match grouped
                .iter_mut()
                .find(|(existing, _)| *existing == status_type)
            {
                Some((_, group)) => group.push(project),
                None => grouped.push((status_type, vec![project])),
            }
        }

        for status_type in PROJECT_STATUS_ORDER {
            if let Some((_, group)) = grouped
                .iter()
                .find(|(existing, _)| *existing == status_type)
            {
                for project in group {
                    lines.push(project_line(project));
                }
            }
        }
        for (status_type, group) in &grouped {
            if !PROJECT_STATUS_ORDER.contains(status_type) {
                for project in group {
                    lines.push(project_line(project));
                }
            }
        }
    }

    // Upstream renders markdown on a terminal via `@littletof/charmd`; this
    // port has no renderer, so it always emits the raw markdown, matching
    // upstream's non-terminal path.
    lines.join("\n")
}

/// `initiative.projects?.nodes || []`.
fn project_nodes(initiative: &Value) -> Vec<Value> {
    initiative
        .pointer("/projects/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// `- **{name}** ({status name})`, with `Unknown` for a project whose status
/// did not come back.
fn project_line(project: &Value) -> String {
    let status_name = match str_at(project, "/status/name") {
        "" => "Unknown",
        status_name => status_name,
    };
    format!("- **{}** ({status_name})", str_at(project, "/name"))
}

/// `owner.displayName || owner.name`, or `None` when there is no owner.
fn owner_display_name(initiative: &Value) -> Option<String> {
    let owner = initiative.get("owner").filter(|value| !value.is_null())?;
    non_empty(owner, "displayName").or_else(|| non_empty(owner, "name"))
}

/// Upstream's `INITIATIVE_STATUS_DISPLAY`, whose keys are lowercase — the
/// capitalized API values therefore pass through unchanged.
fn status_display(status: &str) -> &str {
    match status {
        "active" => "Active",
        "planned" => "Planned",
        "paused" => "Paused",
        "completed" => "Completed",
        "canceled" => "Canceled",
        other => other,
    }
}

fn non_empty(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}
