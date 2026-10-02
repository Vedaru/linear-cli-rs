//! The delivery queue: claim, handle, retry, or park.
//!
//! The queue is the `deliveries` table, not an in-memory channel. That is what
//! makes a restart safe (a claimed delivery is reclaimed when its lease expires)
//! and what keeps memory flat under a burst: a thousand simultaneous webhooks
//! cost a thousand rows, not a thousand queued structs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::clock::now_nanos;
use crate::error::Result;
use crate::store::{Delivery, Store};

/// Consumes deliveries.
///
/// **Handlers must be idempotent.** The queue retries, and a provider may deliver
/// the same change twice; a handler that assumes "called once per change" turns
/// either into duplicated work on the far platform.
pub trait Handler: Send {
    fn handle(&mut self, delivery: &Delivery) -> Result<()>;
}

/// The M2 handler: log the normalized delivery and succeed.
///
/// It exists so intake can be exercised end to end (and deployed) before the
/// reconciler lands, and it is the shape the reconciler will replace - one
/// delivery in, one outcome out.
pub struct LoggingHandler;

impl Handler for LoggingHandler {
    fn handle(&mut self, delivery: &Delivery) -> Result<()> {
        log::info!(
            "delivery {} [{}] {} {} {}/{} attempt {}",
            delivery.id,
            delivery.connector,
            delivery.event,
            delivery.action.as_str(),
            delivery.scope.as_deref().unwrap_or("-"),
            delivery.native_id,
            delivery.attempts,
        );
        Ok(())
    }
}

/// A boxed handler is itself a handler, so a caller can choose the implementation
/// at runtime (the service does: `Box<dyn Handler>` per worker thread) without
/// the worker becoming generic over a type parameter it cannot name.
impl Handler for Box<dyn Handler> {
    fn handle(&mut self, delivery: &Delivery) -> Result<()> {
        (**self).handle(delivery)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerConfig {
    /// Attempts before a delivery is parked as dead.
    pub max_attempts: u32,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// Idle wait when nothing is due.
    pub poll_interval: Duration,
    /// How long a claimed delivery is hidden from other workers. Must exceed the
    /// longest plausible handler run, or a slow handler gets its own delivery
    /// processed underneath it.
    pub lease: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        let backoff_max = Duration::from_secs(300);
        Self {
            max_attempts: 8,
            backoff_base: Duration::from_secs(2),
            backoff_max,
            poll_interval: Duration::from_secs(1),
            lease: backoff_max + Duration::from_secs(60),
        }
    }
}

pub struct Worker<H: Handler> {
    store: Box<dyn Store>,
    handler: H,
    config: WorkerConfig,
}

impl<H: Handler> Worker<H> {
    pub fn new(store: Box<dyn Store>, handler: H, config: WorkerConfig) -> Self {
        Self {
            store,
            handler,
            config,
        }
    }

    /// Claim and process a single delivery. Returns `false` when nothing was due,
    /// which is the caller's cue to idle rather than spin.
    pub fn tick(&mut self) -> Result<bool> {
        let claimed = self.store.claim_due(1, self.config.lease)?;
        let Some(delivery) = claimed.into_iter().next() else {
            return Ok(false);
        };

        match self.handler.handle(&delivery) {
            Ok(()) => {
                self.store.complete(delivery.id)?;
                log::debug!("delivery {} done", delivery.id);
            }
            Err(error) => {
                let message = error.to_string();
                if delivery.attempts >= self.config.max_attempts {
                    self.store.fail(delivery.id, &message, None)?;
                    log::error!(
                        "delivery {} parked as dead after {} attempts: {message}",
                        delivery.id,
                        delivery.attempts
                    );
                } else {
                    let delay = backoff_delay(
                        delivery.attempts,
                        self.config.backoff_base,
                        self.config.backoff_max,
                        now_nanos(),
                    );
                    self.store.fail(delivery.id, &message, Some(delay))?;
                    log::warn!(
                        "delivery {} failed (attempt {}), retrying in {}ms: {message}",
                        delivery.id,
                        delivery.attempts,
                        delay.as_millis()
                    );
                }
            }
        }
        Ok(true)
    }

