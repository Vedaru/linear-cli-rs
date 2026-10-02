//! Entities and the references that point at them.

use crate::domain::ConnectorId;

/// What kind of thing an event is about.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntityKind {
    Issue,
    Comment,
    /// A pull request, merge request or bare commit reference.
    Reference,
    /// A container of issues - Linear's project, a forge's project. Mirrored as its
    /// own entity: it has a title and a description and nothing else the two
    /// platforms share.
    Project,
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
            EntityKind::Project => "project",
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

    /// True when both point at the same entity on the same platform.
    ///
    /// The URL is deliberately **not** part of identity. The same issue arrives
    /// with a URL in an API response and without one in a webhook payload (or with
    /// a per-delivery one), and `EntityRef`'s derived equality would then decide
    /// that a pair the bridge itself recorded is not a pair - which is exactly how
    /// a mirror ends up creating a second copy of everything it has already
    /// mirrored.
    /// The entity as an operator writes it: `connector:scope#id`, or `connector#id` when it has
    /// no scope. The scope is part of the address because an id is only unique within one - a
    /// forge issue number means nothing without the repository it is a number in.
    pub fn describe(&self) -> String {
        match &self.scope {
            Some(scope) if !scope.is_empty() => {
                format!("{}:{scope}#{}", self.connector.as_str(), self.native_id)
            }
            _ => format!("{}#{}", self.connector.as_str(), self.native_id),
        }
    }

    pub fn same_entity(&self, other: &EntityRef) -> bool {
        self.connector == other.connector
            && self.kind == other.kind
            && self.scope == other.scope
            && self.native_id == other.native_id
    }
}

/// The entity an address names: `connector:scope#id`, or `connector#id` without a scope.
///
/// The inverse of [`EntityRef::describe`], so an address a command printed can be pasted into
/// one that acts on it. The kind defaults to an issue because that is what a pairing is about:
/// the reconciler mirrors issues, and a link between two of anything else is not one it reads.
pub fn parse_entity_address(value: &str) -> Result<EntityRef, String> {
    let (where_, native_id) = value.split_once('#').ok_or_else(|| {
        format!("`{value}` must be `connector:scope#id`, e.g. `linear:VED#a-uuid`")
    })?;
    if native_id.is_empty() {
        return Err(format!("`{value}` names no entity after `#`"));
    }
    let (connector, scope) = match where_.split_once(':') {
        Some((connector, scope)) => (connector, Some(scope.to_string())),
        None => (where_, None),
    };
    if connector.is_empty() {
        return Err(format!("`{value}` names no connector"));
    }
    let mut entity = EntityRef::new(
        ConnectorId::new(connector),
        EntityKind::Issue,
        native_id.to_string(),
    );
    if let Some(scope) = scope {
        if scope.is_empty() {
            return Err(format!("`{value}` names no scope after `:`"));
        }
        entity = entity.with_scope(scope);
    }
    Ok(entity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_round_trips_through_its_description() {
        // The reason the pair exists: a command that prints an address prints one another can
        // act on, rather than one a human has to translate.
        for address in [
            "linear:VED#a-uuid",
            "forgejo:Vedaru/linear-cli-rs#7",
            "forgejo#7",
        ] {
            let entity = parse_entity_address(address).unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(entity.describe(), address);
        }
    }

    #[test]
    fn an_address_that_names_nothing_says_which_part_is_missing() {
        // Each of these is a thing someone types. Naming the part that is missing is the
        // difference between fixing it and re-reading the doc.
        for (address, expected) in [
            ("linear-VED-123", "connector:scope#id"),
            ("linear:VED#", "names no entity"),
            ("linear:#a-uuid", "names no scope"),
            (":VED#a-uuid", "names no connector"),
        ] {
            let error = parse_entity_address(address).unwrap_err();
            assert!(
                error.contains(expected),
                "`{address}` should say `{expected}`, said `{error}`"
            );
        }
    }

    #[test]
    fn a_pairing_is_about_issues() {
        // What a link is for: the reconciler mirrors issues, so an address without a kind is an
        // issue rather than a guess.
        let entity = parse_entity_address("linear:VED#a-uuid").unwrap();
        assert_eq!(entity.kind, EntityKind::Issue);
    }
}
