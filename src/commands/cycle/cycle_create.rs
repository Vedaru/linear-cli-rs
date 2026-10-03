//! `linear cycle create` — the API's `cycleCreate`. Upstream's cycle group only reads, so a
//! cycle could not be started from the CLI at all.
//!
//! The window is checked against the team's existing cycles *before* the request, because the
//! API's own answer to an overlap is a generic failure that names nothing: refusing here says
//! which cycle is in the way and when it runs, which is the difference between a caller fixing
//! the command and a caller opening the app.

use clap::Args;
use serde_json::{json, Map, Value};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output};

const CREATE_CYCLE_MUTATION: &str = r#"
mutation CreateCycle($input: CycleCreateInput!) {
  cycleCreate(input: $input) {
    success
    cycle {
      id
      number
      name
      startsAt
      endsAt
      isActive
      isFuture
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct CreateCycleArgs {
    /// Team key, name, or ID (defaults to the configured team)
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Cycle start date (YYYY-MM-DD or ISO 8601)
    #[arg(long = "start-date", value_name = "date")]
    pub start_date: String,
    /// Cycle end date (YYYY-MM-DD or ISO 8601)
    #[arg(long = "end-date", value_name = "date")]
    pub end_date: String,
    /// Cycle name
    #[arg(short = 'n', long, value_name = "name")]
    pub name: Option<String>,
    /// Cycle description
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: CreateCycleArgs) -> Result<()> {
    let Some(start) = instant(&args.start_date) else {
        return Err(CliError::validation(format!(
            "--start-date is not a date: {}",
            args.start_date
        ))
        .suggestion("Use YYYY-MM-DD, or an ISO 8601 timestamp."));
    };
    let Some(end) = instant(&args.end_date) else {
        return Err(
            CliError::validation(format!("--end-date is not a date: {}", args.end_date))
                .suggestion("Use YYYY-MM-DD, or an ISO 8601 timestamp."),
        );
    };
    if end <= start {
        return Err(
            CliError::validation("The cycle's end must be after its start")
                .suggestion("Swap the two dates, or widen the window."),
        );
    }

    let team_reference = match &args.team {
        Some(team) => team.clone(),
        None => linear::get_team_key()?.ok_or_else(|| {
            CliError::validation("Could not determine team key from directory name or team flag")
        })?,
    };
    let team = linear::resolve_team(&team_reference)?;
    let (team_node, cycles) = linear::get_team_cycle_windows(&team.id)?;

    if team_node.get("cyclesEnabled").and_then(Value::as_bool) == Some(false) {
        return Err(
            CliError::validation(format!("Cycles are not enabled for team {}", team.key))
                .suggestion("Enable cycles for the team in Linear's settings before creating one."),
        );
    }

    if let Some(clash) = cycles.iter().find(|cycle| overlaps(cycle, start, end)) {
        let label = cycle_label(clash);
        let window = format!(
            "{} → {}",
            short_date(clash.get("startsAt").and_then(Value::as_str)),
            short_date(clash.get("endsAt").and_then(Value::as_str))
        );
        return Err(CliError::validation(format!(
            "The window {} → {} overlaps cycle {label} ({window})",
            args.start_date, args.end_date
        ))
        .suggestion(
            "Pick a window that does not overlap it, or move the existing cycle with `linear cycle update`.",
        ));
    }

    let mut input = Map::new();
    input.insert("teamId".to_string(), json!(team.id));
    input.insert("startsAt".to_string(), json!(args.start_date));
    input.insert("endsAt".to_string(), json!(args.end_date));
    if let Some(name) = &args.name {
        input.insert("name".to_string(), json!(name));
    }
    if let Some(description) = &args.description {
        input.insert("description".to_string(), json!(description));
    }

    let client = graphql::client()?;
    let document = client.request(
        CREATE_CYCLE_MUTATION,
        json!({ "input": Value::Object(input) }),
    )?;
    let created = document
        .get("cycleCreate")
        .ok_or_else(|| CliError::cli("Linear API response did not contain cycleCreate"))?;
    if created.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli("Failed to create cycle"));
    }

    if args.json {
        output::print_json(&document);
        return Ok(());
    }

    let cycle = created.get("cycle").cloned().unwrap_or(Value::Null);
    output::line(&format!(
        "✓ Created cycle {} in team {} ({} → {})",
        cycle_label(&cycle),
        team.key,
        short_date(cycle.get("startsAt").and_then(Value::as_str)),
        short_date(cycle.get("endsAt").and_then(Value::as_str))
    ));
    Ok(())
}

/// Whether a team cycle's window and the requested one share any time at all. A cycle whose dates
/// the API leaves out cannot be judged, so it is skipped rather than guessed at.
fn overlaps(cycle: &Value, start: DateTime<Utc>, end: DateTime<Utc>) -> bool {
    let (Some(cycle_start), Some(cycle_end)) = (
        cycle
            .get("startsAt")
            .and_then(Value::as_str)
            .and_then(instant),
        cycle
            .get("endsAt")
            .and_then(Value::as_str)
            .and_then(instant),
    ) else {
        return false;
    };
    start < cycle_end && cycle_start < end
}

/// `number`, else the name, else "?" - the same label `cycle list` prints.
fn cycle_label(cycle: &Value) -> String {
    match cycle.get("number").and_then(Value::as_i64) {
        Some(number) => match cycle.get("name").and_then(Value::as_str) {
            Some(name) if !name.is_empty() => format!("#{number} ({name})"),
            _ => format!("#{number}"),
        },
        None => cycle
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string(),
    }
}

/// The first ten characters of a timestamp: a cycle's window is quoted in days.
fn short_date(value: Option<&str>) -> String {
    value.unwrap_or("").chars().take(10).collect()
}

/// An ISO 8601 timestamp, or a bare date read as midnight UTC.
///
/// Timezone-less values are UTC for the same reason `parse_date_filter` treats them as UTC: a
/// window check that moved with the host's zone would refuse a valid cycle on one machine and
/// accept it on another.
fn instant(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    let date = value.get(..10)?;
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Some(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?))
}
