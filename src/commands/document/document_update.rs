//! `linear document update` — port of `src/commands/document/document-update.ts`.
//!
//! Input order matters: title, then icon, then the re-pointed attachment. The
//! resolved attachment is built *before* content is read so a target-only
//! update never slurps stdin as content (the stdin auto-read is guarded on the
//! input being empty). Content is refused when the document still has an
//! unresolved inline comment unless `--force` is passed, because replacing the
//! markdown can orphan comment anchors.
//!
//! The group `mod.rs` supplies the `Failed to update document` context, so this
//! module returns bare errors.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use clap::Args;
use serde_json::{json, Map, Value};

use crate::commands::document::attachment_target::{
    parse_document_target_options, resolve_document_target, to_document_target_input,
    DocumentTargetOptions, TargetRequirement,
};
use crate::errors::{CliError, Result};
use crate::{editor, graphql, linear, output, proc};

const GET_DOCUMENT_FOR_EDIT_QUERY: &str = r#"
query GetDocumentForEdit($id: String!) {
  document(id: $id) {
    id
    title
    content
  }
}
"#;

const DOCUMENT_INLINE_COMMENT_GUARD_QUERY: &str = r#"
query DocumentInlineCommentGuard($id: String!, $after: String) {
  document(id: $id) {
    id
    comments(first: 50, after: $after, orderBy: createdAt) {
      nodes {
        id
        quotedText
        resolvedAt
        archivedAt
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

const UPDATE_DOCUMENT_MUTATION: &str = r#"
mutation UpdateDocument($id: String!, $input: DocumentUpdateInput!) {
  documentUpdate(id: $id, input: $input) {
    success
    document {
      id
      slugId
      title
      url
      updatedAt
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct DocumentUpdateArgs {
    /// Document ID, URL, or slug ID
    #[arg(value_name = "documentId")]
    pub document_id: String,
    /// New title for the document
    #[arg(short = 't', long, value_name = "title")]
    pub title: Option<String>,
    /// New markdown content (inline)
    #[arg(short = 'c', long, value_name = "content")]
    pub content: Option<String>,
    /// Read new content from file
    #[arg(short = 'f', long = "content-file", value_name = "path")]
    pub content_file: Option<String>,
    /// New icon (emoji)
    #[arg(long, value_name = "icon")]
    pub icon: Option<String>,
    /// Re-point to project (UUID, slug ID, or name); replaces the current attachment
    #[arg(long, value_name = "project")]
    pub project: Option<String>,
    /// Re-point to issue (identifier like TC-123); replaces the current attachment
    #[arg(long, value_name = "issue")]
    pub issue: Option<String>,
    /// Re-point to initiative (UUID, slug ID, or name); replaces the current attachment
    #[arg(long, value_name = "initiative")]
    pub initiative: Option<String>,
    /// Re-point to team (key, name, or ID); with --cycle, scopes the cycle lookup instead
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Re-point to cycle: name, number, 'active'/'now', 'next', 'previous', or a relative offset like +1 (team from --team or config)
    #[arg(long, value_name = "cycle")]
    pub cycle: Option<String>,
    /// Re-point to release (UUID, name, or version); replaces the current attachment
    #[arg(long, value_name = "release")]
    pub release: Option<String>,
    /// Open current content in $EDITOR for editing
    #[arg(short = 'e', long)]
    pub edit: bool,
    /// Update content even when document comments may lose inline anchors
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: DocumentUpdateArgs) -> Result<()> {
    let raw_document_id = args.document_id.clone();
    let document_id = linear::resolve_document_reference(&raw_document_id)?;

    let target_options = DocumentTargetOptions {
        project: args.project.clone(),
        issue: args.issue.clone(),
        initiative: args.initiative.clone(),
        team: args.team.clone(),
        cycle: args.cycle.clone(),
        release: args.release.clone(),
    };
    // Validate target cardinality before any lookup, editor, or stdin work.
    // Zero targets is fine — metadata/content-only updates.
    let selector = parse_document_target_options(&target_options, TargetRequirement::AtMostOne)?;

    let client = graphql::client()?;

    let mut input = Map::new();

    if let Some(title) = args.title.as_deref().filter(|value| !value.is_empty()) {
        input.insert("title".to_string(), json!(title));
    }
    if let Some(icon) = args.icon.as_deref().filter(|value| !value.is_empty()) {
        input.insert("icon".to_string(), json!(icon));
    }

    // Re-point the document's attachment. A document has exactly one target;
    // setting a new one makes the server clear the old one. Resolved here
    // alongside the other metadata flags so it participates in the stdin
    // auto-read guard below (a target-only update shouldn't slurp stdin).
    if let Some(selector) = &selector {
        let target = resolve_document_target(selector)?;
        if let Value::Object(fields) = to_document_target_input(&target) {
            for (key, value) in fields {
                input.insert(key, value);
            }
        }
    }

    let mut final_content: Option<String> = None;

    if let Some(content) = args.content.as_deref().filter(|value| !value.is_empty()) {
        final_content = Some(content.to_string());
    } else if let Some(content_file) = &args.content_file {
        match std::fs::read_to_string(content_file) {
            Ok(content) => final_content = Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CliError::not_found("File", content_file));
            }
            Err(error) => {
                return Err(CliError::cli(format!(
                    "Failed to read content file: {error}"
                )));
            }
        }
    } else if args.edit {
        // Edit mode: fetch current content and open in editor.
        let result = client.request(GET_DOCUMENT_FOR_EDIT_QUERY, json!({ "id": document_id }))?;
        let Some(document) = result.get("document").filter(|value| !value.is_null()) else {
            return Err(CliError::not_found("Document", &document_id));
        };
        let current_content = document
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let title = document.get("title").and_then(Value::as_str).unwrap_or("");
        output::line(&format!("Opening {title} in editor..."));

        let Some(edited) = open_editor_with_content(&current_content)? else {
            output::line("No changes made, update cancelled.");
            return Ok(());
        };
        if edited == current_content {
            output::line("No changes detected, update cancelled.");
            return Ok(());
        }
        final_content = Some(edited);
    } else if !std::io::stdin().is_terminal() && input.is_empty() {
        // Only try reading from stdin if no other update fields were provided.
        if let Some(stdin_content) = read_content_from_stdin() {
            final_content = Some(stdin_content);
        }
    }

    if let Some(content) = final_content {
        input.insert("content".to_string(), json!(content));
    }

    if input.is_empty() {
        return Err(CliError::validation("No update fields provided").suggestion(
            "Use --title, --content, --content-file, --icon, --edit, or re-point the attachment with --project, --issue, --initiative, --team, --cycle, or --release.",
        ));
    }

    if input.get("content").is_some() && !args.force {
        if let Some(comment) = get_first_active_inline_comment(&client, &document_id)? {
            let id = comment.get("id").and_then(Value::as_str).unwrap_or("");
            let quoted = comment
                .get("quotedText")
                .and_then(Value::as_str)
                .unwrap_or("");
            return Err(CliError::validation(
                "Refusing to update document content because this document has inline comments.",
            )
            .suggestion(format!(
                "Updating Markdown content can detach or hide Linear document comments. First review comment {id} quoting \"{quoted}\", then rerun with --force if you accept that risk."
            )));
        }
    }

    let result = client.request(
        UPDATE_DOCUMENT_MUTATION,
        json!({ "id": document_id, "input": Value::Object(input) }),
    )?;

    let updated = result
        .get("documentUpdate")
        .and_then(|value| value.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !updated {
        return Err(CliError::cli("Document update failed"));
    }

    let document = result
        .get("documentUpdate")
        .and_then(|value| value.get("document"))
        .filter(|value| !value.is_null())
        .ok_or_else(|| CliError::cli("Document update failed - no document returned"))?;

    let title = document.get("title").and_then(Value::as_str).unwrap_or("");
    let url = document.get("url").and_then(Value::as_str).unwrap_or("");
    output::line(&format!("✓ Updated document: {title}"));
    output::line(url);
    Ok(())
}

/// An inline comment (`quotedText != null`) still anchored to live text — not
/// resolved and not archived — is the only kind a content replacement can
/// meaningfully orphan. Resolved/archived threads are closed, so detaching
/// their anchor loses nothing and must not block the update.
fn get_first_active_inline_comment(
    client: &graphql::Client,
    document_id: &str,
) -> Result<Option<Value>> {
    let mut after: Value = Value::Null;
    loop {
        let result = client.request(
            DOCUMENT_INLINE_COMMENT_GUARD_QUERY,
            json!({ "id": document_id, "after": after }),
        )?;

        let Some(document) = result.get("document").filter(|value| !value.is_null()) else {
            return Err(CliError::not_found("Document", document_id));
        };

        if let Some(nodes) = document.pointer("/comments/nodes").and_then(Value::as_array) {
            if let Some(comment) = nodes.iter().find(|comment| {
                comment
                    .get("quotedText")
                    .map(|value| !value.is_null())
                    .unwrap_or(false)
                    && comment
                        .get("resolvedAt")
                        .map(Value::is_null)
                        .unwrap_or(true)
                    && comment
                        .get("archivedAt")
                        .map(Value::is_null)
                        .unwrap_or(true)
            }) {
                return Ok(Some(comment.clone()));
            }
        }

        let has_next = document
            .pointer("/comments/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !has_next {
            return Ok(None);
        }
        after = document
            .pointer("/comments/pageInfo/endCursor")
            .cloned()
            .unwrap_or(Value::Null);
    }
}

/// Open `$EDITOR` on a temp markdown file seeded with `initial_content` and
/// return the edited, trimmed content — or `None` when the buffer comes back
/// empty. Mirrors upstream's `openEditorWithContent`.
fn open_editor_with_content(initial_content: &str) -> Result<Option<String>> {
    let Some(editor_name) = editor::get_editor() else {
        return Err(CliError::validation("No editor found").suggestion(
            "Set EDITOR environment variable or configure git editor with: git config --global core.editor <editor>",
        ));
    };

    let Some(temp_file) = create_temp_markdown() else {
        return Err(CliError::cli(
            "Failed to open editor: could not create a temporary file",
        ));
    };

    let result = edit_temp_file(&editor_name, &temp_file, initial_content);
    // Upstream removes the temp file in a `finally`; do the same whether the
    // editor succeeded, failed, or the read failed.
    let _ = std::fs::remove_file(&temp_file);
    result
}

fn edit_temp_file(
    editor_name: &str,
    temp_file: &PathBuf,
    initial_content: &str,
) -> Result<Option<String>> {
    if let Err(error) = std::fs::write(temp_file, initial_content) {
        return Err(CliError::cli(format!("Failed to open editor: {error}")));
    }

    let path_arg = temp_file.to_string_lossy().to_string();
    match proc::run_inherit(editor_name, &[&path_arg], None, proc::EDITOR_TIMEOUT) {
        Some(true) => {}
        Some(false) => return Err(CliError::cli("Editor exited with an error")),
        None => {
            return Err(CliError::cli(
                "Failed to open editor: the editor could not be started",
            ))
        }
    }

    match std::fs::read_to_string(temp_file) {
        Ok(content) => {
            let cleaned = content.trim();
            Ok(if cleaned.is_empty() {
                None
            } else {
                Some(cleaned.to_string())
            })
        }
        Err(error) => Err(CliError::cli(format!("Failed to open editor: {error}"))),
    }
}

/// Create an empty `*.md` file in the system temp directory. Mirrors the
/// private helper in `crate::editor`; kept local so the editor module's
/// existing surface stays unchanged.
fn create_temp_markdown() -> Option<PathBuf> {
    let dir = std::env::temp_dir();
    for attempt in 0..100 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let name = format!("linear-{}-{nanos}-{attempt}.md", std::process::id());
        let path = dir.join(name);
        if !path.exists() {
            std::fs::File::create(&path).ok()?;
            return Some(path);
        }
    }
    None
}

/// Read content from stdin if available, mirroring upstream's 100ms race.
/// Upstream joins the parsed ids back with newlines; the reader trims each line
/// and drops blanks, so both agree on the resulting content.
fn read_content_from_stdin() -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = String::new();
        if std::io::stdin().read_to_string(&mut buffer).is_err() {
            let _ = sender.send(None);
            return;
        }
        let content = buffer
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = sender.send(if content.is_empty() {
            None
        } else {
            Some(content)
        });
    });
    receiver
        .recv_timeout(Duration::from_millis(100))
        .ok()
        .flatten()
}
