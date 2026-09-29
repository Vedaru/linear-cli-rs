//! Interactive prompts, bounded so a headless run can never hang.
//!
//! Port of the `@cliffy/prompt` usage in the TypeScript CLI (`Secret`,
//! `Confirm`, `Select`), with one deliberate difference: every prompt first
//! checks that both `stdin` and `stderr` are terminals. A CLI that runs under
//! cron, CI, or an agent harness must never block forever waiting for input
//! that will not arrive — when prompting is impossible, callers get an
//! actionable error and are expected to offer a flag-based path instead.

use std::io::{BufRead, IsTerminal, Write};

use crate::errors::{CliError, Result};

/// Whether interactive prompting is possible: `stdin` and `stderr` are both
/// attached to a terminal.
pub fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Prompt for a secret without echoing it. Returns the trimmed input.
///
/// `hint` is shown next to the message, mirroring cliffy's `hint` option.
pub fn secret(message: &str, hint: &str) -> Result<String> {
    if !is_interactive() {
        return Err(CliError::cli(format!(
            "Cannot read {message} in a non-interactive environment"
        ))
        .suggestion("Provide the value with a flag or environment variable instead."));
    }

    eprint!("{message}");
    if !hint.is_empty() {
        eprint!(" ({hint})");
    }
    eprint!(": ");
    let _ = std::io::stderr().flush();

    let value = rpassword::read_password()
        .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
    // rpassword leaves the cursor on the input line; start a fresh one.
    eprintln!();
    Ok(value.trim().to_string())
}

/// Ask a yes/no question. `default` is used when the user submits an empty
/// line. Port of cliffy's `Confirm.prompt`.
pub fn confirm(message: &str, default: bool) -> Result<bool> {
    if !is_interactive() {
        return Err(CliError::cli(format!(
            "Cannot confirm \"{message}\" in a non-interactive environment"
        ))
        .suggestion("Re-run with the flag that skips this prompt."));
    }

    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    let stdin = std::io::stdin();
    loop {
        eprint!("{message} {suffix} ");
        let _ = std::io::stderr().flush();

        let mut line = String::new();
        let read = stdin
            .lock()
            .read_line(&mut line)
            .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
        if read == 0 {
            // EOF: fall back to the default rather than looping forever.
            return Ok(default);
        }
        match line.trim().to_lowercase().as_str() {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => eprintln!("Please answer \"y\" or \"n\"."),
        }
    }
}

/// Prompt the user to pick one of `labels`, returning its index. Port of
/// cliffy's `Select.prompt`.
pub fn select(message: &str, labels: &[String]) -> Result<usize> {
    if !is_interactive() {
        return Err(CliError::cli(format!(
            "Cannot select \"{message}\" in a non-interactive environment"
        ))
        .suggestion("Pass the value as an argument instead."));
    }

    eprintln!("{message}");
    for (index, label) in labels.iter().enumerate() {
        eprintln!("  {}. {label}", index + 1);
    }

    let stdin = std::io::stdin();
    loop {
        eprint!("Enter a number (1-{}): ", labels.len());
        let _ = std::io::stderr().flush();

        let mut line = String::new();
        let read = stdin
            .lock()
            .read_line(&mut line)
            .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
        if read == 0 {
            return Err(CliError::cli("No selection made"));
        }
        if let Ok(choice) = line.trim().parse::<usize>() {
            if choice >= 1 && choice <= labels.len() {
                return Ok(choice - 1);
            }
        }
        eprintln!("Please enter a number between 1 and {}.", labels.len());
    }
}
