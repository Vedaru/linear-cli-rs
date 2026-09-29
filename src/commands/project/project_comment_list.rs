//! `linear project comment list` — port of
//! `src/commands/project/project-comment-list.ts`.
//!
//! The group `mod.rs` supplies the `Failed to list comments` context, so this
//! module returns bare errors.

use clap::Args;
use serde_json::{json, Value};

use crate::comments::{self, COMMENT_LIST_FIELDS};
use crate::errors::{self, CliError, Result};
use crate::{graphql, linear, output};

#[derive(Args, Debug)]
pub struct ProjectCommentListArgs {
    /// Project ID, URL, slug ID, or name
    #[arg(value_name = "project")]
    pub project: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ProjectCommentListArgs) -> Result<()> {
    let project = linear::resolve_project_id(&args.project)?;

    let query = format!(
        r#"
query GetProjectComments($id: String!, $filterId: ID!, $after: String) {{
  project(id: $id) {{
    id
    name
  }}
  comments(first: 50, after: $after, orderBy: createdAt, filter: {{ project: {{ id: {{ eq: $filterId }} }} }}) {{
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
        let data = errors::translate_not_found("Project", &project, || {
            client.request(
                &query,
                json!({ "id": project, "filterId": project, "after": after }),
            )
        })?;
        data.get("project")
            .filter(|value| !value.is_null())
            .ok_or_else(|| CliError::not_found("Project", &project))?;

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

    comments::render_comment_threads(&collected.nodes, "No comments found for this project");
    Ok(())
}
