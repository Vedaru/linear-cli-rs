//! OSC-8 hyperlinks. Port of `src/utils/hyperlink.ts`.
//!
//! Format: `\x1b]8;;URL\x1b\\TEXT\x1b]8;;\x1b\\`
//! See <https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda>.

use std::io::IsTerminal;

/// Wrap text in an OSC-8 hyperlink escape sequence.
pub fn hyperlink(text: &str, url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// Spinners are disabled for the same reasons as hyperlinks. `linear` has no
/// spinner in the Rust port (agents need machine-readable, line-oriented
/// output), but the predicate is kept because commands and tests ask for it.
pub fn should_show_spinner() -> bool {
    if crate::colors::no_color_env() {
        false
    } else {
        std::io::stdout().is_terminal()
    }
}

/// Resolve the `"default"` hyperlink format to its actual template.
pub fn resolve_hyperlink_format(format: &str) -> String {
    if format == "default" {
        "file://{host}{path}".to_string()
    } else {
        format.to_string()
    }
}

/// Best-effort hostname, replacing `Deno.hostname()`.
///
/// Read from `HOSTNAME` first (set by most shells and container runtimes), then
/// `/etc/hostname`, then the `hostname` command. Falls back to `localhost` so a
/// hyperlink is always well-formed.
pub fn hostname() -> String {
    if let Ok(name) = std::env::var("HOSTNAME") {
        if !name.trim().is_empty() {
            return name.trim().to_string();
        }
    }
    if let Ok(name) = std::fs::read_to_string("/etc/hostname") {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(output) = std::process::Command::new("hostname").output() {
        if output.status.success() {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    "localhost".to_string()
}

/// Percent-encode a path for a `file://` URL the way `encodeURI` does, then
/// escape `#` as well (as the TypeScript version does explicitly) so a path
/// containing a fragment separator stays a single URL.
fn encode_uri(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        let ch = byte as char;
        let unreserved = ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '-' | '_'
                    | '.'
                    | '!'
                    | '~'
                    | '*'
                    | '\''
                    | '('
                    | ')'
                    | ';'
                    | ','
                    | '/'
                    | '?'
                    | ':'
                    | '@'
                    | '&'
                    | '='
                    | '+'
                    | '$'
            );
        if unreserved {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out.replace('#', "%23")
}

/// Render a clickable string for a path or URL.
///
/// Remote URLs link directly; local paths are run through the format template,
/// which defaults to `file://{host}{path}`.
pub fn format_path_hyperlink(display_text: &str, path_or_url: &str, format: &str) -> String {
    let resolved = resolve_hyperlink_format(format);
    let url = if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
        path_or_url.to_string()
    } else {
        resolved
            .replace("{host}", &hostname())
            .replace("{path}", &encode_uri(path_or_url))
    };
    hyperlink(display_text, &url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_uri_like_encodeuri() {
        assert_eq!(encode_uri("/a/b c"), "/a/b%20c");
        assert_eq!(encode_uri("/a#b"), "/a%23b");
        assert_eq!(encode_uri("/a?b=c&d"), "/a?b=c&d");
    }

    #[test]
    fn default_format_resolves() {
        assert_eq!(resolve_hyperlink_format("default"), "file://{host}{path}");
        assert_eq!(
            resolve_hyperlink_format("vscode://file{path}"),
            "vscode://file{path}"
        );
    }

    #[test]
    fn remote_urls_link_directly() {
        let out = format_path_hyperlink("docs", "https://linear.app/x", "default");
        assert!(out.contains("\x1b]8;;https://linear.app/x\x1b\\"));
    }
}
