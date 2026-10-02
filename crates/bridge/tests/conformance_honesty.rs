//! Capability honesty: a preset must back every claim it makes.
//!
//! A preset *claims* what it can carry (`[capabilities]`), and the engine trusts that
//! claim - it decides whether a field is compared, sent, or deliberately left alone. Most
//! of the claim is hand-written, so a preset can say `due_dates = true` and then send
//! nothing that carries one. The symptom is the worst kind: not an error, but a field that
//! never travels while the plan says it will.
//!
//! So every claim needs a *witness* in the same file: a directive the engine would really
//! substitute into a request. Nothing here knows one platform from another - it walks the
//! spec by its declared shape, so an adapter added as configuration is checked by the same
//! code as the ones that shipped. The last test is the point of the exercise: take a
//! preset, leave its claim alone, remove what backs it, and this suite must fail.

use linear_bridge::sink::spec::{CommentSpec, IssueSpec, Operation, SinkSpec};
use linear_bridge::sources::declarative::{RuleSpec, SourceSpec};
use linear_bridge::sources::presets;
use serde_json::Value;

/// Every directive the engine would substitute from an operation's body, by field name.
///
/// Walks the JSON an operation sends and keeps the strings that start with `$`. That is
/// also what keeps GraphQL variables out of it: a Linear query is one long string which
/// contains `$id` and `$teamId` but never *starts* with `$`.
fn directives_in(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            let trimmed = text.trim_start();
            if let Some(rest) = trimmed.strip_prefix('$') {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                out.push(name);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| directives_in(item, out)),
        Value::Object(map) => map.values().for_each(|item| directives_in(item, out)),
        _ => {}
    }
}

fn from_operation(operation: &Operation, out: &mut Vec<String>) {
    if let Some(body) = &operation.body {
        directives_in(body, out);
    }
}

fn from_comments(comment: &CommentSpec, out: &mut Vec<String>) {
    from_operation(&comment.create, out);
    if let Some(operation) = &comment.update {
        from_operation(operation, out);
    }
    if let Some(operation) = &comment.delete {
        from_operation(operation, out);
    }
}

/// Every body an operation would send, mutably - the same set `writable_fields` reads, so
/// a doctored preset cannot be doctored in one operation and still backed by another.
fn operation_bodies(sink: &mut SinkSpec) -> Vec<&mut Value> {
    let issue = &mut sink.issue;
    let mut operations: Vec<&mut Operation> = Vec::new();
    for operation in [
        &mut issue.create,
        &mut issue.update,
        &mut issue.delete,
        &mut issue.attach,
        &mut issue.transition,
        &mut issue.labels,
    ]
    .into_iter()
    .flatten()
    {
        operations.push(operation);
    }
    if let Some(comment) = &mut issue.comment {
        operations.push(&mut comment.create);
        if let Some(operation) = &mut comment.update {
            operations.push(operation);
        }
        if let Some(operation) = &mut comment.delete {
            operations.push(operation);
        }
    }
    for lookup in issue.lookup.values_mut() {
        operations.push(&mut lookup.list);
        if let Some(operation) = &mut lookup.create {
            operations.push(operation);
        }
    }
    operations
        .into_iter()
        .filter_map(|operation| operation.body.as_mut())
        .collect()
}

/// Every field this platform's write half can actually carry, as the engine sees it - the
/// operations a preset declares, and the directives their bodies substitute.
fn writable_fields(sink: &SinkSpec) -> Vec<String> {
    let issue: &IssueSpec = &sink.issue;
    let mut out = Vec::new();

    for operation in [
        &issue.create,
        &issue.update,
        &issue.delete,
        &issue.attach,
        &issue.transition,
        &issue.labels,
    ]
    .into_iter()
    .flatten()
    {
        from_operation(operation, &mut out);
    }
    if let Some(comment) = &issue.comment {
        from_comments(comment, &mut out);
    }
    for lookup in issue.lookup.values() {
        from_operation(&lookup.list, &mut out);
        if let Some(create) = &lookup.create {
            from_operation(create, &mut out);
        }
    }
    out
}

/// Whether the preset recognises a delivery that *is* a pull request, merge request or
/// push - the events a commit reference arrives on.
fn recognises_references(source: &SourceSpec) -> bool {
    fn kind(rule: &RuleSpec) -> bool {
        let event = rule.event.to_ascii_lowercase();
        event.contains("pull") || event.contains("merge") || event.contains("push")
    }
    source.event.rules.iter().any(kind)
}

fn carries(fields: &[String], field: &str) -> bool {
    fields.iter().any(|name| name.starts_with(field))
}

/// One claim, phrased as the defect it would be if nothing backed it.
fn unbacked(name: &str, claim: &str, is_claimed: bool, backed: bool) -> Option<String> {
    (is_claimed && !backed).then(|| {
        format!(
            "{name}: claims `{claim}`, but nothing in the preset carries it - a field that \
             can never travel while the plan says it will"
        )
    })
}

