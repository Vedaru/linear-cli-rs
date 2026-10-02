//! Pure helpers for the secondary issue fields: labels, priority, due dates and
//! assignees. Ported from linforge's `domain/fields.ts`, generalised from
//! "linear <-> forgejo" to "any two connectors", and kept free of I/O so the
//! behaviour is testable on its own.
//!
//! The priority convention is a *degradation policy*, not a fact about either
//! platform: only some platforms have a priority field, so the ones that do not
//! carry it as a `priority:*` label. That is why the translation lives here and
//! not in an adapter.

use std::collections::BTreeMap;

use crate::domain::ConnectorId;

/// Linear's priority scale, as a label, for a platform that has no priority
/// field of its own.
const PRIORITY_LABELS: [(u8, &str); 4] = [
    (1, "priority:urgent"),
    (2, "priority:high"),
    (3, "priority:medium"),
    (4, "priority:low"),
];

/// The canonical, cross-platform view of an issue's synced fields. Both sides of
/// a mapping translate into this and back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueFields {
    pub title: String,
    pub body: String,
    /// Real labels, priority labels excluded, case- and order-normalised.
    pub labels: Vec<String>,
    /// 0 means "no priority".
    pub priority: u8,
    /// `YYYY-MM-DD`, or `None` for no due date.
    pub due_date: Option<String>,
    /// Canonical assignee token, as agreed by the user map.
    pub assignee: Option<String>,
}

impl IssueFields {
    /// Everything a patch needs, computed once.
    pub fn canonical_labels(&self) -> Vec<String> {
        canonical_labels(&self.labels)
    }

    /// What to change on a target that currently holds `target`.
    ///
    /// `self` must already be the *projection* onto the target platform - otherwise
    /// a field the target cannot represent reads as a permanent difference.
    pub fn diff(&self, target: &IssueFields) -> Patch {
        diff(self, target)
    }

    /// The stable hash of every synced field.
    ///
    /// Both directions compute the same value when the two sides already agree,
    /// so a re-delivered or echoed update that changes nothing is recognised and
    /// dropped - this is the field-level loop guard, and its stability across
    /// restarts is what makes it usable as stored state.
    pub fn signature(&self) -> String {
        let body = crate::domain::markers::strip(&self.body);
        let labels = canonical_labels(&self.labels).join(",");
        let priority = self.priority.to_string();
        let assignee = self.assignee.clone().unwrap_or_default().to_lowercase();
        let parts = [
            self.title.trim(),
            body.trim_end(),
            labels.as_str(),
            priority.as_str(),
            self.due_date.as_deref().unwrap_or(""),
            assignee.as_str(),
        ];
        crate::domain::hash::sha256_hex(parts.join("\u{0}").as_bytes())
    }
}

/// What a patch does to one field.
///
/// Three states, not two, because "leave it alone" and "clear it" are different
/// requests: a partial update that omitted an unset due date would leave the old
/// one in place, which is not what the source said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change<T> {
    /// The source and the target agree; say nothing about it.
    Leave,
    /// The source holds this value and the target does not.
    Set(T),
    /// The source holds no value and the target does; say so explicitly.
    Clear,
}

impl<T> Default for Change<T> {
    /// Leaving a field alone is what a patch that does not mention it does.
    fn default() -> Self {
        Change::Leave
    }
}

impl<T> Change<T> {
    pub fn is_leave(&self) -> bool {
        matches!(self, Change::Leave)
    }

    /// The value to write, if this change writes one.
    pub fn value(&self) -> Option<&T> {
        match self {
            Change::Set(value) => Some(value),
            _ => None,
        }
    }
}

/// What to send to bring the target up to the source, field by field.
///
/// Built by [`IssueFields::diff`] and sent as-is: a field that has not changed is
/// not mentioned, so an edit to a title does not restate (and cannot clobber) the
/// labels, the due date or an assignee the target owns differently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Patch {
    pub title: Change<String>,
    pub body: Change<String>,
    pub labels: Change<Vec<String>>,
    pub priority: Change<u8>,
    pub due_date: Change<String>,
    pub assignee: Change<String>,
}

impl Patch {
    /// No field differs: there is nothing to send.
    pub fn is_empty(&self) -> bool {
        self == &Patch::default()
    }

