//! Jujutsu (jj) helpers. Port of `src/utils/jj.ts`.
//!
//! All `jj` invocations go through [`proc::run`] (bounded, no terminal
//! inheritance). Where upstream prints the child's stderr with
//! `console.error` before throwing, this prints it to stderr too, then returns
//! the same clean error message.

use crate::errors::{CliError, Result};
use crate::issue_identifier::find_issue_identifier_in_text;
use crate::proc::{self, RunOptions, DEFAULT_TIMEOUT};

fn jj(args: &[&str]) -> Option<proc::ProcOutput> {
    proc::run("jj", args, &RunOptions::default(), DEFAULT_TIMEOUT)
}

/// Build the description written by `jj describe`:
/// `"{issueId} {title}"`, a blank line, then the `Linear-issue` and
/// `Linear-issue-url` trailers.
pub fn format_issue_description(issue_id: &str, title: &str, url: &str, magic_word: &str) -> String {
    format!(
        "{issue_id} {title}\n\nLinear-issue: {magic_word} {issue_id}\nLinear-issue-url: {url}"
    )
}

/// Whether the current change is empty — no description and no file changes.
pub fn is_jj_change_empty() -> Result<bool> {
    let desc = jj(&["log", "-r", "@", "-T", "description", "--no-graph"])
        .ok_or_else(|| CliError::cli("Failed to inspect jj change: jj is not available"))?;
    let description = desc.stdout_trimmed();
    if !description.is_empty() {
        return Ok(false);
    }

    let diff = jj(&["log", "-p", "-r", "@", "--git", "--no-graph"])
        .ok_or_else(|| CliError::cli("Failed to inspect jj change: jj is not available"))?;
    let diff_output = diff.stdout_string();
    // A file-level change makes `jj log -p` emit a "diff --git" hunk header.
    Ok(!diff_output.contains("diff --git"))
}

/// Prepare a fresh working state: keep the current change when it is empty,
/// otherwise start a new one on top.
pub fn prepare_jj_working_state() -> Result<()> {
    if !is_jj_change_empty()? {
        let output = jj(&["new"])
            .ok_or_else(|| CliError::cli("Failed to create new jj change: jj is not available"))?;
        if !output.success {
            eprint!("{}", output.stderr_string());
            return Err(CliError::cli("Failed to create new jj change"));
        }
    }
    Ok(())
}

/// Set the current change's description.
pub fn set_jj_description(description: &str) -> Result<()> {
    let output = jj(&["describe", "-m", description])
        .ok_or_else(|| CliError::cli("Failed to set jj description: jj is not available"))?;
    if !output.success {
        eprint!("{}", output.stderr_string());
        return Err(CliError::cli("Failed to set jj description"));
    }
    Ok(())
}

/// Create a new empty change (leaving the current one behind).
pub fn create_jj_new_change() -> Result<()> {
    let output = jj(&["new"])
        .ok_or_else(|| CliError::cli("Failed to create new jj change: jj is not available"))?;
    if !output.success {
        eprint!("{}", output.stderr_string());
        return Err(CliError::cli("Failed to create new jj change"));
    }
    Ok(())
}

/// Pull a Linear issue identifier out of a `Linear-issue` trailer value.
///
/// Handles both the new `"Fixes ABC-123"` form and the old markdown-link form
/// `"[ABC-123](https://linear.app/...)"` — both contain a `TEAMKEY-NUMBER`
/// token, which [`find_issue_identifier_in_text`] extracts.
pub fn parse_linear_issue_from_trailer(trailer_value: &str) -> Option<String> {
    find_issue_identifier_in_text(trailer_value).map(|parsed| parsed.identifier)
}

/// Parse the output of `jj log -T 'trailers.map(...)'`.
///
/// Scans only the first commit that carries a `Linear-issue` trailer: a blank
/// line ends that commit's trailer block. When several trailers appear in the
/// same commit, the last one wins. A trailing block with no blank line is
/// still returned.
pub fn parse_jj_trailers_output(output: &str) -> Option<String> {
    let mut last_valid: Option<String> = None;
    for line in output.split('\n') {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            if let Some(issue_id) = parse_linear_issue_from_trailer(trimmed) {
                last_valid = Some(issue_id);
            }
        } else if last_valid.is_some() {
            // A blank line closes the current commit's trailer block.
            return last_valid;
        }
    }
    last_valid
}

/// The most recent Linear issue identifier in the current change or its
/// ancestors, read from `Linear-issue` trailers. `None` when `jj` fails or no
/// trailer is present.
pub fn get_jj_linear_issue() -> Option<String> {
    let output = jj(&[
        "log",
        "-r",
        "::@",
        "-T",
        "trailers.map(|t| if(t.key() == \"Linear-issue\", t.value(), \"\"))",
        "--no-graph",
    ])?;
    if !output.success {
        return None;
    }
    parse_jj_trailers_output(&output.stdout_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_has_title_and_both_trailers() {
        let description =
            format_issue_description("ENG-123", "Fix the thing", "https://linear.app/x/ENG-123", "Fixes");
        assert_eq!(
            description,
            "ENG-123 Fix the thing\n\nLinear-issue: Fixes ENG-123\nLinear-issue-url: https://linear.app/x/ENG-123"
        );
    }

    #[test]
    fn trailer_parser_accepts_both_formats() {
        assert_eq!(
            parse_linear_issue_from_trailer("Fixes ABC-123").as_deref(),
            Some("ABC-123")
        );
        assert_eq!(
            parse_linear_issue_from_trailer("[ABC-123](https://linear.app/x/ABC-123)").as_deref(),
            Some("ABC-123")
        );
        assert!(parse_linear_issue_from_trailer("no identifier").is_none());
    }

    #[test]
    fn trailers_output_stops_at_first_commit_and_takes_last_trailer() {
        // Two trailers in the first commit: the last wins, and the blank line
        // ends the search — the later commit's ABC-9 is ignored.
        let output = "Fixes ABC-1\nFixes ABC-2\n\nFixes ABC-9\n";
        assert_eq!(parse_jj_trailers_output(output).as_deref(), Some("ABC-2"));
    }

    #[test]
    fn trailers_output_handles_missing_trailing_blank_line() {
        assert_eq!(
            parse_jj_trailers_output("Fixes ABC-7").as_deref(),
            Some("ABC-7")
        );
        assert!(parse_jj_trailers_output("").is_none());
        assert!(parse_jj_trailers_output("\n\n").is_none());
    }
}
