//! `linear team list` — list teams in the workspace.

use std::io::IsTerminal;

use serde_json::{json, Map, Value};

use crate::colors;
use crate::config;
use crate::consts;
use crate::display;
use crate::errors::{CliError, Result};
use crate::graphql;
use crate::output;

const GET_TEAMS_QUERY: &str = r#"
  query GetTeams($filter: TeamFilter, $first: Int, $after: String) {
    teams(filter: $filter, first: $first, after: $after) {
      nodes {
        id
        name
        key
        description
        icon
        color
        cyclesEnabled
        createdAt
        updatedAt
        archivedAt
        organization {
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
"#;

#[derive(clap::Args, Debug)]
pub struct ListArgs {
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

pub fn run(args: ListArgs) -> Result<()> {
    if args.web || args.app {
        let workspace = config::cli_workspace().or_else(config::workspace);
        let Some(workspace) = workspace else {
            return Err(CliError::validation(
                "workspace is not set via command line, configuration file, or environment",
            ));
        };

        let url = format!(
            "{}/{}/settings/teams",
            consts::LINEAR_WEB_BASE_URL,
            workspace
        );
        let destination = if args.app { "Linear.app" } else { "web browser" };
        output::line(&format!("Opening {url} in {destination}"));
        crate::actions::open_url(&url, args.app)?;
        return Ok(());
    }

    let client = graphql::client()?;

    // Fetch all teams with pagination.
    let mut all_teams: Vec<Value> = Vec::new();
    let mut page_info = json!({ "hasNextPage": false, "endCursor": null });
    let mut after: Option<String> = None;

    loop {
        let mut variables = Map::new();
        variables.insert("first".to_string(), json!(100));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }

        let data = client.request(GET_TEAMS_QUERY, Value::Object(variables))?;
        let teams = data
            .get("teams")
            .ok_or_else(|| CliError::cli("Linear API response did not contain teams"))?;

        if let Some(nodes) = teams.get("nodes").and_then(Value::as_array) {
            all_teams.extend(nodes.iter().cloned());
        }

        let current = teams
            .get("pageInfo")
            .cloned()
            .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));
        let has_next = current
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let end_cursor = current
            .get("endCursor")
            .and_then(Value::as_str)
            .map(str::to_string);

        if has_next && end_cursor.is_none() {
            return Err(CliError::cli(
                "Linear reported more teams but returned no pagination cursor",
            )
            .suggestion("Retry the command."));
        }

        page_info = current;
        after = end_cursor;

        if !has_next {
            break;
        }
    }

    // Filter out archived teams and sort alphabetically by name.
    let mut teams: Vec<Value> = all_teams
        .into_iter()
        .filter(|team| {
            team.get("archivedAt")
                .map(Value::is_null)
                .unwrap_or(true)
        })
        .collect();
    teams.sort_by_key(|team| {
        team.get("name")
            .and_then(Value::as_str)
            .map(str::to_lowercase)
            .unwrap_or_default()
    });

    if args.json {
        output::print_json(&json!({ "nodes": teams, "pageInfo": page_info }));
        return Ok(());
    }

    if teams.is_empty() {
        output::line("No teams found.");
        return Ok(());
    }

    let columns = if std::io::stdout().is_terminal() {
        terminal_size::terminal_size()
            .map(|(width, _)| usize::from(width.0))
            .unwrap_or(120)
    } else {
        120
    };

    let id_width = std::iter::once(2)
        .chain(teams.iter().map(|team| {
            display::display_width(team.get("id").and_then(Value::as_str).unwrap_or(""))
        }))
        .max()
        .unwrap_or(2);
    let key_width = std::iter::once(3)
        .chain(teams.iter().map(|team| {
            display::display_width(team.get("key").and_then(Value::as_str).unwrap_or(""))
        }))
        .max()
        .unwrap_or(3);
    let cycles_width = std::cmp::max(6, 3);
    let updated_width = std::iter::once(7)
        .chain(teams.iter().map(|team| time_ago_of(team).len()))
        .max()
        .unwrap_or(7);

    let space_width = 5;
    let fixed = id_width + key_width + cycles_width + updated_width + space_width;
    let padding = 1;
    let max_name_width = teams
        .iter()
        .map(|team| display::display_width(team.get("name").and_then(Value::as_str).unwrap_or("")))
        .max()
        .unwrap_or(0);
    let available_width = columns.saturating_sub(padding + fixed);
    let name_width = std::cmp::min(max_name_width, available_width);

    // Print header: each cell underlined, separated by plain spaces.
    let header = [
        display::pad_display("KEY", key_width),
        display::pad_display("NAME", name_width),
        display::pad_display("CYCLES", cycles_width),
        display::pad_display("UPDATED", updated_width),
        display::pad_display("ID", id_width),
    ]
    .iter()
    .map(|cell| colors::underline(cell))
    .collect::<Vec<_>>()
    .join(" ");
    output::line(&header);

    // Print each team.
    for team in &teams {
        let key = team.get("key").and_then(Value::as_str).unwrap_or("");
        let id = team.get("id").and_then(Value::as_str).unwrap_or("");
        let name = team.get("name").and_then(Value::as_str).unwrap_or("");
        let cycles = if team
            .get("cyclesEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "Yes"
        } else {
            "No"
        };
        let updated = time_ago_of(team);
        let color = team
            .get("color")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("#ffffff");

        let trunc_name = display::truncate_text(name, name_width);

        output::line(&format!(
            "{} {} {} {} {}",
            colors::color_hex(color, &display::pad_display(key, key_width)),
            trunc_name,
            display::pad_display(cycles, cycles_width),
            colors::gray(&display::pad_display(&updated, updated_width)),
            colors::gray(&display::pad_display(id, id_width)),
        ));
    }

    Ok(())
}

/// `getTimeAgo(new Date(updatedAt))`; an unparseable stamp renders empty.
fn time_ago_of(team: &Value) -> String {
    team.get("updatedAt")
        .and_then(Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|date| display::get_time_ago(date.with_timezone(&chrono::Utc)))
        .unwrap_or_default()
}
