//! `linear document comment add` — port of
//! `src/commands/document/document-comment-add.ts`.
//!
//! A document comment attaches to the document's *content* record, not to the
//! document itself, so the target id is looked up first. The group `mod.rs`
//! supplies the `Failed to add comment` context, so this module returns bare
//! errors.

use clap::Args;
use serde_json::{json, Value};

use crate::comments::{
    self, CommentTarget, CreateCommentOptions, COMMENT_BODY_DESCRIPTION,
    COMMENT_BODY_FILE_DESCRIPTION, REPLY_TO_DESCRIPTION,
};
use crate::errors::{self, CliError, Result};
use crate::{graphql, linear, output};

#[derive(Args, Debug)]
pub struct DocumentCommentAddArgs {
    /// Document ID, URL, or slug ID
    #[arg(value_name = "document")]
    pub document: String,
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
}

// `document(id:)` accepts a UUID or a slug ID. A document comment attaches to
// the document's content record, so look that id up first.
const GET_DOCUMENT_COMMENT_TARGET_QUERY: &str = r#"
query GetDocumentCommentTarget($id: String!) {
  document(id: $id) {
    id
    title
    documentContentId
  }
}
"#;

pub fn run(args: DocumentCommentAddArgs) -> Result<()> {
    // Inside the resolution: a wrong-kind or cross-workspace URL is rejected
    // here, and that error has to reach the group's context like any other.
    let document = linear::resolve_document_reference(&args.document)?;
    let text_body = comments::resolve_comment_body(args.body.as_deref(), args.body_file.as_deref())?;

    let client = graphql::client()?;
    let data = errors::translate_not_found("Document", &document, || {
        client.request(GET_DOCUMENT_COMMENT_TARGET_QUERY, json!({ "id": document }))
    })?;

    let Some(found) = data.get("document").filter(|value| !value.is_null()) else {
        return Err(CliError::not_found("Document", &document));
    };

    let document_content_id = found.get("documentContentId").and_then(Value::as_str);
    let Some(document_content_id) = document_content_id.filter(|value| !value.is_empty()) else {
        let title = found.get("title").and_then(Value::as_str).unwrap_or("");
        return Err(CliError::cli(format!(
            "Document \"{title}\" has no content record to comment on"
        ))
        .suggestion(
            "Linear attaches document comments to the document's content; open the document in Linear once so it gets one, then retry.",
        ));
    };

    let comment_body = match text_body {
        Some(body) => body,
        None => comments::prompt_comment_body()?,
    };

    let (_id, url) = comments::create_comment(
        &CommentTarget::Document {
            document_content_id: document_content_id.to_string(),
        },
        &CreateCommentOptions {
            body: comment_body,
            parent_id: args.parent.clone(),
            id: None,
        },
    )?;

    output::line(&format!("✓ Comment added to document {document}"));
    output::line(&url);
    Ok(())
}
