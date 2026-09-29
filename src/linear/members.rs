use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

/// A team's members connection, shaped `{ nodes, pageInfo }`.
pub fn get_team_members(team_id: &str) -> Result<Value> {
    let client = graphql::client()?;
    let mut nodes: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    let mut last_page_info = json!({ "hasNextPage": false, "endCursor": null });
    loop {
        let mut variables = Map::new();
        variables.insert("teamId".to_string(), json!(team_id));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }
        let data = client.request(GET_TEAM_MEMBERS_QUERY, Value::Object(variables))?;
        let connection = data
            .get("team")
            .and_then(|team| team.get("members"))
            .ok_or_else(|| CliError::cli("Team not found"))?;
        if let Some(page_nodes) = connection.get("nodes").and_then(Value::as_array) {
            nodes.extend(page_nodes.iter().cloned());
        }
        let page_info = connection.get("pageInfo");
        if let Some(page_info) = page_info {
            last_page_info = page_info.clone();
        }
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
    Ok(json!({ "nodes": nodes, "pageInfo": last_page_info }))
}

/// The organization's members connection, shaped `{ nodes, pageInfo }`.
pub fn get_organization_members() -> Result<Value> {
    let client = graphql::client()?;
    let mut nodes: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    let mut last_page_info = json!({ "hasNextPage": false, "endCursor": null });
    loop {
        let mut variables = Map::new();
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }
        let data = client.request(GET_ORGANIZATION_MEMBERS_QUERY, Value::Object(variables))?;
        let connection = data
            .get("users")
            .ok_or_else(|| CliError::cli("Linear API response did not contain users"))?;
        if let Some(page_nodes) = connection.get("nodes").and_then(Value::as_array) {
            nodes.extend(page_nodes.iter().cloned());
        }
        let page_info = connection.get("pageInfo");
        if let Some(page_info) = page_info {
            last_page_info = page_info.clone();
        }
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
    Ok(json!({ "nodes": nodes, "pageInfo": last_page_info }))
}

