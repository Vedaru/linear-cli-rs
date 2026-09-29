//! ProseMirror → Markdown. Port of `src/utils/prosemirror.ts`.
//!
//! Linear stores a template's `descriptionData` / `contentData` as a
//! ProseMirror document. This is a best-effort converter so a template body can
//! be shown the way `issue view` shows an issue body. Nodes the converter does
//! not know are rendered visibly instead of dropped, and a value that is not a
//! ProseMirror document at all is a validation error rather than an empty
//! string.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::errors::{CliError, Result};

#[derive(Debug)]
struct Mark {
    mark_type: String,
    attrs: Map<String, Value>,
}

#[derive(Debug)]
struct Node {
    node_type: String,
    attrs: Map<String, Value>,
    content: Vec<Node>,
    text: Option<String>,
    marks: Vec<Mark>,
}

fn is_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn read_mark(value: &Value, path: &str) -> Result<Mark> {
    let Some(record) = is_record(value) else {
        return Err(CliError::validation(format!(
            "Invalid ProseMirror mark at {path}: expected an object with a string \"type\""
        )));
    };
    let Some(mark_type) = record.get("type").and_then(Value::as_str) else {
        return Err(CliError::validation(format!(
            "Invalid ProseMirror mark at {path}: expected an object with a string \"type\""
        )));
    };
    let attrs = match record.get("attrs").and_then(Value::as_object) {
        Some(attrs) => attrs.clone(),
        None => Map::new(),
    };
    Ok(Mark {
        mark_type: mark_type.to_string(),
        attrs,
    })
}

fn read_node(value: &Value, path: &str) -> Result<Node> {
    let Some(record) = is_record(value) else {
        return Err(CliError::validation(format!(
            "Invalid ProseMirror node at {path}: expected an object with a string \"type\""
        )));
    };
    let Some(node_type) = record.get("type").and_then(Value::as_str) else {
        return Err(CliError::validation(format!(
            "Invalid ProseMirror node at {path}: expected an object with a string \"type\""
        )));
    };

    let content_values: Vec<&Value> = match record.get("content") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().collect(),
        Some(_) => {
            return Err(CliError::validation(format!(
                "Invalid ProseMirror node at {path}: \"content\" must be an array"
            )))
        }
    };

    let mark_values: Vec<&Value> = match record.get("marks") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().collect(),
        Some(_) => {
            return Err(CliError::validation(format!(
                "Invalid ProseMirror node at {path}: \"marks\" must be an array"
            )))
        }
    };

    let text = match record.get("text") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => {
            return Err(CliError::validation(format!(
                "Invalid ProseMirror node at {path}: \"text\" must be a string"
            )))
        }
    };

    let attrs = match record.get("attrs").and_then(Value::as_object) {
        Some(attrs) => attrs.clone(),
        None => Map::new(),
    };

    let mut content = Vec::with_capacity(content_values.len());
    for (index, child) in content_values.iter().enumerate() {
        content.push(read_node(child, &format!("{path}.content[{index}]"))?);
    }
    let mut marks = Vec::with_capacity(mark_values.len());
    for (index, mark) in mark_values.iter().enumerate() {
        marks.push(read_mark(mark, &format!("{path}.marks[{index}]"))?);
    }

    Ok(Node {
        node_type: node_type.to_string(),
        attrs,
        content,
        text,
        marks,
    })
}

fn attr_string(attrs: &Map<String, Value>, key: &str) -> String {
    attrs
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn attr_number(attrs: &Map<String, Value>, key: &str, fallback: f64) -> f64 {
    match attrs.get(key).and_then(Value::as_f64) {
        Some(value) if value.is_finite() => value,
        _ => fallback,
    }
}

fn escape_chars_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"[\\*_`\[\]~<>#]"#).expect("valid escape regex"))
}

fn list_marker_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Rust's regex crate has no lookaround, so the trailing whitespace is
    // captured instead of asserted and re-emitted in the replacement.
    RE.get_or_init(|| {
        Regex::new(r"(?m)^([ \t]*)(?:([-+])|(\d+)\.)(\s)").expect("valid list marker regex")
    })
}

