//! What a target platform can actually hold.
//!
//! The mirror's rule is to compare what can be made *equal*, not what the source
//! happens to say. A field the target cannot represent - or an identity nobody said
//! how to translate - is dropped **before** the comparison, so it cannot read as a
//! difference that never converges.
//!
//! That is the drift this module exists to stop. Compared raw, an unwritable
//! assignee differs from the target on every single delivery: the bridge rewrites
//! the same content forever, and the two sides never agree. Projected first, the
//! same fields compare equal and the bridge has nothing to do - which is the
//! truthful answer, and it is logged rather than silent.

use std::fmt;

use crate::domain::{Capabilities, ConnectorId, Identity, IssueFields, UserMap};

/// A field the target could not be given, and why.
///
/// Reported, never dropped quietly: "the assignee did not come across" is
/// something an operator has to be able to find out from the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub field: &'static str,
    pub value: String,
    pub reason: Reason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The platform has no such field at all.
    Unsupported,
    /// The identity map exists but does not know this value for that platform.
    Unmapped,
    /// No identity map is configured, so assignee syncing is off rather than
    /// guessed - translating by hope would send one platform's login to another.
    NoIdentityMap,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Unsupported => "unsupported",
            Reason::Unmapped => "unmapped",
            Reason::NoIdentityMap => "no identity map",
        }
    }
}

impl Skipped {
    fn new(field: &'static str, value: impl Into<String>, reason: Reason) -> Self {
        Self {
            field,
            value: value.into(),
            reason,
        }
    }
}

impl fmt::Display for Skipped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} `{}` left behind: {}",
            self.field,
            self.value,
            self.reason.as_str()
        )
    }
}

/// The fields as the target will hold them, plus what could not travel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projected {
    pub fields: IssueFields,
    pub skipped: Vec<Skipped>,
}

impl Projected {
    pub fn is_degraded(&self) -> bool {
        !self.skipped.is_empty()
    }
}

/// What one platform can hold, and how identities translate onto it.
#[derive(Clone, Copy, Debug)]
pub struct Projection<'a> {
    capabilities: &'a Capabilities,
    users: &'a UserMap,
}

impl<'a> Projection<'a> {
    pub fn new(capabilities: &'a Capabilities, users: &'a UserMap) -> Self {
        Self {
            capabilities,
            users,
        }
    }

    /// The source's fields as `onto` will hold them.
    ///
    /// `from` is only needed for the identity lookup: a token means nothing
    /// without the platform it was read from.
    pub fn of(&self, fields: &IssueFields, from: &ConnectorId, onto: &ConnectorId) -> Projected {
        let mut projected = fields.clone();
        let mut skipped = Vec::new();

        if !self.capabilities.labels && !fields.labels.is_empty() {
            projected.labels = Vec::new();
            skipped.push(Skipped::new(
                "labels",
                format!("{} label(s)", fields.labels.len()),
                Reason::Unsupported,
            ));
        }
        if !self.capabilities.due_dates && fields.due_date.is_some() {
            projected.due_date = None;
            skipped.push(Skipped::new(
                "due date",
                fields.due_date.clone().unwrap_or_default(),
                Reason::Unsupported,
            ));
        }
        // Priority is never dropped: a platform without the field carries it as a
        // `priority:*` label, which is exactly what the read half reads back.
        if let Some(assignee) = &fields.assignee {
            match self.identity(assignee, from, onto) {
                Some(key) => projected.assignee = Some(key),
                None => {
                    projected.assignee = None;
                    let reason = if self.users.is_empty() {
                        Reason::NoIdentityMap
                    } else {
                        Reason::Unmapped
                    };
                    skipped.push(Skipped::new("assignee", assignee.clone(), reason));
                }
            }
        }

        Projected {
            fields: projected,
            skipped,
        }
    }

