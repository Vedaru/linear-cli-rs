use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Teams
// ---------------------------------------------------------------------------

/// A team reduced to the fields the CLI needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTeam {
    pub id: String,
    pub key: String,
    pub name: String,
}

fn team_from_value(value: &Value) -> Option<ResolvedTeam> {
    Some(ResolvedTeam {
        id: value.get("id")?.as_str()?.to_string(),
        key: value.get("key")?.as_str()?.to_string(),
        name: value.get("name")?.as_str()?.to_string(),
    })
}

/// A malformed team reaching this layer is a hard error, not a silent skip.
fn assert_team_shape(value: &Value) -> Result<ResolvedTeam> {
    team_from_value(value).ok_or_else(|| {
        CliError::cli(format!(
            "Malformed team in API response: {}",
            serde_json::to_string(value).unwrap_or_default()
        ))
    })
}

/// Find a team by UUID, exact key, or exact name, in that order of precedence.
/// `None` when nothing matches.
pub fn find_team(reference: &str) -> Result<Option<ResolvedTeam>> {
    let client = graphql::client()?;
    let is_uuid = is_linear_uuid(reference);
    let query = r#"
query FindTeam($reference: String!, $id: ID, $isUuid: Boolean!) {
  teams(filter: { or: [{ key: { eq: $reference } }, { name: { eq: $reference } }] }) {
    nodes {
      id
      key
      name
    }
  }
  teamById: teams(filter: { id: { eq: $id } }) @include(if: $isUuid) {
    nodes {
      id
      key
      name
    }
  }
}
"#;
    let data = client.request(
        query,
        json!({
            "reference": reference,
            "id": if is_uuid { json!(reference) } else { Value::Null },
            "isUuid": is_uuid,
        }),
    )?;

    let empty = Vec::new();
    let candidates = data
        .get("teams")
        .and_then(|teams| teams.get("nodes"))
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let candidates: Vec<ResolvedTeam> = candidates
        .iter()
        .map(assert_team_shape)
        .collect::<Result<_>>()?;

    if let Some(team) = candidates
        .iter()
        .find(|team| team.key.eq_ignore_ascii_case(reference))
    {
        return Ok(Some(team.clone()));
    }

    if let Some(team) = data
        .get("teamById")
        .and_then(|team_by_id| team_by_id.get("nodes"))
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.first())
    {
        return Ok(Some(assert_team_shape(team)?));
    }

    let by_name: Vec<&ResolvedTeam> = candidates
        .iter()
        .filter(|team| team.name.eq_ignore_ascii_case(reference))
        .collect();
    if by_name.len() > 1 {
        return Err(CliError::validation(format!(
            "Team name \"{reference}\" is ambiguous; it matches multiple teams. Use the team key instead."
        )));
    }
    Ok(by_name.first().map(|team| (*team).clone()))
}

/// Resolve one team by reference, erroring when it cannot be found.
pub fn resolve_team(reference: &str) -> Result<ResolvedTeam> {
    match find_team(reference)? {
        Some(team) => Ok(team),
        None => Err(team_not_found_error(reference)?),
    }
}

/// Resolve several team references, de-duplicated by ID in input order.
pub fn resolve_teams(references: &[String]) -> Result<Vec<ResolvedTeam>> {
    let mut teams: Vec<ResolvedTeam> = Vec::new();
    for reference in references {
        let team = resolve_team(reference)?;
        if !teams.iter().any(|existing| existing.id == team.id) {
            teams.push(team);
        }
    }
    Ok(teams)
}

/// The error for an unknown team, suggesting the workspace's teams.
pub fn team_not_found_error(reference: &str) -> Result<CliError> {
    let teams = get_all_teams()?;
    let suggestion = if teams.is_empty() {
        "Run `linear team list` to see available teams.".to_string()
    } else {
        let listed = teams
            .iter()
            .map(format_team_option)
            .collect::<Vec<_>>()
            .join(", ");
        format!("Available teams: {listed}")
    };
    Ok(CliError::not_found("Team", reference).suggestion(suggestion))
}

/// Teams whose key contains `substring`, as `(id, "name (KEY)")` pairs sorted
/// by key.
pub fn search_teams_by_key_substring(substring: &str) -> Result<Vec<(String, String)>> {
    let client = graphql::client()?;
    let data = client.request(SEARCH_TEAMS_QUERY, json!({ "key": substring }))?;
    let mut teams = data
        .get("teams")
        .and_then(|teams| teams.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    teams.retain(|team| team_from_value(team).is_some());
    teams.sort_by_key(|team| {
        team.get("key")
            .and_then(Value::as_str)
            .map(str::to_lowercase)
            .unwrap_or_default()
    });
    Ok(teams
        .iter()
        .filter_map(|team| {
            let team = team_from_value(team)?;
            Some((team.id, format!("{} ({})", team.name, team.key)))
        })
        .collect())
}

/// Every team in the workspace, sorted by name.
pub fn get_all_teams() -> Result<Vec<ResolvedTeam>> {
    let client = graphql::client()?;
    let data = client.request(GET_ALL_TEAMS_QUERY, json!({}))?;
    let mut teams: Vec<ResolvedTeam> = data
        .get("teams")
        .and_then(|teams| teams.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| nodes.iter().filter_map(team_from_value).collect())
        .unwrap_or_default();
    teams.sort_by_key(|team| team.name.to_lowercase());
    Ok(teams)
}

/// `"KEY (name)"`, the label used when listing teams.
fn format_team_option(team: &ResolvedTeam) -> String {
    format!("{} ({})", team.key, team.name)
}

