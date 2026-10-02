//! Identifiers. A platform's native ids are opaque strings here; only the
//! connector that produced one may interpret it.

use std::fmt;

/// Name of a configured connector instance, e.g. `linear` or `forgejo`.
///
/// This is the *instance* name from configuration, not a type: a deployment may
/// run two Forgejo connectors (two servers) and they must not collide.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectorId(String);

impl ConnectorId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConnectorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ConnectorId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ConnectorId {
    fn from(value: String) -> Self {
        Self(value)
    }
}
