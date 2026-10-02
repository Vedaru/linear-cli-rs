//! Adapter conformance: one suite, every preset.
//!
//! `presets.rs` checks each preset against the payload shapes its platform really sends.
//! This file checks what must hold for *every* adapter, by running the same code over each
//! one - so an adapter added by configuration is held to the same bar as one that shipped.
//!
//! There is no platform-specific code here, and that is the point. Each platform brings a
//! fixture in `presets/fixtures/<name>.toml` - the body it would really send, plus the
//! headers that are not the proof - and the harness signs it using the scheme the *preset*
//! declares. Adding a platform is a preset and a fixture: two files, no Rust.
//!
//! The suite is written against the declared scheme rather than a list of known platforms:
//! a preset that says it verifies an HMAC over the body is held to that, and one that says
//! it checks a shared token is held to *that* instead. Claiming one scheme and enforcing
//! another is the failure this exists to catch - and where a scheme genuinely cannot offer
//! a guarantee (a token says nothing about the body it arrives with), the suite asserts
//! that limitation instead of pretending it away.

mod support;

use support::{
    conformance_fixtures, fixture_for, proof_header, Fixture, OTHER_SECRET, TEST_SECRET,
};

use linear_bridge::connector::{Algorithm, HeaderMap, Reject, Source};
use linear_bridge::domain::Secret;
use linear_bridge::sources::declarative::DeclarativeSource;
use linear_bridge::sources::presets;

