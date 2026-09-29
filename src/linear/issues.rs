use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Issue details
// ---------------------------------------------------------------------------

/// The raw `issue` object for an identifier, or `None` when it does not exist.
pub fn fetch_issue_details_raw(issue_id: &str, include_comments: bool) -> Result<Option<Value>> {
    let client = graphql::client()?;
    let query = if include_comments {
        GET_ISSUE_DETAILS_WITH_COMMENTS_QUERY
    } else {
        GET_ISSUE_DETAILS_QUERY
    };
    let data = client.request(query, json!({ "id": issue_id }))?;
    Ok(data.get("issue").cloned().filter(|value| !value.is_null()))
}

/// The issue detail object with each connection replaced by its `nodes` array,
/// the shape the display and `--json` paths consume.
pub fn fetch_issue_details(issue_id: &str, include_comments: bool) -> Result<Value> {
    let Some(raw) = fetch_issue_details_raw(issue_id, include_comments)? else {
        return Ok(Value::Null);
    };
    let mut object = raw.as_object().cloned().unwrap_or_default();

    let mut connection_keys = vec!["children", "attachments", "documents"];
    if include_comments {
        connection_keys.push("comments");
    }
    for key in connection_keys {
        let nodes = object
            .get(key)
            .and_then(|connection| connection.get("nodes"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        object.insert(key.to_string(), Value::Array(nodes));
    }

    Ok(Value::Object(object))
}

/// The parent issue's `IDENTIFIER: title`, or `None` when the lookup fails.
/// Titles are metadata: a failure must not sink the issue being displayed.
pub fn fetch_parent_issue_title(parent_id: &str) -> Option<String> {
    let data = (|| -> Result<Value> {
        let client = graphql::client()?;
        client.request(FETCH_PARENT_ISSUE_TITLE_QUERY, json!({ "id": parent_id }))
    })()
    .ok()?;

    let identifier = data.get("issue")?.get("identifier")?.as_str()?;
    let title = data.get("issue")?.get("title")?.as_str()?;
    Some(format!("{identifier}: {title}"))
}

/// The parent issue's identifier, title, and project ID, or `None` on failure.
pub fn fetch_parent_issue_data(parent_id: &str) -> Option<Value> {
    let data = (|| -> Result<Value> {
        let client = graphql::client()?;
        client.request(FETCH_PARENT_ISSUE_DATA_QUERY, json!({ "id": parent_id }))
    })()
    .ok()?;

    let issue = data.get("issue")?;
    let identifier = issue.get("identifier")?.as_str()?;
    let title = issue.get("title")?.as_str()?;
    let project_id = issue
        .get("project")
        .and_then(|project| project.get("id"))
        .cloned()
        .unwrap_or(Value::Null);

    Some(json!({
        "identifier": identifier,
        "title": title,
        "projectId": project_id,
    }))
}

/// The GraphQL sort array for an issue sort mode.
pub fn get_issue_sort_payload(sort: IssueSort) -> Value {
    match sort {
        IssueSort::Manual => json!([{ "workflowState": { "order": "Ascending" } }]),
        _ => json!([{ "priority": { "order": "Ascending" } }]),
    }
}

/// The `labels` clause for an issue filter. One name is a `some` match; several
/// are ANDed so an issue must carry every requested label. Names compare
/// case-insensitively, as the Linear UI does.
pub(crate) fn label_filter(label_names: &[String]) -> Option<Value> {
    match label_names {
        [] => None,
        [only] => Some(json!({ "some": { "name": { "eqIgnoreCase": only } } })),
        names => Some(json!({
            "and": names
                .iter()
                .map(|name| json!({ "some": { "name": { "eqIgnoreCase": name } } }))
                .collect::<Vec<_>>(),
        })),
    }
}

// ---------------------------------------------------------------------------
// Issue fetching
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct FetchIssuesForStateOptions {
    pub assignee: Option<String>,
    pub unassigned: bool,
    pub all_assignees: bool,
    pub limit: Option<u32>,
    pub project_id: Option<String>,
    pub sort: Option<IssueSort>,
    pub cycle_id: Option<String>,
    pub milestone_id: Option<String>,
    pub project_label: Option<String>,
    pub label_names: Option<Vec<String>>,
    pub created_after: Option<String>,
    pub updated_after: Option<String>,
}

/// Issues for one team in a state selection, shaped `{ issues: { nodes } }`.
///
/// `limit` bounds the number of issues fetched; `None` or `Some(0)` fetches
/// everything, matching upstream's `fetchAll` handling.
pub fn fetch_issues_for_state(
    team_key: &str,
    state: Option<&StateSelection>,
    options: &FetchIssuesForStateOptions,
) -> Result<Value> {
    let client = graphql::client()?;

    let limit = options.limit;
    let fetch_all = limit.is_none() || limit == Some(0);
    let page_size: u32 = match limit {
        Some(value) if !fetch_all => value.min(100),
        _ => 50,
    };

    let state_filter = match state {
        Some(selection) => workflow_state_filter(selection)?,
        None => None,
    };

    let labels: Option<Value> = options.label_names.as_deref().and_then(label_filter);

    let mut filter = Map::new();
    filter.insert("team".to_string(), json!({ "key": { "eq": team_key } }));
    if let Some(state_filter) = &state_filter {
        if let Some(state) = state_filter.get("state") {
            filter.insert("state".to_string(), state.clone());
        }
    }
    if options.unassigned && options.all_assignees {
        return Err(CliError::validation(
            "Cannot use both --unassigned and --all-assignees",
        ));
    }
    if options.unassigned {
        filter.insert("assignee".to_string(), Value::Null);
    } else if options.all_assignees {
        // No assignee filter: include all.
    } else if let Some(assignee) = &options.assignee {
        filter.insert("assignee".to_string(), json!({ "id": { "eq": assignee } }));
    }
    if let Some(project_id) = &options.project_id {
        filter.insert("project".to_string(), json!({ "id": { "eq": project_id } }));
    } else if let Some(project_label) = &options.project_label {
        filter.insert(
            "project".to_string(),
            json!({ "labels": { "name": { "eqIgnoreCase": project_label } } }),
        );
    }
    if let Some(milestone_id) = &options.milestone_id {
        filter.insert(
            "projectMilestone".to_string(),
            json!({ "id": { "eq": milestone_id } }),
        );
    }
    if let Some(cycle_id) = &options.cycle_id {
        filter.insert("cycle".to_string(), json!({ "id": { "eq": cycle_id } }));
    }
    if let Some(labels) = labels {
        filter.insert("labels".to_string(), labels);
    }
    if let Some(created_after) = &options.created_after {
        filter.insert(
            "createdAt".to_string(),
            json!({ "gte": parse_date_filter(created_after, "--created-after")? }),
        );
    }
    if let Some(updated_after) = &options.updated_after {
        filter.insert(
            "updatedAt".to_string(),
            json!({ "gte": parse_date_filter(updated_after, "--updated-after")? }),
        );
    }

    let sort = options.sort.unwrap_or(IssueSort::Priority);
    let sort_payload = get_issue_sort_payload(sort);

    let mut all_issues: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let mut variables = Map::new();
        variables.insert("filter".to_string(), Value::Object(filter.clone()));
        variables.insert("sort".to_string(), sort_payload.clone());
        variables.insert("first".to_string(), json!(page_size));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }

        let data = client.request(FETCH_ISSUES_QUERY, Value::Object(variables))?;
        let connection = data
            .get("issues")
            .ok_or_else(|| CliError::cli("Linear API response did not contain issues"))?;

        if let Some(nodes) = connection.get("nodes").and_then(Value::as_array) {
            all_issues.extend(nodes.iter().cloned());
        }

        if !fetch_all {
            if let Some(limit) = limit {
                if all_issues.len() >= limit as usize {
                    break;
                }
            }
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

    // `limit == Some(0)` means "all" for the fetch loop but upstream still
    // slices to 0, so an explicit 0 yields an empty list. Preserved verbatim.
    let nodes = match limit {
        Some(value) => all_issues
            .into_iter()
            .take(value as usize)
            .collect::<Vec<_>>(),
        None => all_issues,
    };
    let mut nodes = nodes;
    sort_issues_by_workflow_state(&mut nodes);

    Ok(json!({ "issues": { "nodes": nodes } }))
}

#[derive(Debug, Clone, Default)]
pub struct FetchIssuesForQueryOptions {
    pub team_keys: Option<Vec<String>>,
    pub all_teams: bool,
    pub state: Option<StateSelection>,
    pub assignee: Option<String>,
    pub unassigned: bool,
    pub sort: Option<IssueSort>,
    pub limit: Option<u32>,
    pub project_id: Option<String>,
    pub project_label: Option<String>,
    pub cycle_id: Option<String>,
    pub milestone_id: Option<String>,
    pub label_names: Option<Vec<String>>,
    pub created_after: Option<String>,
    pub updated_after: Option<String>,
    pub include_archived: Option<bool>,
}

/// Issues across teams in a query filter, shaped `{ nodes, pageInfo }`.
///
/// `limit == Some(0)` fetches everything; `None` uses the default page size.
pub fn fetch_issues_for_query(options: &FetchIssuesForQueryOptions) -> Result<Value> {
    let client = graphql::client()?;

    let mut filter = Map::new();
    if let Some(team_keys) = options.team_keys.as_ref().filter(|keys| !keys.is_empty()) {
        filter.insert("team".to_string(), json!({ "key": { "in": team_keys } }));
    }
    if let Some(state) = &options.state {
        if let Some(state_filter) = workflow_state_filter(state)? {
            if let Some(state) = state_filter.get("state") {
                filter.insert("state".to_string(), state.clone());
            }
        }
    }
    if options.unassigned {
        filter.insert("assignee".to_string(), json!({ "null": true }));
    } else if let Some(assignee) = &options.assignee {
        let user_id =
            lookup_user_id(assignee)?.ok_or_else(|| CliError::not_found("User", assignee))?;
        filter.insert("assignee".to_string(), json!({ "id": { "eq": user_id } }));
    }
    if let Some(project_id) = &options.project_id {
        filter.insert("project".to_string(), json!({ "id": { "eq": project_id } }));
    } else if let Some(project_label) = &options.project_label {
        filter.insert(
            "project".to_string(),
            json!({ "labels": { "name": { "eqIgnoreCase": project_label } } }),
        );
    }
    if let Some(cycle_id) = &options.cycle_id {
        filter.insert("cycle".to_string(), json!({ "id": { "eq": cycle_id } }));
    }
    if let Some(milestone_id) = &options.milestone_id {
        filter.insert(
            "projectMilestone".to_string(),
            json!({ "id": { "eq": milestone_id } }),
        );
    }
    if let Some(labels) = options.label_names.as_deref().and_then(label_filter) {
        filter.insert("labels".to_string(), labels);
    }
    if let Some(created_after) = &options.created_after {
        filter.insert(
            "createdAt".to_string(),
            json!({ "gte": parse_date_filter(created_after, "--created-after")? }),
        );
    }
    if let Some(updated_after) = &options.updated_after {
        filter.insert(
            "updatedAt".to_string(),
            json!({ "gte": parse_date_filter(updated_after, "--updated-after")? }),
        );
    }

    let fetch_all = options.limit == Some(0);
    let limit = options.limit.unwrap_or(50);
    let page_size: u32 = if fetch_all { 100 } else { limit.min(100) };
    let sort_payload = get_issue_sort_payload(options.sort.unwrap_or(IssueSort::Priority));

    let mut all_issues: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    let mut last_page_info = json!({ "hasNextPage": false, "endCursor": null });
    let mut has_next = true;

    while has_next {
        let mut variables = Map::new();
        if !filter.is_empty() {
            variables.insert("filter".to_string(), Value::Object(filter.clone()));
        }
        variables.insert("sort".to_string(), sort_payload.clone());
        variables.insert("first".to_string(), json!(page_size));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }
        if let Some(include_archived) = options.include_archived {
            variables.insert("includeArchived".to_string(), json!(include_archived));
        }

        let data = client.request(FETCH_ISSUES_QUERY, Value::Object(variables))?;
        let connection = data
            .get("issues")
            .ok_or_else(|| CliError::cli("Linear API response did not contain issues"))?;

        if let Some(nodes) = connection.get("nodes").and_then(Value::as_array) {
            all_issues.extend(nodes.iter().cloned());
        }

        if let Some(page_info) = connection.get("pageInfo") {
            last_page_info = page_info.clone();
            has_next = page_info
                .get("hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            after = page_info
                .get("endCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
        } else {
            has_next = false;
        }

        if !fetch_all && all_issues.len() >= limit as usize {
            break;
        }
    }

    let mut nodes = if fetch_all {
        all_issues
    } else {
        all_issues.into_iter().take(limit as usize).collect()
    };
    sort_issues_by_workflow_state(&mut nodes);

    Ok(json!({ "nodes": nodes, "pageInfo": last_page_info }))
}

#[derive(Debug, Clone, Default)]
pub struct SearchIssuesByTermOptions {
    pub team_keys: Option<Vec<String>>,
    pub state: Option<StateSelection>,
    pub assignee: Option<String>,
    pub unassigned: bool,
    pub limit: Option<u32>,
    pub project_id: Option<String>,
    pub project_label: Option<String>,
    pub cycle_id: Option<String>,
    pub label_names: Option<Vec<String>>,
    pub created_after: Option<String>,
    pub updated_after: Option<String>,
    pub include_archived: Option<bool>,
    pub include_comments: Option<bool>,
    pub order_by: Option<String>,
}

/// Full-text issue search, shaped `{ nodes, pageInfo, totalCount }`.
pub fn search_issues_by_term(term: &str, options: &SearchIssuesByTermOptions) -> Result<Value> {
    let client = graphql::client()?;

    let mut filter = Map::new();
    if let Some(team_keys) = options.team_keys.as_ref().filter(|keys| !keys.is_empty()) {
        if team_keys.len() == 1 {
            filter.insert("team".to_string(), json!({ "key": { "eq": team_keys[0] } }));
        } else {
            filter.insert(
                "team".to_string(),
                json!({
                    "or": team_keys
                        .iter()
                        .map(|key| json!({ "key": { "eq": key } }))
                        .collect::<Vec<_>>(),
                }),
            );
        }
    }
    if let Some(state) = &options.state {
        if let Some(state_filter) = workflow_state_filter(state)? {
            if let Some(state) = state_filter.get("state") {
                filter.insert("state".to_string(), state.clone());
            }
        }
    }
    if options.unassigned {
        filter.insert("assignee".to_string(), json!({ "null": true }));
    } else if let Some(assignee) = &options.assignee {
        let user_id =
            lookup_user_id(assignee)?.ok_or_else(|| CliError::not_found("User", assignee))?;
        filter.insert("assignee".to_string(), json!({ "id": { "eq": user_id } }));
    }
    if let Some(project_id) = &options.project_id {
        filter.insert("project".to_string(), json!({ "id": { "eq": project_id } }));
    } else if let Some(project_label) = &options.project_label {
        filter.insert(
            "project".to_string(),
            json!({ "labels": { "name": { "eqIgnoreCase": project_label } } }),
        );
    }
    if let Some(cycle_id) = &options.cycle_id {
        filter.insert("cycle".to_string(), json!({ "id": { "eq": cycle_id } }));
    }
    if let Some(labels) = options.label_names.as_deref().and_then(label_filter) {
        filter.insert("labels".to_string(), labels);
    }
    if let Some(created_after) = &options.created_after {
        filter.insert(
            "createdAt".to_string(),
            json!({ "gte": parse_date_filter(created_after, "--created-after")? }),
        );
    }
    if let Some(updated_after) = &options.updated_after {
        filter.insert(
            "updatedAt".to_string(),
            json!({ "gte": parse_date_filter(updated_after, "--updated-after")? }),
        );
    }

    let fetch_unlimited = options.limit == Some(0);
    let mut all_nodes: Vec<Value> = Vec::new();
    let mut total_count = 0i64;
    let mut last_page_info = json!({ "hasNextPage": false, "endCursor": null });
    let mut after: Option<String> = None;

    loop {
        let remaining: Option<i64> = if fetch_unlimited {
            Some(100)
        } else {
            options
                .limit
                .map(|limit| (limit as i64 - all_nodes.len() as i64).min(100))
        };

        if !fetch_unlimited {
            if let (Some(remaining), Some(limit)) = (remaining, options.limit) {
                if remaining <= 0 || all_nodes.len() >= limit as usize {
                    break;
                }
            }
        }

        let mut variables = Map::new();
        variables.insert("term".to_string(), json!(term));
        if !filter.is_empty() {
            variables.insert("filter".to_string(), Value::Object(filter.clone()));
        }
        if let Some(remaining) = remaining {
            variables.insert("first".to_string(), json!(remaining));
        }
        if let Some(after) = &after {
            variables.insert("after".to_string(), json!(after));
        }
        if let Some(include_archived) = options.include_archived {
            variables.insert("includeArchived".to_string(), json!(include_archived));
        }
        if let Some(include_comments) = options.include_comments {
            variables.insert("includeComments".to_string(), json!(include_comments));
        }
        if let Some(order_by) = &options.order_by {
            variables.insert("orderBy".to_string(), json!(order_by));
        }

        let data = client.request(SEARCH_ISSUES_QUERY, Value::Object(variables))?;
        let connection = data
            .get("searchIssues")
            .ok_or_else(|| CliError::cli("Linear API response did not contain searchIssues"))?;

        if let Some(nodes) = connection.get("nodes").and_then(Value::as_array) {
            all_nodes.extend(nodes.iter().cloned());
        }
        if let Some(count) = connection.get("totalCount").and_then(Value::as_i64) {
            total_count = count;
        }

        let page_info = connection.get("pageInfo");
        if let Some(page_info) = page_info {
            last_page_info = page_info.clone();
        }
        let has_next = page_info
            .and_then(|info| info.get("hasNextPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        after = page_info
            .and_then(|info| info.get("endCursor"))
            .and_then(Value::as_str)
            .map(str::to_string);

        if options.limit.is_none() {
            break;
        }
        if !fetch_unlimited && all_nodes.len() >= options.limit.unwrap() as usize {
            break;
        }
        if !has_next {
            break;
        }
    }

    // A complete result set can outgrow the cap on the final page.
    if !fetch_unlimited {
        if let Some(limit) = options.limit {
            all_nodes.truncate(limit as usize);
        }
    }

    Ok(json!({
        "nodes": all_nodes,
        "pageInfo": last_page_info,
        "totalCount": total_count,
    }))
}
