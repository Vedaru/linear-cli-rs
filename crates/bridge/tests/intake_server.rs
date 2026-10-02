//! End-to-end intake: a real server, real sockets, genuinely signed bodies.
//!
//! The unit tests cover the decision function; this covers the thing an operator
//! actually deploys - a bound socket answering status codes, writing deliveries,
//! and surviving a replay. It deliberately speaks raw HTTP over a `TcpStream`
//! instead of using a client library: the assertions are about the bytes on the
//! wire (status codes, whether the connection is reusable), and a client that
//! hides those would be testing itself.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::Sha256;

use linear_bridge::domain::Secret;
use linear_bridge::http::intake::Intake;
use linear_bridge::http::{Bridge, HandlerFactory, ServeDeps, StoreFactory};
use linear_bridge::queue::{Handler, LoggingHandler, WorkerConfig};
use linear_bridge::sources::declarative::DeclarativeSource;
use linear_bridge::sources::presets;
use linear_bridge::store::sqlite::SqliteStore;
use linear_bridge::store::Store;

mod support;

const LINEAR_SECRET: &str = "0123456789abcdef";
const FORGEJO_SECRET: &str = "fedcba9876543210";
const BODY_LIMIT: usize = 4096;

/// A running bridge on an ephemeral port, with its own database file.
struct Harness {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    database: PathBuf,
}

impl Harness {
    fn start() -> Self {
        // A dying intake thread says so in a log line. Without a logger installed, that
        // line goes nowhere and the symptom is a test that hangs for its whole timeout.
        linear_bridge::logging::init_default();
        let database = std::env::temp_dir().join(format!(
            "linear-bridge-test-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sources = vec![
            Arc::new(DeclarativeSource::new(
                "linear",
                Secret::new(LINEAR_SECRET),
                presets::preset("linear").expect("the linear preset loads"),
            )) as Arc<dyn linear_bridge::connector::Source>,
            Arc::new(DeclarativeSource::new(
                "forgejo",
                Secret::new(FORGEJO_SECRET),
                presets::preset("forgejo").expect("the forgejo preset loads"),
            )),
        ];
        let intake = Arc::new(Intake::new(sources, BODY_LIMIT).expect("intake"));

        let path = database.clone();
        let store: StoreFactory = Arc::new(move || -> linear_bridge::Result<Box<dyn Store>> {
            Ok(Box::new(SqliteStore::open(&path)?))
        });
        let handler: HandlerFactory = Arc::new(|| Box::new(LoggingHandler) as Box<dyn Handler>);

        let deps = ServeDeps {
            addr: "127.0.0.1:0".parse().unwrap(),
            intake,
            store,
            handler,
            http_threads: 2,
            worker_threads: 1,
            worker: WorkerConfig {
                max_attempts: 2,
                backoff_base: Duration::ZERO,
                backoff_max: Duration::ZERO,
                poll_interval: Duration::from_millis(10),
                lease: Duration::from_secs(5),
            },
        };
        let bridge = Bridge::bind(deps).expect("bind");
        let addr = bridge
            .local_addr()
            .expect("an ephemeral port is an IP socket");
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = shutdown.clone();
        let thread = std::thread::spawn(move || {
            bridge.run(flag).expect("the server runs");
        });

        Self {
            addr,
            shutdown,
            thread: Some(thread),
            database,
        }
    }

    /// Store handle for assertions, opened separately from the server's own
    /// connections (SQLite is single-writer, and reads may happen concurrently).
    fn store(&self) -> SqliteStore {
        SqliteStore::open(&self.database).expect("the test database is readable")
    }

    fn post(&self, path: &str, headers: &[(&str, String)], body: &[u8]) -> (u16, String) {
        self.request("POST", path, headers, body)
    }

    fn get(&self, path: &str) -> (u16, String) {
        self.request("GET", path, &[], b"")
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect to the bridge");
        stream
            // Generous on purpose: a test client waiting on a local server must not
            // decide whether the suite is green. Five seconds was enough on an idle
            // machine and not on a busy one; thirty was not enough on a busy one either
            // (in CI the whole request went unanswered while a release build used the
            // same four cores). When something is genuinely broken the suite still
            // fails - two minutes later.
            .set_read_timeout(Some(Duration::from_secs(120)))
            .unwrap();

        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.addr,
            body.len()
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");

        // A refused oversized body can close the connection mid-write; the status
        // line is what the test cares about, so a write error is not fatal here.
        let mut payload = request.into_bytes();
        payload.extend_from_slice(body);
        let _ = stream.write_all(&payload);
        let _ = stream.flush();

        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        parse_response(&response)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.database);
    }
}

