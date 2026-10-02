//! The intake server.
//!
//! A request-per-thread design with no async runtime: `tiny_http`'s accept loop
//! is shared across `http_threads` threads, each of which owns a store
//! connection, and a separate pool of worker threads drains the queue. Both pools
//! are fixed size, so neither throughput nor memory grows with traffic - a burst
//! becomes rows in the database, not threads in the process.

pub mod intake;

use std::io::Read;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tiny_http::{Header, Request, Response, StatusCode};

use crate::connector::HeaderMap;
use crate::error::{Error, Result};
use crate::http::intake::{Intake, Outcome};
use crate::queue::{Handler, Worker, WorkerConfig};
use crate::store::{InsertOutcome, Store};

/// Opens a store connection. Called once per thread, because SQLite connections
/// are neither `Sync` nor safe to share, and a per-thread connection removes the
/// need for a mutex in front of the database.
pub type StoreFactory = Arc<dyn Fn() -> Result<Box<dyn Store>> + Send + Sync>;

/// Builds a handler for one worker thread.
pub type HandlerFactory = Arc<dyn Fn() -> Box<dyn Handler> + Send + Sync>;

pub struct ServeDeps {
    pub addr: SocketAddr,
    pub intake: Arc<Intake>,
    pub store: StoreFactory,
    pub handler: HandlerFactory,
    pub http_threads: usize,
    pub worker_threads: usize,
    pub worker: WorkerConfig,
}

/// Accept-loop granularity. Also the worst-case delay for a shutdown request to
/// be noticed on an idle server.
const ACCEPT_TICK: Duration = Duration::from_millis(250);

/// A bound server. Binding is separated from running so a caller (and a test)
/// can learn the port before the accept loop takes over the thread.
pub struct Bridge {
    server: Arc<tiny_http::Server>,
    deps: ServeDeps,
}

