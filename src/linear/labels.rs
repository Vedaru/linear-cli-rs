use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

/// An issue label ID for an exact, case-insensitive name in a team.
pub fn get_issue_label_id_by_name_for_team(
    name: &str,
    team_key: &str,
) -> Result<Option<String>> {
    reject_linear_url(name, "a label name")?;
    let client = graphql::client()?;
    let data = client.request(
        GET_ISSUE_LABEL_BY_NAME_QUERY,
        json!({ "name": name, "teamKey": team_key }),
    )?;
    Ok(first_id(data.get("issueLabels")))
}

/// A project label ID for an exact, case-insensitive name.
pub fn get_project_label_id_by_name(name: &str) -> Result<Option<String>> {
    reject_linear_url(name, "a label name")?;
    let client = graphql::client()?;
    let data = client.request(GET_PROJECT_LABEL_BY_NAME_QUERY, json!({ "name": name }))?;
    Ok(first_id(data.get("projectLabels")))
}

/// Issue labels in a team whose name contains `name`, sorted by name.
pub fn get_issue_label_options_by_name_for_team(
    name: &str,
    team_key: &str,
) -> Result<Vec<(String, String)>> {
    let client = graphql::client()?;
    let data = client.request(
        GET_ISSUE_LABELS_BY_NAME_QUERY,
        json!({ "name": name, "teamKey": team_key }),
    )?;
    let mut labels: Vec<(String, String)> = data
        .get("issueLabels")
        .and_then(|labels| labels.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| {
                    Some((
                        node.get("id")?.as_str()?.to_string(),
                        node.get("name")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    labels.sort_by_key(|label| label.1.to_lowercase());
    Ok(labels)
}

/// A team's labels, sorted by name.
pub fn get_labels_for_team(team_id: &str) -> Result<Vec<Value>> {
    let client = graphql::client()?;
    let data = client.request(GET_LABELS_QUERY, json!({ "teamId": team_id }))?;
    let mut labels = data
        .get("team")
        .and_then(|team| team.get("labels"))
        .and_then(|labels| labels.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    labels.sort_by_key(name_lowercase);
    Ok(labels)
}

pub(crate) fn first_id(connection: Option<&Value>) -> Option<String> {
    connection
        .and_then(|connection| connection.get("nodes"))
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.first())
        .and_then(|node| node.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

