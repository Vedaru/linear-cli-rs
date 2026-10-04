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
        // The document is shared with `team members`, which takes `$teamKey` and asks for disabled
        // members explicitly, so this caller says `false` rather than relying on an omission
        // (VED-111 collapsed the two).
        variables.insert("teamKey".to_string(), json!(team_id));
        variables.insert("includeDisabled".to_string(), json!(false));
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

