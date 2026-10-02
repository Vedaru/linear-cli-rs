//! The framework seam: what a platform must implement to participate.
//!
//! Two traits and a capability descriptor, none of which name a platform. The
//! shipped connectors are all instances of one implementation
//! ([`crate::sources::declarative`]) driven by configuration; this module is the
//! contract they satisfy, so a future hand-written connector (a platform whose
//! payloads need real logic) plugs in beside them without the core changing.

use serde::Deserialize;
use thiserror::Error;

use crate::domain::{Capabilities, ConnectorId, Event, Secret};

/// Case-insensitive header lookup, decoupled from the HTTP library so a source
/// can be tested by handing it a literal header list.
#[derive(Clone, Debug, Default)]
pub struct HeaderMap(Vec<(String, String)>);

impl HeaderMap {
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self(
            pairs
                .into_iter()
                .map(|(name, value)| (name.into().to_ascii_lowercase(), value.into()))
                .collect(),
        )
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.0
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, value)| value.as_str())
    }

    /// First header present among `names`, in order. Providers rename headers
    /// across versions, sending a digest under one name and its predecessor
    /// under another, and the alternatives belong in one place - the
    /// platform's configuration - rather than in each call site.
    pub fn get_any(&self, names: &[&str]) -> Option<&str> {
        names.iter().find_map(|name| self.get(name))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// How a provider authenticates its deliveries.
///
/// Two algorithms cover every platform this ships: an HMAC over the raw body
/// (Linear, the forges) and a plain shared secret in a header.
/// Anything else is a new variant here plus a branch in
/// [`crate::verify`] - one place, not one per platform.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Algorithm {
    #[default]
    HmacSha256,
    /// The secret itself is sent in the header; compared in constant time.
    Token,
}

/// A connector's verification settings, taken from its configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureScheme {
    /// Header names to try, in order of preference.
    pub headers: Vec<String>,
    pub algorithm: Algorithm,
    /// Optional prefix stripped before decoding, where a platform wraps the digest.
    pub prefix: Option<String>,
}

impl SignatureScheme {
    pub fn hmac_sha256(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(|name| (*name).to_string()).collect(),
            algorithm: Algorithm::HmacSha256,
            prefix: None,
        }
    }

    /// The header carrying the signature, if the delivery presented one.
    pub fn lookup<'a>(&self, headers: &'a HeaderMap) -> Option<(&str, &'a str)> {
        let names: Vec<&str> = self.headers.iter().map(String::as_str).collect();
        self.headers
            .iter()
            .zip(names)
            .find_map(|(owned, name)| headers.get(name).map(|value| (owned.as_str(), value)))
    }
}

/// A delivery that was refused. Every variant is an expected outcome of hostile
/// or stale input, and carries the status the intake path must answer with.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum Reject {
    #[error("missing `{0}` header")]
    MissingHeader(String),
    #[error("signature verification failed")]
    BadSignature,
    #[error("payload is outside the accepted freshness window")]
    Stale,
    #[error("malformed payload: {0}")]
    Malformed(String),
    /// The platform was configured without a webhook secret, so nothing can be
    /// verified for it. Reaching this means a delivery arrived at an endpoint that
    /// was never meant to accept one.
    #[error("no webhook secret is configured for this platform")]
    NoSecret,
}

impl Reject {
    pub fn status(&self) -> u16 {
        match self {
            Reject::BadSignature | Reject::NoSecret => 401,
            Reject::MissingHeader(_) | Reject::Stale | Reject::Malformed(_) => 400,
        }
    }

    /// Message for the HTTP body. Rejections that failed authentication report
    /// nothing about *why*: distinguishing "wrong secret" from "malformed
    /// signature" is free help to an attacker.
    pub fn public_message(&self) -> &'static str {
        match self {
            // A platform with no secret was never meant to accept deliveries at all;
            // it answers exactly like a wrong signature, for the same reason.
            Reject::BadSignature | Reject::NoSecret => "invalid signature",
            Reject::MissingHeader(_) => "missing required header",
            Reject::Stale => "stale or missing timestamp",
            Reject::Malformed(_) => "malformed payload",
        }
    }
}

