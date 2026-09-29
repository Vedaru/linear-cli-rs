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
