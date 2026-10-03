//! `linear cycle complete` — close a cycle, the way the app's "Complete cycle" button does.
//!
//! `CycleUpdateInput.completedAt` is the field, and completing is what the CLI lacked entirely -
//! `cycle archive` retires a cycle permanently (there is no unarchive), so it is not a substitute
//! for finishing one. The timestamp sent is the caller's clock, in UTC, at the moment of the call.

use clap::Args;
use serde_json::{json, Value};

use chrono::Utc;

use crate::errors::{CliError, Result};
use crate::{graphql, output};

use super::resolve_cycle_id;

const COMPLETE_CYCLE_MUTATION: &str = r#"
mutation CompleteCycle($id: String!, $input: CycleUpdateInput!) {
  cycleUpdate(id: $id, input: $input) {
    success
    cycle {
      id
      number
      name
      startsAt
      endsAt
      completedAt
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct CompleteCycleArgs {
    /// Cycle UUID, or a cycle number or name (which needs a team)
    #[arg(value_name = "cycleRef")]
    pub cycle_ref: String,
    /// Team key, name, or ID (required for a cycle number or name)
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: CompleteCycleArgs) -> Result<()> {
    let cycle_id = resolve_cycle_id(&args.cycle_ref, args.team.as_deref())?;
    let completed_at = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

    let client = graphql::client()?;
    let document = client.request(
        COMPLETE_CYCLE_MUTATION,
        json!({ "id": cycle_id, "input": { "completedAt": completed_at } }),
    )?;

    let updated = document
        .get("cycleUpdate")
        .ok_or_else(|| CliError::cli("Linear API response did not contain cycleUpdate"))?;
    if updated.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli("Failed to complete cycle"));
    }

    if args.json {
        output::print_json(&document);
        return Ok(());
    }

    let cycle = updated.get("cycle").cloned().unwrap_or(Value::Null);
    let number = cycle
        .get("number")
        .and_then(Value::as_i64)
        .map(|number| format!("#{number}"))
        .unwrap_or_else(|| args.cycle_ref.clone());
    output::line(&format!("✓ Completed cycle {number}"));
    Ok(())
}
