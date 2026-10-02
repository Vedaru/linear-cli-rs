//! Platform-neutral vocabulary. Nothing in this module may name a platform: an
//! adapter translates its own payloads into these types and keeps its native
//! shapes private.

mod capability;
mod entity;
mod event;
mod ids;
mod secret;

pub use capability::{Capabilities, Field, StateModel};
pub use entity::{Actor, EntityKind, EntityRef, Patch};
pub use event::{Action, DeliveryId, Event, EventDetail};
pub use ids::ConnectorId;
pub use secret::{parse_connector_ref, Secret};
