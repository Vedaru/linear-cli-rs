//! Error type shared by the crate. Intake rejections are their own type
//! ([`connector::Reject`]) because they are *expected* outcomes that map to HTTP
//! status codes, not failures of the service.

use thiserror::Error;

use crate::connector::Reject;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    /// Configuration is unusable. Startup fails with a message naming the field,
    /// because a service that boots with a half-read config fails later, on a
    /// webhook, where it is much harder to diagnose.
    #[error("configuration error: {0}")]
    Config(String),

    #[error("no connector is configured for `{0}`")]
    UnknownConnector(String),

    #[error("store error: {0}")]
    Store(#[from] rusqlite::Error),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid JSON payload: {0}")]
    Json(#[from] serde_json::Error),

    /// A delivery that failed authentication or parsing. The inner value carries
    /// the HTTP status the intake path must answer with.
    #[error("rejected delivery: {0}")]
    Rejected(#[from] Reject),

    /// A queued handler (reconciler, adapter call) failed. Retried by the queue
    /// under its backoff policy; only the message reaches the delivery row.
    #[error("handler failed: {0}")]
    Handler(String),
}
