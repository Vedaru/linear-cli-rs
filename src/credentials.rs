//! Multi-workspace credential storage. Port of `src/credentials.ts`.
//!
//! Two on-disk formats are supported, and the file tells us which one it is:
//!
//! * **Inline (plaintext)** — keys live in `credentials.toml` directly as
//!   `workspace = "lin_api_..."`. No OS keyring needed; suited to servers.
//! * **Keyring** — the file holds only `workspaces = [...]` and a `default`,
//!   while the secrets live in the OS keyring (see [`crate::keyring`]).
//!
//! Reads are forgiving: a missing file is an empty store, an unreadable
//! keyring entry is a warning rather than a hard failure. Writes are not: a
//! requested change either lands on disk or returns an error, and the file is
//! replaced atomically with mode `0600` via [`crate::fsutil::write_atomic`].
//!
//! State is process-global and loaded once. Upstream loads it at module import;
//! here the first accessor triggers the same load, and [`ensure_loaded`] is the
//! entry point used by commands that need credentials before doing anything.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::errors::{CliError, Result};
use crate::fsutil;
use crate::keyring;
use crate::output;

/// Which workspaces are configured, and which one is the default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credentials {
    pub default: Option<String>,
    pub workspaces: Vec<String>,
}

struct State {
    credentials: Credentials,
    is_inline_format: bool,
    api_key_cache: HashMap<String, String>,
    loaded: bool,
    load_error: Option<String>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            credentials: Credentials::default(),
            is_inline_format: false,
            api_key_cache: HashMap::new(),
            loaded: false,
            load_error: None,
        })
    })
}

fn lock_state() -> MutexGuard<'static, State> {
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// --- Paths ---

/// Path to `credentials.toml`, or `None` when no config home is resolvable.
///
/// Windows uses `%APPDATA%\linear\credentials.toml`; everything else uses
/// `$XDG_CONFIG_HOME/linear/credentials.toml` or `~/.config/linear/...`.
pub fn credentials_path() -> Option<PathBuf> {
    crate::paths::config_file("credentials.toml")
}

// --- Parsing ---

/// Inline format is detected by a non-`default`, non-`workspaces` key whose
/// value is a string. A `workspaces` key means keyring format.
fn has_inline_keys(parsed: &toml::Table) -> bool {
    for (key, value) in parsed {
        if key == "default" {
            continue;
        }
        if key == "workspaces" {
            return false;
        }
        if value.is_str() {
            return true;
        }
    }
    false
}

fn parse_inline(parsed: &toml::Table, cache: &mut HashMap<String, String>) -> Credentials {
    let mut workspaces = Vec::new();
    for (key, value) in parsed {
        if key == "default" {
            continue;
        }
        if let Some(secret) = value.as_str() {
            workspaces.push(key.clone());
            cache.insert(key.clone(), secret.to_string());
        }
    }
    Credentials {
        default: parsed
            .get("default")
            .and_then(|v| v.as_str())
            .map(String::from),
        workspaces,
    }
}

fn parse_keyring(parsed: &toml::Table) -> Credentials {
    let mut seen = HashSet::new();
    let workspaces: Vec<String> = parsed
        .get("workspaces")
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str())
                .filter(|name| seen.insert((*name).to_string()))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let declared_default = parsed
        .get("default")
        .and_then(|v| v.as_str())
        .map(String::from);
    let default_is_valid = declared_default
        .as_ref()
        .is_some_and(|name| workspaces.contains(name));

    if let Some(name) = declared_default.as_ref().filter(|_| !default_is_valid) {
        output::warn(&format!(
            "Default workspace \"{name}\" is not in the workspaces list. \
             Run `linear auth default <workspace>` to set a valid default."
        ));
    }

    Credentials {
        default: if default_is_valid {
            declared_default
        } else {
            None
        },
        workspaces,
    }
}

fn populate_keyring_cache(workspaces: &[String], cache: &mut HashMap<String, String>) {
    for workspace in workspaces {
        match keyring::get(workspace) {
            Ok(Some(secret)) => {
                cache.insert(workspace.clone(), secret);
            }
            Ok(None) => output::warn(&format!(
                "No keyring entry for workspace \"{workspace}\". \
                 Run `linear auth login` to re-authenticate."
            )),
            Err(error) => output::warn(&format!(
                "Failed to read keyring for workspace \"{workspace}\": {error}"
            )),
        }
    }
}

// --- Serialization ---

