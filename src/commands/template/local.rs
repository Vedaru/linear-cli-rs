//! Local templates: the ones this machine keeps, as files.
//!
//! One TOML file per template under `<config>/linear/templates/<name>.toml`, with the field names
//! `issue create` takes as flags. Three rules are load-bearing:
//!
//! * **A name is a file name**, so it is validated rather than joined: a template called
//!   `../../linear.toml` must not be able to read or write anything outside the directory. The
//!   check is an allow-list of characters plus a refusal of `.`-leading names and `..`, because a
//!   deny-list of separators is the version that misses one.
//! * **Writes are atomic** ([`crate::atomic`]): a half-written template is a template that will be
//!   applied to somebody's issue, and the failure would surface far from its cause.
//! * **A missing template is not an error here.** [`find`] answers `None` so the caller can decide
//!   what a name means when it is not a local template - `issue create --template` falls through to
//!   the workspace's templates, and `template show` reports it missing.

use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::errors::{CliError, Result};
use crate::paths;

/// A local template that exists: its name and its fields.
pub struct Local {
    pub name: String,
    pub fields: Map<String, Value>,
}

/// The directory local templates live in.
fn directory() -> Result<PathBuf> {
    let file = paths::config_file("templates/.keep").ok_or_else(|| {
        CliError::cli("Cannot tell where the config directory is")
            .suggestion("Set XDG_CONFIG_HOME (or HOME) so local templates have a place to live.")
    })?;
    Ok(file.parent().map(PathBuf::from).unwrap_or_default())
}

/// The file a template name refers to, refusing a name that is not one.
pub fn path_of(name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(directory()?.join(format!("{name}.toml")))
}

/// A template name is a file name: letters, digits, `.`, `_` and `-`, starting with a letter or
/// a digit, no `..` anywhere, at most 64 characters, and not already ending in `.toml` - the file
/// is `<name>.toml`, so `--name bug.toml` would write `bug.toml.toml`, which is a slip worth
/// naming rather than a name worth accepting.
pub fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && !name.to_ascii_lowercase().ends_with(".toml")
        && name
            .chars()
            .next()
            .map(|first| first.is_ascii_alphanumeric())
            .unwrap_or(false)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.contains("..");

    if valid {
        return Ok(());
    }
    Err(CliError::validation(format!(
        "\"{name}\" is not a usable template name"
    ))
    .suggestion(
        "Use letters, digits, dots, dashes and underscores, starting with a letter or digit - a template name is also the name of its file.",
    ))
}

/// The local template with this name, if this machine has one.
pub fn find(name: &str) -> Result<Option<Local>> {
    // An unusable name is not a local template - `issue create --template` still has the
    // workspace's templates to try, and refusing here would break a name Linear accepts.
    if validate_name(name).is_err() {
        return Ok(None);
    }
    let path = path_of(name)?;
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    Ok(Some(Local {
        name: name.to_string(),
        fields: parse(&text, name)?,
    }))
}

/// Write a template, creating the directory on the way.
pub fn write(name: &str, fields: &Map<String, Value>) -> Result<PathBuf> {
    let path = path_of(name)?;
    let directory = path.parent().map(PathBuf::from).unwrap_or_default();
    std::fs::create_dir_all(&directory).map_err(|error| {
        CliError::cli(format!("Failed to create {}: {error}", directory.display()))
    })?;

    let document = Value::Object(fields.clone());
    let text = toml::to_string(&document)
        .map_err(|error| CliError::cli(format!("Failed to write the template as TOML: {error}")))?;
    crate::atomic::write(&path, text.as_bytes())?;
    Ok(path)
}

/// Delete a template. A name that has no file is reported, so a typo is not silence.
pub fn remove(name: &str) -> Result<PathBuf> {
    let path = path_of(name)?;
    if !path.exists() {
        return Err(CliError::not_found("Local template", name)
            .suggestion("Run `linear template show <name>` to see what this machine has."));
    }
    std::fs::remove_file(&path)
        .map_err(|error| CliError::cli(format!("Failed to delete {}: {error}", path.display())))?;
    Ok(path)
}

/// A template file as fields, with a parse failure naming the file rather than the line.
pub fn parse(text: &str, name: &str) -> Result<Map<String, Value>> {
    let parsed: toml::Value = toml::from_str(text).map_err(|error| {
        CliError::validation(format!(
            "The local template \"{name}\" is not valid TOML: {error}"
        ))
        .suggestion(format!(
            "Edit {}",
            path_of(name)
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ))
    })?;
    let value = serde_json::to_value(parsed).map_err(|error| {
        CliError::cli(format!("Failed to read the template \"{name}\": {error}"))
    })?;
    match value {
        Value::Object(fields) => Ok(fields),
        _ => Err(CliError::validation(format!(
            "The local template \"{name}\" is not a table of fields"
        ))),
    }
}

/// The fields of an issue-create invocation as template fields, for `template create`'s flags.
pub fn fields_from_flags(pairs: Vec<(&str, Value)>) -> Map<String, Value> {
    let mut fields = Map::new();
    for (key, value) in pairs {
        if value.is_null() {
            continue;
        }
        if let Value::String(text) = &value {
            if text.is_empty() {
                continue;
            }
        }
        fields.insert(key.to_string(), value);
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_could_escape_the_directory_is_refused() {
        assert!(validate_name("bug-report").is_ok());
        assert!(validate_name("v1.2_final").is_ok());
        for bad in [
            "../linear",
            "..",
            "a/b",
            "a\\b",
            ".hidden",
            "",
            "with space",
            "name.toml",
        ] {
            assert!(validate_name(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn a_template_round_trips_through_toml() {
        let mut fields = Map::new();
        fields.insert("title".to_string(), Value::String("Bug: ".to_string()));
        fields.insert("priority".to_string(), Value::Number(2.into()));
        fields.insert(
            "labels".to_string(),
            Value::Array(vec![Value::String("Bug".to_string())]),
        );
        let text = toml::to_string(&Value::Object(fields.clone())).expect("toml");
        let parsed = parse(&text, "sample").expect("parse");
        assert_eq!(parsed["title"], Value::String("Bug: ".to_string()));
        assert_eq!(parsed["priority"], Value::Number(2.into()));
        assert_eq!(parsed["labels"][0], Value::String("Bug".to_string()));
    }

    #[test]
    fn an_empty_field_is_not_written_into_a_template() {
        let fields = fields_from_flags(vec![
            ("title", Value::String("Kept".to_string())),
            ("description", Value::String(String::new())),
            ("priority", Value::Null),
        ]);
        assert!(fields.contains_key("title"));
        assert!(!fields.contains_key("description"));
        assert!(!fields.contains_key("priority"));
    }
}
