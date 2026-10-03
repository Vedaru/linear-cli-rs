//! `linear issue create` — port of `src/commands/issue/issue-create.ts`.
//!
//! Two modes: an interactive wizard (used when only seed flags such as
//! `--parent`/`--project` are given) and a flag-driven path. The interactive
//! wizard is gated on [`crate::prompt::is_interactive`]; headless runs take the
//! flag path and fail validation naming the flag they should pass instead.
//!
//! Error context mirrors upstream: the description/file pre-validation and the
//! title-required check escape without context, while everything inside the two
//! execution blocks is wrapped with `"Failed to create issue"`.

use std::io::{BufRead, Write};

use clap::Args;
use serde_json::{json, Map, Value};

use crate::commands::template as tmpl;
use crate::errors::{CliError, Result};
use crate::linear::{self, WorkflowState};
use crate::{config, editor, graphql, output, prompt};

const CREATE_ISSUE_MUTATION: &str = r#"
mutation CreateIssue($input: IssueCreateInput!) {
  issueCreate(input: $input) {
    success
    issue { id, identifier, url, team { key } }
  }
}
"#;

const GET_USER_SETTINGS_QUERY: &str = r#"
query GetUserSettings {
  userSettings {
    autoAssignToSelf
  }
}
"#;

#[derive(Args, Debug)]
pub struct IssueCreateArgs {
    /// Start the issue after creation
    #[arg(long)]
    pub start: bool,
    /// Assign the issue to 'self' or someone (by username or name)
    #[arg(short = 'a', long, value_name = "assignee")]
    pub assignee: Option<String>,
    /// Due date of the issue
    #[arg(long = "due-date", value_name = "dueDate")]
    pub due_date: Option<String>,
    /// Parent issue (if any) as a team_number code
    #[arg(long, value_name = "parent")]
    pub parent: Option<String>,
    /// Priority of the issue (1-4, descending priority)
    #[arg(short = 'p', long, value_name = "priority")]
    pub priority: Option<i64>,
    /// Points estimate of the issue
    #[arg(long, value_name = "estimate")]
    pub estimate: Option<i64>,
    /// Description of the issue
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// Read description from a file (preferred for markdown content)
    #[arg(long = "description-file", value_name = "path")]
    pub description_file: Option<String>,
    /// Issue label associated with the issue. May be repeated.
    #[arg(short = 'l', long = "label", value_name = "label", action = clap::ArgAction::Append)]
    pub label: Vec<String>,
    /// Team (key, name, or ID) for the issue, if not your default team
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Project for the issue (UUID, slug ID, or name)
    #[arg(long, value_name = "project")]
    pub project: Option<String>,
    /// Workflow state for the issue (by name or type)
    #[arg(short = 's', long, value_name = "state")]
    pub state: Option<String>,
    /// Project milestone (UUID, or name when --project is set)
    #[arg(long, value_name = "milestone")]
    pub milestone: Option<String>,
    /// Cycle name, number, 'active'/'now', 'next', 'previous', or a relative offset like +1 (use --cycle=-1 for negatives)
    #[arg(long, value_name = "cycle")]
    pub cycle: Option<String>,
    /// Do not use default template for the issue
    #[arg(
        long = "no-use-default-template",
        action = clap::ArgAction::SetFalse,
        default_value_t = true
    )]
    pub use_default_template: bool,
    /// Issue template to apply, by name or ID (the team's templates plus workspace ones). Takes the place of the team's default template. The template fills in anything you do not pass: explicit flags override it, --label merges with the template's labels, and --description replaces the template body (omit it to keep the body). Makes --title optional.
    #[arg(long, value_name = "template")]
    pub template: Option<String>,
    /// Disable interactive prompts
    #[arg(long = "no-interactive")]
    pub no_interactive: bool,
    /// Title of the issue
    #[arg(short = 't', long, value_name = "title")]
    pub title: Option<String>,
    /// Output the created issue as JSON, as the API returned it (an addition to upstream)
    #[arg(short = 'j', long)]
    pub json: bool,
}

fn falsy(option: &Option<String>) -> bool {
    option.as_deref().map_or(true, str::is_empty)
}

