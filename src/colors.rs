//! ANSI styling, mirroring `@std/fmt/colors` as used by the TypeScript CLI.
//!
//! Escape sequences are byte-for-byte identical to `@std/fmt/colors` so that
//! ported snapshots and user expectations carry over unchanged.
//!
//! Color is a process-global toggle, exactly like `setColorEnabled` upstream.
//! [`init`] turns it off when `NO_COLOR` is set or when the stream being
//! written is not a terminal, which is the behavior agents depend on: piped
//! output never contains escape codes.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

static COLOR_ENABLED: AtomicBool = AtomicBool::new(true);

/// Serializes tests that flip the process-global color toggle. `cargo test`
/// runs test functions in parallel threads, so without this the color tests in
/// this module and the width tests in `display.rs` race over `COLOR_ENABLED`.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Enable or disable styling globally.
pub fn set_color_enabled(enabled: bool) {
    COLOR_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether styling is currently enabled.
pub fn color_enabled() -> bool {
    COLOR_ENABLED.load(Ordering::Relaxed)
}

/// True when the `NO_COLOR` environment variable is set to anything at all,
/// matching `Deno.env.get("NO_COLOR") != null` in the TypeScript CLI.
pub fn no_color_env() -> bool {
    std::env::var_os("NO_COLOR").is_some()
}

/// Decide styling from both the environment and whether `stdout` is a TTY.
///
/// Call once at startup. `NO_COLOR` always wins; otherwise color is only
/// emitted on a terminal, so redirected output stays clean for scripts and
/// agents.
pub fn init() {
    set_color_enabled(!no_color_env() && std::io::stdout().is_terminal());
}

/// Refresh the toggle based on the stream an error report will use (stderr),
/// mirroring `setColorEnabled(Deno.stderr.isTerminal())` in `errors.ts`.
pub fn init_stderr() {
    set_color_enabled(!no_color_env() && std::io::stderr().is_terminal());
}

fn wrap(text: &str, open: &str, close: &str) -> String {
    if !color_enabled() {
        return text.to_string();
    }
    format!("{open}{text}{close}")
}

macro_rules! color_fn {
    ($name:ident, $open:literal, $close:literal, $doc:literal) => {
        #[doc = $doc]
        pub fn $name(text: &str) -> String {
            wrap(text, $open, $close)
        }
    };
}

color_fn!(red, "\x1b[31m", "\x1b[39m", "Red foreground.");
color_fn!(green, "\x1b[32m", "\x1b[39m", "Green foreground.");
color_fn!(yellow, "\x1b[33m", "\x1b[39m", "Yellow foreground.");
color_fn!(
    gray,
    "\x1b[90m",
    "\x1b[39m",
    "Bright black (gray) foreground."
);
color_fn!(bold, "\x1b[1m", "\x1b[22m", "Bold.");
color_fn!(underline, "\x1b[4m", "\x1b[24m", "Underline.");

// --- Composite styles from src/utils/styling.ts ---

/// `error(text)` upstream: red + bold.
// Test-only composite; the CLI uses `red`/`bold`/`warning` directly.
#[allow(dead_code)]
pub fn error(text: &str) -> String {
    red(&bold(text))
}

/// `warning(text)` upstream: yellow.
pub fn warning(text: &str) -> String {
    yellow(text)
}

/// `muted(text)` upstream: gray.
pub fn muted(text: &str) -> String {
    gray(text)
}

/// `header(text)` upstream: bold + underline.
pub fn header(text: &str) -> String {
    bold(&underline(text))
}

/// Style `text` with a `#rrggbb` (or bare `rrggbb`) truecolor foreground.
///
/// Mirrors `rgb24(text, parseInt(color.replace("#", ""), 16))`: an unparseable
/// colour degrades to plain text rather than emitting a broken escape.
pub fn color_hex(hex: &str, text: &str) -> String {
    if !color_enabled() {
        return text.to_string();
    }
    let Some((r, g, b)) = parse_hex(hex) else {
        return text.to_string();
    };
    format!("\x1b[38;2;{r};{g};{b}m{text}\x1b[39m")
}

fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    let digits = hex.strip_prefix('#').unwrap_or(hex);
    if digits.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&digits[0..2], 16).ok()?;
    let g = u8::from_str_radix(&digits[2..4], 16).ok()?;
    let b = u8::from_str_radix(&digits[4..6], 16).ok()?;
    Some((r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_with_std_codes() {
        let _guard = TEST_LOCK.lock().unwrap();
        set_color_enabled(true);
        assert_eq!(red("x"), "\x1b[31mx\x1b[39m");
        assert_eq!(bold("x"), "\x1b[1mx\x1b[22m");
        // Nested, as styling.ts composes them.
        assert_eq!(error("x"), "\x1b[31m\x1b[1mx\x1b[22m\x1b[39m");
        assert_eq!(header("x"), "\x1b[1m\x1b[4mx\x1b[24m\x1b[22m");
    }

    #[test]
    fn disabled_returns_plain_text() {
        let _guard = TEST_LOCK.lock().unwrap();
        set_color_enabled(false);
        assert_eq!(error("x"), "x");
        assert_eq!(muted("hello"), "hello");
        set_color_enabled(true);
    }
}