fn parse_response(response: &str) -> (u16, String) {
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The body Linear really sends, read from the fixture the rest of the suite uses.
///
/// This file tests the engine, not the preset, so the payload is not its business: taking
/// it from `presets/fixtures/linear.toml` means the bytes here and the bytes the conformance
/// suite checks cannot drift apart.
fn linear_body(delivery_timestamp_ms: u128) -> Vec<u8> {
    support::fixture_for("linear")
        .deliveries
        .into_iter()
        .find(|delivery| delivery.event.as_deref() == Some("Issue"))
        .expect("the linear fixture declares an Issue delivery")
        .body_at(delivery_timestamp_ms as i64)
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn linear_headers(body: &[u8], delivery: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Linear-Signature", sign(LINEAR_SECRET, body)),
        ("Linear-Delivery", delivery.to_string()),
        ("Content-Type", "application/json".to_string()),
    ]
}

#[test]
fn a_signed_linear_delivery_is_accepted_queued_and_drained() {
    let harness = Harness::start();
    let body = linear_body(now_millis());

    let (status, response) = harness.post(
        "/webhooks/linear",
        &linear_headers(&body, "delivery-1"),
        &body,
    );
    assert_eq!(status, 202, "{response}");
    assert!(response.contains("\"accepted\":true"), "{response}");
    assert!(response.contains("\"duplicate\":false"), "{response}");

    // The queue is durable, so the delivery exists in the store the moment the
    // 202 was sent - that is the property that makes a crash survivable.
    let mut store = harness.store();
    let counts = store.counts().unwrap();
    assert_eq!(
        counts.pending + counts.active + counts.done,
        1,
        "{counts:?}"
    );

    // And the worker picks it up: with a logging handler it completes.
    //
    // The deadline is generous on purpose: CI runs on the same machine as the tests,
    // and so does whatever else is being built - a release build on this four-core box
    // took 2m42s while a workspace test run was going. Ten seconds was not enough
    // (this failed once in three workspace runs, with the suite taking exactly the
    // deadline and passing on the next run unchanged), and sixty was not enough either:
    // in CI the intake's *next* request got no answer at all and this test waited its
    // whole budget. Two minutes costs nothing on the happy path, which is 0.3s.
    let mut drained = false;
    for _ in 0..2400 {
        if store.counts().unwrap().done == 1 {
            drained = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        drained,
        "the worker never drained the delivery: {:?}",
        store.counts().unwrap()
    );
}

#[test]
fn a_replayed_delivery_is_a_no_op() {
    let harness = Harness::start();
    let body = linear_body(now_millis());
    let headers = linear_headers(&body, "delivery-replay");

    let (first, _) = harness.post("/webhooks/linear", &headers, &body);
    let (second, response) = harness.post("/webhooks/linear", &headers, &body);
    assert_eq!(first, 202);
    assert_eq!(second, 202);
    assert!(response.contains("\"duplicate\":true"), "{response}");

    let mut store = harness.store();
    let counts = store.counts().unwrap();
    assert_eq!(
        counts.pending + counts.active + counts.done,
        1,
        "a retry must not queue a second delivery: {counts:?}"
    );
}

#[test]
fn a_tampered_body_is_refused_with_401_and_queues_nothing() {
    let harness = Harness::start();
    let body = linear_body(now_millis());
    let headers = linear_headers(&body, "delivery-tampered");
    let mut tampered = body.clone();
    tampered.extend_from_slice(b" ");

    let (status, response) = harness.post("/webhooks/linear", &headers, &tampered);
    assert_eq!(status, 401, "{response}");
    assert!(response.contains("invalid signature"), "{response}");

    let mut store = harness.store();
    assert_eq!(store.counts().unwrap().pending, 0);
}

#[test]
fn a_stale_linear_delivery_is_refused() {
    let harness = Harness::start();
    let stale = now_millis() - 600_000;
    let body = linear_body(stale);
    let (status, _) = harness.post(
        "/webhooks/linear",
        &linear_headers(&body, "delivery-stale"),
        &body,
    );
    assert_eq!(status, 400);
}

#[test]
fn a_forgejo_ping_is_acknowledged_with_no_work() {
    let harness = Harness::start();
    let body =
        br#"{"repository":{"full_name":"a/b"},"zen":"Non-blocking is better than blocking."}"#;
    let signature = sign(FORGEJO_SECRET, body);
    let headers = vec![
        ("X-Forgejo-Signature", signature),
        ("X-Forgejo-Event", "ping".to_string()),
        ("X-Forgejo-Delivery", "ping-1".to_string()),
    ];

    let (status, response) = harness.post("/webhooks/forgejo", &headers, body);
    assert_eq!(status, 202, "{response}");
    assert!(response.contains("\"events\":0"), "{response}");

    let mut store = harness.store();
    assert_eq!(store.counts().unwrap().pending, 0, "nothing to queue");
}

#[test]
fn routing_and_limits_answer_the_documented_status_codes() {
    let harness = Harness::start();

    assert_eq!(harness.get("/healthz").0, 200);
    assert_eq!(harness.get("/version").0, 200);
    assert_eq!(harness.post("/webhooks/unknown", &[], b"{}").0, 404);
    assert_eq!(harness.get("/webhooks/linear").0, 405);

    // One byte over the limit is enough: the payload is never read in full.
    let oversized = vec![b'x'; BODY_LIMIT + 1];
    let (status, _) = harness.post("/webhooks/linear", &[], &oversized);
    assert_eq!(status, 413);
}

#[test]
fn health_reports_the_store_and_the_delivery_counts() {
    let harness = Harness::start();
    let (status, body) = harness.get("/healthz");
    assert_eq!(status, 200);
    assert!(body.contains("\"status\":\"ok\""), "{body}");
    assert!(body.contains("\"deliveries\""), "{body}");

    let (status, body) = harness.get("/version");
    assert_eq!(status, 200);
    assert!(body.contains(env!("CARGO_PKG_VERSION")), "{body}");
}