/// `default` first, then workspaces alphabetically — a stable file that diffs
/// cleanly in version control.
fn serialize_keyring(credentials: &Credentials) -> String {
    let mut table = toml::Table::new();
    if let Some(default) = &credentials.default {
        table.insert("default".to_string(), toml::Value::String(default.clone()));
    }
    let mut workspaces = credentials.workspaces.clone();
    workspaces.sort();
    table.insert(
        "workspaces".to_string(),
        toml::Value::Array(workspaces.into_iter().map(toml::Value::String).collect()),
    );
    toml::to_string(&table).unwrap_or_default()
}

/// Inline serialization needs a secret for every listed workspace. A missing
/// key is an error rather than a silently dropped workspace, which would
/// otherwise look like the workspace was never configured.
fn serialize_inline(
    credentials: &Credentials,
    cache: &HashMap<String, String>,
    override_key: Option<(&str, &str)>,
) -> Result<String> {
    let mut table = toml::Table::new();
    if let Some(default) = &credentials.default {
        table.insert("default".to_string(), toml::Value::String(default.clone()));
    }
    let mut workspaces = credentials.workspaces.clone();
    workspaces.sort();
    for workspace in workspaces {
        let secret = match override_key {
            Some((name, value)) if name == workspace => Some(value.to_string()),
            _ => cache.get(&workspace).cloned(),
        };
        let Some(secret) = secret else {
            return Err(CliError::cli(format!(
                "Cannot save inline credentials: API key for workspace \"{workspace}\" is missing from cache"
            )));
        };
        table.insert(workspace, toml::Value::String(secret));
    }
    Ok(toml::to_string(&table).unwrap_or_default())
}

// --- Loading / saving ---

fn load_into(state: &mut State) -> Result<()> {
    state.loaded = true;
    state.load_error = None;
    state.api_key_cache.clear();
    state.credentials = Credentials::default();
    state.is_inline_format = false;

    let Some(path) = credentials_path() else {
        return Ok(());
    };

    let file = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            let message = format!(
                "Failed to read credentials file at {}: {error}",
                path.display()
            );
            state.load_error = Some(message.clone());
            return Err(CliError::cli(message));
        }
    };

    let parsed: toml::Table = match toml::from_str(&file) {
        Ok(parsed) => parsed,
        Err(error) => {
            let message = format!(
                "Failed to parse credentials file at {}. The file may be corrupted.\n\
                 You can delete it and re-authenticate with `linear auth login`.\n\
                 Parse error: {error}",
                path.display()
            );
            state.load_error = Some(message.clone());
            return Err(CliError::cli(message));
        }
    };

    if has_inline_keys(&parsed) {
        state.is_inline_format = true;
        let parsed_credentials = parse_inline(&parsed, &mut state.api_key_cache);
        state.credentials = parsed_credentials;
        return Ok(());
    }

    state.credentials = parse_keyring(&parsed);
    let workspaces = state.credentials.workspaces.clone();
    populate_keyring_cache(&workspaces, &mut state.api_key_cache);
    Ok(())
}

/// Load credentials once, returning the cached error on subsequent calls if the
/// file could not be read or parsed.
pub fn ensure_loaded() -> Result<()> {
    let mut state = lock_state();
    if state.loaded {
        return match &state.load_error {
            Some(message) => Err(CliError::cli(message.clone())),
            None => Ok(()),
        };
    }
    load_into(&mut state)
}

fn write_credentials(text: &str) -> Result<()> {
    let path =
        credentials_path().ok_or_else(|| CliError::cli("Could not determine credentials path"))?;
    fsutil::write_atomic(&path, text).map_err(|error| {
        CliError::cli(format!(
            "Failed to write credentials file at {}: {error}",
            path.display()
        ))
    })
}

fn save_keyring(state: &State) -> Result<()> {
    write_credentials(&serialize_keyring(&state.credentials))
}

fn save_inline(state: &State, override_key: Option<(&str, &str)>) -> Result<()> {
    let text = serialize_inline(&state.credentials, &state.api_key_cache, override_key)?;
    write_credentials(&text)
}

// --- Queries ---

/// API key for `workspace`, or for the default workspace when `None`.
///
/// An explicitly named workspace with no cached key returns `None`; it does not
/// fall back to the default.
pub fn get_credential_api_key(workspace: Option<&str>) -> Option<String> {
    let state = lock_state();
    match workspace {
        Some(name) => state.api_key_cache.get(name).cloned(),
        None => state
            .credentials
            .default
            .as_ref()
            .and_then(|name| state.api_key_cache.get(name).cloned()),
    }
}

/// The default workspace slug, if one is configured.
pub fn get_default_workspace() -> Option<String> {
    lock_state().credentials.default.clone()
}

