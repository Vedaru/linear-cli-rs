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

use crate::display;
use crate::errors::{CliError, Result};
use crate::{actions, graphql, linear, output, prompt};

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

    output::line(&format_project_as_markdown(&project)?);
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

    if let Some(issues_value) = project
        .get_mut("issues")
        .and_then(Value::as_object_mut)
    {
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
    let interactive =
        !in_ci && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
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
    let labels: Vec<String> = options
        .iter()
        .map(|(_, display)| display.clone())
        .collect();
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

// ---------------------------------------------------------------------------
// Markdown rendering
// ---------------------------------------------------------------------------

/// The meta line lists its connections inline with no room for a truncation
/// note, so a truncated one ends with an ellipsis instead.
fn join_connection(values: &[String], page_info: &Value) -> String {
    let joined = values.join(", ");
    if page_info
        .get("hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        format!("{joined}, …")
    } else {
        joined
    }
}

/// A connection that could not be shown in full says so rather than trail off.
fn truncation_note(page_info: &Value) -> String {
    if page_info
        .get("hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        format!("\n_…and more (showing the first {CONNECTION_PAGE_SIZE})._\n")
    } else {
        String::new()
    }
}

/// `user.displayName || user.name`, or `None` when the user is absent.
fn display_name(value: &Value, pointer: &str) -> Option<String> {
    let user = value.pointer(pointer)?;
    if user.is_null() {
        return None;
    }
    match user.get("displayName").and_then(Value::as_str) {
        Some(display) if !display.is_empty() => Some(display.to_string()),
        _ => user.get("name").and_then(Value::as_str).map(str::to_string),
    }
}

/// A coarse date is stored as a day plus a resolution, so the resolution is
/// appended when present so "sometime in Q4" is not read as a deadline.
fn format_project_date(value: &Value, date_key: &str, resolution_key: &str) -> Option<String> {
    let date = value.get(date_key).and_then(Value::as_str)?;
    match value.get(resolution_key).and_then(Value::as_str) {
        Some(resolution) => Some(format!("{date} ({resolution})")),
        None => Some(date.to_string()),
    }
}

/// `Project.progress` is a 0-1 ratio.
fn format_ratio_as_percent(ratio: f64) -> String {
    format!("{}%", (ratio * 100.0).round() as i64)
}

/// `ProjectMilestone.progress` arrives as 0-100, unlike the identically named
/// ratio on `Project`.
fn format_milestone_percent(percent: f64) -> String {
    format!("{}%", percent.round() as i64)
}

/// Compare two `Float!` sort keys, refusing values the schema says cannot
/// happen: a scrambled section order is far harder to notice than an error.
fn compare_sort_order(a: f64, b: f64, field: &str, project_name: &str) -> Result<()> {
    if !a.is_finite() || !b.is_finite() {
        return Err(CliError::cli(format!(
            "Linear returned a non-numeric {field} for project {project_name}."
        ))
        .suggestion("Retry, or report this if it keeps happening."));
    }
    Ok(())
}

fn by_sort_order<'a>(
    nodes: &'a [Value],
    field: &str,
    project_name: &str,
) -> Result<Vec<&'a Value>> {
    let mut sorted: Vec<&Value> = nodes.iter().collect();
    for node in &sorted {
        let order = node.get("sortOrder").and_then(Value::as_f64);
        if order.map(|value| value.is_finite()) != Some(true) {
            return Err(CliError::cli(format!(
                "Linear returned a non-numeric {field} for project {project_name}."
            ))
            .suggestion("Retry, or report this if it keeps happening."));
        }
    }
    sorted.sort_by(|a, b| {
        let a_order = a.get("sortOrder").and_then(Value::as_f64).unwrap_or(0.0);
        let b_order = b.get("sortOrder").and_then(Value::as_f64).unwrap_or(0.0);
        compare_sort_order(a_order, b_order, field, project_name).ok();
        a_order.partial_cmp(&b_order).unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(sorted)
}

fn format_milestones_as_markdown(
    nodes: &[Value],
    page_info: &Value,
    project_name: &str,
) -> Result<String> {
    if nodes.is_empty() {
        return Ok(String::new());
    }

    let mut markdown = String::from("\n\n## Milestones\n\n");
    for milestone in by_sort_order(nodes, "milestone sortOrder", project_name)? {
        let mut meta = vec![
            str_at(milestone, "/status").to_string(),
            format_milestone_percent(
                milestone
                    .get("progress")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
            ),
        ];
        if let Some(target) = non_empty(milestone, "targetDate") {
            meta.push(format!("target {target}"));
        }
        markdown += &format!(
            "- **{}** _[{}]_\n",
            str_at(milestone, "/name"),
            meta.join(", ")
        );
        if let Some(description) = non_empty(milestone, "description") {
            markdown += &format!("  {}\n", description.replace('\n', "\n  "));
        }
    }
    Ok((markdown + &truncation_note(page_info)).trim_end().to_string())
}

fn format_resources_as_markdown(
    nodes: &[Value],
    page_info: &Value,
    project_name: &str,
) -> Result<String> {
    if nodes.is_empty() {
        return Ok(String::new());
    }

    let mut markdown = String::from("\n\n## Resources\n\n");
    for link in by_sort_order(nodes, "resource sortOrder", project_name)? {
        markdown += &format!(
            "- **{}**: {}\n",
            str_at(link, "/label"),
            str_at(link, "/url")
        );
    }
    Ok((markdown + &truncation_note(page_info)).trim_end().to_string())
}

fn format_documents_as_markdown(
    nodes: &[Value],
    page_info: &Value,
    project_name: &str,
) -> Result<String> {
    if nodes.is_empty() {
        return Ok(String::new());
    }

    let mut markdown = String::from("\n\n## Documents\n\n");
    for document in by_sort_order(nodes, "document sortOrder", project_name)? {
        markdown += &format!(
            "- **{}**: {}\n",
            str_at(document, "/title"),
            str_at(document, "/url")
        );
    }
    Ok((markdown + &truncation_note(page_info)).trim_end().to_string())
}

fn format_attachments_as_markdown(nodes: &[Value], page_info: &Value) -> String {
    if nodes.is_empty() {
        return String::new();
    }

    let mut markdown = String::from("\n\n## Attachments\n\n");
    for attachment in nodes {
        let source_label = match non_empty(attachment, "sourceType") {
            Some(source) => format!(" _[{source}]_"),
            None => String::new(),
        };
        markdown += &format!(
            "- **{}**: {}{source_label}\n",
            str_at(attachment, "/title"),
            str_at(attachment, "/url")
        );
        if let Some(subtitle) = non_empty(attachment, "subtitle") {
            markdown += &format!("  _{subtitle}_\n");
        }
    }
    (markdown + &truncation_note(page_info)).trim_end().to_string()
}

/// `end -> start` is "this must finish before that begins". `inverseRelations`
/// store anchors from the other project's point of view, so callers reading
/// them swap the arguments before calling.
fn describe_relation(own_anchor: &str, other_anchor: &str) -> &'static str {
    if own_anchor == "end" && other_anchor == "start" {
        "Blocks"
    } else if own_anchor == "start" && other_anchor == "end" {
        "Blocked by"
    } else {
        "Related to"
    }
}

