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
use crate::error::{Error, Result};

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

    /// Every issue in a scope.
    ///
    /// What a *sweep* starts from, as opposed to a delivery: an id known in advance.
    /// A platform that cannot be enumerated refuses by name rather than returning an
    /// empty list, because "there is nothing there" and "I cannot look" must not be
    /// the same answer to a caller that is about to create things.
    fn list_issues(&self, scope: &str) -> Result<Vec<RemoteIssue>>;

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
    /// Apply a patch.
    ///
    /// `effective` is what the issue will *hold* afterwards - the projected field set, of
    /// which `patch` is the difference. A platform that carries the priority or the due
    /// date in a label needs it, because writing the labels means writing the whole set:
    /// the patch says "the labels changed" and says nothing about a value it did not
    /// move, so a sink left to infer it from the patch alone would drop it.
    fn update_issue(
        &self,
        scope: &str,
        id: &str,
        patch: &Patch,
        effective: &IssueFields,
        state: Option<&str>,
    ) -> Result<()>;

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

    // --- projects ----------------------------------------------------------
    //
    // A project is its own entity, not an issue: it has a title and a description
    // and nothing else the platforms share. The operations below mirror the issue
    // ones, and a sink that does not declare them refuses by name rather than
    // silently doing nothing - the same rule every other operation follows.

    /// Read a project's current state.
    fn fetch_project(&self, _scope: &str, _id: &str) -> Result<Option<RemoteIssue>> {
        Err(Error::Unsupported(
            "project.fetch".into(),
            self.id().clone(),
        ))
    }

    /// Every project in a scope.
    fn list_projects(&self, _scope: &str) -> Result<Vec<RemoteIssue>> {
        Err(Error::Unsupported("project.list".into(), self.id().clone()))
    }

    /// Create a project. The description is where the pairing marker is embedded.
    fn create_project(
        &self,
        _scope: &str,
        _fields: &IssueFields,
        _state: Option<&str>,
    ) -> Result<RemoteRef> {
        Err(Error::Unsupported(
            "project.create".into(),
            self.id().clone(),
        ))
    }

    /// Apply the title/description difference to an existing project.
    fn update_project(
        &self,
        _scope: &str,
        _id: &str,
        _patch: &Patch,
        _effective: &IssueFields,
    ) -> Result<()> {
        Err(Error::Unsupported(
            "project.update".into(),
            self.id().clone(),
        ))
    }

    // --- an issue on a project's board -------------------------------------
    //
    // The container is a field of the *issue* mirror, not an entity of its own: an
    // issue whose source names a paired project is put on that project's board.
    // Both are no-ops by default, and that default is deliberate: a forge that has
    // no projects at all (one that keeps milestones, or nothing) must degrade to
    // doing nothing rather than failing the whole mirror over a container it does
    // not have. A platform that *does* declare projects overrides them.

    /// Put an issue on the project's board. `project` is an id on this platform.
    fn place_issue(&self, _scope: &str, _issue: &str, _project: &str) -> Result<()> {
        Ok(())
    }

    /// Take an issue off the project's board it was placed on.
    fn remove_issue(&self, _scope: &str, _issue: &str, _project: &str) -> Result<()> {
        Ok(())
    }
}
