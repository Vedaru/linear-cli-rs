//! `linear issue comment` — port of `src/commands/issue/issue-comment*.ts`.
//!
//! `add`, `list`, `update`, and `delete`. The group itself has no action; with
//! no subcommand it prints help, matching upstream's `this.showHelp()`. The
//! shared body/rendering helpers live in [`crate::comments`].

use clap::{Args, Subcommand};
use regex::Regex;
use serde_json::{json, Value};

use crate::comments::{
    self, CommentTarget, CreateCommentOptions, COMMENT_BODY_DESCRIPTION,
    COMMENT_BODY_FILE_DESCRIPTION, COMMENT_LIST_FIELDS, REPLY_TO_DESCRIPTION,
};
use crate::errors::{self, CliError, Result};
use crate::{graphql, linear, output, prompt, upload};

#[derive(Args, Debug)]
pub struct IssueCommentArgs {
    #[command(subcommand)]
    pub command: Option<CommentCommand>,
}

#[derive(Subcommand, Debug)]
pub enum CommentCommand {
    /// Add a comment to an issue
    Add(CommentAddArgs),
    /// Delete a comment
    Delete(CommentDeleteArgs),
    /// Update an existing comment
    Update(CommentUpdateArgs),
    /// List comments for an issue
    List(CommentListArgs),
}

#[derive(Args, Debug)]
pub struct CommentAddArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
    #[arg(short = 'b', long, value_name = "text", help = COMMENT_BODY_DESCRIPTION)]
    pub body: Option<String>,
    #[arg(long, value_name = "path", help = COMMENT_BODY_FILE_DESCRIPTION)]
    pub body_file: Option<String>,
    #[arg(
        short = 'p',
        long,
        visible_alias = "reply-to",
        value_name = "commentId",
        help = REPLY_TO_DESCRIPTION
    )]
    pub parent: Option<String>,
    /// Caller-supplied UUID for the new comment
    #[arg(long, value_name = "uuid", hide = true)]
    pub id: Option<String>,
    /// Upload a file and add its Markdown link to the comment (images render
    /// inline; repeatable)
    #[arg(short = 'a', long, value_name = "filepath")]
    pub attach: Vec<String>,
    /// Upload attached images to a public, unauthenticated URL (default:
    /// private, workspace-members only)
    #[arg(long)]
    pub public: bool,
}

#[derive(Args, Debug)]
pub struct CommentDeleteArgs {
    /// Comment ID
    #[arg(value_name = "commentId")]
    pub comment_id: String,
}

#[derive(Args, Debug)]
pub struct CommentUpdateArgs {
    /// Comment ID
    #[arg(value_name = "commentId")]
    pub comment_id: String,
    /// New comment body text
    #[arg(short = 'b', long, value_name = "text")]
    pub body: Option<String>,
    /// Read comment body from a file (preferred for markdown content)
    #[arg(long, value_name = "path")]
    pub body_file: Option<String>,
}

#[derive(Args, Debug)]
pub struct CommentListArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: IssueCommentArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <IssueCommentArgs as clap::Args>::augment_args(clap::Command::new("comment"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        CommentCommand::Add(a) => {
            add_comment(a).map_err(|error| error.with_context("Failed to add comment"))
        }
        CommentCommand::Delete(a) => {
            delete_comment(a).map_err(|error| error.with_context("Failed to delete comment"))
        }
        CommentCommand::Update(a) => {
            update_comment(a).map_err(|error| error.with_context("Failed to update comment"))
        }
        CommentCommand::List(a) => {
            list_comments(a).map_err(|error| error.with_context("Failed to list comments"))
        }
    }
}

/// Linear documents CommentCreateInput.id as "The identifier in UUID v4
/// format".
fn is_uuid_v4(value: &str) -> bool {
    static UUID_V4: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let regex = UUID_V4.get_or_init(|| {
        Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
            .expect("uuid v4 regex")
    });
    regex.is_match(value)
}

// ---------------------------------------------------------------------------
// add
// ---------------------------------------------------------------------------