/// Escape characters that Markdown would otherwise interpret, so literal text
/// from the template (an asterisk, a backtick, a leading "1.") survives the
/// Markdown renderer unchanged.
fn escape_markdown(text: &str) -> String {
    let escaped = escape_chars_regex()
        .replace_all(text, |caps: &regex::Captures| format!("\\{}", &caps[0]))
        .into_owned();

    list_marker_regex()
        .replace_all(&escaped, |caps: &regex::Captures| {
            let space = &caps[1];
            let whitespace = &caps[4];
            match caps.get(2) {
                Some(bullet) => format!("{space}\\{}{whitespace}", bullet.as_str()),
                None => format!("{space}{}\\.{whitespace}", &caps[3]),
            }
        })
        .into_owned()
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for ch in text.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

/// A code span whose fence is longer than any backtick run inside it.
fn code_span(text: &str) -> String {
    let longest = longest_backtick_run(text);
    let fence = "`".repeat(longest + 1);
    if longest == 0 {
        format!("{fence}{text}{fence}")
    } else {
        format!("{fence} {text} {fence}")
    }
}

fn apply_marks(text: &str, marks: &[Mark]) -> String {
    let is_code = marks.iter().any(|mark| mark.mark_type == "code");
    let mut result = if is_code {
        code_span(text)
    } else {
        escape_markdown(text)
    };
    for mark in marks {
        match mark.mark_type.as_str() {
            "bold" | "strong" => result = format!("**{result}**"),
            "italic" | "em" => result = format!("_{result}_"),
            // Already fenced above, before the other marks wrap it.
            "code" => {}
            "strike" | "strikethrough" => result = format!("~~{result}~~"),
            "link" => {
                let href = attr_string(&mark.attrs, "href");
                if !href.is_empty() {
                    result = format!("[{result}]({href})");
                }
            }
            // Underline, text color, and other decorations have no Markdown form.
            _ => {}
        }
    }
    result
}

fn render_inline(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node.node_type.as_str() {
            "text" => out.push_str(&apply_marks(
                node.text.as_deref().unwrap_or(""),
                &node.marks,
            )),
            "hard_break" => out.push('\n'),
            "image" => out.push_str(&format!(
                "![{}]({})",
                attr_string(&node.attrs, "alt"),
                attr_string(&node.attrs, "src")
            )),
            _ => {
                // Mentions and similar inline atoms carry their display text
                // in attrs.
                let label = attr_string(&node.attrs, "label");
                if !label.is_empty() {
                    out.push_str(&apply_marks(&label, &node.marks));
                } else if let Some(text) = &node.text {
                    out.push_str(&apply_marks(text, &node.marks));
                } else if !node.content.is_empty() {
                    out.push_str(&render_inline(&node.content));
                } else {
                    out.push_str(&format!("[{}]", node.node_type));
                }
            }
        }
    }
    out
}

