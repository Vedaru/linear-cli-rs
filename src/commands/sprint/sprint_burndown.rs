//! `linear sprint burndown` — remaining work per day across a cycle.
//!
//! Computed from the issues themselves rather than from a snapshot table: each day's remaining is
//! the work created on or before that day minus the work completed on or before it, so scope that
//! arrived mid-cycle shows up as a step in the line instead of being averaged away. Days after
//! today carry `future: true` in the JSON and are marked in the chart.

use chrono::Utc;
use clap::Args;
use serde_json::{json, Value};

use super::{bar, cycle_label, is_canceled, is_completed, timestamp, By};
use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct SprintBurndownArgs {
    /// Team key, name, or ID (defaults to the configured team)
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Cycle number, name, `active`, `next`, or a UUID (defaults to the active cycle)
    #[arg(long, value_name = "cycle")]
    pub cycle: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: SprintBurndownArgs) -> Result<()> {
    let sprint = super::resolve_sprint(args.team.as_deref(), args.cycle.as_deref())?;
    let cycle_id = sprint
        .cycle
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let issues = super::cycle_issues(&sprint.team, &cycle_id)?;
    let counted: Vec<Value> = issues
        .iter()
        .filter(|issue| !is_canceled(issue))
        .cloned()
        .collect();
    let by = By::detect(&counted);

    let days = super::cycle_days(&sprint.cycle);
    if days.is_empty() {
        return Err(crate::errors::CliError::validation(format!(
            "Cycle {} has no start and end date",
            cycle_label(&sprint.cycle)
        )));
    }

    let today = Utc::now().date_naive();
    let total = by.weigh(&counted);

    let mut points: Vec<Value> = Vec::new();
    for day in &days {
        let scope: Vec<Value> = counted
            .iter()
            .filter(|issue| {
                timestamp(issue.get("createdAt").and_then(Value::as_str))
                    .map(|created| created.date_naive() <= *day)
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        let done: Vec<Value> = scope
            .iter()
            .filter(|issue| {
                is_completed(issue)
                    && timestamp(issue.get("completedAt").and_then(Value::as_str))
                        .map(|completed| completed.date_naive() <= *day)
                        .unwrap_or(false)
            })
            .cloned()
            .collect();
        points.push(json!({
            "date": day.to_string(),
            "scope": by.weigh(&scope),
            "done": by.weigh(&done),
            "remaining": by.weigh(&scope) - by.weigh(&done),
            "future": *day > today,
        }));
    }

    if args.json {
        output::print_json(&json!({
            "team": sprint.team.key,
            "cycle": sprint.cycle,
            "by": by.name(),
            "total": total,
            "days": points,
        }));
        return Ok(());
    }

    output::line(&format!(
        "Burndown {}  ·  team {}  ·  by {}",
        cycle_label(&sprint.cycle),
        sprint.team.key,
        by.name()
    ));
    let scale = points
        .iter()
        .map(|point| point.get("remaining").and_then(Value::as_i64).unwrap_or(0))
        .max()
        .unwrap_or(0)
        .max(1);
    for point in &points {
        let date = point.get("date").and_then(Value::as_str).unwrap_or("");
        let remaining = point.get("remaining").and_then(Value::as_i64).unwrap_or(0);
        let future = point
            .get("future")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // The bar is drawn against the day's own scope and inverted (full = nothing done), which is
        // what makes a burndown readable at a glance: it should fall.
        let filled = ((remaining.max(0) as f64 / scale as f64) * 20.0).round() as usize;
        output::line(&format!(
            "{date}  {:<20}  {:>3}{}",
            "▇".repeat(filled.min(20)),
            remaining,
            if future { "  (not yet)" } else { "" }
        ));
    }

    let last_done = points
        .iter()
        .rfind(|point| point.get("future").and_then(Value::as_bool) == Some(false))
        .cloned()
        .unwrap_or(Value::Null);
    let remaining = last_done
        .get("remaining")
        .and_then(Value::as_i64)
        .unwrap_or(total);
    output::line(&format!(
        "{}  {}/{} {} done  ·  {} {} left",
        bar(total - remaining, total, 14),
        total - remaining,
        total,
        by.name(),
        remaining,
        by.name()
    ));
    Ok(())
}
