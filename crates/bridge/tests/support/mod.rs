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

// --- conformance fixtures ---------------------------------------------------
//
// A platform's conformance fixture lives in a config file beside its preset, so the suite
// that runs over every adapter contains no per-platform code: it reads these files, signs
// the body with the scheme each *preset* declares, and asks the same questions of each.
// Adding a platform is a preset and a fixture - two files.

/// One platform's fixture, as configured.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Fixture {
    /// Another preset whose fixture this one is - a line of configuration instead of a
    /// second copy that can drift from the first.
    #[serde(default)]
    pub same_as: Option<String>,
    /// What the platform sends, with `{now}` where its own time goes.
    #[serde(default)]
    pub body: String,
    /// The headers it sends that are not the proof.
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    /// What this platform really signs with, so the suite can hold the preset to the real
    /// contract instead of to itself: the harness builds the proof from the *preset*, so
    /// without this nothing would notice a preset that reads the wrong header.
    ///
    /// Absent on a fixture that says `same_as`: it inherits the fixture it points at,
    /// proof and all.
    #[serde(default)]
    pub proof: Option<Proof>,
}

/// A platform's proof of authenticity, as its own documentation describes it.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Proof {
    /// `hmac-sha256` or `token`.
    pub algorithm: String,
    /// The header the platform sends it in, spelled as the platform spells it.
    pub header: String,
    /// What wraps the digest, if anything (`sha256=` on GitHub).
    #[serde(default)]
    pub prefix: Option<String>,
}

impl Fixture {
    /// Whether this payload carries its own time, and so whether a scheme that binds time
    /// must reject an old one.
    pub fn binds_time(&self) -> bool {
        self.body.contains("{now}")
    }

    /// The body, with the platform's own time filled in.
    pub fn body_at(&self, millis: i64) -> Vec<u8> {
        self.body.replace("{now}", &millis.to_string()).into_bytes()
    }
}

/// A secret the fixtures can be signed with.
pub const TEST_SECRET: &str = "0123456789abcdef";

/// A different one, for the deliveries that must be refused.
pub const OTHER_SECRET: &str = "fedcba9876543210";

fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/fixtures")
}

/// Every fixture in the build, keyed by the preset name it belongs to.
///
/// Read from the directory rather than listed here, so a new platform's fixture is picked
/// up by existing it. `same_as` is resolved to the fixture it names.
pub fn conformance_fixtures() -> Vec<(String, Fixture)> {
    let dir = fixtures_dir();
    let mut fixtures = std::collections::BTreeMap::new();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("reading {}: {error}", dir.display()))
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    entries.sort();

    for path in entries {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("a fixture file is named after its preset")
            .to_string();
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        let fixture: Fixture = toml::from_str(&text)
            .unwrap_or_else(|error| panic!("{} is not a usable fixture: {error}", path.display()));
        fixtures.insert(name, fixture);
    }

    // Resolved in a second pass: a fixture may point at one that was read before or after it.
    let resolved: Vec<(String, Fixture)> = fixtures
        .keys()
        .map(|name| {
            let mut fixture = fixtures[name].clone();
            if let Some(target) = fixture.same_as.clone() {
                fixture = fixtures
                    .get(&target)
                    .unwrap_or_else(|| {
                        panic!("`{name}` says it is `{target}`, which has no fixture")
                    })
                    .clone();
            }
            (name.clone(), fixture)
        })
        .collect();
    resolved
}

/// Every preset the build ships must have a fixture - checked here rather than in a test,
/// so the failure names the file to add.
pub fn fixture_for(name: &str) -> Fixture {
    conformance_fixtures()
        .into_iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, fixture)| fixture)
        .unwrap_or_else(|| {
            panic!(
                "the `{name}` preset has no conformance fixture: add presets/fixtures/{name}.toml \
                 with a delivery it would really send"
            )
        })
}

/// The proof a delivery carries, built from the scheme the *preset* declares - so the
/// harness has no per-platform signing code either.
pub fn proof_header(
    source: &dyn linear_bridge::connector::Source,
    secret: &str,
    body: &[u8],
) -> (String, String) {
    use hmac::{Hmac, Mac};
    use linear_bridge::connector::Algorithm;
    use sha2::Sha256;

    let scheme = source.signature();
    let value = match scheme.algorithm {
        Algorithm::HmacSha256 => {
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("a key");
            mac.update(body);
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        }
        Algorithm::Token => secret.to_string(),
    };
    let header = scheme
        .headers
        .first()
        .expect("every scheme names the header it reads proof from")
        .clone();
    (
        header,
        format!("{}{value}", scheme.prefix.clone().unwrap_or_default()),
    )
}

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
