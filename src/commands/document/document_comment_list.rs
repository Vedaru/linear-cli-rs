//! `linear document comment list` — port of
//! `src/commands/document/document-comment-list.ts`.
//!
//! `document(id:)` accepts a UUID or a slug ID, so only the shared reference
//! resolver runs before the paged query. The group `mod.rs` supplies the
//! `Failed to list comments` context, so this module returns bare errors.

use clap::Args;
use serde_json::{json, Value};

use crate::comments::{self, COMMENT_LIST_FIELDS};
use crate::errors::{self, CliError, Result};
use crate::{graphql, linear, output};

#[derive(Args, Debug)]
pub struct DocumentCommentListArgs {
    /// Document ID, URL, or slug ID
    #[arg(value_name = "document")]
    pub document: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: DocumentCommentListArgs) -> Result<()> {
    let document = linear::resolve_document_reference(&args.document)?;

    let query = format!(
        r#"
query GetDocumentComments($id: String!, $after: String) {{
  document(id: $id) {{
    id
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
        let data = errors::translate_not_found("Document", &document, || {
            client.request(&query, json!({ "id": document, "after": after }))
        })?;
        let found = data
            .get("document")
            .filter(|value| !value.is_null())
            .ok_or_else(|| CliError::not_found("Document", &document))?;

        let connection = found.get("comments");
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

    comments::render_comment_threads(&collected.nodes, "No comments found for this document");
    Ok(())
}