/// All configured workspaces, in file order.
pub fn get_workspaces() -> Vec<String> {
    lock_state().credentials.workspaces.clone()
}

/// Whether `workspace` is configured.
pub fn has_workspace(workspace: &str) -> bool {
    lock_state()
        .credentials
        .workspaces
        .iter()
        .any(|w| w == workspace)
}

/// Whether the credentials file is in the inline (plaintext) format.
pub fn is_using_inline_format() -> bool {
    lock_state().is_inline_format
}

// --- Mutations ---

/// Move every inline credential into the OS keyring and rewrite the file in
/// keyring format. Already-written entries are rolled back if a later one
/// fails, so the keyring is never left holding a partial set.
pub fn migrate_to_keyring() -> Result<Vec<String>> {
    ensure_loaded()?;
    let mut state = lock_state();
    if !state.is_inline_format {
        return Ok(Vec::new());
    }

    let mut migrated: Vec<String> = Vec::new();
    for workspace in state.credentials.workspaces.clone() {
        let Some(secret) = state.api_key_cache.get(&workspace).cloned() else {
            continue;
        };
        match keyring::set(&workspace, &secret) {
            Ok(()) => migrated.push(workspace),
            Err(error) => {
                for written in &migrated {
                    let _ = keyring::delete(written);
                }
                return Err(CliError::cli(format!(
                    "Failed to store API key in system keyring for workspace \"{workspace}\": {error}. \
                     Rolled back {} already-written entries.",
                    migrated.len()
                )));
            }
        }
    }

    state.is_inline_format = false;
    save_keyring(&state)?;
    Ok(migrated)
}

/// Add or update a credential. The first workspace added becomes the default.
///
/// `plaintext` selects the storage format: `Some(true)` forces inline, and
/// `Some(false)` forces the keyring (migrating any existing inline keys first,
/// so nothing is lost). `None` preserves the file's current format.
pub fn add_credential(workspace: &str, api_key: &str, plaintext: Option<bool>) -> Result<()> {
    ensure_loaded()?;
    let mut state = lock_state();
    let use_inline = plaintext.unwrap_or(state.is_inline_format);

    // Explicitly asking for keyring storage while the file is inline: move the
    // whole file over in one step rather than leaving it half-migrated.
    if plaintext == Some(false) && state.is_inline_format {
        state
            .api_key_cache
            .insert(workspace.to_string(), api_key.to_string());
        record_workspace(&mut state, workspace);

        for name in state.credentials.workspaces.clone() {
            let Some(secret) = state.api_key_cache.get(&name).cloned() else {
                continue;
            };
            keyring::set(&name, &secret).map_err(|error| {
                CliError::cli(format!(
                    "Failed to store API key in system keyring for workspace \"{name}\": {error}"
                ))
            })?;
        }

        state.is_inline_format = false;
        return save_keyring(&state);
    }

    if !use_inline {
        keyring::set(workspace, api_key).map_err(|error| {
            CliError::cli(format!(
                "Failed to store API key in system keyring for workspace \"{workspace}\": {error}"
            ))
        })?;
    }

    state
        .api_key_cache
        .insert(workspace.to_string(), api_key.to_string());
    record_workspace(&mut state, workspace);

    if use_inline {
        save_inline(&state, Some((workspace, api_key)))
    } else {
        save_keyring(&state)
    }
}

/// Register `workspace` if new, and promote it to default when it is the first
/// workspace ever configured.
fn record_workspace(state: &mut State, workspace: &str) {
    let is_new = !state.credentials.workspaces.iter().any(|w| w == workspace);
    if is_new {
        state.credentials.workspaces.push(workspace.to_string());
    }
    if is_new && state.credentials.workspaces.len() == 1 {
        state.credentials.default = Some(workspace.to_string());
    }
}

/// Remove a credential and its keyring entry, reassigning the default if the
/// removed workspace held it.
pub fn remove_credential(workspace: &str) -> Result<()> {
    ensure_loaded()?;
    let mut state = lock_state();

    if !state.is_inline_format {
        keyring::delete(workspace).map_err(|error| {
            CliError::cli(format!(
                "Failed to remove API key from system keyring for workspace \"{workspace}\": {error}"
            ))
        })?;
    }

    state.api_key_cache.remove(workspace);
    state.credentials.workspaces.retain(|w| w != workspace);
    if state.credentials.default.as_deref() == Some(workspace) {
        state.credentials.default = state.credentials.workspaces.first().cloned();
    }

    if state.is_inline_format {
        save_inline(&state, None)
    } else {
        save_keyring(&state)
    }
}