/// Claims that nothing in the preset backs, each phrased as the defect it is.
fn unbacked_claims(name: &str, source: &SourceSpec, sink: &SinkSpec) -> Vec<String> {
    let claimed = &source.capabilities;
    let writable = writable_fields(sink);
    let mut failures = Vec::new();

    failures.extend(unbacked(
        name,
        "labels",
        claimed.labels,
        carries(&writable, "label") || sink.issue.labels.is_some(),
    ));
    failures.extend(unbacked(
        name,
        "due_dates",
        claimed.due_dates,
        carries(&writable, "due_date"),
    ));
    failures.extend(unbacked(
        name,
        "priorities",
        claimed.priorities,
        carries(&writable, "priority"),
    ));
    failures.extend(unbacked(
        name,
        "multiple_assignees",
        claimed.multiple_assignees,
        carries(&writable, "assignee"),
    ));
    failures.extend(unbacked(
        name,
        "native_pull_requests",
        claimed.native_pull_requests,
        recognises_references(source),
    ));
    failures.extend(unbacked(
        name,
        "deletion",
        claimed.deletion,
        sink.issue.delete.is_some(),
    ));
    // The claim covers both halves - emitting and accepting - so it needs evidence on the
    // read side too: a preset that says it can observe a deletion must have a rule that
    // would recognise one, or the platform's deletion arrives and is dropped.
    failures.extend(unbacked(
        name,
        "deletion (observable)",
        claimed.deletion,
        source.event.rules.iter().any(|rule| {
            let event = rule.event.to_ascii_lowercase();
            event.contains("delet")
                || event.contains("remove")
                || rule.actions.iter().any(|(action, kind)| {
                    action.to_ascii_lowercase().contains("delet")
                        || action.to_ascii_lowercase().contains("remove")
                        || kind.to_ascii_lowercase().contains("delet")
                })
        }),
    ));

    // And the mirror of the same defect: a field the preset *reads* is a field the
    // reconciler compares, so reading one the platform can never be told about makes
    // every issue that has one differ forever. This is the bug this whole check exists
    // because of, met in the wild: an assignee no target could represent.
    let read = sink.issue.read.as_ref();
    let reads =
        |pick: fn(&linear_bridge::sink::spec::ReadSpec) -> bool| read.map(pick).unwrap_or(false);
    for (claim, field, is_read, is_claimed) in [
        (
            "due_dates",
            "due_date",
            reads(|read| read.due_date.is_some()),
            claimed.due_dates,
        ),
        (
            "priorities",
            "priority",
            reads(|read| read.priority.is_some()),
            claimed.priorities,
        ),
        (
            "labels",
            "label",
            reads(|read| read.labels.is_some()),
            claimed.labels,
        ),
    ] {
        if is_claimed && is_read && !carries(&writable, field) {
            failures.push(format!(
                "{name}: claims `{claim}`, reads it so it takes part in every comparison, \
                 but no operation can write it - the difference can never be resolved"
            ));
        }
    }

    failures
}

#[test]
fn every_preset_backs_every_capability_it_claims() {
    // Driven by the presets the build ships, so an adapter that arrives as configuration
    // is checked by the same code as the ones that shipped - no per-platform test code is
    // involved in the answer.
    for name in presets::preset_names() {
        let spec = presets::preset(name).expect("the preset loads");
        let Some(sink) = spec.sink.clone() else {
            // No write half: this preset receives deliveries and sends nothing, so its
            // capabilities describe what it can read, and there is no write claim to
            // contradict. Saying so beats skipping it silently.
            continue;
        };
        let failures = unbacked_claims(name, &spec, &sink);
        assert!(failures.is_empty(), "{}", failures.join("\n  "));
    }
}

#[test]
fn a_preset_that_claims_a_field_it_never_sends_fails_this_suite() {
    // VED-29's acceptance clause, written as the defect it describes: an adapter that
    // claims due-date support and drops it. Leaving the claim alone and removing what
    // backs it is what "drops it" looks like in a preset.
    // No platform named: the first preset that ships a fixture *and* claims due dates is
    // the one to doctor, so this test cannot rot into naming a platform that changed.
    let name = presets::preset_names()
        .into_iter()
        .find(|name| {
            presets::preset(name)
                .map(|spec| spec.capabilities.due_dates && spec.sink.is_some())
                .unwrap_or(false)
        })
        .expect("some preset claims due dates, or this check has nothing to prove");
    let mut spec = presets::preset(name).expect("the preset loads");
    let mut sink = spec.sink.clone().expect("it writes");

    // Removing what backs the claim, leaving the claim: exactly what "claims due-date
    // support and drops it" looks like. Every operation, because a preset that carries a
    // field in its create *and* its update is still honest if one of them is stripped -
    // the claim would be backed, and this test would pass for the wrong reason.
    for body in operation_bodies(&mut sink) {
        *body = replace_strings(body, "$due_date", "$title");
    }
    spec.sink = Some(sink.clone());

    assert!(
        spec.capabilities.due_dates,
        "the fixture has to keep claiming it, or this proves nothing"
    );
    let failures = unbacked_claims(&format!("{name} (doctored)"), &spec, &sink);
    assert!(
        failures.iter().any(|failure| failure.contains("due_dates")),
        "a claim with nothing behind it must fail the suite, got: {failures:?}"
    );
    assert_eq!(
        failures.len(),
        2,
        "the doctored field is both claimed-and-unbacked and read-without-a-write, so the \
         failure should name both: {failures:?}"
    );
    assert!(
        failures.iter().all(|failure| failure.contains("due_dates")),
        "nothing but the doctored claim should have failed, so the message stays specific: \
         {failures:?}"
    );
}

/// A copy of a JSON value with one string swapped for another, at any depth.
fn replace_strings(value: &Value, from: &str, to: &str) -> Value {
    match value {
        Value::String(text) if text == from => Value::String(to.to_string()),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| replace_strings(item, from, to))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), replace_strings(item, from, to)))
                .collect(),
        ),
        other => other.clone(),
    }
}
