//! `linear milestone delete` — port of
//! `src/commands/milestone/milestone-delete.ts`.
//!
//! Milestones have no URL of their own, so a pasted Linear URL is refused
//! before the confirmation prompt. The prompt is guarded by
//! [`crate::prompt::is_interactive`]; non-interactive runs must pass `--force`
//! instead of blocking.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output, prompt};

const DELETE_PROJECT_MILESTONE_MUTATION: &str = r#"
mutation DeleteProjectMilestone($id: String!) {
  projectMilestoneDelete(id: $id) {
    success
  }
}
"#;

#[derive(Args, Debug)]
pub struct DeleteArgs {
    /// Milestone UUID
    #[arg(value_name = "id")]
    pub id: String,
    /// Skip confirmation prompt
    #[arg(short = 'f', long)]
    pub force: bool,
}

pub fn run(args: DeleteArgs) -> Result<()> {
    // Refused before the confirmation prompt, so nobody is asked to confirm
    // deleting a URL.
    crate::linear_url::reject_linear_url(&args.id, "a milestone UUID")?;

    if !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --force to skip confirmation."));
        }
        let confirmed = prompt::confirm(
            &format!("Are you sure you want to delete milestone {}?", args.id),
            false,
        )?;
        if !confirmed {
            output::line("Deletion canceled");
            return Ok(());
        }
    }

    let client = graphql::client()?;
    let result = client
        .request(DELETE_PROJECT_MILESTONE_MUTATION, json!({ "id": args.id }))
        .map_err(|error| linear::missing_milestone(error, &args.id))?;

    let success = result
        .get("projectMilestoneDelete")
        .and_then(|payload| payload.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !success {
        return Err(CliError::cli("Failed to delete milestone"));
    }

    output::line(&format!("✓ Deleted milestone {}", args.id));
    Ok(())
}
