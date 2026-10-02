//! Platform-agnostic bridge framework behind `linear webhook serve`.
//!
//! The crate exists because the repository's CLI was one-shot: every command
//! authenticated, called Linear, printed and exited. A bridge that mirrors work
//! between platforms is the same logic with a lifecycle, and it must not be a
//! second code base.
//!
//! Layering (see `docs/architecture-decisions` in the project for the reasoning):
//!
//! ```text
//! http  ->  connector::Source  ->  domain::Event  ->  store  ->  queue  ->  handler
//! ```
//!
//! - [`domain`] is the vocabulary: platform-neutral entities, events and
//!   capabilities. It has no I/O.
//! - [`connector`] is the seam a platform implements: a `Source` parses and
//!   authenticates a delivery, a `Sink` writes to the platform. Neither trait
//!   names a platform.
//! - [`store`] is durable state: the delivery queue and the link tables.
//! - [`queue`] claims due deliveries and hands them to a [`queue::Handler`].
//! - [`http`] is the intake surface: verify, persist, acknowledge.
//!
//! Design constraints that shaped the code, all deliberate:
//!
//! - **No async runtime.** A request-per-thread server and a small worker pool;
//!   outbound calls are blocking. One concurrency model for CLI and service.
//! - **Bounded memory by construction.** Bodies are capped while reading, a
//!   worker holds one delivery at a time, and the queue is the database rather
//!   than a channel, so a burst costs disk and not RSS.
//! - **The request path never panics** on attacker-controlled input: every parse
//!   failure is a typed [`connector::Reject`], and a rejection is a status code.

pub mod clock;
pub mod config;
pub mod connector;
pub mod domain;
pub mod error;
pub mod http;
pub mod http_client;
pub mod logging;
pub mod pointer;
pub mod queue;
pub mod sink;
pub mod sources;
pub mod store;
pub mod verify;

pub use error::{Error, Result};

/// Largest webhook body accepted, in bytes. A payload larger than this is
/// rejected with `413` before it is fully read into memory.
pub const DEFAULT_BODY_LIMIT: usize = 1024 * 1024;

/// Minimum accepted length of a webhook signing secret. The webhook endpoints
/// are the only unauthenticated surface this service exposes, so a short secret
/// is a configuration error, not a warning.
pub const MIN_SECRET_LEN: usize = 16;
