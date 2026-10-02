//! What a sweep found, and what it would do about it.
//!
//! A survey is the *reading* half of `linear sync`: it lists both ends of a mapping,
//! works out which entities are the same thing and who moved, and produces a plan. It
//! writes nothing. That is what makes the default a dry run rather than a promise - the
//! plan the operator reads is the plan that would run, because carrying it out is not a
//! second decision.

use crate::domain::EntityRef;

use super::{Side, Step};

/// One mapping, read but not acted on.
#[derive(Clone, Debug, Default)]
pub struct Survey {
    pub mapping: String,
    /// The mapping's two ends, for the report's first line.
    pub source: String,
    pub sink: String,
    pub entries: Vec<Entry>,
}

/// One pair (or one unpaired entity), and what a sweep would do about it.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The entity the decision was made from - the one whose revision wins.
    pub subject: EntityRef,
    /// Which end that entity is on.
    pub side: Side,
    /// The copy on the other end, when there is one.
    pub counterpart: Option<EntityRef>,
    /// That copy's state, for the revision a write records.
    pub counterpart_state: Option<String>,
    pub action: Action,
    /// The step to carry out. `None` means nothing is written.
    pub step: Option<Step>,
    /// The revision to record for a pair that needs no write, so the next sweep has a
    /// baseline to compare against. Without it, a pair that is merely *in step* would
    /// never become known and every sweep would compare two sides it cannot date.
    pub record: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Both ends read the same.
    InStep,
    /// The other end has no copy yet.
    Create,
    /// The other end gets this side's revision.
    Write { touched: Vec<String> },
    /// Both ends changed since the bridge last wrote. Reported, never guessed at: a
    /// sweep cannot know which edit was meant, and choosing one throws the other away.
    Conflict,
    /// One end only, and the mapping does not mirror that way.
    NotMirrored { why: String },
}

impl Action {
    /// True when carrying this out would write something.
    pub fn writes(&self) -> bool {
        matches!(self, Action::Create | Action::Write { .. })
    }

    pub fn describe(&self) -> String {
        match self {
            Action::InStep => "in step".into(),
            Action::Create => "create".into(),
            Action::Write { touched } => format!("write {}", touched.join(", ")),
            Action::Conflict => "conflict".into(),
            Action::NotMirrored { why } => format!("not mirrored ({why})"),
        }
    }
}

impl Survey {
    pub fn writes(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.action.writes())
            .count()
    }

    /// Nothing to write: a second sweep reports this, and so does one that only found
    /// conflicts.
    pub fn is_quiet(&self) -> bool {
        self.writes() == 0
    }

    /// The survey as an operator reads it.
    pub fn report(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "{}: {} <-> {}",
            self.mapping, self.source, self.sink
        )];

        let count =
            |action: fn(&Action) -> bool| self.entries.iter().filter(|e| action(&e.action)).count();
        let in_step = count(|action| matches!(action, Action::InStep));
        let creates = count(|action| matches!(action, Action::Create));
        let writes = count(|action| matches!(action, Action::Write { .. }));
        let conflicts = count(|action| matches!(action, Action::Conflict));
        let unmirrored = count(|action| matches!(action, Action::NotMirrored { .. }));

        lines.push(format!("  in step: {in_step}"));
        if creates + writes > 0 {
            lines.push(format!(
                "  to write: {} ({writes} change(s), {creates} creation(s))",
                creates + writes
            ));
        }
        if conflicts > 0 {
            lines.push(format!(
                "  conflicts: {conflicts} - both ends changed since the last sync"
            ));
        }
        if unmirrored > 0 {
            lines.push(format!("  not mirrored: {unmirrored}"));
        }

        for entry in &self.entries {
            if entry.action.writes() || entry.action == Action::Conflict {
                lines.push(format!(
                    "    {} {}: {} (-> {})",
                    entry.subject.connector,
                    entry.subject.native_id,
                    entry.action.describe(),
                    entry
                        .counterpart
                        .as_ref()
                        .map(|other| format!("{} {}", other.connector, other.native_id))
                        .unwrap_or_else(|| "a new one".to_string())
                ));
            }
        }

        lines
    }
}
