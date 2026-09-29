//! Shared issue-table renderer — port of upstream's `formatIssueTable` (from
//! `issue-query.ts`), with the inline table `issue-mine.ts` builds folded in.
//!
//! Both `issue mine` and `issue query` render through [`render`]; they differ
//! only in the [`Options`] they pass. See `docs/issue-list-commands.md` for the
//! column-sizing rules this mirrors.

use std::io::IsTerminal;

use serde_json::Value;

use crate::colors;
use crate::display;
use crate::linear;

/// Layout knobs that differ between the two list commands.
pub struct Options {
    pub show_team_column: bool,
    pub show_assignee_column: bool,
    /// `0` for `mine`, `10` for `query`.
    pub min_title_width: usize,
    /// Columns subtracted before title sizing: `1` for `mine`, `0` for `query`.
    pub padding: usize,
}

/// Priority glyph column: always three display columns wide.
const PRIORITY_WIDTH: usize = 3;
const BLOCKED_WIDTH: usize = 1;
const ESTIMATE_WIDTH: usize = 1;
const ASSIGNEE_WIDTH: usize = 2;

/// Render `issues` as table lines, header first. The caller decides what to do
/// about an empty slice (both commands print `No issues found.`).
pub fn render(issues: &[Value], options: &Options) -> Vec<String> {
    if issues.is_empty() {
        return Vec::new();
    }

    let columns = terminal_columns();

    // --- measure columns ---------------------------------------------------
    let mut id_width = 2usize;
    let mut team_width = 0usize;
    let mut label_width = 6usize;
    let mut state_width = 5usize;
    let mut updated_width = 7usize;
    let mut max_title_width = 0usize;
    let mut max_label_join_width = 0usize;
    let mut cycle_width = 0usize;
    let mut show_cycle = false;

    for issue in issues {
        id_width = id_width.max(display::display_width(str_at(issue, "identifier")));
        if options.show_team_column {
            team_width = team_width.max(display::display_width(team_key(issue)));
        }
        max_title_width = max_title_width.max(display::display_width(str_at(issue, "title")));
        max_label_join_width = max_label_join_width.max(joined_label_width(issue));
        state_width = state_width.max(display::display_width(state_name(issue)));
        updated_width = updated_width.max(display::display_width(&time_ago_of(issue)));

        if issue_shows_cycle(issue) {
            show_cycle = true;
            cycle_width = cycle_width.max(display::display_width(&cycle_short_text(issue)));
        }
    }

    team_width = if options.show_team_column {
        team_width.max(4)
    } else {
        0
    };
    label_width = label_width.max(max_label_join_width).min(25);
    state_width = state_width.min(20);
    cycle_width = if show_cycle { cycle_width.max(3) } else { 0 };

    // --- title sizing ------------------------------------------------------
    let fixed_cells: &[usize] = &{
        let mut cells = vec![PRIORITY_WIDTH, id_width];
        if options.show_team_column {
            cells.push(team_width);
        }
        cells.push(label_width);
        cells.push(BLOCKED_WIDTH);
        cells.push(ESTIMATE_WIDTH);
        if show_cycle {
            cells.push(cycle_width);
        }
        if options.show_assignee_column {
            cells.push(ASSIGNEE_WIDTH);
        }
        cells.push(state_width);
        cells.push(updated_width);
        cells
    };
    let fixed = fixed_cells.iter().sum::<usize>() + fixed_cells.len() + 1;
    let title_width = options
        .min_title_width
        .max(max_title_width.min(columns.saturating_sub(options.padding).saturating_sub(fixed)));

    // --- header ------------------------------------------------------------
    let mut header: Vec<String> = vec![display::pad_display("◌", PRIORITY_WIDTH)];
    header.push(display::pad_display("ID", id_width));
    if options.show_team_column {
        header.push(display::pad_display("TEAM", team_width));
    }
    header.push(display::pad_display("TITLE", title_width));
    header.push(display::pad_display("LABELS", label_width));
    header.push(display::pad_display("B", BLOCKED_WIDTH));
    header.push(display::pad_display("E", ESTIMATE_WIDTH));
    if show_cycle {
        header.push(display::pad_display("CYC", cycle_width));
    }
    if options.show_assignee_column {
        header.push(display::pad_display("A", ASSIGNEE_WIDTH));
    }
    header.push(display::pad_display("STATE", state_width));
    header.push(display::pad_display("UPDATED", updated_width));

    let mut lines = vec![colors::header(&header.join(" "))];

    // --- rows --------------------------------------------------------------
    for issue in issues {
        let mut row: Vec<String> = Vec::new();

        let priority = issue.get("priority").and_then(Value::as_i64).unwrap_or(0);
        row.push(display::pad_display(
            &display::get_priority_display(priority),
            PRIORITY_WIDTH,
        ));
        row.push(display::pad_display(str_at(issue, "identifier"), id_width));
        if options.show_team_column {
            row.push(display::pad_display(team_key(issue), team_width));
        }
        row.push(display::pad_display(
            &display::truncate_text(str_at(issue, "title"), title_width),
            title_width,
        ));
        row.push(format_labels(issue, label_width));

        let blocked = if linear::is_issue_blocked(issue) {
            colors::warning("⊘")
        } else {
            " ".to_string()
        };
        row.push(display::pad_display(&blocked, BLOCKED_WIDTH));

        let estimate = issue
            .get("estimate")
            .and_then(Value::as_i64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        row.push(display::pad_display(&estimate, ESTIMATE_WIDTH));

        if show_cycle {
            let short = cycle_short(issue);
            let padding = cycle_width.saturating_sub(display::display_width(&short.text));
            row.push(format!("{}{}", display::color_cycle_short(&short), " ".repeat(padding)));
        }

        if options.show_assignee_column {
            row.push(display::pad_display(&assignee_initials(issue), ASSIGNEE_WIDTH));
        }

        let state_text = display::truncate_text(state_name(issue), state_width);
        let state_padding = state_width.saturating_sub(display::display_width(&state_text));
        row.push(format!(
            "{}{}",
            colors::color_hex(state_color(issue), &state_text),
            " ".repeat(state_padding)
        ));

        row.push(colors::muted(&display::pad_display(
            &time_ago_of(issue),
            updated_width,
        )));

        lines.push(row.join(" "));
    }

    lines
}

/// Terminal width: the real size on a TTY, else a stable 120-column default.
fn terminal_columns() -> usize {
    if std::io::stdout().is_terminal() {
        if let Some((width, _)) = terminal_size::terminal_size() {
            return width.0 as usize;
        }
    }
    120
}

fn str_at<'a>(issue: &'a Value, key: &str) -> &'a str {
    issue.get(key).and_then(Value::as_str).unwrap_or("")
}

