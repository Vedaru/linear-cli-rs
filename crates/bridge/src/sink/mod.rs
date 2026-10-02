//! The write half of the connector boundary.
//!
//! A [`Sink`] writes to one platform. Every operation takes the *platform-side*
//! scope (a repository, a team) because that is what a platform needs to be told,
//! and a platform-neutral [`IssueFields`] because that is what the reconciler
//! knows. How those become requests is configuration
//! ([`spec`]); the trait is what the reconciler codes against.
//!
//! Two rules from the port map live in this layer:
//!
//! - an operation a platform does not have is an [`Error::Unsupported`], not a
//!   silent no-op: a mapping that asks a forge to attach an issue link should fail
//!   visibly;
//! - a mutation returns the platform's own id, and the caller stores it in the
//!   link table. Nothing derives an id from a payload.

pub mod declarative;
pub mod spec;
pub mod template;

use crate::domain::{Capabilities, ConnectorId, IssueFields, Patch};
use crate::error::Result;

/// A pointer to an entity on the platform, as the platform identifies it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRef {
    pub id: String,
    pub url: Option<String>,
}

/// An issue read back from the platform, in neutral terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteIssue {
    pub reference: RemoteRef,
    pub fields: IssueFields,
    /// The platform's state name, when it has one.
    pub state: Option<String>,
}

pub trait Sink: Send + Sync {
    fn id(&self) -> &ConnectorId;

    fn capabilities(&self) -> Capabilities;

    /// Read an issue's current state.
    ///
    /// The reconciler re-reads rather than trusting a webhook body: an `edited`
    /// event is a stale snapshot, and the authoritative state is the one a GET
    /// returns. `Ok(None)` means the platform says it is gone.
    fn fetch_issue(&self, scope: &str, id: &str) -> Result<Option<RemoteIssue>>;

    /// Create an issue, optionally in a given state (the initial state a new
    /// mirror should land in, rather than the platform's default).
    fn create_issue(
        &self,
        scope: &str,
        fields: &IssueFields,
        state: Option<&str>,
    ) -> Result<RemoteRef>;

    /// Apply the fields that differ to an existing issue.
    ///
    /// A [`Patch`], not a field set: an update that restated every field would
    /// overwrite whatever the other side holds for the ones it did not actually
    /// change, and it is the difference the caller has already worked out.
    fn update_issue(&self, scope: &str, id: &str, patch: &Patch, state: Option<&str>)
        -> Result<()>;

    fn comment(&self, scope: &str, id: &str, body: &str) -> Result<RemoteRef>;

    /// Edit a comment this bridge mirrored. `id` is the *comment's* id - the one
    /// [`Sink::comment`] returned - not the issue's.
    fn update_comment(&self, scope: &str, id: &str, body: &str) -> Result<()>;

    /// Remove a comment this bridge mirrored.
    fn delete_comment(&self, scope: &str, id: &str) -> Result<()>;

    fn transition(&self, scope: &str, id: &str, state: &str) -> Result<()>;

    fn delete_issue(&self, scope: &str, id: &str) -> Result<()>;

    /// Attach a link (a mirrored issue, a pull request, a commit) to an issue.
    fn attach(&self, scope: &str, id: &str, url: &str, title: &str) -> Result<()>;
}