    /// The fields this patch mentions, for a log line or a test.
    pub fn touched(&self) -> Vec<&'static str> {
        let named = [
            ("title", !self.title.is_leave()),
            ("body", !self.body.is_leave()),
            ("labels", !self.labels.is_leave()),
            ("priority", !self.priority.is_leave()),
            ("due_date", !self.due_date.is_leave()),
            ("assignee", !self.assignee.is_leave()),
        ];
        named
            .into_iter()
            .filter_map(|(name, touched)| touched.then_some(name))
            .collect()
    }
}

/// Compare two sides' fields and say what to change.
///
/// `self` is what the source *should* look like on the target (already projected),
/// `target` is what the target actually holds.
pub fn diff(source: &IssueFields, target: &IssueFields) -> Patch {
    fn change<T: PartialEq + Clone>(source: Option<&T>, target: Option<&T>) -> Change<T> {
        match (source, target) {
            (Some(left), Some(right)) if left == right => Change::Leave,
            (Some(value), _) => Change::Set(value.clone()),
            (None, Some(_)) => Change::Clear,
            (None, None) => Change::Leave,
        }
    }

    let source_labels = source.canonical_labels();
    let target_labels = target.canonical_labels();
    Patch {
        title: change(
            Some(&source.title.trim().to_string()),
            Some(&target.title.trim().to_string()),
        ),
        body: change(Some(&source.body), Some(&target.body)),
        labels: change(Some(&source_labels), Some(&target_labels)),
        priority: change(Some(&source.priority), Some(&target.priority)),
        due_date: change(source.due_date.as_ref(), target.due_date.as_ref()),
        assignee: change(source.assignee.as_ref(), target.assignee.as_ref()),
    }
}

/// True when a label is one of the synthetic `priority:*` labels.
pub fn is_priority_label(label: &str) -> bool {
    label.to_ascii_lowercase().starts_with("priority:")
}

/// A priority value as the label that carries it, if the platform needs one.
pub fn priority_to_label(priority: u8) -> Option<&'static str> {
    PRIORITY_LABELS
        .iter()
        .find(|(value, _)| *value == priority)
        .map(|(_, label)| *label)
}

/// Find a `priority:*` label in a set and read the priority out of it.
pub fn labels_to_priority(labels: &[String]) -> Option<u8> {
    labels.iter().find_map(|label| {
        let lower = label.to_ascii_lowercase();
        PRIORITY_LABELS
            .iter()
            .find(|(_, name)| *name == lower)
            .map(|(value, _)| *value)
    })
}

/// Normalise label names: drop the synthetic priority labels, lowercase, dedupe,
/// sort. Sorting is what makes the hash independent of the order a platform
/// happened to return labels in.
pub fn canonical_labels(names: &[String]) -> Vec<String> {
    let mut labels: Vec<String> = names
        .iter()
        .filter(|name| !is_priority_label(name))
        .map(|name| name.to_ascii_lowercase())
        .collect();
    labels.sort();
    labels.dedup();
    labels
}

/// Normalise a due date to the `YYYY-MM-DD` the synced platforms expect.
///
/// Accepts a full ISO timestamp (what a forge returns) or a bare date, and maps
/// both "empty" and the forge's zero timestamp (`0001-01-01T…`, which is how it
/// spells "unset") to `None`, so "no due date" round-trips as "no due date".
pub fn normalise_due_date(value: Option<&str>) -> Option<String> {
    let value = value.filter(|value| !value.is_empty())?;
    if value.starts_with("0001-01-01") {
        return None;
    }
    let date = value.get(..10)?;
    let bytes = date.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
    shaped.then(|| date.to_string())
}

/// One end of an identity mapping: which connector, and the key it is known by
/// there (an id, an email, a login - whatever the deployment finds convenient).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub connector: ConnectorId,
    pub key: String,
}

impl Identity {
    pub fn new(connector: impl Into<ConnectorId>, key: impl Into<String>) -> Self {
        Self {
            connector: connector.into(),
            key: key.into(),
        }
    }
}

/// Configured identity pairs, looked up case-insensitively in both directions.
///
/// An empty map is meaningful: it means the deployment has not said how
/// identities correspond, so assignee sync is *off* rather than guessed.
#[derive(Clone, Debug, Default)]
pub struct UserMap {
    /// One group per person: the keys they are known by, one per connector.
    entries: Vec<Vec<Identity>>,
}