/// A string field of a local template, ignoring an empty one.
fn text_field(fields: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn run(mut args: IssueCreateArgs) -> Result<()> {
    // Validate that description and descriptionFile are not both provided.
    if args.description.is_some() && args.description_file.is_some() {
        return Err(CliError::validation(
            "Cannot specify both --description and --description-file",
        ));
    }

    // A local template is a file of flags, applied here on this side of the API: the rest of this
    // function then sees an ordinary flag invocation and nothing downstream has to know. The file
    // wins over a workspace template of the same name, and the one it shadows is *named* rather
    // than silently losing - "which of my two `bug` templates just ran" is the question a
    // shadowed name creates.
    if let Some(name) = args.template.clone() {
        if let Some(local_template) = tmpl::local::find(&name)? {
            let fields = &local_template.fields;
            let mut filled: Vec<&str> = Vec::new();
            if falsy(&args.title) {
                if let Some(value) = text_field(fields, "title") {
                    args.title = Some(value);
                    filled.push("title");
                }
            }
            if falsy(&args.description) && args.description_file.is_none() {
                if let Some(value) = text_field(fields, "description") {
                    args.description = Some(value);
                    filled.push("description");
                }
            }
            for (key, target) in [
                ("assignee", &mut args.assignee),
                ("team", &mut args.team),
                ("project", &mut args.project),
                ("state", &mut args.state),
                ("cycle", &mut args.cycle),
                ("milestone", &mut args.milestone),
                ("parent", &mut args.parent),
                ("due_date", &mut args.due_date),
            ] {
                if target.is_none() {
                    if let Some(value) = text_field(fields, key) {
                        *target = Some(value);
                        filled.push(key);
                    }
                }
            }
            if args.priority.is_none() {
                if let Some(value) = fields.get("priority").and_then(Value::as_i64) {
                    args.priority = Some(value);
                    filled.push("priority");
                }
            }
            if args.estimate.is_none() {
                if let Some(value) = fields.get("estimate").and_then(Value::as_i64) {
                    args.estimate = Some(value);
                    filled.push("estimate");
                }
            }
            if args.label.is_empty() {
                if let Some(labels) = fields.get("labels").and_then(Value::as_array) {
                    args.label = labels
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect();
                    if !args.label.is_empty() {
                        filled.push("labels");
                    }
                }
            }

            if !filled.is_empty() && !args.json {
                output::line(&format!(
                    "Using local template {name}: {}",
                    filled.join(", ")
                ));
            }
            if let Some(workspace) = tmpl::find_workspace_by_name(&name)? {
                if !args.json {
                    output::warn(&format!(
                        "a workspace template named \"{}\" also exists and is shadowed by the local file",
                        tmpl::template_name(&workspace)
                    ));
                }
            }
            // A local template takes the place of the team's default template for the same reason
            // an explicit workspace `--template` does: the caller named what should fill this
            // issue, and letting the server's default also apply would fill it a second time.
            args.use_default_template = false;
            // Done with it: from here the invocation is flags, and a local template is not a
            // server-side template id to send.
            args.template = None;
        }
    }

    // Read description from file if provided.
    let mut final_description = args.description.clone();
    if let Some(path) = &args.description_file {
        final_description = Some(std::fs::read_to_string(path).map_err(|error| {
            CliError::validation(format!("Failed to read description file: {path}"))
                .suggestion(format!("Error: {error}"))
        })?);
    }

    let interactive = !args.no_interactive && prompt::is_interactive();

    // If no creation flags are provided beyond project/parent, use interactive mode.
    let only_interactive_seed_flags_provided = falsy(&args.title)
        && falsy(&args.assignee)
        && falsy(&args.due_date)
        && args.priority.is_none()
        && args.estimate.is_none()
        && final_description.as_deref().map_or(true, str::is_empty)
        && args.label.is_empty()
        && falsy(&args.team)
        && falsy(&args.state)
        && falsy(&args.milestone)
        && falsy(&args.cycle)
        && !args.start
        && args.template.is_none();

    if only_interactive_seed_flags_provided && interactive {
        return run_interactive(&args, interactive)
            .map_err(|error| error.with_context("Failed to create issue"));
    }

    // Fallback to flag-based mode. A template can supply the title.
    if falsy(&args.title) && args.template.is_none() {
        return Err(CliError::validation(
            "Title is required when not using interactive mode",
        )
        .suggestion(
            "Use --title, pass --template to take the title from a template, or run without any flags (or only --parent/--project) for interactive mode.",
        ));
    }

    run_flags(&args, final_description, interactive)
        .map_err(|error| error.with_context("Failed to create issue"))
}

/// The interactive wizard. Mirrors `promptInteractiveIssueCreation` plus the
/// `resolveParentIssueForCreate`/`resolveProjectIdForCreate` calls that precede
/// it, and the `issueCreate` mutation that follows.
fn run_interactive(args: &IssueCreateArgs, interactive: bool) -> Result<()> {
    let (parent_id, parent_data) = resolve_parent_issue_for_create(args.parent.as_deref())?;
    let explicit_project_id = match &args.project {
        None => None,
        Some(project) => Some(resolve_project_id_for_create(project, interactive)?),
    };

    let data =
        prompt_interactive_issue_creation(explicit_project_id, parent_id, parent_data.as_ref())?;

    output::line("Creating issue...");
    output::blank();

    let mut input = Map::new();
    input.insert("title".to_string(), json!(data.title));
    if let Some(assignee_id) = &data.assignee_id {
        input.insert("assigneeId".to_string(), json!(assignee_id));
    }
    // dueDate: undefined in interactive mode, omitted.
    if let Some(parent_id) = &data.parent_id {
        input.insert("parentId".to_string(), json!(parent_id));
    }
    if let Some(priority) = data.priority {
        input.insert("priority".to_string(), json!(priority));
    }
    if let Some(estimate) = data.estimate {
        input.insert("estimate".to_string(), json!(estimate));
    }
    input.insert("labelIds".to_string(), json!(data.label_ids));
    input.insert("teamId".to_string(), json!(data.team_id));
    input.insert("projectId".to_string(), data.project_id.clone());
    if let Some(state_id) = &data.state_id {
        input.insert("stateId".to_string(), json!(state_id));
    }
    input.insert(
        "useDefaultTemplate".to_string(),
        json!(args.use_default_template),
    );
    if let Some(description) = &data.description {
        input.insert("description".to_string(), json!(description));
    }

    let client = graphql::client()?;
    let result = client.request(
        CREATE_ISSUE_MUTATION,
        json!({ "input": Value::Object(input) }),
    )?;
    let issue = created_issue(&result)?;
    let issue_id = issue.get("id").and_then(Value::as_str).unwrap_or("");
    let identifier = issue
        .get("identifier")
        .and_then(Value::as_str)
        .unwrap_or("");
    let url = issue.get("url").and_then(Value::as_str).unwrap_or("");

    if data.start {
        let team_key = issue
            .get("team")
            .and_then(|team| team.get("key"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // The state update still happens with `--json`; only its line is
        // suppressed, so the document stays the only thing on stdout.
        super::issue_start::start_work_on_issue(issue_id, &team_key, None, None, true, args.json)?;
    }

    if args.json {
        // The API's own payload, verbatim: `issueCreate` carries the issue, so a
        // caller gets the id, the identifier and the url without a second query.
        output::print_json(&result);
        return Ok(());
    }

    output::line(&format!("✓ Created issue {identifier}: {}", data.title));
    output::line(url);

    Ok(())
}

/// `issueCreate` result validation shared by both paths.
fn created_issue(result: &Value) -> Result<Value> {
    let issue_create = result.get("issueCreate").cloned().unwrap_or(Value::Null);
    if !issue_create
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CliError::cli("Issue creation failed"));
    }
    let issue = issue_create.get("issue").cloned().unwrap_or(Value::Null);
    if issue.is_null() {
        return Err(CliError::cli("Issue creation failed - no issue returned"));
    }
    Ok(issue)
}

/// Flag-driven creation. Mirrors the tail of the upstream action.
fn run_flags(
    args: &IssueCreateArgs,
    final_description: Option<String>,
    interactive: bool,
) -> Result<()> {
    // An explicit --team (key, name, or UUID) must resolve or error. Only the
    // configured default, which the user did not type, may fall back to the
    // interactive substring picker.
    let team_id: String;
    let team_key: String;
    if let Some(team) = &args.team {
        let resolved = linear::resolve_team(team)?;
        team_id = resolved.id;
        team_key = resolved.key;
    } else {
        let default_team = linear::get_team_key()?
            .ok_or_else(|| CliError::validation("Could not determine team key"))?;
        team_key = default_team.clone();
        match linear::find_team(&default_team)? {
            Some(found) => team_id = found.id,
            None => {
                let mut picked: Option<String> = None;
                if interactive {
                    let options = linear::search_teams_by_key_substring(&default_team)?;
                    picked = linear::select_option("Team", &default_team, &options)?;
                }
                match picked {
                    Some(picked) => team_id = picked,
                    None => return Err(CliError::not_found("Team", &default_team)),
                }
            }
        }
    }

    // Linear rejects useDefaultTemplate next to templateId, so an explicit
    // template also drops the default-template flag.
    let template_id = match &args.template {
        None => None,
        Some(template) => {
            let resolved =
                resolve_template_scoped(template, "issue", std::slice::from_ref(&team_id))?;
            Some(tmpl::template_id(&resolved))
        }
    };

    let mut assignee = args.assignee.clone();
    if args.start && assignee.is_none() {
        assignee = Some("self".to_string());
    }
    if args.start {
        if let Some(non_self) = assignee.as_deref().filter(|value| *value != "self") {
            let _ = non_self;
            return Err(CliError::validation(
                "Cannot use --start and a non-self --assignee",
            ));
        }
    }

    let state_id = match &args.state {
        None => None,
        Some(state) => {
            let states = linear::get_workflow_states(&team_key)?;
            match linear::resolve_workflow_state(&states, state)? {
                Some(workflow_state) => Some(workflow_state.id),
                None => {
                    return Err(linear::workflow_state_not_found_error(
                        &team_key, state, &states,
                    ))
                }
            }
        }
    };

    let mut assignee_id: Option<String> = None;
    if should_assign_self_by_default_for_flag_create() {
        assignee_id = linear::lookup_user_id("self")?;
    }
    if let Some(assignee) = assignee.as_deref().filter(|value| !value.is_empty()) {
        assignee_id = linear::lookup_user_id(assignee)?;
        if assignee_id.is_none() {
            return Err(CliError::not_found("User", assignee));
        }
    }

    let mut label_ids: Vec<String> = Vec::new();
    for label in &args.label {
        let mut label_id = linear::get_issue_label_id_by_name_for_team(label, &team_key)?;
        if label_id.is_none() && interactive {
            let options = linear::get_issue_label_options_by_name_for_team(label, &team_key)?;
            label_id = linear::select_option("Issue label", label, &options)?;
        }
        match label_id {
            Some(label_id) => label_ids.push(label_id),
            None => return Err(CliError::not_found("Issue label", label)),
        }
    }

    let mut project_id: Option<String> = None;
    if let Some(project) = &args.project {
        project_id = Some(resolve_project_id_for_create(project, interactive)?);
    }

    let mut project_milestone_id: Option<String> = None;
    if let Some(milestone) = &args.milestone {
        if linear::is_linear_uuid(milestone) {
            project_milestone_id = Some(milestone.clone());
        } else {
            if project_id.is_none() {
                return Err(CliError::validation(
                    "--milestone requires --project to be set",
                )
                .suggestion(
                    "Use --project to specify which project the milestone belongs to, or pass a milestone UUID directly.",
                ));
            }
            project_milestone_id = Some(linear::resolve_milestone_id(
                milestone,
                project_id.as_deref(),
            )?);
        }
    }

    let cycle_id = match &args.cycle {
        None => None,
        Some(cycle) => Some(linear::get_cycle_id_by_name_or_number(&team_id, cycle)?),
    };

    let (parent_id, parent_data) = resolve_parent_issue_for_create(args.parent.as_deref())?;

    // `projectId || parentData?.projectId`: a falsy projectId falls back to the
    // parent's project (which may itself be null), otherwise omitted.
    let project_id_value: Option<Value> = match project_id {
        Some(project_id) if !project_id.is_empty() => Some(json!(project_id)),
        _ => parent_data
            .as_ref()
            .map(|data| data.get("projectId").cloned().unwrap_or(Value::Null)),
    };

    let mut input = Map::new();
    if let Some(title) = args.title.as_deref().filter(|value| !value.is_empty()) {
        input.insert("title".to_string(), json!(title));
    }
    if let Some(assignee_id) = &assignee_id {
        input.insert("assigneeId".to_string(), json!(assignee_id));
    }
    if let Some(due_date) = &args.due_date {
        input.insert("dueDate".to_string(), json!(due_date));
    }
    if let Some(parent_id) = &parent_id {
        input.insert("parentId".to_string(), json!(parent_id));
    }
    if let Some(priority) = args.priority {
        input.insert("priority".to_string(), json!(priority));
    }
    if let Some(estimate) = args.estimate {
        input.insert("estimate".to_string(), json!(estimate));
    }
    input.insert("labelIds".to_string(), json!(label_ids));
    input.insert("teamId".to_string(), json!(team_id));
    if let Some(project_id_value) = project_id_value {
        input.insert("projectId".to_string(), project_id_value);
    }
    if let Some(project_milestone_id) = &project_milestone_id {
        input.insert(
            "projectMilestoneId".to_string(),
            json!(project_milestone_id),
        );
    }
    if let Some(cycle_id) = &cycle_id {
        input.insert("cycleId".to_string(), json!(cycle_id));
    }
    if let Some(state_id) = &state_id {
        input.insert("stateId".to_string(), json!(state_id));
    }
    if let Some(template_id) = &template_id {
        input.insert("templateId".to_string(), json!(template_id));
    }
    if args.template.is_none() {
        input.insert(
            "useDefaultTemplate".to_string(),
            json!(args.use_default_template),
        );
    }
    if let Some(description) = &final_description {
        input.insert("description".to_string(), json!(description));
    }

    if !args.json {
        output::line(&format!("Creating issue in {team_key}"));
        output::blank();
    }

    let client = graphql::client()?;
    let result = client.request(
        CREATE_ISSUE_MUTATION,
        json!({ "input": Value::Object(input) }),
    )?;
    let issue = created_issue(&result)?;
    let issue_id = issue.get("id").and_then(Value::as_str).unwrap_or("");
    let url = issue.get("url").and_then(Value::as_str).unwrap_or("");

    if args.start {
        let start_team_key = issue
            .get("team")
            .and_then(|team| team.get("key"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        super::issue_start::start_work_on_issue(
            issue_id,
            &start_team_key,
            None,
            None,
            true,
            args.json,
        )?;
    }

    if args.json {
        output::print_json(&result);
        return Ok(());
    }

    output::line(url);

    Ok(())
}

// ---------------------------------------------------------------------------
// Creation helpers (ported from issue-create.ts)
// ---------------------------------------------------------------------------

fn resolve_project_id_for_create(project: &str, interactive: bool) -> Result<String> {
    let mut project_id = linear::get_project_id_by_name(project)?;
    if project_id.is_none() && interactive {
        let options = linear::get_project_options_by_name(project)?;
        project_id = linear::select_option("Project", project, &options)?;
    }
    match project_id {
        Some(project_id) => Ok(project_id),
        None => Err(CliError::not_found("Project", project)),
    }
}

/// Returns `(parent_id, parent_data)`.
fn resolve_parent_issue_for_create(
    parent_identifier: Option<&str>,
) -> Result<(Option<String>, Option<Value>)> {
    let Some(parent_identifier) = parent_identifier.filter(|value| !value.is_empty()) else {
        return Ok((None, None));
    };

    let Some(resolved) = linear::get_issue_identifier(Some(parent_identifier))? else {
        return Err(CliError::validation(format!(
            "Could not resolve parent issue identifier: {parent_identifier}"
        )));
    };

    let Some(parent_id) = linear::get_issue_id(&resolved)? else {
        return Err(CliError::not_found("Parent issue", &resolved));
    };

    let parent_data = linear::fetch_parent_issue_data(&parent_id);
    Ok((Some(parent_id), parent_data))
}

/// The result of the interactive wizard, mirroring upstream's return object.
struct InteractiveData {
    title: String,
    team_id: String,
    assignee_id: Option<String>,
    priority: Option<i64>,
    estimate: Option<i64>,
    label_ids: Vec<String>,
    description: Option<String>,
    state_id: Option<String>,
    start: bool,
    parent_id: Option<String>,
    project_id: Value,
}

/// Mirrors `promptProjectSelection`. `team_id` is the Rust helper's ID key.
fn prompt_project_selection(team_id: &str, preloaded: Option<&[Value]>) -> Result<Option<String>> {
    let projects: Vec<Value> = match preloaded {
        Some(projects) => projects.to_vec(),
        None => linear::get_projects_for_team(team_id)?,
    };
    if projects.is_empty() {
        return Ok(None);
    }

    const NO_PROJECT: &str = "__none__";
    let mut options: Vec<(String, String)> =
        vec![(NO_PROJECT.to_string(), "No project".to_string())];
    for project in &projects {
        let id = project.get("id").and_then(Value::as_str).unwrap_or("");
        let name = project.get("name").and_then(Value::as_str).unwrap_or("");
        options.push((id.to_string(), name.to_string()));
    }

    let selected = prompt_select("Which project should this issue belong to?", &options, 0)?;
    if selected == NO_PROJECT {
        Ok(None)
    } else {
        Ok(Some(selected))
    }
}

struct AdditionalFieldsResult {
    assignee_id: Option<String>,
    priority: Option<i64>,
    estimate: Option<i64>,
    label_ids: Vec<String>,
    state_id: Option<String>,
    project_id: Option<String>,
}

/// Mirrors `promptAdditionalFields` and the `ADDITIONAL_FIELDS` handlers.
fn prompt_additional_fields(
    team_key: &str,
    team_id: &str,
    states: &[WorkflowState],
    labels: &[Value],
    include_project: bool,
    auto_assign_to_self: bool,
) -> Result<AdditionalFieldsResult> {
    let default_state_name: Option<String> = if states.is_empty() {
        None
    } else {
        let default_state =
            linear::lowest_position_state_of_type(states, "unstarted").unwrap_or(&states[0]);
        Some(default_state.name.clone())
    };

    const FIELDS: [(&str, &str); 6] = [
        ("workflow_state", "Workflow state"),
        ("assignee", "Assignee"),
        ("priority", "Priority"),
        ("labels", "Labels"),
        ("estimate", "Estimate"),
        ("project", "Project"),
    ];

    let mut options: Vec<(String, String)> = Vec::new();
    for (key, label) in FIELDS {
        if !include_project && key == "project" {
            continue;
        }
        let name = if key == "workflow_state" {
            match &default_state_name {
                Some(default_state_name) => format!("{label} ({default_state_name})"),
                None => label.to_string(),
            }
        } else if key == "assignee" {
            let who = if auto_assign_to_self {
                "self"
            } else {
                "unassigned"
            };
            format!("{label} ({who})")
        } else {
            label.to_string()
        };
        options.push((key.to_string(), name));
    }

    let selected_fields = prompt_checkbox("Select additional fields to configure", &options)?;

    let mut result = AdditionalFieldsResult {
        assignee_id: None,
        priority: None,
        estimate: None,
        label_ids: Vec::new(),
        state_id: None,
        project_id: None,
    };

    if auto_assign_to_self {
        result.assignee_id = linear::lookup_user_id("self")?;
    }

    for field_key in &selected_fields {
        match field_key.as_str() {
            "workflow_state" => {
                if states.is_empty() {
                    continue;
                }
                let default_state = linear::lowest_position_state_of_type(states, "unstarted")
                    .unwrap_or(&states[0]);
                let options: Vec<(String, String)> = states
                    .iter()
                    .map(|state| {
                        (
                            state.id.clone(),
                            format!("{} ({})", state.name, state.state_type),
                        )
                    })
                    .collect();
                let default_index = states
                    .iter()
                    .position(|state| state.id == default_state.id)
                    .unwrap_or(0);
                result.state_id = Some(prompt_select(
                    "Which workflow state should this issue be in?",
                    &options,
                    default_index,
                )?);
            }
            "assignee" => {
                let answer = prompt_select_bool("Assign this issue to yourself?", false)?;
                result.assignee_id = if answer {
                    linear::lookup_user_id("self")?
                } else {
                    None
                };
            }
            "priority" => {
                let options: Vec<(String, String)> = (0..=4)
                    .map(|priority| {
                        let label = match priority {
                            0 => "No priority",
                            1 => "Urgent",
                            2 => "High",
                            3 => "Medium",
                            _ => "Low",
                        };
                        (
                            priority.to_string(),
                            format!("{} {label}", crate::display::get_priority_display(priority)),
                        )
                    })
                    .collect();
                let selected = prompt_select("What priority should this issue have?", &options, 0)?;
                let selected = selected.parse::<i64>().unwrap_or(0);
                result.priority = if selected == 0 { None } else { Some(selected) };
            }
            "labels" => {
                if labels.is_empty() {
                    result.label_ids = Vec::new();
                    continue;
                }
                let options: Vec<(String, String)> = labels
                    .iter()
                    .map(|label| {
                        (
                            label
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            label
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        )
                    })
                    .collect();
                result.label_ids = prompt_checkbox(
                    "Select labels (use space to select, enter to confirm)",
                    &options,
                )?;
            }
            "estimate" => {
                let estimate = prompt_text("Estimate (leave blank for none)", "")?;
                result.estimate = estimate.trim().parse::<i64>().ok();
            }
            "project" => {
                let projects = if include_project {
                    Some(linear::get_projects_for_team(team_id)?)
                } else {
                    None
                };
                result.project_id = prompt_project_selection(team_id, projects.as_deref())?;
            }
            _ => {}
        }
    }

    let _ = team_key;
    Ok(result)
}

/// Mirrors `promptInteractiveIssueCreation`.
fn prompt_interactive_issue_creation(
    initial_project_id: Option<String>,
    parent_id: Option<String>,
    parent_data: Option<&Value>,
) -> Result<InteractiveData> {
    let auto_assign_to_self = should_assign_self_by_default_for_interactive_create()?;

    // Resolve the default team from the configured key, if any.
    let mut resolved_team: Option<(String, String)> = None;
    if let Some(default_team_key) = linear::get_team_key()? {
        if let Some(team) = linear::find_team(&default_team_key)? {
            resolved_team = Some((team.id, team.key));
        }
    }

    if let Some(parent_data) = parent_data {
        let identifier = parent_data
            .get("identifier")
            .and_then(Value::as_str)
            .unwrap_or("");
        let title = parent_data
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("");
        output::line(&format!("Creating sub-issue for: {identifier}: {title}"));
        output::blank();
    }

    let title = prompt_text_required("What's the title of your issue?")?;

    let ask_project = should_ask_project_during_interactive_create();
    let (team_id, team_key) = match resolved_team {
        Some(team) => team,
        None => {
            let teams = linear::get_all_teams()?;
            let options: Vec<(String, String)> = teams
                .iter()
                .map(|team| (team.id.clone(), format!("{} ({})", team.name, team.key)))
                .collect();
            let selected_team_id =
                prompt_select("Which team should this issue belong to?", &options, 0)?;
            let team = teams
                .iter()
                .find(|team| team.id == selected_team_id)
                .ok_or_else(|| CliError::not_found("Team", &selected_team_id))?;
            (team.id.clone(), team.key.clone())
        }
    };

    // Preload team-scoped data.
    let states = linear::get_workflow_states(&team_key)?;
    let labels = linear::get_labels_for_team(&team_id)?;
    let projects: Option<Vec<Value>> =
        if ask_project && parent_data.is_none() && initial_project_id.is_none() {
            Some(linear::get_projects_for_team(&team_id)?)
        } else {
            None
        };

    // Description prompt.
    let editor_name = editor::get_editor();
    let editor_display_name = editor_name
        .as_deref()
        .and_then(|name| name.rsplit('/').next())
        .map(str::to_string);
    let prompt_message = match &editor_display_name {
        Some(editor_display_name) => format!("Description [(e) to launch {editor_display_name}]"),
        None => "Description".to_string(),
    };
    let description = prompt_text(&prompt_message, "")?;

    let mut final_description: Option<String> = None;
    if description == "e" && editor_display_name.is_some() {
        let editor_display_name = editor_display_name.as_deref().unwrap_or("");
        output::line(&format!("Opening {editor_display_name}..."));
        match editor::open_editor() {
            Some(value) if !value.is_empty() => {
                output::line(&format!(
                    "Description entered ({} characters)",
                    value.chars().count()
                ));
                final_description = Some(value);
            }
            _ => {
                output::line("No description entered");
                final_description = None;
            }
        }
    } else if description == "e" {
        eprintln!(
            "No editor found. Please set EDITOR environment variable or configure git editor with: git config --global core.editor <editor>"
        );
        final_description = None;
    } else if !description.trim().is_empty() {
        final_description = Some(description.trim().to_string());
    }

    let mut project_id = initial_project_id;
    if parent_data.is_none() && project_id.is_none() && ask_project {
        project_id = prompt_project_selection(&team_id, projects.as_deref())?;
    }

    let default_state: Option<WorkflowState> = if states.is_empty() {
        None
    } else {
        Some(
            linear::lowest_position_state_of_type(&states, "unstarted")
                .cloned()
                .unwrap_or_else(|| states[0].clone()),
        )
    };

    let next_action = prompt_select(
        "What's next?",
        &[
            ("submit".to_string(), "Submit issue".to_string()),
            ("more_fields".to_string(), "Add more fields".to_string()),
        ],
        0,
    )?;

    let mut assignee_id: Option<String> = None;
    if auto_assign_to_self {
        assignee_id = linear::lookup_user_id("self")?;
    }

    let mut state_id: Option<String> = default_state.as_ref().map(|state| state.id.clone());

    if next_action == "more_fields" {
        let additional = prompt_additional_fields(
            &team_key,
            &team_id,
            &states,
            &labels,
            !ask_project && parent_data.is_none() && project_id.is_none(),
            auto_assign_to_self,
        )?;
        assignee_id = additional.assignee_id;
        // `priority`/`estimate`/`stateId` are replaced outright by the
        // additional-field result, matching upstream.
        let priority = additional.priority;
        let estimate = additional.estimate;
        state_id = additional.state_id;
        project_id = additional.project_id.or(project_id);

        let start = prompt_select_bool(
            "Start working on this issue now? (creates branch and updates status)",
            false,
        )?;

        let project_value = project_value(project_id, parent_data);
        return Ok(InteractiveData {
            title,
            team_id,
            assignee_id,
            priority,
            estimate,
            label_ids: additional.label_ids,
            description: final_description,
            state_id,
            start,
            parent_id,
            project_id: project_value,
        });
    }

    let start = prompt_select_bool(
        "Start working on this issue now? (creates branch and updates status)",
        false,
    )?;

    let project_value = project_value(project_id, parent_data);
    Ok(InteractiveData {
        title,
        team_id,
        assignee_id,
        priority: None,
        estimate: None,
        label_ids: Vec::new(),
        description: final_description,
        state_id,
        start,
        parent_id,
        project_id: project_value,
    })
}

/// `projectId ?? parentData?.projectId ?? null`.
fn project_value(project_id: Option<String>, parent_data: Option<&Value>) -> Value {
    if let Some(project_id) = project_id {
        return json!(project_id);
    }
    if let Some(parent_data) = parent_data {
        return parent_data.get("projectId").cloned().unwrap_or(Value::Null);
    }
    Value::Null
}

// ---------------------------------------------------------------------------
// Assign-self configuration
// ---------------------------------------------------------------------------

fn should_assign_self_by_default_for_flag_create() -> bool {
    matches!(
        config::issue_create_assign_self(),
        Some(config::AssignSelf::Always)
    )
}

fn should_assign_self_by_default_for_interactive_create() -> Result<bool> {
    match config::issue_create_assign_self() {
        Some(config::AssignSelf::Always) => Ok(true),
        Some(config::AssignSelf::Never) => Ok(false),
        _ => get_linear_auto_assign_to_self(),
    }
}

fn get_linear_auto_assign_to_self() -> Result<bool> {
    let client = graphql::client()?;
    let result = client.request(GET_USER_SETTINGS_QUERY, json!({}))?;
    Ok(result
        .get("userSettings")
        .and_then(|settings| settings.get("autoAssignToSelf"))
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

fn should_ask_project_during_interactive_create() -> bool {
    config::issue_create_ask_project() == Some(true)
}

// ---------------------------------------------------------------------------
// Scoped template resolution (ported from utils/templates.ts, issue scope)
// ---------------------------------------------------------------------------

fn resolve_template_scoped(
    reference: &str,
    template_type: &str,
    team_ids: &[String],
) -> Result<Value> {
    crate::linear_url::reject_linear_url(reference, "a template name or UUID")?;
    if linear::is_linear_uuid(reference) {
        let template = tmpl::fetch_template(reference)?;
        assert_template_in_scope(&template, template_type, team_ids)?;
        return Ok(template);
    }

    let all = tmpl::fetch_templates()?;
    let wanted = reference.to_lowercase();
    let by_name: Vec<Value> = all
        .iter()
        .filter(|template| tmpl::template_name(template).to_lowercase() == wanted)
        .cloned()
        .collect();
    let in_scope = |template: &Value| {
        tmpl::template_type(template) == template_type
            && tmpl::template_is_available_to(template, team_ids)
    };
    let candidates: Vec<Value> = by_name.iter().filter(|t| in_scope(t)).cloned().collect();

    if candidates.len() == 1 {
        return Ok(candidates[0].clone());
    }

    if candidates.is_empty() {
        if !by_name.is_empty() {
            return Err(scope_mismatch_error(&by_name, template_type));
        }
        let names = available_names(&all, template_type, team_ids);
        let what = format!("{template_type} templates");
        let suggestion = if names.is_empty() {
            format!(
                "No {what} are available here. Run `linear template list` to see every template."
            )
        } else {
            format!(
                "Available {what}: {}. Run `linear template list` to see every template.",
                names
                    .iter()
                    .map(|name| format!("\"{name}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        return Err(CliError::not_found("Template", reference).suggestion(suggestion));
    }

    let ids = candidates
        .iter()
        .map(|template| {
            format!(
                "{} ({}, {})",
                tmpl::template_id(template),
                tmpl::template_type(template),
                tmpl::template_scope_label(template)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(CliError::validation(format!(
        "Template name \"{reference}\" is ambiguous: it matches {} templates",
        candidates.len()
    ))
    .suggestion(format!("Pass the template ID instead: {ids}")))
}

fn available_names(all: &[Value], template_type: &str, team_ids: &[String]) -> Vec<String> {
    let mut names: Vec<String> = all
        .iter()
        .filter(|template| {
            tmpl::template_type(template) == template_type
                && tmpl::template_is_available_to(template, team_ids)
        })
        .map(tmpl::template_name)
        .collect();
    names.sort();
    names.dedup();
    names
}

fn assert_template_in_scope(
    template: &Value,
    template_type: &str,
    team_ids: &[String],
) -> Result<()> {
    if tmpl::template_type(template) != template_type {
        return Err(wrong_type_error(template, template_type));
    }
    if !tmpl::template_is_available_to(template, team_ids) {
        return match template.get("team").filter(|team| !team.is_null()) {
            None => Err(CliError::cli(format!(
                "Template \"{}\" is not available here",
                tmpl::template_name(template)
            ))),
            Some(team) => {
                let key = team.get("key").and_then(Value::as_str).unwrap_or("");
                Err(other_team_error(
                    &tmpl::template_name(template),
                    &[key.to_string()],
                    template_type,
                ))
            }
        };
    }
    Ok(())
}

fn scope_mismatch_error(matches: &[Value], template_type: &str) -> CliError {
    let same_type: Vec<&Value> = matches
        .iter()
        .filter(|template| tmpl::template_type(template) == template_type)
        .collect();
    if !same_type.is_empty() {
        let mut team_keys: Vec<String> = Vec::new();
        for template in &same_type {
            if let Some(key) = template
                .get("team")
                .filter(|team| !team.is_null())
                .and_then(|team| team.get("key"))
                .and_then(Value::as_str)
            {
                if !team_keys.iter().any(|existing| existing == key) {
                    team_keys.push(key.to_string());
                }
            }
        }
        if !team_keys.is_empty() {
            return other_team_error(
                &tmpl::template_name(same_type[0]),
                &team_keys,
                template_type,
            );
        }
    }
    wrong_type_error(&matches[0], template_type)
}

fn wrong_type_error(template: &Value, template_type: &str) -> CliError {
    CliError::validation(format!(
        "Template \"{}\" is {}, not {}",
        tmpl::template_name(template),
        describe_type(&tmpl::template_type(template)),
        describe_type(template_type)
    ))
    .suggestion(format!(
        "Run `linear template list --type {template_type}` to see the {template_type} templates."
    ))
}

fn other_team_error(name: &str, team_keys: &[String], template_type: &str) -> CliError {
    let teams = team_keys.join(", ");
    let plural = if team_keys.len() == 1 { "" } else { "s" };
    CliError::validation(format!(
        "Template \"{name}\" belongs to team{plural} {teams} and cannot be applied here"
    ))
    .suggestion(format!(
        "Pass --team {}, or pick a workspace template or one from the target team with `linear template list --type {template_type} --team <team>`.",
        team_keys.first().map(String::as_str).unwrap_or("")
    ))
}

fn describe_type(template_type: &str) -> String {
    let article = match template_type.chars().next() {
        Some(first) if "aeiouAEIOU".contains(first) => "an",
        _ => "a",
    };
    format!("{article} {template_type} template")
}

// `startWorkOnIssue` lives in `issue_start` - one implementation, shared by
// `issue start` and `issue create --start`. This module used to carry a near-copy
// of it (without `--from-ref`/`--branch`), which is exactly the duplication the
// workflow-verb ticket exists to remove.

// ---------------------------------------------------------------------------
// Prompt helpers (line-based equivalents of @cliffy/prompt)
// ---------------------------------------------------------------------------

/// Read one line from the terminal, returning `default` on an empty line or
/// EOF. Only called after [`crate::prompt::is_interactive`] has confirmed the
/// run can block.
fn prompt_text(message: &str, default: &str) -> Result<String> {
    eprint!("{message}");
    if !default.is_empty() {
        eprint!(" [{default}]");
    }
    eprint!(" ");
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
    if read == 0 {
        return Ok(default.to_string());
    }
    let value = line.trim().to_string();
    if value.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(value)
    }
}

/// `Input.prompt` with `minLength: 1`: re-prompt until something is entered.
fn prompt_text_required(message: &str) -> Result<String> {
    loop {
        let value = prompt_text(message, "")?;
        if !value.is_empty() {
            return Ok(value);
        }
        // Guard against a closed stdin looping forever.
        if !prompt::is_interactive() {
            return Err(CliError::validation("No title provided"));
        }
    }
}

/// A numbered single-choice prompt returning the selected option's *value*.
fn prompt_select(
    message: &str,
    options: &[(String, String)],
    default_index: usize,
) -> Result<String> {
    if options.is_empty() {
        return Err(CliError::validation("No options available"));
    }
    eprintln!("{message}");
    for (index, (_, display)) in options.iter().enumerate() {
        let marker = if index == default_index {
            " (default)"
        } else {
            ""
        };
        eprintln!("  {}. {display}{marker}", index + 1);
    }

    let stdin = std::io::stdin();
    loop {
        eprint!(
            "Enter a number (1-{}) [{}]: ",
            options.len(),
            default_index + 1
        );
        let _ = std::io::stderr().flush();

        let mut line = String::new();
        let read = stdin
            .lock()
            .read_line(&mut line)
            .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
        if read == 0 {
            return Ok(options[default_index].0.clone());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(options[default_index].0.clone());
        }
        if let Ok(choice) = trimmed.parse::<usize>() {
            if choice >= 1 && choice <= options.len() {
                return Ok(options[choice - 1].0.clone());
            }
        }
        eprintln!("Please enter a number between 1 and {}.", options.len());
    }
}

/// A boolean `Select.prompt` with `No`/`Yes` options.
fn prompt_select_bool(message: &str, default: bool) -> Result<bool> {
    let default_index = if default { 1 } else { 0 };
    let options = vec![
        ("false".to_string(), "No".to_string()),
        ("true".to_string(), "Yes".to_string()),
    ];
    let selected = prompt_select(message, &options, default_index)?;
    Ok(selected == "true")
}

/// A line-based checkbox: numbered options, whitespace/comma-separated
/// selections. Returns the selected option values in input order.
fn prompt_checkbox(message: &str, options: &[(String, String)]) -> Result<Vec<String>> {
    if options.is_empty() {
        return Ok(Vec::new());
    }
    eprintln!("{message}");
    for (index, (_, display)) in options.iter().enumerate() {
        eprintln!("  {}. {display}", index + 1);
    }
    eprint!("Enter numbers separated by spaces (leave blank for none): ");
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
    if read == 0 {
        return Ok(Vec::new());
    }

    let mut selected = Vec::new();
    for token in line.split(|c: char| c.is_whitespace() || c == ',') {
        if token.is_empty() {
            continue;
        }
        if let Ok(choice) = token.parse::<usize>() {
            if choice >= 1 && choice <= options.len() {
                let value = options[choice - 1].0.clone();
                if !selected.contains(&value) {
                    selected.push(value);
                }
            }
        }
    }
    Ok(selected)
}
