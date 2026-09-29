//! `linear issue update` — port of `src/commands/issue/issue-update.ts`.
//!
//! Every field is optional; only the flags actually passed are sent in the
//! `issueUpdate` input. Clearing a field requires its explicit `--clear-*`
//! flag (or `--unassign`), never a falsy value, so an omitted flag leaves the
//! field untouched. The conflicting-flag checks run in upstream's order, so the
//! first problem reported is the same one upstream would report.

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::issue_identifier::get_team_key_from_issue_identifier;
use crate::linear;
use crate::{graphql, output};

const GET_ISSUE_PROJECT_ID_QUERY: &str = r#"
query GetIssueProjectId($id: String!) {
  issue(id: $id) {
    project {
      id
    }
  }
}
"#;

const UPDATE_ISSUE_MUTATION: &str = r#"
mutation UpdateIssue($id: String!, $input: IssueUpdateInput!) {
  issueUpdate(id: $id, input: $input) {
    success
    issue { id, identifier, url, title }
  }
}
"#;

#[derive(Args, Debug)]
pub struct IssueUpdateArgs {
    /// Assign the issue to 'self' or someone (by username or name)
    #[arg(short = 'a', long, value_name = "assignee")]
    pub assignee: Option<String>,
    /// Clear the issue's assignee (cannot be combined with --assignee)
    #[arg(long)]
    pub unassign: bool,
    /// Due date of the issue. Use --clear-due-date to remove it
    #[arg(long = "due-date", value_name = "dueDate")]
    pub due_date: Option<String>,
    /// Remove the issue's due date (cannot be combined with --due-date)
    #[arg(long = "clear-due-date")]
    pub clear_due_date: bool,
    /// Parent issue (if any) as a team_number code. Use --clear-parent to remove it
    #[arg(long, value_name = "parent")]
    pub parent: Option<String>,
    /// Remove the issue's parent (cannot be combined with --parent)
    #[arg(long = "clear-parent")]
    pub clear_parent: bool,
    /// Priority of the issue (1-4, descending priority)
    #[arg(short = 'p', long, value_name = "priority")]
    pub priority: Option<i64>,
    /// Points estimate of the issue. Use --clear-estimate to remove it
    #[arg(long, value_name = "estimate")]
    pub estimate: Option<i64>,
    /// Remove the issue's estimate (cannot be combined with --estimate)
    #[arg(long = "clear-estimate")]
    pub clear_estimate: bool,
    /// Description of the issue
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// Read description from a file (preferred for markdown content)
    #[arg(long = "description-file", value_name = "path")]
    pub description_file: Option<String>,
    /// Issue label associated with the issue; replaces the issue's entire label
    /// set. May be repeated. Use --add-label/--remove-label to change labels
    /// incrementally.
    #[arg(short = 'l', long = "label", value_name = "label", action = clap::ArgAction::Append)]
    pub label: Vec<String>,
    /// Add a label to the issue, keeping its existing labels. May be repeated.
    #[arg(long = "add-label", value_name = "label", action = clap::ArgAction::Append)]
    pub add_label: Vec<String>,
    /// Remove a label from the issue, keeping its other labels (does not delete
    /// the label from the team). May be repeated.
    #[arg(long = "remove-label", value_name = "label", action = clap::ArgAction::Append)]
    pub remove_label: Vec<String>,
    /// Team (key, name, or ID) to move the issue to
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Project to assign the issue to (UUID, slug ID, or name). Use
    /// --clear-project to remove it
    #[arg(long, value_name = "project")]
    pub project: Option<String>,
    /// Remove the issue from its project (cannot be combined with --project or
    /// --milestone)
    #[arg(long = "clear-project")]
    pub clear_project: bool,
    /// Workflow state for the issue (by name or type)
    #[arg(short = 's', long, value_name = "state")]
    pub state: Option<String>,
    /// Project milestone (UUID, or name when --project is set or the issue
    /// already has a project). Use --clear-milestone to remove it
    #[arg(long, value_name = "milestone")]
    pub milestone: Option<String>,
    /// Remove the issue from its project milestone (cannot be combined with
    /// --milestone)
    #[arg(long = "clear-milestone")]
    pub clear_milestone: bool,
    /// Cycle name, number, 'active'/'now', 'next', 'previous', or a relative
    /// offset like +1 (use --cycle=-1 for negatives). Use --clear-cycle to
    /// remove the issue from its cycle
    #[arg(long, value_name = "cycle")]
    pub cycle: Option<String>,
    /// Remove the issue from its cycle
    #[arg(long = "clear-cycle")]
    pub clear_cycle: bool,
    /// Title of the issue
    #[arg(short = 't', long, value_name = "title")]
    pub title: Option<String>,
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
}

