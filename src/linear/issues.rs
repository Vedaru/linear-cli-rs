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
    /// A saved view's filter, handed to `issues(filter:)` unchanged.
    ///
    /// When set it *is* the filter: `issue query` refuses the typed filter flags beside `--view`,
    /// so nothing above contributes to the document next to it.
    pub raw_filter: Option<Value>,
}

/// What a query filters on, in one place.
///
/// `fetch_issues_for_query` and `count_issues` must ask the same question - a count that
/// filtered differently from the list would answer a question nobody asked. The unit tests ask
/// it too, which is why this is `pub(crate)` rather than private to this module: a view's filter
/// being passed through untouched is a claim about *this* function, and nowhere else can see it.
pub(crate) fn issue_filter(options: &FetchIssuesForQueryOptions) -> Result<Map<String, Value>> {
    // A view's `filterData` is already the document `issues(filter:)` takes, so it goes through
    // untouched - translating it here would be a second place to get the filter language wrong,
    // and the first thing to drift from what the app shows for that view.
    if let Some(raw) = &options.raw_filter {
        return match raw {
            Value::Object(filter) => Ok(filter.clone()),
            _ => Err(CliError::validation("The view's filter is not a JSON object").suggestion(
                "A view's filterData is the API's `issues(filter:)` shape; fix it with `linear view update --filter`.",
            )),
        };
    }

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
    Ok(filter)
}

/// Whether the API will state this count itself.
///
/// It will for a team scope and *nothing else*: `Team.issueCount` takes no filter arguments
/// and counts non-archived issues, which is exactly what an unfiltered query returns. Any
/// filter at all - a state, a label, a date bound, an archived-including read - and the
/// number has to be counted rather than asked for.
pub(crate) fn count_is_stated(options: &FetchIssuesForQueryOptions) -> bool {
    let scoped = options.all_teams
        || options
            .team_keys
            .as_ref()
            .is_some_and(|keys| !keys.is_empty());
    scoped
        && options.state.is_none()
        && options.assignee.is_none()
        && !options.unassigned
        && options.project_id.is_none()
        && options.project_label.is_none()
        && options.cycle_id.is_none()
        && options.milestone_id.is_none()
        && options
            .label_names
            .as_ref()
            .map(|names| names.is_empty())
            .unwrap_or(true)
        && options.created_after.is_none()
        && options.updated_after.is_none()
        && !options.include_archived.unwrap_or(false)
        && options.raw_filter.is_none()
}

/// How many issues match, without fetching them.
///
/// Two mechanisms, because Linear states exactly one count and it takes no filters: a team's
/// total arrives as a single field (no nodes, no pages), and a *filtered* count has no
/// server-side equivalent, so it asks for `nodes { id }` - the smallest thing an issue can be -
/// and counts those, one request while the answer fits in one page and a cursor walk when it
/// does not.
pub fn count_issues(options: &FetchIssuesForQueryOptions) -> Result<i64> {
    let client = graphql::client()?;

    if count_is_stated(options) {
        let (query, variables) = if options.all_teams {
            (ALL_TEAM_ISSUE_COUNTS_QUERY, json!({}))
        } else {
            (
                TEAM_ISSUE_COUNTS_QUERY,
                json!({ "keys": options.team_keys.clone().unwrap_or_default() }),
            )
        };
        let data = client.request(query, variables)?;
        let total = data
            .get("teams")
            .and_then(|teams| teams.get("nodes"))
            .and_then(Value::as_array)
            .map(|teams| {
                teams
                    .iter()
                    .filter_map(|team| team.get("issueCount").and_then(Value::as_i64))
                    .sum::<i64>()
            })
            .unwrap_or(0);
        return Ok(total);
    }

    let filter = issue_filter(options)?;
    let sort = get_issue_sort_payload(options.sort.unwrap_or(IssueSort::Priority));
    let page_size: u32 = 100;
    let mut total: i64 = 0;
    let mut after: Option<String> = None;
    let mut has_next = true;

    while has_next {
        let mut variables = Map::new();
        if !filter.is_empty() {
            variables.insert("filter".to_string(), Value::Object(filter.clone()));
        }
        variables.insert("sort".to_string(), sort.clone());
        variables.insert("first".to_string(), json!(page_size));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }
        if let Some(include_archived) = options.include_archived {
            variables.insert("includeArchived".to_string(), json!(include_archived));
        }

        let data = client.request(COUNT_ISSUES_QUERY, Value::Object(variables))?;
        let connection = data
            .get("issues")
            .ok_or_else(|| CliError::cli("Linear API response did not contain issues"))?;
        total += connection
            .get("nodes")
            .and_then(Value::as_array)
            .map(|nodes| nodes.len() as i64)
            .unwrap_or(0);

        match connection.get("pageInfo") {
            Some(page_info) => {
                has_next = page_info
                    .get("hasNextPage")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                after = page_info
                    .get("endCursor")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            None => has_next = false,
        }
    }

    Ok(total)
}

