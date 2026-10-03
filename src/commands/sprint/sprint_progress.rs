//! `linear sprint progress` — how much of a cycle is done, and how much arrived after it started.
//!
//! The second number is the one a cycle review turns on: an issue created after the cycle began is
//! scope nobody planned for, and a completion rate quoted without it flatters a sprint that grew.

use clap::Args;
use serde_json::{json, Value};

use super::{bar, cycle_label, identifier_of, is_canceled, is_completed, By};
use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct SprintProgressArgs {
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

pub fn run(args: SprintProgressArgs) -> Result<()> {
    let sprint = super::resolve_sprint(args.team.as_deref(), args.cycle.as_deref())?;
    let cycle_id = sprint
        .cycle
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let issues = super::cycle_issues(&sprint.team, &cycle_id)?;
    let by = By::detect(&issues);

    let completed: Vec<Value> = issues
        .iter()
        .filter(|issue| is_completed(issue))
        .cloned()
        .collect();
    let counted: Vec<Value> = issues
        .iter()
        .filter(|issue| !is_canceled(issue))
        .cloned()
        .collect();

    let total_work = by.weigh(&counted);
    let done_work = by.weigh(&completed);

    // Scope is "arrived after the cycle started" by the issue's own creation time, not by when it
    // was attached to the cycle - Linear records the latter only as history, which the API does not
    // expose per issue.
    let started_at = super::timestamp(sprint.cycle.get("startsAt").and_then(Value::as_str));
    let added: Vec<&Value> = match started_at {
        Some(started_at) => issues
            .iter()
            .filter(|issue| {
                super::timestamp(issue.get("createdAt").and_then(Value::as_str))
                    .map(|created| created > started_at)
                    .unwrap_or(false)
            })
            .collect(),
        None => Vec::new(),
    };

    if args.json {
        output::print_json(&json!({
            "team": sprint.team.key,
            "cycle": sprint.cycle,
            "by": by.name(),
            "issues": {
                "completed": completed.len(),
                "total": counted.len(),
                "percent": ratio(completed.len() as i64, counted.len() as i64),
            },
            "work": { "completed": done_work, "total": total_work, "percent": ratio(done_work, total_work) },
            "addedAfterStart": {
                "count": added.len(),
                "identifiers": added.iter().map(|issue| identifier_of(issue)).collect::<Vec<String>>(),
            },
        }));
        return Ok(());
    }

    output::line(&format!(
        "Sprint {}  ·  team {}",
        cycle_label(&sprint.cycle),
        sprint.team.key
    ));
    output::line(&format!(
        "{}  {}/{} {}  ·  {}/{} issues",
        bar(done_work, total_work, 14),
        done_work,
        total_work,
        by.name(),
        completed.len(),
        counted.len()
    ));
    output::line(&format!(
        "  done: {} of {} {}  ·  {} of {} issues",
        done_work,
        total_work,
        by.name(),
        completed.len(),
        counted.len()
    ));
    output::line(&format!(
        "  added after the cycle started: {}{}",
        added.len(),
        if added.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                added
                    .iter()
                    .map(|issue| identifier_of(issue))
                    .collect::<Vec<String>>()
                    .join(", ")
            )
        }
    ));
    Ok(())
}

/// A percentage carried as a number, so a caller can average it without parsing a `%`.
fn ratio(done: i64, total: i64) -> f64 {
    if total <= 0 {
        return 0.0;
    }
    ((done as f64 / total as f64) * 1000.0).round() / 10.0
}
