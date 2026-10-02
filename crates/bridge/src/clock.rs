//! Wall-clock helpers, in one place so the modules that need a timestamp agree
//! on the unit. Instants are epoch milliseconds; Unix seconds are exposed only
//! where a protocol dictates them.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// Nanoseconds since the epoch. Used as cheap entropy for backoff jitter.
pub fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or_default()
}
