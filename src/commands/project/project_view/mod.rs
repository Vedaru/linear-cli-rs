//! `linear project view` — port of `src/commands/project/project-view.ts`.
//!
//! The project is rendered as raw Linear-flavored Markdown (see AGENTS.md —
//! there is no Rust equivalent of `@littletof/charmd`), so the non-TTY branch
//! upstream already uses is the only branch here. `--json` emits the raw
//! GraphQL project object with its issue connection paginated to exhaustion.
//!
//! This module self-wraps every failure with `Failed to view project`, matching
//! upstream's single `handleError` around the whole action; the group `mod.rs`
//! must not wrap it again.

use std::io::IsTerminal;

use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{actions, graphql, linear, output, prompt};

mod markdown;

const CONNECTION_PAGE_SIZE: i64 = 250;
const PICKER_PAGE_SIZE: i64 = 100;

const GET_PROJECT_DETAILS_QUERY: &str = r#"
query GetProjectDetails($id: String!, $first: Int!) {
  project(id: $id) {
    id
    name
    identifier
    description
    content
    slugId
    icon
    color
    progress
    scope
    url
    priority
    health
    healthUpdatedAt
    startDate
    startDateResolution
    targetDate
    targetDateResolution
    startedAt
    completedAt
    canceledAt
    archivedAt
    autoArchivedAt
    createdAt
    updatedAt
    status {
      id
      name
      color
      type
      position
    }
    creator {
      id
      name
      displayName
    }
    lead {
      id
      name
      displayName
    }
    teams(first: $first) {
      nodes {
        id
        key
        name
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
    labels(first: $first) {
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
    members(first: $first) {
      nodes {
        id
        name
        displayName
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
    initiatives(first: $first) {
      nodes {
        id
        name
        url
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
    projectMilestones(first: $first) {
      nodes {
        id
        name
        description
        targetDate
        progress
        status
        sortOrder
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
    externalLinks(first: $first) {
      nodes {
        id
        label
        url
        sortOrder
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
    documents(first: $first) {
      nodes {
        id
        title
        url
        sortOrder
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
    attachments(first: $first) {
      nodes {
        id
        title
        subtitle
        url
        sourceType
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
    relations(first: $first) {
      nodes {
        id
        type
        anchorType
        relatedAnchorType
        projectMilestone {
          id
          name
        }
        relatedProject {
          id
          name
          url
        }
        relatedProjectMilestone {
          id
          name
        }
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
    inverseRelations(first: $first) {
      nodes {
        id
        type
        anchorType
        relatedAnchorType
        projectMilestone {
          id
          name
        }
        project {
          id
          name
          url
        }
        relatedProjectMilestone {
          id
          name
        }
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
    issues(first: $first) {
      nodes {
        id
        identifier
        title
        state {
          id
          name
          type
        }
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
    lastUpdate {
      id
      body
      health
      createdAt
      user {
        id
        name
        displayName
      }
    }
  }
}
"#;

const GET_PROJECT_ISSUES_PAGE_QUERY: &str = r#"
query GetProjectIssuesPage($id: String!, $first: Int!, $after: String!) {
  project(id: $id) {
    id
    issues(first: $first, after: $after) {
      nodes {
        id
        identifier
        title
        state {
          id
          name
          type
        }
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

const GET_PROJECTS_FOR_PICKER_QUERY: &str = r#"
query GetProjectsForPicker($filter: ProjectFilter, $first: Int!, $after: String) {
  projects(filter: $filter, first: $first, after: $after) {
    nodes {
      id
      name
      slugId
      status {
        name
      }
      teams(first: 10) {
        nodes {
          key
        }
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

#[derive(clap::Args, Debug)]
pub struct ProjectViewArgs {
    /// Project ID, URL, slug ID, or exact name; omit to pick from a list
    #[arg(value_name = "projectId")]
    pub project: Option<String>,
    /// Open in web browser
    #[arg(short = 'w', long)]
    pub web: bool,
    /// Open in Linear.app
    #[arg(short = 'a', long)]
    pub app: bool,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
    /// Disable automatic paging for long output
    #[arg(long = "no-pager", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub pager: bool,
}

pub fn run(args: ProjectViewArgs) -> Result<()> {
    view(&args).map_err(|error| error.with_context("Failed to view project"))
}

fn view(args: &ProjectViewArgs) -> Result<()> {
    // With no argument the picker already hands back a UUID; with one, resolve
    // the reference (UUID, URL, slug, or name) to a UUID up front so a name
    // works everywhere the command accepts an identifier.
    let (reference, resolved_id) = match &args.project {
        Some(project) => (project.clone(), linear::resolve_project_id(project)?),
        None => {
            let selected = select_project(args.json)?;
            (selected.clone(), selected)
        }
    };

    if args.web || args.app {
        return actions::open_project_page(&resolved_id, args.app);
    }

    let project = fetch_project_details(&resolved_id, &reference)?;

    if args.json {
        output::print_json(&project);
        return Ok(());
    }

    output::line(&markdown::format_project_as_markdown(&project)?);
    Ok(())
}

/// Fetch a project and exhaust its issue connection.
///
/// A cursor that stops advancing means the API told us there is another page
/// but gave us no way to ask for it, so both a missing and a repeated cursor
/// are refused rather than silently under-reporting issue counts.
fn fetch_project_details(project_id: &str, original_input: &str) -> Result<Value> {
    let client = graphql::client()?;
    let result = client.request(
        GET_PROJECT_DETAILS_QUERY,
        json!({ "id": project_id, "first": CONNECTION_PAGE_SIZE }),
    )?;

    let Some(mut project) = result
        .get("project")
        .filter(|value| !value.is_null())
        .cloned()
    else {
        return Err(CliError::not_found("Project", original_input));
    };

    let project_name = project
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let mut issues = project
        .pointer("/issues/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut page_info = project
        .pointer("/issues/pageInfo")
        .cloned()
        .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));
    let mut cursor = page_info.get("endCursor").cloned();

    while page_info
        .get("hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let current_cursor = match cursor.as_ref().filter(|value| !value.is_null()) {
            Some(cursor) => cursor.clone(),
            None => {
                return Err(CliError::cli(format!(
                    "Linear reported more issues for project {project_name} but returned no cursor to fetch them."
                ))
                .suggestion("Retry, or report this if it keeps happening."));
            }
        };

        let page = client.request(
            GET_PROJECT_ISSUES_PAGE_QUERY,
            json!({ "id": project_id, "first": CONNECTION_PAGE_SIZE, "after": current_cursor }),
        )?;
        let Some(page_project) = page.get("project").filter(|value| !value.is_null()) else {
            return Err(CliError::not_found("Project", original_input));
        };

        if let Some(nodes) = page_project
            .pointer("/issues/nodes")
            .and_then(Value::as_array)
        {
            issues.extend(nodes.iter().cloned());
        }
        let next_page_info = page_project
            .pointer("/issues/pageInfo")
            .cloned()
            .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));

        let next_cursor = next_page_info.get("endCursor").cloned();
        if next_page_info
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && next_cursor.as_ref() == Some(&current_cursor)
        {
            return Err(CliError::cli(format!(
                "Linear returned the same issue cursor twice for project {project_name}."
            ))
            .suggestion("Retry, or report this if it keeps happening."));
        }

        page_info = next_page_info;
        cursor = next_cursor;
    }

    if let Some(issues_value) = project.get_mut("issues").and_then(Value::as_object_mut) {
        issues_value.insert("nodes".to_string(), Value::Array(issues));
        issues_value.insert("pageInfo".to_string(), page_info);
    }

    Ok(project)
}

/// Resolve the project to act on when no argument was given.
///
/// Prompting is only ever right when a person is actually there to answer, so
/// every other case errors up front — before any network call — rather than
/// hanging a pipeline on a prompt nobody can see or interleaving prompt output
/// with JSON on stdout.
fn select_project(json_output: bool) -> Result<String> {
    if json_output {
        return Err(CliError::validation("A project is required with --json").suggestion(
            "Pass a project UUID, slug ID, or exact name, or drop --json to pick one from a list.",
        ));
    }

    // Some CI runners allocate a pseudo-terminal, which makes both isTerminal()
    // checks pass even though nobody is there to answer the prompt. CI is
    // therefore treated as non-interactive regardless of what the tty looks like.
    let in_ci = std::env::var("CI")
        .map(|value| !value.is_empty() && value != "false")
        .unwrap_or(false);
    let interactive = !in_ci && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if !interactive {
        return Err(CliError::validation("No project specified").suggestion(
            "Pass a project UUID, slug ID, or exact name. Running `linear project view` with no argument picks from a list, but only on a terminal.",
        ));
    }

    let team_key = linear::get_team_key()?;
    let projects = fetch_projects_for_picker(team_key.as_deref())?;
    if projects.is_empty() {
        let (identifier, suggestion) = match &team_key {
            Some(team_key) => (
                format!("team {team_key}"),
                format!(
                    "No projects are accessible to team {team_key}. Check `linear project list --all-teams`, or create one with `linear project create`."
                ),
            ),
            None => (
                "this workspace".to_string(),
                "Create one with `linear project create`.".to_string(),
            ),
        };
        return Err(CliError::not_found("Project", &identifier).suggestion(suggestion));
    }

    let options = build_project_picker_options(&projects);
    let labels: Vec<String> = options.iter().map(|(_, display)| display.clone()).collect();
    let selected = prompt::select("Select a project", &labels)?;
    Ok(options[selected].0.clone())
}

/// Label each project for the picker. The name is not unique and a project can
/// span several teams, so status, team keys, and slug stay in the label; the
/// value is always the UUID, so what the user sees cannot change what opens.
fn build_project_picker_options(projects: &[Value]) -> Vec<(String, String)> {
    let mut sorted: Vec<&Value> = projects.iter().collect();
    sorted.sort_by(|a, b| {
        str_at(a, "/name")
            .to_lowercase()
            .cmp(&str_at(b, "/name").to_lowercase())
            .then_with(|| str_at(a, "/slugId").cmp(str_at(b, "/slugId")))
            .then_with(|| str_at(a, "/id").cmp(str_at(b, "/id")))
    });

    sorted
        .into_iter()
        .map(|project| {
            let teams = project
                .pointer("/teams/nodes")
                .and_then(Value::as_array)
                .map(|nodes| {
                    nodes
                        .iter()
                        .filter_map(|node| node.get("key").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let mut parts = vec![
                str_at(project, "/name").to_string(),
                str_at(project, "/status/name").to_string(),
            ];
            if !teams.is_empty() {
                parts.push(teams);
            }
            parts.push(str_at(project, "/slugId").to_string());
            (str_at(project, "/id").to_string(), parts.join("  ·  "))
        })
        .collect()
}

/// Fetch every project the picker can offer. The prompt filters client-side, so
/// anything left unfetched is simply undiscoverable — hence every page rather
/// than a cap. Scope matches `project list`: the configured team when there is
/// one, otherwise everything accessible.
fn fetch_projects_for_picker(team_key: Option<&str>) -> Result<Vec<Value>> {
    let client = graphql::client()?;
    let mut projects: Vec<Value> = Vec::new();
    let mut after: Option<String> = None;

    loop {
        let mut variables = Map::new();
        if let Some(team_key) = team_key {
            variables.insert(
                "filter".to_string(),
                json!({ "accessibleTeams": { "some": { "key": { "eq": team_key } } } }),
            );
        }
        variables.insert("first".to_string(), json!(PICKER_PAGE_SIZE));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }

        let data = client.request(GET_PROJECTS_FOR_PICKER_QUERY, Value::Object(variables))?;
        let connection = data.get("projects");
        if let Some(nodes) = connection
            .and_then(|value| value.get("nodes"))
            .and_then(Value::as_array)
        {
            projects.extend(nodes.iter().cloned());
        }

        let page_info = connection
            .and_then(|value| value.get("pageInfo"))
            .cloned()
            .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));
        if !page_info
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            break;
        }

        let end_cursor = page_info
            .get("endCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        match end_cursor {
            Some(cursor) if Some(&cursor) != after.as_ref() => after = Some(cursor),
            _ => {
                return Err(CliError::cli(
                    "Linear reported more projects but returned no new cursor to fetch them.",
                )
                .suggestion("Retry, or pass a project explicitly."));
            }
        }
    }

    Ok(projects)
}


fn str_at<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}