/// Change the default workspace.
pub fn set_default_workspace(workspace: &str) -> Result<()> {
    ensure_loaded()?;
    let mut state = lock_state();

    if !state.credentials.workspaces.iter().any(|w| w == workspace) {
        return Err(CliError::cli(format!(
            "Workspace \"{workspace}\" not found in credentials"
        )));
    }
    state.credentials.default = Some(workspace.to_string());

    if state.is_inline_format {
        save_inline(&state, None)
    } else {
        save_keyring(&state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(source: &str) -> toml::Table {
        toml::from_str(source).unwrap()
    }

    #[test]
    fn detects_inline_format() {
        assert!(has_inline_keys(&table("acme = \"lin_api_x\"")));
        assert!(has_inline_keys(&table("default = \"acme\"\nacme = \"x\"")));
        assert!(!has_inline_keys(&table("workspaces = [\"acme\"]")));
        assert!(!has_inline_keys(&table("default = \"acme\"")));
    }

    #[test]
    fn parses_inline_keys_and_default() {
        let mut cache = HashMap::new();
        let credentials = parse_inline(
            &table("default = \"beta\"\nalpha = \"key-a\"\nbeta = \"key-b\""),
            &mut cache,
        );
        assert_eq!(credentials.default.as_deref(), Some("beta"));
        assert_eq!(credentials.workspaces, vec!["alpha", "beta"]);
        assert_eq!(cache.get("alpha").map(String::as_str), Some("key-a"));
        assert_eq!(cache.get("beta").map(String::as_str), Some("key-b"));
    }

    #[test]
    fn keyring_default_must_be_in_workspace_list() {
        let credentials = parse_keyring(&table(
            "default = \"ghost\"\nworkspaces = [\"alpha\", \"beta\"]",
        ));
        assert_eq!(credentials.default, None);

        let credentials = parse_keyring(&table(
            "default = \"beta\"\nworkspaces = [\"alpha\", \"beta\"]",
        ));
        assert_eq!(credentials.default.as_deref(), Some("beta"));
    }

    #[test]
    fn keyring_workspaces_are_deduped_in_order() {
        let credentials = parse_keyring(&table("workspaces = [\"beta\", \"alpha\", \"beta\"]"));
        assert_eq!(credentials.workspaces, vec!["beta", "alpha"]);
    }

    #[test]
    fn keyring_serialization_puts_default_first_then_sorts() {
        let credentials = Credentials {
            default: Some("beta".to_string()),
            workspaces: vec!["gamma".to_string(), "alpha".to_string(), "beta".to_string()],
        };
        let text = serialize_keyring(&credentials);
        let round_trip: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(
            round_trip.get("default").and_then(|v| v.as_str()),
            Some("beta")
        );
        let workspaces: Vec<&str> = round_trip
            .get("workspaces")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(workspaces, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn inline_serialization_round_trips() {
        // Serialization writes workspaces sorted, mirroring upstream
        // `saveInlineCredentials()`, so an unsorted in-memory list comes back
        // normalized rather than in its original order.
        let credentials = Credentials {
            default: Some("alpha".to_string()),
            workspaces: vec!["beta".to_string(), "alpha".to_string()],
        };
        let mut cache = HashMap::new();
        cache.insert("alpha".to_string(), "key-a".to_string());
        cache.insert("beta".to_string(), "key-b".to_string());

        let text = serialize_inline(&credentials, &cache, None).unwrap();
        let mut reloaded_cache = HashMap::new();
        let reloaded = parse_inline(&toml::from_str(&text).unwrap(), &mut reloaded_cache);
        assert_eq!(
            reloaded,
            Credentials {
                default: Some("alpha".to_string()),
                workspaces: vec!["alpha".to_string(), "beta".to_string()],
            }
        );
        assert_eq!(reloaded_cache, cache);
    }

    #[test]
    fn inline_serialization_uses_override_key() {
        let credentials = Credentials {
            default: Some("alpha".to_string()),
            workspaces: vec!["alpha".to_string()],
        };
        let cache = HashMap::new();
        let text = serialize_inline(&credentials, &cache, Some(("alpha", "fresh"))).unwrap();
        assert!(text.contains("fresh"));
    }

    #[test]
    fn inline_serialization_errors_on_missing_key() {
        let credentials = Credentials {
            default: None,
            workspaces: vec!["alpha".to_string()],
        };
        let error = serialize_inline(&credentials, &HashMap::new(), None).unwrap_err();
        assert!(error.to_string().contains("missing from cache"));
    }
}
