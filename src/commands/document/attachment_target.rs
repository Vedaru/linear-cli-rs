//! Shared attachment-target resolution for the `linear document` commands —
//! port of `src/commands/document/attachment-target.ts`.
//!
//! A Linear document is attached to exactly one target. The API enforces
//! "exactly one of initiativeId, teamId, issueId, releaseId, cycleId or
//! projectId"; this module owns the CLI side of that rule so create, update,
//! and list can't drift apart.

use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::linear;
use crate::linear_url::{expect_linear_url_kind, LinearUrlRef};
use crate::graphql;

pub const TARGET_FLAGS_SUGGESTION: &str = "Pass exactly one of --project, --issue, --initiative, --team, --cycle, or --release. (--team combined with --cycle scopes the cycle lookup and does not count as a second target.)";

/// Raw `--project`/`--issue`/… option values as parsed from the command line.
#[derive(Debug, Clone, Default)]
pub struct DocumentTargetOptions {
    pub project: Option<String>,
    pub issue: Option<String>,
    pub initiative: Option<String>,
    pub team: Option<String>,
    pub cycle: Option<String>,
    pub release: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentTargetKind {
    Project,
    Issue,
    Initiative,
    Team,
    Cycle,
    Release,
}

impl DocumentTargetKind {
    fn flag(self) -> &'static str {
        match self {
            DocumentTargetKind::Project => "--project",
            DocumentTargetKind::Issue => "--issue",
            DocumentTargetKind::Initiative => "--initiative",
            DocumentTargetKind::Team => "--team",
            DocumentTargetKind::Cycle => "--cycle",
            DocumentTargetKind::Release => "--release",
        }
    }
}

/// A target resolved to its UUID.
#[derive(Debug, Clone)]
pub struct DocumentTarget {
    pub kind: DocumentTargetKind,
    pub id: String,
}

/// A raw selector, before any network resolution.
#[derive(Debug, Clone)]
pub enum DocumentTargetSelector {
    Project(String),
    Issue(String),
    Initiative(String),
    Team(String),
    Cycle { cycle: String, team: Option<String> },
    Release(String),
}

impl DocumentTargetSelector {
    fn kind(&self) -> DocumentTargetKind {
        match self {
            DocumentTargetSelector::Project(_) => DocumentTargetKind::Project,
            DocumentTargetSelector::Issue(_) => DocumentTargetKind::Issue,
            DocumentTargetSelector::Initiative(_) => DocumentTargetKind::Initiative,
            DocumentTargetSelector::Team(_) => DocumentTargetKind::Team,
            DocumentTargetSelector::Cycle { .. } => DocumentTargetKind::Cycle,
            DocumentTargetSelector::Release(_) => DocumentTargetKind::Release,
        }
    }
}

/// Requirement level for [`parse_document_target_options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetRequirement {
    ExactlyOne,
    AtMostOne,
}

/// Turn raw CLI option values into at most one target selector, validating
/// mutual exclusivity before any network work. `--team` together with
/// `--cycle` scopes the cycle lookup (like the issue commands) rather than
/// acting as a second target.
pub fn parse_document_target_options(
    options: &DocumentTargetOptions,
    requirement: TargetRequirement,
) -> Result<Option<DocumentTargetSelector>> {
    let mut selectors: Vec<DocumentTargetSelector> = Vec::new();
    if let Some(project) = &options.project {
        selectors.push(DocumentTargetSelector::Project(project.clone()));
    }
    if let Some(issue) = &options.issue {
        selectors.push(DocumentTargetSelector::Issue(issue.clone()));
    }
    if let Some(initiative) = &options.initiative {
        selectors.push(DocumentTargetSelector::Initiative(initiative.clone()));
    }
    if let Some(cycle) = &options.cycle {
        selectors.push(DocumentTargetSelector::Cycle {
            cycle: cycle.clone(),
            team: options.team.clone(),
        });
    } else if let Some(team) = &options.team {
        selectors.push(DocumentTargetSelector::Team(team.clone()));
    }
    if let Some(release) = &options.release {
        selectors.push(DocumentTargetSelector::Release(release.clone()));
    }

    if selectors.len() > 1 {
        let flags = selectors
            .iter()
            .map(|selector| selector.kind().flag())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(
            CliError::validation(format!("Only one attachment target may be set (got {flags})"))
                .suggestion(TARGET_FLAGS_SUGGESTION),
        );
    }
    if selectors.is_empty() {
        if requirement == TargetRequirement::ExactlyOne {
            return Err(CliError::validation("A document attachment target is required")
                .suggestion(TARGET_FLAGS_SUGGESTION));
        }
        return Ok(None);
    }
    Ok(Some(selectors.remove(0)))
}

const GET_ISSUE_FOR_DOCUMENT_TARGET_QUERY: &str = r#"
query GetIssueForDocumentTarget($id: String!) {
  issue(id: $id) {
    id
  }
}
"#;

