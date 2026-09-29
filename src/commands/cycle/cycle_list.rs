//! `linear cycle list` — port of `src/commands/cycle/cycle-list.ts`.

use std::io::IsTerminal;

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{colors, display, graphql, linear, output};

const GET_TEAM_CYCLES_QUERY: &str = r#"
query GetTeamCycles($teamId: String!, $first: Int, $after: String) {
  team(id: $teamId) {
    id
    name
    cycles(first: $first, after: $after) {
      nodes {
        id
        number
        name
        startsAt
        endsAt
        completedAt
        isActive
        isFuture
        isPast
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
    /// Team key, name, or ID (defaults to current team)
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ListArgs) -> Result<()> {
    let team_key = match args.team {
        Some(team) => team,
        None => linear::get_team_key().ok_or_else(|| {
            CliError::validation("Could not determine team key from directory name or team flag")
        })?,
    };

    let team_id = linear::resolve_team(&team_key)?.id;
    let client = graphql::client()?;

    let mut cycles: Vec<Value> = Vec::new();
    let mut page_info = json!({ "hasNextPage": false, "endCursor": null });
    let mut after: Option<String> = None;

    loop {
        let mut variables = Map::new();
        variables.insert("teamId".to_string(), json!(team_id));
        variables.insert("first".to_string(), json!(100));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }

        let result = client.request(GET_TEAM_CYCLES_QUERY, Value::Object(variables))?;

        let Some(team) = result.get("team").filter(|value| !value.is_null()) else {
            return Err(CliError::not_found("Team", &team_key));
        };

        if let Some(nodes) = team
            .get("cycles")
            .and_then(|cycles| cycles.get("nodes"))
            .and_then(Value::as_array)
        {
            cycles.extend(nodes.iter().cloned());
        }

        let next_page_info = team
            .get("cycles")
            .and_then(|cycles| cycles.get("pageInfo"))
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
                "Linear reported more cycles but returned no pagination cursor",
            )
            .suggestion("Retry the command."));
        }

        page_info = next_page_info;
        if !has_next {
            break;
        }
        after = end_cursor;
    }

    // Most recent start first, mirroring `b.startsAt.localeCompare(a.startsAt)`.
    cycles.sort_by(|a, b| {
        let a_start = a.get("startsAt").and_then(Value::as_str).unwrap_or("");
        let b_start = b.get("startsAt").and_then(Value::as_str).unwrap_or("");
        b_start.cmp(a_start)
    });

    if args.json {
        output::print_json(&json!({ "nodes": cycles, "pageInfo": page_info }));
        return Ok(());
    }

    if cycles.is_empty() {
        output::line("No cycles found for this team.");
        return Ok(());
    }

    let columns = terminal_columns();

    let number_width = cycles
        .iter()
        .map(|cycle| cycle_number(cycle).to_string().chars().count())
        .max()
        .unwrap_or(1)
        .max(1);
    const START_WIDTH: usize = 10;
    const END_WIDTH: usize = 10;
    const STATUS_WIDTH: usize = 9;
    const SPACE_WIDTH: usize = 4;

    let fixed = number_width + START_WIDTH + END_WIDTH + STATUS_WIDTH + SPACE_WIDTH;
    const PADDING: usize = 1;
    let max_name_width = cycles
        .iter()
        .map(|cycle| display::display_width(&cycle_name(cycle)))
        .max()
        .unwrap_or(4)
        .max(4);
    let available_width = columns.saturating_sub(PADDING + fixed);
    let name_width = max_name_width.min(available_width);

    let header_cells = [
        display::pad_display("#", number_width),
        display::pad_display("NAME", name_width),
        display::pad_display("START", START_WIDTH),
        display::pad_display("END", END_WIDTH),
        display::pad_display("STATUS", STATUS_WIDTH),
    ];
    output::line(&colors::header(&header_cells.join(" ")));

    for cycle in &cycles {
        let name = cycle_name(cycle);
        let trunc_name = truncate_name(&name, name_width);

        let status = cycle_status(cycle);
        let status_str = display::pad_display(status, STATUS_WIDTH);
        let status_display = if bool_field(cycle, "isActive") {
            colors::green(&status_str)
        } else if bool_field(cycle, "isPast") || completed(cycle) {
            colors::muted(&status_str)
        } else {
            status_str
        };

        let line = format!(
            "{} {} {} {} {}",
            display::pad_display(&cycle_number(cycle).to_string(), number_width),
            trunc_name,
            display::pad_display(&format_date(cycle.get("startsAt").and_then(Value::as_str)), START_WIDTH),
            display::pad_display(&format_date(cycle.get("endsAt").and_then(Value::as_str)), END_WIDTH),
            status_display,
        );
        output::line(&line);
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

fn cycle_number(cycle: &Value) -> i64 {
    cycle.get("number").and_then(Value::as_i64).unwrap_or(0)
}

fn bool_field(cycle: &Value, key: &str) -> bool {
    cycle.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn completed(cycle: &Value) -> bool {
    cycle
        .get("completedAt")
        .map(|value| !value.is_null())
        .unwrap_or(false)
}

fn cycle_name(cycle: &Value) -> String {
    cycle
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Cycle {}", cycle_number(cycle)))
}

fn cycle_status(cycle: &Value) -> &'static str {
    if bool_field(cycle, "isActive") {
        "Active"
    } else if bool_field(cycle, "isFuture") {
        "Upcoming"
    } else if completed(cycle) {
        "Completed"
    } else if bool_field(cycle, "isPast") {
        "Past"
    } else {
        "Unknown"
    }
}

fn format_date(date: Option<&str>) -> String {
    date.unwrap_or("").chars().take(10).collect()
}

fn truncate_name(name: &str, width: usize) -> String {
    if name.chars().count() > width {
        let take = width.saturating_sub(3);
        let mut truncated: String = name.chars().take(take).collect();
        truncated.push_str("...");
        truncated
    } else {
        display::pad_display(name, width)
    }
}
