//! `linear document view` — port of `src/commands/document/document-view.ts`.
//!
//! Like `issue view`, this ports the non-TTY branch: the document is emitted as
//! raw Linear-flavored Markdown (see AGENTS.md — there is no Rust equivalent of
//! `@littletof/charmd`). `--raw` and a piped stdout both short-circuit to the
//! bare content, exactly as upstream does.

use std::io::IsTerminal;

use serde_json::{json, Value};

use crate::config;
use crate::display;
use crate::errors::{CliError, Result};
use crate::graphql;
use crate::linear;
use crate::markdown;
use crate::output;

#[derive(clap::Args, Debug)]
pub struct DocumentViewArgs {
    /// Document ID, URL, or slug ID
    #[arg(value_name = "id")]
    pub id: String,
    /// Output raw markdown without rendering
    #[arg(long)]
    pub raw: bool,
    /// Open document in browser
    #[arg(short = 'w', long)]
    pub web: bool,
    /// Output full document as JSON
    #[arg(long)]
    pub json: bool,
    /// Keep remote URLs instead of downloading files
    #[arg(long = "no-download", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub download: bool,
}

const GET_DOCUMENT_QUERY: &str = r#"
query GetDocument($id: String!) {
  document(id: $id) {
    id
    title
    slugId
    content
    url
    createdAt
    updatedAt
    creator {
      name
      email
    }
    project {
      name
      slugId
    }
    issue {
      identifier
      title
    }
    initiative {
      name
      slugId
    }
    team {
      name
      key
    }
    cycle {
      name
      number
      team {
        key
      }
    }
    release {
      name
      version
    }
  }
}
"#;

