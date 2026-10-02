//! Connectors.
//!
//! There is one engine ([`declarative`]) and one file per platform
//! ([`presets`]). Adding a platform is adding a spec - a preset in this crate, or
//! an inline `[platform.<name>.spec]` in the deployment's own configuration - and
//! never a new module here.
//!
//! The default closing keywords a reference rule uses live in
//! [`crate::domain::references`], beside the scanner that reads them.

pub mod declarative;
pub mod presets;
