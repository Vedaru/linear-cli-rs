//! `linear project list` — port of `src/commands/project/project-list.ts`.
//!
//! This module self-wraps: the `--web`/`--app` branch reports
//! `Failed to open projects` (upstream's inner `handleError`) while the listing
//! reports `Failed to fetch projects`, so the group `mod.rs` must not wrap it
//! again.

use std::io::IsTerminal;

use serde_json::{json, Map, Value};

use crate::display;
use crate::errors::{CliError, Result};
use crate::{colors, config, consts, graphql, linear, output};

const GET_PROJECTS_QUERY: &str = r#"
query GetProjects($filter: ProjectFilter, $first: Int, $after: String) {
  projects(filter: $filter, first: $first, after: $after) {
    nodes {
      id
      name
      description
      slugId
      icon
      color
      sortOrder
      status {
        id
        name
        color
        type
      }
      lead {
        name
        displayName
        initials
      }
      priority
      health
      startDate
      targetDate
      startedAt
      completedAt
      canceledAt
      createdAt
      updatedAt
      url
      teams {
        nodes {
          key
        }
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

const GET_VIEWER_QUERY: &str = r#"
query GetViewer {
  viewer {
    organization {
      urlKey
    }
  }
}
"#;

#[derive(clap::Args, Debug)]
pub struct ProjectListArgs {
    /// Filter by team key, name, or ID
    #[arg(long = "team")]
    pub team: Option<String>,
    /// Show projects from all teams
    #[arg(long = "all-teams")]
    pub all_teams: bool,
    /// Filter by status name
    #[arg(long = "status")]
    pub status: Option<String>,
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

pub fn run(args: ProjectListArgs) -> Result<()> {
    if args.web || args.app {
        return open_projects(&args).map_err(|error| error.with_context("Failed to open projects"));
    }
    list(&args).map_err(|error| error.with_context("Failed to fetch projects"))
}

/// Resolve the team key the listing is scoped to. `--all-teams` wins, then an
/// explicit `--team`, then the configured team.
fn resolve_team_key(team: &Option<String>, all_teams: bool) -> Result<Option<String>> {
    if all_teams {
        return Ok(None);
    }
    match team {
        Some(reference) => Ok(Some(linear::resolve_team(reference)?.key)),
        None => linear::get_team_key(),
    }
}

fn open_projects(args: &ProjectListArgs) -> Result<()> {
    let workspace = match config::cli_workspace().or_else(config::workspace) {
        Some(workspace) => workspace,
        None => {
            let client = graphql::client()?;
            let data = client.request(GET_VIEWER_QUERY, json!({}))?;
            data.pointer("/viewer/organization/urlKey")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| CliError::cli("Linear API returned no workspace URL key"))?
        }
    };

    let team_key = resolve_team_key(&args.team, args.all_teams)?;
    let url = match team_key {
        Some(team_key) => format!(
            "{}/{}/team/{}/projects/all",
            consts::LINEAR_WEB_BASE_URL,
            workspace,
            team_key
        ),
        None => format!("{}/{}/projects/all", consts::LINEAR_WEB_BASE_URL, workspace),
    };
    let destination = if args.app { "Linear.app" } else { "web browser" };
    output::line(&format!("Opening {url} in {destination}"));
    crate::actions::open_url(&url, args.app)
}

fn list(args: &ProjectListArgs) -> Result<()> {
    if args.team.is_some() && args.all_teams {
        return Err(CliError::validation(
            "Cannot use both --team and --all-teams flags",
        ));
    }

    let team_key = resolve_team_key(&args.team, args.all_teams)?;

    let mut filter = Map::new();
    if let Some(team_key) = &team_key {
        filter.insert(
            "accessibleTeams".to_string(),
            json!({ "some": { "key": { "eq": team_key } } }),
        );
    }
    if let Some(status) = &args.status {
        filter.insert(
            "status".to_string(),
            json!({ "name": { "eq": status } }),
        );
    }

    let client = graphql::client()?;

    let mut all_projects: Vec<Value> = Vec::new();
    let mut page_info = json!({ "hasNextPage": false, "endCursor": null });
    let mut after: Option<String> = None;
    let mut has_next = true;

    while has_next {
        let mut variables = Map::new();
        if !filter.is_empty() {
            variables.insert("filter".to_string(), Value::Object(filter.clone()));
        }
        variables.insert("first".to_string(), json!(100));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }

        let data = client.request(GET_PROJECTS_QUERY, Value::Object(variables))?;
        let connection = data.get("projects");
        if let Some(nodes) = connection
            .and_then(|value| value.get("nodes"))
            .and_then(Value::as_array)
        {
            all_projects.extend(nodes.iter().cloned());
        }

        let current = connection
            .and_then(|value| value.get("pageInfo"))
            .cloned()
            .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));
        has_next = current
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let end_cursor = current
            .get("endCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        page_info = current;
        after = end_cursor;

        if has_next && after.is_none() {
            return Err(CliError::cli(
                "Linear reported another page of projects but returned no pagination cursor",
            )
            .suggestion("Retry the command."));
        }
    }

    if all_projects.is_empty() {
        if args.json {
            output::print_json(&json!({ "nodes": all_projects, "pageInfo": page_info }));
        } else {
            output::line("No projects found.");
        }
        return Ok(());
    }

    sort_projects(&mut all_projects)?;

    if args.json {
        output::print_json(&json!({ "nodes": all_projects, "pageInfo": page_info }));
        return Ok(());
    }

    render_table(&all_projects)
}

/// Linear's own project list order: manual `sortOrder` ascending, then name,
/// then id. A missing or non-numeric `sortOrder` is a hard error rather than a
/// scrambled listing.
fn sort_projects(projects: &mut [Value]) -> Result<()> {
    for project in projects.iter() {
        let field = project.get("sortOrder");
        if field.and_then(Value::as_f64).is_none() {
            return Err(CliError::cli(
                "Linear returned a non-numeric sortOrder for a project.",
            )
            .suggestion("Retry, or report this if it keeps happening."));
        }
    }
    projects.sort_by(|a, b| {
        let a_order = a.get("sortOrder").and_then(Value::as_f64).unwrap_or(0.0);
        let b_order = b.get("sortOrder").and_then(Value::as_f64).unwrap_or(0.0);
        a_order
            .partial_cmp(&b_order)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| str_of(a, "name").cmp(str_of(b, "name")))
            .then_with(|| str_of(a, "id").cmp(str_of(b, "id")))
    });
    Ok(())
}

fn render_table(projects: &[Value]) -> Result<()> {
    let columns = if std::io::stdout().is_terminal() {
        terminal_size::terminal_size()
            .map(|(width, _)| width.0 as usize)
            .unwrap_or(120)
    } else {
        120
    };

    let slug_width = projects
        .iter()
        .map(|project| display::display_width(str_of(project, "slugId")))
        .fold(4usize, usize::max);
    let status_width = projects
        .iter()
        .map(|project| display::display_width(str_at(project, "/status/name")))
        .fold(6usize, usize::max);
    let priority_width = projects
        .iter()
        .map(|project| {
            display::display_width(&display::get_project_priority_label(priority_of(project)))
        })
        .fold(8usize, usize::max);
    let health_width = projects
        .iter()
        .map(|project| display::display_width(&health_of(project)))
        .fold(6usize, usize::max);
    let lead_width = projects
        .iter()
        .map(|project| display::display_width(&lead_of(project)))
        .fold(4usize, usize::max);
    let teams_width = projects
        .iter()
        .map(|project| display::display_width(&teams_of(project)))
        .fold(5usize, usize::max);
    let mut date_width = 4usize;
    let mut dates: Vec<String> = Vec::with_capacity(projects.len());
    for project in projects {
        let date = get_display_date(project)?;
        date_width = date_width.max(display::display_width(&date));
        dates.push(date);
    }

    let space_width = 4usize;
    let fixed = slug_width
        + status_width
        + priority_width
        + health_width
        + lead_width
        + teams_width
        + date_width
        + space_width;
    let padding = 1usize;
    let max_name_width = projects
        .iter()
        .map(|project| display::display_width(str_of(project, "name")))
        .max()
        .unwrap_or(0);
    let available_width = columns.saturating_sub(padding).saturating_sub(fixed);
    let name_width = max_name_width.min(available_width);

    let header = [
        display::pad_display("SLUG", slug_width),
        display::pad_display("NAME", name_width),
        display::pad_display("STATUS", status_width),
        display::pad_display("PRIORITY", priority_width),
        display::pad_display("HEALTH", health_width),
        display::pad_display("LEAD", lead_width),
        display::pad_display("TEAMS", teams_width),
        display::pad_display("DATE", date_width),
    ];
    output::line(&colors::header(&header.join(" ")));

    for (project, date) in projects.iter().zip(dates.iter()) {
        let name = str_of(project, "name");
        let trunc_name = if display::display_width(name) > name_width {
            let slice = display::truncate_text(name, name_width.saturating_sub(3));
            format!("{slice}...")
        } else {
            display::pad_display(name, name_width)
        };

        let status = str_at(project, "/status/name");
        let status_color = str_at(project, "/status/color");
        let priority = display::get_project_priority_label(priority_of(project));
        let health = health_of(project);
        let lead = lead_of(project);
        let teams = teams_of(project);

        let status_cell = if status_color.is_empty() {
            display::pad_display(status, status_width)
        } else {
            colors::color_hex(status_color, &display::pad_display(status, status_width))
        };

        let line = format!(
            "{} {} {} {} {} {} {} {}",
            display::pad_display(str_of(project, "slugId"), slug_width),
            trunc_name,
            status_cell,
            display::pad_display(&priority, priority_width),
            display::pad_display(&health, health_width),
            display::pad_display(&lead, lead_width),
            display::pad_display(&teams, teams_width),
            colors::gray(&display::pad_display(date, date_width)),
        );
        output::line(&line);
    }

    Ok(())
}

/// The most relevant date for a project, keyed off its status type.
fn get_display_date(project: &Value) -> Result<String> {
    let status_type = str_at(project, "/status/type");
    let date = match status_type {
        "started" => {
            if let Some(started_at) = non_empty(project.get("startedAt")) {
                Some(format!(
                    "Started {}",
                    display::format_relative_time(&started_at)
                ))
            } else if let Some(start_date) = non_empty(project.get("startDate")) {
                Some(format!("Start: {start_date}"))
            } else {
                Some(format!(
                    "Created {}",
                    display::format_relative_time(str_of(project, "createdAt"))
                ))
            }
        }
        "completed" => {
            if let Some(completed_at) = non_empty(project.get("completedAt")) {
                Some(format!(
                    "Done {}",
                    display::format_relative_time(&completed_at)
                ))
            } else {
                Some(format!(
                    "Updated {}",
                    display::format_relative_time(str_of(project, "updatedAt"))
                ))
            }
        }
        "canceled" => {
            if let Some(canceled_at) = non_empty(project.get("canceledAt")) {
                Some(format!(
                    "Canceled {}",
                    display::format_relative_time(&canceled_at)
                ))
            } else {
                Some(format!(
                    "Updated {}",
                    display::format_relative_time(str_of(project, "updatedAt"))
                ))
            }
        }
        "planned" => {
            if let Some(start_date) = non_empty(project.get("startDate")) {
                Some(format!("Start: {start_date}"))
            } else if let Some(target_date) = non_empty(project.get("targetDate")) {
                Some(format!("Target: {target_date}"))
            } else {
                Some(format!(
                    "Created {}",
                    display::format_relative_time(str_of(project, "createdAt"))
                ))
            }
        }
        "backlog" | "paused" => Some(format!(
            "Updated {}",
            display::format_relative_time(str_of(project, "updatedAt"))
        )),
        other => {
            return Err(CliError::cli(format!(
                "Linear returned an unknown project status type: {other}"
            ))
            .suggestion("Update the CLI, or report this if it persists."));
        }
    };
    Ok(date.unwrap_or_default())
}

fn health_of(project: &Value) -> String {
    non_empty(project.get("health")).unwrap_or_else(|| "Unknown".to_string())
}

fn lead_of(project: &Value) -> String {
    non_empty(project.pointer("/lead/initials")).unwrap_or_else(|| "-".to_string())
}

fn teams_of(project: &Value) -> String {
    let teams = project
        .pointer("/teams/nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| node.get("key").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    if teams.is_empty() {
        "-".to_string()
    } else {
        teams
    }
}

fn priority_of(project: &Value) -> i64 {
    project
        .get("priority")
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

fn non_empty(value: Option<&Value>) -> Option<String> {
    match value.and_then(Value::as_str) {
        Some(text) if !text.is_empty() => Some(text.to_string()),
        _ => None,
    }
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}
