//! Stdout writing helpers.
//!
//! Two jobs beyond `println!`:
//!
//! 1. **Write errors are not fatal.** A closed reader (`linear issue list |
//!    head`) is handled once, centrally, by `main`'s `restore_default_sigpipe`:
//!    restoring the default `SIGPIPE` disposition makes the kernel end the
//!    process the way it does for any other Unix tool, so no write site has to
//!    reason about a reader that stopped early. What is left for these helpers
//!    is that a failure while writing output must not abort a command that has
//!    already decided what to say.
//! 2. **One JSON convention.** Every `--json` path emits `JSON.stringify(v,
//!    null, 2)`-equivalent output with GraphQL field names preserved.

use serde_json::Value;
use std::io::Write;

/// Write a line to stdout, ignoring a closed pipe.
pub fn line(text: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{text}");
}

/// Write an empty line to stdout.
pub fn blank() {
    line("");
}

/// Write to stdout without a trailing newline.
pub fn raw(text: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = write!(lock, "{text}");
    let _ = lock.flush();
}

/// Pretty-print a JSON value, matching `JSON.stringify(value, null, 2)`.
pub fn to_pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Print a JSON value with two-space indentation.
///
/// Streamed to stdout rather than through [`to_pretty`] + [`line`]: a `--json`
/// payload can be megabytes (a listing, or the whole introspection document) and
/// materialising it as a `String` first doubles the peak for a copy nothing reads.
pub fn print_json(value: &Value) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if write_json(&mut lock, value).is_err() {
        return;
    }
    let _ = writeln!(lock);
}

/// Write a JSON value as a single line - one object per line, the NDJSON shape.
///
/// Compact and flushed on purpose. Compact, because a line is a record and pretty-printing
/// would spread one record over several lines, which is the format's promise broken. Flushed,
/// because stdout is block-buffered when it is a pipe: without this a stream arrives at exit,
/// which is a buffered list wearing a stream's name.
pub fn print_json_line(value: &Value) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if serde_json::to_writer(&mut lock, value).is_err() {
        return;
    }
    let _ = writeln!(lock);
    let _ = lock.flush();
}

/// Write a JSON value with two-space indentation to any writer.
pub fn write_json<W: Write>(writer: &mut W, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer_pretty(writer, value).map_err(std::io::Error::other)
}

/// Write a warning to stderr. Warnings never touch stdout so `--json` output
/// and shell-completion scripts stay clean.
pub fn warn(message: &str) {
    eprintln!("{}", crate::colors::yellow(&format!("Warning: {message}")));
}

/// Write a warning to stderr with a dimmed suggestion beneath it.
pub fn warn_with_suggestion(message: &str, suggestion: &str) {
    warn(message);
    eprintln!("{}", crate::colors::gray(&format!("  {suggestion}")));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pretty_matches_two_space_indent() {
        let value = json!({"a": {"b": [1, 2]}, "c": "d"});
        assert_eq!(
            to_pretty(&value),
            "{\n  \"a\": {\n    \"b\": [\n      1,\n      2\n    ]\n  },\n  \"c\": \"d\"\n}"
        );
    }

    #[test]
    fn preserves_key_order() {
        // `preserve_order` keeps GraphQL response field order, which agents and
        // diff-based tests rely on.
        let value: Value = serde_json::from_str(r#"{"z":1,"a":2}"#).unwrap();
        assert_eq!(to_pretty(&value), "{\n  \"z\": 1,\n  \"a\": 2\n}");
    }
}
