//! One blocking HTTP client, shared by every sink.
//!
//! A platform's write path is configuration ([`crate::sink::spec`]), so the code
//! that actually speaks HTTP is platform-agnostic by construction: it takes a
//! fully resolved request and returns the parsed response. Nothing here knows what
//! Linear or Forgejo is, and adding a platform does not touch it.
//!
//! Blocking, reuse-the-agent `ureq`, exactly like the CLI's Linear client: one
//! concurrency model for the whole binary (D1 in the architecture decisions).
//!
//! The request policy - deadlines, and which requests may be repeated - lives in
//! [`crate::net`], which is a byte-identical copy of the CLI's `src/net.rs` and is kept that way by
//! a test: the CLI must build without this crate, so the policy cannot be one shared item, and two
//! hand-kept sets of numbers is how the CLI and the service end up disagreeing about what "a retry"
//! means.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::net;

const USER_AGENT: &str = concat!("linear-bridge/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

impl Method {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_uppercase().as_str() {
            "GET" => Some(Method::Get),
            "POST" => Some(Method::Post),
            "PATCH" => Some(Method::Patch),
            "PUT" => Some(Method::Put),
            "DELETE" => Some(Method::Delete),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }
}

/// A request the client can send, with nothing left to decide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Value>,
    /// Whether this request may be sent twice (see [`crate::net`]).
    ///
    /// A delivery that repeats a create duplicates an issue, so this is `false` unless the caller
    /// can see that the request is a read: an idempotent method, or a GraphQL `query`.
    pub repeatable: bool,
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub body: Value,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The response body as one line, for an error message. Truncated because
    /// upstream bodies can be a whole HTML page.
    pub fn summary(&self) -> String {
        let text = self.body.to_string();
        let mut summary: String = text.chars().take(400).collect();
        if text.chars().count() > 400 {
            summary.push('…');
        }
        summary
    }
}

pub struct HttpClient {
    agent: ureq::Agent,
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient {
    pub fn new() -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            // Read 4xx/5xx bodies ourselves: an API's error payload is the only
            // useful thing about a failure, and `ureq` would otherwise turn the
            // whole response into a transport error and discard it.
            .http_status_as_error(false)
            .timeout_global(Some(net::request_timeout()))
            .timeout_connect(Some(net::connect_timeout()))
            .build()
            .into();
        Self { agent }
    }

    pub fn send(&self, request: &Request) -> Result<Response> {
        let attempts = if request.repeatable {
            net::MAX_ATTEMPTS
        } else {
            1
        };

        for attempt in 0..attempts {
            let last_try = attempt + 1 == attempts;
            match self.attempt(request) {
                Ok(response) => {
                    if last_try || !net::is_retryable_status(response.status) {
                        return Ok(response);
                    }
                }
                Err(error) => {
                    if last_try {
                        return Err(error);
                    }
                }
            }
            std::thread::sleep(net::backoff(attempt));
        }

        // Not reachable: the loop returns on its last attempt. Kept as a real
        // call rather than an `unreachable!` so a future edit cannot panic a worker.
        self.attempt(request)
    }

    /// One attempt, with transport and body failures mapped to what a caller can act on.
    fn attempt(&self, request: &Request) -> Result<Response> {
        // Each method builder type differs by whether a body is expected, so the
        // match is on (method, body) rather than on the method alone.
        let sent = match (request.method, request.body.as_ref()) {
            (Method::Get, _) => with_headers(self.agent.get(&request.url), &request.headers).call(),
            (Method::Delete, _) => {
                with_headers(self.agent.delete(&request.url), &request.headers).call()
            }
            (Method::Post, Some(body)) => {
                with_headers(self.agent.post(&request.url), &request.headers).send_json(body)
            }
            (Method::Patch, Some(body)) => {
                with_headers(self.agent.patch(&request.url), &request.headers).send_json(body)
            }
            (Method::Put, Some(body)) => {
                with_headers(self.agent.put(&request.url), &request.headers).send_json(body)
            }
            (Method::Post, None) => {
                // A body-capable builder still needs an explicit empty send.
                with_headers(self.agent.post(&request.url), &request.headers).send_empty()
            }
            (Method::Patch, None) => {
                with_headers(self.agent.patch(&request.url), &request.headers).send_empty()
            }
            (Method::Put, None) => {
                with_headers(self.agent.put(&request.url), &request.headers).send_empty()
            }
        };

        let mut response = sent.map_err(|error| {
            Error::Upstream(format!(
                "{} {} failed: {error}",
                request.method.as_str(),
                request.url
            ))
        })?;

        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().map_err(|error| {
            Error::Upstream(format!(
                "{} {} returned {status} with an unreadable body: {error}",
                request.method.as_str(),
                request.url
            ))
        })?;

        // An empty body is normal for a DELETE or a 204, and the callers that
        // need a value check for null rather than guessing.
        let body = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).map_err(|error| {
                Error::Upstream(format!(
                    "{} {} returned {status} with a body that is not JSON: {error}",
                    request.method.as_str(),
                    request.url
                ))
            })?
        };

        Ok(Response { status, body })
    }
}

/// Apply the standard and per-request headers to a builder, whatever body policy
/// it carries.
fn with_headers<T>(
    mut call: ureq::RequestBuilder<T>,
    headers: &[(String, String)],
) -> ureq::RequestBuilder<T> {
    call = call.header("User-Agent", USER_AGENT);
    for (name, value) in headers {
        call = call.header(name, value);
    }
    call
}
