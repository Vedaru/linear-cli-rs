//! `linear template` — browse and inspect Linear templates. Port of
//! `src/commands/template/template.ts` and `src/utils/templates.ts`.
//!
//! The group has no action of its own: with no subcommand it prints help,
//! matching upstream's `this.showHelp()`. Each action wraps its failure with
//! the same context string upstream passes to `handleError`.
//!
//! The fetch/resolve/parse helpers live here rather than in `src/linear/` so
//! the port stays within its owned files; they are the direct translation of
//! `utils/templates.ts`.

use clap::{Args, Subcommand};
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output};

pub mod local;
mod template_create;
mod template_delete;
mod template_list;
mod template_show;
mod template_update;
mod template_view;

pub(crate) const GET_TEMPLATES_QUERY: &str = r#"
query GetTemplates {
  templates {
    id
    name
    description
    type
    icon
    color
    hasFormFields
    lastAppliedAt
    sortOrder
    createdAt
    updatedAt
    team {
      id
      key
      name
    }
    inheritedFrom {
      id
      name
    }
    creator {
      id
      name
    }
    templateData
  }
}
"#;

pub(crate) const GET_TEMPLATE_QUERY: &str = r#"
query GetTemplate($id: String!) {
  template(id: $id) {
    id
    name
    description
    type
    icon
    color
    hasFormFields
    lastAppliedAt
    sortOrder
    createdAt
    updatedAt
    team {
      id
      key
      name
    }
    inheritedFrom {
      id
      name
    }
    creator {
      id
      name
    }
    templateData
  }
}
"#;

#[derive(Args, Debug)]
pub struct TemplateArgs {
    #[command(subcommand)]
    pub command: Option<TemplateCommand>,
}

#[derive(Subcommand, Debug)]
pub enum TemplateCommand {
    /// List templates. Without --team, every template in the workspace is shown.
    List(template_list::TemplateListArgs),
    /// Show a template and what it pre-fills. Pass its name or ID.
    #[command(alias = "v")]
    View(template_view::TemplateViewArgs),
    /// Read a template, local or workspace (the unified lookup, naming a shadowed one)
    Show(template_show::TemplateShowArgs),
    /// Write a template: a local file by default, Linear's own with --workspace
    Create(Box<template_create::TemplateCreateArgs>),
    /// Change a template's fields (local by default; --workspace for Linear's)
    Update(template_update::TemplateUpdateArgs),
    /// Delete a template (local by default; --workspace for Linear's)
    Delete(template_delete::TemplateDeleteArgs),
}

pub fn run(args: TemplateArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <TemplateArgs as clap::Args>::augment_args(clap::Command::new("template"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        TemplateCommand::List(args) => {
            template_list::run(args).map_err(|error| error.with_context("Failed to list templates"))
        }
        TemplateCommand::View(args) => {
            template_view::run(args).map_err(|error| error.with_context("Failed to view template"))
        }
        TemplateCommand::Show(args) => {
            template_show::run(args).map_err(|error| error.with_context("Failed to show template"))
        }
        // Boxed: this variant carries every field `issue create` accepts, and the enum is built
        // once per process - but clippy is right that the difference is real, and a pointer costs
        // nothing here.
        TemplateCommand::Create(args) => template_create::run(*args)
            .map_err(|error| error.with_context("Failed to create template")),
        TemplateCommand::Update(args) => template_update::run(args)
            .map_err(|error| error.with_context("Failed to update template")),
        TemplateCommand::Delete(args) => template_delete::run(args)
            .map_err(|error| error.with_context("Failed to delete template")),
    }
}

/// The workspace template with this name, if Linear has one.
///
/// Used only to *report* a shadowed name (`issue create --template`, `template show`), so a
/// workspace with several templates of the same name answers the first one rather than refusing -
/// the local file is the template being applied either way.
pub(crate) fn find_workspace_by_name(name: &str) -> Result<Option<Value>> {
    let wanted = name.to_lowercase();
    Ok(fetch_templates()?
        .into_iter()
        .find(|template| template_name(template).to_lowercase() == wanted))
}

/// A JSON document read from a file (`-` for stdin), for `--data-file`.
pub(crate) fn read_json_file(path: &str) -> Result<Value> {
    let text = if path == "-" {
        use std::io::Read;
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|error| CliError::cli(format!("Failed to read stdin: {error}")))?;
        text
    } else {
        std::fs::read_to_string(path).map_err(|error| {
            CliError::validation(format!("Failed to read {path}"))
                .suggestion(format!("Error: {error}"))
        })?
    };
    serde_json::from_str(&text)
        .map_err(|error| CliError::validation(format!("{path} is not valid JSON: {error}")))
}

