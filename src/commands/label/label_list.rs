//! `linear label list` — port of `src/commands/label/label-list.ts`.

use std::io::IsTerminal;

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::Result;
use crate::{colors, display, graphql, linear, output};

const GET_ISSUE_LABELS_QUERY: &str = r#"
query GetIssueLabels($filter: IssueLabelFilter, $first: Int, $after: String) {
  issueLabels(filter: $filter, first: $first, after: $after) {
    nodes {
      id
      name
      description
      color
      team {
        key
        name
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct LabelListArgs {
    /// Filter by team key, name, or ID (e.g., TC). Shows that team's labels plus workspace labels.
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Show only workspace-level labels (not team-specific)
    #[arg(long)]
    pub workspace: bool,
    /// Show all labels (both workspace and team)
    #[arg(long)]
    pub all: bool,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: LabelListArgs) -> Result<()> {
    let client = graphql::client()?;

    // Build the filter exactly as upstream does.
    let mut filter: Map<String, Value> = Map::new();
    if args.workspace {
        filter.insert("team".to_string(), json!({ "null": true }));
    } else if let Some(team_key) = &args.team {
        let team = linear::resolve_team(team_key)?;
        filter = or_filter(&team.key);
    } else if !args.all {
        if let Some(default_team) = linear::get_team_key()? {
            filter = or_filter(&default_team);
        }
        // If no team configured and not --all, show all anyway.
    }

    let mut variables = Map::new();
    if !filter.is_empty() {
        variables.insert("filter".to_string(), Value::Object(filter));
    }
    variables.insert("first".to_string(), json!(100));

    let (mut labels, page_info) =
        client.paginate_connection_page(GET_ISSUE_LABELS_QUERY, variables, &["issueLabels"])?;

    if labels.is_empty() {
        if args.json {
            output::print_json(&json!({ "nodes": labels, "pageInfo": page_info }));
        } else {
            output::line("No labels found.");
        }
        return Ok(());
    }

    labels.sort_by_key(|label| {
        label
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase()
    });

    if args.json {
        output::print_json(&json!({ "nodes": labels, "pageInfo": page_info }));
        return Ok(());
    }

    let columns = terminal_columns();

    const ID_WIDTH: usize = 36;
    const COLOR_WIDTH: usize = 7;
    let team_width = labels
        .iter()
        .map(|label| display::display_width(&team_display(label)))
        .max()
        .unwrap_or(4)
        .clamp(4, 15);

    const SPACE_WIDTH: usize = 6;
    let fixed = ID_WIDTH + COLOR_WIDTH + team_width + SPACE_WIDTH;
    const PADDING: usize = 1;
    let max_name_width = labels
        .iter()
        .map(|label| display::display_width(label_name(label)))
        .max()
        .unwrap_or(0);
    let available_width = columns.saturating_sub(PADDING + fixed);
    let name_width = max_name_width.min(available_width.max(20));

    let header_cells = [
        display::pad_display("ID", ID_WIDTH),
        display::pad_display("NAME", name_width),
        display::pad_display("COLOR", COLOR_WIDTH),
        display::pad_display("TEAM", team_width),
    ];
    let header = header_cells
        .iter()
        .map(|cell| colors::underline(cell))
        .collect::<Vec<_>>()
        .join(" ");
    output::line(&header);

    for label in &labels {
        let name = label_name(label);
        let trunc_name = truncate_name(name, name_width);

        let id_display = display::pad_display(
            label.get("id").and_then(Value::as_str).unwrap_or(""),
            ID_WIDTH,
        );
        let color_display = display::pad_display(
            label.get("color").and_then(Value::as_str).unwrap_or(""),
            COLOR_WIDTH,
        );
        let team_col = display::pad_display(&team_display(label), team_width);

        output::line(&format!(
            "{id_display} {trunc_name} {color_display} {team_col}"
        ));
    }

    output::blank();
    output::line(&format!("{} labels found.", labels.len()));
    Ok(())
}

fn or_filter(team_key: &str) -> Map<String, Value> {
    let mut filter = Map::new();
    filter.insert(
        "or".to_string(),
        json!([
            { "team": { "key": { "eq": team_key } } },
            { "team": { "null": true } },
        ]),
    );
    filter
}

fn label_name(label: &Value) -> &str {
    label.get("name").and_then(Value::as_str).unwrap_or("")
}

fn team_display(label: &Value) -> String {
    label
        .get("team")
        .filter(|team| !team.is_null())
        .and_then(|team| team.get("key"))
        .and_then(Value::as_str)
        .unwrap_or("Workspace")
        .to_string()
}

fn truncate_name(name: &str, width: usize) -> String {
    if name.chars().count() > width {
        let take = width.saturating_sub(3);
        let mut truncated: String = name.chars().take(take).collect();
        truncated.push_str("...");
        truncated
    } else {
        display::pad_display(name, width)
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
