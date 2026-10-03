//! One request policy for outbound calls: deadlines, and retries only where a repeat is safe.
//!
//! **This file also lives at `crates/bridge/src/net.rs`, byte for byte.** The copies are
//! deliberate: the CLI must build *without* the bridge crate - that is the whole point of the
//! `service` feature - so the policy cannot be a crate-level item shared by both, and the
//! alternative is each side inventing its own numbers until the CLI and the service disagree about
//! what "a retry" means. `crates/bridge/tests/net_policy.rs` fails when the two files stop being
//! identical, so the copy cannot drift silently.
//!
//! The rules, and why each one is a rule:
//!
//! * **Every call has a deadline.** A stalled API call must not hang an agent session or a cron
//!   job. [`request_timeout`] bounds the whole call; [`connect_timeout`] is shorter, because a
//!   black-holed host is a different failure from a slow answer and should not spend a minute
//!   discovering which one it is.
//! * **Only a read is repeated.** A retried create is a duplicate - the failure this codebase
//!   spends the most effort avoiding - so "is this safe to send twice?" is answered by the request
//!   itself: a GraphQL document says what it is ([`is_repeatable_document`]), and a preset's plain
//!   REST call says it with its method ([`is_repeatable_method`]). Anything not recognisable as a
//!   read is treated as a write.
//! * **Retries are bounded, and jittered.** Three attempts, backing off from 250ms, jittered so a
//!   fleet of agents that hits one rate limit does not synchronise into a second stampede.
//! * **429 and 5xx are worth another try; 4xx is not.** The API answered, and asking the same
//!   question again will get the same answer.
//!
//! A GraphQL response that *is* a 200 carrying an `errors` array is not retried either: it is an
//! answer, and the caller reports it.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Wall-clock budget for one outbound call, including connect and body read.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long to wait for a connection before calling the host dead.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Attempts in total, not "retries": one try plus two.
pub const MAX_ATTEMPTS: u32 = 3;

/// First backoff, doubled per attempt and then jittered.
pub const BACKOFF_BASE: Duration = Duration::from_millis(250);

/// Ceiling on a single backoff sleep.
pub const BACKOFF_MAX: Duration = Duration::from_secs(4);

/// The deadline for one call.
///
/// `LINEAR_REQUEST_TIMEOUT_SECS` overrides it. That is how a test proves a hanging server produces
/// a bounded failure instead of a wedged process, without waiting out the real minute.
pub fn request_timeout() -> Duration {
    env_secs("LINEAR_REQUEST_TIMEOUT_SECS", DEFAULT_REQUEST_TIMEOUT)
}

/// How long to wait for a connection.
pub fn connect_timeout() -> Duration {
    env_secs("LINEAR_CONNECT_TIMEOUT_SECS", DEFAULT_CONNECT_TIMEOUT)
}

fn env_secs(name: &str, default: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(default)
}

/// Whether a GraphQL document may be sent twice.
///
/// The document says what it is. `query` reads, and so does the `{ … }` shorthand; `mutation` and
/// `subscription` write, and anything unrecognisable is treated as a write, because the cost of
/// being wrong is a duplicate rather than a slower failure.
pub fn is_repeatable_document(document: &str) -> bool {
    let mut rest = document.trim_start();
    // A leading comment is not the operation type.
    while rest.starts_with('#') {
        rest = match rest.find('\n') {
            Some(newline) => rest[newline + 1..].trim_start(),
            None => "",
        };
    }

    let keyword: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    match keyword.as_str() {
        "query" => true,
        "mutation" | "subscription" => false,
        _ => rest.starts_with('{'),
    }
}

/// Whether an HTTP method may be repeated: the idempotent ones, by definition.
///
/// `POST` is not here even though many APIs use it for reads, because "many" is not "all" - the
/// document check covers the GraphQL case, which is where the reads live.
pub fn is_repeatable_method(method: &str) -> bool {
    matches!(
        method.to_ascii_uppercase().as_str(),
        "GET" | "HEAD" | "DELETE"
    )
}

/// Whether a status is worth another attempt.
pub fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

/// The sleep before attempt `attempt` (0-based), doubled and jittered.
///
/// Jitter comes from the clock rather than a random number generator: it only has to break up
/// synchronised retries, and it costs no dependency.
pub fn backoff(attempt: u32) -> Duration {
    let doubled = BACKOFF_BASE
        .checked_mul(1u32 << attempt.min(8))
        .unwrap_or(BACKOFF_MAX)
        .min(BACKOFF_MAX);

    let jitter_ceiling = (doubled.as_millis() as u64 / 2).max(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|now| now.subsec_nanos() as u64)
        .unwrap_or(0);
    let jitter = Duration::from_millis(nanos % jitter_ceiling);

    (doubled + jitter).min(BACKOFF_MAX)
}
