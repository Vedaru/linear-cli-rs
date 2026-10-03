//! `linear notification list` — what Linear told this user, newest first.
//!
//! The shape follows `view list` - `{nodes, pageInfo}` under `--json` - with two additions that are
//! this command's whole point: `unreadCount`, which is the API's own number rather than a count of
//! the rows shown, and `viewer`, because these are one user's notifications and the listing says
//! whose.
//!
//! `--unread` filters the page here, not on the server: `NotificationFilter` has no read state (see
//! `src/linear/notifications.rs`). So `--unread` with a `--limit` can show fewer rows than the
//! limit, and `unreadCount` is the honest total - the two are both printed for exactly that reason.

use std::io::IsTerminal;

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{colors, display, linear, output};

use super::{is_unread, subject_of, whose};

#[derive(Args, Debug)]
pub struct NotificationListArgs {
    /// Only notifications that are still unread
    #[arg(long)]
    pub unread: bool,
    /// Only notifications created after this age or date (7d, 24h, 2024-01-15)
    #[arg(long = "since", value_name = "AGE|DATE")]
    pub since: Option<String>,
    /// Include archived notifications
    #[arg(long = "include-archived")]
    pub include_archived: bool,
    /// Maximum number of notifications to show (0 for no limit)
    #[arg(long = "limit", default_value_t = 50)]
    pub limit: i64,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: NotificationListArgs) -> Result<()> {
    if args.limit < 0 {
        return Err(CliError::validation("--limit must be 0 or greater"));
    }
    // `None` is "everything", which is what `0` means at the flag.
    let limit = if args.limit == 0 {
        None
    } else {
        Some(args.limit as u32)
    };

    // The same relative-date parser the rest of the CLI uses, so `--since 7d` means here what it
    // means on `issue query`.
    let since = match args.since.as_deref() {
        Some(value) => Some(linear::parse_date_filter_or_age(value, "--since")?),
        None => None,
    };

    let (mut nodes, page_info, viewer) =
        linear::list_notifications(limit, since, args.include_archived)?;
    let unread_count = linear::unread_count()?;

    if args.unread {
        nodes.retain(is_unread);
    }

    if args.json {
        output::print_json(&json!({
            "nodes": nodes,
            "pageInfo": page_info,
            "unreadCount": unread_count,
            "viewer": viewer,
        }));
        return Ok(());
    }

    if nodes.is_empty() {
        output::line(&format!(
            "No{} notifications for {}.",
            if args.unread { " unread" } else { "" },
            whose(&viewer)
        ));
        return Ok(());
    }

    const READ_WIDTH: usize = 6;
    let columns = terminal_columns();
    let what_width = nodes
        .iter()
        .map(|node| display::display_width(&subject_of(node)))
        .max()
        .unwrap_or(20)
        .clamp(20, columns.saturating_sub(30).max(20));

    let header_cells = [
        display::pad_display("ID", 36),
        display::pad_display("READ", READ_WIDTH),
        display::pad_display("CREATED", 20),
        display::pad_display("WHAT", what_width),
    ];
    let header = header_cells
        .iter()
        .map(|cell| colors::underline(cell))
        .collect::<Vec<_>>()
        .join(" ");
    output::line(&header);

    for node in &nodes {
        let id = display::pad_display(node.get("id").and_then(Value::as_str).unwrap_or(""), 36);
        let read = display::pad_display(if is_unread(node) { "no" } else { "yes" }, READ_WIDTH);
        let created = display::pad_display(&created_at(node), 20);
        let what = display::pad_display(&truncate(&subject_of(node), what_width), what_width);
        output::line(&format!("{id} {read} {created} {what}"));
    }

    output::blank();
    output::line(&format!(
        "{} notifications shown, {} unread for {}.",
        nodes.len(),
        unread_count,
        whose(&viewer)
    ));
    Ok(())
}

/// `createdAt` in the coordinate space every other listing prints dates in.
fn created_at(notification: &Value) -> String {
    let raw = notification
        .get("createdAt")
        .and_then(Value::as_str)
        .unwrap_or("");
    if raw.len() >= 16 {
        // 2026-10-02T01:22:42.936Z -> 2026-10-02 01:22, which is what fits a column.
        format!("{} {}", &raw[0..10], &raw[11..16])
    } else {
        raw.to_string()
    }
}

fn truncate(text: &str, width: usize) -> String {
    if display::display_width(text) > width {
        let take = width.saturating_sub(3);
        let mut truncated: String = text.chars().take(take).collect();
        truncated.push_str("...");
        truncated
    } else {
        text.to_string()
    }
}

fn terminal_columns() -> usize {
    if std::io::stdout().is_terminal() {
        if let Some((width, _)) = terminal_size::terminal_size() {
            return width.0 as usize;
        }
    }
    120
}