    fn identity(&self, token: &str, from: &ConnectorId, onto: &ConnectorId) -> Option<String> {
        self.users
            .counterpart_for(&Identity::new(from.clone(), token), onto)
            .map(|identity| identity.key.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::StateModel;

    fn forgejo() -> Capabilities {
        Capabilities {
            states: StateModel::OpenClosed,
            list: true,
            labels: true,
            due_dates: true,
            priorities: false,
            multiple_assignees: false,
            native_pull_requests: true,
            deletion: false,
        }
    }

    fn minimal() -> Capabilities {
        Capabilities {
            labels: false,
            due_dates: false,
            ..forgejo()
        }
    }

    fn full_fields() -> IssueFields {
        IssueFields {
            title: "Title".into(),
            body: "Body".into(),
            labels: vec!["bug".into()],
            priority: 2,
            due_date: Some("2026-10-02".into()),
            assignee: Some("loner@example.com".into()),
        }
    }

    fn linear() -> ConnectorId {
        ConnectorId::new("linear")
    }

    fn forge() -> ConnectorId {
        ConnectorId::new("forgejo")
    }

    #[test]
    fn a_platform_without_the_field_does_not_get_it_and_says_so() {
        let (caps, users) = (minimal(), UserMap::default());
        let projection = Projection::new(&caps, &users);
        let projected = projection.of(&full_fields(), &linear(), &forge());

        assert!(projected.fields.labels.is_empty());
        assert_eq!(projected.fields.due_date, None);
        assert!(projected.is_degraded());
        let reasons: Vec<&str> = projected
            .skipped
            .iter()
            .map(|skipped| skipped.field)
            .collect();
        assert!(reasons.contains(&"labels"), "{reasons:?}");
        assert!(reasons.contains(&"due date"), "{reasons:?}");
        // Title, body and priority are always representable: priority travels as a
        // label on a platform that has no field for it.
        assert_eq!(projected.fields.title, "Title");
        assert_eq!(projected.fields.priority, 2);
    }

    #[test]
    fn an_identity_the_map_does_not_know_is_left_behind_not_guessed() {
        let map = UserMap::new([(
            Identity::new("linear", "someone@example.com"),
            Identity::new("forgejo", "someone"),
        )]);
        let caps = forgejo();
        let projection = Projection::new(&caps, &map);
        let projected = projection.of(&full_fields(), &linear(), &forge());

        assert_eq!(projected.fields.assignee, None);
        assert_eq!(projected.skipped.len(), 1);
        assert_eq!(projected.skipped[0].field, "assignee");
        assert_eq!(projected.skipped[0].reason, Reason::Unmapped);
        assert_eq!(
            projected.skipped[0].to_string(),
            "assignee `loner@example.com` left behind: unmapped"
        );
    }

    #[test]
    fn no_map_at_all_means_assignee_sync_is_off_rather_than_guessed() {
        let (caps, users) = (forgejo(), UserMap::default());
        let projection = Projection::new(&caps, &users);
        let projected = projection.of(&full_fields(), &linear(), &forge());

        assert_eq!(projected.fields.assignee, None);
        assert_eq!(projected.skipped[0].reason, Reason::NoIdentityMap);
    }

    #[test]
    fn a_configured_identity_arrives_as_the_targets_own_key() {
        let map = UserMap::new([(
            Identity::new("linear", "loner@example.com"),
            Identity::new("forgejo", "vedaru"),
        )]);
        let caps = forgejo();
        let projection = Projection::new(&caps, &map);

        let onto_forge = projection.of(&full_fields(), &linear(), &forge());
        assert_eq!(onto_forge.fields.assignee.as_deref(), Some("vedaru"));
        assert!(!onto_forge.is_degraded());

        // ...and the same identity resolves backwards, which is what makes a
        // bidirectional mapping converge.
        let mut on_linear = full_fields();
        on_linear.assignee = Some("vedaru".into());
        let linear_caps = Capabilities {
            states: StateModel::Named,
            priorities: true,
            ..forgejo()
        };
        let projection = Projection::new(&linear_caps, &map);
        let onto_linear = projection.of(&on_linear, &forge(), &linear());
        assert_eq!(
            onto_linear.fields.assignee.as_deref(),
            Some("loner@example.com")
        );
    }

    #[test]
    fn projecting_twice_changes_nothing() {
        // The projection is applied to whatever each side holds, so it has to be
        // idempotent: otherwise a projected side would project to something else
        // again and the two sides would chase each other.
        let (caps, users) = (forgejo(), UserMap::default());
        let projection = Projection::new(&caps, &users);
        let once = projection.of(&full_fields(), &linear(), &forge());
        let twice = projection.of(&once.fields, &linear(), &forge());
        assert_eq!(once.fields, twice.fields);
        assert!(!twice.is_degraded(), "the second pass has nothing to drop");
    }
}
