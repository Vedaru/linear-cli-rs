//! `linear sprint status` — where a cycle stands, in one screen.

use chrono::Utc;
use clap::Args;
use serde_json::{json, Value};

use super::{bar, counts_by_type, cycle_label, is_canceled, is_completed, short_date, By, Sprint};
use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct SprintStatusArgs {
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

pub fn run(args: SprintStatusArgs) -> Result<()> {
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
    let canceled: Vec<Value> = issues
        .iter()
        .filter(|issue| is_canceled(issue))
        .cloned()
        .collect();
    let remaining: Vec<Value> = issues
        .iter()
        .filter(|issue| !is_completed(issue) && !is_canceled(issue))
        .cloned()
        .collect();

    let total_work = by.weigh(&remaining) + by.weigh(&completed);
    let done_work = by.weigh(&completed);
    let days = days_of(&sprint);

    if args.json {
        output::print_json(&json!({
            "team": sprint.team.key,
            "cycle": sprint.cycle,
            "by": by.name(),
            "days": days,
            "issues": {
                "total": issues.len() - canceled.len(),
                "completed": completed.len(),
                "remaining": remaining.len(),
                "canceled": canceled.len(),
            },
            "counts": counts_by_type(&issues),
            "work": { "total": total_work, "completed": done_work, "remaining": total_work - done_work },
        }));
        return Ok(());
    }

    output::line(&format!(
        "Sprint {}  ·  team {}",
        cycle_label(&sprint.cycle),
        sprint.team.key
    ));
    output::line(&format!(
        "{} → {}  ·  {}",
        short_date(sprint.cycle.get("startsAt").and_then(Value::as_str)),
        short_date(sprint.cycle.get("endsAt").and_then(Value::as_str)),
        day_sentence(&days)
    ));
    output::line(&format!(
        "{}  {}/{} {} ({})  ·  {}/{} issues",
        bar(done_work, total_work, 14),
        done_work,
        total_work,
        by.name(),
        percent(done_work, total_work),
        completed.len(),
        issues.len() - canceled.len()
    ));

    let counts: Vec<String> = counts_by_type(&issues)
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    Some(format!(
                        "{} {}",
                        entry.get("count")?.as_i64()?,
                        entry.get("type")?.as_str()?
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    if !counts.is_empty() {
        output::line(&format!("  {}", counts.join(" · ")));
    }
    Ok(())
}

/// `{total, elapsed, remaining}`, in days, as of today.
fn days_of(sprint: &Sprint) -> Value {
    let start = super::date_of(sprint.cycle.get("startsAt").and_then(Value::as_str));
    let end = super::date_of(sprint.cycle.get("endsAt").and_then(Value::as_str));
    let today = Utc::now().date_naive();
    match (start, end) {
        (Some(start), Some(end)) => {
            let total = (end - start).num_days() + 1;
            let elapsed = (today - start).num_days() + 1;
            let remaining = (end - today).num_days();
            json!({
                "total": total,
                "elapsed": elapsed.clamp(0, total),
                "remaining": remaining.max(0),
                "started": today >= start,
                "ended": today > end,
            })
        }
        _ => json!(null),
    }
}

fn day_sentence(days: &Value) -> String {
    let total = days.get("total").and_then(Value::as_i64).unwrap_or(0);
    if days.is_null() {
        return "no window".to_string();
    }
    if days.get("ended").and_then(Value::as_bool) == Some(true) {
        return format!("ended · {total} day(s)");
    }
    if days.get("started").and_then(Value::as_bool) == Some(false) {
        return format!("starts today · {total} day(s)");
    }
    format!(
        "day {} of {} · {} day(s) left",
        days.get("elapsed").and_then(Value::as_i64).unwrap_or(0),
        total,
        days.get("remaining").and_then(Value::as_i64).unwrap_or(0)
    )
}

fn percent(done: i64, total: i64) -> String {
    if total <= 0 {
        return "0%".to_string();
    }
    format!("{}%", ((done as f64 / total as f64) * 100.0).round() as i64)
}