fn indent_continuation(text: &str, indent: &str) -> String {
    text.split('\n')
        .enumerate()
        .map(|(index, line)| {
            if index == 0 || line.is_empty() {
                line.to_string()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_list_items<F>(items: &[Node], marker: F) -> String
where
    F: Fn(&Node, usize) -> String,
{
    let mut rendered = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let prefix = marker(item, index);
        let indent = " ".repeat(prefix.chars().count());
        let body = if item.node_type == "list_item" || item.node_type == "todo_item" {
            render_blocks(&item.content)
        } else {
            render_block(item)
        };
        rendered.push(format!("{prefix}{}", indent_continuation(&body, &indent)));
    }
    rendered.join("\n")
}

fn render_block(node: &Node) -> String {
    match node.node_type.as_str() {
        "paragraph" => render_inline(&node.content),
        "heading" => {
            let level = attr_number(&node.attrs, "level", 1.0).clamp(1.0, 6.0) as usize;
            format!("{} {}", "#".repeat(level), render_inline(&node.content))
        }
        "bullet_list" => render_list_items(&node.content, |_item, _index| "- ".to_string()),
        "ordered_list" => {
            let start = attr_number(&node.attrs, "order", 1.0) as i64;
            render_list_items(&node.content, |_item, index| {
                format!("{}. ", start + index as i64)
            })
        }
        "todo_list" => render_list_items(&node.content, |item, _index| {
            let done = item.attrs.get("done") == Some(&Value::Bool(true))
                || item.attrs.get("checked") == Some(&Value::Bool(true));
            if done {
                "- [x] ".to_string()
            } else {
                "- [ ] ".to_string()
            }
        }),
        "code_block" => {
            let language = attr_string(&node.attrs, "language");
            let code: String = node
                .content
                .iter()
                .map(|child| child.text.as_deref().unwrap_or(""))
                .collect();
            // The fence must be longer than any backtick run inside the code.
            let fence = "`".repeat(longest_backtick_run(&code).max(2) + 1);
            format!("{fence}{language}\n{code}\n{fence}")
        }
        "blockquote" => render_blocks(&node.content)
            .split('\n')
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        "horizontal_rule" => "---".to_string(),
        "text" | "hard_break" | "image" => render_inline(std::slice::from_ref(node)),
        _ => {
            if !node.content.is_empty() {
                render_blocks(&node.content)
            } else if let Some(text) = &node.text {
                apply_marks(text, &node.marks)
            } else {
                format!("[unsupported {} node]", node.node_type)
            }
        }
    }
}

fn render_blocks(nodes: &[Node]) -> String {
    nodes
        .iter()
        .map(render_block)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Convert a ProseMirror document to Markdown. Errors when the value is not a
/// ProseMirror document at all.
pub fn prose_mirror_to_markdown(doc: &Value) -> Result<String> {
    let root = read_node(doc, "doc")?;
    if root.node_type != "doc" {
        return Err(CliError::validation(format!(
            "Expected a ProseMirror document, got a \"{}\" node",
            root.node_type
        )));
    }
    Ok(render_blocks(&root.content).trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_paragraphs_and_marks() {
        let doc = json!({
            "type": "doc",
            "content": [
                { "type": "paragraph", "content": [
                    { "type": "text", "text": "hello " },
                    { "type": "text", "text": "world", "marks": [{ "type": "bold", "attrs": {} }] }
                ]}
            ]
        });
        assert_eq!(prose_mirror_to_markdown(&doc).unwrap(), "hello **world**");
    }

    #[test]
    fn headings_are_clamped_to_six_levels() {
        let doc = json!({
            "type": "doc",
            "content": [
                { "type": "heading", "attrs": { "level": 9 },
                  "content": [{ "type": "text", "text": "Title" }] }
            ]
        });
        assert_eq!(prose_mirror_to_markdown(&doc).unwrap(), "###### Title");
    }

    #[test]
    fn escapes_literal_markdown_characters() {
        let doc = json!({
            "type": "doc",
            "content": [{ "type": "paragraph", "content": [
                { "type": "text", "text": "a * b ` c" }
            ]}]
        });
        assert_eq!(prose_mirror_to_markdown(&doc).unwrap(), "a \\* b \\` c");
    }

    #[test]
    fn leading_list_markers_are_escaped() {
        let doc = json!({
            "type": "doc",
            "content": [{ "type": "paragraph", "content": [
                { "type": "text", "text": "- not a list" }
            ]}]
        });
        assert_eq!(prose_mirror_to_markdown(&doc).unwrap(), "\\- not a list");
    }

    #[test]
    fn code_spans_grow_the_fence() {
        let doc = json!({
            "type": "doc",
            "content": [{ "type": "paragraph", "content": [
                { "type": "text", "text": "a``b", "marks": [{ "type": "code", "attrs": {} }] }
            ]}]
        });
        assert_eq!(prose_mirror_to_markdown(&doc).unwrap(), "``` a``b ```");
    }

    #[test]
    fn unsupported_nodes_are_visible() {
        let doc = json!({
            "type": "doc",
            "content": [{ "type": "mystery", "attrs": {} }]
        });
        assert_eq!(
            prose_mirror_to_markdown(&doc).unwrap(),
            "[unsupported mystery node]"
        );
    }

    #[test]
    fn rejects_non_document_roots() {
        let error = prose_mirror_to_markdown(&json!({ "type": "paragraph" })).unwrap_err();
        assert_eq!(error.kind, crate::errors::ErrorKind::Validation);
        assert!(error.user_message.contains("paragraph"));
    }

    #[test]
    fn rejects_malformed_nodes() {
        let error = prose_mirror_to_markdown(&json!({
            "type": "doc",
            "content": "nope"
        }))
        .unwrap_err();
        assert!(error.user_message.contains("content"));
    }
}
