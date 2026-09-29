//! `linear milestone list` — port of
//! `src/commands/milestone/milestone-list.ts`.

use std::cmp::Ordering;
use std::io::IsTerminal;

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{display, graphql, linear, output};

const GET_PROJECT_MILESTONES_QUERY: &str = r#"
query GetProjectMilestones($projectId: String!, $first: Int, $after: String) {
  project(id: $projectId) {
    id
    name
    projectMilestones(first: $first, after: $after) {
      nodes {
        id
        name
        targetDate
        sortOrder
        project {
          id
          name
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
pub struct ListArgs {
    /// Project (UUID, slug ID, or name)
    #[arg(long, value_name = "project")]
    pub project: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ListArgs) -> Result<()> {
    let project_input = args.project.clone();
    let project_id = linear::resolve_project_id(&args.project)?;

    let client = graphql::client()?;
    let mut milestones: Vec<Value> = Vec::new();
    let mut page_info = json!({ "hasNextPage": false, "endCursor": null });
    let mut after: Option<String> = None;

    loop {
        let mut variables = Map::new();
        variables.insert("projectId".to_string(), json!(project_id));
        variables.insert("first".to_string(), json!(100));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }

        let result = client.request(GET_PROJECT_MILESTONES_QUERY, Value::Object(variables))?;

        let Some(project) = result.get("project").filter(|value| !value.is_null()) else {
            return Err(CliError::not_found("Project", &project_input));
        };

        if let Some(nodes) = project
            .get("projectMilestones")
            .and_then(|connection| connection.get("nodes"))
            .and_then(Value::as_array)
        {
            milestones.extend(nodes.iter().cloned());
        }

        let next_page_info = project
            .get("projectMilestones")
            .and_then(|connection| connection.get("pageInfo"))
            .cloned()
            .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));

        let has_next = next_page_info
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let end_cursor = next_page_info
            .get("endCursor")
            .and_then(Value::as_str)
            .map(str::to_string);

        if has_next && end_cursor.is_none() {
            return Err(CliError::cli(
                "Linear reported more milestones but returned no pagination cursor",
            )
            .suggestion("Retry the command."));
        }

        page_info = next_page_info;
        if !has_next {
            break;
        }
        after = end_cursor;
    }

    // Sort milestones by targetDate (nulls last) then by name.
    milestones.sort_by(|a, b| {
        let a_date = a.get("targetDate").and_then(Value::as_str);
        let b_date = b.get("targetDate").and_then(Value::as_str);
        let a_name = a.get("name").and_then(Value::as_str).unwrap_or("");
        let b_name = b.get("name").and_then(Value::as_str).unwrap_or("");
        match (a_date, b_date) {
            (None, None) => a_name.cmp(b_name),
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a_date), Some(b_date)) => {
                let date_comparison = a_date.cmp(b_date);
                if date_comparison != Ordering::Equal {
                    date_comparison
                } else {
                    a_name.cmp(b_name)
                }
            }
        }
    });

    if args.json {
        output::print_json(&json!({ "nodes": milestones, "pageInfo": page_info }));
        return Ok(());
    }

    if milestones.is_empty() {
        output::line("No milestones found for this project.");
        return Ok(());
    }

    let columns = terminal_columns();

    const ID_WIDTH: usize = 36;
    const TARGET_DATE_WIDTH: usize = 12;
    let project_width = 30usize.min(
        7usize.max(
            milestones
                .iter()
                .map(|milestone| display::display_width(project_name(milestone)))
                .max()
                .unwrap_or(0),
        ),
    );

    const SPACE_WIDTH: usize = 4;
    let fixed = ID_WIDTH + TARGET_DATE_WIDTH + project_width + SPACE_WIDTH;
    const PADDING: usize = 1;
    let max_name_width = milestones
        .iter()
        .map(|milestone| display::display_width(milestone_name(milestone)))
        .max()
        .unwrap_or(0);
    let available_width = columns.saturating_sub(PADDING + fixed);
    let name_width = max_name_width.min(available_width);

    let header_cells = [
        display::pad_display("NAME", name_width),
        display::pad_display("ID", ID_WIDTH),
        display::pad_display("TARGET DATE", TARGET_DATE_WIDTH),
        display::pad_display("PROJECT", project_width),
    ];
    // Upstream builds the header with `%c` console placeholders whose CSS
    // (`text-decoration: underline`) a terminal ignores; the rendered header is
    // the cells joined by spaces.
    output::line(&header_cells.join(" "));

    for milestone in &milestones {
        let target_date = milestone
            .get("targetDate")
            .and_then(Value::as_str)
            .filter(|date| !date.is_empty())
            .unwrap_or("No date");

        let name_cell = truncate_or_pad(milestone_name(milestone), name_width);
        let project_cell = truncate_or_pad(project_name(milestone), project_width);

        output::line(&format!(
            "{} {} {} {}",
            name_cell,
            display::pad_display(&milestone_id(milestone), ID_WIDTH),
            display::pad_display(target_date, TARGET_DATE_WIDTH),
            project_cell,
        ));
    }

    Ok(())
}

fn terminal_columns() -> usize {
    if std::io::stdout().is_terminal() {
        if let Some((width, _)) = terminal_size::terminal_size() {
            return width.0 as usize;
        }
    }
    120
}

fn milestone_name(milestone: &Value) -> &str {
    milestone.get("name").and_then(Value::as_str).unwrap_or("")
}

fn milestone_id(milestone: &Value) -> String {
    milestone
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn project_name(milestone: &Value) -> &str {
    milestone
        .get("project")
        .and_then(|project| project.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// Truncate to `width` characters with a trailing ellipsis, or pad to it.
/// Mirrors the `name.slice(0, width - 3) + "..."` / `padDisplay` branch.
fn truncate_or_pad(text: &str, width: usize) -> String {
    if text.chars().count() > width {
        let take = width.saturating_sub(3);
        let mut truncated: String = text.chars().take(take).collect();
        truncated.push_str("...");
        truncated
    } else {
        display::pad_display(text, width)
    }
}
