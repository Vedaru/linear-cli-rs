//! Text layout helpers shared by every command. Port of `src/utils/display.ts`.

use chrono::{DateTime, Datelike, Local, Utc};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::colors;

/// Remove ANSI escape sequences (CSI, OSC-8 hyperlinks and other string
/// terminators) so width math is based on what a terminal actually shows.
///
/// `@std/cli`'s `unicodeWidth` strips escapes the same way; this keeps table
/// alignment correct for styled cells such as OSC-8 links and colored tokens.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // CSI: ESC [ ... final-byte in @-~
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: ESC ] ... terminated by BEL or ESC \
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // Two-character escape.
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// `stripConsoleFormat()` upstream: Linear's markdown renders inline color
/// switches as `%c` placeholders, which occupy no display width.
pub fn strip_console_format(s: &str) -> String {
    s.replace("%c", "")
}

/// Display width of a string, ignoring ANSI escapes and `%c` markers.
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(strip_ansi(&strip_console_format(s)).as_str())
}

/// Pad a plain string to `width` display columns.
pub fn pad_display(s: &str, width: usize) -> String {
    let w = display_width(s);
    format!("{s}{}", " ".repeat(width.saturating_sub(w)))
}

/// Pad a string that may contain styling and `%c` markers.
pub fn pad_display_formatted(s: &str, width: usize) -> String {
    let w = display_width(s);
    format!("{s}{}", " ".repeat(width.saturating_sub(w)))
}

/// Truncate to `max_width` display columns, appending `...` when it does not
/// fit. Unicode-aware: never splits a character.
pub fn truncate_text(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width < 3 {
        return text.chars().take(max_width).collect();
    }
    let max_content = max_width - 3;
    let mut truncated = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + char_width > max_content {
            break;
        }
        truncated.push(ch);
        width += char_width;
    }
    truncated.push_str("...");
    truncated
}

/// Coarse "time since" for lists. Mirrors `getTimeAgo()`.
pub fn get_time_ago(date: DateTime<Utc>) -> String {
    let diff = Utc::now().signed_duration_since(date);
    let minutes = diff.num_minutes();
    let hours = diff.num_hours();
    let days = diff.num_days();

    if minutes < 1 {
        "just now".to_string()
    } else if minutes < 60 {
        format!("{minutes} minutes ago")
    } else if hours < 24 {
        format!("{hours} hour{} ago", if hours == 1 { "" } else { "s" })
    } else {
        format!("{days} day{} ago", if days == 1 { "" } else { "s" })
    }
}

/// Finer-grained "time since" for comments, with an absolute date fallback.
///
/// The fallback formats as `M/D/YYYY` (no zero padding), matching the `en-US`
/// short date that `toLocaleDateString()` produces upstream. Formatting it
/// explicitly keeps output stable regardless of the server's locale.
pub fn format_relative_time(date_string: &str) -> String {
    let Ok(parsed) = DateTime::parse_from_rfc3339(date_string) else {
        // Also accept the trailing-Z form and naive timestamps.
        match date_string.parse::<DateTime<Utc>>() {
            Ok(parsed) => return format_relative_time_from(parsed.with_timezone(&Utc)),
            Err(_) => return date_string.to_string(),
        }
    };
    format_relative_time_from(parsed.with_timezone(&Utc))
}

fn format_relative_time_from(date: DateTime<Utc>) -> String {
    let diff = Utc::now().signed_duration_since(date);
    let minutes = diff.num_minutes();
    let hours = diff.num_hours();
    let days = diff.num_days();

    if minutes < 60 {
        if minutes <= 1 {
            "1 minute ago".to_string()
        } else {
            format!("{minutes} minutes ago")
        }
    } else if hours < 24 {
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{hours} hours ago")
        }
    } else if days < 7 {
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{days} days ago")
        }
    } else {
        let local: DateTime<Local> = date.with_timezone(&Local);
        format!("{}/{}/{}", local.month(), local.day(), local.year())
    }
}

/// Bar-glyph rendering of Linear's 0-4 priority scale. Mirrors
/// `getPriorityDisplay()`.
pub fn get_priority_display(priority: i64) -> String {
    match priority {
        0 => "---".to_string(),
        1 => "⚠⚠⚠".to_string(),
        2 => "▄▆█".to_string(),
        3 => "▄▆ ".to_string(),
        4 => "▄  ".to_string(),
        other => other.to_string(),
    }
}

const PROJECT_PRIORITY_LABELS: [&str; 5] = ["None", "Urgent", "High", "Medium", "Low"];

