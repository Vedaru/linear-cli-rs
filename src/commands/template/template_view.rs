//! `linear template view` — port of `src/commands/template/template-view.ts`.
//!
//! Upstream renders the template body through `renderMarkdown` from
//! `@littletof/charmd`. No equivalent renderer exists in this crate, so
//! [`render_markdown`] is a minimal local stand-in: it preserves markdown
//! structure and wraps each line to the requested width. See the module report
//! for this divergence.

use std::io::IsTerminal;

use clap::Args;
use serde_json::{Map, Value};

use super::{parse_template_data, resolve_template, template_id, template_name, template_type};
use crate::errors::Result;
use crate::{display, output};

const INDENT: &str = "  ";

/// Keys whose value is a ProseMirror document holding the template body.
const RICH_TEXT_KEYS: [&str; 2] = ["descriptionData", "contentData"];

const LABEL_KEYS: [&str; 2] = ["title", "name"];

#[derive(Args, Debug)]
pub struct TemplateViewArgs {
    /// Template name or UUID
    pub template: String,
    /// Output the template as JSON (templateData stays a JSON-encoded string; use `jq '.templateData | fromjson'`)
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: TemplateViewArgs) -> Result<()> {
    let template = resolve_template(&args.template)?;

    if args.json {
        output::print_json(&template);
        return Ok(());
    }

    let line_width = if std::io::stdout().is_terminal() {
        terminal_size::terminal_size()
            .map(|(width, _)| width.0 as usize)
            .unwrap_or(80)
    } else {
        80
    };

    output::line(&format_template(&template, line_width)?);
    Ok(())
}

fn format_template(template: &Value, line_width: usize) -> Result<String> {
    let data = parse_template_data(template)?;
    let mut lines: Vec<String> = Vec::new();

    lines.push(template_name(template));
    let scope = match template.get("team").filter(|team| !team.is_null()) {
        None => "Workspace".to_string(),
        Some(team) => {
            let key = team.get("key").and_then(Value::as_str).unwrap_or("");
            let name = team.get("name").and_then(Value::as_str).unwrap_or("");
            format!("Team {key} ({name})")
        }
    };
    lines.push(format!("{} template · {scope}", capitalize(&template_type(template))));
    lines.push(format!("ID: {}", template_id(template)));
    if let Some(description) = template
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("Description: {description}"));
    }
    if template
        .get("hasFormFields")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        lines.push(
            "Form template: yes (its form is filled in inside Linear; applying it from the CLI creates the entity with the form unanswered)"
                .to_string(),
        );
    }
    if let Some(inherited) = template.get("inheritedFrom").filter(|value| !value.is_null()) {
        let name = inherited.get("name").and_then(Value::as_str).unwrap_or("");
        let id = inherited.get("id").and_then(Value::as_str).unwrap_or("");
        lines.push(format!("Inherited from: {name} ({id})"));
    }
    if let Some(creator) = template.get("creator").filter(|value| !value.is_null()) {
        let name = creator.get("name").and_then(Value::as_str).unwrap_or("");
        lines.push(format!("Created by: {name}"));
    }
    if let Some(last_applied) = template
        .get("lastAppliedAt")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!(
            "Last applied: {}",
            display::format_relative_time(last_applied)
        ));
    }
    let updated = template
        .get("updatedAt")
        .and_then(Value::as_str)
        .unwrap_or("");
    lines.push(format!("Updated: {}", display::format_relative_time(updated)));

    lines.push(String::new());
    lines.push("Pre-fills:".to_string());
    let entries: Vec<(String, Value)> = data.into_iter().collect();
    if entries.is_empty() {
        lines.push(format!("{INDENT}(nothing)"));
    }
    lines.extend(render_entries(entries, INDENT.to_string(), line_width)?);
    lines.push(String::new());
    lines.push(
        "References are IDs. Map them with `linear team states`, `linear label list`, `linear user list`, or `linear project list`."
            .to_string(),
    );

    Ok(lines.join("\n"))
}

fn is_record(value: &Value) -> bool {
    value.is_object()
}