impl UserMap {
    /// Groups written as pairs - the common case, and the shape tests use.
    pub fn new(pairs: impl IntoIterator<Item = (Identity, Identity)>) -> Self {
        Self {
            entries: pairs
                .into_iter()
                .map(|(left, right)| vec![left, right])
                .collect(),
        }
    }

    /// Groups of any size: a person may be known on three platforms, and pairing
    /// them up two at a time would silently pick one of the two.
    pub fn from_groups(groups: impl IntoIterator<Item = Vec<Identity>>) -> Self {
        Self {
            entries: groups.into_iter().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The counterpart of `identity`, if one is configured. Keys are compared
    /// case-insensitively, and any key configured for that connector matches.
    /// The other key in this identity's group, whichever connector it belongs to.
    pub fn counterpart(&self, identity: &Identity) -> Option<&Identity> {
        self.other_in_group(identity, None)
    }

    /// The key this identity is known by on `onto`, if the user map says.
    ///
    /// The target matters: with three platforms configured, "the counterpart" is
    /// ambiguous, and picking the first one would send a GitHub login to Forgejo.
    pub fn counterpart_for(&self, identity: &Identity, onto: &ConnectorId) -> Option<&Identity> {
        self.other_in_group(identity, Some(onto))
    }

    fn other_in_group(&self, identity: &Identity, onto: Option<&ConnectorId>) -> Option<&Identity> {
        self.entries
            .iter()
            .find(|group| group.iter().any(|known| same(known, identity)))
            .and_then(|group| {
                group.iter().find(|known| {
                    !same(known, identity) && onto.is_none_or(|onto| &known.connector == onto)
                })
            })
    }

    /// Every identity known for a connector, for validation and diagnostics.
    pub fn keys_for(&self, connector: &ConnectorId) -> Vec<&str> {
        self.entries
            .iter()
            .flat_map(|group| group.iter())
            .filter(|identity| &identity.connector == connector)
            .map(|identity| identity.key.as_str())
            .collect()
    }

    /// The mapping as configuration writes it, for `--check` output.
    pub fn describe(&self) -> BTreeMap<String, String> {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, group)| {
                let described = group
                    .iter()
                    .map(|identity| format!("{}:{}", identity.connector, identity.key))
                    .collect::<Vec<_>>()
                    .join(" <-> ");
                (format!("{index}"), described)
            })
            .collect()
    }
}

