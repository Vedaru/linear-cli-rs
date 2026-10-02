//! Preset conformance.
//!
//! Each preset is a *description* of a platform's payloads, so the thing worth
//! testing is that the description matches real deliveries. These are the payload
//! shapes the platforms actually send, checked field by field: if a preset drifts
//! from reality, intake starts rejecting or misreading live deliveries, and this
//! is where that shows up first.

use std::sync::Arc;

use linear_bridge::connector::Source;
use linear_bridge::domain::Secret;
use linear_bridge::sources::declarative::DeclarativeSource;
use linear_bridge::sources::presets;

const SECRET: &str = "0123456789abcdef";

fn source(name: &str) -> DeclarativeSource {
    DeclarativeSource::new(
        name,
        Secret::new(SECRET),
        presets::preset(name).expect("the preset loads"),
    )
}

// --- Linear -----------------------------------------------------------------

// --- Forgejo ----------------------------------------------------------------

// --- GitHub and GitLab ------------------------------------------------------

#[test]
fn enumeration_is_derived_from_the_sink_rather_than_declared_twice() {
    // The capability follows the operation, so the two cannot disagree - and a sweep
    // asks "can I look?" rather than discovering it on the first run.
    // GitHub gained its write half (VED-25), so it is sweepable too - and the fact that
    // this list had to change is the derivation working: nothing declared it twice.
    for name in ["linear", "forgejo", "github"] {
        let preset = presets::preset(name).expect("the preset loads");
        let capabilities = preset.capabilities.resolve(preset.sink.as_ref());
        assert!(capabilities.list, "{name} declares a list operation");
        assert!(capabilities.describe().contains(&"list"), "{name}");
    }

    // The intake-only presets cannot be enumerated, and they say so: their API half
    // is a separate piece of work, and a sweep running against one must refuse
    // rather than report an empty scope.
    for name in ["gitlab"] {
        let preset = presets::preset(name).expect("the preset loads");
        assert!(preset.sink.is_none(), "{name} is intake-only today");
        // Its API half is separate work; until then a sweep must refuse rather than
        // report an empty scope.
        let capabilities = preset.capabilities.resolve(preset.sink.as_ref());
        assert!(!capabilities.list, "{name} cannot be swept");
    }
}

#[test]
fn every_preset_is_reachable_as_a_configured_platform() {
    // The point of the presets is that a deployment selects one by name; if a
    // name in the list did not resolve, `type = "<name>"` would fail at startup.
    for name in presets::preset_names() {
        let preset = presets::preset(name).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(!preset.event.rules.is_empty(), "{name} has no rules");
    }
    // And the same engine treats a hand-written spec and a preset identically.
    let inline = DeclarativeSource::new(
        "custom",
        Secret::new(SECRET),
        presets::preset("forgejo").expect("preset"),
    );
    let built_in = source("forgejo");
    assert_eq!(
        inline.capabilities(),
        built_in.capabilities(),
        "a preset is just a spec"
    );
    assert_eq!(
        Arc::strong_count(&Arc::new(inline)),
        1,
        "the source is shareable across threads"
    );
}
