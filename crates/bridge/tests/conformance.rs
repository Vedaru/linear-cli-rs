//! Adapter conformance: one suite, every preset.
//!
//! `presets.rs` checks each preset against the payload shapes its platform really sends.
//! This file checks what must hold for *every* adapter, in the same way, by running the
//! same code over each one. An adapter added by configuration is then held to the same bar
//! as one that shipped, and a new arrival cannot quietly be held to a lower one.
//!
//! It is written against the scheme a preset *declares*, not against a list of known
//! platforms: a preset that says it verifies an HMAC over the body is held to that, and one
//! that says it checks a shared token is held to *that* instead. Claiming one scheme and
//! enforcing another is the failure this exists to catch - so where a scheme genuinely
//! cannot offer a guarantee (a token says nothing about the body it arrives with), the
//! suite asserts that limitation instead of pretending it away.

use hmac::{Hmac, Mac};
use sha2::Sha256;

use linear_bridge::connector::{Algorithm, HeaderMap, Reject, Source};
use linear_bridge::domain::Secret;
use linear_bridge::sources::declarative::DeclarativeSource;
use linear_bridge::sources::presets;

const SECRET: &str = "0123456789abcdef";
const OTHER_SECRET: &str = "fedcba9876543210";

fn source(name: &str) -> DeclarativeSource {
    DeclarativeSource::new(
        name,
        Secret::new(SECRET),
        presets::preset(name).expect("the preset loads"),
    )
}

fn hmac_hex(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// A delivery a platform would send, and the headers it would carry.
struct Delivery {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// One adapter's contribution to the suite: a delivery of a kind it models, built with a
/// secret *given* to it - so the same builder can produce the version signed with the
/// wrong one.
struct Case {
    name: &'static str,
    /// Built with a secret *and* the time it should carry: the version signed with the
    /// wrong secret and the version that is an hour old are the same fixture, so a stale
    /// case cannot quietly differ from a fresh one in some other way too.
    delivery: fn(&str, i64) -> Delivery,
    /// Whether the platform's payload carries its own timestamp. Where it does, a stale
    /// delivery is rejectable and this suite requires it to be rejected; where it does
    /// not, the scheme cannot tell and no preset may pretend otherwise.
    binds_time: bool,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "linear",
            binds_time: true,
            delivery: |secret, at| {
                let body = format!(
                    r#"{{"action":"create","type":"Issue","webhookTimestamp":{},"data":{{"id":"issue-uuid","identifier":"VED-1","team":{{"key":"VED"}}}}}}"#,
                    at
                )
                .into_bytes();
                let signature = hmac_hex(secret, &body);
                Delivery {
                    headers: vec![
                        ("Linear-Signature".into(), signature),
                        ("Linear-Delivery".into(), "d-1".into()),
                        ("Linear-Event".into(), "Issue".into()),
                    ],
                    body,
                }
            },
        },
        Case {
            name: "forgejo",
            binds_time: false,
            delivery: |secret, _at| {
                let body = br#"{"action":"opened","number":7,"issue":{"id":7,"number":7,"title":"T","body":"B"},"repository":{"full_name":"Vedaru/linear-cli-rs"}}"#.to_vec();
                let signature = hmac_hex(secret, &body);
                Delivery {
                    headers: vec![
                        ("X-Gitea-Signature".into(), signature),
                        ("X-Gitea-Event".into(), "issues".into()),
                    ],
                    body,
                }
            },
        },
        Case {
            name: "github",
            binds_time: false,
            delivery: |secret, _at| {
                let body = br#"{"action":"opened","repository":{"full_name":"o/r"},"sender":{"login":"vedaru"},"issue":{"number":12,"html_url":"https://github.com/o/r/issues/12"}}"#.to_vec();
                let signature = format!("sha256={}", hmac_hex(secret, &body));
                Delivery {
                    headers: vec![
                        ("X-Hub-Signature-256".into(), signature),
                        ("X-GitHub-Event".into(), "issues".into()),
                    ],
                    body,
                }
            },
        },
        Case {
            name: "gitlab",
            binds_time: false,
            delivery: |secret, _at| {
                let body = br#"{"object_attributes":{"action":"open","iid":9,"url":"https://gitlab/x/-/issues/9"},"project":{"path_with_namespace":"group/project"},"user":{"username":"vedaru"}}"#.to_vec();
                Delivery {
                    headers: vec![
                        ("X-Gitlab-Token".into(), secret.to_string()),
                        ("X-Gitlab-Event".into(), "Issue Hook".into()),
                    ],
                    body,
                }
            },
        },
    ]
}

fn verify(source: &DeclarativeSource, delivery: &Delivery) -> Result<(), Reject> {
    let pairs: Vec<(&str, &str)> = delivery
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    source.authenticate(&HeaderMap::from_pairs(pairs), &delivery.body)
}

#[test]
fn every_adapter_accepts_a_delivery_it_would_have_signed() {
    for case in cases() {
        let source = source(case.name);
        let delivery = (case.delivery)(SECRET, now_millis());

        verify(&source, &delivery)
            .unwrap_or_else(|reject| panic!("{} refused its own delivery: {reject}", case.name));

        // And it is a delivery this preset actually models: a verified payload that no
        // rule recognises is a delivery the platform sends and the bridge drops.
        let events = (case.delivery)(SECRET, now_millis());
        let parsed = source
            .parse(
                &HeaderMap::from_pairs(
                    events
                        .headers
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_str())),
                ),
                &events.body,
            )
            .unwrap_or_else(|reject| {
                panic!("{} could not read its own delivery: {reject}", case.name)
            });
        assert!(
            !parsed.is_empty(),
            "{}: a delivery it verifies must become an event",
            case.name
        );
    }
}

