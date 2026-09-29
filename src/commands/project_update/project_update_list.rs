//! `linear project-update list` — port of
//! `src/commands/project-update/project-update-list.ts`.
//!
//! Error context (`Failed to fetch project updates`) is supplied by the group
//! `mod.rs`, matching upstream's single `handleError` wrapper.

use std::io::IsTerminal;

use serde_json::{json, Value};

use crate::display;
use crate::errors::{CliError, Result};
use crate::{colors, graphql, linear, output};

const LIST_PROJECT_UPDATES_QUERY: &str = r#"
query ListProjectUpdates($id: String!, $first: Int) {
  project(id: $id) {
    name
    slugId
    projectUpdates(first: $first) {
      nodes {
        id
        body
        health
        url
        createdAt
        user {
          name
          displayName
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

#[derive(clap::Args, Debug)]
pub struct ProjectUpdateListArgs {
    /// Project ID, slug ID, URL, or exact name
    pub project_id: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Limit results
    #[arg(long, default_value_t = 10)]
    pub limit: i64,
}

pub fn run(args: ProjectUpdateListArgs) -> Result<()> {
    // Resolve before the request so a bad reference fails with upstream's
    // `Project not found: ...`.
    let resolved_project_id = linear::resolve_project_id(&args.project_id)?;

    let client = graphql::client()?;
    let result = client.request(
        LIST_PROJECT_UPDATES_QUERY,
        json!({ "id": resolved_project_id, "first": args.limit }),
    )?;

    let project = result
        .get("project")
        .filter(|value| !value.is_null())
        .ok_or_else(|| CliError::not_found("Project", &args.project_id))?;

    let updates = project
        .pointer("/projectUpdates/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if args.json {
        output::print_json(project);
        return Ok(());
    }

    let project_name = project.get("name").and_then(Value::as_str).unwrap_or("");

    if updates.is_empty() {
        output::line(&format!(
            "No status updates found for project: {project_name}"
        ));
        return Ok(());
    }

    output::line(&format!("Status updates for: {project_name}"));
    output::blank();

    let columns = if std::io::stdout().is_terminal() {
        terminal_size::terminal_size()
            .map(|(width, _)| width.0 as usize)
            .unwrap_or(120)
    } else {
        120
    };

    // --- measure columns ---------------------------------------------------
    let id_width = 8usize;

    let health_width = updates
        .iter()
        .map(|update| str_at(update, "health").len().max(1))
        .fold(6usize, usize::max);

    let date_width = updates
        .iter()
        .map(|update| display::display_width(&created_ago(update)))
        .fold(4usize, usize::max);

    let author_width = updates
        .iter()
        .map(|update| display::display_width(&author_of(update)))
        .fold(6usize, usize::max);

    let space_width = 4usize;
    let fixed = id_width + health_width + date_width + author_width + space_width;
    let padding = 1usize;
    let available_width = columns.saturating_sub(padding).saturating_sub(fixed).max(10);

    // --- header ------------------------------------------------------------
    let header = [
        display::pad_display("ID", id_width),
        display::pad_display("HEALTH", health_width),
        display::pad_display("DATE", date_width),
        display::pad_display("AUTHOR", author_width),
    ]
    .iter()
    .map(|cell| colors::underline(cell))
    .collect::<Vec<_>>()
    .join(" ");
    output::line(&header);

    // --- rows --------------------------------------------------------------
    for update in &updates {
        let id = str_at(update, "id");
        let short_id = id.get(..id.len().min(id_width)).unwrap_or(id);
        let health = str_at(update, "health");
        let health = if health.is_empty() { "-" } else { health };
        let date = created_ago(update);
        let author = author_of(update);

        let health_cell = display::pad_display(health, health_width);
        let health_cell = match str_at(update, "health") {
            "onTrack" => colors::green(&health_cell),
            "atRisk" => colors::yellow(&health_cell),
            "offTrack" => colors::red(&health_cell),
            _ => health_cell,
        };

        output::line(&format!(
            "{} {} {} {}",
            display::pad_display(short_id, id_width),
            health_cell,
            display::pad_display(&date, date_width),
            display::pad_display(&author, author_width),
        ));

        let body = str_at(update, "body");
        if !body.is_empty() {
            let body_preview = body.replace('\n', " ");
            let truncated = display::truncate_text(body_preview.trim(), available_width);
            output::line(&colors::gray(&format!("   {truncated}")));
        }
    }

    Ok(())
}

fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn author_of(update: &Value) -> String {
    if let Some(display_name) = non_empty(update.pointer("/user/displayName")) {
        return display_name;
    }
    if let Some(name) = non_empty(update.pointer("/user/name")) {
        return name;
    }
    "-".to_string()
}

fn non_empty(value: Option<&Value>) -> Option<String> {
    match value.and_then(Value::as_str) {
        Some(text) if !text.is_empty() => Some(text.to_string()),
        _ => None,
    }
}

fn created_ago(update: &Value) -> String {
    update
        .get("createdAt")
        .and_then(Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|date| display::get_time_ago(date.with_timezone(&chrono::Utc)))
        .unwrap_or_default()
}
