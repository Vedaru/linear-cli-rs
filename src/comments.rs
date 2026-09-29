//! Everything about comments that does not depend on which entity they hang
//! off. Port of `src/utils/comments.ts`.
//!
//! Issues, documents, projects, and initiatives each own their argument
//! resolution and their list query; the create mutation, body handling, the
//! selection set, pagination, and the threaded rendering live here so the four
//! surfaces cannot drift apart.

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use crate::colors;
use crate::display;
use crate::errors::{CliError, Result};
use crate::graphql;
use crate::linear_url::{reject_comment_url, reject_linear_url};
use crate::prompt;

/// Shared option descriptions so the four `comment add` commands read alike.
pub const COMMENT_BODY_DESCRIPTION: &str = "Comment body text";
pub const COMMENT_BODY_FILE_DESCRIPTION: &str =
    "Read comment body from a file (preferred for markdown content)";
pub const REPLY_TO_DESCRIPTION: &str =
    "Reply to a top-level comment by ID (the reply joins that thread)";

/// The entity a new comment is attached to. Linear's `CommentCreateInput`
/// requires exactly one of these even for replies — a `parentId` on its own is
/// rejected — so every caller names its target explicitly and the input is
/// built in one place.
#[derive(Debug, Clone)]
pub enum CommentTarget {
    Issue { issue_id: String },
    Document { document_content_id: String },
    Project { project_id: String },
    Initiative { initiative_id: String },
}

/// Options for [`create_comment`].
#[derive(Debug, Clone, Default)]
pub struct CreateCommentOptions {
    pub body: String,
    /// Top-level comment to reply to. Linear rejects a reply to a reply.
    pub parent_id: Option<String>,
    /// Caller-supplied UUID v4, for idempotent retries.
    pub id: Option<String>,
}

/// Build the `CommentCreateInput` payload, validating any `parentId`.
pub fn build_comment_create_input(
    target: &CommentTarget,
    options: &CreateCommentOptions,
) -> Result<Value> {
    let mut input = Map::new();
    input.insert("body".to_string(), Value::String(options.body.clone()));

    if let Some(parent_id) = &options.parent_id {
        // Every comment-add command's --reply-to lands here. A pasted comment
        // link carries only the first eight characters of the comment's ID, so
        // it gets the specific explanation; any other Linear URL gets the
        // general one.
        reject_comment_url(parent_id)?;
        reject_linear_url(parent_id, "the UUID of the comment to reply to")?;
        input.insert("parentId".to_string(), Value::String(parent_id.clone()));
    }
    if let Some(id) = &options.id {
        input.insert("id".to_string(), Value::String(id.clone()));
    }

    match target {
        CommentTarget::Issue { issue_id } => {
            input.insert("issueId".to_string(), Value::String(issue_id.clone()));
        }
        CommentTarget::Document {
            document_content_id,
        } => {
            input.insert(
                "documentContentId".to_string(),
                Value::String(document_content_id.clone()),
            );
        }
        CommentTarget::Project { project_id } => {
            input.insert("projectId".to_string(), Value::String(project_id.clone()));
        }
        CommentTarget::Initiative { initiative_id } => {
            input.insert(
                "initiativeId".to_string(),
                Value::String(initiative_id.clone()),
            );
        }
    }

    Ok(Value::Object(input))
}

const ADD_COMMENT_MUTATION: &str = r#"
mutation AddComment($input: CommentCreateInput!) {
  commentCreate(input: $input) {
    success
    comment {
      id
      url
    }
  }
}
"#;

