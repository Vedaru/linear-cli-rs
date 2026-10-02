//! Webhook intake: routing, authentication and the decision to queue.
//!
//! [`Intake::decide`] is a pure function of (method, path, headers, body): no
//! socket, no database, no clock beyond what a source reads for freshness. That
//! is deliberate - the interesting cases (a tampered body, a stale timestamp, an
//! unknown connector, an oversized payload) are exactly the ones that must be
//! tested, and they are testable here without a server.

use std::sync::Arc;

use crate::connector::{HeaderMap, Reject, Source};
use crate::domain::{Event, EventDetail};
use crate::error::{Error, Result};
use crate::store::NewDelivery;

/// Where intake paths live. One endpoint per configured connector instance.
pub const INTAKE_PREFIX: &str = "/webhooks";

/// What intake decided. Each variant owns the HTTP status it must produce, so
/// the server cannot disagree with the decision.
#[derive(Debug)]
pub enum Outcome {
    Accepted {
        delivery: Box<NewDelivery>,
        /// How many events the delivery carried; a push may carry several.
        events: usize,
    },
    /// Authenticated and parsed, with nothing to do (`ping`, unmodelled event).
    Nothing,
    Rejected(Reject),
    NotFound,
    MethodNotAllowed,
    PayloadTooLarge,
}

impl Outcome {
    pub fn status(&self) -> u16 {
        match self {
            Outcome::Accepted { .. } | Outcome::Nothing => 202,
            Outcome::Rejected(reject) => reject.status(),
            Outcome::NotFound => 404,
            Outcome::MethodNotAllowed => 405,
            Outcome::PayloadTooLarge => 413,
        }
    }
}

pub struct Intake {
    sources: Vec<Arc<dyn Source>>,
    body_limit: usize,
}

impl Intake {
    pub fn new(sources: Vec<Arc<dyn Source>>, body_limit: usize) -> Result<Self> {
        if sources.is_empty() {
            return Err(Error::Config(
                "intake needs at least one platform to accept webhooks for".into(),
            ));
        }
        Ok(Self {
            sources,
            body_limit,
        })
    }

    pub fn body_limit(&self) -> usize {
        self.body_limit
    }

    /// Configured connectors, for logs and `--json` diagnostics.
    pub fn connectors(&self) -> Vec<(&str, Vec<&'static str>)> {
        self.sources
            .iter()
            .map(|source| (source.id().as_str(), source.capabilities().describe()))
            .collect()
    }

    pub fn decide(&self, method: &str, path: &str, headers: &HeaderMap, body: &[u8]) -> Outcome {
        if body.len() > self.body_limit {
            return Outcome::PayloadTooLarge;
        }
        if method != "POST" {
            return Outcome::MethodNotAllowed;
        }
        let Some(name) = path.strip_prefix(&format!("{INTAKE_PREFIX}/")) else {
            return Outcome::NotFound;
        };
        // A trailing slash is the same endpoint, not a different one.
        let name = name.trim_end_matches('/');
        let Some(source) = self.sources.iter().find(|s| s.id().as_str() == name) else {
            return Outcome::NotFound;
        };

        if let Err(reject) = source.authenticate(headers, body) {
            log::warn!("rejected delivery for `{name}`: {reject}");
            return Outcome::Rejected(reject);
        }

        match source.parse(headers, body) {
            Err(reject) => {
                log::warn!("unparseable delivery for `{name}`: {reject}");
                Outcome::Rejected(reject)
            }
            Ok(events) if events.is_empty() => Outcome::Nothing,
            Ok(events) => match new_delivery(&events, body) {
                Ok((delivery, count)) => Outcome::Accepted {
                    delivery: Box::new(delivery),
                    events: count,
                },
                Err(error) => {
                    log::warn!("delivery for `{name}` could not be stored as text: {error}");
                    Outcome::Rejected(Reject::Malformed(error.to_string()))
                }
            },
        }
    }
}

/// One delivery row per HTTP request, describing its first event.
///
/// A push can carry many events under one delivery id; they share a row because
/// they share an idempotency key, and the reconciler re-parses the stored body to
/// see all of them. Storing one row per event would break the "a retry is a
/// no-op insert" guarantee that intake depends on.
fn new_delivery(events: &[Event], body: &[u8]) -> Result<(NewDelivery, usize)> {
    let first = events
        .first()
        .ok_or_else(|| Error::Config("no events to store".into()))?;
    let body = std::str::from_utf8(body)
        .map_err(|error| Error::Config(format!("body is not UTF-8: {error}")))?;
    let delivery = NewDelivery {
        connector: first.connector.clone(),
        delivery_id: first.delivery.as_str().to_owned(),
        event: first.event.clone(),
        kind: first.kind.clone(),
        action: first.action.clone(),
        scope: first.subject.scope.clone(),
        native_id: first.subject.native_id.clone(),
        body: body.to_owned(),
    };
    Ok((delivery, events.len()))
}