/// Walk a query's pages, handing each page to `on_page` as it arrives, and answer the last
/// page's `pageInfo`.
///
/// This is the pagination loop, in one place: [`fetch_issues_for_query`] accumulates the
/// pages and `--ndjson` writes each one out the moment it lands. A stream that waited for
/// the last page before printing the first would be a buffered list with worse parsing, so
/// the loop has to be here rather than behind the fetcher.
///
/// The limit is applied *before* a page is handed over: a consumer that writes as it goes
/// cannot print more than was asked for and cannot un-print it afterwards.
pub fn stream_issues_for_query(
    options: &FetchIssuesForQueryOptions,
    mut on_page: impl FnMut(&[Value]) -> Result<()>,
) -> Result<Value> {
    let client = graphql::client()?;
    let filter = issue_filter(options)?;

    let fetch_all = options.limit == Some(0);
    let limit = options.limit.unwrap_or(50);
    let page_size: u32 = if fetch_all { 100 } else { limit.min(100) };
    let sort_payload = get_issue_sort_payload(options.sort.unwrap_or(IssueSort::Priority));

    let mut seen: usize = 0;
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

        let no_nodes: Vec<Value> = Vec::new();
        let page = connection
            .get("nodes")
            .and_then(Value::as_array)
            .unwrap_or(&no_nodes);
        let room = if fetch_all {
            page.len()
        } else {
            (limit as usize).saturating_sub(seen)
        };
        let taken: Vec<Value> = page.iter().take(room).cloned().collect();
        seen += taken.len();
        if !taken.is_empty() {
            on_page(&taken)?;
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

        if !fetch_all && seen >= limit as usize {
            break;
        }
    }

    Ok(last_page_info)
}

/// Issues across teams in a query filter, shaped `{ nodes, pageInfo }`.
///
/// `limit == Some(0)` fetches everything; `None` uses the default page size.
pub fn fetch_issues_for_query(options: &FetchIssuesForQueryOptions) -> Result<Value> {
    let mut all_issues: Vec<Value> = Vec::new();
    let page_info = stream_issues_for_query(options, |page| {
        all_issues.extend(page.iter().cloned());
        Ok(())
    })?;

    let mut nodes = all_issues;
    sort_issues_by_workflow_state(&mut nodes);

    Ok(json!({ "nodes": nodes, "pageInfo": page_info }))
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

// ---------------------------------------------------------------------------
// Issue transfer (`linear export issues` / `linear import issues`)
// ---------------------------------------------------------------------------

/// The issue fields a transfer needs, and the one document both directions read.
///
/// Deliberately *not* `FETCH_ISSUES_QUERY`: that listing answers the screen, so it carries
/// `priorityLabel`, `initials`, `avatarUrl` and the relation graph, and it has no `description` or
/// `dueDate` - the two fields an import can write. One document for the export and for the
/// "what does Linear already have" read means a row and the issue it came from are read the same
/// way, which is what makes re-importing an unchanged export a no-op.
const EXPORT_ISSUES_QUERY: &str = r#"
query ExportIssues($filter: IssueFilter, $sort: [IssueSortInput!], $first: Int, $after: String, $includeArchived: Boolean) {
  issues(filter: $filter, sort: $sort, first: $first, after: $after, includeArchived: $includeArchived) {
    nodes {
      id
      identifier
      title
      description
      priority
      estimate
      dueDate
      completedAt
      url
      createdAt
      updatedAt
      state {
        id
        name
        type
      }
      assignee {
        id
        name
        displayName
      }
      team {
        id
        key
        name
      }
      project {
        id
        name
      }
      projectMilestone {
        id
        name
      }
      cycle {
        id
        number
        name
      }
      labels {
        nodes {
          id
          name
        }
      }
      parent {
        id
        identifier
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

/// Ask Linear the same question `issue query` asks, and hand back pages as they arrive.
///
/// The streaming shape is the point: "large exports stream as NDJSON instead of buffering the
/// whole team" means the caller writes each page as it lands, and the closure is where it does.
pub fn stream_export_issues(
    options: &FetchIssuesForQueryOptions,
    mut on_page: impl FnMut(&[Value]) -> Result<()>,
) -> Result<Value> {
    let client = graphql::client()?;
    let filter = issue_filter(options)?;

    let fetch_all = options.limit == Some(0);
    let limit = options.limit.unwrap_or(50);
    let page_size: u32 = if fetch_all { 100 } else { limit.min(100) };
    let sort_payload = get_issue_sort_payload(options.sort.unwrap_or(IssueSort::Priority));

    let mut seen: usize = 0;
    let mut after: Option<String> = None;
    let mut last_page_info = json!({ "hasNextPage": false, "endCursor": null });

    loop {
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

        let data = client.request(EXPORT_ISSUES_QUERY, Value::Object(variables))?;
        let connection = data
            .get("issues")
            .ok_or_else(|| CliError::cli("Linear API response did not contain issues"))?;

        let no_nodes: Vec<Value> = Vec::new();
        let page = connection
            .get("nodes")
            .and_then(Value::as_array)
            .unwrap_or(&no_nodes);
        let room = if fetch_all {
            page.len()
        } else {
            (limit as usize).saturating_sub(seen)
        };
        let taken: Vec<Value> = page.iter().take(room).cloned().collect();
        seen += taken.len();
        if !taken.is_empty() {
            on_page(&taken)?;
        }

        let page_info = connection.get("pageInfo");
        let has_next = page_info
            .and_then(|info| info.get("hasNextPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(info) = page_info {
            last_page_info = info.clone();
        }
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
        if !fetch_all && seen >= limit as usize {
            break;
        }
    }

    Ok(last_page_info)
}

/// The same document, collected - shaped `{ nodes, pageInfo }`, exactly like `issue query --json`.
pub fn fetch_export_issues(options: &FetchIssuesForQueryOptions) -> Result<Value> {
    let mut nodes: Vec<Value> = Vec::new();
    let page_info = stream_export_issues(options, |page| {
        nodes.extend(page.iter().cloned());
        Ok(())
    })?;
    Ok(json!({ "nodes": nodes, "pageInfo": page_info }))
}
