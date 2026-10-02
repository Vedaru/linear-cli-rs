//! Minimal stderr logger.
//!
//! `log` is a façade with no implementation, so without this every `log::warn!`
//! in the crate would vanish silently - the worst possible failure mode for a
//! service. A full logger stack is not needed for that: level, target, message is
//! the whole contract.
//!
//! The format deliberately carries no timestamp. The service runs under a
//! supervisor (systemd, docker, a tmux pane) and those already stamp every line;
//! a second timestamp only creates two clocks to disagree about.

use std::io::Write;
use std::sync::Once;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Environment variable holding the level: `error`, `warn`, `info`, `debug`,
/// `trace` or `off`.
pub const LEVEL_ENV: &str = "LINEAR_BRIDGE_LOG";

pub const DEFAULT_LEVEL: LevelFilter = LevelFilter::Info;

struct StderrLogger {
    level: LevelFilter,
}

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let level = match record.level() {
            Level::Error => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        };
        // One `writeln!` per record, and the lock is held only for that write:
        // logs from many threads must not interleave mid-line.
        let stderr = std::io::stderr();
        let mut handle = stderr.lock();
        let _ = writeln!(handle, "{level} {}: {}", record.target(), record.args());
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

/// Install the logger once, at `level`. Later calls are no-ops.
pub fn init(level: LevelFilter) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let logger: &'static StderrLogger = Box::leak(Box::new(StderrLogger { level }));
        if log::set_logger(logger).is_ok() {
            log::set_max_level(level);
        }
    });
}

/// Install the logger at the level named by [`LEVEL_ENV`], defaulting to `info`.
pub fn init_default() {
    let level = std::env::var(LEVEL_ENV)
        .ok()
        .and_then(|value| parse_level(&value))
        .unwrap_or(DEFAULT_LEVEL);
    init(level);
}

pub fn parse_level(value: &str) -> Option<LevelFilter> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" | "warning" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_parse_and_unknown_values_are_ignored() {
        assert_eq!(parse_level("DEBUG"), Some(LevelFilter::Debug));
        assert_eq!(parse_level(" off "), Some(LevelFilter::Off));
        assert_eq!(parse_level("shouty"), None);
    }
}