fn same(configured: &Identity, asked: &Identity) -> bool {
    configured.connector == asked.connector && configured.key.eq_ignore_ascii_case(&asked.key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(title: &str, body: &str) -> IssueFields {
        IssueFields {
            title: title.into(),
            body: body.into(),
            ..IssueFields::default()
        }
    }

    #[test]
    fn priority_round_trips_through_its_label() {
        for priority in 1..=4 {
            let label = priority_to_label(priority).expect("a label for 1..4");
            assert_eq!(labels_to_priority(&[label.to_string()]), Some(priority));
        }
        assert_eq!(priority_to_label(0), None, "0 is 'no priority'");
        assert_eq!(labels_to_priority(&["bug".into()]), None);
        assert_eq!(
            labels_to_priority(&["PRIORITY:HIGH".into()]),
            Some(2),
            "labels are matched case-insensitively"
        );
    }

    #[test]
    fn canonical_labels_drop_priority_lowercase_and_sort() {
        let labels = canonical_labels(&[
            "Bug".into(),
            "priority:high".into(),
            "bug".into(),
            "Enhancement".into(),
        ]);
        assert_eq!(labels, vec!["bug", "enhancement"]);
        assert!(is_priority_label("Priority:Low"));
        assert!(!is_priority_label("priorityish"));
    }

    #[test]
    fn due_dates_normalise_and_map_unset_to_none() {
        assert_eq!(
            normalise_due_date(Some("2026-10-02T00:00:00Z")).as_deref(),
            Some("2026-10-02")
        );
        assert_eq!(
            normalise_due_date(Some("2026-10-02")).as_deref(),
            Some("2026-10-02")
        );
        assert_eq!(normalise_due_date(Some("0001-01-01T00:00:00Z")), None);
        assert_eq!(normalise_due_date(Some("")), None);
        assert_eq!(normalise_due_date(None), None);
        assert_eq!(normalise_due_date(Some("not a date")), None);
        assert_eq!(normalise_due_date(Some("2026-1-2")), None, "must be shaped");
    }

    #[test]
    fn the_signature_ignores_trailing_space_labels_order_and_markers() {
        let mut left = fields("Title", "Body");
        left.labels = vec!["Bug".into(), "Enhancement".into()];
        let mut right = fields("Title  ", "Body\n\n<!-- linear-bridge:forgejo:12 -->");
        right.labels = vec!["enhancement".into(), "bug".into()];
        assert_eq!(left.signature(), right.signature());
    }

    #[test]
    fn the_signature_changes_for_every_synced_field() {
        let base = fields("Title", "Body");
        let mut other = base.clone();
        other.title = "Other".into();
        assert_ne!(base.signature(), other.signature());

        other = base.clone();
        other.body = "Changed".into();
        assert_ne!(base.signature(), other.signature());

        other = base.clone();
        other.labels = vec!["bug".into()];
        assert_ne!(base.signature(), other.signature());

        other = base.clone();
        other.priority = 2;
        assert_ne!(base.signature(), other.signature());

        other = base.clone();
        other.due_date = Some("2026-10-02".into());
        assert_ne!(base.signature(), other.signature());

        other = base.clone();
        other.assignee = Some("vedaru".into());
        assert_ne!(base.signature(), other.signature());

        // ...but a priority label that only restates the priority is not a change
        // of its own: it is the same field, represented the way that side can.
        other = base.clone();
        other.priority = 2;
        let mut with_label = other.clone();
        with_label.labels = vec!["priority:high".into()];
        assert_eq!(other.signature(), with_label.signature());
    }

    #[test]
    fn a_signature_is_a_stable_hex_digest() {
        let signature = fields("Title", "Body").signature();
        assert_eq!(signature.len(), 64);
        assert!(signature
            .chars()
            .all(|character| character.is_ascii_hexdigit()));
        assert_eq!(signature, fields("Title", "Body").signature());
    }

    #[test]
    fn the_user_map_resolves_both_directions_case_insensitively() {
        let map = UserMap::new([(
            Identity::new("linear", "loner@example.com"),
            Identity::new("forgejo", "vedaru"),
        )]);
        assert!(!map.is_empty());
        assert_eq!(map.len(), 1);

        let from_linear = map
            .counterpart(&Identity::new("linear", "LONER@example.com"))
            .expect("the linear key resolves");
        assert_eq!(from_linear.connector.as_str(), "forgejo");
        assert_eq!(from_linear.key, "vedaru");

        let from_forge = map
            .counterpart(&Identity::new("forgejo", "VEDARU"))
            .expect("the forgejo key resolves");
        assert_eq!(from_forge.connector.as_str(), "linear");

        // A key that is configured for a different connector must not match.
        assert_eq!(
            map.counterpart(&Identity::new("github", "vedaru")),
            None,
            "connectors are not interchangeable"
        );
        assert_eq!(map.keys_for(&ConnectorId::new("forgejo")), vec!["vedaru"]);
        assert_eq!(map.describe().len(), 1);
    }

    #[test]
    fn a_three_platform_identity_resolves_to_the_one_asked_for() {
        let map = UserMap::from_groups([vec![
            Identity::new("linear", "loner@example.com"),
            Identity::new("forgejo", "vedaru"),
            Identity::new("github", "vedaru-gh"),
        ]]);
        assert_eq!(map.len(), 1);
        assert_eq!(map.keys_for(&ConnectorId::new("linear")).len(), 1);
        assert_eq!(map.describe().len(), 1);

        let on_forge = map
            .counterpart_for(
                &Identity::new("linear", "loner@example.com"),
                &ConnectorId::new("forgejo"),
            )
            .expect("known on forgejo");
        assert_eq!(on_forge.key, "vedaru");

        let on_github = map
            .counterpart_for(
                &Identity::new("linear", "loner@example.com"),
                &ConnectorId::new("github"),
            )
            .expect("known on github");
        assert_eq!(on_github.key, "vedaru-gh");

        // A connector this person is not known on must not borrow another's key.
        assert_eq!(
            map.counterpart_for(
                &Identity::new("linear", "loner@example.com"),
                &ConnectorId::new("gitlab"),
            ),
            None
        );
    }

    #[test]
    fn an_empty_user_map_says_so() {
        let map = UserMap::default();
        assert!(map.is_empty());
        assert_eq!(map.counterpart(&Identity::new("linear", "x")), None);
    }

    #[test]
    fn the_marker_prefix_is_the_crate_wide_one() {
        assert_eq!(crate::domain::markers::MARKER_PREFIX, "linear-bridge");
    }
}
