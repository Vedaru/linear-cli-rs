//! `linear roadmap list` — every roadmap this token can see.
//!
//! The shape follows `label list` and `view list`: `{nodes, pageInfo}` under `--json`, a table and
//! a count on the human path. Projects are not listed here - that is `roadmap view`, one roadmap at
//! a time, because a listing that fetched every project of every roadmap would be slow for a field
//! it does not print.

use std::io::IsTerminal;

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{colors, display, linear, output};

#[derive(Args, Debug)]
pub struct RoadmapListArgs {
    /// Include archived roadmaps
    #[arg(long = "include-archived")]
    pub include_archived: bool,
    /// Maximum number of roadmaps to show (0 for no limit)
    #[arg(long = "limit", default_value_t = 50)]
    pub limit: i64,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: RoadmapListArgs) -> Result<()> {
    if args.limit < 0 {
        return Err(CliError::validation("--limit must be 0 or greater"));
    }
    let limit = if args.limit == 0 {
        None
    } else {
        Some(args.limit as u32)
    };

    let (roadmaps, page_info) = linear::list_roadmaps(limit, args.include_archived)?;

    if args.json {
        output::print_json(&json!({ "nodes": roadmaps, "pageInfo": page_info }));
        return Ok(());
    }

    if roadmaps.is_empty() {
        // The empty case carries the reason, because "No roadmaps found." on its own invites the
        // reader to go looking for the `roadmap create` that will never exist.
        output::line(
            "No roadmaps found. Linear deprecated roadmaps - use `linear initiative list`.",
        );
        return Ok(());
    }

    let columns = terminal_columns();
    const ID_WIDTH: usize = 36;
    const COLOR_WIDTH: usize = 7;
    let owner_width = roadmaps
        .iter()
        .map(|roadmap| display::display_width(&owner_of(roadmap)))
        .max()
        .unwrap_or(5)
        .clamp(5, 20);

    const SEPARATORS: usize = 3;
    let fixed = ID_WIDTH + COLOR_WIDTH + owner_width + SEPARATORS;
    let name_width = roadmaps
        .iter()
        .map(|roadmap| display::display_width(&name_of(roadmap)))
        .max()
        .unwrap_or(4)
        .min(columns.saturating_sub(fixed).max(12));

    let header_cells = [
        display::pad_display("ID", ID_WIDTH),
        display::pad_display("NAME", name_width),
        display::pad_display("OWNER", owner_width),
        display::pad_display("COLOR", COLOR_WIDTH),
    ];
    let header = header_cells
        .iter()
        .map(|cell| colors::underline(cell))
        .collect::<Vec<_>>()
        .join(" ");
    output::line(&header);

    for roadmap in &roadmaps {
        let id = display::pad_display(
            roadmap.get("id").and_then(Value::as_str).unwrap_or(""),
            ID_WIDTH,
        );
        let name = display::pad_display(&truncate(&name_of(roadmap), name_width), name_width);
        let owner = display::pad_display(&owner_of(roadmap), owner_width);
        let color = display::pad_display(
            roadmap.get("color").and_then(Value::as_str).unwrap_or(""),
            COLOR_WIDTH,
        );
        output::line(&format!("{id} {name} {owner} {color}"));
    }

    output::blank();
    output::line(&format!("{} roadmaps found.", roadmaps.len()));
    Ok(())
}

fn name_of(roadmap: &Value) -> String {
    roadmap
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn owner_of(roadmap: &Value) -> String {
    roadmap
        .get("owner")
        .filter(|owner| !owner.is_null())
        .and_then(|owner| owner.get("displayName"))
        .and_then(Value::as_str)
        .unwrap_or("(none)")
        .to_string()
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
