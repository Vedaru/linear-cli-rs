//! `linear cycle update` — the API's `cycleUpdate`. Upstream's cycle group only
//! reads (list, view), so a cycle's name, description, or dates could not be
//! corrected from the CLI at all.

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, output};

use super::resolve_cycle_id;

const UPDATE_CYCLE_MUTATION: &str = r#"
mutation UpdateCycle($id: String!, $input: CycleUpdateInput!) {
  cycleUpdate(id: $id, input: $input) {
    success
    cycle {
      id
      number
      name
      startsAt
      endsAt
    }
  }
}
"#;

/// Update a cycle
#[derive(Args, Debug)]
pub struct CycleUpdateArgs {
    /// Cycle UUID, or a cycle number or name (which needs a team)
    #[arg(value_name = "cycleRef")]
    pub cycle_ref: String,
    /// New cycle name
    #[arg(short = 'n', long, value_name = "name")]
    pub name: Option<String>,
    /// New description
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// New start date (YYYY-MM-DD or ISO 8601)
    #[arg(long = "start-date", value_name = "date")]
    pub start_date: Option<String>,
    /// New end date (YYYY-MM-DD or ISO 8601)
    #[arg(long = "end-date", value_name = "date")]
    pub end_date: Option<String>,
    /// Team key, name, or ID (required for a cycle number or name)
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
}

pub fn run(args: CycleUpdateArgs) -> Result<()> {
    let mut input = Map::new();
    if let Some(name) = &args.name {
        input.insert("name".to_string(), json!(name));
    }
    if let Some(description) = &args.description {
        input.insert("description".to_string(), json!(description));
    }
    // Linear's `startsAt` / `endsAt` are DateTime scalars; both a date and a
    // full timestamp are accepted, so the value is forwarded as given.
    if let Some(start_date) = &args.start_date {
        input.insert("startsAt".to_string(), json!(start_date));
    }
    if let Some(end_date) = &args.end_date {
        input.insert("endsAt".to_string(), json!(end_date));
    }

    if input.is_empty() {
        return Err(CliError::validation("Nothing to update")
            .suggestion("Pass --name, --description, --start-date, or --end-date."));
    }

    let cycle_id = resolve_cycle_id(&args.cycle_ref, args.team.as_deref())?;
    let client = graphql::client()?;
    let result = client.request(
        UPDATE_CYCLE_MUTATION,
        json!({ "id": cycle_id, "input": Value::Object(input) }),
    )?;

    let updated = result.get("cycleUpdate").cloned().unwrap_or(Value::Null);
    if !updated
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CliError::cli("Failed to update cycle"));
    }

    let cycle = updated.get("cycle").cloned().unwrap_or(Value::Null);
    output::line(&format!(
        "✓ Updated cycle: {}",
        super::cycle_label(&cycle, &args.cycle_ref)
    ));
    if let Some(start) = cycle.get("startsAt").and_then(Value::as_str) {
        output::line(&format!("  Start: {}", date_prefix(start)));
    }
    if let Some(end) = cycle.get("endsAt").and_then(Value::as_str) {
        output::line(&format!("  End: {}", date_prefix(end)));
    }

    Ok(())
}

/// `2024-01-31T00:00:00.000Z` -> `2024-01-31`, the same prefix `cycle view` shows.
fn date_prefix(value: &str) -> &str {
    value.split('T').next().unwrap_or(value)
}
