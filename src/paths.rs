//! User-level config directory resolution.
//!
//! Mirrors upstream exactly rather than delegating to `dirs`: on Windows the
//! base is `%APPDATA%` (Roaming); everywhere else it is `$XDG_CONFIG_HOME` or
//! `$HOME/.config`. Notably that means macOS uses `~/.config/linear/`, **not**
//! `~/Library/Application Support/` — matching `config.ts` and
//! `credentials.ts` so an existing `linear.toml` is found by this port.

use std::path::PathBuf;

/// Base directory for user-level configuration, or `None` when no relevant
/// environment variable is set (an unusual, but survivable, environment).
pub fn config_home() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join(".config"))
            })
    }
}

/// Path to a file under the `linear/` subdirectory of [`config_home`].
pub fn config_file(name: &str) -> Option<PathBuf> {
    config_home().map(|dir| dir.join("linear").join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_file_is_under_linear_dir() {
        // The test environment may or may not have HOME/APPDATA set; only
        // assert the shape when a base directory is resolvable.
        if let Some(path) = config_file("credentials.toml") {
            assert!(path.ends_with("linear/credentials.toml"));
        }
    }
}
