//! The connectors this build ships: all of them are [`SourceSpec`] files.
//!
//! A preset is not a special case in the engine - it is a spec with `include_str!`
//! on it, so `type = "forgejo"` and an inline `[platform.internal]` spec run the
//! same code. That is the whole point: a platform's quirks are data.

use crate::sink::spec::SinkSpec;
use crate::sources::declarative::SourceSpec;
use crate::Error;

/// Preset names accepted in `[platform.<name>] type = "..."`.
pub const PRESETS: &[(&str, &str)] = &[
    ("linear", include_str!("../../presets/linear.toml")),
    ("forgejo", include_str!("../../presets/forgejo.toml")),
    // Codeberg and Gitea are the same API and the same webhook shape.
    ("codeberg", include_str!("../../presets/forgejo.toml")),
    ("gitea", include_str!("../../presets/forgejo.toml")),
    // The first platform that is neither Linear nor a forge: different signature header (and a
    // prefixed digest), different event actions, several assignees, no due date, no board. Read
    // half only - see the file for why its write half is not configuration.
];

/// The names a config may use, for error messages.
pub fn preset_names() -> Vec<&'static str> {
    PRESETS.iter().map(|(name, _)| *name).collect()
}

pub fn preset_text(name: &str) -> Option<&'static str> {
    PRESETS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, text)| *text)
}

/// Load a preset by name.
pub fn preset(name: &str) -> Result<SourceSpec, Error> {
    let text = preset_text(name).ok_or_else(|| {
        Error::Config(format!(
            "unknown platform type `{name}` (known: {})",
            preset_names().join(", ")
        ))
    })?;
    SourceSpec::from_toml(text)
        .map_err(|error| Error::Config(format!("the built-in `{name}` preset is invalid: {error}")))
}

/// The write half of a preset, when it has one.
///
/// Deliberately derived from the same text as [`preset`]: a platform's two
/// directions cannot drift apart if they are one file.
pub fn preset_sink(name: &str) -> Result<Option<SinkSpec>, Error> {
    Ok(preset(name)?.sink)
}

/// Every preset that ships with a sink, so a caller can see what a build can
/// write to without parsing the spec itself.
pub fn sink_preset_names() -> Vec<&'static str> {
    let mut names = Vec::new();
    for (name, text) in PRESETS {
        if SourceSpec::from_toml(text)
            .map(|spec| spec.sink.is_some())
            .unwrap_or(false)
        {
            names.push(*name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_parses_and_validates() {
        for (name, text) in PRESETS {
            let spec = SourceSpec::from_toml(text)
                .unwrap_or_else(|error| panic!("preset `{name}` is invalid: {error}"));
            assert!(!spec.event.rules.is_empty(), "preset `{name}` has no rules");
            assert!(
                !spec.describe().is_empty(),
                "preset `{name}` describes itself"
            );
            if let Some(sink) = &spec.sink {
                sink.validate()
                    .unwrap_or_else(|error| panic!("preset `{name}` sink is invalid: {error}"));
            }
        }
    }

    #[test]
    fn the_presets_that_can_write_are_known() {
        let names = sink_preset_names();
        assert!(names.contains(&"forgejo"), "{names:?}");
        assert!(names.contains(&"linear"), "{names:?}");
        // Reading is the half every platform has, so a spec may stop there. This is that
        // shape, taken from a shipped preset minus its write half - not from a platform this
        // build ships, because every one of them can be written to.
        let read_only = source_without_sink("forgejo");
        assert!(
            read_only.sink.is_none(),
            "the write half is absent, and the spec is still a connector"
        );
    }

    /// A shipped preset with everything from `[sink]` on removed: the shape of a platform a
    /// deployment can only be told about, never written to.
    fn source_without_sink(name: &str) -> SourceSpec {
        let text = preset_text(name).expect("the preset is shipped");
        let head: String = text
            .lines()
            .take_while(|line| !line.starts_with("[sink"))
            .collect::<Vec<_>>()
            .join("\n");
        SourceSpec::from_toml(&head).expect("a source-only spec is still a spec")
    }

    #[test]
    fn an_unknown_preset_lists_the_known_ones() {
        let error = preset("bitbucket").unwrap_err().to_string();
        assert!(error.contains("linear"), "{error}");
        assert!(error.contains("forgejo"), "{error}");
    }

    #[test]
    fn the_family_names_share_one_spec() {
        assert_eq!(
            preset_text("gitea"),
            preset_text("forgejo"),
            "gitea and codeberg are the forge preset"
        );
    }
}