fn add_comment(args: CommentAddArgs) -> Result<()> {
    // Reject a malformed --id here rather than letting the API reject it, so
    // the user gets an actionable message instead of a raw GraphQL error.
    if let Some(id) = &args.id {
        if !is_uuid_v4(id) {
            return Err(CliError::validation(format!("Invalid comment ID: {id}"))
                .suggestion("--id must be a v4 UUID, like 123e4567-e89b-42d3-a456-426614174000."));
        }
    }

    let text_body =
        comments::resolve_comment_body(args.body.as_deref(), args.body_file.as_deref())?;

    let Some(resolved_identifier) = linear::get_issue_identifier(args.issue_id.as_deref())? else {
        return Err(CliError::validation("Could not determine issue ID")
            .suggestion("Please provide an issue ID like 'ENG-123'."));
    };

    // Validate and upload attachments first.
    if args.public && args.attach.is_empty() {
        return Err(
            CliError::validation("--public requires at least one --attach")
                .suggestion("Add --attach <file> to upload, or remove --public."),
        );
    }

    let mut attachment_links: Vec<String> = Vec::new();
    if !args.attach.is_empty() {
        // Validate all files exist and, if --public, that every file may be
        // uploaded publicly -- before uploading any, so a mixed batch cannot
        // publish some files before failing on an unsupported one.
        for filepath in &args.attach {
            upload::validate_file_path(filepath)?;
            upload::resolve_make_public(&upload::get_mime_type(filepath), Some(args.public))?;
        }

        for filepath in &args.attach {
            let result = upload::upload_file(
                filepath,
                &upload::UploadOptions {
                    make_public: Some(args.public),
                },
            )?;
            output::line(&format!("✓ Uploaded {}", result.filename));
            if result.public {
                eprintln!(
                    "⚠ Uploaded to a public URL readable by anyone: {}",
                    result.asset_url
                );
            }
            attachment_links.push(upload::format_as_markdown_link(&result));
        }
    }

    // Attachment links alone are a valid body; otherwise prompt for text.
    let prompted_body = if text_body.is_none() && attachment_links.is_empty() {
        Some(comments::prompt_comment_body()?)
    } else {
        text_body
    };

    let comment_body = [prompted_body, Some(attachment_links.join("\n"))]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    let (_id, url) = comments::create_comment(
        &CommentTarget::Issue {
            issue_id: resolved_identifier.clone(),
        },
        &CreateCommentOptions {
            body: comment_body,
            parent_id: args.parent.clone(),
            id: args.id.clone(),
        },
    )?;

    output::line(&format!("✓ Comment added to {resolved_identifier}"));
    output::line(&url);
    Ok(())
}

// ---------------------------------------------------------------------------
// delete
// ---------------------------------------------------------------------------

