//! `linear initiative list` — port of
//! `src/commands/initiative/initiative-list.ts`.
//!
//! This module self-wraps: the listing reports `Failed to fetch initiatives`
//! (upstream's `handleError` context), while upstream's `--web`/`--app` branch
//! runs *outside* that wrapper and is therefore left unwrapped here too. The
//! group `mod.rs` must not wrap it again.
//!
//! The listing is a hand-built table (there is no table helper): header cells
//! padded by `display::pad_display`, styled with `colors::underline`, joined by
//! a single space — the rendered form of upstream's `%c` header placeholders,
//! whose CSS a terminal ignores.
//!
//! Two module-private documents, text-identical to upstream: `GetInitiatives`
//! (the listing) and `GetViewerForInitiatives` (the workspace lookup that
//! `--web`/`--app` falls back to when no workspace is configured).

use std::io::IsTerminal;

use clap::Args;
use serde_json::{json, Map, Value};

use crate::display;
use crate::errors::{CliError, Result};
use crate::{actions, colors, config, consts, graphql, linear, output};

const GET_INITIATIVES_QUERY: &str = r#"
query GetInitiatives($filter: InitiativeFilter, $includeArchived: Boolean) {
  initiatives(filter: $filter, includeArchived: $includeArchived) {
    nodes {
      id
      slugId
      name
      description
      status
      targetDate
      health
      color
      icon
      url
      archivedAt
      owner {
        id
        displayName
        initials
      }
      projects {
        nodes {
          id
          name
          status {
            name
          }
        }
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

const GET_VIEWER_FOR_INITIATIVES_QUERY: &str = r#"
query GetViewerForInitiatives {
  viewer {
    organization {
      urlKey
    }
  }
}
"#;

/// Upstream's `STATUS_INPUT_MAP`: what `--status` accepts, and the API value
/// each input maps to.
const STATUS_INPUT_MAP: [(&str, &str); 3] = [
    ("active", "Active"),
    ("planned", "Planned"),
    ("completed", "Completed"),
];

/// Upstream's `statusColors` fallback. The target-date column uses it too, as
/// the port of upstream's `color: gray` placeholder.
const DEFAULT_STATUS_COLOR: &str = "#6B6F76";

#[derive(Args, Debug)]
pub struct InitiativeListArgs {
    /// Filter by status (active, planned, completed)
    #[arg(short = 's', long, value_name = "status")]
    pub status: Option<String>,
    /// Show all statuses (default: active only)
    #[arg(long = "all-statuses")]
    pub all_statuses: bool,
    /// Filter by owner (username or email)
    #[arg(short = 'o', long, value_name = "owner")]
    pub owner: Option<String>,
    /// Open initiatives page in web browser
    #[arg(short = 'w', long)]
    pub web: bool,
    /// Open initiatives page in Linear.app
    #[arg(short = 'a', long)]
    pub app: bool,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
    /// Include archived initiatives
    #[arg(long)]
    pub archived: bool,
}

pub fn run(args: InitiativeListArgs) -> Result<()> {
    if args.web || args.app {
        return open_initiatives(&args);
    }
    list(&args).map_err(|error| error.with_context("Failed to fetch initiatives"))
}

/// `--web`/`--app`: open `{web base}/{workspace}/initiatives`. The workspace
/// comes from the CLI/configuration, falling back to the viewer's organization
/// url key — exactly the two-step lookup upstream performs.
fn open_initiatives(args: &InitiativeListArgs) -> Result<()> {
    let workspace = match config::cli_workspace().or_else(config::workspace) {
        Some(workspace) => workspace,
        None => {
            let client = graphql::client()?;
            let data = client.request(GET_VIEWER_FOR_INITIATIVES_QUERY, json!({}))?;
            data.pointer("/viewer/organization/urlKey")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| CliError::cli("Linear API returned no workspace URL key"))?
        }
    };

    let url = format!("{}/{}/initiatives", consts::LINEAR_WEB_BASE_URL, workspace);
    let app = args.app;
    let destination = if app { "Linear.app" } else { "web browser" };
    output::line(&format!("Opening {url} in {destination}"));
    actions::open_url(&url, app)
}

fn list(args: &InitiativeListArgs) -> Result<()> {
    // --- build the filter --------------------------------------------------
    let mut filter = Map::new();

    // An explicit --status wins; otherwise the listing is active-only unless
    // --all-statuses asked for everything.
    if let Some(status) = &args.status {
        let requested = status.to_lowercase();
        let Some(api_status) = STATUS_INPUT_MAP
            .iter()
            .find(|(input, _)| *input == requested)
            .map(|(_, api_status)| *api_status)
        else {
            let valid_values = STATUS_INPUT_MAP
                .iter()
                .map(|(input, _)| *input)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(CliError::validation(format!(
                "Invalid status: {status}. Valid values: {valid_values}"
            )));
        };
        filter.insert("status".to_string(), json!({ "eq": api_status }));
    } else if !args.all_statuses {
        filter.insert("status".to_string(), json!({ "eq": "Active" }));
    }

    // An owner the API cannot resolve is a hard error, as upstream's
    // `NotFoundError("Owner", owner)`.
    if let Some(owner) = &args.owner {
        let Some(owner_id) = linear::lookup_user_id(owner)? else {
            return Err(CliError::not_found("Owner", owner));
        };
        filter.insert("owner".to_string(), json!({ "id": { "eq": owner_id } }));
    }

    let mut variables = Map::new();
    if !filter.is_empty() {
        variables.insert("filter".to_string(), Value::Object(filter));
    }
    variables.insert("includeArchived".to_string(), json!(args.archived));

    let client = graphql::client()?;
    let result = client.request(GET_INITIATIVES_QUERY, Value::Object(variables))?;

    // `result.initiatives ?? { nodes: [], pageInfo: … }`: a null connection
    // lists nothing rather than failing.
    let connection = result
        .get("initiatives")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "nodes": [],
                "pageInfo": { "hasNextPage": false, "endCursor": null },
            })
        });

    let mut initiatives = connection
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if initiatives.is_empty() {
        if args.json {
            output::print_json(&connection);
        } else {
            output::line("No initiatives found.");
        }
        return Ok(());
    }

    // Status order first, then name, matching upstream's comparator (an
    // unknown status sorts last via the 999 sentinel).
    initiatives.sort_by(|a, b| {
        let status_a = status_order(str_of(a, "status"));
        let status_b = status_order(str_of(b, "status"));
        if status_a != status_b {
            return status_a.cmp(&status_b);
        }
        str_of(a, "name").cmp(str_of(b, "name"))
    });

    if args.json {
        let page_info = connection
            .get("pageInfo")
            .cloned()
            .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));
        output::print_json(&json!({ "nodes": initiatives, "pageInfo": page_info }));
        return Ok(());
    }

    render_table(&initiatives);
    Ok(())
}

