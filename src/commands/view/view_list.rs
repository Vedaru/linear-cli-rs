//! `linear view list` — every custom view this token can see.
//!
//! Not a port: upstream has no `view` command. The shape follows `label list` - `{nodes,
//! pageInfo}` under `--json`, a table and a count on the human path - so the two list the same
//! way and an agent can read either with the same code.
//!
//! The `FILTER` column is the view's whole point, so it is printed even though it is the widest
//! thing on the line: a view whose filter is not visible is a name, and names are what the
//! filter exists to stop people guessing at.

use std::io::IsTerminal;

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{colors, display, linear, output};

#[derive(Args, Debug)]
pub struct ViewListArgs {
    /// Include archived views
    #[arg(long = "include-archived")]
    pub include_archived: bool,
    /// Maximum number of views to show (0 for no limit)
    #[arg(long = "limit", default_value_t = 50)]
    pub limit: i64,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ViewListArgs) -> Result<()> {
    if args.limit < 0 {
        return Err(CliError::validation("--limit must be 0 or greater"));
    }
    // `None` is "everything", which is what `0` means at the flag.
    let limit = if args.limit == 0 {
        None
    } else {
        Some(args.limit as u32)
    };

    let (views, page_info) = linear::list_views(limit, args.include_archived)?;

    if args.json {
        output::print_json(&json!({ "nodes": views, "pageInfo": page_info }));
        return Ok(());
    }

    if views.is_empty() {
        output::line("No views found.");
        return Ok(());
    }

    let columns = terminal_columns();
    const ID_WIDTH: usize = 36;
    const SHARED_WIDTH: usize = 6;
    let scope_width = views
        .iter()
        .map(|view| display::display_width(&scope_of(view)))
        .max()
        .unwrap_or(9)
        .clamp(9, 15);

    const SEPARATORS: usize = 4;
    let fixed = ID_WIDTH + scope_width + SHARED_WIDTH + SEPARATORS;
    let available = columns.saturating_sub(fixed);
    let name_width = views
        .iter()
        .map(|view| display::display_width(&name_of(view)))
        .max()
        .unwrap_or(0)
        .min(available.saturating_sub(20).max(12));
    let filter_width = available.saturating_sub(name_width).max(20);

    let header_cells = [
        display::pad_display("ID", ID_WIDTH),
        display::pad_display("NAME", name_width),
        display::pad_display("FILTER", filter_width),
        display::pad_display("SCOPE", scope_width),
        display::pad_display("SHARED", SHARED_WIDTH),
    ];
    let header = header_cells
        .iter()
        .map(|cell| colors::underline(cell))
        .collect::<Vec<_>>()
        .join(" ");
    output::line(&header);

    for view in &views {
        let id = display::pad_display(
            view.get("id").and_then(Value::as_str).unwrap_or(""),
            ID_WIDTH,
        );
        let name = display::pad_display(&truncate(&name_of(view), name_width), name_width);
        let filter =
            display::pad_display(&truncate(&filter_summary(view), filter_width), filter_width);
        let scope = display::pad_display(&scope_of(view), scope_width);
        let shared = display::pad_display(if is_shared(view) { "yes" } else { "no" }, SHARED_WIDTH);

        output::line(&format!("{id} {name} {filter} {scope} {shared}"));
    }

    output::blank();
    output::line(&format!("{} views found.", views.len()));
    Ok(())
}

fn name_of(view: &Value) -> String {
    view.get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn is_shared(view: &Value) -> bool {
    view.get("shared").and_then(Value::as_bool).unwrap_or(false)
}

fn scope_of(view: &Value) -> String {
    view.get("team")
        .filter(|team| !team.is_null())
        .map(|team| {
            let key = team.get("key").and_then(Value::as_str).unwrap_or("");
            let name = team.get("name").and_then(Value::as_str).unwrap_or("");
            format!("{key} ({name})")
        })
        .unwrap_or_else(|| "Workspace".to_string())
}

/// The saved filter, compact enough for a column. A view with no filter says so rather than
/// printing an empty cell that reads like a rendering bug.
fn filter_summary(view: &Value) -> String {
    match view.get("filterData") {
        Some(filter) if !filter.is_null() => {
            serde_json::to_string(filter).unwrap_or_else(|_| "(unprintable)".to_string())
        }
        _ => "(no filter)".to_string(),
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