fn milestone_note(own: Option<&Value>, other: Option<&Value>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(own) = own.filter(|value| !value.is_null()) {
        parts.push(format!("from milestone {}", str_at(own, "/name")));
    }
    if let Some(other) = other.filter(|value| !value.is_null()) {
        parts.push(format!("to milestone {}", str_at(other, "/name")));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" _({})_", parts.join(", "))
    }
}

fn format_related_projects_as_markdown(
    outgoing: &[Value],
    outgoing_page_info: &Value,
    incoming: &[Value],
    incoming_page_info: &Value,
) -> String {
    if outgoing.is_empty() && incoming.is_empty() {
        return String::new();
    }

    let mut markdown = String::from("\n\n## Related projects\n\n");

    for relation in outgoing {
        let label = describe_relation(
            str_at(relation, "/anchorType"),
            str_at(relation, "/relatedAnchorType"),
        );
        let note = milestone_note(
            relation.get("projectMilestone"),
            relation.get("relatedProjectMilestone"),
        );
        markdown += &format!(
            "- **{label}** {}: {}{note}\n",
            str_at(relation, "/relatedProject/name"),
            str_at(relation, "/relatedProject/url")
        );
    }

    for relation in incoming {
        // The stored anchors belong to the other project; swap them to describe
        // the relationship from this project's side.
        let label = describe_relation(
            str_at(relation, "/relatedAnchorType"),
            str_at(relation, "/anchorType"),
        );
        let note = milestone_note(
            relation.get("relatedProjectMilestone"),
            relation.get("projectMilestone"),
        );
        markdown += &format!(
            "- **{label}** {}: {}{note}\n",
            str_at(relation, "/project/name"),
            str_at(relation, "/project/url")
        );
    }

    (markdown + &truncation_note(outgoing_page_info) + &truncation_note(incoming_page_info))
        .trim_end()
        .to_string()
}

