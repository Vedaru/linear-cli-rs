//! Linux keyring via `secret-tool` (libsecret). Port of `src/keyring/linux.ts`.

use super::{KeyringError, SERVICE};
use crate::proc::{self, ProcOutput, RunOptions};

/// Message shown when `secret-tool` is missing, mirroring upstream guidance.
fn spawn_error(detail: &str) -> KeyringError {
    KeyringError::new(format!(
        "Could not run secret-tool. Install libsecret \
         (e.g. apt install libsecret-tools, pacman -S libsecret).\n\
         Alternatively, set the LINEAR_API_KEY environment variable.\n  ({detail})"
    ))
}

fn code_of(output: &ProcOutput) -> String {
    output
        .code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".to_string())
}

fn secret_tool(args: &[&str], stdin: Option<&str>) -> Result<ProcOutput, KeyringError> {
    let options = RunOptions {
        stdin: stdin.map(|s| s.as_bytes().to_vec()),
        ..RunOptions::default()
    };
    proc::run("secret-tool", args, &options, proc::DEFAULT_TIMEOUT)
        .ok_or_else(|| spawn_error("program not found or timed out"))
}

/// `secret-tool` exits 2 after printing usage when given no arguments, so a
/// completed process is enough to prove the executable is on PATH.
pub fn is_available() -> bool {
    secret_tool(&[], None).is_ok()
}

pub fn get(account: &str) -> Result<Option<String>, KeyringError> {
    let result = secret_tool(&["lookup", "service", SERVICE, "account", account], None)?;
    if !result.success {
        // secret-tool returns exit 1 both when no items match and when the
        // lookup fails. Operational failures write to stderr; a miss does not.
        if result.code == Some(1) && result.stderr.is_empty() {
            return Ok(None);
        }
        return Err(KeyringError::new(format!(
            "secret-tool lookup failed (exit {}): {}",
            code_of(&result),
            result.stderr_string().trim()
        )));
    }
    // secret-tool writes the stored secret verbatim on a pipe, and an empty
    // stdout means the value itself is empty. Linear API keys are never empty,
    // so treat empty as not-found.
    let value = result.stdout_string();
    Ok(if value.is_empty() { None } else { Some(value) })
}

pub fn set(account: &str, password: &str) -> Result<(), KeyringError> {
    let label = format!("{SERVICE}: {account}");
    let result = secret_tool(
        &[
            "store", "--label", &label, "service", SERVICE, "account", account,
        ],
        Some(password),
    )?;
    if !result.success {
        return Err(KeyringError::new(format!(
            "secret-tool store failed (exit {}): {}",
            code_of(&result),
            result.stderr_string().trim()
        )));
    }
    Ok(())
}

pub fn delete(account: &str) -> Result<(), KeyringError> {
    let result = secret_tool(&["clear", "service", SERVICE, "account", account], None)?;
    if !result.success {
        return Err(KeyringError::new(format!(
            "secret-tool clear failed (exit {}): {}",
            code_of(&result),
            result.stderr_string().trim()
        )));
    }
    Ok(())
}