impl Bridge {
    pub fn bind(deps: ServeDeps) -> Result<Self> {
        let server = tiny_http::Server::http(deps.addr)
            .map_err(|error| Error::Config(format!("cannot bind {}: {error}", deps.addr)))?;
        Ok(Self {
            server: Arc::new(server),
            deps,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.server
            .server_addr()
            .to_ip()
            .ok_or_else(|| Error::Config("server is not bound to an IP socket".into()))
    }

    /// Spawn both pools and block until `shutdown` is set.
    pub fn run(self, shutdown: Arc<AtomicBool>) -> Result<()> {
        let mut handles = Vec::new();

        for index in 0..self.deps.http_threads.max(1) {
            let server = self.server.clone();
            let intake = self.deps.intake.clone();
            let store = self.deps.store.clone();
            let shutdown = shutdown.clone();
            handles.push(
                std::thread::Builder::new()
                    .name(format!("intake-{index}"))
                    .spawn(move || {
                        if let Err(error) = serve_requests(server, intake, store, shutdown) {
                            log::error!("intake thread stopped: {error}");
                        }
                    })?,
            );
        }

        for index in 0..self.deps.worker_threads.max(1) {
            let store = self.deps.store.clone();
            let handler = self.deps.handler.clone();
            let config = self.deps.worker;
            let shutdown = shutdown.clone();
            handles.push(
                std::thread::Builder::new()
                    .name(format!("worker-{index}"))
                    .spawn(move || {
                        let store = match store() {
                            Ok(store) => store,
                            Err(error) => {
                                log::error!("worker cannot open the store: {error}");
                                return;
                            }
                        };
                        let mut worker = Worker::new(store, handler(), config);
                        if let Err(error) = worker.run(&shutdown) {
                            log::error!("queue worker stopped: {error}");
                        }
                    })?,
            );
        }

        log::info!(
            "bridge listening on {} ({} intake threads, {} workers)",
            self.local_addr()
                .map(|addr| addr.to_string())
                .unwrap_or_else(|_| "?".into()),
            self.deps.http_threads.max(1),
            self.deps.worker_threads.max(1),
        );

        while !shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(ACCEPT_TICK);
        }
        for handle in handles {
            if handle.join().is_err() {
                // A panicked thread is reported, not fatal: the process is
                // supervised, and refusing to shut down cleanly would be worse.
                log::error!("a bridge thread panicked");
            }
        }
        Ok(())
    }
}

fn serve_requests(
    server: Arc<tiny_http::Server>,
    intake: Arc<Intake>,
    store: StoreFactory,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut store = store()?;
    while !shutdown.load(Ordering::Relaxed) {
        match server.recv_timeout(ACCEPT_TICK) {
            Ok(Some(request)) => handle_request(request, &intake, store.as_mut()),
            Ok(None) => {}
            Err(error) => log::warn!("intake accept failed: {error}"),
        }
    }
    Ok(())
}

fn handle_request(mut request: Request, intake: &Intake, store: &mut dyn Store) {
    let method = request.method().as_str().to_owned();
    let path = request
        .url()
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .to_owned();
    let headers = HeaderMap::from_pairs(request.headers().iter().map(|header| {
        (
            header.field.as_str().as_str().to_owned(),
            header.value.as_str().to_owned(),
        )
    }));

    if method == "GET" && path == "/healthz" {
        let (status, body) = health(store);
        respond(request, status, body);
        return;
    }
    if method == "GET" && path == "/version" {
        respond(
            request,
            200,
            json!({
                "name": env!("CARGO_PKG_NAME"),
                "version": env!("CARGO_PKG_VERSION"),
            })
            .to_string(),
        );
        return;
    }

    // Read the body *before* deciding anything: `respond` consumes the request,
    // and the cap is enforced while reading, so an oversized payload is never
    // fully materialised.
    let limit = intake.body_limit();
    let body = match read_body(request.as_reader(), limit) {
        Ok(body) => body,
        Err(error) => {
            log::warn!("could not read a request body: {error}");
            respond(
                request,
                400,
                json!({ "error": "could not read request body" }).to_string(),
            );
            return;
        }
    };

    match intake.decide(&method, &path, &headers, &body) {
        Outcome::Accepted { delivery, events } => match store.insert_delivery(&delivery) {
            Ok(outcome) => {
                let duplicate = outcome == InsertOutcome::Duplicate;
                log::debug!(
                    "accepted {}/{} ({}) {} {} duplicate={duplicate}",
                    delivery.connector,
                    delivery.event,
                    delivery.action.as_str(),
                    delivery.scope.as_deref().unwrap_or("-"),
                    delivery.native_id,
                );
                respond(
                    request,
                    202,
                    json!({ "accepted": true, "duplicate": duplicate, "events": events })
                        .to_string(),
                );
            }
            Err(error) => {
                // The delivery is authentic but cannot be queued, so the provider
                // must retry: 500, and the reason goes to the log, not the wire.
                log::error!("cannot store an accepted delivery: {error}");
                respond(
                    request,
                    500,
                    json!({ "error": "storage unavailable" }).to_string(),
                );
            }
        },
        Outcome::Nothing => respond(
            request,
            202,
            json!({ "accepted": true, "duplicate": false, "events": 0 }).to_string(),
        ),
        Outcome::Rejected(reject) => respond(
            request,
            reject.status(),
            json!({ "error": reject.public_message() }).to_string(),
        ),
        Outcome::NotFound => respond(
            request,
            404,
            json!({ "error": "no such endpoint" }).to_string(),
        ),
        Outcome::MethodNotAllowed => {
            respond(request, 405, json!({ "error": "POST only" }).to_string())
        }
        Outcome::PayloadTooLarge => {
            // Ask for the connection to close: the rest of an oversized body is
            // still on the wire, and reusing this connection would desynchronise
            // the next request.
            let response = build_response(413, json!({ "error": "payload too large" }).to_string())
                .with_header(header("Connection", "close"));
            if let Err(error) = request.respond(response) {
                log::warn!("failed to answer an oversized request: {error}");
            }
        }
    }
}

fn health(store: &mut dyn Store) -> (u16, String) {
    match store.health().and_then(|()| store.counts()) {
        Ok(counts) => (
            200,
            json!({
                "status": "ok",
                "deliveries": {
                    "pending": counts.pending,
                    "active": counts.active,
                    "done": counts.done,
                    "dead": counts.dead,
                }
            })
            .to_string(),
        ),
        Err(error) => {
            log::error!("health check failed: {error}");
            (503, json!({ "status": "degraded" }).to_string())
        }
    }
}

fn read_body(reader: &mut dyn Read, limit: usize) -> std::io::Result<Vec<u8>> {
    // One byte over the limit is enough to detect an oversized body without
    // reading it: the decision belongs to the intake, and the rest of the body
    // stays on the wire.
    let mut body = Vec::with_capacity(limit.min(64 * 1024) + 1);
    let mut limited = reader.take(limit as u64 + 1);
    limited.read_to_end(&mut body)?;
    Ok(body)
}

fn respond(request: Request, status: u16, body: String) {
    if let Err(error) = request.respond(build_response(status, body)) {
        log::warn!("failed to send a response: {error}");
    }
}

fn build_response(status: u16, body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_status_code(StatusCode(status))
        .with_header(header("Content-Type", "application/json"))
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .expect("response header names and values are static ASCII")
}
