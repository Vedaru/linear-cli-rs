//! `linear project create` — port of
//! `src/commands/project/project-create.ts`.
//!
//! The group `mod.rs` supplies the `Failed to create project` context, so this
//! module returns bare errors. Interactive prompting is gated exactly as
//! upstream: only when no name/team flag was given (or `--interactive` was
//! passed) and stdout is a terminal.

use std::io::IsTerminal;

use clap::Args;
use serde_json::{json, Map, Value};

use crate::commands::template as tmpl;
use crate::errors::{CliError, Result};
use crate::linear;
use crate::{graphql, linear_url, output, prompt};

use super::project_description::resolve_project_description;

mod interactive;
mod templates;

const CREATE_PROJECT_MUTATION: &str = r#"
mutation CreateProject($input: ProjectCreateInput!) {
  projectCreate(input: $input) {
    success
    project {
      id
      slugId
      name
      url
    }
  }
}
"#;

const GET_PROJECT_STATUSES_QUERY: &str = {
    // The document lives in `linear/queries.rs` and is re-exported: it was defined here as well,
    // byte for byte (101 chars each), which is the kind of twin that stops being byte for byte one
    // commit later. `tests/graphql_document_names.rs` counts definitions by name and fails on a
    // second one, so this cannot come back.
    crate::linear::GET_PROJECT_STATUSES_QUERY
};

const ADD_PROJECT_TO_INITIATIVE_MUTATION: &str = r#"
mutation AddProjectToInitiativeForCreate($input: InitiativeToProjectCreateInput!) {
  initiativeToProjectCreate(input: $input) {
    success
  }
}
"#;

const GET_INITIATIVE_BY_SLUG_FOR_CREATE_QUERY: &str = r#"
query GetInitiativeBySlugForCreate($slugId: String!) {
  initiatives(filter: { slugId: { eq: $slugId } }) {
    nodes {
      id
      slugId
    }
  }
}
"#;