/// Every template in the workspace, team-scoped and workspace-level alike.
pub(crate) fn fetch_templates() -> Result<Vec<Value>> {
    let client = graphql::client()?;
    let result = client.request(GET_TEMPLATES_QUERY, json!({}))?;
    Ok(result
        .get("templates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// One template by UUID. `template(id:)` is non-null, so Linear answers a
/// missing UUID with a GraphQL error ("No template found with id ...") rather
/// than a null field; translate that into a NotFound error.
pub(crate) fn fetch_template(id: &str) -> Result<Value> {
    let client = graphql::client()?;
    match client.request(GET_TEMPLATE_QUERY, json!({ "id": id })) {
        Ok(result) => Ok(result.get("template").cloned().unwrap_or(Value::Null)),
        Err(error) => {
            if error
                .user_message
                .to_lowercase()
                .contains("no template found")
            {
                Err(CliError::not_found("Template", id)
                    .suggestion("Run `linear template list` to see every template."))
            } else {
                Err(error)
            }
        }
    }
}

pub(crate) fn template_name(template: &Value) -> String {
    template
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(crate) fn template_id(template: &Value) -> String {
    template
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(crate) fn template_type(template: &Value) -> String {
    template
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// A template qualifies for a scope when it is a workspace template or belongs
/// to one of the given teams.
pub(crate) fn template_is_available_to(template: &Value, team_ids: &[String]) -> bool {
    match template.get("team").filter(|team| !team.is_null()) {
        None => true,
        Some(team) => team
            .get("id")
            .and_then(Value::as_str)
            .map(|id| team_ids.iter().any(|candidate| candidate == id))
            .unwrap_or(false),
    }
}

pub(crate) fn template_scope_label(template: &Value) -> String {
    match template.get("team").filter(|team| !team.is_null()) {
        None => "Workspace".to_string(),
        Some(team) => team
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

/// Resolve a template reference (UUID or exact, case-insensitive name). A
/// reference that matches nothing errors with the names that would have
/// qualified; a reference that matches several errors with their IDs.
pub(crate) fn resolve_template(reference: &str) -> Result<Value> {
    crate::linear_url::reject_linear_url(reference, "a template name or UUID")?;
    if linear::is_linear_uuid(reference) {
        return fetch_template(reference);
    }

    let all = fetch_templates()?;
    let wanted = reference.to_lowercase();
    let by_name: Vec<Value> = all
        .iter()
        .filter(|template| template_name(template).to_lowercase() == wanted)
        .cloned()
        .collect();

    if by_name.len() == 1 {
        return Ok(by_name[0].clone());
    }

    if by_name.is_empty() {
        let mut names: Vec<String> = all.iter().map(template_name).collect();
        names.sort();
        names.dedup();
        let suggestion = if names.is_empty() {
            "No templates are available here. Run `linear template list` to see every template."
                .to_string()
        } else {
            format!(
                "Available templates: {}. Run `linear template list` to see every template.",
                names
                    .iter()
                    .map(|name| format!("\"{name}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        return Err(CliError::not_found("Template", reference).suggestion(suggestion));
    }

    let ids = by_name
        .iter()
        .map(|template| {
            format!(
                "{} ({}, {})",
                template_id(template),
                template_type(template),
                template_scope_label(template)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    Err(CliError::validation(format!(
        "Template name \"{reference}\" is ambiguous: it matches {} templates",
        by_name.len()
    ))
    .suggestion(format!("Pass the template ID instead: {ids}")))
}

/// The pre-filled attributes of a template. Linear returns `templateData` as a
/// JSON-encoded string inside the JSON scalar; accept an already-decoded object
/// too, and reject anything else loudly.
pub(crate) fn parse_template_data(template: &Value) -> Result<Map<String, Value>> {
    let raw = template.get("templateData").cloned().unwrap_or(Value::Null);
    let decoded: Value = match &raw {
        Value::String(text) => serde_json::from_str(text).map_err(|error| {
            CliError::cli(format!(
                "Template data for \"{}\" ({}) is not valid JSON",
                template_name(template),
                template_id(template)
            ))
            .cause(error)
        })?,
        other => other.clone(),
    };

    match decoded {
        Value::Object(map) => Ok(map),
        _ => Err(CliError::cli(format!(
            "Template data for \"{}\" ({}) is not a JSON object",
            template_name(template),
            template_id(template)
        ))),
    }
}
