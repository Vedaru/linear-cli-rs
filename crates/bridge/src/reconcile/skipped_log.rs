//! What did not travel, said once per reason instead of once per issue.
//!
//! `report_skipped` used to warn for every skipped field of every delivery. The sweep delivers one
//! issue at a time, so "once per delivery" is "once per issue": with no `[[mapping.user]]` entry,
//! every issue's assignee is skipped for the same reason, and a pass over a workspace produces a
//! hundred identical lines. The real warnings - the `unmapped` and `emulated` ones, which mean
//! something different for each issue - are then invisible in the noise.
//!
//! The refusal itself is design, not a bug: `Reason::NoIdentityMap` exists so the bridge does not
//! translate one platform's login into another's by hope. What was wrong was the promptness of the
//! report, so this keeps the report and drops the repetition:
//!
//! * the first skip of a given `(reason, field)` in this process is logged at `warn`, with the
//!   config a person could set to stop it;
//! * every later one is logged at `debug`, so `RUST_LOG=debug` still shows each issue;
//! * a *new* reason is always a `warn`, however far into the pass it appears - nothing that differs
//!   is ever lost, only the identical repeats.
//!
//! Process-wide rather than per-pass on purpose: the bridge has no pass boundary to flush at, and a
//! restart is exactly when an operator wants to be told again.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::projection::{Reason, Skipped};

/// The record of what has been reported so far in this process.
#[derive(Default)]
pub struct SkippedLog {
    seen: OnceLock<Mutex<HashMap<(&'static str, Reason), u64>>>,
}

impl SkippedLog {
    pub const fn new() -> Self {
        Self {
            seen: OnceLock::new(),
        }
    }

    /// Record one skip. `true` the first time this `(field, reason)` has been seen here.
    pub fn record(&self, skipped: &Skipped) -> bool {
        let mut seen = self
            .seen
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = seen.entry((skipped.field, skipped.reason)).or_insert(0);
        *count += 1;
        *count == 1
    }

    /// How many skips of this `(field, reason)` this process has recorded.
    pub fn count(&self, skipped: &Skipped) -> u64 {
        let seen = self
            .seen
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        seen.get(&(skipped.field, skipped.reason))
            .copied()
            .unwrap_or(0)
    }
}

/// The line for a skip that is being reported for the first time.
///
/// The reason carries the fix when there is one: "no identity map" is a fact about the config, and a
/// warning that says so without saying what to write is a warning an operator has to research.
pub fn line(mapping: &str, skipped: &Skipped) -> String {
    match advice(skipped.reason) {
        Some(advice) => format!("`{mapping}`: {skipped} — {advice}"),
        None => format!("`{mapping}`: {skipped}"),
    }
}

/// What an operator can do about a reason, when there is something.
fn advice(reason: Reason) -> Option<&'static str> {
    match reason {
        Reason::NoIdentityMap => Some(
            "account translation is off, not guessed: add one [[mapping.user]] entry per account \
             to translate logins between the platforms",
        ),
        Reason::Unsupported | Reason::Unmapped | Reason::Emulated => None,
    }
}

/// The process-wide record, which is what `report_skipped` reports through.
pub static SKIPPED_THIS_PROCESS: SkippedLog = SkippedLog::new();

#[cfg(test)]
mod tests {
    use super::*;

    fn skip(field: &'static str, reason: Reason) -> Skipped {
        Skipped {
            field,
            value: "loner@example.com".to_string(),
            reason,
        }
    }

    #[test]
    fn one_reason_over_many_issues_reports_once_and_counts_the_rest() {
        let log = SkippedLog::new();
        let assignee = skip("assignee", Reason::NoIdentityMap);

        assert!(
            log.record(&assignee),
            "the first is the one an operator sees"
        );
        assert!(!log.record(&assignee), "the second is a repeat");
        assert!(!log.record(&assignee), "and so is the hundredth");
        assert_eq!(log.count(&assignee), 3, "the repeats are still counted");
    }

    #[test]
    fn a_different_reason_is_never_lost_in_the_noise() {
        let log = SkippedLog::new();
        assert!(log.record(&skip("assignee", Reason::NoIdentityMap)));
        // A field that is genuinely unmapped says something the first warning did not, so it must
        // not be suppressed by it - which is the whole risk of de-duplicating a log.
        assert!(log.record(&skip("assignee", Reason::Unmapped)));
        assert!(log.record(&skip("priority", Reason::Unmapped)));
        assert!(!log.record(&skip("priority", Reason::Unmapped)));
    }

    #[test]
    fn the_identity_map_warning_names_the_config_that_would_fix_it() {
        let reported = line("VED-42", &skip("assignee", Reason::NoIdentityMap));
        assert!(reported.contains("no identity map"), "{reported}");
        assert!(
            reported.contains("[[mapping.user]]"),
            "the fix is in the line: {reported}"
        );

        // Reasons that are not a config gap do not pretend to have an answer.
        let reported = line("VED-42", &skip("assignee", Reason::Unmapped));
        assert!(!reported.contains("[[mapping.user]]"), "{reported}");
    }
}
