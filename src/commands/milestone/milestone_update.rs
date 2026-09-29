//! `linear milestone update` — port of
//! `src/commands/milestone/milestone-update.ts`.

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output};

const UPDATE_PROJECT_MILESTONE_MUTATION: &str = r#"
mutation UpdateProjectMilestone($id: String!, $input: ProjectMilestoneUpdateInput!) {
  projectMilestoneUpdate(id: $id, input: $input) {
    success
    projectMilestone {
      id
      name
      targetDate
      sortOrder
      project {
        id
        name
      }
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Milestone UUID
    #[arg(value_name = "id")]
    pub id: String,
    /// Milestone name
    #[arg(long, value_name = "name")]
    pub name: Option<String>,
    /// Milestone description
    #[arg(long, value_name = "description")]
    pub description: Option<String>,
    /// Target date (YYYY-MM-DD)
    #[arg(long, value_name = "date")]
    pub target_date: Option<String>,
    /// Sort order relative to other milestones
    #[arg(long, value_name = "value")]
    pub sort_order: Option<i64>,
    /// Move to a different project (UUID, slug ID, or name)
    #[arg(long, value_name = "project")]
    pub project: Option<String>,
}

pub fn run(args: UpdateArgs) -> Result<()> {
    crate::linear_url::reject_linear_url(&args.id, "a milestone UUID")?;

    if args.name.is_none()
        && args.description.is_none()
        && args.target_date.is_none()
        && args.sort_order.is_none()
        && args.project.is_none()
    {
        return Err(
            CliError::validation("At least one update option must be provided").suggestion(
                "Use --name, --description, --target-date, --sort-order, or --project",
            ),
        );
    }

    // Only provided fields are sent; `sortOrder` is included even at 0.
    let mut input = Map::new();
    if let Some(name) = &args.name {
        input.insert("name".to_string(), json!(name));
    }
    if let Some(description) = &args.description {
        input.insert("description".to_string(), json!(description));
    }
    if let Some(target_date) = &args.target_date {
        input.insert("targetDate".to_string(), json!(target_date));
    }
    if let Some(sort_order) = args.sort_order {
        input.insert("sortOrder".to_string(), json!(sort_order));
    }
    if let Some(project) = &args.project {
        // Resolve project slug to full UUID.
        input.insert(
            "projectId".to_string(),
            json!(linear::resolve_project_id(project)?),
        );
    }

    let client = graphql::client()?;
    let result = client.request(
        UPDATE_PROJECT_MILESTONE_MUTATION,
        json!({ "id": args.id, "input": Value::Object(input) }),
    )?;

    let payload = result
        .get("projectMilestoneUpdate")
        .cloned()
        .unwrap_or(Value::Null);
    let success = payload
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Err(CliError::cli("Failed to update milestone"));
    }

    let Some(milestone) = payload
        .get("projectMilestone")
        .filter(|value| !value.is_null())
    else {
        return Ok(());
    };

    output::line(&format!(
        "✓ Updated milestone: {}",
        string_field(milestone, "name").unwrap_or("")
    ));
    output::line(&format!("  ID: {}", string_field(milestone, "id").unwrap_or("")));
    if let Some(target_date) = string_field(milestone, "targetDate").filter(|date| !date.is_empty()) {
        output::line(&format!("  Target Date: {target_date}"));
    }
    // `${milestone.sortOrder}` renders a nullable field as "null".
    let sort_order = milestone
        .get("sortOrder")
        .map(Value::to_string)
        .unwrap_or_else(|| "null".to_string());
    output::line(&format!("  Sort Order: {sort_order}"));
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
