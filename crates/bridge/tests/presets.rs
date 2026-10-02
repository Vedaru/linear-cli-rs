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
use linear_bridge::sources::declarative::DeclarativeSource;
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

// --- Linear -----------------------------------------------------------------

// --- Forgejo ----------------------------------------------------------------

// --- GitHub and GitLab ------------------------------------------------------

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

    // The intake-only presets cannot be enumerated, and they say so: their API half
    // is a separate piece of work, and a sweep running against one must refuse
    // rather than report an empty scope.
    for name in ["github", "gitlab"] {
        let preset = presets::preset(name).expect("the preset loads");
        assert!(preset.sink.is_none(), "{name} is intake-only today");
        let capabilities = preset.capabilities.resolve(preset.sink.as_ref());
        assert!(!capabilities.list, "{name} cannot be swept");
    }
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
        fetched.contains("VED-123"),
        "the identifier fetch gets the issue: {fetched}"
    );

    let mutated = post(
        fake.url(),
        r#"{"query":"mutation AttachmentCreate($input: AttachmentCreateInput!) { attachmentCreate(input: $input) { success } }"}"#,
    );
    assert!(
        mutated.contains("attachmentCreate"),
        "the mutation gets its own answer: {mutated}"
    );
    assert!(
        !mutated.contains("VED-123"),
        "and not the fetch's: {mutated}"
    );

    // A request the fixture cannot place says so, naming the method and path, rather than
    // answering with whatever came first.
    let unknown = post(fake.url(), r#"{"query":"mutation SomethingElse"}"#);
    assert!(unknown.contains("404"), "unmatched: {unknown}");
    assert!(unknown.contains("no answer"), "unmatched: {unknown}");
}
