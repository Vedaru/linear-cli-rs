//! Templates: how a platform-neutral field set becomes a platform's request body,
//! and how that platform's response becomes neutral fields again.
//!
//! Both directions are *data* (see [`crate::sink::spec`]); this module is the
//! pure function underneath them, with no I/O at all, which is why the whole
//! mapping can be tested without a network or a platform.
//!
//! The template language is deliberately tiny. In a request body written as JSON:
//!
//! - a string that does not start with `$` is a literal;
//! - `"$name"` is replaced by the value of the directive `name`;
//! - `"$name!"` is replaced by that value, or by `null` when it is absent - the
//!   explicit "clear this field" a platform needs to distinguish from "leave it
//!   alone";
//! - a directive with no value and no `!` **drops its key** (or its element, in an
//!   array): sending `"due_date": null` when there is no due date is how a mirror
//!   accidentally clears a field the user never touched;
//! - a directive whose value is present but `null` is written as `null`. That is a
//!   deliberate distinction, and the reason a partial update works: the engine puts
//!   a `null` there only when it means "clear this", while a field it has nothing
//!   to say about is left out of the values altogether.
//!
//! Objects and arrays recurse, so a template can build nested request bodies and
//! GraphQL `variables` blocks with the same rules.

use serde_json::{Map, Value};

