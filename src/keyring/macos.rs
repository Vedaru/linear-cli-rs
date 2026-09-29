//! macOS keyring via the `security` binary. Port of `src/keyring/macos.ts`.
//!
//! `security` exits `44` both for "no such item" and for some malformed-query
//! cases; upstream treats 44 as not-found, and we keep that so a missing entry
//! never blocks `auth login`.

use super::{KeyringError, SERVICE};
use crate::proc::{self, ProcOutput, RunOptions};

const SECURITY: &str = "/usr/bin/security";
const ERR_SEC_ITEM_NOT_FOUND: i32 = 44;

fn run(args: &[&str]) -> Result<ProcOutput, KeyringError> {
    proc::run(
        SECURITY,
        args,
        &RunOptions::default(),
        proc::DEFAULT_TIMEOUT,
    )
    .ok_or_else(|| {
        KeyringError::new(format!(
            "Could not run {SECURITY}. The macOS keychain helper is unavailable or timed out."
        ))
    })
}

fn failure(action: &str, output: &ProcOutput) -> KeyringError {
    let code = output
        .code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".to_string());
    KeyringError::new(format!(
        "security {action} failed (exit {code}): {}",
        output.stderr_string().trim()
    ))
}

/// `security` ships with macOS, so no probe is needed.
pub fn is_available() -> bool {
    true
}

pub fn get(account: &str) -> Result<Option<String>, KeyringError> {
    let output = run(&["find-generic-password", "-a", account, "-s", SERVICE, "-w"])?;
    if !output.success {
        if output.code == Some(ERR_SEC_ITEM_NOT_FOUND) {
            return Ok(None);
        }
        return Err(failure("find-generic-password", &output));
    }
    let value = output.stdout_string().trim_end_matches('\n').to_string();
    Ok(if value.is_empty() { None } else { Some(value) })
}

pub fn set(account: &str, password: &str) -> Result<(), KeyringError> {
    // `-U` updates an existing item instead of failing with errSecDuplicateItem.
    let output = run(&[
        "add-generic-password",
        "-a",
        account,
        "-s",
        SERVICE,
        "-w",
        password,
        "-U",
    ])?;
    if !output.success {
        return Err(failure("add-generic-password", &output));
    }
    Ok(())
}

pub fn delete(account: &str) -> Result<(), KeyringError> {
    let output = run(&["delete-generic-password", "-a", account, "-s", SERVICE])?;
    if !output.success && output.code != Some(ERR_SEC_ITEM_NOT_FOUND) {
        return Err(failure("delete-generic-password", &output));
    }
    Ok(())
}
