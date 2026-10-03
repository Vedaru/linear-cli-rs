//! `linear sprint velocity` — what the last N cycles actually finished.
//!
//! Each cycle is read the same way `sprint status` reads one, so the per-cycle figures here are the
//! same figures that command prints. The average is over the cycles that have *ended*: including
//! the cycle in flight would report a velocity nobody achieved, which is the number a team would
//! plan the next cycle with.

use clap::Args;
use serde_json::{json, Value};

use super::{cycle_label, is_canceled, is_completed, short_date, By};
use crate::errors::{CliError, Result};
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct SprintVelocityArgs {
    /// Team key, name, or ID (defaults to the configured team)
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// How many cycles back to read (most recent first)
    #[arg(long, default_value_t = 6, value_name = "n")]
    pub cycles: usize,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: SprintVelocityArgs) -> Result<()> {
    let reference = match args.team.as_deref() {
        Some(team) => team.to_string(),
        None => linear::get_team_key()?.ok_or_else(|| {
            CliError::validation("Could not determine team key from directory name or team flag")
        })?,
    };
    let team = linear::resolve_team(&reference)?;
    let (team_node, cycles) = linear::get_team_cycle_windows(&team.id)?;
    if team_node.get("cyclesEnabled").and_then(Value::as_bool) == Some(false) {
        return Err(CliError::validation(format!(
            "Cycles are not enabled for team {}",
            team.key
        )));
    }
    if args.cycles == 0 {
        return Err(CliError::validation("--cycles must be at least 1"));
    }

    // Most recent first, and never a cycle that has not started: a future cycle's velocity is zero
    // by construction and would drag the average down for no reason.
    let mut ordered: Vec<&Value> = cycles
        .iter()
        .filter(|cycle| cycle.get("isFuture").and_then(Value::as_bool) != Some(true))
        .collect();
    ordered.sort_by_key(|cycle| {
        std::cmp::Reverse(
            cycle
                .get("startsAt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        )
    });
    // At most 24 and at most what the team has; each one is a request.
    let wanted = args.cycles.min(24);
    ordered.truncate(wanted);

    if ordered.is_empty() {
        return Err(CliError::validation(format!(
            "Team {} has no cycles that have started",
            team.key
        )));
    }

    // One weighting for the whole report, so a cycle that estimated nothing is not silently a
    // different unit from its neighbours.
    let mut fetched: Vec<(Value, Vec<Value>)> = Vec::new();
    for cycle in &ordered {
        let Some(id) = cycle.get("id").and_then(Value::as_str) else {
            continue;
        };
        let issues = super::cycle_issues(&team, id)?;
        fetched.push(((*cycle).clone(), issues));
    }
    let all: Vec<Value> = fetched
        .iter()
        .flat_map(|(_, issues)| issues.iter().cloned())
        .collect();
    let by = By::detect(&all);

    let mut report: Vec<Value> = Vec::new();
    let mut ended_work = 0i64;
    let mut ended_cycles = 0i64;
    for (cycle, issues) in &fetched {
        let counted: Vec<Value> = issues
            .iter()
            .filter(|issue| !is_canceled(issue))
            .cloned()
            .collect();
        let completed: Vec<Value> = counted
            .iter()
            .filter(|issue| is_completed(issue))
            .cloned()
            .collect();
        let committed = by.weigh(&counted);
        let done = by.weigh(&completed);
        let ended = cycle.get("isActive").and_then(Value::as_bool) != Some(true);
        if ended {
            ended_work += done;
            ended_cycles += 1;
        }
        report.push(json!({
            "number": cycle.get("number"),
            "name": cycle.get("name"),
            "startsAt": cycle.get("startsAt"),
            "endsAt": cycle.get("endsAt"),
            "active": cycle.get("isActive").and_then(Value::as_bool).unwrap_or(false),
            "committed": committed,
            "completed": done,
            "issuesCompleted": completed.len(),
            "issuesCommitted": counted.len(),
        }));
    }

    let average = if ended_cycles > 0 {
        ((ended_work as f64 / ended_cycles as f64) * 10.0).round() / 10.0
    } else {
        0.0
    };

    if args.json {
        output::print_json(&json!({
            "team": team.key,
            "by": by.name(),
            "cycles": report,
            "average": { "work": average, "over": ended_cycles },
        }));
        return Ok(());
    }

    output::line(&format!(
        "Velocity  ·  team {}  ·  by {}  ·  last {} cycle(s)",
        team.key,
        by.name(),
        report.len()
    ));
    let scale = report
        .iter()
        .map(|entry| entry.get("committed").and_then(Value::as_i64).unwrap_or(0))
        .max()
        .unwrap_or(0)
        .max(1);
    for entry in &report {
        let date = short_date(entry.get("startsAt").and_then(Value::as_str));
        let done = entry.get("completed").and_then(Value::as_i64).unwrap_or(0);
        let committed = entry.get("committed").and_then(Value::as_i64).unwrap_or(0);
        let filled = ((done.max(0) as f64 / scale as f64) * 20.0).round() as usize;
        output::line(&format!(
            "{:<18} {date}  {:<20}  {:>3}/{:<3} {}{}",
            cycle_label(entry),
            "▇".repeat(filled.min(20)),
            done,
            committed,
            by.name(),
            if entry.get("active").and_then(Value::as_bool) == Some(true) {
                "  (in flight)"
            } else {
                ""
            }
        ));
    }
    output::line(&format!(
        "average {average} {} per cycle over {ended_cycles} finished cycle(s)",
        by.name()
    ));
    Ok(())
}
