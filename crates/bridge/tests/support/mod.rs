//! A fake platform on a real socket, shared by the integration tests.
//!
//! Deliberately dumb: a route table and a recording of what arrived. The
//! assertions are about the bytes a connector sends and the bytes it accepts, so
//! the platform has to be boring for a failure to mean anything.
//!
//! `allow(dead_code)`: every test binary compiles its own copy of this module, and
//! a binary that does not need, say, `graphql()` should not fail the build for it.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tiny_http::{Header, Response, Server};

use linear_bridge::domain::{Capabilities, Secret};
use linear_bridge::sink::declarative::DeclarativeSink;
use linear_bridge::sink::spec::SinkSpec;
use linear_bridge::sources::presets;

/// What the platform answers for one request.
pub type Route =
    Box<dyn Fn(&str, &str, &serde_json::Value) -> (u16, serde_json::Value) + Send + Sync>;

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub body: serde_json::Value,
}

pub struct Fake {
    base_url: String,
    seen: Arc<Mutex<Vec<Recorded>>>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Fake {
    pub fn start(
        route: impl Fn(&str, &str, &serde_json::Value) -> (u16, serde_json::Value)
            + Send
            + Sync
            + 'static,
    ) -> Self {
        let server = Server::http("127.0.0.1:0").expect("a fake platform binds");
        let base_url = format!("http://{}", server.server_addr());
        let seen: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let thread = {
            let seen = Arc::clone(&seen);
            let shutdown = Arc::clone(&shutdown);
            thread::spawn(move || {
                while !shutdown.load(Ordering::SeqCst) {
                    let Ok(Some(mut request)) = server.recv_timeout(Duration::from_millis(25))
                    else {
                        continue;
                    };
                    let method = request.method().as_str().to_string();
                    let path = request.url().to_string();
                    let mut raw = String::new();
                    let _ = request.as_reader().read_to_string(&mut raw);
                    let body: serde_json::Value =
                        serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
                    seen.lock().expect("recorded").push(Recorded {
                        method: method.clone(),
                        path: path.clone(),
                        body: body.clone(),
                    });
                    let (status, reply) = route(&method, &path, &body);
                    let header = Header::from_bytes("Content-Type", "application/json")
                        .expect("a valid header");
                    let _ = request.respond(
                        Response::from_string(reply.to_string())
                            .with_status_code(status)
                            .with_header(header),
                    );
                }
            })
        };

        Self {
            base_url,
            seen,
            shutdown,
            thread: Some(thread),
        }
    }

    pub fn url(&self) -> &str {
        &self.base_url
    }

    pub fn seen(&self) -> Vec<Recorded> {
        self.seen.lock().expect("recorded").clone()
    }

    /// The one request with this method and path.
    pub fn only(&self, method: &str, path: &str) -> Recorded {
        let seen = self.seen();
        let found: Vec<&Recorded> = seen
            .iter()
            .filter(|record| record.method == method && record.path == path)
            .collect();
        assert_eq!(found.len(), 1, "expected one {method} {path}, got {seen:?}");
        found[0].clone()
    }

    /// Every request whose GraphQL query mentions `needle` - how a fake tells one
    /// mutation from another when they share a path.
    pub fn graphql(&self, needle: &str) -> Vec<Recorded> {
        self.seen()
            .into_iter()
            .filter(|record| {
                record
                    .body
                    .get("query")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|query| query.contains(needle))
            })
            .collect()
    }

    /// A sink built from a preset, pointed at this fake instead of the real API.
    pub fn sink(&self, preset: &str) -> DeclarativeSink {
        let source = presets::preset(preset).expect("the preset loads");
        let capabilities: Capabilities = source.capabilities.into();
        let spec: SinkSpec = source.sink.expect("the preset has a write half");
        self.sink_with(preset, spec, capabilities)
    }

    /// The same, for a spec a test has changed - the way to ask what a platform
    /// *without* some operation does, rather than asserting a preset stays limited.
    pub fn sink_with(
        &self,
        connector: &str,
        mut spec: SinkSpec,
        capabilities: Capabilities,
    ) -> DeclarativeSink {
        let path = spec.base_url.splitn(4, '/').nth(3).unwrap_or("");
        spec.base_url = if path.is_empty() {
            self.base_url.clone()
        } else {
            format!("{}/{path}", self.base_url)
        };
        DeclarativeSink::new(
            connector,
            spec,
            Some(Secret::new("test-token")),
            capabilities,
        )
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
