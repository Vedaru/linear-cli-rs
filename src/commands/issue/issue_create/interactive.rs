use std::io::{BufRead, Write};

use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::linear::{self, WorkflowState};
use crate::{config, editor, graphql, output, prompt};

use super::{
    created_issue, resolve_parent_issue_for_create, resolve_project_id_for_create,
    IssueCreateArgs, CREATE_ISSUE_MUTATION, GET_USER_SETTINGS_QUERY,
};

/// The interactive wizard. Mirrors `promptInteractiveIssueCreation` plus the
/// `resolveParentIssueForCreate`/`resolveProjectIdForCreate` calls that precede
/// it, and the `issueCreate` mutation that follows.
pub(super) fn run_interactive(args: &IssueCreateArgs, interactive: bool) -> Result<()> {
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
        crate::commands::issue::issue_start::start_work_on_issue(issue_id, &team_key, None, None, true, args.json)?;
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
