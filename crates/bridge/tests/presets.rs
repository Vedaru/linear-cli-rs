//! Preset conformance.
//!
//! Each preset is a *description* of a platform's payloads, so the thing worth
//! testing is that the description matches real deliveries. These are the payload
//! shapes the platforms actually send, checked field by field: if a preset drifts
//! from reality, intake starts rejecting or misreading live deliveries, and this
//! is where that shows up first.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use linear_bridge::connector::Source;
use linear_bridge::domain::Secret;
use linear_bridge::sources::declarative::{DeclarativeSource, SourceSpec};
use linear_bridge::sources::presets;

mod support;

const SECRET: &str = "0123456789abcdef";

/// One HTTP POST, spoken raw: the assertion is about which answer the fixture chose, and a
/// client library would only add a way for the test to be wrong about that.
fn post(url: &str, body: &str) -> String {
    let addr = url.trim_start_matches("http://");
    let mut stream = TcpStream::connect(addr).expect("the fake is listening");
    let request = format!(
        "POST /graphql HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("write");
    let mut answer = String::new();
    stream.read_to_string(&mut answer).expect("read");
    answer
}

fn source(name: &str) -> DeclarativeSource {
    DeclarativeSource::new(
        name,
        Secret::new(SECRET),
        presets::preset(name).expect("the preset loads"),
    )
}

#[test]
fn enumeration_is_derived_from_the_sink_rather_than_declared_twice() {
    // The capability follows the operation, so the two cannot disagree - and a sweep
    // asks "can I look?" rather than discovering it on the first run.
    for name in ["linear", "forgejo"] {
        let preset = presets::preset(name).expect("the preset loads");
        let capabilities = preset.capabilities.resolve(preset.sink.as_ref());
        assert!(capabilities.list, "{name} declares a list operation");
        assert!(capabilities.describe().contains(&"list"), "{name}");
    }

    // A spec with no write half cannot be enumerated, and says so: a sweep against one must
    // refuse by name rather than report an empty scope. Taken from a shipped preset minus its
    // sink, because every platform this build ships can be written to - reading is the half
    // every platform has, writing is the half some have.
    let head: String = presets::preset_text("forgejo")
        .expect("the preset is shipped")
        .lines()
        .take_while(|line| !line.starts_with("[sink"))
        .collect::<Vec<_>>()
        .join("\n");
    let read_only = SourceSpec::from_toml(&head).expect("a source-only spec parses");
    assert!(read_only.sink.is_none(), "no write half");
    let capabilities = read_only.capabilities.resolve(read_only.sink.as_ref());
    assert!(!capabilities.list, "a spec with no sink cannot be swept");
}

#[test]
fn every_preset_is_reachable_as_a_configured_platform() {
    // The point of the presets is that a deployment selects one by name; if a
    // name in the list did not resolve, `type = "<name>"` would fail at startup.
    for name in presets::preset_names() {
        let preset = presets::preset(name).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(!preset.event.rules.is_empty(), "{name} has no rules");
    }
    // And the same engine treats a hand-written spec and a preset identically.
    let inline = DeclarativeSource::new(
        "custom",
        Secret::new(SECRET),
        presets::preset("forgejo").expect("preset"),
    );
    let built_in = source("forgejo");
    assert_eq!(
        inline.capabilities(),
        built_in.capabilities(),
        "a preset is just a spec"
    );
    assert_eq!(
        Arc::strong_count(&Arc::new(inline)),
        1,
        "the source is shareable across threads"
    );
}

#[test]
fn an_answer_is_chosen_by_what_the_body_asks_for() {
    // Linear answers every operation on one url, so a fetch and a mutation are the same
    // method and path - only the body tells them apart. Without a body match the first
    // answer configured would win, and a test would be trusting a mutation's answer for a
    // fetch without noticing. This is that distinction, held to by the fixture.
    let fake = support::Fake::start_from("linear");

    let fetched = post(
        fake.url(),
        r#"{"query":"query Issue($id: String!) { issue(id: $id) }"}"#,
    );
    assert!(
        fetched.contains("VED-2"),
        "the identifier fetch gets the issue the fixture declares: {fetched}"
    );

    let mutated = post(
        fake.url(),
        r#"{"query":"mutation AttachmentCreate($input: AttachmentCreateInput!) { attachmentCreate(input: $input) { success } }"}"#,
    );
    assert!(
        mutated.contains("attachmentCreate"),
        "the mutation gets its own answer: {mutated}"
    );
    assert!(!mutated.contains("VED-2"), "and not the fetch's: {mutated}");

    // A request the fixture cannot place says so, naming the method and path, rather than
    // answering with whatever came first.
    let unknown = post(fake.url(), r#"{"query":"mutation SomethingElse"}"#);
    assert!(unknown.contains("404"), "unmatched: {unknown}");
    assert!(unknown.contains("no answer"), "unmatched: {unknown}");
}

#[test]
fn an_id_position_takes_an_id_variable() {
    // A preset's GraphQL is validated by the platform and by nothing here: the engine's checks are
    // structural, the fixtures answer whatever shape a test asks for, and the end-to-end harnesses
    // talk to a fake. So a wrong *variable type* - not a wrong field, which the fixtures do catch -
    // reaches a release and fails on the first sweep. It shipped once: the `list` operation
    // declared `$teamId: String!` for a value Linear reads in an `ID` position, and Linear rejects
    // such a document for *every* value:
    //
    //   Variable "$teamId" of type "String!" used in position expecting type "ID".
    //
    // The broad check is `graphql-core` over the live SDL, which validates every document in the
    // tree - mutations included, statically - but needs a schema, so it cannot run in this suite.
    // This is the narrow tripwire for the trap that actually shipped.
    for name in presets::preset_names() {
        let text = presets::preset_text(name).expect("the preset is shipped");
        for (at, query) in documents(text) {
            for variable in id_position_variables(&query) {
                let declared = declaration(&query, &variable);
                assert!(
                    matches!(declared.as_deref(), Some("ID") | Some("ID!")),
                    "{name} {at}: ${variable} is compared as an id but declared {}\n\
                     Linear validates the document before it reads any value, so this fails for \
                     every value.\n{query}",
                    declared.as_deref().unwrap_or("nothing"),
                );
            }
        }
    }
}

/// Every `query = "..."` in a preset, with the path of the table it came from.
fn documents(text: &str) -> Vec<(String, String)> {
    // `toml::Value`'s own `FromStr` parses a *value*; a preset is a document, so it goes through
    // the deserialiser - which is also what reports a malformed preset by name.
    let parsed: toml::Value = toml::from_str(text).expect("the preset is valid TOML");
    let mut found = Vec::new();
    collect_documents(&parsed, "", &mut found);
    found
}

fn collect_documents(value: &toml::Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        toml::Value::Table(table) => {
            if let Some(toml::Value::String(query)) = table.get("query") {
                out.push((path.to_string(), query.clone()));
            }
            for (key, child) in table {
                collect_documents(child, &format!("{path}.{key}"), out);
            }
        }
        toml::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_documents(child, &format!("{path}[{index}]"), out);
            }
        }
        _ => {}
    }
}

/// The variables a document compares as an id: `id: { eq: $x }`, `id: { in: [$x] }`.
fn id_position_variables(query: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for marker in ["id: { eq: $", "id: { in: [$"] {
        let mut rest = query;
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// The type a document declares for `$name`, if it declares one at all.
fn declaration(query: &str, name: &str) -> Option<String> {
    let marker = format!("${name}:");
    let rest = query[query.find(&marker)? + marker.len()..].trim_start();
    let declared: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '!' | '[' | ']'))
        .collect();
    (!declared.is_empty()).then_some(declared)
}