const ISSUE_STATE_LABELS: [(&str, &str); 6] = [
    ("triage", "Triage"),
    ("backlog", "Backlog"),
    ("unstarted", "To Do"),
    ("started", "In Progress"),
    ("completed", "Completed"),
    ("canceled", "Canceled"),
];

fn format_issues_as_markdown(nodes: &[Value]) -> String {
    if nodes.is_empty() {
        return String::new();
    }

    // Insertion-ordered counts so a state type Linear adds later still shows up.
    let mut counts: Vec<(String, i64)> = Vec::new();
    for issue in nodes {
        let state_type = str_at(issue, "/state/type").to_string();
        match counts.iter_mut().find(|(key, _)| *key == state_type) {
            Some((_, count)) => *count += 1,
            None => counts.push((state_type, 1)),
        }
    }

    let mut parts = vec![format!("{} total", nodes.len())];
    for (state_type, label) in ISSUE_STATE_LABELS {
        if let Some(index) = counts.iter().position(|(key, _)| key == state_type) {
            let count = counts[index].1;
            if count > 0 {
                parts.push(format!("{count} {}", label.to_lowercase()));
            }
            counts.remove(index);
        }
    }
    for (state_type, count) in &counts {
        parts.push(format!("{count} {state_type}"));
    }

    format!("\n\n## Issues\n\n{}", parts.join(" · "))
}

fn format_details_as_markdown(project: &Value) -> String {
    let mut rows: Vec<String> = Vec::new();
    let mut push = |label: &str, value: Option<String>| {
        if let Some(value) = value {
            if !value.is_empty() {
                rows.push(format!("- **{label}:** {value}"));
            }
        }
    };

    push("Slug", non_empty(project, "slugId"));
    push("URL", non_empty(project, "url"));
    // `icon` holds a Linear icon name such as "Rocket", never an emoji.
    push("Icon", non_empty(project, "icon"));
    push("Creator", display_name(project, "/creator"));

    let members = project.pointer("/members/nodes").and_then(Value::as_array);
    push(
        "Members",
        match members {
            Some(nodes) if !nodes.is_empty() => {
                let names: Vec<String> = nodes
                    .iter()
                    .map(|member| {
                        display_name(member, "")
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| str_at(member, "/name").to_string())
                    })
                    .collect();
                let page_info = project
                    .pointer("/members/pageInfo")
                    .cloned()
                    .unwrap_or(Value::Null);
                Some(join_connection(&names, &page_info))
            }
            _ => None,
        },
    );

    let scope = project.get("scope").and_then(Value::as_i64).unwrap_or(0);
    push(
        "Scope",
        if scope > 0 {
            Some(scope.to_string())
        } else {
            None
        },
    );
    push(
        "Start date",
        format_project_date(project, "startDate", "startDateResolution"),
    );
    push(
        "Target date",
        format_project_date(project, "targetDate", "targetDateResolution"),
    );
    if let Some(started) = non_empty(project, "startedAt") {
        push("Started", Some(display::format_relative_time(&started)));
    }
    if let Some(completed) = non_empty(project, "completedAt") {
        push("Completed", Some(display::format_relative_time(&completed)));
    }
    if let Some(canceled) = non_empty(project, "canceledAt") {
        push("Canceled", Some(display::format_relative_time(&canceled)));
    }
    if let Some(archived) = non_empty(project, "archivedAt") {
        let relative = display::format_relative_time(&archived);
        push(
            "Archived",
            Some(if non_empty(project, "autoArchivedAt").is_some() {
                format!("{relative} (automatically)")
            } else {
                relative
            }),
        );
    }
    if let Some(health_updated) = non_empty(project, "healthUpdatedAt") {
        push(
            "Health updated",
            Some(display::format_relative_time(&health_updated)),
        );
    }
    push(
        "Created",
        Some(display::format_relative_time(str_at(project, "/createdAt"))),
    );
    push(
        "Updated",
        Some(display::format_relative_time(str_at(project, "/updatedAt"))),
    );

    format!("\n\n## Details\n\n{}", rows.join("\n"))
}