const GET_DOCUMENT_WITH_COMMENTS_QUERY: &str = r#"
query GetDocumentWithComments($id: String!, $commentsAfter: String) {
  document(id: $id) {
    id
    title
    slugId
    content
    url
    createdAt
    updatedAt
    creator {
      name
      email
    }
    project {
      name
      slugId
    }
    issue {
      identifier
      title
    }
    initiative {
      name
      slugId
    }
    team {
      name
      key
    }
    cycle {
      name
      number
      team {
        key
      }
    }
    release {
      name
      version
    }
    comments(first: 50, after: $commentsAfter, orderBy: createdAt) {
      nodes {
        id
        body
        quotedText
        documentContentId
        createdAt
        updatedAt
        archivedAt
        resolvedAt
        url
        user {
          name
          email
        }
        parent {
          id
        }
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

/// `getDocumentWithAllComments`: fetch the first page, then append every
/// following page's nodes onto the same document, so callers see one fully
/// paginated comment list.
fn get_document_with_all_comments(id: &str) -> Result<Option<Value>> {
    let client = graphql::client()?;
    let first = client.request(
        GET_DOCUMENT_WITH_COMMENTS_QUERY,
        json!({ "id": id, "commentsAfter": Value::Null }),
    )?;

    let Some(mut document) = first.get("document").filter(|value| !value.is_null()).cloned() else {
        return Ok(None);
    };

    loop {
        let has_next = document
            .pointer("/comments/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !has_next {
            break;
        }
        let cursor = document
            .pointer("/comments/pageInfo/endCursor")
            .cloned()
            .unwrap_or(Value::Null);
        let next = client.request(
            GET_DOCUMENT_WITH_COMMENTS_QUERY,
            json!({ "id": id, "commentsAfter": cursor }),
        )?;
        let Some(next_document) = next.get("document").filter(|value| !value.is_null()) else {
            return Ok(None);
        };

        let next_nodes = next_document
            .pointer("/comments/nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(nodes) = document
            .pointer_mut("/comments/nodes")
            .and_then(Value::as_array_mut)
        {
            nodes.extend(next_nodes);
        }
        let next_page_info = next_document
            .pointer("/comments/pageInfo")
            .cloned()
            .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));
        document["comments"]["pageInfo"] = next_page_info;
    }

    Ok(Some(document))
}

pub fn run(args: DocumentViewArgs) -> Result<()> {
    let raw_id = args.id.clone();
    let id = linear::resolve_document_reference(&raw_id)?;

    let result = view(&args, &id, &raw_id);
    result.map_err(|error| {
        // Upstream's catch reports a missing document against the *raw* id, not
        // the slug it resolved to.
        let reported = if error.kind != crate::errors::ErrorKind::NotFound && error.is_not_found() {
            CliError::not_found("Document", &raw_id)
        } else {
            error
        };
        reported.with_context("Failed to view document")
    })
}

fn view(args: &DocumentViewArgs, id: &str, raw_id: &str) -> Result<()> {
    let document = if args.json {
        get_document_with_all_comments(id)?
    } else {
        let client = graphql::client()?;
        client
            .request(GET_DOCUMENT_QUERY, json!({ "id": id }))?
            .get("document")
            .filter(|value| !value.is_null())
            .cloned()
    };

    let Some(document) = document else {
        return Err(CliError::not_found("Document", raw_id));
    };

    if args.web {
        let url = document.get("url").and_then(Value::as_str).unwrap_or("");
        output::line(&format!("Opening {url} in web browser"));
        return crate::actions::open_url(url, false);
    }

    if args.json {
        output::print_json(&document);
        return Ok(());
    }

    let mut content = document
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_string);

    let should_download = args.download && config::download_images() != Some(false);
    if should_download {
        if let Some(text) = &content {
            let url_to_path = markdown::download_markdown_images(&[Some(text)]);
            if !url_to_path.is_empty() {
                content = Some(markdown::replace_urls(text, &url_to_path));
            }
        }
    }

    // Raw output (for piping).
    if args.raw || !std::io::stdout().is_terminal() {
        if let Some(text) = &content {
            output::line(text);
        }
        return Ok(());
    }

    output::line(&render_document_markdown(&document, content.as_deref()));
    Ok(())
}

/// The rendered-output branch. Upstream hands this markdown to `charmd`; the
/// port emits it verbatim (see AGENTS.md).
fn render_document_markdown(document: &Value, content: Option<&str>) -> String {
    let title = document.get("title").and_then(Value::as_str).unwrap_or("");
    let slug_id = document.get("slugId").and_then(Value::as_str).unwrap_or("");
    let url = document.get("url").and_then(Value::as_str).unwrap_or("");

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# {title}"));
    lines.push(String::new());
    lines.push(format!("**Slug:** {slug_id}"));
    lines.push(format!("**URL:** {url}"));

    if let Some(creator) = document.pointer("/creator/name").and_then(Value::as_str) {
        lines.push(format!("**Creator:** {creator}"));
    }
    if let Some(project) = document.pointer("/project/name").and_then(Value::as_str) {
        lines.push(format!("**Project:** {project}"));
    }
    if let Some(issue) = document.get("issue").filter(|value| !value.is_null()) {
        let identifier = issue.get("identifier").and_then(Value::as_str).unwrap_or("");
        let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
        lines.push(format!("**Issue:** {identifier} - {title}"));
    }
    if let Some(initiative) = document.pointer("/initiative/name").and_then(Value::as_str) {
        lines.push(format!("**Initiative:** {initiative}"));
    }
    if let Some(team) = document.get("team").filter(|value| !value.is_null()) {
        let name = team.get("name").and_then(Value::as_str).unwrap_or("");
        let key = team.get("key").and_then(Value::as_str).unwrap_or("");
        lines.push(format!("**Team:** {name} ({key})"));
    }
    if let Some(cycle) = document.get("cycle").filter(|value| !value.is_null()) {
        let team_key = cycle
            .pointer("/team/key")
            .and_then(Value::as_str)
            .unwrap_or("");
        let number = cycle.get("number").and_then(Value::as_i64).unwrap_or(0);
        let cycle_name = match cycle.get("name").and_then(Value::as_str) {
            Some(name) if !name.is_empty() => format!(" - {name}"),
            _ => String::new(),
        };
        lines.push(format!("**Cycle:** {team_key} #{number}{cycle_name}"));
    }
    if let Some(release) = document.get("release").filter(|value| !value.is_null()) {
        let name = release.get("name").and_then(Value::as_str).unwrap_or("");
        let version = match release.get("version").and_then(Value::as_str) {
            Some(version) if !version.is_empty() => format!(" ({version})"),
            _ => String::new(),
        };
        lines.push(format!("**Release:** {name}{version}"));
    }

    let created = document
        .get("createdAt")
        .and_then(Value::as_str)
        .map(display::format_relative_time)
        .unwrap_or_default();
    let updated = document
        .get("updatedAt")
        .and_then(Value::as_str)
        .map(display::format_relative_time)
        .unwrap_or_default();
    lines.push(format!("**Created:** {created}"));
    lines.push(format!("**Updated:** {updated}"));

    if let Some(content) = content {
        lines.push(String::new());
        lines.push("---".to_string());
        lines.push(String::new());
        lines.push(content.to_string());
    }

    lines.join("\n")
}