const GET_INITIATIVE_BY_NAME_FOR_CREATE_QUERY: &str = r#"
query GetInitiativeByNameForCreate($name: String!) {
  initiatives(filter: { name: { eqIgnoreCase: $name } }) {
    nodes {
      id
      name
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct ProjectCreateArgs {
    /// Project name (required)
    #[arg(short = 'n', long, value_name = "name")]
    pub name: Option<String>,
    /// Project description
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// Read project description from file
    #[arg(short = 'f', long = "description-file", value_name = "path")]
    pub description_file: Option<String>,
    /// Project overview markdown
    #[arg(long, value_name = "markdown")]
    pub content: Option<String>,
    /// Read project overview markdown from a file
    #[arg(long = "content-file", value_name = "path")]
    pub content_file: Option<String>,
    /// Team key, name, or ID (required, can be repeated)
    #[arg(short = 't', long = "team", value_name = "team")]
    pub team: Vec<String>,
    /// Project lead (username, email, or @me)
    #[arg(short = 'l', long, value_name = "lead")]
    pub lead: Option<String>,
    /// Project status (planned, started, paused, completed, canceled, backlog)
    #[arg(short = 's', long, value_name = "status")]
    pub status: Option<String>,
    /// Start date (YYYY-MM-DD)
    #[arg(long = "start-date", value_name = "startDate")]
    pub start_date: Option<String>,
    /// Target completion date (YYYY-MM-DD)
    #[arg(long = "target-date", value_name = "targetDate")]
    pub target_date: Option<String>,
    /// Project priority (none, urgent, high, medium, low)
    #[arg(long, value_name = "priority")]
    pub priority: Option<String>,
    /// Project label. May be repeated.
    #[arg(long = "label", value_name = "label")]
    pub label: Vec<String>,
    /// Project member. May be repeated.
    #[arg(long = "member", value_name = "user")]
    pub member: Vec<String>,
    /// Project icon
    #[arg(long, value_name = "icon")]
    pub icon: Option<String>,
    /// Project color as a HEX string
    #[arg(long, value_name = "color")]
    pub color: Option<String>,
    /// Add to initiative immediately (ID, slug, or name)
    #[arg(long, value_name = "initiative")]
    pub initiative: Option<String>,
    /// Project template to apply, by name or ID
    #[arg(long, value_name = "template")]
    pub template: Option<String>,
    /// Interactive mode (default if no flags provided)
    #[arg(short = 'i', long)]
    pub interactive: bool,
    /// Output created project as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

const PRIORITY_HELP: &str = "Valid values: none, urgent, high, medium, low";

fn parse_priority(priority: &str) -> Result<i64> {
    match priority.to_lowercase().as_str() {
        "none" => Ok(0),
        "urgent" => Ok(1),
        "high" => Ok(2),
        "medium" => Ok(3),
        "low" => Ok(4),
        _ => Err(
            CliError::validation(format!("Invalid priority: {priority}")).suggestion(PRIORITY_HELP),
        ),
    }
}

/// Resolve `--content` / `--content-file`. Shared with `project update`.
pub(crate) fn resolve_project_content(
    content: Option<&str>,
    content_file: Option<&str>,
) -> Result<Option<String>> {
    if content.is_some() && content_file.is_some() {
        return Err(CliError::validation(
            "Cannot specify both --content and --content-file",
        ));
    }

    let Some(path) = content_file else {
        return Ok(content.map(str::to_string));
    };

    std::fs::read_to_string(path).map(Some).map_err(|error| {
        CliError::validation(format!("Failed to read content file: {path}"))
            .suggestion(format!("Error: {error}"))
    })
}

/// Resolve an initiative for `--initiative`, returning `None` (rather than
/// erroring) when it cannot be found so create can warn and keep the project.
fn resolve_initiative_id_optional(
    client: &graphql::Client,
    reference: &str,
) -> Result<Option<String>> {
    let mut reference = reference.to_string();

    if let Some(linear_url::LinearUrlRef::Initiative { slug_id, .. }) =
        linear_url::expect_linear_url_kind(
            &reference,
            "initiative",
            "an initiative URL, UUID, slug ID, or exact name",
        )?
    {
        match linear::find_initiative_id_by_slug(&slug_id, false)? {
            Some(id) => reference = id,
            None => return Ok(None),
        }
    }

    if linear::is_linear_uuid(&reference) {
        return Ok(Some(reference));
    }

    if let Ok(result) = client.request(
        GET_INITIATIVE_BY_SLUG_FOR_CREATE_QUERY,
        json!({ "slugId": reference }),
    ) {
        if let Some(id) = first_id(&result, "initiatives") {
            return Ok(Some(id));
        }
    }

    if let Ok(result) = client.request(
        GET_INITIATIVE_BY_NAME_FOR_CREATE_QUERY,
        json!({ "name": reference }),
    ) {
        if let Some(id) = first_id(&result, "initiatives") {
            return Ok(Some(id));
        }
    }

    Ok(None)
}

fn first_id(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|connection| connection.get("nodes"))
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.first())
        .and_then(|node| node.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `planned` and friends map to the API's status *type* tokens. Shared with
/// `project update`.
pub(crate) fn api_status_type(status: &str) -> Option<&'static str> {
    match status.to_lowercase().as_str() {
        "planned" => Some("planned"),
        "in progress" => Some("started"),
        "started" => Some("started"),
        "paused" => Some("paused"),
        "completed" => Some("completed"),
        "canceled" => Some("canceled"),
        "backlog" => Some("backlog"),
        _ => None,
    }
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

pub fn run(args: ProjectCreateArgs) -> Result<()> {
    let content = resolve_project_content(args.content.as_deref(), args.content_file.as_deref())?;
    let priority = args.priority.as_deref().map(parse_priority).transpose()?;

    let client = graphql::client()?;

    let mut name = args.name.clone();
    let mut description = args.description.clone();
    let description_file = args.description_file.clone();
    let mut teams = args.team.clone();
    let mut lead = args.lead.clone();
    let mut status = args.status.clone();
    let mut start_date = args.start_date.clone();
    let mut target_date = args.target_date.clone();

    let no_flags_provided = name.is_none() && teams.is_empty();
    let is_interactive = (no_flags_provided || args.interactive)
        && std::io::stdout().is_terminal()
        && prompt::is_interactive();

    if is_interactive {
        interactive::interactive_prompt(
            &client,
            &mut name,
            &mut description,
            description_file.as_deref(),
            &mut teams,
            &mut lead,
            &mut status,
            &mut start_date,
            &mut target_date,
        )?;
    }

    let resolved_description =
        resolve_project_description(description.as_deref(), description_file.as_deref())?;

    let Some(name) = name else {
        return Err(CliError::validation("Project name is required")
            .suggestion("Use --name or -n flag to specify a project name."));
    };

    if teams.is_empty() {
        match linear::get_team_key()? {
            Some(default_team) => teams = vec![default_team],
            None => {
                return Err(CliError::validation("At least one team is required")
                    .suggestion("Use --team or -t flag to specify a team."));
            }
        }
    }

    let team_ids: Vec<String> = linear::resolve_teams(&teams)?
        .into_iter()
        .map(|team| team.id)
        .collect();

    let template_id = match args.template.as_deref() {
        Some(reference) => {
            let template = templates::resolve_template_scoped(reference, "project", &team_ids)?;
            Some(tmpl::template_id(&template))
        }
        None => None,
    };

    let mut lead_id: Option<String> = None;
    if let Some(lead) = &lead {
        match linear::lookup_user_id(lead)? {
            Some(id) => lead_id = Some(id),
            None => return Err(CliError::not_found("Lead", lead)),
        }
    }

    let mut status_id: Option<String> = None;
    if let Some(status) = &status {
        let Some(api_type) = api_status_type(status) else {
            return Err(
                CliError::validation(format!("Invalid status: {status}")).suggestion(
                    "Valid values: planned, started, paused, completed, canceled, backlog",
                ),
            );
        };
        let data = client.request(GET_PROJECT_STATUSES_QUERY, json!({}))?;
        let matching = project_statuses(&data)
            .into_iter()
            .find(|node| node.get("type").and_then(Value::as_str) == Some(api_type))
            .and_then(|node| node.get("id").and_then(Value::as_str).map(str::to_string));
        match matching {
            Some(id) => status_id = Some(id),
            None => return Err(CliError::not_found("Project status", api_type)),
        }
    }

    let mut label_ids: Vec<String> = Vec::new();
    for label in &args.label {
        match linear::get_project_label_id_by_name(label)? {
            Some(id) => label_ids.push(id),
            None => return Err(CliError::not_found("Project label", label)),
        }
    }

    let mut member_ids: Vec<String> = Vec::new();
    for member in &args.member {
        match linear::lookup_user_id(member)? {
            Some(id) => member_ids.push(id),
            None => return Err(CliError::not_found("User", member)),
        }
    }

    if let Some(start_date) = &start_date {
        if !is_iso_date(start_date) {
            return Err(CliError::validation(
                "Start date must be in YYYY-MM-DD format",
            ));
        }
    }
    if let Some(target_date) = &target_date {
        if !is_iso_date(target_date) {
            return Err(CliError::validation(
                "Target date must be in YYYY-MM-DD format",
            ));
        }
    }

    let mut input = Map::new();
    input.insert("name".to_string(), json!(name));
    input.insert("teamIds".to_string(), json!(team_ids));
    if let Some(description) = &resolved_description {
        input.insert("description".to_string(), json!(description));
    }
    if let Some(content) = &content {
        input.insert("content".to_string(), json!(content));
    }
    if let Some(lead_id) = &lead_id {
        input.insert("leadId".to_string(), json!(lead_id));
    }
    if let Some(status_id) = &status_id {
        input.insert("statusId".to_string(), json!(status_id));
    }
    if let Some(start_date) = &start_date {
        input.insert("startDate".to_string(), json!(start_date));
    }
    if let Some(target_date) = &target_date {
        input.insert("targetDate".to_string(), json!(target_date));
    }
    if let Some(priority) = priority {
        input.insert("priority".to_string(), json!(priority));
    }
    if !label_ids.is_empty() {
        input.insert("labelIds".to_string(), json!(label_ids));
    }
    if !member_ids.is_empty() {
        input.insert("memberIds".to_string(), json!(member_ids));
    }
    if let Some(icon) = &args.icon {
        input.insert("icon".to_string(), json!(icon));
    }
    if let Some(color) = &args.color {
        input.insert("color".to_string(), json!(color));
    }
    if let Some(template_id) = &template_id {
        input.insert("templateId".to_string(), json!(template_id));
    }

    let result = client.request(
        CREATE_PROJECT_MUTATION,
        json!({ "input": Value::Object(input) }),
    )?;
    let project_create = result
        .get("projectCreate")
        .cloned()
        .ok_or_else(|| CliError::cli("Failed to create project: no projectCreate returned"))?;

    if project_create.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli("Failed to create project"));
    }

    let Some(project) = project_create
        .get("project")
        .filter(|value| !value.is_null())
    else {
        return Err(CliError::cli(
            "Failed to create project: no project returned",
        ));
    };

    if let Some(initiative) = &args.initiative {
        add_to_initiative(&client, initiative, project, args.json)?;
    }

    if args.json {
        output::print_json(&project_create);
    } else {
        output::line(&format!(
            "✓ Created project: {}",
            project.get("name").and_then(Value::as_str).unwrap_or("")
        ));
        output::line(&format!(
            "  Slug: {}",
            project.get("slugId").and_then(Value::as_str).unwrap_or("")
        ));
        if let Some(url) = project.get("url").and_then(Value::as_str) {
            if !url.is_empty() {
                output::line(&format!("  URL: {url}"));
            }
        }
    }

    Ok(())
}

fn add_to_initiative(
    client: &graphql::Client,
    initiative: &str,
    project: &Value,
    json_output: bool,
) -> Result<()> {
    let initiative_id = resolve_initiative_id_optional(client, initiative)?;
    let Some(initiative_id) = initiative_id else {
        eprintln!("\nWarning: Initiative not found: {initiative}");
        eprintln!("Project was created but not added to initiative.");
        return Ok(());
    };

    let project_id = project.get("id").and_then(Value::as_str).unwrap_or("");
    let result = client.request(
        ADD_PROJECT_TO_INITIATIVE_MUTATION,
        json!({ "input": { "initiativeId": initiative_id, "projectId": project_id } }),
    );

    match result {
        Ok(data) => {
            let success = data
                .pointer("/initiativeToProjectCreate/success")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if success {
                if !json_output {
                    output::line(&format!("✓ Added to initiative: {initiative}"));
                }
            } else {
                eprintln!("\nWarning: Failed to add project to initiative");
            }
        }
        Err(error) => {
            eprintln!("\nWarning: Failed to add project to initiative: {error}");
        }
    }

    Ok(())
}

fn project_statuses(data: &Value) -> Vec<Value> {
    data.pointer("/projectStatuses/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