/// Create a comment (or reply) on the given target and return its id and URL.
pub fn create_comment(
    target: &CommentTarget,
    options: &CreateCommentOptions,
) -> Result<(String, String)> {
    let input = build_comment_create_input(target, options)?;
    let client = graphql::client()?;
    let data = client.request(ADD_COMMENT_MUTATION, json!({ "input": input }))?;

    let created = data
        .get("commentCreate")
        .and_then(|value| value.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !created {
        return Err(CliError::cli("Failed to create comment"));
    }

    let comment = data
        .get("commentCreate")
        .and_then(|value| value.get("comment"))
        .filter(|value| !value.is_null())
        .ok_or_else(|| CliError::cli("Comment creation failed - no comment returned"))?;

    let id = comment.get("id").and_then(Value::as_str).unwrap_or("");
    let url = comment.get("url").and_then(Value::as_str).unwrap_or("");
    Ok((id.to_string(), url.to_string()))
}

/// Turn the `--body` / `--body-file` flags into a body, or `None` when neither
/// was given so the caller can prompt. Explicitly supplied input that is blank
/// is an error, never a fallback to the prompt.
pub fn resolve_comment_body(body: Option<&str>, body_file: Option<&str>) -> Result<Option<String>> {
    if body.is_some() && body_file.is_some() {
        return Err(CliError::validation(
            "Cannot specify both --body and --body-file",
        ));
    }

    if let Some(path) = body_file {
        let content = std::fs::read_to_string(path).map_err(|error| {
            CliError::validation(format!("Failed to read body file: {path}"))
                .suggestion(format!("Error: {error}"))
        })?;
        if content.trim().is_empty() {
            return Err(CliError::validation(format!("Body file is empty: {path}"))
                .suggestion("Write the comment into the file, or use --body."));
        }
        return Ok(Some(content));
    }

    if let Some(text) = body {
        if text.trim().is_empty() {
            return Err(
                CliError::validation("Comment body cannot be empty")
                    .suggestion("Pass text with --body, or omit it to be prompted."),
            );
        }
        return Ok(Some(text.to_string()));
    }

    Ok(None)
}

/// Interactive fallback when no body flag was given.
pub fn prompt_comment_body() -> Result<String> {
    if !prompt::is_interactive() {
        return Err(CliError::cli(
            "Cannot prompt for a comment body in a non-interactive environment",
        )
        .suggestion("Provide the body with --body or --body-file instead."));
    }

    eprint!("Comment body: ");
    let _ = std::io::Write::flush(&mut std::io::stderr());

    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)
        .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
    let body = line.trim_end_matches(['\r', '\n']).to_string();

    if body.trim().is_empty() {
        return Err(CliError::validation("Comment body cannot be empty"));
    }
    Ok(body)
}

/// The fields every `comment list --json` node carries. `quotedText` is set on
/// inline comments anchored to text (documents, issue descriptions);
/// `parent.id` is set on replies.
///
/// Upstream spells this as a named fragment; the port inlines the selection so
/// the verbatim `{ nodes, pageInfo }` shape is preserved without a fragment
/// resolver.
pub const COMMENT_LIST_FIELDS: &str = r#"id
        body
        quotedText
        createdAt
        updatedAt
        editedAt
        url
        user {
          id
          name
          displayName
        }
        externalUser {
          id
          name
          displayName
        }
        botActor {
          id
          name
          type
          subType
        }
        parent {
          id
        }"#;

/// One page of a comment connection, in the same `{ nodes, pageInfo }` shape.
#[derive(Debug, Clone)]
pub struct CommentPage {
    pub nodes: Vec<Value>,
    pub page_info: Value,
}

/// Fetch every page of a comment connection and return it in the same
/// `{ nodes, pageInfo }` shape (all nodes, the last page's pageInfo), so
/// `--json` output stays a GraphQL connection. Throws rather than looping or
/// returning a partial list if Linear reports another page without a usable
/// cursor.
pub fn collect_comment_pages<F>(mut fetch_page: F) -> Result<CommentPage>
where
    F: FnMut(Option<&str>) -> Result<CommentPage>,
{
    let mut nodes: Vec<Value> = Vec::new();
    let mut seen_cursors: HashSet<String> = HashSet::new();
    let mut after: Option<String> = None;

    loop {
        let page = fetch_page(after.as_deref())?;
        nodes.extend(page.nodes);

        let has_next = page
            .page_info
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !has_next {
            return Ok(CommentPage {
                nodes,
                page_info: page.page_info,
            });
        }

        let cursor = page
            .page_info
            .get("endCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        let Some(cursor) = cursor else {
            return Err(CliError::cli(
                "Linear reported more comments but did not return a usable cursor",
            )
            .suggestion("Rerun the command; if it persists, report it."));
        };
        if !seen_cursors.insert(cursor.clone()) {
            return Err(CliError::cli(
                "Linear reported more comments but did not return a usable cursor",
            )
            .suggestion("Rerun the command; if it persists, report it."));
        }
        after = Some(cursor);
    }
}

/// The name to render for a comment's author.
///
/// Integration-authored comments have neither `user` nor `externalUser`, so
/// they used to fall all the way through to "Unknown". `botActor` is checked
/// last so a comment carrying both a user and a bot actor still renders the
/// human.
pub fn format_comment_author(comment: &Value) -> String {
    let author_field = |object: &str, field: &str| -> Option<String> {
        comment
            .get(object)
            .and_then(|value| value.get(field))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };

    for field in ["displayName", "name"] {
        if let Some(name) = author_field("user", field) {
            return name;
        }
    }
    for field in ["displayName", "name"] {
        if let Some(name) = author_field("externalUser", field) {
            return name;
        }
    }

    let bot = comment.get("botActor").filter(|value| !value.is_null());
    match bot {
        None => "Unknown".to_string(),
        Some(bot) => {
            // ActorBot.name is nullable; type ("github", "slack", ...) is not.
            bot.get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .or_else(|| bot.get("type").and_then(Value::as_str))
                .unwrap_or("")
                .to_string()
        }
    }
}

