//! `linear issue subscribe` — the API's `issueSubscribe`.
//!
//! Nothing upstream or in Linear's own clients subscribes a watcher through the
//! API, so agents could comment on an issue but never follow it. The mutation's
//! `userId` / `userEmail` are optional and default to the authenticated user,
//! which is what this command subscribes: the account the API key belongs to.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::linear;
use crate::{graphql, output};

use super::issue_label;

const SUBSCRIBE_ISSUE_MUTATION: &str = r#"
mutation SubscribeToIssue($id: String!) {
  issueSubscribe(id: $id) {
    success
    issue {
      identifier
      title
    }
  }
}
"#;

/// Subscribe to an issue
#[derive(Args, Debug)]
pub struct IssueSubscribeArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
}

pub fn run(args: IssueSubscribeArgs) -> Result<()> {
    let Some(issue_id) = linear::get_issue_identifier(args.issue_id.as_deref())? else {
        return Err(CliError::validation("Could not determine issue ID")
            .suggestion("Please provide an issue ID like 'ENG-123'."));
    };

    let client = graphql::client()?;
    let result = client.request(SUBSCRIBE_ISSUE_MUTATION, json!({ "id": issue_id }))?;

    let payload = result.get("issueSubscribe").cloned().unwrap_or(Value::Null);
    if !payload
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CliError::cli("Failed to subscribe to issue"));
    }

    let issue = payload.get("issue").cloned().unwrap_or(Value::Null);
    output::line(&format!(
        "✓ Subscribed to issue: {}",
        issue_label(&issue, &issue_id)
    ));
    Ok(())
}
