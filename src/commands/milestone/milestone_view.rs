//! `linear milestone view` — port of
//! `src/commands/milestone/milestone-view.ts`.

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{display, graphql, linear, output};

const PAGE_SIZE: usize = 50;
const LIST_PREVIEW: usize = 10;

const GET_MILESTONE_DETAILS_QUERY: &str = r#"
query GetMilestoneDetails($id: String!, $first: Int!, $after: String) {
  projectMilestone(id: $id) {
    id
    name
    description
    targetDate
    sortOrder
    createdAt
    updatedAt
    project {
      id
      name
      slugId
      url
    }
    issues(first: $first, after: $after) {
      nodes {
        id
        identifier
        title
        state {
          name
          type
        }
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct ViewArgs {
    /// Milestone UUID or name
    #[arg(value_name = "milestone")]
    pub milestone: String,
    /// Fetch and list every issue attached to the milestone (paginates the Linear API).
    #[arg(long)]
    pub all: bool,
    /// Project for resolving a milestone name (UUID, slug ID, or name)
    #[arg(long, value_name = "project")]
    pub project: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ViewArgs) -> Result<()> {
    let milestone_input = args.milestone.clone();
    let milestone_id = match &args.project {
        Some(project) => {
            let project_id = linear::resolve_project_id(project)?;
            linear::resolve_milestone_id(&milestone_input, Some(&project_id))?
        }
        // Without --project, pass the input through to the API. Linear will
        // resolve it if it's a UUID and return null otherwise.
        None => milestone_input.clone(),
    };

    let client = graphql::client()?;
    let first_page = client.request(
        GET_MILESTONE_DETAILS_QUERY,
        json!({ "id": milestone_id, "first": PAGE_SIZE }),
    )?;

    let Some(milestone) = first_page
        .get("projectMilestone")
        .filter(|value| !value.is_null())
    else {
        return Err(CliError::not_found("Milestone", &milestone_input));
    };

    let mut issues: Vec<Value> = milestone
        .get("issues")
        .and_then(|issues| issues.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut page_info = milestone
        .get("issues")
        .and_then(|issues| issues.get("pageInfo"))
        .cloned()
        .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));

    if args.all {
        // Paginate the full set. Fail loudly on inconsistent pagination rather
        // than silently returning a partial list — silently dropping issues is
        // the exact bug --all exists to prevent.
        loop {
            let has_next = page_info
                .get("hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !has_next {
                break;
            }
            let Some(end_cursor) = page_info.get("endCursor").and_then(Value::as_str) else {
                let milestone_id = milestone.get("id").and_then(Value::as_str).unwrap_or("");
                return Err(CliError::cli(
                    "Linear reported more issues but returned no pagination cursor",
                )
                .suggestion(format!(
                    "Retry, or use `linear issue query --milestone {milestone_id} --json` for the full list."
                )));
            };
            let end_cursor = end_cursor.to_string();

            let next_page = client.request(
                GET_MILESTONE_DETAILS_QUERY,
                json!({ "id": milestone_id, "first": PAGE_SIZE, "after": end_cursor }),
            )?;
            let Some(next) = next_page
                .get("projectMilestone")
                .filter(|value| !value.is_null())
            else {
                return Err(CliError::not_found("Milestone", &milestone_input));
            };
            if let Some(nodes) = next
                .get("issues")
                .and_then(|issues| issues.get("nodes"))
                .and_then(Value::as_array)
            {
                issues.extend(nodes.iter().cloned());
            }
            page_info = next
                .get("issues")
                .and_then(|issues| issues.get("pageInfo"))
                .cloned()
                .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));
        }
    }

    if args.json {
        // Same connection the human output works from: the first page, or
        // every page under --all. The 10-item preview is presentation only.
        let mut merged = milestone.clone();
        if let Some(object) = merged.as_object_mut() {
            object.insert(
                "issues".to_string(),
                json!({ "nodes": issues, "pageInfo": page_info }),
            );
        }
        output::print_json(&merged);
        return Ok(());
    }

    let truncated = !args.all
        && page_info
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);

    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("# {}", string_field(milestone, "name").unwrap_or("")));
    lines.push(String::new());

    lines.push(format!(
        "**ID:** {}",
        string_field(milestone, "id").unwrap_or("")
    ));
    match string_field(milestone, "targetDate").filter(|date| !date.is_empty()) {
        Some(target_date) => lines.push(format!("**Target Date:** {target_date}")),
        None => lines.push("**Target Date:** Not set".to_string()),
    }

    let project = milestone.get("project");
    lines.push(format!(
        "**Project:** {} ({})",
        project
            .and_then(|project| project.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(""),
        project
            .and_then(|project| project.get("slugId"))
            .and_then(Value::as_str)
            .unwrap_or(""),
    ));
    lines.push(format!(
        "**Project URL:** {}",
        project
            .and_then(|project| project.get("url"))
            .and_then(Value::as_str)
            .unwrap_or("")
    ));

    lines.push(String::new());
    lines.push(format!(
        "**Created:** {}",
        display::format_relative_time(string_field(milestone, "createdAt").unwrap_or(""))
    ));
    lines.push(format!(
        "**Updated:** {}",
        display::format_relative_time(string_field(milestone, "updatedAt").unwrap_or(""))
    ));

    if let Some(description) = string_field(milestone, "description").filter(|text| !text.is_empty())
    {
        lines.push(String::new());
        lines.push("## Description".to_string());
        lines.push(String::new());
        lines.push(description.to_string());
    }

    if !issues.is_empty() {
        lines.push(String::new());
        lines.push("## Issues".to_string());
        lines.push(String::new());

        let mut by_state: Map<String, Value> = Map::new();
        for issue in &issues {
            if let Some(state_type) = issue
                .get("state")
                .and_then(|state| state.get("type"))
                .and_then(Value::as_str)
            {
                let entry = by_state
                    .entry(state_type.to_string())
                    .or_insert_with(|| json!(0));
                if let Some(count) = entry.as_i64() {
                    *entry = json!(count + 1);
                }
            }
        }
        let count = |key: &str| by_state.get(key).and_then(Value::as_i64).unwrap_or(0);

        let fetched = issues.len() as i64;
        let completed = count("completed");
        let started = count("started");
        let unstarted = count("unstarted");
        let canceled = count("canceled");
        let backlog = count("backlog");
        let triage = count("triage");

        if truncated {
            lines.push(format!(
                "**Issues fetched:** {fetched} (milestone has more — use `--all` for full counts)"
            ));
        } else {
            lines.push(format!("**Total Issues:** {fetched}"));
        }
        if completed > 0 {
            lines.push(format!("**Completed:** {completed}"));
        }
        if started > 0 {
            lines.push(format!("**In Progress:** {started}"));
        }
        if unstarted > 0 {
            lines.push(format!("**To Do:** {unstarted}"));
        }
        if backlog > 0 {
            lines.push(format!("**Backlog:** {backlog}"));
        }
        if triage > 0 {
            lines.push(format!("**Triage:** {triage}"));
        }
        if canceled > 0 {
            lines.push(format!("**Canceled:** {canceled}"));
        }

        lines.push(String::new());
        lines.push(if args.all {
            "**All Issues:**".to_string()
        } else {
            "**Recent Issues:**".to_string()
        });
        lines.push(String::new());
        let listed = if args.all {
            issues.as_slice()
        } else {
            &issues[..issues.len().min(LIST_PREVIEW)]
        };
        for issue in listed {
            lines.push(format!(
                "- {}: {} ({})",
                string_field(issue, "identifier").unwrap_or(""),
                string_field(issue, "title").unwrap_or(""),
                issue
                    .get("state")
                    .and_then(|state| state.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            ));
        }

        if !args.all {
            let hidden_loaded = (fetched - LIST_PREVIEW as i64).max(0);
            let milestone_id = string_field(milestone, "id").unwrap_or("");
            if truncated {
                lines.push(String::new());
                lines.push(format!(
                    "_Showing {} of {fetched}+ issues — the milestone contains more than {PAGE_SIZE}. Re-run with `--all` or use `linear issue query --milestone {milestone_id} --json` for the full list._",
                    (LIST_PREVIEW as i64).min(fetched),
                ));
            } else if hidden_loaded > 0 {
                let plural = if hidden_loaded == 1 { "" } else { "s" };
                lines.push(String::new());
                lines.push(format!(
                    "_...and {hidden_loaded} more issue{plural}. Re-run with `--all` or use `linear issue query --milestone {milestone_id} --json` to see them all._"
                ));
            }
        }
    } else {
        lines.push(String::new());
        lines.push("_No issues in this milestone yet._".to_string());
    }

    // Upstream renders markdown on a terminal via `@littletof/charmd`; this
    // port has no renderer, so it always emits the raw markdown.
    output::line(&lines.join("\n"));

    Ok(())
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
