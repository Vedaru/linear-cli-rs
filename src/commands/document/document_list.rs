//! `linear document list` — port of `src/commands/document/document-list.ts`.

use std::io::IsTerminal;

use serde_json::{json, Value};

use crate::commands::document::attachment_target::{
    parse_document_target_options, resolve_document_target, to_document_target_filter,
    DocumentTargetOptions, TargetRequirement,
};
use crate::display;
use crate::{colors, errors::Result, graphql, output};

const LIST_DOCUMENTS_QUERY: &str = r#"
query ListDocuments($filter: DocumentFilter, $first: Int) {
  documents(filter: $filter, first: $first) {
    nodes {
      id
      title
      slugId
      url
      updatedAt
      project { name slugId }
      issue { identifier title }
      initiative { name slugId }
      team { name key }
      cycle { name number team { key } }
      release { name version }
      creator { name }
    }
    pageInfo { hasNextPage endCursor }
  }
}
"#;

#[derive(clap::Args, Debug)]
pub struct DocumentListArgs {
    /// Filter by project (UUID, slug ID, or name)
    #[arg(long = "project")]
    pub project: Option<String>,
    /// Filter by issue (identifier like TC-123)
    #[arg(long = "issue")]
    pub issue: Option<String>,
    /// Filter by initiative (UUID, slug ID, or name)
    #[arg(long = "initiative")]
    pub initiative: Option<String>,
    /// Filter by team (key, name, or ID); with --cycle, scopes the cycle lookup instead
    #[arg(long = "team")]
    pub team: Option<String>,
    /// Filter by cycle: name, number, 'active'/'now', 'next', 'previous', or a relative offset like +1 (team from --team or config)
    #[arg(long = "cycle")]
    pub cycle: Option<String>,
    /// Filter by release (UUID, name, or version)
    #[arg(long = "release")]
    pub release: Option<String>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Limit results
    #[arg(long, default_value_t = 50)]
    pub limit: i64,
}

/// One-line typed attachment label for the list table. Targets come from six
/// namespaces, so a bare name would be ambiguous. A document has exactly one
/// target; the chain order is just a deterministic fallback for anomalous or
/// legacy (targetless) documents.
pub fn format_document_attachment(doc: &Value) -> String {
    if let Some(name) = non_empty(doc.pointer("/project/name")) {
        return format!("Project: {name}");
    }
    if let Some(identifier) = non_empty(doc.pointer("/issue/identifier")) {
        return format!("Issue: {identifier}");
    }
    if let Some(name) = non_empty(doc.pointer("/initiative/name")) {
        return format!("Initiative: {name}");
    }
    if let Some(team) = doc.get("team").filter(|value| !value.is_null()) {
        let name = team.get("name").and_then(Value::as_str).unwrap_or("");
        let key = team.get("key").and_then(Value::as_str).unwrap_or("");
        return format!("Team: {name} ({key})");
    }
    if let Some(cycle) = doc.get("cycle").filter(|value| !value.is_null()) {
        let name = match cycle.get("name").and_then(Value::as_str) {
            Some(value) if !value.is_empty() => format!(" — {value}"),
            _ => String::new(),
        };
        let number = cycle.get("number").and_then(Value::as_i64).unwrap_or(0);
        let team_key = cycle
            .pointer("/team/key")
            .and_then(Value::as_str)
            .unwrap_or("");
        return format!("Cycle: {team_key} #{number}{name}");
    }
    if let Some(release) = doc.get("release").filter(|value| !value.is_null()) {
        let name = release.get("name").and_then(Value::as_str).unwrap_or("");
        let version = match release.get("version").and_then(Value::as_str) {
            Some(value) if !value.is_empty() => format!(" ({value})"),
            _ => String::new(),
        };
        return format!("Release: {name}{version}");
    }
    "-".to_string()
}

