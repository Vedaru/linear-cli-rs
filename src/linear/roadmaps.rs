//! Roadmaps, read-only, and the relation that holds their projects.
//!
//! **Why there is no write half here.** Linear deprecated `Roadmap` *and* `RoadmapToProject`
//! ("Roadmaps are deprecated, use initiatives instead"), and the API refuses the writes by name:
//! `roadmapCreate`, `roadmapArchive`, `roadmapDelete` and `roadmapToProjectDelete` all answer with
//! that sentence, measured against the live API before this module was written. A `roadmap create`
//! would therefore be a command that can only fail - the same reason `issueLabelRetire` is
//! deliberately not wrapped. `initiative` is the successor, and its write half (including
//! `initiative add-project` / `remove-project`) is wrapped next door.
//!
//! What is left is reading the roadmaps a workspace already has, and the projects on them. The
//! projects come from the relation (`roadmap.projects`) rather than from listing every project and
//! filtering client-side: the API already knows which projects belong to a roadmap.
//!
//! Two shapes worth knowing, both established by probing the live API rather than inferred from the
//! schema: `roadmaps` takes **no `filter` argument** - a `RoadmapFilter` type exists, but the field
//! does not accept it - so a roadmap is found by id, or by matching a name client-side; and
//! `roadmap(id:)` answers `Entity not found: Roadmap` rather than a null node.

use super::prelude::*;
use super::*;

const LIST_ROADMAPS_QUERY: &str = r#"
query ListRoadmaps($first: Int, $after: String, $includeArchived: Boolean) {
  roadmaps(first: $first, after: $after, includeArchived: $includeArchived) {
    nodes {
      id
      name
      description
      color
      slugId
      archivedAt
      createdAt
      updatedAt
      owner {
        id
        displayName
      }
      creator {
        id
        displayName
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

const GET_ROADMAP_QUERY: &str = r#"
query GetRoadmap($id: String!) {
  roadmap(id: $id) {
    id
    name
    description
    color
    slugId
    archivedAt
    createdAt
    updatedAt
    owner {
      id
      displayName
    }
    creator {
      id
      displayName
    }
  }
}
"#;

/// The projects on a roadmap, as the relation answers them.
const ROADMAP_PROJECTS_QUERY: &str = r#"
query RoadmapProjects($id: String!, $first: Int, $after: String) {
  roadmap(id: $id) {
    projects(first: $first, after: $after) {
      nodes {
        id
        name
        state
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

/// Every roadmap the token can see, following pages, with the last page's `pageInfo`.
///
/// The projects are deliberately *not* included: a listing shows one line per roadmap, and asking
/// for every project on every roadmap to print a count nobody asked for is the kind of request that
/// makes a listing slow. `view_roadmap` reads them, one roadmap at a time.
pub fn list_roadmaps(limit: Option<u32>, include_archived: bool) -> Result<(Vec<Value>, Value)> {
    let client = graphql::client()?;
    let fetch_all = limit.is_none() || limit == Some(0);
    let limit = limit.unwrap_or(50);
    let page_size: u32 = if fetch_all { 50 } else { limit.min(50) };

    let mut roadmaps: Vec<Value> = Vec::new();
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

        let data = client.request(LIST_ROADMAPS_QUERY, Value::Object(variables))?;
        let connection = data
            .get("roadmaps")
            .ok_or_else(|| CliError::cli("Linear API response did not contain roadmaps"))?;

        if let Some(nodes) = connection.get("nodes").and_then(Value::as_array) {
            roadmaps.extend(nodes.iter().cloned());
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

        if !fetch_all && roadmaps.len() >= limit as usize {
            break;
        }
    }

    roadmaps.truncate(limit as usize);
    Ok((roadmaps, last_page_info))
}

/// One roadmap by id, or by exact name.
///
/// A name is matched against the listing because `roadmaps` has no filter argument to ask with -
/// verified live, the API rejects `filter` on that field. A name that matches more than once is a
/// question for the caller, not a coin toss we make for them.
pub fn resolve_roadmap(reference: &str) -> Result<Value> {
    let client = graphql::client()?;

    if is_linear_uuid(reference) {
        return get_roadmap(&client, reference);
    }

    let (roadmaps, _) = list_roadmaps(None, true)?;
    let matches: Vec<&Value> = roadmaps
        .iter()
        .filter(|roadmap| roadmap.get("name").and_then(Value::as_str) == Some(reference))
        .collect();

    match matches.len() {
        0 => Err(roadmap_not_found_error(reference, &roadmaps)?),
        1 => Ok(matches[0].clone()),
        _ => Err(
            CliError::validation(format!("More than one roadmap is named \"{reference}\""))
                .suggestion("Pass the roadmap's id, or a name that is unique."),
        ),
    }
}

/// One roadmap by id, with the projects on it read from the relation.
pub fn view_roadmap(reference: &str) -> Result<Value> {
    let client = graphql::client()?;
    let roadmap = resolve_roadmap(reference)?;
    let id = roadmap
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::cli("The roadmap has no id"))?
        .to_string();

    // Read after resolving, so a name that resolved to a listing node still gets its projects -
    // and so a caller that passed an id pays for exactly one extra request.
    let mut variables = Map::new();
    variables.insert("id".to_string(), json!(id));
    let (projects, page_info) = client.paginate_connection_page(
        ROADMAP_PROJECTS_QUERY,
        variables,
        &["roadmap", "projects"],
    )?;

    let mut roadmap = roadmap;
    if let Some(object) = roadmap.as_object_mut() {
        object.insert(
            "projects".to_string(),
            json!({ "nodes": projects, "pageInfo": page_info }),
        );
    }
    Ok(roadmap)
}

fn get_roadmap(client: &graphql::Client, id: &str) -> Result<Value> {
    let data = client.request(GET_ROADMAP_QUERY, json!({ "id": id }))?;
    data.get("roadmap")
        .filter(|roadmap| !roadmap.is_null())
        .cloned()
        .ok_or_else(|| CliError::not_found("Roadmap", id))
}

/// The error for a roadmap that is not there, naming the ones that are.
///
/// This workspace has none, and the API will not create one, so the suggestion has to say what to
/// do instead rather than only what was missing.
fn roadmap_not_found_error(reference: &str, known: &[Value]) -> Result<CliError> {
    let names: Vec<String> = known
        .iter()
        .filter_map(|roadmap| {
            roadmap
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();

    let suggestion = if names.is_empty() {
        "This workspace has no roadmaps. Linear deprecated them - use `linear initiative list` (and `linear initiative add-project`) instead.".to_string()
    } else {
        format!("Known roadmaps: {}", names.join(", "))
    };

    Ok(CliError::not_found("Roadmap", reference).suggestion(suggestion))
}