/// Whether a provided optional string counts as a value, matching upstream's
/// truthiness checks (`description && descriptionFile`).
fn truthy(value: Option<&str>) -> bool {
    value.is_some_and(|text| !text.is_empty())
}

pub fn run(args: IssueUpdateArgs) -> Result<()> {
    let result = (|| -> Result<()> {
        let IssueUpdateArgs {
            assignee,
            unassign,
            due_date,
            clear_due_date,
            parent,
            clear_parent,
            priority,
            estimate,
            clear_estimate,
            description,
            description_file,
            label: labels,
            add_label,
            remove_label,
            team,
            project,
            clear_project,
            state,
            milestone,
            clear_milestone,
            cycle,
            clear_cycle,
            title,
            issue_id: issue_id_arg,
        } = args;

        // ----- conflicting-flag validation (upstream order) -----

        if unassign && assignee.is_some() {
            return Err(CliError::validation(
                "Cannot specify both --assignee and --unassign",
            )
            .suggestion(
                "Use --assignee <user> to set an assignee, or --unassign on its own to clear it.",
            ));
        }

        if clear_cycle && cycle.is_some() {
            return Err(CliError::validation(
                "Cannot specify both --cycle and --clear-cycle",
            )
            .suggestion(
                "Use --cycle <cycle> to set a cycle, or --clear-cycle on its own to remove it.",
            ));
        }

        if clear_due_date && due_date.is_some() {
            return Err(CliError::validation(
                "Cannot specify both --due-date and --clear-due-date",
            )
            .suggestion(
                "Use --due-date <date> to set a due date, or --clear-due-date on its own to remove it.",
            ));
        }

        // `is_some`, not truthiness: `--estimate 0` is an explicit value.
        if clear_estimate && estimate.is_some() {
            return Err(CliError::validation(
                "Cannot specify both --estimate and --clear-estimate",
            )
            .suggestion(
                "Use --estimate <points> to set an estimate, or --clear-estimate on its own to remove it.",
            ));
        }

        if clear_parent && parent.is_some() {
            return Err(CliError::validation(
                "Cannot specify both --parent and --clear-parent",
            )
            .suggestion(
                "Use --parent <issue> to set a parent, or --clear-parent on its own to remove it.",
            ));
        }

        if clear_project && project.is_some() {
            return Err(CliError::validation(
                "Cannot specify both --project and --clear-project",
            )
            .suggestion(
                "Use --project <project> to set a project, or --clear-project on its own to remove it.",
            ));
        }

        // A milestone belongs to a project, so keeping one while removing the
        // project is contradictory (and a milestone name would resolve against
        // the project being removed).
        if clear_project && milestone.is_some() {
            return Err(CliError::validation(
                "Cannot specify --milestone while clearing the issue's project",
            )
            .suggestion(
                "Drop --milestone, or replace it with --clear-milestone to remove both the project and the milestone.",
            ));
        }

        if clear_milestone && milestone.is_some() {
            return Err(CliError::validation(
                "Cannot specify both --milestone and --clear-milestone",
            )
            .suggestion(
                "Use --milestone <milestone> to set a milestone, or --clear-milestone on its own to remove it.",
            ));
        }

        if !labels.is_empty() && (!add_label.is_empty() || !remove_label.is_empty()) {
            return Err(CliError::validation(
                "Cannot combine --label with --add-label or --remove-label",
            )
            .suggestion(
                "--label replaces the issue's entire label set. Use it alone to set the exact set, or use --add-label/--remove-label alone to change it incrementally.",
            ));
        }

        // Label names resolve against the issue's (destination) team, so a
        // team move combined with incremental label changes would silently
        // make source-team labels unresolvable.
        if team.is_some() && (!add_label.is_empty() || !remove_label.is_empty()) {
            return Err(CliError::validation(
                "Cannot combine --team with --add-label or --remove-label",
            )
            .suggestion("Move the issue with --team first, then change labels in a second update."));
        }

        if truthy(description.as_deref()) && description_file.is_some() {
            return Err(CliError::validation(
                "Cannot specify both --description and --description-file",
            ));
        }

        // Read description from file if provided. An empty --description is
        // falsy and so is silently replaced by the file's contents, matching
        // upstream.
        let mut final_description = description;
        if let Some(path) = description_file {
            final_description = Some(std::fs::read_to_string(&path).map_err(|error| {
                CliError::validation(format!("Failed to read description file: {path}"))
                    .suggestion(format!("Error: {error}"))
            })?);
        }

        // ----- resolve the issue and its team -----

        let Some(issue_id) = linear::get_issue_identifier(issue_id_arg.as_deref())? else {
            return Err(CliError::validation("Could not determine issue ID").suggestion(
                "Please provide an issue ID like 'ENG-123' or run from a branch with an issue ID.",
            ));
        };

        // An explicit --team may be a key, name, or UUID; otherwise the team
        // is the one in the issue identifier.
        let team_ref = team
            .clone()
            .or_else(|| get_team_key_from_issue_identifier(&issue_id));
        let Some(team_ref) = team_ref.filter(|value| !value.is_empty()) else {
            return Err(CliError::validation(
                "Could not determine team key from issue ID",
            ));
        };

        // The mutation needs the UUID; state and label lookups use the key.
        let resolved_team = linear::resolve_team(&team_ref)?;
        let team_id = resolved_team.id;
        let team_key = resolved_team.key;

        // ----- optional field resolution -----

        let state_id = match &state {
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

        let assignee_id = match assignee.as_deref() {
            None => None,
            Some(assignee) => match linear::lookup_user_id(assignee)? {
                Some(id) => Some(id),
                None => return Err(CliError::not_found("User", assignee)),
            },
        };

        // Resolves label names to IDs, deduped by resolved ID so case variants
        // of the same label collapse to one entry.
        let resolve_label_ids = |names: &[String]| -> Result<Vec<String>> {
            let mut ids: Vec<String> = Vec::new();
            for name in names {
                let label_id = linear::get_issue_label_id_by_name_for_team(name, &team_key)?;
                let Some(label_id) = label_id else {
                    return Err(CliError::not_found("Issue label", name).suggestion(format!(
                        "Run `linear label list --team {team_key}` to see available labels."
                    )));
                };
                if !ids.contains(&label_id) {
                    ids.push(label_id);
                }
            }
            Ok(ids)
        };

        let label_ids = if labels.is_empty() {
            Vec::new()
        } else {
            resolve_label_ids(&labels)?
        };
        let added_label_ids = if add_label.is_empty() {
            Vec::new()
        } else {
            resolve_label_ids(&add_label)?
        };
        let removed_label_ids = if remove_label.is_empty() {
            Vec::new()
        } else {
            resolve_label_ids(&remove_label)?
        };

        if added_label_ids
            .iter()
            .any(|id| removed_label_ids.contains(id))
        {
            return Err(CliError::validation(
                "Cannot add and remove the same label in one update",
            )
            .suggestion("Remove the duplicate label from either --add-label or --remove-label."));
        }

        let project_id = match &project {
            None => None,
            Some(project) => match linear::get_project_id_by_name(project)? {
                Some(project_id) => Some(project_id),
                None => {
                    return Err(CliError::not_found("Project", project).suggestion(
                        "Pass a project UUID, slug ID (from `linear project list`), or exact project name.",
                    ))
                }
            },
        };

        let project_milestone_id = match &milestone {
            None => None,
            Some(milestone) => {
                if linear::is_linear_uuid(milestone) {
                    Some(milestone.clone())
                } else {
                    let milestone_project_id = match project_id.clone() {
                        Some(project_id) => Some(project_id),
                        None => get_issue_project_id(&issue_id)?,
                    };
                    let Some(milestone_project_id) = milestone_project_id else {
                        return Err(CliError::validation(
                            "--milestone requires --project to be set (issue has no existing project)",
                        )
                        .suggestion(
                            "Use --project to specify the project for the milestone, or pass a milestone UUID directly.",
                        ));
                    };
                    Some(linear::resolve_milestone_id(
                        milestone,
                        Some(&milestone_project_id),
                    )?)
                }
            }
        };

        let cycle_id = match &cycle {
            None => None,
            Some(cycle) => Some(linear::get_cycle_id_by_name_or_number(&team_id, cycle)?),
        };

        // ----- build the update input -----
        //
        // Only fields that were provided are included. Clearing requires an
        // explicit flag; a field is never set to null implicitly.
        let mut input = Map::new();

        if let Some(title) = title {
            input.insert("title".to_string(), json!(title));
        }
        if unassign {
            input.insert("assigneeId".to_string(), Value::Null);
        } else if let Some(assignee_id) = &assignee_id {
            input.insert("assigneeId".to_string(), json!(assignee_id));
        }
        if clear_due_date {
            input.insert("dueDate".to_string(), Value::Null);
        } else if let Some(due_date) = &due_date {
            input.insert("dueDate".to_string(), json!(due_date));
        }
        if clear_parent {
            input.insert("parentId".to_string(), Value::Null);
        } else if let Some(parent) = &parent {
            let Some(parent_identifier) = linear::get_issue_identifier(Some(parent))? else {
                return Err(CliError::validation(format!(
                    "Could not resolve parent issue identifier: {parent}"
                )));
            };
            let Some(parent_id) = linear::get_issue_id(&parent_identifier)? else {
                return Err(CliError::not_found("Parent issue", &parent_identifier));
            };
            input.insert("parentId".to_string(), json!(parent_id));
        }
        if let Some(priority) = priority {
            input.insert("priority".to_string(), json!(priority));
        }
        if clear_estimate {
            input.insert("estimate".to_string(), Value::Null);
        } else if let Some(estimate) = estimate {
            input.insert("estimate".to_string(), json!(estimate));
        }
        if let Some(final_description) = &final_description {
            input.insert("description".to_string(), json!(final_description));
        }
        if !labels.is_empty() {
            input.insert("labelIds".to_string(), json!(label_ids));
        } else {
            if !add_label.is_empty() {
                input.insert("addedLabelIds".to_string(), json!(added_label_ids));
            }
            if !remove_label.is_empty() {
                input.insert("removedLabelIds".to_string(), json!(removed_label_ids));
            }
        }
        input.insert("teamId".to_string(), json!(team_id));
        if clear_project {
            input.insert("projectId".to_string(), Value::Null);
        } else if let Some(project_id) = &project_id {
            input.insert("projectId".to_string(), json!(project_id));
        }
        if clear_milestone {
            input.insert("projectMilestoneId".to_string(), Value::Null);
        } else if let Some(project_milestone_id) = &project_milestone_id {
            input.insert("projectMilestoneId".to_string(), json!(project_milestone_id));
        }
        if clear_cycle {
            input.insert("cycleId".to_string(), Value::Null);
        } else if let Some(cycle_id) = &cycle_id {
            input.insert("cycleId".to_string(), json!(cycle_id));
        }
        if let Some(state_id) = &state_id {
            input.insert("stateId".to_string(), json!(state_id));
        }

        output::line(&format!("Updating issue {issue_id}"));
        output::blank();

        let client = graphql::client()?;
        let data = client.request(
            UPDATE_ISSUE_MUTATION,
            json!({ "id": issue_id, "input": Value::Object(input) }),
        )?;

        let updated = data
            .get("issueUpdate")
            .and_then(|value| value.get("success"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !updated {
            return Err(CliError::cli("Issue update failed"));
        }

        let issue = data
            .get("issueUpdate")
            .and_then(|value| value.get("issue"))
            .filter(|value| !value.is_null())
            .ok_or_else(|| CliError::cli("Issue update failed - no issue returned"))?;

        let identifier = issue.get("identifier").and_then(Value::as_str).unwrap_or("");
        let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
        let url = issue.get("url").and_then(Value::as_str).unwrap_or("");

        output::line(&format!("✓ Updated issue {identifier}: {title}"));
        output::line(url);
        Ok(())
    })();

    result.map_err(|error| error.with_context("Failed to update issue"))
}

/// `getIssueProjectId`: the UUID of the issue's project, or `None` when the
/// issue has no project. Used to resolve a milestone name when `--project` was
/// not passed.
fn get_issue_project_id(issue_id: &str) -> Result<Option<String>> {
    let client = graphql::client()?;
    let data = client.request(GET_ISSUE_PROJECT_ID_QUERY, json!({ "id": issue_id }))?;
    Ok(data
        .get("issue")
        .filter(|value| !value.is_null())
        .and_then(|issue| issue.get("project"))
        .filter(|value| !value.is_null())
        .and_then(|project| project.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string))
}
