//! `linear issue link` — port of `src/commands/issue/issue-link.ts`.
//!
//! Links a URL to an issue. With one argument the issue is detected from the
//! current branch; with two, the first is the issue and the second the URL.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::linear;
use crate::{graphql, output};

/// Link a URL to an issue
#[derive(Args, Debug)]
pub struct IssueLinkArgs {
    /// URL, or issue ID when a URL is also given
    #[arg(value_name = "urlOrIssueId")]
    pub url_or_issue_id: String,
    /// URL to link
    #[arg(value_name = "url")]
    pub url: Option<String>,
    /// Custom title for the link
    #[arg(short = 't', long, value_name = "title")]
    pub title: Option<String>,
}

const ATTACHMENT_LINK_URL_MUTATION: &str = r#"
mutation AttachmentLinkURL($issueId: String!, $url: String!, $title: String) {
  attachmentLinkURL(issueId: $issueId, url: $url, title: $title) {
    success
    attachment {
      id
      title
      url
    }
  }
}
"#;

pub fn run(args: IssueLinkArgs) -> Result<()> {
    run_inner(args).map_err(|error| error.with_context("Failed to link URL"))
}

fn run_inner(args: IssueLinkArgs) -> Result<()> {
    let (issue_id_input, link_url): (Option<String>, String) = if let Some(url) = args.url.clone() {
        // Two args: first is issue ID, second is URL.
        (Some(args.url_or_issue_id.clone()), url)
    } else if looks_like_url(&args.url_or_issue_id) {
        // One arg that looks like a URL: auto-detect issue from branch.
        (None, args.url_or_issue_id.clone())
    } else {
        return Err(CliError::validation(format!(
            "Expected a URL but got '{}'",
            args.url_or_issue_id
        ))
        .suggestion("Provide a URL starting with http:// or https://."));
    };

    if !looks_like_url(&link_url) {
        return Err(CliError::validation(format!("Invalid URL: '{link_url}'"))
            .suggestion("Provide a URL starting with http:// or https://."));
    }

    let resolved_identifier = linear::get_issue_identifier(issue_id_input.as_deref())?;
    let Some(resolved_identifier) = resolved_identifier else {
        return Err(CliError::validation("Could not determine issue ID").suggestion(
            "Please provide an issue ID like 'ENG-123', or run from a branch that contains an issue identifier.",
        ));
    };

    // attachmentLinkURL needs a UUID.
    let issue_uuid = match linear::get_issue_id(&resolved_identifier) {
        Ok(id) => id,
        Err(error) if error.is_not_found() => {
            return Err(CliError::not_found("Issue", &resolved_identifier));
        }
        Err(error) => return Err(error),
    };
    let Some(issue_uuid) = issue_uuid else {
        return Err(CliError::not_found("Issue", &resolved_identifier));
    };

    let client = graphql::client()?;
    let data = client.request(
        ATTACHMENT_LINK_URL_MUTATION,
        json!({ "issueId": issue_uuid, "url": link_url, "title": args.title }),
    )?;

    let linked = data
        .get("attachmentLinkURL")
        .and_then(|value| value.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !linked {
        return Err(CliError::cli("Failed to link URL to issue"));
    }

    let attachment_title = data
        .get("attachmentLinkURL")
        .and_then(|value| value.get("attachment"))
        .and_then(|value| value.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("");
    output::line(&format!(
        "✓ Linked to {resolved_identifier}: {attachment_title}"
    ));

    Ok(())
}

fn looks_like_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}
