//! Platform-neutral vocabulary. Nothing in this module may name a platform: an
//! adapter translates its own payloads into these types and keeps its native
//! shapes private.

mod capability;
mod entity;
mod event;
mod fields;
pub mod hash;
mod ids;
pub mod markers;
pub mod references;
mod secret;

pub use capability::{Capabilities, Field, StateModel};
pub use entity::{Actor, EntityKind, EntityRef};
pub use event::{Action, DeliveryId, Event, EventDetail};
pub use fields::{
    canonical_labels, diff, due_date_to_label, is_due_date_label, is_priority_label,
    labels_to_due_date, labels_to_priority, normalise_due_date, priority_to_label, Change,
    Identity, IssueFields, Patch, UserMap,
};
pub use ids::ConnectorId;
pub use markers::{OriginMarker, MARKER_PREFIX};
pub use references::{Reference, ReferenceKind};
pub use secret::{parse_connector_ref, Secret};

/// Mined issue references, re-exported under the name the caller wants.
pub use references::extract as extract_references;
pub use references::filter_by_team_keys;
