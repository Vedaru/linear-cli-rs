//! `linear sprint` — the numbers a cycle is managed by.
//!
//! Upstream ships these five as ASCII charts; here the numbers are the product and the chart is a
//! rendering of them, so every command answers `--json` with the figures it prints. `carry-over`
//! is the only one that writes (it moves work into the next cycle), which is why it is a dry run
//! unless `--apply` and reports what it would move per issue.
//!
//! One source for every figure: the team's cycles come from `linear::get_team_cycle_windows` and
//! the issues from `linear::fetch_export_issues` filtered by `cycle.id`. Nothing here recomputes
//! a window or a state that the API already answered, which is what makes "correct against the
//! data the app shows" a property of the data rather than of five separate implementations.

pub mod sprint_burndown;
pub mod sprint_carry_over;
pub mod sprint_progress;
pub mod sprint_status;
pub mod sprint_velocity;

use chrono::{DateTime, NaiveDate, Utc};
use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct SprintArgs {
    #[command(subcommand)]
    pub command: Option<SprintCommand>,
}

#[derive(Subcommand, Debug)]
pub enum SprintCommand {
    /// Where a cycle stands: dates, counts, points
    Status(sprint_status::SprintStatusArgs),
    /// How much of a cycle is done, and how much scope arrived after it started
    Progress(sprint_progress::SprintProgressArgs),
    /// Move a cycle's unfinished issues into the next one (dry run unless --apply)
    #[command(name = "carry-over")]
    CarryOver(sprint_carry_over::CarryOverArgs),
    /// Remaining work per day across a cycle
    Burndown(sprint_burndown::SprintBurndownArgs),
    /// Completed work per cycle over the last N cycles
    Velocity(sprint_velocity::SprintVelocityArgs),
}

pub fn run(args: SprintArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <SprintArgs as clap::Args>::augment_args(clap::Command::new("sprint"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        SprintCommand::Status(args) => sprint_status::run(args)
            .map_err(|error| error.with_context("Failed to read the sprint")),
        SprintCommand::Progress(args) => sprint_progress::run(args)
            .map_err(|error| error.with_context("Failed to read the sprint's progress")),
        SprintCommand::CarryOver(args) => sprint_carry_over::run(args)
            .map_err(|error| error.with_context("Failed to carry work over")),
        SprintCommand::Burndown(args) => sprint_burndown::run(args)
            .map_err(|error| error.with_context("Failed to compute the burndown")),
        SprintCommand::Velocity(args) => sprint_velocity::run(args)
            .map_err(|error| error.with_context("Failed to compute velocity")),
    }
}

/// A cycle, the team it belongs to, and every cycle that team has.
pub(crate) struct Sprint {
    pub team: linear::ResolvedTeam,
    pub cycle: Value,
    pub cycles: Vec<Value>,
}

/// Resolve the team and the cycle a sprint command is about.
///
/// The cycle is the `--cycle` reference when one is given (a number, name, `active`, `next`, or a
/// UUID - the same resolver the cycle commands use) and the team's active cycle otherwise. A team
/// with cycles switched off is refused, because every figure below would be zero for a reason
/// nobody could see.
pub(crate) fn resolve_sprint(team: Option<&str>, cycle: Option<&str>) -> Result<Sprint> {
    let reference = match team {
        Some(team) => team.to_string(),
        None => linear::get_team_key()?.ok_or_else(|| {
            CliError::validation("Could not determine team key from directory name or team flag")
        })?,
    };
    let team = linear::resolve_team(&reference)?;

    let (team_node, cycles) = linear::get_team_cycle_windows(&team.id)?;
    if team_node.get("cyclesEnabled").and_then(Value::as_bool) == Some(false) {
        return Err(
            CliError::validation(format!("Cycles are not enabled for team {}", team.key))
                .suggestion(
                    "Enable cycles for the team in Linear's settings before asking about sprints.",
                ),
        );
    }

    let cycle_node = match cycle {
        Some(reference) => {
            let id = crate::commands::cycle::resolve_cycle_id(reference, Some(&team.key))?;
            cycles
                .iter()
                .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(id.as_str()))
                .cloned()
                .ok_or_else(|| CliError::not_found("Cycle", reference))?
        }
        None => cycles
            .iter()
            .find(|candidate| candidate.get("isActive").and_then(Value::as_bool) == Some(true))
            .cloned()
            .ok_or_else(|| {
                CliError::validation(format!("Team {} has no active cycle", team.key)).suggestion(
                    "Pass --cycle <number|name|next> to ask about a specific one, or `linear cycle list`.",
                )
            })?,
    };

    Ok(Sprint {
        team,
        cycle: cycle_node,
        cycles,
    })
}