fn resolve_issue_id(input: &str) -> Result<String> {
    let client = graphql::client()?;
    // `--issue` on document create/list/update comes through here rather than
    // `getIssueIdentifier`, so a pasted issue URL has to be read here too —
    // otherwise it is uppercased whole and sent to the API as an identifier.
    let url_ref = expect_linear_url_kind(
        input,
        "issue",
        "an issue URL, identifier like ENG-123, or UUID",
    )?;
    let id = match url_ref {
        Some(LinearUrlRef::Issue { identifier, .. }) => identifier,
        _ => {
            if linear::is_linear_uuid(input) {
                input.to_string()
            } else {
                input.to_uppercase()
            }
        }
    };

    let not_found = || {
        CliError::not_found("Issue", input)
            .suggestion("Provide a valid issue identifier (e.g., TC-123) or UUID.")
    };

    match client.request(GET_ISSUE_FOR_DOCUMENT_TARGET_QUERY, json!({ "id": id })) {
        Ok(result) => {
            if let Some(issue) = result.get("issue").filter(|value| !value.is_null()) {
                if let Some(resolved) = issue.get("id").and_then(Value::as_str) {
                    return Ok(resolved.to_string());
                }
            }
            Err(not_found())
        }
        Err(error) => {
            if error.is_not_found() {
                Err(not_found())
            } else {
                Err(error)
            }
        }
    }
}

fn resolve_team_id_strict(team: &str) -> Result<String> {
    Ok(linear::resolve_team(team)?.id)
}

fn resolve_cycle_scope_team_id(explicit_team: Option<&str>) -> Result<String> {
    // An explicitly passed team must resolve or error — never fall back to the
    // configured default team when explicit input is invalid.
    if let Some(explicit) = explicit_team {
        return resolve_team_id_strict(explicit);
    }
    if let Some(config_team) = linear::get_team_key() {
        return resolve_team_id_strict(&config_team);
    }
    Err(
        CliError::validation("--cycle requires a team to look the cycle up in")
            .suggestion("Pass --team <key, name, or ID> or configure a default team."),
    )
}

/// Resolve a parsed selector to the target's UUID.
pub fn resolve_document_target(selector: &DocumentTargetSelector) -> Result<DocumentTarget> {
    match selector {
        DocumentTargetSelector::Project(project) => Ok(DocumentTarget {
            kind: DocumentTargetKind::Project,
            id: linear::resolve_project_id(project)?,
        }),
        DocumentTargetSelector::Issue(issue) => Ok(DocumentTarget {
            kind: DocumentTargetKind::Issue,
            id: resolve_issue_id(issue)?,
        }),
        DocumentTargetSelector::Initiative(initiative) => Ok(DocumentTarget {
            kind: DocumentTargetKind::Initiative,
            id: linear::resolve_initiative_id(initiative)?,
        }),
        DocumentTargetSelector::Team(team) => Ok(DocumentTarget {
            kind: DocumentTargetKind::Team,
            id: resolve_team_id_strict(team)?,
        }),
        DocumentTargetSelector::Cycle { cycle, team } => {
            let team_id = resolve_cycle_scope_team_id(team.as_deref())?;
            Ok(DocumentTarget {
                kind: DocumentTargetKind::Cycle,
                id: linear::get_cycle_id_by_name_or_number(&team_id, cycle)?,
            })
        }
        DocumentTargetSelector::Release(release) => Ok(DocumentTarget {
            kind: DocumentTargetKind::Release,
            id: linear::resolve_release_id(release)?,
        }),
    }
}

/// Map a resolved target to the single DocumentCreateInput/DocumentUpdateInput
/// ID field it sets.
pub fn to_document_target_input(target: &DocumentTarget) -> Value {
    let mut map = Map::new();
    let key = match target.kind {
        DocumentTargetKind::Project => "projectId",
        DocumentTargetKind::Issue => "issueId",
        DocumentTargetKind::Initiative => "initiativeId",
        DocumentTargetKind::Team => "teamId",
        DocumentTargetKind::Cycle => "cycleId",
        DocumentTargetKind::Release => "releaseId",
    };
    map.insert(key.to_string(), json!(target.id));
    Value::Object(map)
}

/// Map a resolved target to the single DocumentFilter relation fragment it
/// filters by.
pub fn to_document_target_filter(target: &DocumentTarget) -> Value {
    let key = match target.kind {
        DocumentTargetKind::Project => "project",
        DocumentTargetKind::Issue => "issue",
        DocumentTargetKind::Initiative => "initiative",
        DocumentTargetKind::Team => "team",
        DocumentTargetKind::Cycle => "cycle",
        DocumentTargetKind::Release => "release",
    };
    json!({ key: { "id": { "eq": target.id } } })
}