fn source(name: &str) -> DeclarativeSource {
    DeclarativeSource::new(
        name,
        Secret::new(TEST_SECRET),
        presets::preset(name).expect("the preset loads"),
    )
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// The headers a delivery carries: its own, plus the proof the preset's scheme calls for.
fn delivery_headers(
    source: &DeclarativeSource,
    fixture: &Fixture,
    secret: &str,
    body: &[u8],
) -> HeaderMap {
    let mut pairs: Vec<(String, String)> = fixture
        .headers
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let (name, value) = proof_header(source, secret, body);
    pairs.push((name, value));
    HeaderMap::from_pairs(
        pairs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    )
}

/// The headers the scheme names, as the platform spells them.
fn declared_headers(source: &DeclarativeSource) -> Vec<String> {
    source
        .signature()
        .headers
        .iter()
        .map(|header| header.to_ascii_lowercase())
        .collect()
}

#[test]
fn every_preset_in_the_build_has_a_fixture() {
    // The rule that keeps this suite self-extending: it runs over the presets the build
    // ships, and a preset without a fixture fails with the file to add rather than quietly
    // being held to nothing.
    for name in presets::preset_names() {
        let fixture = fixture_for(name);
        assert!(
            !fixture.body.trim().is_empty(),
            "{name}: its fixture carries no delivery"
        );
    }
}

#[test]
fn every_adapter_accepts_a_delivery_it_would_have_signed() {
    for (name, fixture) in conformance_fixtures() {
        let source = source(&name);
        let body = fixture.body_at(now_millis());

        source
            .authenticate(
                &delivery_headers(&source, &fixture, TEST_SECRET, &body),
                &body,
            )
            .unwrap_or_else(|reject| panic!("{name} refused its own delivery: {reject}"));

        // And it is a delivery this preset actually models: a verified payload that no
        // rule recognises is a delivery the platform sends and the bridge drops.
        let events = source
            .parse(
                &delivery_headers(&source, &fixture, TEST_SECRET, &body),
                &body,
            )
            .unwrap_or_else(|reject| panic!("{name} could not read its own delivery: {reject}"));
        assert!(
            !events.is_empty(),
            "{name}: a delivery it verifies must become an event"
        );
    }
}

#[test]
fn every_adapter_refuses_a_delivery_signed_with_another_secret() {
    for (name, fixture) in conformance_fixtures() {
        let source = source(&name);
        let body = fixture.body_at(now_millis());
        let headers = delivery_headers(&source, &fixture, OTHER_SECRET, &body);

        let rejected = source
            .authenticate(&headers, &body)
            .expect_err(&format!("{name} accepted a foreign secret"));
        assert_eq!(
            rejected,
            Reject::BadSignature,
            "{name}: a foreign secret is a bad signature, not {rejected:?}"
        );
    }
}

#[test]
fn every_adapter_refuses_a_delivery_with_no_proof_at_all() {
    for (name, fixture) in conformance_fixtures() {
        let source = source(&name);
        let body = fixture.body_at(now_millis());

        // Every header the scheme names, not just the first: a platform that accepts
        // either form has *two* ways to prove itself, so removing the proof means
        // removing both.
        let declared = declared_headers(&source);
        let pairs: Vec<(String, String)> = fixture
            .headers
            .iter()
            .filter(|(header, _)| !declared.contains(&header.to_ascii_lowercase()))
            .map(|(header, value)| (header.clone(), value.clone()))
            .collect();
        let headers = HeaderMap::from_pairs(
            pairs
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        );

        let rejected = source
            .authenticate(&headers, &body)
            .expect_err(&format!("{name} accepted a delivery with no signature"));
        assert!(
            matches!(rejected, Reject::MissingHeader(_)),
            "{name}: {rejected:?} should be reported as a missing header"
        );
    }
}

#[test]
fn a_body_signature_covers_the_body_and_a_token_does_not() {
    for (name, fixture) in conformance_fixtures() {
        let source = source(&name);
        let original = fixture.body_at(now_millis());
        // The proof is over the *original* body, and only then is the body changed: a
        // signature over the changed bytes would of course verify, which is the mistake
        // this ordering exists to prevent.
        let headers = delivery_headers(&source, &fixture, TEST_SECRET, &original);
        let mut body = original;
        // One byte, in the middle: the smallest change a delivery could suffer.
        let middle = body.len() / 2;
        body[middle] ^= 0x01;

        match source.signature().algorithm {
            Algorithm::HmacSha256 => {
                let rejected = source.authenticate(&headers, &body).expect_err(&format!(
                    "{name}: an HMAC over the body must not survive a changed body"
                ));
                assert_eq!(rejected, Reject::BadSignature, "{name}");
            }
            Algorithm::Token => {
                // A shared token travels *with* the body and says nothing about it, so
                // this is a real limitation rather than a bug - and the suite states it
                // out loud, so nobody reads a green run as "tamper-proof".
                source
                    .authenticate(&headers, &body)
                    .unwrap_or_else(|reject| {
                        panic!(
                            "{name}: a token scheme cannot detect a changed body, so this should \
                         still verify (and the platform must be trusted for it): {reject}"
                        )
                    });
            }
        }
    }
}

#[test]
fn every_adapter_refuses_a_stale_delivery_where_its_payload_carries_time() {
    for (name, fixture) in conformance_fixtures()
        .into_iter()
        .filter(|(_, f)| f.binds_time())
    {
        let source = source(&name);

        // An hour old, and signed correctly *for that time*: the signature is valid and
        // the delivery is not. The intake runs both halves - verify the sender, then apply
        // time - so the refusal has to come out of that pipeline, and this is a statement
        // about where each half lives: the signature says who sent it, the freshness
        // window says when.
        let body = fixture.body_at(now_millis() - 3_600_000);
        let headers = delivery_headers(&source, &fixture, TEST_SECRET, &body);
        source.authenticate(&headers, &body).unwrap_or_else(|reject| {
            panic!("{name}: the signature covers the delivery's own time, so it is valid: {reject}")
        });

        let rejected = source
            .parse(&headers, &body)
            .expect_err(&format!("{name} accepted an hour-old delivery"));
        assert_eq!(
            rejected,
            Reject::Stale,
            "{name}: {rejected:?} is not what staleness looks like"
        );
    }
}

#[test]
fn every_preset_names_what_it_needs_to_be_configured() {
    for (name, _) in conformance_fixtures() {
        let spec = presets::preset(&name).expect("the preset loads");
        assert!(
            !spec.event.rules.is_empty(),
            "{name}: a preset that recognises no event types would verify every delivery \
             and then drop all of them"
        );
        assert!(
            !spec.signature.headers.is_empty(),
            "{name}: every scheme names the header it reads the proof from"
        );
        let source = source(&name);
        assert_eq!(
            spec.signature.headers,
            source.signature().headers,
            "{name}: the source enforces the scheme the preset declares"
        );
        // The name a config writes in `type` is the name the preset is looked up by, so
        // the fixtures on disk are the list of types a deployment can actually use.
        assert_eq!(source.id().as_str(), name, "{name}");
    }
}