pub fn run(args: DocumentListArgs) -> Result<()> {
    let target_options = DocumentTargetOptions {
        project: args.project.clone(),
        issue: args.issue.clone(),
        initiative: args.initiative.clone(),
        team: args.team.clone(),
        cycle: args.cycle.clone(),
        release: args.release.clone(),
    };

    // Validate target cardinality before any network work. A document has
    // exactly one target, so combining two target filters can never match
    // anything — error instead of printing an empty list.
    let selector = parse_document_target_options(&target_options, TargetRequirement::AtMostOne)?;
    let filter = match &selector {
        Some(selector) => Some(to_document_target_filter(&resolve_document_target(
            selector,
        )?)),
        None => None,
    };

    let client = graphql::client()?;
    let mut variables = serde_json::Map::new();
    if let Some(filter) = &filter {
        variables.insert("filter".to_string(), filter.clone());
    }
    variables.insert("first".to_string(), json!(args.limit));
    let result = client.request(LIST_DOCUMENTS_QUERY, Value::Object(variables))?;

    let documents_connection = result.get("documents").cloned().unwrap_or_else(
        || json!({ "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } }),
    );

    if args.json {
        output::print_json(&documents_connection);
        return Ok(());
    }

    let documents = documents_connection
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if documents.is_empty() {
        output::line("No documents found.");
        return Ok(());
    }

    let columns = if std::io::stdout().is_terminal() {
        terminal_size::terminal_size()
            .map(|(width, _)| width.0 as usize)
            .unwrap_or(120)
    } else {
        120
    };

    // --- measure columns ---------------------------------------------------
    let slug_width = documents
        .iter()
        .map(|doc| display::display_width(str_at(doc, "slugId")))
        .fold(4usize, usize::max);

    let attachment_width = documents
        .iter()
        .map(|doc| display::display_width(&format_document_attachment(doc)))
        .fold(10usize, usize::max);

    let updated_width = documents
        .iter()
        .map(|doc| display::display_width(&updated_ago(doc)))
        .fold(7usize, usize::max);

    let space_width = 3usize;
    let fixed = slug_width + attachment_width + updated_width + space_width;
    let padding = 1usize;
    let available_width = columns
        .saturating_sub(padding)
        .saturating_sub(fixed)
        .max(10);
    let max_title_width = documents
        .iter()
        .map(|doc| display::display_width(str_at(doc, "title")))
        .max()
        .unwrap_or(0);
    let title_width = max_title_width.min(available_width);

    // --- header ------------------------------------------------------------
    let header = [
        display::pad_display("SLUG", slug_width),
        display::pad_display("TITLE", title_width),
        display::pad_display("ATTACHMENT", attachment_width),
        display::pad_display("UPDATED", updated_width),
    ];
    output::line(&colors::header(&header.join(" ")));

    // --- rows --------------------------------------------------------------
    for doc in &documents {
        let title = str_at(doc, "title");
        let title_width_actual = display::display_width(title);
        let trunc_title = if title_width_actual > title_width {
            let slice = display::truncate_text(title, title_width.saturating_sub(3));
            format!("{slice}...")
        } else {
            display::pad_display(title, title_width)
        };

        let attachment = format_document_attachment(doc);
        let updated = updated_ago(doc);

        let mut line = String::new();
        line.push_str(&display::pad_display(str_at(doc, "slugId"), slug_width));
        line.push(' ');
        line.push_str(&trunc_title);
        line.push(' ');
        line.push_str(&display::pad_display(&attachment, attachment_width));
        line.push(' ');
        line.push_str(&colors::muted(&display::pad_display(
            &updated,
            updated_width,
        )));
        output::line(&line);
    }

    Ok(())
}

fn non_empty(value: Option<&Value>) -> Option<String> {
    match value.and_then(Value::as_str) {
        Some(text) if !text.is_empty() => Some(text.to_string()),
        _ => None,
    }
}

fn str_at<'a>(doc: &'a Value, key: &str) -> &'a str {
    doc.get(key).and_then(Value::as_str).unwrap_or("")
}

fn updated_ago(doc: &Value) -> String {
    doc.get("updatedAt")
        .and_then(Value::as_str)
        .map(display::format_relative_time)
        .unwrap_or_default()
}
