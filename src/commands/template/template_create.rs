//! `linear template create` — write a template, locally by default.
//!
//! The local form is the point of this command for an agent: a file of the same fields
//! `issue create` takes as flags, so `--template bug` is a shorthand for a set of defaults rather
//! than a server round trip. `--workspace` writes Linear's own template instead, which needs the
//! API's `templateData` document - a ProseMirror-shaped JSON - so it is passed through from a file
//! rather than assembled from flags this command would have to invent.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output};

use super::local;

const CREATE_TEMPLATE_MUTATION: &str = r#"
mutation CreateWorkspaceTemplate($input: TemplateCreateInput!) {
  templateCreate(input: $input) {
    success
    template {
      id
      name
      type
      team {
        key
      }
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct TemplateCreateArgs {
    /// Template name (also the file name, for a local one)
    pub name: String,
    /// Issue title to pre-fill
    #[arg(short = 't', long, value_name = "title")]
    pub title: Option<String>,
    /// Issue description to pre-fill
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// Read the description from a file
    #[arg(long = "description-file", value_name = "path")]
    pub description_file: Option<String>,
    /// Team key, name, or ID
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Project to pre-fill (ID, slug, or name)
    #[arg(long, value_name = "project")]
    pub project: Option<String>,
    /// Workflow state to pre-fill
    #[arg(long, value_name = "state")]
    pub state: Option<String>,
    /// Assignee to pre-fill
    #[arg(short = 'a', long, value_name = "assignee")]
    pub assignee: Option<String>,
    /// Priority to pre-fill (1-4)
    #[arg(short = 'p', long, value_name = "priority")]
    pub priority: Option<i64>,
    /// Estimate to pre-fill
    #[arg(short = 'e', long, value_name = "estimate")]
    pub estimate: Option<i64>,
    /// Label to pre-fill (repeatable)
    #[arg(short = 'l', long, value_name = "label")]
    pub label: Vec<String>,
    /// Cycle to pre-fill (number, name, or `active`)
    #[arg(long, value_name = "cycle")]
    pub cycle: Option<String>,
    /// Project milestone to pre-fill
    #[arg(long, value_name = "milestone")]
    pub milestone: Option<String>,
    /// Parent issue to pre-fill (e.g. ENG-12)
    #[arg(long, value_name = "parent")]
    pub parent: Option<String>,
    /// Due date to pre-fill (YYYY-MM-DD)
    #[arg(long = "due-date", value_name = "date")]
    pub due_date: Option<String>,
    /// Create a workspace template instead of a local one
    #[arg(long)]
    pub workspace: bool,
    /// The API's `templateData` document, read from a file (`--workspace` only)
    #[arg(long = "data-file", value_name = "path")]
    pub data_file: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: TemplateCreateArgs) -> Result<()> {
    if args.workspace {
        return create_workspace(&args);
    }
    create_local(&args)
}

fn create_local(args: &TemplateCreateArgs) -> Result<()> {
    if args.description.is_some() && args.description_file.is_some() {
        return Err(CliError::validation(
            "Cannot specify both --description and --description-file",
        ));
    }
    let description = match (&args.description, &args.description_file) {
        (Some(text), _) => Some(text.clone()),
        (None, Some(path)) => Some(std::fs::read_to_string(path).map_err(|error| {
            CliError::validation(format!("Failed to read description file: {path}"))
                .suggestion(format!("Error: {error}"))
        })?),
        (None, None) => None,
    };

    let fields = local::fields_from_flags(vec![
        ("title", json!(args.title)),
        ("description", json!(description)),
        ("team", json!(args.team)),
        ("project", json!(args.project)),
        ("state", json!(args.state)),
        ("assignee", json!(args.assignee)),
        (
            "priority",
            args.priority
                .map(|value| json!(value))
                .unwrap_or(Value::Null),
        ),
        (
            "estimate",
            args.estimate
                .map(|value| json!(value))
                .unwrap_or(Value::Null),
        ),
        (
            "labels",
            if args.label.is_empty() {
                Value::Null
            } else {
                json!(args.label)
            },
        ),
        ("cycle", json!(args.cycle)),
        ("milestone", json!(args.milestone)),
        ("parent", json!(args.parent)),
        ("due_date", json!(args.due_date)),
    ]);

    if fields.is_empty() {
        return Err(CliError::validation("Nothing to put in the template").suggestion(
            "Pass at least one field (--title, --label, --priority, ...); a template with no fields would pre-fill nothing.",
        ));
    }

    let path = local::write(&args.name, &fields)?;
    if args.json {
        output::print_json(&json!({
            "name": args.name,
            "kind": "local",
            "path": path.display().to_string(),
            "fields": Value::Object(fields),
        }));
        return Ok(());
    }

    let names: Vec<String> = fields.keys().cloned().collect();
    output::line(&format!("✓ Created local template {}", args.name));
    output::line(&format!("  {} ({})", path.display(), names.join(", ")));
    Ok(())
}

fn create_workspace(args: &TemplateCreateArgs) -> Result<()> {
    let Some(data_file) = args.data_file.as_deref() else {
        return Err(CliError::validation(
            "--workspace needs --data-file: the API's templateData document",
        )
        .suggestion(
            "A workspace template is a ProseMirror-shaped JSON document, not a set of flags - export one from Linear or write it by hand and pass it here.",
        ));
    };
    let data = super::read_json_file(data_file)?;
    let team_reference = match args.team.as_deref() {
        Some(team) => team.to_string(),
        None => linear::get_team_key()?.ok_or_else(|| {
            CliError::validation("--workspace needs --team (or a configured team)")
        })?,
    };
    let team = linear::resolve_team(&team_reference)?;

    let client = graphql::client()?;
    let document = client.request(
        CREATE_TEMPLATE_MUTATION,
        json!({ "input": {
            "name": args.name,
            "type": "issue",
            "teamId": team.id,
            "description": args.description,
            "templateData": data,
        } }),
    )?;
    let created = document
        .get("templateCreate")
        .ok_or_else(|| CliError::cli("Linear API response did not contain templateCreate"))?;
    if created.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli("Failed to create the workspace template"));
    }

    if args.json {
        output::print_json(&document);
        return Ok(());
    }
    let name = created
        .pointer("/template/name")
        .and_then(Value::as_str)
        .unwrap_or(&args.name);
    output::line(&format!(
        "✓ Created workspace template {name} in team {}",
        team.key
    ));
    Ok(())
}
