//! `linear sprint carry-over` — move what a cycle did not finish into the next one.
//!
//! The only sprint command that writes, so it is a dry run unless `--apply` and it names every
//! issue it would move: a bulk move nobody could preview is the failure mode this exists to
//! avoid. Only issues that are neither completed nor canceled are candidates — a canceled issue is
//! a decision, not unfinished work.

use clap::Args;
use serde_json::{json, Value};

use super::{cycle_label, identifier_of, is_remaining, state_type, title_of};
use crate::errors::{CliError, Result};
use crate::{graphql, output};

const CARRY_OVER_MUTATION: &str = r#"
mutation CarryOverIssue($id: String!, $input: IssueUpdateInput!) {
  issueUpdate(id: $id, input: $input) {
    success
    issue {
      identifier
      cycle {
        number
      }
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct CarryOverArgs {
    /// Team key, name, or ID (defaults to the configured team)
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Cycle to take unfinished work out of (defaults to the active cycle)
    #[arg(long, value_name = "cycle")]
    pub from: Option<String>,
    /// Cycle to move it into (defaults to the cycle that starts next)
    #[arg(long, value_name = "cycle")]
    pub to: Option<String>,
    /// Write the moves; without it the run is a dry run
    #[arg(long)]
    pub apply: bool,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: CarryOverArgs) -> Result<()> {
    let sprint = super::resolve_sprint(args.team.as_deref(), args.from.as_deref())?;
    let from_id = sprint
        .cycle
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let issues = super::cycle_issues(&sprint.team, &from_id)?;

    let to_cycle = target_cycle(&sprint, args.to.as_deref())?;
    let to_label = cycle_label(&to_cycle);

    let unfinished: Vec<&Value> = issues.iter().filter(|issue| is_remaining(issue)).collect();
    if unfinished.is_empty() {
        if args.json {
            output::print_json(&json!({
                "team": sprint.team.key,
                "from": sprint.cycle,
                "to": to_cycle,
                "applied": args.apply,
                "count": 0,
                "moves": [],
            }));
        } else {
            output::line(&format!(
                "Nothing to carry over: every issue in {} is completed or canceled.",
                cycle_label(&sprint.cycle)
            ));
        }
        return Ok(());
    }

    if !args.apply {
        if args.json {
            output::print_json(&json!({
                "team": sprint.team.key,
                "from": sprint.cycle,
                "to": to_cycle,
                "applied": false,
                "count": unfinished.len(),
                "moves": moves_json(&unfinished),
            }));
            return Ok(());
        }
        for issue in &unfinished {
            output::line(&format!(
                "would move  {}  {}  ({})",
                identifier_of(issue),
                title_of(issue),
                state_type(issue)
            ));
        }
        output::line(&format!(
            "Dry run: {} issue(s) would move from {} to {to_label}. Pass --apply to write them.",
            unfinished.len(),
            cycle_label(&sprint.cycle)
        ));
        return Ok(());
    }

    let to_id = to_cycle
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let client = graphql::client()?;
    for issue in &unfinished {
        let Some(id) = issue.get("id").and_then(Value::as_str) else {
            continue;
        };
        let document = client.request(
            CARRY_OVER_MUTATION,
            json!({ "id": id, "input": { "cycleId": to_id } }),
        )?;
        let updated = document
            .get("issueUpdate")
            .ok_or_else(|| CliError::cli("Linear API response did not contain issueUpdate"))?;
        if updated.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(CliError::cli(format!(
                "Failed to move {} to {to_label}",
                identifier_of(issue)
            )));
        }
        if !args.json {
            output::line(&format!("✓ Moved {} → {to_label}", identifier_of(issue)));
        }
    }

    if args.json {
        output::print_json(&json!({
            "team": sprint.team.key,
            "from": sprint.cycle,
            "to": to_cycle,
            "applied": true,
            "count": unfinished.len(),
            "moves": moves_json(&unfinished),
        }));
        return Ok(());
    }

    output::line(&format!(
        "Applied: {} issue(s) moved to {to_label}.",
        unfinished.len()
    ));
    Ok(())
}

/// The cycle the work goes into: `--to` when given, else the first cycle that starts after this
/// one. A team whose cycles have run out is told so, with the command that makes another.
fn target_cycle(sprint: &super::Sprint, requested: Option<&str>) -> Result<Value> {
    if let Some(reference) = requested {
        let id = crate::commands::cycle::resolve_cycle_id(reference, Some(&sprint.team.key))?;
        return sprint
            .cycles
            .iter()
            .find(|cycle| cycle.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .cloned()
            .ok_or_else(|| CliError::not_found("Cycle", reference));
    }

    let from_start = sprint
        .cycle
        .get("startsAt")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut later: Vec<&Value> = sprint
        .cycles
        .iter()
        .filter(|cycle| {
            cycle
                .get("startsAt")
                .and_then(Value::as_str)
                .map(|starts| starts > from_start.as_str())
                .unwrap_or(false)
        })
        .collect();
    later.sort_by_key(|cycle| {
        cycle
            .get("startsAt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    });

    later.first().map(|cycle| (*cycle).clone()).ok_or_else(|| {
        CliError::validation(format!(
            "Team {} has no cycle after {}",
            sprint.team.key,
            cycle_label(&sprint.cycle)
        ))
        .suggestion("Create the next cycle with `linear cycle create`, or pass --to <cycle>.")
    })
}

fn moves_json(unfinished: &[&Value]) -> Value {
    json!(unfinished
        .iter()
        .map(|issue| json!({
            "identifier": identifier_of(issue),
            "title": title_of(issue),
            "state": state_type(issue),
        }))
        .collect::<Vec<Value>>())
}