fn team_key(issue: &Value) -> &str {
    issue.pointer("/team/key").and_then(Value::as_str).unwrap_or("")
}

fn state_name(issue: &Value) -> &str {
    issue.pointer("/state/name").and_then(Value::as_str).unwrap_or("")
}

fn state_color(issue: &Value) -> &str {
    issue.pointer("/state/color").and_then(Value::as_str).unwrap_or("")
}

/// `initials.slice(0, 2)`, or `-` when unassigned / lacking initials.
fn assignee_initials(issue: &Value) -> String {
    let initials = issue
        .pointer("/assignee/initials")
        .and_then(Value::as_str)
        .unwrap_or("");
    if initials.is_empty() {
        "-".to_string()
    } else {
        initials.chars().take(2).collect()
    }
}

fn time_ago_of(issue: &Value) -> String {
    issue
        .get("updatedAt")
        .and_then(Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|date| display::get_time_ago(date.with_timezone(&chrono::Utc)))
        .unwrap_or_default()
}

/// `issue.cycle != null || issue.team.cyclesEnabled`.
fn issue_shows_cycle(issue: &Value) -> bool {
    issue.get("cycle").is_some_and(|cycle| !cycle.is_null())
        || issue
            .pointer("/team/cyclesEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn cycle_short(issue: &Value) -> display::CycleShort {
    let info = issue
        .get("cycle")
        .filter(|cycle| !cycle.is_null())
        .map(|cycle| {
            let flag = |key: &str| cycle.get(key).and_then(Value::as_bool).unwrap_or(false);
            display::CycleDisplayInfo {
                number: cycle.get("number").and_then(Value::as_i64).unwrap_or(0),
                is_active: flag("isActive"),
                is_next: flag("isNext"),
                is_previous: flag("isPrevious"),
                is_past: flag("isPast"),
            }
        });
    let active = issue
        .pointer("/team/activeCycle/number")
        .and_then(Value::as_i64);
    display::format_cycle_short(info, active)
}

fn cycle_short_text(issue: &Value) -> String {
    cycle_short(issue).text
}

/// Labels from `issue.labels.nodes`, in server order, as `(name, color)`.
fn labels_of(issue: &Value) -> Vec<(&str, &str)> {
    issue
        .pointer("/labels/nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .map(|node| {
                    (
                        node.get("name").and_then(Value::as_str).unwrap_or(""),
                        node.get("color").and_then(Value::as_str).unwrap_or(""),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Display width of the labels joined with `", "` — the natural width used to
/// size the LABELS column.
fn joined_label_width(issue: &Value) -> usize {
    let names: Vec<&str> = labels_of(issue).into_iter().map(|(name, _)| name).collect();
    display::display_width(&names.join(", "))
}

/// `formatLabels`: comma-joined coloured labels, truncating the one that spills
/// past `width` and dropping the rest; padded to `width` visible columns.
fn format_labels(issue: &Value, width: usize) -> String {
    let labels = labels_of(issue);
    if labels.is_empty() {
        return " ".repeat(width);
    }

    let mut out = String::new();
    let mut current = 0usize;
    for (index, (name, color)) in labels.iter().enumerate() {
        let separator = if index > 0 { ", " } else { "" };
        let segment_width = display::display_width(&format!("{separator}{name}"));
        if current + segment_width > width {
            let remaining = width.saturating_sub(current);
            if remaining >= 4 {
                let truncated = display::truncate_text(name, remaining - separator.len());
                out.push_str(separator);
                out.push_str(&colors::color_hex(color, &truncated));
            }
            break;
        }
        out.push_str(separator);
        out.push_str(&colors::color_hex(color, name));
        current += segment_width;
    }
    display::pad_display(&out, width)
}
