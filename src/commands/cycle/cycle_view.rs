//! `linear cycle view` — port of `src/commands/cycle/cycle-view.ts`.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::linear_url::{expect_linear_url_kind, LinearUrlRef};
use crate::{display, linear, output};

const GET_CYCLE_DETAILS_QUERY: &str = r#"
query GetCycleDetails($id: String!) {
  cycle(id: $id) {
    id
    number
    name
    description
    startsAt
    endsAt
    completedAt
    isActive
    isFuture
    isPast
    createdAt
    updatedAt
    team {
      id
      key
      name
    }
    issues {
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
    /// Cycle URL, number, or name
    #[arg(value_name = "cycleRef")]
    pub cycle_ref: String,
    /// Team key, name, or ID (defaults to current team)
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ViewArgs) -> Result<()> {
    // A pasted cycle URL names its team. With no --team, that is the team
    // meant — not whichever one happens to be configured. An explicit --team
    // still wins, and the lookup refuses it if it contradicts the URL.
    let url_ref = expect_linear_url_kind(
        &args.cycle_ref,
        "cycle",
        "a cycle URL, number, or name",
    )?;
    let url_team_key = url_ref.as_ref().and_then(|reference| match reference {
        LinearUrlRef::Cycle { team_key, .. } => Some(team_key.clone()),
        _ => None,
    });

    let team_key = match args.team.or(url_team_key) {
        Some(team_key) => Some(team_key),
        None => linear::get_team_key()?,
    }
    .ok_or_else(|| {
            CliError::validation("Could not determine team key from directory name or team flag")
        })?;

    let team_id = linear::resolve_team(&team_key)?.id;
    let cycle_id = linear::get_cycle_id_by_name_or_number(&team_id, &args.cycle_ref)?;

    let client = crate::graphql::client()?;
    let result = client.request(GET_CYCLE_DETAILS_QUERY, json!({ "id": cycle_id }))?;

    let Some(cycle) = result.get("cycle").filter(|value| !value.is_null()) else {
        return Err(CliError::not_found("Cycle", &args.cycle_ref));
    };

    if args.json {
        output::print_json(cycle);
        return Ok(());
    }

    let mut lines: Vec<String> = Vec::new();

    let title = match cycle.get("name").and_then(Value::as_str) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => format!("Cycle {}", cycle_number(cycle)),
    };
    lines.push(format!("# {title}"));
    lines.push(String::new());

    lines.push(format!("**Number:** {}", cycle_number(cycle)));
    lines.push(format!(
        "**Start:** {}",
        date_prefix(string_field(cycle, "startsAt").unwrap_or(""))
    ));
    lines.push(format!(
        "**End:** {}",
        date_prefix(string_field(cycle, "endsAt").unwrap_or(""))
    ));

    let mut status = "Unknown";
    if bool_field(cycle, "isActive") {
        status = "Active";
    } else if bool_field(cycle, "isFuture") {
        status = "Upcoming";
    } else if completed(cycle) {
        status = "Completed";
    } else if bool_field(cycle, "isPast") {
        status = "Past";
    }
    lines.push(format!("**Status:** {status}"));

    let team = cycle.get("team");
    lines.push(format!(
        "**Team:** {} ({})",
        team.and_then(|team| team.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(""),
        team.and_then(|team| team.get("key"))
            .and_then(Value::as_str)
            .unwrap_or(""),
    ));

    lines.push(String::new());
    lines.push(format!(
        "**Created:** {}",
        display::format_relative_time(string_field(cycle, "createdAt").unwrap_or(""))
    ));
    lines.push(format!(
        "**Updated:** {}",
        display::format_relative_time(string_field(cycle, "updatedAt").unwrap_or(""))
    ));

    if let Some(description) = string_field(cycle, "description").filter(|text| !text.is_empty()) {
        lines.push(String::new());
        lines.push("## Description".to_string());
        lines.push(String::new());
        lines.push(description.to_string());
    }

    let issues = cycle
        .get("issues")
        .and_then(|issues| issues.get("nodes"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    if !issues.is_empty() {
        lines.push(String::new());
        lines.push("## Issues".to_string());
        lines.push(String::new());

        let mut by_state = std::collections::HashMap::new();
        for issue in issues {
            if let Some(state_type) = issue
                .get("state")
                .and_then(|state| state.get("type"))
                .and_then(Value::as_str)
            {
                *by_state.entry(state_type.to_string()).or_insert(0i64) += 1;
            }
        }
        let count = |key: &str| *by_state.get(key).unwrap_or(&0);

        let total = issues.len() as i64;
        let completed = count("completed");
        let started = count("started");
        let unstarted = count("unstarted");
        let canceled = count("canceled");
        let backlog = count("backlog");
        let triage = count("triage");

        let pct = (completed as f64 / total as f64 * 100.0).round() as i64;
        lines.push(format!("**Progress:** {completed}/{total} ({pct}%)"));
        lines.push(format!("**Total Issues:** {total}"));
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
        lines.push("**Issues:**".to_string());
        lines.push(String::new());
        for issue in issues.iter().take(10) {
            let identifier = string_field(issue, "identifier").unwrap_or("");
            let issue_title = string_field(issue, "title").unwrap_or("");
            let state_name = issue
                .get("state")
                .and_then(|state| state.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            lines.push(format!("- {identifier}: {issue_title} ({state_name})"));
        }

        if issues.len() > 10 {
            lines.push(String::new());
            lines.push(format!("_...and {} more issues_", issues.len() - 10));
        }
    } else {
        lines.push(String::new());
        lines.push("_No issues in this cycle yet._".to_string());
    }

    // Upstream renders markdown on a terminal via `@littletof/charmd`; this
    // port has no renderer, so it always emits the raw markdown, matching
    // upstream's non-terminal path.
    output::line(&lines.join("\n"));

    Ok(())
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
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

fn date_prefix(date: &str) -> String {
    date.chars().take(10).collect()
}
