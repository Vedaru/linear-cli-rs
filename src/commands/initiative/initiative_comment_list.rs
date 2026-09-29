//! `linear initiative comment list` — port of
//! `src/commands/initiative/initiative-comment-list.ts`.
//!
//! `Initiative` has no comments connection in the schema, so the listing goes
//! through the root `comments` query filtered by initiative. The initiative
//! itself is selected in the same operation so an unknown UUID — which
//! `resolve_initiative_id` passes through unchecked — is reported as not found
//! instead of as an empty list. `initiative(id:)` takes `String!` while the
//! filter's `eq` takes `ID!`, hence two variables carrying the same value.
//!
//! The comment subgroup `mod.rs` supplies the `Failed to list comments`
//! context, so this module returns bare errors.

use clap::Args;
use serde_json::{json, Value};

use crate::comments::{self, COMMENT_LIST_FIELDS};
use crate::errors::{self, CliError, Result};
use crate::{graphql, linear, output};

#[derive(Args, Debug)]
pub struct InitiativeCommentListArgs {
    /// Initiative ID, URL, slug ID, or name
    #[arg(value_name = "initiative")]
    pub initiative: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: InitiativeCommentListArgs) -> Result<()> {
    let initiative = linear::resolve_initiative_id(&args.initiative)?;

    let query = format!(
        r#"
query GetInitiativeComments($id: String!, $filterId: ID!, $after: String) {{
  initiative(id: $id) {{
    id
    name
  }}
  comments(first: 50, after: $after, orderBy: createdAt, filter: {{ initiative: {{ id: {{ eq: $filterId }} }} }}) {{
    nodes {{
      {COMMENT_LIST_FIELDS}
    }}
    pageInfo {{
      hasNextPage
      endCursor
    }}
  }}
}}
"#
    );

    let client = graphql::client()?;
    let collected = comments::collect_comment_pages(|after| {
        let data = errors::translate_not_found("Initiative", &initiative, || {
            client.request(
                &query,
                json!({ "id": initiative, "filterId": initiative, "after": after }),
            )
        })?;
        data.get("initiative")
            .filter(|value| !value.is_null())
            .ok_or_else(|| CliError::not_found("Initiative", &initiative))?;

        let connection = data.get("comments");
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

    comments::render_comment_threads(&collected.nodes, "No comments found for this initiative");
    Ok(())
}