/// Build the whole view as one markdown document, in display order.
fn format_project_as_markdown(project: &Value) -> Result<String> {
    let name = str_at(project, "/name");
    let title = match non_empty(project, "identifier") {
        Some(identifier) => format!("# {name} [{identifier}]"),
        None => format!("# {name}"),
    };

    let mut meta_parts = vec![
        format!("**Status:** {}", str_at(project, "/status/name")),
        format!(
            "**Priority:** {}",
            display::get_project_priority_label(
                project.get("priority").and_then(Value::as_i64).unwrap_or(0)
            )
        ),
    ];
    if let Some(health) = non_empty(project, "health") {
        meta_parts.push(format!("**Health:** {health}"));
    }
    let lead = display_name(project, "/lead");
    meta_parts.push(format!(
        "**Lead:** {}",
        match lead {
            Some(lead) => format!("@{lead}"),
            None => "Unassigned".to_string(),
        }
    ));

    if let Some(nodes) = project.pointer("/teams/nodes").and_then(Value::as_array) {
        if !nodes.is_empty() {
            let teams: Vec<String> = nodes
                .iter()
                .map(|team| {
                    format!(
                        "{} ({})",
                        str_at(team, "/name"),
                        str_at(team, "/key")
                    )
                })
                .collect();
            let page_info = project
                .pointer("/teams/pageInfo")
                .cloned()
                .unwrap_or(Value::Null);
            meta_parts.push(format!(
                "**Teams:** {}",
                join_connection(&teams, &page_info)
            ));
        }
    }
    if let Some(nodes) = project.pointer("/labels/nodes").and_then(Value::as_array) {
        if !nodes.is_empty() {
            let labels: Vec<String> = nodes
                .iter()
                .map(|label| str_at(label, "/name").to_string())
                .collect();
            let page_info = project
                .pointer("/labels/pageInfo")
                .cloned()
                .unwrap_or(Value::Null);
            meta_parts.push(format!(
                "**Labels:** {}",
                join_connection(&labels, &page_info)
            ));
        }
    }
    if let Some(nodes) = project
        .pointer("/initiatives/nodes")
        .and_then(Value::as_array)
    {
        if !nodes.is_empty() {
            let initiatives: Vec<String> = nodes
                .iter()
                .map(|initiative| str_at(initiative, "/name").to_string())
                .collect();
            let page_info = project
                .pointer("/initiatives/pageInfo")
                .cloned()
                .unwrap_or(Value::Null);
            meta_parts.push(format!(
                "**Initiatives:** {}",
                join_connection(&initiatives, &page_info)
            ));
        }
    }
    // `progress` is Linear's estimate-weighted ratio, not completed-over-total
    // issues, so it is reported on its own.
    meta_parts.push(format!(
        "**Progress:** {}",
        format_ratio_as_percent(project.get("progress").and_then(Value::as_f64).unwrap_or(0.0))
    ));

    let mut markdown = format!("{title}\n\n{}", meta_parts.join(" | "));

    if let Some(description) = non_empty(project, "description") {
        markdown += &format!("\n\n{description}");
    }
    if let Some(content) = non_empty(project, "content") {
        markdown += &format!("\n\n## Overview\n\n{content}");
    }

    let empty_page = json!({ "hasNextPage": false, "endCursor": null });
    markdown += &format_milestones_as_markdown(
        project
            .pointer("/projectMilestones/nodes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        project
            .pointer("/projectMilestones/pageInfo")
            .unwrap_or(&empty_page),
        name,
    )?;
    markdown += &format_resources_as_markdown(
        project
            .pointer("/externalLinks/nodes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        project.pointer("/externalLinks/pageInfo").unwrap_or(&empty_page),
        name,
    )?;
    markdown += &format_documents_as_markdown(
        project
            .pointer("/documents/nodes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        project.pointer("/documents/pageInfo").unwrap_or(&empty_page),
        name,
    )?;
    markdown += &format_attachments_as_markdown(
        project
            .pointer("/attachments/nodes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        project.pointer("/attachments/pageInfo").unwrap_or(&empty_page),
    );
    markdown += &format_related_projects_as_markdown(
        project
            .pointer("/relations/nodes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        project.pointer("/relations/pageInfo").unwrap_or(&empty_page),
        project
            .pointer("/inverseRelations/nodes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        project
            .pointer("/inverseRelations/pageInfo")
            .unwrap_or(&empty_page),
    );

    if let Some(update) = project.get("lastUpdate").filter(|value| !value.is_null()) {
        let author = display_name(update, "/user").unwrap_or_else(|| "Unknown".to_string());
        markdown += "\n\n## Latest Update\n\n";
        markdown += &format!("**By:** {author}\n");
        markdown += &format!(
            "**When:** {}\n",
            display::format_relative_time(str_at(update, "/createdAt"))
        );
        if let Some(health) = non_empty(update, "health") {
            markdown += &format!("**Health:** {health}\n");
        }
        markdown += &format!("\n{}", str_at(update, "/body"));
    }

    markdown += &format_issues_as_markdown(
        project
            .pointer("/issues/nodes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
    );
    markdown += &format_details_as_markdown(project);

    Ok(markdown)
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn non_empty(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}
