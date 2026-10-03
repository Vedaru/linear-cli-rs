use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Cycles
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CycleNode {
    id: String,
    number: i64,
    name: Option<String>,
    starts_at: Option<String>,
    is_next: bool,
    is_previous: bool,
}

impl CycleNode {
    fn from_value(value: &Value) -> Option<Self> {
        Some(CycleNode {
            id: value.get("id")?.as_str()?.to_string(),
            number: integer_field(value.get("number"))?,
            name: value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string),
            starts_at: value
                .get("startsAt")
                .and_then(Value::as_str)
                .map(str::to_string),
            is_next: value
                .get("isNext")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            is_previous: value
                .get("isPrevious")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

#[derive(Debug, Clone)]
struct ActiveCycle {
    id: String,
    number: i64,
}

/// Resolve a cycle to its UUID from a URL, keyword (`active`/`now`, `next`,
/// `previous`), signed offset, number, or name.
pub fn get_cycle_id_by_name_or_number(team_id: &str, cycle_name_or_number: &str) -> Result<String> {
    let url_ref = expect_linear_url_kind(
        cycle_name_or_number,
        "cycle",
        "a cycle URL, number, or name",
    )?;

    let client = graphql::client()?;
    let mut after: Option<String> = None;
    let mut cycles: Vec<CycleNode> = Vec::new();
    let mut team_key = String::new();
    let mut team_name = String::new();
    let mut cycles_enabled = false;
    let mut active_cycle: Option<ActiveCycle> = None;
    let mut first_page = true;

    loop {
        let mut variables = Map::new();
        variables.insert("teamId".to_string(), json!(team_id));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }
        let data = client.request(GET_TEAM_CYCLES_QUERY, Value::Object(variables))?;
        let team = data
            .get("team")
            .ok_or_else(|| CliError::not_found("Team", team_id))?;
        if team.is_null() {
            return Err(CliError::not_found("Team", team_id));
        }

        if first_page {
            first_page = false;
            if let Some(url_ref) = &url_ref {
                let url_team_key = url_ref_team_key(url_ref);
                let data_team_key = team.get("key").and_then(Value::as_str).unwrap_or("");
                if let Some(url_team_key) = url_team_key {
                    if !url_team_key.eq_ignore_ascii_case(data_team_key) {
                        return Err(CliError::validation(format!(
                            "The URL belongs to team {url_team_key}, but --team resolved to {data_team_key}."
                        )));
                    }
                }
            }
            team_key = team
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            team_name = team
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            cycles_enabled = team
                .get("cyclesEnabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            active_cycle = team
                .get("activeCycle")
                .filter(|value| !value.is_null())
                .and_then(|value| {
                    Some(ActiveCycle {
                        id: value.get("id")?.as_str()?.to_string(),
                        number: integer_field(value.get("number"))?,
                    })
                });
        }

        let connection = team.get("cycles");
        if let Some(nodes) = connection
            .and_then(|cycles| cycles.get("nodes"))
            .and_then(Value::as_array)
        {
            cycles.extend(nodes.iter().filter_map(CycleNode::from_value));
        }

        if !cycles_enabled {
            return Err(CliError::validation(format!(
                "Cycles are not enabled for team {team_key}"
            ))
            .suggestion(
                "Enable cycles for the team in Linear's settings before filtering or assigning by cycle.",
            ));
        }

        let page_info = connection.and_then(|cycles| cycles.get("pageInfo"));
        let has_next = page_info
            .and_then(|info| info.get("hasNextPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !has_next {
            break;
        }
        after = page_info
            .and_then(|info| info.get("endCursor"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if after.is_none() {
            return Err(CliError::cli(
                "Pagination stalled: Linear did not return a new cursor.",
            ));
        }
    }

    let _ = team_name;

    let mut reference = cycle_name_or_number.to_string();
    if let Some(url_ref) = &url_ref {
        let cycle_ref = url_ref_cycle(url_ref);
        if let Some(CycleRef::Number(number)) = cycle_ref {
            // Match the number alone. The general path below also matches cycle
            // names, so a cycle that happened to be named "5" could win there.
            let number = *number as i64;
            return cycles
                .iter()
                .find(|cycle| cycle.number == number)
                .map(|cycle| cycle.id.clone())
                .ok_or_else(|| {
                    CliError::not_found("Cycle", &format!("#{number} in team {team_key}"))
                });
        }
        if let Some(CycleRef::Active) = cycle_ref {
            reference = "active".to_string();
        } else if let Some(CycleRef::Next) = cycle_ref {
            reference = "next".to_string();
        }
    }

    let keyword = reference.to_lowercase();

    // Reserved keywords take precedence over coincidental cycle names; use the
    // cycle number to reach a cycle literally named "next"/"previous"/"active".
    if keyword == "active" || keyword == "now" {
        if let Some(active) = &active_cycle {
            return Ok(active.id.clone());
        }
        let next = cycles.iter().find(|cycle| cycle.is_next);
        let suggestion = match next {
            Some(next) => {
                let starts = next
                    .starts_at
                    .as_deref()
                    .map(|value| value.chars().take(10).collect::<String>())
                    .unwrap_or_default();
                format!(
                    "The next cycle (#{}) starts {} — use --cycle next, a cycle number, or a name.",
                    next.number, starts
                )
            }
            None => "Use a cycle number or name instead.".to_string(),
        };
        return Err(
            CliError::cli(format!("Team {team_key} has no active cycle")).suggestion(suggestion),
        );
    }

    if keyword == "next" {
        let next = cycles.iter().find(|cycle| cycle.is_next).ok_or_else(|| {
            CliError::cli(format!("Team {team_key} has no upcoming cycle"))
                .suggestion("Use a cycle number or name instead.")
        })?;
        return Ok(next.id.clone());
    }

    if keyword == "previous" {
        let previous = cycles
            .iter()
            .find(|cycle| cycle.is_previous)
            .ok_or_else(|| {
                CliError::cli(format!("Team {team_key} has no previous cycle"))
                    .suggestion("Use a cycle number or name instead.")
            })?;
        return Ok(previous.id.clone());
    }

    if is_signed_integer(&reference) {
        let Ok(offset) = reference.parse::<i64>() else {
            return Err(CliError::validation(format!(
                "Cycle offset {reference} is out of range"
            )));
        };
        if offset.abs() > 9_007_199_254_740_991 {
            return Err(CliError::validation(format!(
                "Cycle offset {reference} is out of range"
            )));
        }
        let Some(active) = &active_cycle else {
            return Err(CliError::validation(format!(
                "Cannot resolve relative cycle {reference}: the team has no active cycle"
            ))
            .suggestion("Use 'next', a cycle number, or a cycle name while no cycle is active."));
        };
        let Some(target_number) = active.number.checked_add(offset) else {
            return Err(CliError::not_found("Cycle", &reference));
        };
        let target = cycles
            .iter()
            .find(|cycle| cycle.number == target_number)
            .ok_or_else(|| {
                CliError::not_found("Cycle", &format!("{reference} (cycle {target_number})"))
            })?;
        return Ok(target.id.clone());
    }

    let match_ = cycles.iter().find(|cycle| {
        cycle
            .name
            .as_ref()
            .map(|name| name.to_lowercase() == keyword)
            .unwrap_or(false)
            || cycle.number.to_string() == reference
    });
    match match_ {
        Some(cycle) => Ok(cycle.id.clone()),
        None => Err(CliError::not_found("Cycle", &reference)),
    }
}

fn url_ref_team_key(url_ref: &LinearUrlRef) -> Option<&str> {
    match url_ref {
        LinearUrlRef::Cycle { team_key, .. } => Some(team_key),
        _ => None,
    }
}

fn url_ref_cycle(url_ref: &LinearUrlRef) -> Option<&CycleRef> {
    match url_ref {
        LinearUrlRef::Cycle { cycle, .. } => Some(cycle),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Cycle windows
// ---------------------------------------------------------------------------

const GET_TEAM_CYCLE_WINDOWS_QUERY: &str = r#"
query GetTeamCycleWindows($teamId: String!, $first: Int, $after: String) {
  team(id: $teamId) {
    id
    key
    name
    cyclesEnabled
    cycles(first: $first, after: $after) {
      nodes {
        id
        number
        name
        startsAt
        endsAt
        completedAt
        archivedAt
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

/// A team's cycles with the fields a window check needs, plus the team's own `key`/`name` and
/// whether cycles are enabled at all.
///
/// Archived cycles are included on purpose: an archived cycle still occupies its window, and a new
/// cycle created across it would be the overlap the caller is trying to avoid.
pub fn get_team_cycle_windows(team_id: &str) -> Result<(Value, Vec<Value>)> {
    let client = graphql::client()?;
    let mut nodes: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    let mut team = Value::Null;

    loop {
        let mut variables = Map::new();
        variables.insert("teamId".to_string(), json!(team_id));
        variables.insert("first".to_string(), json!(50));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }

        let data = client.request(GET_TEAM_CYCLE_WINDOWS_QUERY, Value::Object(variables))?;
        let node = data
            .get("team")
            .filter(|team| !team.is_null())
            .ok_or_else(|| CliError::not_found("Team", team_id))?;
        if team.is_null() {
            team = json!({
                "id": node.get("id").cloned().unwrap_or(Value::Null),
                "key": node.get("key").cloned().unwrap_or(Value::Null),
                "name": node.get("name").cloned().unwrap_or(Value::Null),
                "cyclesEnabled": node.get("cyclesEnabled").cloned().unwrap_or(Value::Null),
            });
        }

        let connection = node
            .get("cycles")
            .ok_or_else(|| CliError::cli("Linear API response did not contain cycles"))?;
        if let Some(page) = connection.get("nodes").and_then(Value::as_array) {
            nodes.extend(page.iter().cloned());
        }
        let page_info = connection.get("pageInfo");
        let has_next = page_info
            .and_then(|info| info.get("hasNextPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !has_next {
            break;
        }
        after = page_info
            .and_then(|info| info.get("endCursor"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if after.is_none() {
            break;
        }
    }

    Ok((team, nodes))
}