fn indent_block(text: &str, indent: &str) -> String {
    text.split('\n')
        .map(|line| {
            if line.is_empty() {
                line.to_string()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn item_label(item: &Map<String, Value>) -> Option<String> {
    for key in LABEL_KEYS {
        if let Some(Value::String(value)) = item.get(key) {
            if !value.is_empty() {
                return Some(value.clone());
            }
        }
    }
    None
}

fn render_entries(
    entries: Vec<(String, Value)>,
    indent: String,
    line_width: usize,
) -> Result<Vec<String>> {
    // Scalars and references first, the body last, so the long part reads last.
    let (mut ordered, rich): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .partition(|(key, _)| !RICH_TEXT_KEYS.contains(&key.as_str()));

    let mut lines = Vec::new();
    for (key, value) in ordered.drain(..).chain(rich) {
        lines.extend(render_pre_fill(&key, &value, &indent, line_width)?);
    }
    Ok(lines)
}

/// One pre-filled value. Every key is shown, at any depth: nested objects and
/// the items of `subIssueData` / `issueData` keep all of their fields.
fn render_pre_fill(
    key: &str,
    value: &Value,
    indent: &str,
    line_width: usize,
) -> Result<Vec<String>> {
    let nested = format!("{indent}{INDENT}");
    if RICH_TEXT_KEYS.contains(&key) && is_record(value) {
        let markdown = crate::prosemirror::prose_mirror_to_markdown(value)?;
        let width = 20.max(line_width.saturating_sub(nested.chars().count()));
        let rendered = render_markdown(&markdown, width);
        return Ok(vec![
            format!("{indent}{key}:"),
            indent_block(rendered.trim_end(), &nested),
        ]);
    }
    if key == "priority" {
        if let Some(number) = value.as_i64() {
            let name = priority_name(number);
            let suffix = name.map(|name| format!(" ({name})")).unwrap_or_default();
            return Ok(vec![format!("{indent}{key}: {number}{suffix}")]);
        }
    }
    match value {
        Value::String(text) => {
            if text.contains('\n') {
                return Ok(vec![
                    format!("{indent}{key}:"),
                    indent_block(text.trim_end(), &nested),
                ]);
            }
            Ok(vec![format!("{indent}{key}: {text}")])
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {
            Ok(vec![format!("{indent}{key}: {value}")])
        }
        Value::Array(items) => {
            if items.is_empty() {
                return Ok(vec![format!("{indent}{key}: (none)")]);
            }
            if items.iter().all(Value::is_string) {
                let joined = items
                    .iter()
                    .map(|item| item.as_str().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Ok(vec![format!("{indent}{key}: {joined}")]);
            }
            let mut lines = vec![format!(
                "{indent}{key}: {} {}",
                items.len(),
                if items.len() == 1 { "item" } else { "items" }
            )];
            for item in items {
                lines.extend(render_item(item, &nested, line_width)?);
            }
            Ok(lines)
        }
        Value::Object(map) => {
            let entries: Vec<(String, Value)> =
                map.iter().map(|(key, value)| (key.clone(), value.clone())).collect();
            let mut lines = vec![format!("{indent}{key}:")];
            lines.extend(render_entries(entries, nested, line_width)?);
            Ok(lines)
        }
    }
}

/// An item of a list such as `subIssueData`: its label, then its other fields.
fn render_item(item: &Value, indent: &str, line_width: usize) -> Result<Vec<String>> {
    let Value::Object(map) = item else {
        return Ok(vec![format!("{indent}- {}", json_stringify(item))]);
    };
    let label = item_label(map);
    let rest: Vec<(String, Value)> = map
        .iter()
        .filter(|(key, value)| {
            !(label.is_some()
                && LABEL_KEYS.contains(&key.as_str())
                && matches!(value, Value::String(text) if Some(text) == label.as_ref()))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut lines = vec![format!(
        "{indent}- {}",
        label.unwrap_or_else(|| "(untitled)".to_string())
    )];
    lines.extend(render_entries(
        rest,
        format!("{indent}{INDENT}{INDENT}"),
        line_width,
    )?);
    Ok(lines)
}

fn priority_name(priority: i64) -> Option<&'static str> {
    match priority {
        0 => Some("none"),
        1 => Some("urgent"),
        2 => Some("high"),
        3 => Some("medium"),
        4 => Some("low"),
        _ => None,
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn json_stringify(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Minimal stand-in for `@littletof/charmd`'s `renderMarkdown`: keep blank
/// lines and markdown structure, wrap each non-empty line to `line_width`.
fn render_markdown(markdown: &str, line_width: usize) -> String {
    let mut out = String::new();
    for (index, line) in markdown.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if line.is_empty() {
            continue;
        }
        let mut current = String::new();
        for word in line.split(' ') {
            if current.is_empty() {
                current.push_str(word);
            } else if current.chars().count() + 1 + word.chars().count() <= line_width {
                current.push(' ');
                current.push_str(word);
            } else {
                out.push_str(&current);
                out.push('\n');
                current = word.to_string();
            }
        }
        out.push_str(&current);
    }
    out
}
