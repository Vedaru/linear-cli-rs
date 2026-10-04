use std::collections::HashSet;

use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear};

use super::{
    ADD_PROJECT_TO_INITIATIVE_FOR_UPDATE_MUTATION, GET_INITIATIVE_BY_ID_FOR_UPDATE_QUERY,
    REMOVE_PROJECT_FROM_INITIATIVE_FOR_UPDATE_MUTATION,
};

/// A user-supplied reference resolved to an id, keeping the reference for
/// messages.
#[derive(Clone, Debug)]
pub(super) struct ResolvedRef {
    pub(super) id: String,
    pub(super) label: String,
}

pub(super) struct ConnectionPage {
    nodes: Vec<Value>,
    has_next: bool,
    end_cursor: Option<String>,
}

/// Follow a connection's cursor until every page has been read, deduping by
/// node id.
pub(super) fn fetch_all_pages(
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

pub(super) fn connection_page(data: &Value, pointer: &str) -> ConnectionPage {
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
pub(super) fn apply_collection_edit(
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

pub(super) fn reject_add_remove_overlap(
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

pub(super) fn reject_replace_with_incremental(
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
pub(super) fn resolve_project_labels(names: &[String]) -> Result<Vec<ResolvedRef>> {
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

pub(super) fn resolve_initiatives(
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

pub(super) enum InitiativeChange {
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
pub(super) fn apply_initiative_changes(
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
