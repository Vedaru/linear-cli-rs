//! `linear milestone create` — port of
//! `src/commands/milestone/milestone-create.ts`.

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output};

const CREATE_PROJECT_MILESTONE_MUTATION: &str = r#"
mutation CreateProjectMilestone($input: ProjectMilestoneCreateInput!) {
  projectMilestoneCreate(input: $input) {
    success
    projectMilestone {
      id
      name
      targetDate
      project {
        id
        name
      }
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct CreateArgs {
    /// Project (UUID, slug ID, or name)
    #[arg(long, value_name = "project")]
    pub project: String,
    /// Milestone name
    #[arg(long, value_name = "name")]
    pub name: String,
    /// Milestone description
    #[arg(long, value_name = "description")]
    pub description: Option<String>,
    /// Target date (YYYY-MM-DD)
    #[arg(long, value_name = "date")]
    pub target_date: Option<String>,
}

pub fn run(args: CreateArgs) -> Result<()> {
    // Resolve project slug to full UUID.
    let project_id = linear::resolve_project_id(&args.project)?;

    // Upstream always passes `description`/`targetDate`, but JSON.stringify
    // drops `undefined`, so only provided options reach the API.
    let mut input = Map::new();
    input.insert("projectId".to_string(), json!(project_id));
    input.insert("name".to_string(), json!(args.name));
    if let Some(description) = &args.description {
        input.insert("description".to_string(), json!(description));
    }
    if let Some(target_date) = &args.target_date {
        input.insert("targetDate".to_string(), json!(target_date));
    }

    let client = graphql::client()?;
    let result = client.request(
        CREATE_PROJECT_MILESTONE_MUTATION,
        json!({ "input": Value::Object(input) }),
    )?;

    let payload = result
        .get("projectMilestoneCreate")
        .cloned()
        .unwrap_or(Value::Null);
    let success = payload
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Err(CliError::cli("Failed to create milestone"));
    }

    let Some(milestone) = payload
        .get("projectMilestone")
        .filter(|value| !value.is_null())
    else {
        return Ok(());
    };

    output::line(&format!(
        "✓ Created milestone: {}",
        string_field(milestone, "name").unwrap_or("")
    ));
    output::line(&format!("  ID: {}", string_field(milestone, "id").unwrap_or("")));
    if let Some(target_date) = string_field(milestone, "targetDate").filter(|date| !date.is_empty()) {
        output::line(&format!("  Target Date: {target_date}"));
    }
    output::line(&format!(
        "  Project: {}",
        milestone
            .get("project")
            .and_then(|project| project.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
    ));

    Ok(())
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
