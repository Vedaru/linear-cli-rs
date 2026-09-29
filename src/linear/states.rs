use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Blocked
// ---------------------------------------------------------------------------

/// `true` when an issue is blocked by another issue.
///
/// The relation is read from `inverseRelations` with type `blocks`: on the
/// blocked issue, an incoming "blocks" relation points at the issue doing the
/// blocking. A blocker that is already completed or canceled does not count.
pub fn is_issue_blocked(issue: &Value) -> bool {
    let Some(nodes) = issue
        .get("inverseRelations")
        .and_then(|relations| relations.get("nodes"))
        .and_then(Value::as_array)
    else {
        return false;
    };

    for relation in nodes {
        if relation.get("type").and_then(Value::as_str) != Some("blocks") {
            continue;
        }
        let blocker_type = relation
            .get("issue")
            .and_then(|blocker| blocker.get("state"))
            .and_then(|state| state.get("type"))
            .and_then(Value::as_str);
        if blocker_type != Some("completed") && blocker_type != Some("canceled") {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Workflow states
// ---------------------------------------------------------------------------

/// One team's workflow state.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
    pub state_type: String,
    pub position: f64,
}

impl WorkflowState {
    fn from_value(value: &Value) -> Option<Self> {
        Some(WorkflowState {
            id: value.get("id")?.as_str()?.to_string(),
            name: value.get("name")?.as_str()?.to_string(),
            state_type: value.get("type")?.as_str()?.to_string(),
            position: value.get("position")?.as_f64()?,
        })
    }
}

pub(crate) fn compare_workflow_state_types(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let rank = |t: &str| {
        WORKFLOW_STATE_TYPE_ORDER
            .iter()
            .position(|known| *known == t)
    };
    match (rank(a), rank(b)) {
        (Some(a_rank), Some(b_rank)) => a_rank.cmp(&b_rank),
        // An unrecognised status sorts after every known one, grouped by its
        // own name. It must not be promoted ahead of the known lifecycle.
        (None, None) => a.cmp(b),
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
    }
}

/// Order two workflow states the way the Linear app does: type group first,
/// then position DESCENDING inside the group.
///
/// The descending tiebreak contradicts the schema's doc comment but matches the
/// app, which is what a listing is trying to reproduce. Do not "correct" it to
/// ascending on the strength of the comment alone.
pub fn compare_workflow_states(a: &WorkflowState, b: &WorkflowState) -> std::cmp::Ordering {
    compare_workflow_state_types(&a.state_type, &b.state_type).then_with(|| {
        b.position
            .partial_cmp(&a.position)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// The state a bare type name refers to: the earliest one of that type in the
/// team's workflow, i.e. the LOWEST position.
///
/// Deliberately independent of the order of `states`; `compare_workflow_states`
/// sorts descending, so the first match in a sorted list would be the wrong one.
pub fn lowest_position_state_of_type<'a>(
    states: &'a [WorkflowState],
    state_type: &str,
) -> Option<&'a WorkflowState> {
    let wanted = state_type.to_lowercase();
    states
        .iter()
        .filter(|state| state.state_type == wanted)
        .min_by(|a, b| {
            a.position
                .partial_cmp(&b.position)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// Every workflow state for a team, sorted the way the app groups them.
pub fn get_workflow_states(team_key: &str) -> Result<Vec<WorkflowState>> {
    let client = graphql::client()?;
    let data = client.request(GET_WORKFLOW_STATES_QUERY, json!({ "teamKey": team_key }))?;
    let mut states: Vec<WorkflowState> = data
        .get("team")
        .and_then(|team| team.get("states"))
        .and_then(|states| states.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| nodes.iter().filter_map(WorkflowState::from_value).collect())
        .unwrap_or_default();
    states.sort_by(compare_workflow_states);
    Ok(states)
}

/// The first `started` state in the workflow, which is what `issue start`
/// moves an issue to.
pub fn get_started_state(team_key: &str) -> Result<WorkflowState> {
    let states = get_workflow_states(team_key)?;
    lowest_position_state_of_type(&states, "started")
        .cloned()
        .ok_or_else(|| CliError::cli("No 'started' state found in workflow"))
}

/// Resolve a workflow state from an already-fetched list by name
/// (case-insensitive) or by type. A type with several states resolves to the
/// lowest-position one, independent of the order of `states`.
pub fn resolve_workflow_state(
    states: &[WorkflowState],
    name_or_type: &str,
) -> Result<Option<WorkflowState>> {
    // A pasted URL is refused here rather than reported as a state that does
    // not exist; workflow states have no URL.
    reject_linear_url(name_or_type, "a workflow state name or type")?;

    let lower = name_or_type.to_lowercase();
    if let Some(state) = states
        .iter()
        .find(|state| state.name.to_lowercase() == lower)
    {
        return Ok(Some(state.clone()));
    }
    Ok(lowest_position_state_of_type(states, &lower).cloned())
}

/// Build the error thrown when a requested workflow state can't be resolved for
/// a team. Shared by `issue create` and `issue update`.
pub fn workflow_state_not_found_error(
    team_key: &str,
    requested: &str,
    states: &[WorkflowState],
) -> CliError {
    let suggestion = if states.is_empty() {
        format!("Team {team_key} has no workflow states. Run `linear team states {team_key}`.")
    } else {
        let listed = states
            .iter()
            .map(|state| {
                format!(
                    "{} ({})",
                    serde_json::to_string(&state.name).unwrap_or_default(),
                    state.state_type
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("Valid states: {listed}. Run `linear team states {team_key}` to list them.")
    };

    CliError::new(
        ErrorKind::NotFound,
        format!("Workflow state not found: '{requested}' for team {team_key}"),
    )
    .suggestion(suggestion)
}

// ---------------------------------------------------------------------------
// State scopes and selection
// ---------------------------------------------------------------------------

/// Which teams a workflow-state lookup covers.
#[derive(Debug, Clone)]
pub enum StateScope {
    TeamKeys(Vec<String>),
    AllTeams,
}

/// A resolved `--state` filter: bare type names and explicit state IDs are
/// combined into a single `or` filter.
#[derive(Debug, Clone, Default)]
pub struct StateSelection {
    pub types: Vec<String>,
    pub state_ids: Vec<String>,
}

/// `true` when a value is one of the bare workflow state type tokens.
pub fn is_issue_state_type(value: &str) -> bool {
    ISSUE_STATE_TYPES.contains(&value)
}

/// A workflow state tagged with the team it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedWorkflowState {
    pub id: String,
    pub name: String,
    pub state_type: String,
    pub team_key: String,
}

impl ScopedWorkflowState {
    fn from_value(value: &Value) -> Option<Self> {
        Some(ScopedWorkflowState {
            id: value.get("id")?.as_str()?.to_string(),
            name: value.get("name")?.as_str()?.to_string(),
            state_type: value.get("type")?.as_str()?.to_string(),
            team_key: value.get("team")?.get("key")?.as_str()?.to_string(),
        })
    }
}

/// Workflow states across a scope, paginated to exhaustion. `AllTeams` sends no
/// filter; `TeamKeys` restricts the query to those teams so a state from a team
/// outside the scope can never silently match.
pub fn get_workflow_states_in_scope(scope: &StateScope) -> Result<Vec<ScopedWorkflowState>> {
    let client = graphql::client()?;
    let filter_value = match scope {
        StateScope::AllTeams => None,
        StateScope::TeamKeys(keys) => Some(json!({ "team": { "key": { "in": keys } } })),
    };

    let mut states = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let mut variables = Map::new();
        if let Some(filter_value) = &filter_value {
            variables.insert("filter".to_string(), filter_value.clone());
        }
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }

        let data = client.request(
            GET_WORKFLOW_STATES_WITH_TEAMS_QUERY,
            Value::Object(variables),
        )?;
        let connection = data
            .get("workflowStates")
            .ok_or_else(|| CliError::cli("Linear API response did not contain workflowStates"))?;

        if let Some(nodes) = connection.get("nodes").and_then(Value::as_array) {
            states.extend(nodes.iter().filter_map(ScopedWorkflowState::from_value));
        }

        let page_info = connection.get("pageInfo");
        let has_next = page_info
            .and_then(|info| info.get("hasNextPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !has_next {
            break;
        }
        let end_cursor = page_info
            .and_then(|info| info.get("endCursor"))
            .and_then(Value::as_str)
            .map(str::to_string);
        match end_cursor {
            Some(cursor) if Some(cursor.as_str()) != after.as_deref() => {
                after = Some(cursor);
            }
            _ => {
                return Err(CliError::cli(
                    "Linear reported more workflow states but returned no new pagination cursor",
                ))
            }
        }
    }

    Ok(states)
}

/// Resolve `--state` values into a selection of bare types and state IDs.
///
/// Each non-type value is matched case-insensitively against the scoped state
/// names; an unknown value errors with the states that *are* available.
pub fn resolve_state_selection(values: &[String], scope: &StateScope) -> Result<StateSelection> {
    let mut selection = StateSelection::default();
    if values.is_empty() {
        return Ok(selection);
    }

    let types: Vec<String> = values
        .iter()
        .filter(|value| is_issue_state_type(value))
        .cloned()
        .collect();
    let names: Vec<&String> = values
        .iter()
        .filter(|value| !is_issue_state_type(value))
        .collect();

    selection.types = types;
    if names.is_empty() {
        return Ok(selection);
    }

    let states = get_workflow_states_in_scope(scope)?;
    for value in names {
        // Exact name first, then type text: a value like "Started" is a real
        // name to prefer, while "started" would have been caught above.
        let matched = states
            .iter()
            .find(|state| state.name.to_lowercase() == value.to_lowercase())
            .or_else(|| {
                states
                    .iter()
                    .find(|state| state.state_type.to_lowercase() == value.to_lowercase())
            });
        match matched {
            Some(state) => selection.state_ids.push(state.id.clone()),
            None => return Err(state_not_found_in_scope_error(value, scope, &states)),
        }
    }

    Ok(selection)
}

/// The error shown when a `--state` value matches no state in scope.
pub fn state_not_found_in_scope_error(
    value: &str,
    scope: &StateScope,
    states: &[ScopedWorkflowState],
) -> CliError {
    let where_ = match scope {
        StateScope::AllTeams => "any team".to_string(),
        StateScope::TeamKeys(keys) => {
            let separated = keys
                .iter()
                .map(|key| format!("\"{key}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "team{s} {separated}",
                s = if keys.len() == 1 { "" } else { "s" }
            )
        }
    };

    let mut available: Vec<&ScopedWorkflowState> = states.iter().collect();
    available.sort_by(|a, b| {
        a.team_key
            .to_lowercase()
            .cmp(&b.team_key.to_lowercase())
            .then_with(|| compare_workflow_state_types(&a.state_type, &b.state_type))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    let suggestion = if available.is_empty() {
        "Run `linear team states <team>` to list available states.".to_string()
    } else {
        let listed = available
            .iter()
            .map(|state| format!("{} ({})", state.name, state.team_key))
            .collect::<Vec<_>>()
            .join(", ");
        format!("Valid states: {listed}")
    };

    CliError::new(
        ErrorKind::NotFound,
        format!("Workflow state not found: '{value}' in {where_}"),
    )
    .suggestion(suggestion)
}

/// Build the GraphQL `IssueFilter` fragment for a state selection, or `None`
/// when the selection is empty.
pub fn workflow_state_filter(selection: &StateSelection) -> Result<Option<Value>> {
    if selection.types.is_empty() && selection.state_ids.is_empty() {
        return Ok(None);
    }
    let mut filter = Map::new();
    if !selection.types.is_empty() {
        filter.insert(
            "state".to_string(),
            json!({ "type": { "in": selection.types } }),
        );
    }
    if !selection.state_ids.is_empty() {
        filter.insert(
            "state".to_string(),
            json!({ "id": { "in": selection.state_ids } }),
        );
    }
    if filter.len() > 1 {
        return Err(CliError::cli(
            "A state filter cannot combine type names and state IDs at once.",
        ));
    }
    Ok(Some(Value::Object(filter)))
}

/// The state to move an issue to on `issue start`: the team's lowest-position
/// `started` state. Delegates to [`get_started_state`].
pub fn update_issue_state(team_key: &str) -> Result<WorkflowState> {
    get_started_state(team_key)
}
