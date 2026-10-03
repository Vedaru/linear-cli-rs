//! Custom views: a saved filter, and the four things one can do with it.
//!
//! A view's `filterData` *is* the JSON the API accepts as the `issues` `filter` argument, so
//! applying a view is a pass-through - resolve the view, hand its filterData to the query we
//! already send - rather than a field-by-field translation that could drift out of step with
//! Linear's filter language. Verified live before this module was written: a view created with
//! `{state: {type: {eq: "started"}}}` returns exactly the issues `--state started` returns.
//!
//! Two shapes are worth knowing because they are not symmetrical: `customViews` filters on
//! `id`, `name`, `modelName`, `team`, `creator` and `shared` but **not** `slugId`, so a
//! reference is resolved by id or by exact name; and `customView(id:)` is nullable, answering
//! `Entity not found: CustomView` for an id that does not exist rather than an empty node.

use super::prelude::*;
use super::*;

const LIST_VIEWS_QUERY: &str = r#"
query ListViews($filter: CustomViewFilter, $first: Int, $after: String, $includeArchived: Boolean) {
  customViews(filter: $filter, first: $first, after: $after, includeArchived: $includeArchived) {
    nodes {
      id
      name
      description
      slugId
      shared
      filterData
      team {
        id
        key
        name
      }
      owner {
        id
        name
        displayName
      }
      createdAt
      updatedAt
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

const GET_VIEW_QUERY: &str = r#"
query GetView($id: String!) {
  customView(id: $id) {
    id
    name
    description
    slugId
    shared
    filterData
    team {
      id
      key
      name
    }
    owner {
      id
      name
      displayName
    }
    createdAt
    updatedAt
  }
}
"#;

const CREATE_VIEW_MUTATION: &str = r#"
mutation CreateView($input: CustomViewCreateInput!) {
  customViewCreate(input: $input) {
    success
    customView {
      id
      name
      description
      slugId
      shared
      filterData
      team {
        id
        key
        name
      }
    }
  }
}
"#;

const UPDATE_VIEW_MUTATION: &str = r#"
mutation UpdateView($id: String!, $input: CustomViewUpdateInput!) {
  customViewUpdate(id: $id, input: $input) {
    success
    customView {
      id
      name
      description
      slugId
      shared
      filterData
      team {
        id
        key
        name
      }
    }
  }
}
"#;

const DELETE_VIEW_MUTATION: &str = r#"
mutation DeleteView($id: String!) {
  customViewDelete(id: $id) {
    success
  }
}
"#;

/// Every view the token can see, following pages, with the last page's `pageInfo`.
///
/// The nodes come back separately from the connection because both callers need them: a
/// listing prints `{nodes, pageInfo}` (`label list`'s shape), and the resolver only wants the
/// nodes.
pub fn list_views(limit: Option<u32>, include_archived: bool) -> Result<(Vec<Value>, Value)> {
    let client = graphql::client()?;
    let fetch_all = limit.is_none() || limit == Some(0);
    let limit = limit.unwrap_or(50);
    let page_size: u32 = if fetch_all { 50 } else { limit.min(50) };

    let mut views: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;
    let mut has_next = true;
    let mut last_page_info = json!({ "hasNextPage": false, "endCursor": null });

    while has_next {
        let mut variables = Map::new();
        variables.insert("first".to_string(), json!(page_size));
        variables.insert("includeArchived".to_string(), json!(include_archived));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }

        let data = client.request(LIST_VIEWS_QUERY, Value::Object(variables))?;
        let connection = data
            .get("customViews")
            .ok_or_else(|| CliError::cli("Linear API response did not contain customViews"))?;

        if let Some(nodes) = connection.get("nodes").and_then(Value::as_array) {
            views.extend(nodes.iter().cloned());
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

        if !fetch_all && views.len() >= limit as usize {
            break;
        }
    }

    views.truncate(limit as usize);
    Ok((views, last_page_info))
}

/// One view, by UUID or by exact name.
pub fn resolve_view(reference: &str) -> Result<Value> {
    let client = graphql::client()?;

    if is_linear_uuid(reference) {
        let data = client.request(GET_VIEW_QUERY, json!({ "id": reference }))?;
        let view = data.get("customView").cloned().unwrap_or(Value::Null);
        if !view.is_null() {
            return Ok(view);
        }
        return Err(view_not_found_error(reference)?);
    }

    let filter = json!({ "name": { "eq": reference } });
    let data = client.request(
        LIST_VIEWS_QUERY,
        json!({ "filter": filter, "first": 50, "includeArchived": false }),
    )?;
    let nodes = data
        .get("customViews")
        .and_then(|connection| connection.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    match nodes.len() {
        0 => Err(view_not_found_error(reference)?),
        1 => Ok(nodes[0].clone()),
        // A view name is not unique across teams, so a name that matches twice is a question
        // the caller has to answer - not a coin toss we make for them.
        _ => Err(
            CliError::validation(format!("More than one view is named \"{reference}\""))
                .suggestion("Pass the view's id, or a name that is unique."),
        ),
    }
}

/// The filter a view saves, ready to hand to `issues(filter:)` unchanged.
pub fn view_filter(reference: &str) -> Result<Value> {
    let view = resolve_view(reference)?;
    match view.get("filterData") {
        Some(filter) if !filter.is_null() => Ok(filter.clone()),
        // A view with no filterData is a view that selects nothing: saying so is better than
        // sending `filter: null`, which would list everything and look like it worked.
        _ => Err(
            CliError::cli(format!("The view \"{reference}\" has no filter to apply"))
                .suggestion("List the views to see which ones carry a filter: `linear view list`."),
        ),
    }
}

pub fn create_view(input: Value) -> Result<Value> {
    let client = graphql::client()?;
    let data = client.request(CREATE_VIEW_MUTATION, json!({ "input": input }))?;
    mutation_result(data, "customViewCreate", "customView")
}

pub fn update_view(id: &str, input: Value) -> Result<Value> {
    let client = graphql::client()?;
    let data = client.request(UPDATE_VIEW_MUTATION, json!({ "id": id, "input": input }))?;
    mutation_result(data, "customViewUpdate", "customView")
}

pub fn delete_view(id: &str) -> Result<Value> {
    let client = graphql::client()?;
    let data = client.request(DELETE_VIEW_MUTATION, json!({ "id": id }))?;
    let result = data
        .get("customViewDelete")
        .ok_or_else(|| CliError::cli("Linear API response did not contain customViewDelete"))?;
    if result.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli(format!(
            "Linear refused to delete the view {id}"
        )));
    }
    Ok(json!({ "success": true, "id": id }))
}

/// The payload a view mutation returns, refusing a `success: false` rather than reporting one.
fn mutation_result(data: Value, mutation: &str, field: &str) -> Result<Value> {
    let result = data
        .get(mutation)
        .ok_or_else(|| CliError::cli(format!("Linear API response did not contain {mutation}")))?;
    if result.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli(format!(
            "Linear refused the {mutation} request"
        )));
    }
    Ok(result
        .get(field)
        .cloned()
        .unwrap_or_else(|| json!({ "success": true })))
}

/// The error for a view that is not there, naming the ones that are.
fn view_not_found_error(reference: &str) -> Result<CliError> {
    let known: Vec<String> = list_views(None, false)
        .map(|(views, _)| views)
        .unwrap_or_default()
        .iter()
        .filter_map(|view| view.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();

    let suggestion = if known.is_empty() {
        "This workspace has no custom views yet: create one with `linear view create`.".to_string()
    } else {
        format!("Known views: {}", known.join(", "))
    };

    Ok(CliError::not_found("CustomView", reference).suggestion(suggestion))
}