    /// Process deliveries until `shutdown` is set.
    ///
    /// A handler panic is *not* caught: the release profile is built with
    /// `panic = "abort"` for size, so unwinding does not exist in the shipped
    /// binary. The contract is therefore that handlers return errors instead of
    /// panicking, and the process is supervised (restart on failure) as the
    /// backstop. `catch_unwind` here would be a lie that only holds in debug.
    pub fn run(&mut self, shutdown: &AtomicBool) -> Result<()> {
        while !shutdown.load(Ordering::Relaxed) {
            let worked = match self.tick() {
                Ok(worked) => worked,
                Err(error) => {
                    // A store failure is not the delivery's fault: log it, back
                    // off a little, and keep the service alive.
                    log::error!("queue tick failed: {error}");
                    false
                }
            };
            if !worked {
                sleep_interruptibly(shutdown, self.config.poll_interval);
            }
        }
        Ok(())
    }
}

/// Exponential backoff with **full jitter**: a uniform delay in
/// `[0, min(max, base * 2^(attempt-1)))`.
///
/// Full jitter rather than "delay ± noise" because the failure mode being avoided
/// is a fleet of workers retrying in lockstep after an outage - the jitter has to
/// spread them over the whole window to do that. `entropy` is passed in so tests
/// are deterministic and no RNG is needed.
pub fn backoff_delay(attempt: u32, base: Duration, max: Duration, entropy: u64) -> Duration {
    let exponent = attempt.saturating_sub(1).min(31);
    let base_ms = base.as_millis() as u64;
    let window_ms = base_ms
        .saturating_mul(1u64 << exponent)
        .min(max.as_millis() as u64);
    if window_ms == 0 {
        return Duration::ZERO;
    }
    Duration::from_millis(entropy % window_ms)
}

/// Sleep in short slices so a shutdown request is noticed promptly instead of
/// after a full poll interval.
fn sleep_interruptibly(shutdown: &AtomicBool, duration: Duration) {
    const SLICE: Duration = Duration::from_millis(100);
    let mut slept = Duration::ZERO;
    while slept < duration && !shutdown.load(Ordering::Relaxed) {
        let chunk = (duration - slept).min(SLICE);
        std::thread::sleep(chunk);
        slept += chunk;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Action, ConnectorId, EntityKind};
    use crate::store::sqlite::SqliteStore;
    use crate::store::{NewDelivery, Store};
    use std::collections::VecDeque;

    struct Scripted {
        outcomes: VecDeque<bool>,
        seen: Vec<String>,
    }

    impl Handler for Scripted {
        fn handle(&mut self, delivery: &Delivery) -> Result<()> {
            self.seen.push(delivery.native_id.clone());
            match self.outcomes.pop_front() {
                Some(true) | None => Ok(()),
                Some(false) => Err(crate::error::Error::Handler("scripted failure".into())),
            }
        }
    }

    fn store(ids: &[&str]) -> Box<dyn Store> {
        let mut store = SqliteStore::open_in_memory().unwrap();
        for id in ids {
            store
                .insert_delivery(&NewDelivery {
                    connector: ConnectorId::new("linear"),
                    delivery_id: (*id).to_string(),
                    event: "Issue".into(),
                    kind: EntityKind::Issue,
                    action: Action::Created,
                    scope: Some("VED".into()),
                    native_id: (*id).to_string(),
                    body: "{}".into(),
                })
                .unwrap();
        }
        Box::new(store)
    }

    fn config() -> WorkerConfig {
        WorkerConfig {
            max_attempts: 3,
            // No delay at all: the retry must be immediately due, otherwise this
            // test would be asserting on the scheduler's reaction time.
            backoff_base: Duration::ZERO,
            backoff_max: Duration::ZERO,
            poll_interval: Duration::from_millis(1),
            lease: Duration::from_secs(30),
        }
    }

    #[test]
    fn a_successful_delivery_is_completed() {
        let mut worker = Worker::new(
            store(&["d-1"]),
            Scripted {
                outcomes: VecDeque::from([true]),
                seen: vec![],
            },
            config(),
        );
        assert!(worker.tick().unwrap());
        let counts = worker.store.counts().unwrap();
        assert_eq!(counts.done, 1);
        assert_eq!(counts.pending, 0);
        assert_eq!(worker.handler.seen, vec!["d-1"]);
        assert!(!worker.tick().unwrap(), "nothing left to do");
    }

    #[test]
    fn a_failing_delivery_is_retried_then_parked_as_dead() {
        let mut worker = Worker::new(
            store(&["d-1"]),
            Scripted {
                outcomes: VecDeque::from([false, false, false]),
                seen: vec![],
            },
            config(),
        );
        // Each tick claims the delivery again because the retry is immediately
        // due: attempts 1 and 2 reschedule, attempt 3 exhausts max_attempts.
        assert!(worker.tick().unwrap());
        assert!(worker.tick().unwrap());
        assert!(worker.tick().unwrap());
        assert!(!worker.tick().unwrap());
        assert_eq!(worker.store.counts().unwrap().dead, 1);
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let base = Duration::from_millis(1000);
        let max = Duration::from_millis(8000);
        assert_eq!(backoff_delay(1, base, max, 0), Duration::ZERO);
        assert!(backoff_delay(1, base, max, u64::MAX) < base);
        assert!(backoff_delay(2, base, max, u64::MAX) < base * 2);
        // Capped: the window never exceeds `max`, whatever the attempt count.
        assert!(backoff_delay(20, base, max, u64::MAX) < max);
    }
}