#[test]
fn every_adapter_refuses_a_delivery_signed_with_another_secret() {
    for case in cases() {
        let source = source(case.name);
        let delivery = (case.delivery)(OTHER_SECRET, now_millis());

        let rejected = verify(&source, &delivery)
            .expect_err(&format!("{} accepted a foreign secret", case.name));
        assert_eq!(
            rejected,
            Reject::BadSignature,
            "{}: a foreign secret is a bad signature, not {:?}",
            case.name,
            rejected
        );
    }
}

#[test]
fn every_adapter_refuses_a_delivery_with_no_proof_at_all() {
    for case in cases() {
        let source = source(case.name);
        let delivery = (case.delivery)(SECRET, now_millis());
        // Every header the scheme names, not just the first: a platform that accepts
        // either form has *two* ways to prove itself, and removing the proof means
        // removing both.
        let declared: Vec<String> = source
            .signature()
            .headers
            .iter()
            .map(|header| header.to_ascii_lowercase())
            .collect();
        let without: Vec<(String, String)> = delivery
            .headers
            .iter()
            .filter(|(name, _)| !declared.contains(&name.to_ascii_lowercase()))
            .cloned()
            .collect();

        let stripped = Delivery {
            headers: without,
            body: delivery.body,
        };
        let rejected = verify(&source, &stripped).expect_err(&format!(
            "{} accepted a delivery with no signature",
            case.name
        ));
        assert!(
            matches!(rejected, Reject::MissingHeader(_)),
            "{}: {:?} should be reported as a missing header",
            case.name,
            rejected
        );
    }
}

#[test]
fn a_body_signature_covers_the_body_and_a_token_does_not() {
    for case in cases() {
        let source = source(case.name);
        let delivery = (case.delivery)(SECRET, now_millis());
        let tampered = Delivery {
            headers: delivery.headers.clone(),
            body: {
                let mut body = delivery.body.clone();
                // One byte, in the middle: the smallest change a delivery could suffer.
                let middle = body.len() / 2;
                body[middle] ^= 0x01;
                body
            },
        };

        match source.signature().algorithm {
            Algorithm::HmacSha256 => {
                let rejected = verify(&source, &tampered).expect_err(&format!(
                    "{}: an HMAC over the body must not survive a changed body",
                    case.name
                ));
                assert_eq!(rejected, Reject::BadSignature, "{}", case.name);
            }
            Algorithm::Token => {
                // A shared token travels *with* the body and says nothing about it, so
                // this is a real limitation rather than a bug - and the suite states it
                // out loud, so nobody reads a green run as "tamper-proof".
                verify(&source, &tampered).unwrap_or_else(|reject| {
                    panic!(
                        "{}: a token scheme cannot detect a changed body, so this should \
                         still verify (and the platform must be trusted for it): {reject}",
                        case.name
                    )
                });
            }
        }
    }
}

#[test]
fn every_adapter_refuses_a_stale_delivery_where_its_payload_carries_time() {
    for case in cases().into_iter().filter(|case| case.binds_time) {
        let source = source(case.name);

        // An hour old, and signed correctly *for that time*: the signature is valid and
        // the delivery is not. The intake runs both halves - verify the sender, then
        // apply time - so the refusal has to come out of that pipeline, and this is a
        // statement about *where* each half lives: the signature says who sent it, the
        // freshness window says when.
        let delivery = (case.delivery)(SECRET, now_millis() - 3_600_000);
        let pairs: Vec<(&str, &str)> = delivery
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let headers = HeaderMap::from_pairs(pairs);
        verify(&source, &delivery).unwrap_or_else(|reject| {
            panic!(
                "{}: the signature covers the delivery's own time and so is valid here: {reject}",
                case.name
            )
        });

        let rejected = source
            .parse(&headers, &delivery.body)
            .expect_err(&format!("{} accepted an hour-old delivery", case.name));
        assert_eq!(
            rejected,
            Reject::Stale,
            "{}: {:?} is not what staleness looks like",
            case.name,
            rejected
        );
    }
}

#[test]
fn every_preset_declares_a_platform_it_can_be_configured_as() {
    for case in cases() {
        let spec = presets::preset(case.name).expect("the preset loads");
        assert!(
            !spec.event.rules.is_empty(),
            "{}: a preset that recognises no event types would verify every delivery and \
             then drop all of them",
            case.name
        );
        assert!(
            !spec.event.headers.is_empty() || spec.event.body_field.is_some(),
            "{}: an event name has to come from somewhere - a header or the body",
            case.name
        );
        assert!(
            !spec.signature.headers.is_empty(),
            "{}: every scheme names the header it reads the proof from",
            case.name
        );
        let source = source(case.name);
        assert_eq!(
            spec.signature.headers,
            source.signature().headers,
            "{}: the source enforces the scheme the preset declares",
            case.name
        );
        // The name a config writes in `type` is the name the preset is looked up by, so
        // the suite's own case list is the list of types a deployment can actually use.
        assert_eq!(source.id().as_str(), case.name, "{}", case.name);
    }
}
