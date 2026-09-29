//! `linear issue unsubscribe` — the API's `issueUnsubscribe`, the inverse of
//! [`super::issue_subscribe`]: it drops the authenticated user from the issue's
//! watchers (the mutation's `userId` / `userEmail` are optional and default to
//! that same user).

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::linear;
use crate::{graphql, output};

use super::issue_label;

const UNSUBSCRIBE_ISSUE_MUTATION: &str = r#"
mutation UnsubscribeFromIssue($id: String!) {
  issueUnsubscribe(id: $id) {
    success
    issue {
      identifier
      title
    }
  }
}
"#;

/// Unsubscribe from an issue
#[derive(Args, Debug)]
pub struct IssueUnsubscribeArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
}

pub fn run(args: IssueUnsubscribeArgs) -> Result<()> {
    let Some(issue_id) = linear::get_issue_identifier(args.issue_id.as_deref())? else {
        return Err(CliError::validation("Could not determine issue ID")
            .suggestion("Please provide an issue ID like 'ENG-123'."));
    };

    let client = graphql::client()?;
    let result = client.request(UNSUBSCRIBE_ISSUE_MUTATION, json!({ "id": issue_id }))?;

    let payload = result
        .get("issueUnsubscribe")
        .cloned()
        .unwrap_or(Value::Null);
    if !payload
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CliError::cli("Failed to unsubscribe from issue"));
    }

    let issue = payload.get("issue").cloned().unwrap_or(Value::Null);
    output::line(&format!(
        "✓ Unsubscribed from issue: {}",
        issue_label(&issue, &issue_id)
    ));
    Ok(())
}
