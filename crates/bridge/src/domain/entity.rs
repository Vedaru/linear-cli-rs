//! Entities and the references that point at them.

use crate::domain::ConnectorId;

/// What kind of thing an event is about.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntityKind {
    Issue,
    Comment,
    /// A pull request, merge request or bare commit reference.
    Reference,
    /// A kind this build does not model. Kept rather than dropped so intake can
    /// acknowledge a delivery from a newer platform version instead of failing.
    Other(String),
}

impl EntityKind {
    pub fn as_str(&self) -> &str {
        match self {
            EntityKind::Issue => "issue",
            EntityKind::Comment => "comment",
            EntityKind::Reference => "reference",
            EntityKind::Other(name) => name,
        }
    }
}

/// The person or agent on the platform that caused an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    pub id: String,
    pub name: Option<String>,
}

/// A pointer to one entity on one platform.
///
/// `scope` is the container the platform puts the entity in: a Linear team key,
/// a `owner/name` repository, a project id. The core never interprets it; it is
/// carried so a mapping can be resolved without a second API call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityRef {
    pub connector: ConnectorId,
    pub kind: EntityKind,
    pub scope: Option<String>,
    pub native_id: String,
    pub url: Option<String>,
}

impl EntityRef {
    pub fn new(connector: ConnectorId, kind: EntityKind, native_id: impl Into<String>) -> Self {
        Self {
            connector,
            kind,
            scope: None,
            native_id: native_id.into(),
            url: None,
        }
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Same as [`EntityRef::with_url`], for payloads where the platform may not
    /// carry a URL at all.
    pub fn with_optional_url(mut self, url: Option<String>) -> Self {
        self.url = url;
        self
    }
}

/// A change to apply to an entity, in platform-neutral terms.
///
/// Every field is optional because a patch is partial by nature; `None` means
/// "leave it alone", which is different from "clear it".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Patch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub labels: Option<Vec<String>>,
    pub due_date: Option<String>,
    pub priority: Option<u8>,
    pub assignees: Option<Vec<String>>,
    /// Name of the target state, as the destination platform names it.
    pub state: Option<String>,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self == &Patch::default()
    }
}