/// The issues in one cycle, as the API answers them.
pub(crate) fn cycle_issues(team: &linear::ResolvedTeam, cycle_id: &str) -> Result<Vec<Value>> {
    let options = linear::FetchIssuesForQueryOptions {
        team_keys: Some(vec![team.key.clone()]),
        all_teams: false,
        state: None,
        assignee: None,
        unassigned: false,
        sort: None,
        limit: Some(0),
        project_id: None,
        project_label: None,
        cycle_id: Some(cycle_id.to_string()),
        milestone_id: None,
        label_names: None,
        created_after: None,
        updated_after: None,
        include_archived: Some(true),
        raw_filter: None,
    };
    let document = linear::fetch_export_issues(&options)?;
    Ok(document
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// What a figure counts: points when the cycle estimates its work, issues otherwise.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum By {
    Points,
    Issues,
}

impl By {
    /// Points when any issue in the set carries an estimate, issues otherwise - so a team that
    /// does not estimate still gets a burndown rather than a flat line of zeros.
    pub(crate) fn detect(issues: &[Value]) -> By {
        if issues.iter().any(|issue| estimate_of(issue) > 0) {
            By::Points
        } else {
            By::Issues
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            By::Points => "points",
            By::Issues => "issues",
        }
    }

    pub(crate) fn weigh(self, issues: &[Value]) -> i64 {
        match self {
            By::Points => issues.iter().map(estimate_of).sum(),
            By::Issues => issues.len() as i64,
        }
    }
}

pub(crate) fn estimate_of(issue: &Value) -> i64 {
    issue.get("estimate").and_then(Value::as_i64).unwrap_or(0)
}

pub(crate) fn state_type(issue: &Value) -> String {
    issue
        .pointer("/state/type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(crate) fn is_completed(issue: &Value) -> bool {
    state_type(issue) == "completed"
}

pub(crate) fn is_canceled(issue: &Value) -> bool {
    state_type(issue) == "canceled" || state_type(issue) == "duplicate"
}

/// Neither finished nor dropped: the work a carry-over moves.
pub(crate) fn is_remaining(issue: &Value) -> bool {
    !is_completed(issue) && !is_canceled(issue)
}

pub(crate) fn identifier_of(issue: &Value) -> String {
    issue
        .get("identifier")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(crate) fn title_of(issue: &Value) -> String {
    issue
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// `#8 (Sprint 8)`, or `#8` when the cycle has no name.
pub(crate) fn cycle_label(cycle: &Value) -> String {
    let number = cycle.get("number").and_then(Value::as_i64);
    let name = cycle.get("name").and_then(Value::as_str).unwrap_or("");
    match (number, name.is_empty()) {
        (Some(number), false) => format!("#{number} ({name})"),
        (Some(number), true) => format!("#{number}"),
        (None, false) => name.to_string(),
        (None, true) => "?".to_string(),
    }
}

pub(crate) fn timestamp(value: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

pub(crate) fn date_of(value: Option<&str>) -> Option<NaiveDate> {
    let text = value?;
    let head = text.get(..10)?;
    NaiveDate::parse_from_str(head, "%Y-%m-%d").ok()
}

pub(crate) fn short_date(value: Option<&str>) -> String {
    value.unwrap_or("").chars().take(10).collect()
}

/// A text progress bar: `[██████░░░░]`, scaled to `width` cells.
pub(crate) fn bar(done: i64, total: i64, width: usize) -> String {
    let filled = if total <= 0 {
        0
    } else {
        ((done.max(0) as f64 / total as f64) * width as f64).round() as usize
    };
    let filled = filled.min(width);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(width - filled))
}

/// The days of a cycle's window, in order.
pub(crate) fn cycle_days(cycle: &Value) -> Vec<NaiveDate> {
    let Some(start) = date_of(cycle.get("startsAt").and_then(Value::as_str)) else {
        return Vec::new();
    };
    let Some(end) = date_of(cycle.get("endsAt").and_then(Value::as_str)) else {
        return Vec::new();
    };
    let mut days = Vec::new();
    let mut day = start;
    while day <= end {
        days.push(day);
        day = day.succ_opt().unwrap_or(end);
        if day == end && days.last() == Some(&end) {
            break;
        }
    }
    days
}

/// The state-type counts an issue set falls into, in lifecycle order.
pub(crate) fn counts_by_type(issues: &[Value]) -> Value {
    let mut counts: Vec<(&str, i64)> = Vec::new();
    for kind in [
        "triage",
        "backlog",
        "unstarted",
        "started",
        "completed",
        "canceled",
    ] {
        let count = issues
            .iter()
            .filter(|issue| state_type(issue) == kind)
            .count() as i64;
        if count > 0 {
            counts.push((kind, count));
        }
    }
    json!(counts
        .into_iter()
        .map(|(kind, count)| json!({ "type": kind, "count": count }))
        .collect::<Vec<Value>>())
}
