//! JSON Pointer (RFC 6901), the only addressing language the declarative
//! connector understands.
//!
//! A platform's payload shape is *data*: it lives in a preset file, not in a Rust
//! module. That needs one small, well-specified way to name a field inside a JSON
//! body, and JSON Pointer is it - a standard, with escaping rules that are easy to
//! get subtly wrong, which is exactly why it lives alone in a file with tests.

use serde_json::Value;

/// Resolve a JSON Pointer against a document.
///
/// `""` (the empty pointer) is the whole document. `/a/b` walks into objects;
/// `/a/0` indexes arrays; `~1` and `~0` are the escapes for `/` and `~`. Every
/// failure is `None` rather than an error: a missing field is the normal case for
/// an optional payload field, and the caller decides what that means.
pub fn resolve<'a>(document: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer.is_empty() {
        return Some(document);
    }
    let path = pointer.strip_prefix('/')?;
    let mut current = document;
    for raw in path.split('/') {
        let token = unescape(raw);
        current = match current {
            Value::Object(map) => map.get(token.as_ref())?,
            Value::Array(items) => items.get(token.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Resolve and render as a string, the form every extracted id or scope is used
/// in. Numbers and booleans are rendered by their JSON form so a payload with
/// `"number": 7` and one with `"number": "7"` address the same entity.
pub fn resolve_string(document: &Value, pointer: &str) -> Option<String> {
    match resolve(document, pointer)? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Resolve several pointers and join the non-empty results, in order. Used for a
/// reference's text, which some platforms split across a title and a body.
pub fn resolve_joined(document: &Value, pointers: &[String]) -> Option<String> {
    let parts: Vec<String> = pointers
        .iter()
        .filter_map(|pointer| resolve_string(document, pointer))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// RFC 6901 token unescaping: `~1` is `/`, `~0` is `~`, and the order matters
/// (`~01` is `~1`, not `/`), which is why this is not a `replace` chain.
fn unescape(token: &str) -> std::borrow::Cow<'_, str> {
    if !token.contains('~') {
        return std::borrow::Cow::Borrowed(token);
    }
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(character) = chars.next() {
        if character != '~' {
            out.push(character);
            continue;
        }
        match chars.next() {
            Some('0') => out.push('~'),
            Some('1') => out.push('/'),
            // An invalid escape is kept verbatim rather than dropped: silently
            // changing a key would look like a payload mismatch.
            other => {
                out.push('~');
                if let Some(character) = other {
                    out.push(character);
                }
            }
        }
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document() -> Value {
        json!({
            "action": "opened",
            "data": { "id": "abc", "team": { "key": "VED" }, "number": 7, "flag": true },
            "commits": [ { "id": "c1", "message": "fixes VED-1" } ],
            "weird/key": { "a~b": "escaped" },
            "empty": "",
            "null": null
        })
    }

    #[test]
    fn walks_objects_and_arrays() {
        let doc = document();
        assert_eq!(resolve_string(&doc, "/action").as_deref(), Some("opened"));
        assert_eq!(
            resolve_string(&doc, "/data/team/key").as_deref(),
            Some("VED")
        );
        assert_eq!(
            resolve_string(&doc, "/commits/0/message").as_deref(),
            Some("fixes VED-1")
        );
    }

    #[test]
    fn numbers_and_booleans_render_as_their_json_form() {
        let doc = document();
        assert_eq!(resolve_string(&doc, "/data/number").as_deref(), Some("7"));
        assert_eq!(resolve_string(&doc, "/data/flag").as_deref(), Some("true"));
    }

    #[test]
    fn the_empty_pointer_is_the_whole_document() {
        let doc = document();
        assert_eq!(resolve(&doc, ""), Some(&doc));
    }

    #[test]
    fn missing_paths_null_and_empty_strings_are_none() {
        let doc = document();
        assert_eq!(resolve(&doc, "/nope"), None);
        assert_eq!(resolve(&doc, "/null"), Some(&Value::Null));
        assert_eq!(resolve_string(&doc, "/null"), None);
        assert_eq!(resolve_string(&doc, "/empty"), None, "empty is not a value");
        assert_eq!(resolve(&doc, "/commits/9"), None);
        assert_eq!(resolve(&doc, "/commits/not-a-number"), None);
        assert_eq!(resolve(&doc, "no-leading-slash"), None);
        assert_eq!(resolve_string(&doc, "/action/deeper"), None);
    }

    #[test]
    fn escapes_follow_rfc_6901() {
        let doc = document();
        // `~1` -> `/`, `~0` -> `~`, and `~01` -> `~1` (order matters).
        assert_eq!(
            resolve_string(&doc, "/weird~1key/a~0b").as_deref(),
            Some("escaped")
        );
        assert_eq!(resolve_string(&doc, "/a~01b"), None);
    }

    #[test]
    fn joining_skips_missing_parts_and_keeps_order() {
        let doc = document();
        let joined = resolve_joined(&doc, &["/action".into(), "/nope".into(), "/data/id".into()]);
        assert_eq!(joined.as_deref(), Some("opened\n\nabc"));
        assert_eq!(resolve_joined(&doc, &["/nope".into()]), None);
    }
}
