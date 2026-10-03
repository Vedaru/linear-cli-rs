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

/// A user-supplied reference resolved to an id, keeping the reference for
/// messages.
#[derive(Clone, Debug)]
struct ResolvedRef {
    id: String,
    label: String,
}

struct ConnectionPage {
    nodes: Vec<Value>,
    has_next: bool,
    end_cursor: Option<String>,
}

/// Follow a connection's cursor until every page has been read, deduping by
/// node id.
fn fetch_all_pages(
    mut fetch_page: impl FnMut(Option<&str>) -> Result<ConnectionPage>,
) -> Result<Vec<Value>> {
    let mut nodes: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut after: Option<String> = None;
    loop {
        let page = fetch_page(after.as_deref())?;
        for node in page.nodes {
            let id = node
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if seen.insert(id) {
                nodes.push(node);
            }
        }
        if !page.has_next {
            return Ok(nodes);
        }
        let Some(end_cursor) = page.end_cursor else {
            return Err(CliError::cli(
                "Linear reported another page of results but returned no cursor to fetch it",
            ));
        };
        if Some(&end_cursor) == after.as_ref() {
            return Err(CliError::cli(
                "Linear reported another page of results but returned the same cursor again",
            ));
        }
        after = Some(end_cursor);
    }
}

fn connection_page(data: &Value, pointer: &str) -> ConnectionPage {
    let connection = data.pointer(pointer);
    let nodes = connection
        .and_then(|value| value.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let has_next = connection
        .and_then(|value| value.pointer("/pageInfo/hasNextPage"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let end_cursor = connection
        .and_then(|value| value.pointer("/pageInfo/endCursor"))
        .and_then(Value::as_str)
        .map(str::to_string);
    ConnectionPage {
        nodes,
        has_next,
        end_cursor,
    }
}

/// Apply `--add`/`--remove` to a collection: current order is kept, removed ids
/// are dropped, added ids not already present are appended in flag order. A
/// removal that is not in the current set errors before anything is sent.
fn apply_collection_edit(
    current: &[String],
    add: &[ResolvedRef],
    remove: &[ResolvedRef],
    on_missing: impl Fn(&ResolvedRef) -> CliError,
) -> Result<Vec<String>> {
    let current_set: HashSet<&str> = current.iter().map(String::as_str).collect();
    for reference in remove {
        if !current_set.contains(reference.id.as_str()) {
            return Err(on_missing(reference));
        }
    }
    let remove_ids: HashSet<&str> = remove
        .iter()
        .map(|reference| reference.id.as_str())
        .collect();
    let mut result: Vec<String> = current
        .iter()
        .filter(|id| !remove_ids.contains(id.as_str()))
        .cloned()
        .collect();
    for reference in add {
        if !result.iter().any(|id| id == &reference.id) {
            result.push(reference.id.clone());
        }
    }
    Ok(result)
}

fn reject_add_remove_overlap(
    kind: &str,
    add: &[ResolvedRef],
    remove: &[ResolvedRef],
) -> Result<()> {
    let remove_ids: HashSet<&str> = remove
        .iter()
        .map(|reference| reference.id.as_str())
        .collect();
    if add
        .iter()
        .any(|reference| remove_ids.contains(reference.id.as_str()))
    {
        return Err(CliError::validation(format!(
            "Cannot add and remove the same {kind} in one update"
        ))
        .suggestion(format!(
            "Remove the duplicate {kind} from either --add-{kind} or --remove-{kind}."
        )));
    }
    Ok(())
}

fn reject_replace_with_incremental(
    kind: &str,
    replace: bool,
    add: bool,
    remove: bool,
) -> Result<()> {
    if replace && (add || remove) {
        return Err(CliError::validation(format!(
            "Cannot combine --{kind} with --add-{kind} or --remove-{kind}"
        ))
        .suggestion(format!(
            "--{kind} replaces the project's entire {kind} set. Use it alone to set the exact set, or use --add-{kind}/--remove-{kind} alone to change it incrementally."
        )));
    }
    Ok(())
}

/// Resolve project label names to ids, deduped by id, erroring on unknown names.
fn resolve_project_labels(names: &[String]) -> Result<Vec<ResolvedRef>> {
    let mut resolved: Vec<ResolvedRef> = Vec::new();
    for name in names {
        let Some(id) = linear::get_project_label_id_by_name(name)? else {
            return Err(CliError::not_found("Project label", name));
        };
        if !resolved.iter().any(|reference| reference.id == id) {
            resolved.push(ResolvedRef {
                id,
                label: name.clone(),
            });
        }
    }
    Ok(resolved)
}

fn resolve_initiatives(
    client: &graphql::Client,
    references: &[String],
) -> Result<Vec<ResolvedRef>> {
    let mut resolved: Vec<ResolvedRef> = Vec::new();
    for reference in references {
        let resolved_ref = if linear::is_linear_uuid(reference) {
            let data = client.request(
                GET_INITIATIVE_BY_ID_FOR_UPDATE_QUERY,
                json!({ "id": reference }),
            )?;
            let initiative = data
                .pointer("/initiatives/nodes")
                .and_then(Value::as_array)
                .and_then(|nodes| nodes.first());
            let Some(initiative) = initiative else {
                return Err(CliError::not_found("Initiative", reference)
                    .suggestion("Pass an initiative UUID, slug ID, or exact initiative name."));
            };
            ResolvedRef {
                id: initiative
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                label: initiative
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            }
        } else {
            let id = linear::resolve_initiative_id(reference)?;
            ResolvedRef {
                id,
                label: reference.clone(),
            }
        };
        if !resolved
            .iter()
            .any(|existing| existing.id == resolved_ref.id)
        {
            resolved.push(resolved_ref);
        }
    }
    Ok(resolved)
}

enum InitiativeChange {
    Add {
        initiative_id: String,
        label: String,
    },
    Remove {
        link_id: String,
        initiative_id: String,
        label: String,
    },
}

fn describe_initiative_change(change: &InitiativeChange) -> String {
    match change {
        InitiativeChange::Add { label, .. } => format!("added \"{label}\""),
        InitiativeChange::Remove { label, .. } => format!("removed \"{label}\""),
    }
}

/// By UUID: initiative names are not unique and the resolver rejects an
/// ambiguous name, so a name here could make the suggested command unrunnable.
fn initiative_change_flag(change: &InitiativeChange) -> String {
    match change {
        InitiativeChange::Add { initiative_id, .. } => {
            format!("--add-initiative {initiative_id}")
        }
        InitiativeChange::Remove { initiative_id, .. } => {
            format!("--remove-initiative {initiative_id}")
        }
    }
}

enum InitiativeOutcome {
    Rejected,
    Unknown,
}

/// Apply initiative link changes one mutation at a time. Linear has no
/// transaction across join-row mutations, so a failure part-way leaves earlier
/// changes applied; the error says exactly which, and which are still pending.
fn apply_initiative_changes(
    client: &graphql::Client,
    project_id: &str,
    changes: &[InitiativeChange],
    prior_applied: Option<&str>,
) -> Result<()> {
    let fail = |applied: usize, outcome: InitiativeOutcome, cause: CliError| -> CliError {
        let mut done: Vec<String> = Vec::new();
        if let Some(prior) = prior_applied {
            done.push(prior.to_string());
        }
        done.extend(changes.iter().take(applied).map(describe_initiative_change));
        let current = &changes[applied];
        let rest = &changes[applied + 1..];
        let unknown = matches!(outcome, InitiativeOutcome::Unknown);
        let unknown_text = if unknown {
            format!(
                " Unknown (the request failed before Linear answered): {}.",
                describe_initiative_change(current)
            )
        } else {
            String::new()
        };
        let not_applied: Vec<&InitiativeChange> = if unknown {
            rest.iter().collect()
        } else {
            std::iter::once(current).chain(rest.iter()).collect()
        };
        let not_applied_text = if not_applied.is_empty() {
            String::new()
        } else {
            format!(
                " Not applied: {}.",
                not_applied
                    .iter()
                    .map(|change| describe_initiative_change(change))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let remaining = std::iter::once(current)
            .chain(rest.iter())
            .map(initiative_change_flag)
            .collect::<Vec<_>>()
            .join(" ");
        let applied_text = if done.is_empty() {
            "none".to_string()
        } else {
            done.join(", ")
        };
        let message = format!(
            "Failed to update project initiatives after {applied} of {total} changes; earlier changes were not rolled back. Applied: {applied_text}.{unknown_text}{not_applied_text}",
            total = changes.len()
        );
        let suggestion = if unknown {
            format!(
                "Check the project's initiatives, then re-run with only the remaining changes ({remaining}), or use --initiative to set the exact set."
            )
        } else {
            format!(
                "Re-run with only the remaining changes ({remaining}), or use --initiative to set the exact set."
            )
        };
        CliError::cli(message).suggestion(suggestion).cause(cause)
    };

    for (applied, change) in changes.iter().enumerate() {
        let success = match change {
            InitiativeChange::Add { initiative_id, .. } => {
                let result = client.request(
                    ADD_PROJECT_TO_INITIATIVE_FOR_UPDATE_MUTATION,
                    json!({ "input": { "initiativeId": initiative_id, "projectId": project_id } }),
                );
                match result {
                    Ok(data) => data
                        .pointer("/initiativeToProjectCreate/success")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    Err(error) => return Err(fail(applied, InitiativeOutcome::Unknown, error)),
                }
            }
            InitiativeChange::Remove { link_id, .. } => {
                let result = client.request(
                    REMOVE_PROJECT_FROM_INITIATIVE_FOR_UPDATE_MUTATION,
                    json!({ "id": link_id }),
                );
                match result {
                    Ok(data) => data
                        .pointer("/initiativeToProjectDelete/success")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    Err(error) => return Err(fail(applied, InitiativeOutcome::Unknown, error)),
                }
            }
        };
        if !success {
            let label = match change {
                InitiativeChange::Add { label, .. } | InitiativeChange::Remove { label, .. } => {
                    label.clone()
                }
            };
            return Err(fail(
                applied,
                InitiativeOutcome::Rejected,
                CliError::cli(format!(
                    "Linear reported failure for initiative \"{label}\""
                )),
            ));
        }
    }
    Ok(())
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

    reject_replace_with_incremental("team", replace_team, add_team, remove_team)?;
    reject_replace_with_incremental("label", replace_label, add_label, remove_label)?;
    reject_replace_with_incremental(
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
        let added: Vec<ResolvedRef> = linear::resolve_teams(&args.add_team)?
            .into_iter()
            .map(|team| ResolvedRef {
                id: team.id,
                label: team.key,
            })
            .collect();
        let removed: Vec<ResolvedRef> = linear::resolve_teams(&args.remove_team)?
            .into_iter()
            .map(|team| ResolvedRef {
                id: team.id,
                label: team.key,
            })
            .collect();
        reject_add_remove_overlap("team", &added, &removed)?;
        let current = fetch_all_pages(|after| {
            let data = client.request(
                GET_PROJECT_TEAMS_FOR_UPDATE_QUERY,
                json!({ "id": &resolved_id, "after": after }),
            )?;
            Ok(connection_page(&data, "/project/teams"))
        })?;
        let current_ids: Vec<String> = current
            .iter()
            .filter_map(|team| team.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        let team_ids = apply_collection_edit(&current_ids, &added, &removed, |reference| {
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
        let ids: Vec<String> = resolve_project_labels(&args.label)?
            .into_iter()
            .map(|reference| reference.id)
            .collect();
        input.insert("labelIds".to_string(), json!(ids));
    } else if add_label || remove_label {
        let added = resolve_project_labels(&args.add_label)?;
        let removed = resolve_project_labels(&args.remove_label)?;
        reject_add_remove_overlap("label", &added, &removed)?;
        let current = fetch_all_pages(|after| {
            let data = client.request(
                GET_PROJECT_LABELS_FOR_UPDATE_QUERY,
                json!({ "id": &resolved_id, "after": after }),
            )?;
            Ok(connection_page(&data, "/project/labels"))
        })?;
        let current_ids: Vec<String> = current
            .iter()
            .filter_map(|label| label.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        let label_ids = apply_collection_edit(&current_ids, &added, &removed, |reference| {
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

    let mut initiative_changes: Vec<InitiativeChange> = Vec::new();
    let mut project_from_links: Option<(String, String)> = None;
    if replace_initiative || add_initiative || remove_initiative {
        let replacement = if replace_initiative {
            Some(resolve_initiatives(&client, &args.initiative)?)
        } else {
            None
        };
        let added = resolve_initiatives(&client, &args.add_initiative)?;
        let removed = resolve_initiatives(&client, &args.remove_initiative)?;
        reject_add_remove_overlap("initiative", &added, &removed)?;

        let links = fetch_all_pages(|after| {
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
            Ok(connection_page(&data, "/project/initiativeToProjects"))
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
                apply_collection_edit(&current_ids, &added, &removed, |reference| {
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
                initiative_changes.push(InitiativeChange::Remove {
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
                initiative_changes.push(InitiativeChange::Add {
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

    apply_initiative_changes(&client, &resolved_id, &initiative_changes, prior_applied)?;

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
