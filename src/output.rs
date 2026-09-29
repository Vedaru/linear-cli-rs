//! Stdout writing helpers.
//!
//! Two jobs beyond `println!`:
//!
//! 1. **Broken pipe tolerance.** `linear issue list | head` closes the pipe
//!    early. Rust's `println!` panics on `EPIPE`; these helpers treat a closed
//!    reader as a normal end of output, so a pipeline never sees a panic or a
//!    spurious non-zero exit from `linear` itself.
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
pub fn print_json(value: &Value) {
    line(&to_pretty(value));
}

/// Print raw JSON text already formatted upstream (used by `linear api`).
pub fn print_json_raw(raw_json: &str) {
    line(raw_json);
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