fn delete_comment(args: CommentDeleteArgs) -> Result<()> {
    crate::linear_url::reject_comment_url(&args.comment_id)?;
    crate::linear_url::reject_linear_url(&args.comment_id, "a comment UUID")?;

    const DELETE_COMMENT_MUTATION: &str = r#"
mutation DeleteComment($id: String!) {
  commentDelete(id: $id) {
    success
  }
}
"#;

    let client = graphql::client()?;
    let data = client.request(DELETE_COMMENT_MUTATION, json!({ "id": args.comment_id }))?;

    let deleted = data
        .get("commentDelete")
        .and_then(|value| value.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !deleted {
        return Err(CliError::cli("Failed to delete comment"));
    }

    output::line("✓ Comment deleted");
    Ok(())
}

// ---------------------------------------------------------------------------
// update
// ---------------------------------------------------------------------------

fn update_comment(args: CommentUpdateArgs) -> Result<()> {
    crate::linear_url::reject_comment_url(&args.comment_id)?;
    crate::linear_url::reject_linear_url(&args.comment_id, "a comment UUID")?;

    // Upstream's `update` handles its own body/flags rather than the shared
    // resolver: a blank body falls through to the prompt instead of erroring.
    if args.body.is_some() && args.body_file.is_some() {
        return Err(CliError::validation(
            "Cannot specify both --body and --body-file",
        ));
    }

    let mut new_body = args.body.clone().unwrap_or_default();
    if let Some(path) = &args.body_file {
        new_body = std::fs::read_to_string(path).map_err(|error| {
            CliError::validation(format!("Failed to read body file: {path}"))
                .suggestion(format!("Error: {error}"))
        })?;
    }

    const GET_COMMENT_QUERY: &str = r#"
query GetComment($id: String!) {
  comment(id: $id) {
    body
  }
}
"#;
    const UPDATE_COMMENT_MUTATION: &str = r#"
mutation UpdateComment($id: String!, $input: CommentUpdateInput!) {
  commentUpdate(id: $id, input: $input) {
    success
    comment {
      id
      body
      updatedAt
      url
      user {
        name
        displayName
      }
    }
  }
}
"#;

    if new_body.is_empty() {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Comment body cannot be empty")
                .suggestion("Pass the new body with --body or --body-file."));
        }

        let client = graphql::client()?;
        let comment_data = client.request(GET_COMMENT_QUERY, json!({ "id": args.comment_id }))?;
        let existing_body = comment_data
            .get("comment")
            .and_then(|comment| comment.get("body"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        new_body = prompt_text_with_default("New comment body", &existing_body)?;
        if new_body.trim().is_empty() {
            return Err(CliError::validation("Comment body cannot be empty"));
        }
    }

    let client = graphql::client()?;
    let data = client.request(
        UPDATE_COMMENT_MUTATION,
        json!({ "id": args.comment_id, "input": { "body": new_body } }),
    )?;

    let updated = data
        .get("commentUpdate")
        .and_then(|value| value.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !updated {
        return Err(CliError::cli("Failed to update comment"));
    }

    let url = data
        .get("commentUpdate")
        .and_then(|value| value.get("comment"))
        .filter(|value| !value.is_null())
        .and_then(|comment| comment.get("url"))
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::cli("Comment update failed - no comment returned"))?;

    output::line("✓ Comment updated");
    output::line(url);
    Ok(())
}

/// Prompt for a line of text with an editable default, mirroring
/// `Input.prompt({ message, default })`. Only called after
/// [`prompt::is_interactive`] has confirmed stdin is a terminal.
fn prompt_text_with_default(message: &str, default: &str) -> Result<String> {
    use std::io::BufRead;

    eprint!("{message} [{default}] ");
    let _ = std::io::Write::flush(&mut std::io::stderr());

    let mut line = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
    if read == 0 {
        return Ok(default.to_string());
    }
    let value = line.trim_end_matches(['\r', '\n']).to_string();
    if value.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(value)
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn list_comments(args: CommentListArgs) -> Result<()> {
    let Some(resolved_identifier) = linear::get_issue_identifier(args.issue_id.as_deref())? else {
        return Err(CliError::validation("Could not determine issue ID")
            .suggestion("Please provide an issue ID like 'ENG-123'."));
    };

    let query = format!(
        r#"
query GetIssueComments($id: String!, $after: String) {{
  issue(id: $id) {{
    comments(first: 50, after: $after, orderBy: createdAt) {{
      nodes {{
        {COMMENT_LIST_FIELDS}
      }}
      pageInfo {{
        hasNextPage
        endCursor
      }}
    }}
  }}
}}
"#
    );

    let client = graphql::client()?;
    let collected = comments::collect_comment_pages(|after| {
        let data = errors::translate_not_found("Issue", &resolved_identifier, || {
            client.request(&query, json!({ "id": resolved_identifier, "after": after }))
        })?;
        let issue = data
            .get("issue")
            .filter(|value| !value.is_null())
            .ok_or_else(|| CliError::not_found("Issue", &resolved_identifier))?;

        let connection = issue.get("comments");
        let nodes = connection
            .and_then(|value| value.get("nodes"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let page_info = connection
            .and_then(|value| value.get("pageInfo"))
            .cloned()
            .unwrap_or(Value::Null);
        Ok(comments::CommentPage { nodes, page_info })
    })?;

    if args.json {
        output::print_json(&json!({
            "nodes": collected.nodes,
            "pageInfo": collected.page_info,
        }));
        return Ok(());
    }

    comments::render_comment_threads(&collected.nodes, "No comments found for this issue");
    Ok(())
}
