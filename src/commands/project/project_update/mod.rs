//! `linear project update` — port of
//! `src/commands/project/project-update.ts`.
//!
//! The group `mod.rs` supplies the `Failed to update project` context, so this
//! module returns bare errors.
//!
//! `ProjectUpdateInput` only has replace-style `teamIds`/`labelIds`, so the
//! `--add-*`/`--remove-*` flags read the current set (following every page),
//! compute the new one, and send the full set. Initiative membership is a join
//! row with no field on the input, so it is applied through its own mutations
//! one at a time.

use std::collections::HashSet;

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output};

use super::project_create::{api_status_type, resolve_project_content};
use super::project_description::resolve_project_description;

mod helpers;

const UPDATE_PROJECT_MUTATION: &str = r#"
mutation UpdateProject($id: String!, $input: ProjectUpdateInput!) {
  projectUpdate(id: $id, input: $input) {
    success
    project {
      id
      slugId
      name
      description
      url
      updatedAt
    }
  }
}
"#;

const GET_PROJECT_STATUSES_QUERY: &str = {
    // See `project_create`: one definition, in `linear/queries.rs`. The local name stays so the call
    // sites read the same as before, but there is no second document to drift from the first.
    crate::linear::GET_PROJECT_STATUSES_QUERY
};

const GET_PROJECT_TEAMS_FOR_UPDATE_QUERY: &str = r#"
query GetProjectTeamsForUpdate($id: String!, $after: String) {
  project(id: $id) {
    teams(first: 250, after: $after) {
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
  }
}
"#;

const GET_PROJECT_LABELS_FOR_UPDATE_QUERY: &str = r#"
query GetProjectLabelsForUpdate($id: String!, $after: String) {
  project(id: $id) {
    labels(first: 250, after: $after) {
      nodes {
        id
        name
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

const GET_PROJECT_INITIATIVE_LINKS_FOR_UPDATE_QUERY: &str = r#"
query GetProjectInitiativeLinksForUpdate($id: String!, $after: String) {
  project(id: $id) {
    id
    name
    url
    initiativeToProjects(first: 250, after: $after) {
      nodes {
        id
        initiative {
          id
          name
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

const GET_INITIATIVE_BY_ID_FOR_UPDATE_QUERY: &str = r#"
query GetInitiativeByIdForUpdate($id: ID!) {
  initiatives(filter: { id: { eq: $id } }) {
    nodes {
      id
      name
    }
  }
}
"#;

const ADD_PROJECT_TO_INITIATIVE_FOR_UPDATE_MUTATION: &str = r#"
mutation AddProjectToInitiativeForUpdate($input: InitiativeToProjectCreateInput!) {
  initiativeToProjectCreate(input: $input) {
    success
  }
}
"#;

const REMOVE_PROJECT_FROM_INITIATIVE_FOR_UPDATE_MUTATION: &str = r#"
mutation RemoveProjectFromInitiativeForUpdate($id: String!) {
  initiativeToProjectDelete(id: $id) {
    success
  }
}
"#;

#[derive(Args, Debug)]
pub struct ProjectUpdateArgs {
    /// Project ID, slug, or name
    #[arg(value_name = "projectId")]
    pub project_id: String,
    /// Project name
    #[arg(short = 'n', long, value_name = "name")]
    pub name: Option<String>,
    /// Project description (max 255 characters, enforced by Linear's API)
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// Read project description from file (still subject to the 255-character API limit)
    #[arg(short = 'f', long = "description-file", value_name = "path")]
    pub description_file: Option<String>,
    /// Project overview markdown
    #[arg(long, value_name = "markdown")]
    pub content: Option<String>,
    /// Read project overview markdown from a file
    #[arg(long = "content-file", value_name = "path")]
    pub content_file: Option<String>,
    /// Status (planned, started, paused, completed, canceled, backlog)
    #[arg(short = 's', long, value_name = "status")]
    pub status: Option<String>,
    /// Project lead (username, email, or @me). Use --clear-lead to remove it
    #[arg(short = 'l', long, value_name = "lead")]
    pub lead: Option<String>,
    /// Remove the project's lead (cannot be combined with --lead)
    #[arg(long = "clear-lead")]
    pub clear_lead: bool,
    /// Start date (YYYY-MM-DD). Use --clear-start-date to remove it
    #[arg(long = "start-date", value_name = "startDate")]
    pub start_date: Option<String>,
    /// Remove the project's start date (cannot be combined with --start-date)
    #[arg(long = "clear-start-date")]
    pub clear_start_date: bool,
    /// Target date (YYYY-MM-DD). Use --clear-target-date to remove it
    #[arg(long = "target-date", value_name = "targetDate")]
    pub target_date: Option<String>,
    /// Remove the project's target date (cannot be combined with --target-date)
    #[arg(long = "clear-target-date")]
    pub clear_target_date: bool,
    /// Team key, name, or ID; replaces the project's entire team set. May be repeated. Use --add-team/--remove-team to change teams incrementally.
    #[arg(short = 't', long = "team", value_name = "team")]
    pub team: Vec<String>,
    /// Add a team to the project, keeping its existing teams. May be repeated.
    #[arg(long = "add-team", value_name = "team")]
    pub add_team: Vec<String>,
    /// Remove a team from the project, keeping its other teams. May be repeated.
    #[arg(long = "remove-team", value_name = "team")]
    pub remove_team: Vec<String>,
    /// Project label; replaces the project's entire label set. May be repeated. Use --add-label/--remove-label to change labels incrementally.
    #[arg(long = "label", value_name = "label")]
    pub label: Vec<String>,
    /// Add a label to the project, keeping its existing labels. May be repeated.
    #[arg(long = "add-label", value_name = "label")]
    pub add_label: Vec<String>,
    /// Remove a label from the project, keeping its other labels (does not delete the label). May be repeated.
    #[arg(long = "remove-label", value_name = "label")]
    pub remove_label: Vec<String>,
    /// Initiative ID, slug, or name; replaces the project's entire initiative set. May be repeated. Use --add-initiative/--remove-initiative to change initiatives incrementally.
    #[arg(long = "initiative", value_name = "initiative")]
    pub initiative: Vec<String>,
    /// Add the project to an initiative, keeping its existing initiatives. May be repeated.
    #[arg(long = "add-initiative", value_name = "initiative")]
    pub add_initiative: Vec<String>,
    /// Remove the project from an initiative, keeping its other initiatives (does not delete the initiative). May be repeated.
    #[arg(long = "remove-initiative", value_name = "initiative")]
    pub remove_initiative: Vec<String>,
}
pub fn run(args: ProjectUpdateArgs) -> Result<()> {
    let replace_team = !args.team.is_empty();
    let add_team = !args.add_team.is_empty();
    let remove_team = !args.remove_team.is_empty();
    let replace_label = !args.label.is_empty();
    let add_label = !args.add_label.is_empty();
    let remove_label = !args.remove_label.is_empty();
    let replace_initiative = !args.initiative.is_empty();
    let add_initiative = !args.add_initiative.is_empty();
    let remove_initiative = !args.remove_initiative.is_empty();

    // Null checks, not truthiness: an empty --content-file is still an explicit
    // value to forward, so it must count as an update.
    let has_any = args.name.as_ref().is_some_and(|value| !value.is_empty())
        || args.description.is_some()
        || args.description_file.is_some()
        || args.content.is_some()
        || args.content_file.is_some()
        || args.status.as_ref().is_some_and(|value| !value.is_empty())
        || args.lead.as_ref().is_some_and(|value| !value.is_empty())
        || args.clear_lead
        || args.start_date.is_some()
        || args.clear_start_date
        || args.target_date.is_some()
        || args.clear_target_date
        || replace_team
        || add_team
        || remove_team
        || replace_label
        || add_label
        || remove_label
        || replace_initiative
        || add_initiative
        || remove_initiative;
    if !has_any {
        return Err(CliError::validation(
            "At least one update option must be provided",
        )
        .suggestion(
            "Use --name, --description, --description-file, --content, --content-file, --status, --lead, --clear-lead, --start-date, --clear-start-date, --target-date, --clear-target-date, --team, --add-team, --remove-team, --label, --add-label, --remove-label, --initiative, --add-initiative, or --remove-initiative",
        ));
    }

    if args.clear_lead && args.lead.is_some() {
        return Err(
            CliError::validation("Cannot specify both --lead and --clear-lead").suggestion(
                "Use --lead <user> to set a lead, or --clear-lead on its own to remove it.",
            ),
        );
    }
    if args.clear_start_date && args.start_date.is_some() {
        return Err(CliError::validation(
            "Cannot specify both --start-date and --clear-start-date",
        )
        .suggestion(
            "Use --start-date <date> to set a start date, or --clear-start-date on its own to remove it.",
        ));
    }
    if args.clear_target_date && args.target_date.is_some() {
        return Err(CliError::validation(
            "Cannot specify both --target-date and --clear-target-date",
        )
        .suggestion(
            "Use --target-date <date> to set a target date, or --clear-target-date on its own to remove it.",
        ));
    }

    helpers::reject_replace_with_incremental("team", replace_team, add_team, remove_team)?;
    helpers::reject_replace_with_incremental("label", replace_label, add_label, remove_label)?;
    helpers::reject_replace_with_incremental(
        "initiative",
        replace_initiative,
        add_initiative,
        remove_initiative,
    )?;

    for label in args
        .label
        .iter()
        .chain(args.add_label.iter())
        .chain(args.remove_label.iter())
    {
        if label.trim().is_empty() {
            return Err(CliError::validation("Project label cannot be empty")
                .suggestion("Provide a label name, e.g. --label \"My Label\"."));
        }
    }

    let resolved_description = resolve_project_description(
        args.description.as_deref(),
        args.description_file.as_deref(),
    )?;
    let resolved_content =
        resolve_project_content(args.content.as_deref(), args.content_file.as_deref())?;

    if let Some(start_date) = &args.start_date {
        if !is_iso_date(start_date) {
            return Err(CliError::validation(
                "Start date must be in YYYY-MM-DD format",
            ));
        }
    }
    if let Some(target_date) = &args.target_date {
        if !is_iso_date(target_date) {
            return Err(CliError::validation(
                "Target date must be in YYYY-MM-DD format",
            ));
        }
    }

    let client = graphql::client()?;
    let resolved_id = linear::resolve_project_id(&args.project_id)?;

    let mut input = Map::new();

    if let Some(name) = &args.name {
        if !name.is_empty() {
            input.insert("name".to_string(), json!(name));
        }
    }
    if let Some(description) = &resolved_description {
        input.insert("description".to_string(), json!(description));
    }
    if let Some(content) = &resolved_content {
        input.insert("content".to_string(), json!(content));
    }
    // Clearing a field requires an explicit flag; never set a field to null
    // implicitly.
    if args.clear_start_date {
        input.insert("startDate".to_string(), Value::Null);
    } else if let Some(start_date) = &args.start_date {
        input.insert("startDate".to_string(), json!(start_date));
    }
    if args.clear_target_date {
        input.insert("targetDate".to_string(), Value::Null);
    } else if let Some(target_date) = &args.target_date {
        input.insert("targetDate".to_string(), json!(target_date));
    }

    if let Some(status) = &args.status {
        if !status.is_empty() {
            let Some(api_type) = api_status_type(status) else {
                return Err(
                    CliError::validation(format!("Invalid status: {status}")).suggestion(
                        "Valid values: planned, started, paused, completed, canceled, backlog",
                    ),
                );
            };
            let data = client.request(GET_PROJECT_STATUSES_QUERY, json!({}))?;
            let nodes = data
                .pointer("/projectStatuses/nodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let matching = nodes
                .iter()
                .find(|node| node.get("type").and_then(Value::as_str) == Some(api_type))
                .and_then(|node| node.get("id"))
                .and_then(Value::as_str);
            let Some(status_id) = matching else {
                return Err(CliError::not_found("Project status", api_type));
            };
            input.insert("statusId".to_string(), json!(status_id));
        }
    }

    if args.clear_lead {
        input.insert("leadId".to_string(), Value::Null);
    } else if let Some(lead) = &args.lead {
        if !lead.is_empty() {
            let Some(lead_id) = linear::lookup_user_id(lead)? else {
                return Err(CliError::not_found("Lead", lead));
            };
            input.insert("leadId".to_string(), json!(lead_id));
        }
    }

    if replace_team {
        let ids: Vec<String> = linear::resolve_teams(&args.team)?
            .into_iter()
            .map(|team| team.id)
            .collect();
        input.insert("teamIds".to_string(), json!(ids));
    } else if add_team || remove_team {
        let added: Vec<helpers::ResolvedRef> = linear::resolve_teams(&args.add_team)?
            .into_iter()
            .map(|team| helpers::ResolvedRef {
                id: team.id,
                label: team.key,
            })
            .collect();
        let removed: Vec<helpers::ResolvedRef> = linear::resolve_teams(&args.remove_team)?
            .into_iter()
            .map(|team| helpers::ResolvedRef {
                id: team.id,
                label: team.key,
            })
            .collect();
        helpers::reject_add_remove_overlap("team", &added, &removed)?;
        let current = helpers::fetch_all_pages(|after| {
            let data = client.request(
                GET_PROJECT_TEAMS_FOR_UPDATE_QUERY,
                json!({ "id": &resolved_id, "after": after }),
            )?;
            Ok(helpers::connection_page(&data, "/project/teams"))
        })?;
        let current_ids: Vec<String> = current
            .iter()
            .filter_map(|team| team.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        let team_ids = helpers::apply_collection_edit(&current_ids, &added, &removed, |reference| {
            let current_text = current
                .iter()
                .map(|team| {
                    format!(
                        "{} ({})",
                        team.get("key").and_then(Value::as_str).unwrap_or(""),
                        team.get("name").and_then(Value::as_str).unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            CliError::validation(format!(
                "Cannot remove team \"{}\": it is not on this project",
                reference.label
            ))
            .suggestion(format!(
                "Current teams: {current_text}. Use --add-team to add one."
            ))
        })?;
        if team_ids.is_empty() {
            return Err(CliError::validation(
                "Removing these teams would leave the project with no teams; Linear requires at least one",
            )
            .suggestion(
                "Keep at least one team, or use --team to replace the set.",
            ));
        }
        input.insert("teamIds".to_string(), json!(team_ids));
    }

    if replace_label {
        let ids: Vec<String> = helpers::resolve_project_labels(&args.label)?
            .into_iter()
            .map(|reference| reference.id)
            .collect();
        input.insert("labelIds".to_string(), json!(ids));
    } else if add_label || remove_label {
        let added = helpers::resolve_project_labels(&args.add_label)?;
        let removed = helpers::resolve_project_labels(&args.remove_label)?;
        helpers::reject_add_remove_overlap("label", &added, &removed)?;
        let current = helpers::fetch_all_pages(|after| {
            let data = client.request(
                GET_PROJECT_LABELS_FOR_UPDATE_QUERY,
                json!({ "id": &resolved_id, "after": after }),
            )?;
            Ok(helpers::connection_page(&data, "/project/labels"))
        })?;
        let current_ids: Vec<String> = current
            .iter()
            .filter_map(|label| label.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        let label_ids = helpers::apply_collection_edit(&current_ids, &added, &removed, |reference| {
            let suggestion = if current.is_empty() {
                "The project has no labels. Use --add-label to add one.".to_string()
            } else {
                format!(
                    "Current labels: {}. Use --add-label to add one.",
                    current
                        .iter()
                        .filter_map(|label| label.get("name").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            CliError::validation(format!(
                "Cannot remove label \"{}\": it is not on this project",
                reference.label
            ))
            .suggestion(suggestion)
        })?;
        input.insert("labelIds".to_string(), json!(label_ids));
    }

    let mut initiative_changes: Vec<helpers::InitiativeChange> = Vec::new();
    let mut project_from_links: Option<(String, String)> = None;
    if replace_initiative || add_initiative || remove_initiative {
        let replacement = if replace_initiative {
            Some(helpers::resolve_initiatives(&client, &args.initiative)?)
        } else {
            None
        };
        let added = helpers::resolve_initiatives(&client, &args.add_initiative)?;
        let removed = helpers::resolve_initiatives(&client, &args.remove_initiative)?;
        helpers::reject_add_remove_overlap("initiative", &added, &removed)?;

        let links = helpers::fetch_all_pages(|after| {
            let data = client.request(
                GET_PROJECT_INITIATIVE_LINKS_FOR_UPDATE_QUERY,
                json!({ "id": &resolved_id, "after": after }),
            )?;
            project_from_links = data.pointer("/project").map(|project| {
                (
                    project
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    project
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                )
            });
            Ok(helpers::connection_page(&data, "/project/initiativeToProjects"))
        })?;
        let current_ids: Vec<String> = links
            .iter()
            .filter_map(|link| {
                link.pointer("/initiative/id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let desired_ids: Vec<String> = match &replacement {
            Some(replacement) => replacement
                .iter()
                .map(|reference| reference.id.clone())
                .collect(),
            None => {
                let current_names = links
                    .iter()
                    .filter_map(|link| link.pointer("/initiative/name").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(", ");
                helpers::apply_collection_edit(&current_ids, &added, &removed, |reference| {
                    let suggestion = if links.is_empty() {
                        "The project is not linked to any initiative. Use --add-initiative to link one."
                            .to_string()
                    } else {
                        format!(
                            "Current initiatives: {current_names}. Use --add-initiative to link one."
                        )
                    };
                    CliError::validation(format!(
                        "Cannot remove initiative \"{}\": it is not linked to this project",
                        reference.label
                    ))
                    .suggestion(suggestion)
                })?
            }
        };
        let desired: HashSet<&str> = desired_ids.iter().map(String::as_str).collect();
        let current: HashSet<&str> = current_ids.iter().map(String::as_str).collect();
        let label_for = |id: &str| -> String {
            replacement
                .iter()
                .flat_map(|references| references.iter())
                .chain(added.iter())
                .find(|reference| reference.id == id)
                .map(|reference| reference.label.clone())
                .unwrap_or_else(|| id.to_string())
        };
        // Deletes first: a project may appear only once in an initiative
        // hierarchy, so moving it between related initiatives is rejected while
        // the old link still exists.
        for link in &links {
            let initiative_id = link
                .pointer("/initiative/id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !desired.contains(initiative_id) {
                initiative_changes.push(helpers::InitiativeChange::Remove {
                    link_id: link
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    initiative_id: initiative_id.to_string(),
                    label: link
                        .pointer("/initiative/name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }
        for id in &desired_ids {
            if !current.contains(id.as_str()) {
                initiative_changes.push(helpers::InitiativeChange::Add {
                    initiative_id: id.clone(),
                    label: label_for(id),
                });
            }
        }
    }

    // Everything above is resolution and validation; nothing has been sent yet.
    // An initiative-only update has nothing for projectUpdate.
    let prior_applied = if input.is_empty() {
        None
    } else {
        Some("updated the project's other fields")
    };
    let mut project: Option<(String, String)> = None;
    if !input.is_empty() {
        let result = client.request(
            UPDATE_PROJECT_MUTATION,
            json!({ "id": &resolved_id, "input": Value::Object(input) }),
        )?;
        let project_update = result
            .get("projectUpdate")
            .cloned()
            .ok_or_else(|| CliError::cli("Failed to update project"))?;
        if project_update.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(CliError::cli("Failed to update project"));
        }
        if let Some(updated) = project_update
            .get("project")
            .filter(|value| !value.is_null())
        {
            project = Some((
                updated
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                updated
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ));
        }
    } else {
        project = project_from_links;
    }

    helpers::apply_initiative_changes(&client, &resolved_id, &initiative_changes, prior_applied)?;

    if let Some((name, url)) = project {
        output::line(&format!("✓ Updated project: {name}"));
        if !url.is_empty() {
            output::line(&url);
        }
    }

    Ok(())
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