/// Project surfaces spell priority out; unknown values fall through to the
/// number so a new priority level is visible rather than mislabeled "None".
pub fn get_project_priority_label(priority: i64) -> String {
    match usize::try_from(priority)
        .ok()
        .and_then(|i| PROJECT_PRIORITY_LABELS.get(i))
    {
        Some(label) => (*label).to_string(),
        None => priority.to_string(),
    }
}

/// The subset of cycle fields the compact table token needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleDisplayInfo {
    pub number: i64,
    pub is_active: bool,
    pub is_next: bool,
    pub is_previous: bool,
    pub is_past: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleShortKind {
    Active,
    Future,
    Past,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleShort {
    pub text: String,
    pub kind: CycleShortKind,
}

/// Compact cycle token for table columns: `now` for the active cycle, signed
/// offsets (`+1`, `-2`) relative to the team's active cycle, or an absolute
/// `#N` when no anchor exists.
///
/// The API's `isNext`/`isPrevious` flags take precedence over arithmetic so
/// display always agrees with what `--cycle next`/`--cycle previous` selects.
pub fn format_cycle_short(
    cycle: Option<CycleDisplayInfo>,
    active_cycle_number: Option<i64>,
) -> CycleShort {
    let Some(cycle) = cycle else {
        return CycleShort {
            text: "-".to_string(),
            kind: CycleShortKind::None,
        };
    };
    if cycle.is_active {
        return CycleShort {
            text: "now".to_string(),
            kind: CycleShortKind::Active,
        };
    }
    if cycle.is_next {
        return CycleShort {
            text: "+1".to_string(),
            kind: CycleShortKind::Future,
        };
    }
    if cycle.is_previous {
        return CycleShort {
            text: "-1".to_string(),
            kind: CycleShortKind::Past,
        };
    }
    if let Some(active) = active_cycle_number {
        let offset = cycle.number - active;
        if offset == 0 {
            return CycleShort {
                text: "now".to_string(),
                kind: CycleShortKind::Active,
            };
        }
        if offset > 0 {
            return CycleShort {
                text: format!("+{offset}"),
                kind: CycleShortKind::Future,
            };
        }
        return CycleShort {
            text: offset.to_string(),
            kind: CycleShortKind::Past,
        };
    }
    CycleShort {
        text: format!("#{}", cycle.number),
        kind: if cycle.is_past {
            CycleShortKind::Past
        } else {
            CycleShortKind::Future
        },
    }
}

/// Style a [`CycleShort`]. Active cycles are green; past and absent muted.
pub fn color_cycle_short(short: &CycleShort) -> String {
    match short.kind {
        CycleShortKind::Active => colors::green(&short.text),
        CycleShortKind::Future => short.text.clone(),
        CycleShortKind::Past | CycleShortKind::None => colors::muted(&short.text),
    }
}

// ---------------------------------------------------------------------------
// Member display (port of src/utils/member-display.ts)
// ---------------------------------------------------------------------------

/// Read a string field, treating a missing or null value as empty — matching
/// the way the renderer tests `if (member.email)` upstream.
fn member_str<'a>(member: &'a serde_json::Value, key: &str) -> &'a str {
    member
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

