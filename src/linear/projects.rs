use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

/// Look up a project by slug ID only. A URL's slug must not fall through to a
/// name lookup that a same-named project could win.
pub fn find_project_id_by_slug(slug_id: &str) -> Result<Option<String>> {
    let client = graphql::client()?;
    let data = client.request(FIND_PROJECT_BY_SLUG_QUERY, json!({ "slugId": slug_id }))?;
    Ok(data
        .get("projects")
        .and_then(|projects| projects.get("nodes"))
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.first())
        .and_then(|node| node.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string))
}

/// Resolve a project to its UUID by URL, UUID, or exact name. `None` when
/// nothing matches; a name that matches several projects is an error.
pub fn get_project_id_by_name(input: &str) -> Result<Option<String>> {
    if let Some(LinearUrlRef::Project { slug_id, .. }) =
        expect_linear_url_kind(input, "project", "a project URL, UUID, or exact name")?
    {
        return find_project_id_by_slug(&slug_id);
    }

    if is_linear_uuid(input) {
        return Ok(Some(input.to_string()));
    }

    let client = graphql::client()?;
    let data = client.request(GET_PROJECT_BY_NAME_QUERY, json!({ "name": input }))?;
    let nodes = data
        .get("projects")
        .and_then(|projects| projects.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if nodes.len() > 1 {
        let listing = nodes
            .iter()
            .map(|node| {
                format!(
                    "  {} ({})",
                    node.get("name").and_then(Value::as_str).unwrap_or(""),
                    node.get("id").and_then(Value::as_str).unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(CliError::validation(format!(
            "Project \"{input}\" is ambiguous; it matches multiple projects:\n{listing}"
        ))
        .suggestion("Pass the project's UUID instead."));
    }

    if let Some(node) = nodes.first() {
        return Ok(node.get("id").and_then(Value::as_str).map(str::to_string));
    }

    find_project_id_by_slug(input)
}

/// Resolve a project to its UUID, erroring when nothing matches.
pub fn resolve_project_id(input: &str) -> Result<String> {
    get_project_id_by_name(input)?.ok_or_else(|| {
        CliError::not_found("Project", input)
            .suggestion("Pass a project UUID, project URL, or exact project name.")
    })
}

/// Projects whose name contains `name`, as `(id, name)` pairs in server order.
pub fn get_project_options_by_name(name: &str) -> Result<Vec<(String, String)>> {
    let client = graphql::client()?;
    let data = client.request(GET_PROJECTS_BY_NAME_QUERY, json!({ "name": name }))?;
    Ok(data
        .get("projects")
        .and_then(|projects| projects.get("nodes"))
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
        .unwrap_or_default())
}

/// A team's projects, sorted by name.
pub fn get_projects_for_team(team_id: &str) -> Result<Vec<Value>> {
    let client = graphql::client()?;
    let data = client.request(GET_PROJECTS_FOR_TEAM_QUERY, json!({ "teamId": team_id }))?;
    let mut projects = data
        .get("team")
        .and_then(|team| team.get("projects"))
        .and_then(|projects| projects.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    projects.sort_by_key(name_lowercase);
    Ok(projects)
}

// ---------------------------------------------------------------------------
// The project's members and labels, and the two reversible retirement verbs
// ---------------------------------------------------------------------------
//
// `projectDelete` is what `project delete` already runs, and it *trashes* rather than destroys:
// the API's own doc says the project "can be restored later with projectUnarchive". So a project's
// retirement is reversible either way, and the reason to have `archive`/`unarchive` as separate
// verbs is that nobody should have to reach for the destructive-sounding name to do it.
//
// `projectArchive` is marked `@deprecated(reason: "Deprecated in favor of projectDelete.")` in the
// schema and still answers (probed live with a bogus id: `Entity not found: Project`, not a
// deprecation refusal) - and it is the only call that archives *without* trashing, since
// `projectDelete` always trashes. `--trash` on the command is the successor's behaviour by hand.

const GET_PROJECT_MEMBERS_QUERY: &str = r#"
query GetProjectMembers($id: String!, $first: Int, $after: String) {
  project(id: $id) {
    id
    name
    members(first: $first, after: $after) {
      nodes {
        id
        name
        displayName
        email
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

const GET_PROJECT_LABELS_QUERY: &str = r#"
query GetProjectLabels($id: String!, $first: Int, $after: String) {
  project(id: $id) {
    id
    name
    labels(first: $first, after: $after) {
      nodes {
        id
        name
        color
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

const FIND_PROJECT_LABEL_QUERY: &str = r#"
query FindProjectLabel($name: String!) {
  projectLabels(filter: { name: { eq: $name } }, first: 2) {
    nodes {
      id
      name
      color
    }
  }
}
"#;

const UPDATE_PROJECT_MEMBERSHIP_MUTATION: &str = r#"
mutation UpdateProjectMembership($id: String!, $input: ProjectUpdateInput!) {
  projectUpdate(id: $id, input: $input) {
    success
    project {
      id
      name
      labelIds: labels(first: 50) {
        nodes {
          id
          name
          color
        }
      }
      members(first: 50) {
        nodes {
          id
          name
          displayName
        }
      }
    }
  }
}
"#;

const ARCHIVE_PROJECT_MUTATION: &str = r#"
mutation ArchiveProject($id: String!, $trash: Boolean) {
  projectArchive(id: $id, trash: $trash) {
    success
    entity {
      id
      name
    }
  }
}
"#;

const UNARCHIVE_PROJECT_MUTATION: &str = r#"
mutation UnarchiveProject($id: String!) {
  projectUnarchive(id: $id) {
    success
    entity {
      id
      name
    }
  }
}
"#;

/// The one variable every project read here takes.
fn id_variables(project_id: &str) -> Map<String, Value> {
    let mut variables = Map::new();
    variables.insert("id".to_string(), json!(project_id));
    variables
}

/// A project's label ids, as the write input wants them.
pub fn get_project_label_ids(project_id: &str) -> Result<Vec<String>> {
    let (labels, _) = get_project_labels(project_id)?;
    Ok(labels
        .iter()
        .filter_map(|label| label.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect())
}

/// A project's members and the last page's `pageInfo`.
pub fn get_project_members(project_id: &str) -> Result<(Vec<Value>, Value)> {
    let client = graphql::client()?;
    client.paginate_connection_page(
        GET_PROJECT_MEMBERS_QUERY,
        id_variables(project_id),
        &["project", "members"],
    )
}

/// A project's labels and the last page's `pageInfo`.
pub fn get_project_labels(project_id: &str) -> Result<(Vec<Value>, Value)> {
    let client = graphql::client()?;
    client.paginate_connection_page(
        GET_PROJECT_LABELS_QUERY,
        id_variables(project_id),
        &["project", "labels"],
    )
}

/// A project label's id, by exact name. Two labels with the same name is a question for the
/// caller, not a coin toss: the API's own filter answers both.
pub fn resolve_project_label_id(reference: &str) -> Result<String> {
    if is_linear_uuid(reference) {
        return Ok(reference.to_string());
    }
    let client = graphql::client()?;
    let data = client.request(FIND_PROJECT_LABEL_QUERY, json!({ "name": reference }))?;
    let nodes = data
        .get("projectLabels")
        .and_then(|connection| connection.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    match nodes.len() {
        0 => Err(CliError::not_found("ProjectLabel", reference)
            .suggestion("List the workspace's project labels with `linear project label list`.")),
        1 => Ok(nodes[0]
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()),
        _ => Err(CliError::validation(format!(
            "More than one project label is named \"{reference}\""
        ))
        .suggestion("Pass the label's id instead.")),
    }
}

/// Set a project's label set, or its member set, through `projectUpdate`.
///
/// The input carries the **whole** set either way - the API has no incremental form - so the
/// increment happens here, against what the project had a moment ago, and the caller is the one
/// that decides whether the difference is an edit or a replacement.
pub fn update_project_set(
    project_id: &str,
    label_ids: Option<Vec<String>>,
    member_ids: Option<Vec<String>>,
) -> Result<Value> {
    let mut input = Map::new();
    if let Some(ids) = label_ids {
        input.insert("labelIds".to_string(), json!(ids));
    }
    if let Some(ids) = member_ids {
        input.insert("memberIds".to_string(), json!(ids));
    }
    let client = graphql::client()?;
    let data = client.request(
        UPDATE_PROJECT_MEMBERSHIP_MUTATION,
        json!({ "id": project_id, "input": Value::Object(input) }),
    )?;
    let result = data
        .get("projectUpdate")
        .ok_or_else(|| CliError::cli("Linear API response did not contain projectUpdate"))?;
    if result.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli("Failed to update project"));
    }
    Ok(data)
}

/// Archive a project, or trash it - both reversible: `project unarchive` is the way back from
/// either.
pub fn archive_project(project_id: &str, trash: bool) -> Result<Value> {
    let client = graphql::client()?;
    let data = client.request(
        ARCHIVE_PROJECT_MUTATION,
        json!({ "id": project_id, "trash": trash }),
    )?;
    archive_result(data, "projectArchive")
}

/// Restore a project that was archived or trashed.
pub fn unarchive_project(project_id: &str) -> Result<Value> {
    let client = graphql::client()?;
    let data = client.request(UNARCHIVE_PROJECT_MUTATION, json!({ "id": project_id }))?;
    archive_result(data, "projectUnarchive")
}

/// The payload an archive/unarchive answers, refusing a `success: false`.
fn archive_result(data: Value, mutation: &str) -> Result<Value> {
    let result = data
        .get(mutation)
        .ok_or_else(|| CliError::cli(format!("Linear API response did not contain {mutation}")))?;
    if result.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli(format!(
            "Linear refused the {mutation} request"
        )));
    }
    Ok(data)
}
