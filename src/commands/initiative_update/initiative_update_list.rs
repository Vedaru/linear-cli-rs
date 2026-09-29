//! `linear initiative-update list` — port of
//! `src/commands/initiative-update/initiative-update-list.ts`.
//!
//! Error context (`Failed to fetch initiative updates`) is supplied by the
//! group `mod.rs`, matching upstream's single `handleError` wrapper.

use std::io::IsTerminal;

use serde_json::{json, Value};

use crate::display;
use crate::errors::{CliError, Result};
use crate::{colors, graphql, linear, output};

const LIST_INITIATIVE_UPDATES_QUERY: &str = r#"
query ListInitiativeUpdates($id: String!, $first: Int) {
  initiative(id: $id) {
    name
    slugId
    initiativeUpdates(first: $first) {
      nodes {
        id
        body
        health
        url
        createdAt
        user {
          name
        }
      }
    }
  }
}
"#;

/// Linear's health hex palette, matching upstream's `HEALTH_COLORS`.
const DEFAULT_HEALTH_COLOR: &str = "#6B6F76";

#[derive(clap::Args, Debug)]
pub struct InitiativeUpdateListArgs {
    /// Initiative ID, slug ID, URL, or exact name
    pub initiative_id: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
    /// Limit results
    #[arg(long, default_value_t = 10)]
    pub limit: i64,
}

pub fn run(args: InitiativeUpdateListArgs) -> Result<()> {
    // Resolve before the request so a bad reference fails with upstream's
    // `Initiative not found: ...`.
    let resolved_id = linear::resolve_initiative_id(&args.initiative_id)?;

    let client = graphql::client()?;
    let result = client.request(
        LIST_INITIATIVE_UPDATES_QUERY,
        json!({ "id": resolved_id, "first": args.limit }),
    )?;

    let initiative = result
        .get("initiative")
        .filter(|value| !value.is_null())
        .ok_or_else(|| CliError::not_found("Initiative", &args.initiative_id))?;

    let updates = initiative
        .pointer("/initiativeUpdates/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if args.json {
        output::print_json(initiative);
        return Ok(());
    }

    let initiative_name = initiative.get("name").and_then(Value::as_str).unwrap_or("");

    if updates.is_empty() {
        output::line(&format!("No status updates found for: {initiative_name}"));
        return Ok(());
    }

    output::line(&format!("Status updates for: {initiative_name}"));
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
        .map(|update| match str_at(update, "health") {
            "" => 1,
            health => display::display_width(&health_display(health)).max(1),
        })
        .fold(6usize, usize::max);

    let date_width = updates
        .iter()
        .map(|update| display::display_width(&relative_time(update)))
        .fold(4usize, usize::max);

    let author_width = updates
        .iter()
        .map(|update| display::display_width(&author_of(update)))
        .fold(6usize, usize::max);

    let space_width = 4usize;
    let fixed = id_width + health_width + date_width + author_width + space_width;
    let padding = 1usize;
    let available_width = columns
        .saturating_sub(padding)
        .saturating_sub(fixed)
        .max(10);

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
        let health_text = if health.is_empty() {
            "-".to_string()
        } else {
            health_display(health)
        };
        let health_cell = display::pad_display(&health_text, health_width);
        let health_cell = colors::color_hex(health_color(health), &health_cell);
        let date = relative_time(update);
        let author = author_of(update);

        output::line(&format!(
            "{} {} {} {}",
            display::pad_display(short_id, id_width),
            health_cell,
            colors::color_hex(
                DEFAULT_HEALTH_COLOR,
                &display::pad_display(&date, date_width)
            ),
            display::pad_display(&author, author_width),
        ));

        let body = str_at(update, "body");
        if !body.is_empty() {
            let body_preview = body.replace('\n', " ");
            let truncated = display::truncate_text(body_preview.trim(), available_width);
            output::line(&colors::color_hex("#6B6F76", &format!("  {truncated}")));
        }
    }

    Ok(())
}

fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn health_display(health: &str) -> String {
    match health {
        "onTrack" => "On Track".to_string(),
        "atRisk" => "At Risk".to_string(),
        "offTrack" => "Off Track".to_string(),
        other => other.to_string(),
    }
}

fn health_color(health: &str) -> &'static str {
    match health {
        "onTrack" => "#27AE60",
        "atRisk" => "#F2994A",
        "offTrack" => "#EB5757",
        _ => DEFAULT_HEALTH_COLOR,
    }
}

fn author_of(update: &Value) -> String {
    non_empty(update.pointer("/user/name")).unwrap_or_else(|| "-".to_string())
}

fn non_empty(value: Option<&Value>) -> Option<String> {
    match value.and_then(Value::as_str) {
        Some(text) if !text.is_empty() => Some(text.to_string()),
        _ => None,
    }
}

fn relative_time(update: &Value) -> String {
    update
        .get("createdAt")
        .and_then(Value::as_str)
        .map(display::format_relative_time)
        .unwrap_or_default()
}