fn render_table(initiatives: &[Value]) {
    let columns = if std::io::stdout().is_terminal() {
        terminal_size::terminal_size()
            .map(|(width, _)| width.0 as usize)
            .unwrap_or(120)
    } else {
        120
    };

    // --- measure columns ---------------------------------------------------
    let slug_width = initiatives
        .iter()
        .map(|initiative| display::display_width(str_of(initiative, "slugId")))
        .fold(4usize, usize::max);
    // Upstream measures the *display* name; for the three API values the
    // display map is the identity, so the raw status is the same string.
    let status_width = initiatives
        .iter()
        .map(|initiative| display::display_width(str_of(initiative, "status")))
        .fold(6usize, usize::max);
    let health_width = initiatives
        .iter()
        .map(|initiative| display::display_width(&health_of(initiative)))
        .fold(6usize, usize::max);
    let owner_width = initiatives
        .iter()
        .map(|initiative| display::display_width(&owner_of(initiative)))
        .fold(5usize, usize::max);
    let projects_width = initiatives
        .iter()
        .map(|initiative| display::display_width(&project_count(initiative)))
        .fold(4usize, usize::max);
    let target_width = initiatives
        .iter()
        .map(|initiative| display::display_width(&target_of(initiative)))
        .fold(10usize, usize::max);

    let space_width = 6usize;
    let fixed = slug_width
        + status_width
        + health_width
        + owner_width
        + projects_width
        + target_width
        + space_width;
    let padding = 1usize;
    let max_name_width = initiatives
        .iter()
        .map(|initiative| display::display_width(str_of(initiative, "name")))
        .max()
        .unwrap_or(0);
    let available_width = columns.saturating_sub(padding + fixed).max(10);
    let name_width = max_name_width.min(available_width);

    // --- header ------------------------------------------------------------
    let header = [
        display::pad_display("SLUG", slug_width),
        display::pad_display("NAME", name_width),
        display::pad_display("STATUS", status_width),
        display::pad_display("HEALTH", health_width),
        display::pad_display("OWNER", owner_width),
        display::pad_display("PROJ", projects_width),
        display::pad_display("TARGET", target_width),
    ]
    .iter()
    .map(|cell| colors::underline(cell))
    .collect::<Vec<_>>()
    .join(" ");
    output::line(&header);

    // --- rows --------------------------------------------------------------
    for initiative in initiatives {
        let status_cell = colors::color_hex(
            status_color(str_of(initiative, "status")),
            &display::pad_display(str_of(initiative, "status"), status_width),
        );
        let name_cell = display::pad_display(
            &display::truncate_text(str_of(initiative, "name"), name_width),
            name_width,
        );
        let target_cell = colors::color_hex(
            DEFAULT_STATUS_COLOR,
            &display::pad_display(&target_of(initiative), target_width),
        );

        output::line(&format!(
            "{} {} {} {} {} {} {}",
            display::pad_display(str_of(initiative, "slugId"), slug_width),
            name_cell,
            status_cell,
            display::pad_display(&health_of(initiative), health_width),
            display::pad_display(&owner_of(initiative), owner_width),
            display::pad_display(&project_count(initiative), projects_width),
            target_cell,
        ));
    }
}