fn created_at_millis(comment: &Value) -> i64 {
    comment
        .get("createdAt")
        .and_then(Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp_millis())
        .unwrap_or(0)
}

fn indent(text: &str) -> String {
    text.split('\n')
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_reply_header(reply: &Value, verb: &str) -> String {
    let author = format_comment_author(reply);
    let date = display::format_relative_time(
        reply.get("createdAt").and_then(Value::as_str).unwrap_or(""),
    );
    let id = reply.get("id").and_then(Value::as_str).unwrap_or("");
    format!("{} {verb} {date} [{id}]", colors::bold(&format!("@{author}")))
}

fn quoted_text(comment: &Value) -> Option<&str> {
    comment
        .get("quotedText")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// Print comments as threads: root comments newest first, each followed by its
/// replies oldest first. An inline comment shows the text it is anchored to.
/// Replies whose parent is not in the list (for example a deleted root) are
/// printed last, still labelled as replies, rather than dropped.
pub fn render_comment_threads(comments: &[Value], empty_message: &str) {
    if comments.is_empty() {
        crate::output::line(empty_message);
        return;
    }

    let roots: Vec<&Value> = comments
        .iter()
        .filter(|comment| {
            comment
                .get("parent")
                .map(|parent| parent.is_null())
                .unwrap_or(true)
        })
        .collect();
    let root_ids: HashSet<&str> = roots
        .iter()
        .filter_map(|comment| comment.get("id").and_then(Value::as_str))
        .collect();

    let mut replies_by_parent: std::collections::HashMap<String, Vec<&Value>> =
        std::collections::HashMap::new();
    let mut orphan_replies: Vec<(&Value, String)> = Vec::new();
    for comment in comments {
        let parent_id = comment
            .get("parent")
            .and_then(|parent| parent.get("id"))
            .and_then(Value::as_str);
        let Some(parent_id) = parent_id else {
            continue;
        };
        if !root_ids.contains(parent_id) {
            orphan_replies.push((comment, parent_id.to_string()));
            continue;
        }
        replies_by_parent
            .entry(parent_id.to_string())
            .or_default()
            .push(comment);
    }

    let mut sorted_roots = roots.clone();
    sorted_roots.sort_by_key(|root| std::cmp::Reverse(created_at_millis(root)));

    for root in sorted_roots {
        let author = format_comment_author(root);
        let date = display::format_relative_time(
            root.get("createdAt").and_then(Value::as_str).unwrap_or(""),
        );
        let id = root.get("id").and_then(Value::as_str).unwrap_or("");
        crate::output::line(&format!(
            "{} commented {date} [{id}]",
            colors::bold(&format!("@{author}"))
        ));
        if let Some(quoted) = quoted_text(root) {
            crate::output::line(&format!("> {quoted}"));
        }
        crate::output::line(root.get("body").and_then(Value::as_str).unwrap_or(""));

        let mut replies = replies_by_parent
            .get(id)
            .cloned()
            .unwrap_or_default();
        replies.sort_by_key(|reply| created_at_millis(reply));
        if !replies.is_empty() {
            crate::output::blank();
            for reply in replies {
                crate::output::line(&indent(&format_reply_header(reply, "replied")));
                if let Some(quoted) = quoted_text(reply) {
                    crate::output::line(&indent(&format!("> {quoted}")));
                }
                crate::output::line(&indent(
                    reply.get("body").and_then(Value::as_str).unwrap_or(""),
                ));
            }
        }

        crate::output::blank();
    }

    orphan_replies.sort_by_key(|(reply, _)| created_at_millis(reply));
    for (reply, parent_id) in orphan_replies {
        crate::output::line(&indent(&format_reply_header(
            reply,
            &format!("replied to [{parent_id}]"),
        )));
        if let Some(quoted) = quoted_text(reply) {
            crate::output::line(&indent(&format!("> {quoted}")));
        }
        crate::output::line(&indent(
            reply.get("body").and_then(Value::as_str).unwrap_or(""),
        ));
        crate::output::blank();
    }
}
