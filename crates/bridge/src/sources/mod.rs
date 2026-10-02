//! Connectors.
//!
//! There is one engine ([`declarative`]) and one file per platform
//! ([`presets`]). Adding a platform is adding a spec - a preset in this crate, or
//! an inline `[platform.<name>.spec]` in the deployment's own configuration - and
//! never a new module here.

pub mod declarative;
pub mod presets;

/// Closing keywords shared by the forge platforms: `Fixes ENG-123` transitions
/// the issue, a bare mention only links it. A platform that spells them
/// differently overrides them in its own spec.
pub const FORGE_CLOSING_KEYWORDS: &[&str] = &[
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];
