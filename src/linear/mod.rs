//! Linear data layer. Port of the core of `src/utils/linear.ts`.
//!
//! This module owns the lookups every command shares: turning loose user input
//! into a canonical issue identifier or ID, resolving teams, projects, labels,
//! members, workflow states, cycles, milestones, initiatives, and releases. It
//! deliberately works with `serde_json::Value` rather than a typed model so
//! `--json` output can preserve GraphQL field names and nesting verbatim, as
//! the project's agent contract requires.
//!
//! The layer is split one file per domain. Each submodule pulls the shared
//! imports from [`prelude`] and the sibling items the parent re-exports.

// The split keeps each domain small; the glob imports that make that ergonomic
// also make it easy to leave a name unused in a given file.
#![allow(unused_imports)]

mod cycles;
mod dates;
mod documents;
mod identifiers;
mod initiatives;
mod interactive;
mod issues;
mod labels;
mod members;
mod milestones;
mod ordering;
mod projects;
mod queries;
mod releases;
mod states;
mod teams;
mod users;
mod views;

#[cfg(test)]
mod tests;

pub use cycles::*;
pub use dates::*;
pub use documents::*;
pub use identifiers::*;
pub use initiatives::*;
pub use interactive::*;
pub use issues::*;
pub use labels::*;
pub use members::*;
pub use milestones::*;
pub use ordering::*;
pub use projects::*;
pub use releases::*;
pub use states::*;
pub use teams::*;
pub use users::*;
pub use views::*;

pub(crate) use queries::*;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The order the app groups statuses in. It is NOT lifecycle order: `started`
/// sits above `unstarted`, so the states a person is working on lead the
/// listing and the finished ones trail it. This table is the single place to
/// fix if a future release moves a group.
pub(crate) const WORKFLOW_STATE_TYPE_ORDER: [&str; 7] = [
    "triage",
    "started",
    "unstarted",
    "backlog",
    "completed",
    "canceled",
    "duplicate",
];

/// The bare workflow state type tokens `--state` accepts.
pub const ISSUE_STATE_TYPES: [&str; 6] = [
    "triage",
    "backlog",
    "unstarted",
    "started",
    "completed",
    "canceled",
];

/// Imports every domain file needs. Submodules use this via
/// `use super::prelude::*;` so the split does not repeat a long import list.
pub(crate) mod prelude {
    pub(crate) use std::sync::OnceLock;

    pub(crate) use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
    pub(crate) use regex::Regex;
    pub(crate) use serde_json::{json, Map, Value};

    pub(crate) use crate::config::{self, IssueSort, Resolved};
    pub(crate) use crate::errors::{CliError, ErrorKind, Result};
    pub(crate) use crate::graphql;
    pub(crate) use crate::issue_identifier::normalize_issue_identifier;
    pub(crate) use crate::linear_url::{
        expect_linear_url_kind, reject_linear_url, CycleRef, LinearUrlRef,
    };
    pub(crate) use crate::prompt;
}