/// Upstream's `INITIATIVE_STATUS_ORDER`, with 999 for a status the table does
/// not know.
fn status_order(status: &str) -> i64 {
    match status {
        "Active" => 1,
        "Planned" => 2,
        "Completed" => 3,
        _ => 999,
    }
}

/// Upstream's `statusColors`, falling back to the default gray.
fn status_color(status: &str) -> &'static str {
    match status {
        "Active" => "#27AE60",
        "Planned" => "#5E6AD2",
        "Completed" => "#6B6F76",
        _ => DEFAULT_STATUS_COLOR,
    }
}

fn health_of(initiative: &Value) -> String {
    non_empty(initiative.get("health")).unwrap_or_else(|| "-".to_string())
}

fn owner_of(initiative: &Value) -> String {
    non_empty(initiative.pointer("/owner/initials")).unwrap_or_else(|| "-".to_string())
}

fn target_of(initiative: &Value) -> String {
    non_empty(initiative.get("targetDate")).unwrap_or_else(|| "-".to_string())
}

/// `String(initiative.projects?.nodes?.length || 0)`.
fn project_count(initiative: &Value) -> String {
    initiative
        .pointer("/projects/nodes")
        .and_then(Value::as_array)
        .map(|nodes| nodes.len())
        .unwrap_or(0)
        .to_string()
}

fn non_empty(value: Option<&Value>) -> Option<String> {
    match value.and_then(Value::as_str) {
        Some(text) if !text.is_empty() => Some(text.to_string()),
        _ => None,
    }
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
