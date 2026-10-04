//! OS keyring access. Port of `src/keyring/`.
//!
//! Each platform has its own backend, and each is best-effort by design: a
//! missing `secret-tool` or a locked keyring must degrade into a warning (or,
//! for `auth login`, an actionable error) rather than a crash. Callers get a
//! [`KeyringError`] whose message names the failing helper so the fix is
//! obvious from the terminal.
//!
//! Availability checks are advisory. [`is_available`] returning `false` means
//! "offer the plaintext fallback", not "the keyring is definitely broken".

use std::fmt;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Keyring service name shared by every backend. Must stay `linear-cli` so
/// existing upstream entries remain readable.
pub const SERVICE: &str = "linear-cli";

/// A keyring operation failed. The message is user-facing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyringError {
    message: String,
}

impl KeyringError {
    pub fn new(message: impl Into<String>) -> Self {
        KeyringError {
            message: message.into(),
        }
    }
}

impl fmt::Display for KeyringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for KeyringError {}

// Only reached on platforms without a supported keyring.
#[allow(dead_code)]
fn unsupported() -> KeyringError {
    KeyringError::new(format!("Unsupported platform: {}", std::env::consts::OS))
}

/// Whether a keyring backend looks usable on this machine.
pub fn is_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::is_available()
    }
    #[cfg(target_os = "macos")]
    {
        macos::is_available()
    }
    #[cfg(target_os = "windows")]
    {
        windows::is_available()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

/// Read the secret for `account`, or `None` when no entry exists.
pub fn get(account: &str) -> Result<Option<String>, KeyringError> {
    #[cfg(target_os = "linux")]
    {
        linux::get(account)
    }
    #[cfg(target_os = "macos")]
    {
        macos::get(account)
    }
    #[cfg(target_os = "windows")]
    {
        windows::get(account)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = account;
        Err(unsupported())
    }
}

/// Store `password` under `account`, replacing any existing entry.
pub fn set(account: &str, password: &str) -> Result<(), KeyringError> {
    #[cfg(target_os = "linux")]
    {
        linux::set(account, password)
    }
    #[cfg(target_os = "macos")]
    {
        macos::set(account, password)
    }
    #[cfg(target_os = "windows")]
    {
        windows::set(account, password)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (account, password);
        Err(unsupported())
    }
}

/// Delete the entry for `account`. Missing entries are not an error.
pub fn delete(account: &str) -> Result<(), KeyringError> {
    #[cfg(target_os = "linux")]
    {
        linux::delete(account)
    }
    #[cfg(target_os = "macos")]
    {
        macos::delete(account)
    }
    #[cfg(target_os = "windows")]
    {
        windows::delete(account)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = account;
        Err(unsupported())
    }
}