/// A source of events: one webhook endpoint on one platform.
pub trait Source: Send + Sync {
    /// Configured instance name, e.g. `forgejo`.
    fn id(&self) -> &ConnectorId;

    fn signature(&self) -> SignatureScheme;

    /// What this platform signs its deliveries with, if this deployment has one.
    ///
    /// A secret is what an endpoint *verifies* with, and a connector that was never
    /// asked to verify anything is still a perfectly good translator: `sync` uses the
    /// same connectors to read and write a platform without receiving a single
    /// delivery. Requiring one here would mean a deployment that only pushes changes
    /// carrying webhook configuration it has no use for.
    fn secret(&self) -> Option<&Secret>;

    /// Verify the delivery signature against the *raw* body.
    ///
    /// Defaulted so that a connector cannot forget, or silently diverge from,
    /// the check: a connector with an unusual scheme overrides this, and that
    /// override is then visible in review.
    fn authenticate(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), Reject> {
        let secret = self.secret().ok_or(Reject::NoSecret)?;
        crate::verify::verify(secret, &self.signature(), headers, body)
    }

    /// Translate a verified delivery into zero or more events.
    ///
    /// An empty result is a valid outcome (a `ping`, or an event type this
    /// deployment does not model): the delivery is acknowledged, not retried.
    fn parse(&self, headers: &HeaderMap, body: &[u8]) -> Result<Vec<Event>, Reject>;

    /// The headers a *stored* delivery needs to be parsed again.
    ///
    /// The worker re-parses the raw body, because one delivery can carry several
    /// events and the stored row keeps only the first one's summary. The body alone
    /// does not always say what the event was - for a connector whose event name
    /// arrives in a header, the row's event name is the only record of it, so the
    /// connector reconstructs the header it needs. The default is honest for a
    /// connector that reads everything it needs from the body.
    fn replay_headers(&self, _event: &str) -> HeaderMap {
        HeaderMap::default()
    }

    fn capabilities(&self) -> Capabilities;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_case_insensitive_and_ordered() {
        let headers = HeaderMap::from_pairs([("X-Forgejo-Event", "issues")]);
        assert_eq!(headers.get("x-forgejo-event"), Some("issues"));
        assert_eq!(
            headers.get_any(&["x-gitea-event", "x-forgejo-event"]),
            Some("issues")
        );
        assert_eq!(headers.get("missing"), None);
    }

    #[test]
    fn signature_lookup_reports_which_header_matched() {
        let scheme = SignatureScheme::hmac_sha256(&["x-forgejo-signature", "x-gitea-signature"]);
        let headers = HeaderMap::from_pairs([("X-Gitea-Signature", "abc")]);
        assert_eq!(scheme.lookup(&headers), Some(("x-gitea-signature", "abc")));
        assert_eq!(scheme.lookup(&HeaderMap::default()), None);
    }

    #[test]
    fn rejections_map_to_status_codes() {
        assert_eq!(Reject::BadSignature.status(), 401);
        assert_eq!(Reject::MissingHeader("x".into()).status(), 400);
        assert_eq!(Reject::Stale.status(), 400);
        assert_eq!(Reject::BadSignature.public_message(), "invalid signature");
    }

    #[test]
    fn the_default_algorithm_is_an_hmac() {
        #[derive(serde::Deserialize)]
        struct Holder {
            algorithm: Algorithm,
        }
        assert_eq!(Algorithm::default(), Algorithm::HmacSha256);
        let parsed: Holder = toml::from_str("algorithm = \"token\"").unwrap();
        assert_eq!(parsed.algorithm, Algorithm::Token);
    }
}