fn member_bool(member: &serde_json::Value, key: &str) -> bool {
    member
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Suffixes such as ` (admin) (you)`. `admin` and `owner` are independent in
/// Linear's schema, so an owner who is also an admin shows both.
fn member_markers(member: &serde_json::Value) -> String {
    let mut markers = String::new();
    let mut push = |marker: &str| {
        markers.push_str(&format!(" ({marker})"));
    };
    if !member_bool(member, "active") {
        push("inactive");
    }
    if member_bool(member, "guest") {
        push("guest");
    }
    if !member_bool(member, "isAssignable") {
        push("not assignable");
    }
    if member_bool(member, "admin") {
        push("admin");
    }
    if member_bool(member, "owner") {
        push("owner");
    }
    if member_bool(member, "isMe") {
        push("you");
    }
    markers
}

/// Format a `lastSeen` timestamp the way `Date.toLocaleString()` renders it in
/// the `en-US` locale: `9/29/2026, 1:23:45 PM`.
fn format_last_seen(raw: &str) -> String {
    let parsed = DateTime::parse_from_rfc3339(raw)
        .map(|date| date.with_timezone(&Local))
        .or_else(|_| {
            raw.parse::<DateTime<Utc>>()
                .map(|date| date.with_timezone(&Local))
        });
    match parsed {
        Ok(local) => local.format("%-m/%-d/%Y, %-I:%M:%S %p").to_string(),
        Err(_) => raw.to_string(),
    }
}

/// Print a member list under a heading. `members` are GraphQL member nodes;
/// fields absent from a node render as empty, exactly as upstream's optional
/// fields do.
pub fn print_members(members: &[serde_json::Value], heading: &str) {
    crate::output::line(&format!("{heading} ({}):", members.len()));
    crate::output::blank();

    for member in members {
        let name = member_str(member, "name");
        let display_name = member_str(member, "displayName");
        let primary = if display_name.is_empty() {
            name
        } else {
            display_name
        };
        let full_name = if name != display_name {
            format!(" ({name})")
        } else {
            String::new()
        };

        crate::output::line(&format!(
            "{primary}{full_name} [{}]{}",
            member_str(member, "initials"),
            member_markers(member)
        ));
        let email = member_str(member, "email");
        if !email.is_empty() {
            crate::output::line(&format!("  Email: {email}"));
        }
        let description = member_str(member, "description");
        if !description.is_empty() {
            crate::output::line(&format!("  Role: {description}"));
        }
        let timezone = member_str(member, "timezone");
        if !timezone.is_empty() {
            crate::output::line(&format!("  Timezone: {timezone}"));
        }
        let status_emoji = member_str(member, "statusEmoji");
        let status_label = member_str(member, "statusLabel");
        if !status_emoji.is_empty() && !status_label.is_empty() {
            crate::output::line(&format!("  Status: {status_emoji} {status_label}"));
        }
        let last_seen = member_str(member, "lastSeen");
        if !last_seen.is_empty() {
            crate::output::line(&format!("  Last seen: {}", format_last_seen(last_seen)));
        }
        crate::output::blank();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colors;

    #[test]
    fn width_ignores_ansi_and_markers() {
        let _guard = colors::TEST_LOCK.lock().unwrap();
        colors::set_color_enabled(true);
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width(&colors::red("abc")), 3);
        assert_eq!(display_width("a%cb"), 2);
        colors::set_color_enabled(true);
    }

    #[test]
    fn pad_uses_display_width() {
        let _guard = colors::TEST_LOCK.lock().unwrap();
        colors::set_color_enabled(false);
        assert_eq!(pad_display("ab", 4), "ab  ");
        assert_eq!(pad_display("abcd", 2), "abcd");
        colors::set_color_enabled(true);
    }

    #[test]
    fn truncate_reserves_ellipsis() {
        assert_eq!(truncate_text("hello world", 8), "hello...");
        assert_eq!(truncate_text("hi", 8), "hi");
        assert_eq!(truncate_text("hello", 2), "he");
    }

    #[test]
    fn priority_glyphs() {
        assert_eq!(get_priority_display(0), "---");
        assert_eq!(get_priority_display(1), "⚠⚠⚠");
        assert_eq!(get_priority_display(4), "▄  ");
        assert_eq!(get_priority_display(9), "9");
    }

    #[test]
    fn project_priority_labels() {
        assert_eq!(get_project_priority_label(0), "None");
        assert_eq!(get_project_priority_label(1), "Urgent");
        assert_eq!(get_project_priority_label(7), "7");
    }

    #[test]
    fn cycle_short_relative_and_absolute() {
        let cycle = |number, is_active, is_next, is_previous, is_past| CycleDisplayInfo {
            number,
            is_active,
            is_next,
            is_previous,
            is_past,
        };

        assert_eq!(format_cycle_short(None, Some(5)).text, "-");
        assert_eq!(
            format_cycle_short(Some(cycle(5, true, false, false, false)), Some(5)).text,
            "now"
        );
        assert_eq!(
            format_cycle_short(Some(cycle(6, false, true, false, false)), Some(5)).text,
            "+1"
        );
        assert_eq!(
            format_cycle_short(Some(cycle(4, false, false, true, true)), Some(5)).text,
            "-1"
        );
        // No anchor: absolute number, colored by past/future.
        let short = format_cycle_short(Some(cycle(3, false, false, false, true)), None);
        assert_eq!(short.text, "#3");
        assert_eq!(short.kind, CycleShortKind::Past);
    }

    #[test]
    fn member_markers_follow_upstream_order() {
        let member = serde_json::json!({
            "active": false, "guest": true, "isAssignable": false,
            "admin": true, "owner": true, "isMe": true
        });
        assert_eq!(
            member_markers(&member),
            " (inactive) (guest) (not assignable) (admin) (owner) (you)"
        );
        // A fully active, non-guest, assignable member has no markers.
        let member = serde_json::json!({ "active": true, "isAssignable": true });
        assert_eq!(member_markers(&member), "");
    }

    #[test]
    fn last_seen_renders_in_en_us_style() {
        // Rendered in the machine's local zone; only the shape is asserted.
        let formatted = format_last_seen("2026-09-29T13:23:45Z");
        assert!(formatted.contains("/2026,"), "unexpected: {formatted}");
        assert!(formatted.ends_with("M"), "unexpected: {formatted}");
        assert_eq!(format_last_seen("not a date"), "not a date");
    }
}