/// Text a reference event carries, if any. Used by the reconciler and by
/// diagnostics; kept here so callers do not pattern-match on [`EventDetail`]
/// themselves.
pub fn reference_text(event: &Event) -> Option<&str> {
    match &event.detail {
        EventDetail::Reference { text, .. } => Some(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::now_millis;
    use crate::domain::Secret;
    use crate::sources::declarative::DeclarativeSource;
    use crate::sources::presets;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    fn secret() -> &'static str {
        "0123456789abcdef"
    }

    fn intake() -> Intake {
        Intake::new(
            vec![
                Arc::new(DeclarativeSource::new(
                    "linear",
                    Secret::new(secret()),
                    presets::preset("linear").expect("the preset loads"),
                )) as Arc<dyn Source>,
                Arc::new(DeclarativeSource::new(
                    "forgejo",
                    Secret::new(secret()),
                    presets::preset("forgejo").expect("the preset loads"),
                )),
            ],
            64 * 1024,
        )
        .unwrap()
    }

    fn sign(body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret().as_bytes()).unwrap();
        mac.update(body);
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn linear_body() -> Vec<u8> {
        format!(
            r#"{{"action":"create","type":"Issue","webhookTimestamp":{},
                "data":{{"id":"issue-1","identifier":"VED-1","team":{{"key":"VED"}}}}}}"#,
            now_millis()
        )
        .into_bytes()
    }

    fn linear_headers(body: &[u8]) -> HeaderMap {
        HeaderMap::from_pairs([
            ("Linear-Signature".to_string(), sign(body)),
            ("Linear-Delivery".to_string(), "delivery-1".to_string()),
        ])
    }

    #[test]
    fn accepts_a_signed_linear_delivery() {
        let body = linear_body();
        let outcome = intake().decide("POST", "/webhooks/linear", &linear_headers(&body), &body);
        match outcome {
            Outcome::Accepted { delivery, events } => {
                assert_eq!(events, 1);
                assert_eq!(delivery.delivery_id, "delivery-1");
                assert_eq!(delivery.native_id, "issue-1");
                assert_eq!(delivery.scope.as_deref(), Some("VED"));
                assert_eq!(delivery.connector.as_str(), "linear");
            }
            other => panic!("expected acceptance, got {other:?}"),
        }
    }

    #[test]
    fn a_tampered_body_is_rejected_with_401() {
        let body = linear_body();
        let mut tampered = body.clone();
        tampered.push(b' ');
        let outcome = intake().decide(
            "POST",
            "/webhooks/linear",
            &linear_headers(&body),
            &tampered,
        );
        assert_eq!(outcome.status(), 401);
        assert!(matches!(outcome, Outcome::Rejected(Reject::BadSignature)));
    }

    #[test]
    fn a_missing_signature_header_is_rejected_without_leaking_the_reason() {
        let body = linear_body();
        let outcome = intake().decide("POST", "/webhooks/linear", &HeaderMap::default(), &body);
        assert_eq!(outcome.status(), 400);
        match outcome {
            Outcome::Rejected(reject) => {
                assert_eq!(reject.public_message(), "missing required header");
            }
            other => panic!("expected rejection, got {other:?}"),
        }
    }

    #[test]
    fn a_stale_linear_timestamp_is_rejected() {
        let body = r#"{"action":"create","type":"Issue","webhookTimestamp":0,"data":{"id":"x"}}"#
            .as_bytes()
            .to_vec();
        let outcome = intake().decide("POST", "/webhooks/linear", &linear_headers(&body), &body);
        assert_eq!(outcome.status(), 400);
    }

    #[test]
    fn a_forgejo_ping_is_acknowledged_with_nothing_to_do() {
        let body =
            br#"{"repository":{"full_name":"a/b"},"zen":"Non-blocking is better than blocking."}"#;
        let headers = HeaderMap::from_pairs([
            ("X-Forgejo-Signature".to_string(), sign(body)),
            ("X-Forgejo-Event".to_string(), "ping".to_string()),
        ]);
        let outcome = intake().decide("POST", "/webhooks/forgejo", &headers, body);
        assert!(matches!(outcome, Outcome::Nothing));
        assert_eq!(outcome.status(), 202);
    }

    #[test]
    fn routing_errors_are_distinguishable() {
        let body = linear_body();
        assert!(matches!(
            intake().decide("POST", "/webhooks/nope", &linear_headers(&body), &body),
            Outcome::NotFound
        ));
        assert!(matches!(
            intake().decide("GET", "/webhooks/linear", &linear_headers(&body), &body),
            Outcome::MethodNotAllowed
        ));
        assert!(matches!(
            intake().decide("POST", "/healthz", &linear_headers(&body), &body),
            Outcome::NotFound
        ));
    }

    #[test]
    fn an_oversized_body_is_refused_before_parsing() {
        let mut intake = intake();
        intake.body_limit = 8;
        let body = linear_body();
        let outcome = intake.decide("POST", "/webhooks/linear", &linear_headers(&body), &body);
        assert_eq!(outcome.status(), 413);
    }

    #[test]
    fn the_connector_list_is_reportable() {
        let intake = intake();
        let connectors = intake.connectors();
        assert_eq!(connectors.len(), 2);
        let linear = connectors
            .iter()
            .find(|(name, _)| *name == "linear")
            .expect("linear is configured");
        assert!(linear.1.contains(&"states:named"));
    }
}
