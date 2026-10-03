//! `linear team states` — list a team's workflow states.

use serde_json::{json, Value};

use crate::colors;
use crate::display;
use crate::errors::{CliError, Result};
use crate::graphql;
use crate::linear::{
    compare_workflow_states, get_team_key, resolve_team, WorkflowState, GET_WORKFLOW_STATES_QUERY,
};
use crate::output;

// The `GetWorkflowStates` document lives in `linear/queries.rs` and is used from here: it was
// defined in both files, byte for byte, which is the kind of twin that stops being byte for byte
// one commit later. `tests/graphql_document_names.rs` keeps it from coming back.

#[derive(clap::Args, Debug)]
pub struct StatesArgs {
    /// Team key, name, or ID (defaults to the configured team)
    pub team: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: StatesArgs) -> Result<()> {
    let resolved_team_key = match &args.team {
        Some(team) => resolve_team(team)?.key,
        None => get_team_key()?.ok_or_else(|| {
            CliError::validation("Could not determine team key from directory name")
                .suggestion("Please specify a team key, name, or ID as an argument.")
        })?,
    };

    let client = graphql::client()?;
    let data = client.request(
        GET_WORKFLOW_STATES_QUERY,
        json!({ "teamKey": resolved_team_key }),
    )?;
    let nodes = data
        .get("team")
        .and_then(|team| team.get("states"))
        .and_then(|states| states.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // States arrive in the app's display order (type group, then position
    // descending). Sort the raw values so `--json` keeps GraphQL field names.
    let mut states: Vec<(WorkflowState, Value)> = nodes
        .into_iter()
        .filter_map(|value| state_from_value(&value).map(|state| (state, value)))
        .collect();
    states.sort_by(|a, b| compare_workflow_states(&a.0, &b.0));
    let states: Vec<Value> = states.into_iter().map(|(_, value)| value).collect();

    if args.json {
        output::print_json(&json!({ "nodes": states }));
        return Ok(());
    }

    if states.is_empty() {
        output::line("No workflow states found for this team.");
        return Ok(());
    }

    let name_width = std::iter::once(display::display_width("NAME"))
        .chain(
            states
                .iter()
                .map(|state| state_str(state, "name"))
                .map(display::display_width),
        )
        .max()
        .unwrap_or(4);
    let type_width = std::iter::once(display::display_width("TYPE"))
        .chain(
            states
                .iter()
                .map(|state| state_str(state, "type"))
                .map(display::display_width),
        )
        .max()
        .unwrap_or(4);

    let header = format!(
        "{} {}",
        display::pad_display("NAME", name_width),
        display::pad_display("TYPE", type_width)
    );
    output::line(&colors::underline(&header));

    for state in &states {
        output::line(&format!(
            "{} {}",
            display::pad_display(state_str(state, "name"), name_width),
            display::pad_display(state_str(state, "type"), type_width)
        ));
    }

    Ok(())
}

fn state_str<'a>(state: &'a Value, key: &str) -> &'a str {
    state.get(key).and_then(Value::as_str).unwrap_or("")
}

fn state_from_value(value: &Value) -> Option<WorkflowState> {
    Some(WorkflowState {
        id: value.get("id")?.as_str()?.to_string(),
        name: value.get("name")?.as_str()?.to_string(),
        state_type: value.get("type")?.as_str()?.to_string(),
        position: value.get("position")?.as_f64()?,
    })
}