/// Substitute every directive in `template` using `values`.
pub fn render(template: &Value, values: &Value) -> Value {
    match template {
        Value::String(text) => render_string(text, values).unwrap_or(Value::Null),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .filter_map(|item| {
                    // An element that resolves to nothing is dropped rather than sent as
                    // null: an array of ids must not contain a hole.
                    match item {
                        Value::String(text) => render_string(text, values),
                        other => Some(render(other, values)),
                    }
                })
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                match value {
                    Value::String(text) => {
                        if let Some(rendered) = render_string(text, values) {
                            out.insert(key.clone(), rendered);
                        }
                        // Absent without `!`: the key is omitted entirely.
                    }
                    other => {
                        out.insert(key.clone(), render(other, values));
                    }
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// One component rendered on its own, for the places that are not a JSON tree - a
/// query parameter, for instance.
pub fn render_component(text: &str, values: &Value) -> Option<String> {
    match render_string(text, values)? {
        Value::String(rendered) => Some(rendered),
        other => Some(other.to_string()),
    }
}

/// One string: a literal, or a directive that resolved (or did not).
fn render_string(text: &str, values: &Value) -> Option<Value> {
    let Some(name) = text.strip_prefix('$') else {
        return Some(Value::String(text.to_string()));
    };
    let (name, explicit_null) = match name.strip_suffix('!') {
        Some(name) => (name, true),
        None => (name, false),
    };
    match values.get(name) {
        // Present and null: the engine means "clear this field", so it is written.
        Some(Value::Null) => Some(Value::Null),
        // Absent: nothing to say. `!` still spells that out as an explicit null,
        // which is what a *create* wants (there is no earlier value to leave alone).
        None => explicit_null.then_some(Value::Null),
        Some(value) => Some(value.clone()),
    }
}

/// The directives a template may use, as bare names. Kept in one place so a typo
/// in a preset is a validation error rather than a silent omission at runtime.
///
/// These are exactly the neutral field model: a platform that needs something
/// else (a project id, a milestone) needs a directive, and adding one is a
/// deliberate change to the vocabulary rather than a string in a preset.
pub const DIRECTIVES: &[&str] = &[
    "title",
    "body",
    "labels",
    "label_ids",
    // One milestone id, resolved from its name the same way the label ids above are.
    "milestone_id",
    // A label's colour, which is only ever named when a lookup has to create one.
    "color",
    "priority",
    "due_date",
    "assignee",
    "assignees",
    "assignee_id",
    "state",
    "state_id",
    // A card's column on a board: the name the mapping gave it, and the id the
    // platform wants. The second is the only directive resolved against a *project*
    // rather than a scope, because a board's columns belong to it.
    "column",
    "column_id",
    "scope",
    "scope_id",
    "id",
    "url",
    "name",
    // Pagination, for a `[sink.issue.list]` request: whichever of the two this
    // platform pages with, and never both.
    "page",
    "cursor",
];

/// The directive a string names, if it is a directive at all: `$name` or
/// `$name!`, with the `!` stripped. The suffix is generic - it means "send null
/// rather than dropping the key" for any directive - so it is not part of the
/// vocabulary.
pub fn directive_name(text: &str) -> Option<&str> {
    let name = text.strip_prefix('$')?;
    let name = name.strip_suffix('!').unwrap_or(name);
    (!name.is_empty()).then_some(name)
}

/// Check one rendered component - a query value, say - which is not part of a JSON
/// tree and so cannot be walked by [`validate`].
pub fn validate_component(text: &str) -> Result<(), String> {
    if !text.starts_with('$') {
        return Ok(());
    }
    match directive_name(text) {
        Some(name) if DIRECTIVES.contains(&name) => Ok(()),
        Some(name) => Err(format!("`${name}` is not a known directive")),
        None => Err("`$` is not a directive".to_string()),
    }
}

/// Check a template against [`DIRECTIVES`] and report the first unknown one.
///
/// A preset is validated at load for exactly this reason: an unknown directive
/// would otherwise render as a dropped key, and a bridge that silently stops
/// sending a field looks like a platform bug for weeks.
pub fn validate(template: &Value) -> Result<(), String> {
    match template {
        Value::String(text) if text.starts_with('$') => match directive_name(text) {
            Some(name) if DIRECTIVES.contains(&name) => Ok(()),
            Some(name) => Err(format!(
                "`${name}` is not a known directive (known: {})",
                DIRECTIVES
                    .iter()
                    .map(|name| format!("${name}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            None => Err("`$` is not a directive".to_string()),
        },
        Value::Array(items) => items.iter().try_for_each(validate),
        Value::Object(map) => map.values().try_for_each(validate),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values() -> Value {
        json!({
            "title": "Fix the thing",
            "body": "why",
            "labels": ["bug", "urgent"],
            "label_ids": [3, 7],
            "due_date": "2026-10-02",
            "state": "closed",
            "scope": "a/b",
            "id": "7",
        })
    }

    #[test]
    fn literals_and_directives_mix() {
        let template = json!({ "title": "$title", "kind": "issue", "nested": { "text": "$body" } });
        assert_eq!(
            render(&template, &values()),
            json!({ "title": "Fix the thing", "kind": "issue", "nested": { "text": "why" } })
        );
    }

    #[test]
    fn an_absent_directive_drops_its_key() {
        // `assignee` is not in `values`. Sending it as null would clear the
        // assignee on the far side - the exact bug this rule exists to prevent.
        let template = json!({ "title": "$title", "assignee": "$assignee" });
        assert_eq!(
            render(&template, &values()),
            json!({ "title": "Fix the thing" })
        );
    }

    #[test]
    fn an_explicit_null_directive_sends_null() {
        let template = json!({ "title": "$title", "assignee": "$assignee!" });
        assert_eq!(
            render(&template, &values()),
            json!({ "title": "Fix the thing", "assignee": null })
        );
    }

    #[test]
    fn arrays_keep_their_shape_and_drop_unresolved_elements() {
        let template = json!({ "labels": "$label_ids", "notes": ["$body", "$assignee", "fixed"] });
        assert_eq!(
            render(&template, &values()),
            json!({ "labels": [3, 7], "notes": ["why", "fixed"] })
        );
    }

    #[test]
    fn a_present_null_is_written_and_an_absent_value_is_dropped() {
        // Present and null: the engine is saying "clear this". A partial update is
        // built from these, so dropping it would make a cleared field unsyncable.
        let present = json!({ "body": null });
        assert_eq!(
            render(&json!({ "body": "$body" }), &present),
            json!({ "body": null })
        );

        // Absent: nothing to say about this field, so it is left out of the request
        // entirely - which is what keeps a patch from restating what it did not
        // change. `!` spells the same absence out as an explicit null, for the
        // creates where there is no earlier value to leave alone.
        let absent = json!({ "other": 1 });
        assert_eq!(render(&json!({ "body": "$body" }), &absent), json!({}));
        assert_eq!(
            render(&json!({ "body": "$body!" }), &absent),
            json!({ "body": null })
        );
    }

    #[test]
    fn a_graphql_variables_block_renders_with_the_same_rules() {
        let template = json!({
            "query": "mutation ($input: IssueCreateInput!) { issueCreate(input: $input) { success } }",
            "variables": { "input": { "teamId": "$scope_id", "title": "$title", "labelIds": "$label_ids" } }
        });
        let values = json!({ "scope_id": "uuid-1", "title": "T", "label_ids": [1] });
        assert_eq!(
            render(&template, &values),
            json!({
                "query": "mutation ($input: IssueCreateInput!) { issueCreate(input: $input) { success } }",
                "variables": { "input": { "teamId": "uuid-1", "title": "T", "labelIds": [1] } }
            })
        );
    }

    #[test]
    fn a_whole_body_may_be_a_single_directive() {
        // `issueUpdate(input: { ... })` style bodies pass the field set through.
        assert_eq!(
            render(&json!("$labels"), &values()),
            json!(["bug", "urgent"])
        );
        assert_eq!(render(&json!("$missing"), &values()), Value::Null);
    }

    #[test]
    fn validation_names_the_offending_directive() {
        assert!(validate(&json!({ "a": "$title" })).is_ok());
        let error = validate(&json!({ "a": "$titel" })).unwrap_err();
        assert!(error.contains("$titel"), "{error}");
        assert!(error.contains("$title"), "{error}");
        assert!(validate(&json!({ "deep": [{ "x": "$label_ids" }] })).is_ok());
        // The `!` suffix is generic, so every directive has both forms without
        // appearing twice in the vocabulary.
        assert!(validate(&json!({ "a": "$priority!" })).is_ok());
        assert!(validate(&json!({ "a": "$assignees" })).is_ok());
        assert!(validate(&json!({ "a": "$" })).is_err());
        assert!(validate(&json!({ "a": "$titel!" })).is_err());
    }
}
