//! Pager integration. Port of `src/utils/pager.ts`.
//!
//! Paging only happens on an interactive terminal and only when the content is
//! taller than the window; otherwise output is printed straight through. The
//! pager child inherits stdout/stderr and receives the document on its stdin
//! via [`proc::run_inherit`], so `less`/`more` behave normally while remaining
//! bounded by [`proc::DEFAULT_TIMEOUT`].
//!
//! Upstream's fallback chain (`less` → `more` → `cat`) is preserved: if the
//! configured pager cannot be spawned or exits non-zero, each fallback is tried
//! in turn, and the content is printed directly when all of them fail.

use std::io::IsTerminal;

use crate::output;
use crate::proc;

/// A pager invocation: the program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagerCommand {
    pub command: String,
    pub args: Vec<String>,
}

/// The pager to use: `$PAGER` split on whitespace when set, else a
/// platform-specific default (`more` on Windows, `less -R -X` elsewhere).
pub fn get_pager_command() -> Option<PagerCommand> {
    if let Ok(user_pager) = std::env::var("PAGER") {
        if !user_pager.is_empty() {
            let mut parts = user_pager.split_whitespace();
            if let Some(command) = parts.next() {
                return Some(PagerCommand {
                    command: command.to_string(),
                    args: parts.map(|part| part.to_string()).collect(),
                });
            }
        }
    }

    if cfg!(windows) {
        Some(PagerCommand {
            command: "more".to_string(),
            args: Vec::new(),
        })
    } else {
        Some(PagerCommand {
            command: "less".to_string(),
            args: vec!["-R".to_string(), "-X".to_string()],
        })
    }
}

fn run_pager(pager: &PagerCommand, content: &str) -> bool {
    let args: Vec<&str> = pager.args.iter().map(String::as_str).collect();
    matches!(
        proc::run_inherit(
            &pager.command,
            &args,
            Some(content.as_bytes()),
            proc::DEFAULT_TIMEOUT
        ),
        Some(true)
    )
}

/// Try the remaining pagers after `failed_pager` did not work, printing the
/// content directly if none succeed.
fn try_fallback_pagers(content: &str, failed_pager: &str) {
    let fallbacks: Vec<PagerCommand> = if cfg!(windows) {
        let mut list = Vec::new();
        if failed_pager != "more" {
            list.push(PagerCommand {
                command: "more".to_string(),
                args: Vec::new(),
            });
        }
        if failed_pager != "less" {
            list.push(PagerCommand {
                command: "less".to_string(),
                args: vec!["-R".to_string(), "-X".to_string()],
            });
        }
        list
    } else {
        let mut list = Vec::new();
        if failed_pager != "less" {
            list.push(PagerCommand {
                command: "less".to_string(),
                args: vec!["-R".to_string(), "-X".to_string()],
            });
        }
        if failed_pager != "more" {
            list.push(PagerCommand {
                command: "more".to_string(),
                args: Vec::new(),
            });
        }
        if failed_pager != "cat" {
            list.push(PagerCommand {
                command: "cat".to_string(),
                args: Vec::new(),
            });
        }
        list
    };

    for fallback in fallbacks {
        if run_pager(&fallback, content) {
            return;
        }
    }

    output::line(content);
}

/// Pipe `content` to the user's pager, falling back on failure.
pub fn pipe_to_user_pager(content: &str) {
    let Some(pager) = get_pager_command() else {
        output::line(content);
        return;
    };

    if run_pager(&pager, content) {
        return;
    }
    try_fallback_pagers(content, &pager.command);
}

/// Whether output tall enough to warrant a pager should actually be paged.
///
/// Requires `use_pager` and a terminal on stdout. Otherwise the content only
/// gets paged when it exceeds the terminal height minus two rows (room for the
/// shell prompt); when the height is unavailable, a fixed threshold of 50 lines
/// is used.
pub fn should_use_pager(output_lines: usize, use_pager: bool) -> bool {
    if !use_pager || !std::io::stdout().is_terminal() {
        return false;
    }

    match terminal_size::terminal_size() {
        Some((_, terminal_size::Height(height))) => output_lines as i64 > height as i64 - 2,
        None => output_lines > 50,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_pager_is_never_used() {
        assert!(!should_use_pager(10_000, false));
    }

    #[test]
    fn default_pager_command_is_set() {
        // With no PAGER in the environment (the test harness does not set one),
        // the default is chosen for the host platform.
        if std::env::var("PAGER").is_err() {
            let pager = get_pager_command().expect("a default pager");
            if cfg!(windows) {
                assert_eq!(pager.command, "more");
            } else {
                assert_eq!(pager.command, "less");
                assert_eq!(pager.args, vec!["-R", "-X"]);
            }
        }
    }
}
