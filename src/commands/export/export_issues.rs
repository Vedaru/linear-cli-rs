//! `linear export issues` — the issue half of the transfer pair.
//!
//! One fetch feeds every format: the same `FetchIssuesForQueryOptions` the query layer builds, so
//! a `--team ENG --state started` export is the filter an agent would have typed into
//! `issue query`. JSON and CSV are collected only when their format needs the whole document
//! (Markdown's headings and CSV's header are written before the first page arrives; NDJSON never
//! holds more than one page).

use std::io::Write;

use clap::Args;
use serde_json::Value;

use super::{open_output, write_error, Format};
use crate::errors::{CliError, Result};
use crate::transfer;
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct ExportIssuesArgs {
    /// Team key, name, or ID (repeatable; defaults to the configured team)
    #[arg(long, value_name = "team")]
    pub team: Vec<String>,
    /// Export every team in the workspace
    #[arg(long)]
    pub all_teams: bool,
    /// Filter by state type or name (repeatable)
    #[arg(short = 's', long, value_name = "state")]
    pub state: Vec<String>,
    /// Filter by project (ID, slug, or name)
    #[arg(long, value_name = "project")]
    pub project: Option<String>,
    /// Filter by label (repeatable)
    #[arg(short = 'l', long, value_name = "label")]
    pub label: Vec<String>,
    /// Filter by assignee (name, email, or ID)
    #[arg(short = 'a', long, value_name = "assignee")]
    pub assignee: Option<String>,
    /// Only issues created after this date (YYYY-MM-DD or an age like 30d)
    #[arg(long, value_name = "date")]
    pub since: Option<String>,
    /// Maximum number of issues; 0 exports every one
    #[arg(long, default_value_t = 0)]
    pub limit: u32,
    /// csv (default), json, ndjson or markdown
    #[arg(long, value_name = "format", default_value = "csv")]
    pub format: String,
    /// Write to this file instead of stdout (`-` is stdout)
    #[arg(short = 'o', long, value_name = "path")]
    pub output: Option<String>,
}

pub fn run(args: ExportIssuesArgs) -> Result<()> {
    let format = Format::parse(&args.format)?;
    let options = query_options(&args)?;
    let mut out = open_output(args.output.as_deref())?;

    match format {
        Format::Json => {
            let document = linear::fetch_export_issues(&options)?;
            let text = output::to_pretty(&document);
            writeln!(out, "{text}").map_err(|error| write_error(&error))?;
        }
        Format::Ndjson => {
            linear::stream_export_issues(&options, |page| {
                for node in page {
                    let line = serde_json::to_string(node).map_err(|error| {
                        CliError::cli(format!("Failed to encode a row: {error}"))
                    })?;
                    writeln!(out, "{line}").map_err(|error| write_error(&error))?;
                }
                Ok(())
            })?;
        }
        Format::Csv => {
            writeln!(out, "{}", transfer::issue_header()).map_err(|error| write_error(&error))?;
            linear::stream_export_issues(&options, |page| {
                for node in page {
                    let row = transfer::issue_row(node);
                    writeln!(out, "{}", crate::csv::encode_row(&row))
                        .map_err(|error| write_error(&error))?;
                }
                Ok(())
            })?;
        }
        Format::Markdown => {
            let document = linear::fetch_export_issues(&options)?;
            let nodes = document
                .get("nodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            write!(out, "{}", render_markdown(&nodes)).map_err(|error| write_error(&error))?;
        }
    }

    // A BufWriter dropped without this loses its last buffer, and the command would report success
    // for a file that is missing its tail.
    out.flush().map_err(|error| write_error(&error))?;
    Ok(())
}

/// The query an export runs, built from the flags it shares with `issue query`.
fn query_options(args: &ExportIssuesArgs) -> Result<linear::FetchIssuesForQueryOptions> {
    if !args.team.is_empty() && args.all_teams {
        return Err(CliError::validation(
            "Cannot use both --team and --all-teams flags",
        ));
    }

    let team_keys: Option<Vec<String>> = if args.all_teams {
        None
    } else if !args.team.is_empty() {
        Some(
            linear::resolve_teams(&args.team)?
                .into_iter()
                .map(|team| team.key)
                .collect(),
        )
    } else {
        let Some(team) = linear::get_team_key()? else {
            return Err(CliError::validation(
                "No default team configured and no team scope provided",
            )
            .suggestion("Use --team <key, name, or ID>, or --all-teams for the whole workspace."));
        };
        Some(vec![team])
    };

    // The state scope follows the team scope: `--state started` on one team means that team's
    // started states, and on every team it means the type everywhere.
    let scope = match &team_keys {
        Some(keys) => linear::StateScope::TeamKeys(keys.clone()),
        None => linear::StateScope::AllTeams,
    };
    let state = if args.state.is_empty() {
        None
    } else {
        Some(linear::resolve_state_selection(&args.state, &scope)?)
    };

    let project_id = match args.project.as_deref() {
        Some(project) => Some(linear::resolve_project_id(project)?),
        None => None,
    };
    let assignee = match args.assignee.as_deref() {
        Some(reference) => linear::lookup_user_id(reference)?,
        None => None,
    };
    let created_after = match args.since.as_deref() {
        Some(since) => Some(linear::parse_date_filter_or_age(since, "--since")?),
        None => None,
    };

    Ok(linear::FetchIssuesForQueryOptions {
        team_keys,
        all_teams: args.all_teams,
        state,
        assignee,
        unassigned: false,
        sort: None,
        limit: Some(args.limit),
        project_id,
        project_label: None,
        cycle_id: None,
        milestone_id: None,
        label_names: if args.label.is_empty() {
            None
        } else {
            Some(args.label.clone())
        },
        created_after,
        updated_after: None,
        include_archived: Some(false),
        raw_filter: None,
    })
}

/// A Markdown rendering of the same nodes: for reading, not for re-importing.
fn render_markdown(nodes: &[Value]) -> String {
    let mut text = String::new();
    for node in nodes {
        let title = node.get("title").and_then(Value::as_str).unwrap_or("");
        let identifier = node.get("identifier").and_then(Value::as_str).unwrap_or("");
        text.push_str(&format!("## {identifier}: {title}\n\n"));

        let mut facts: Vec<String> = Vec::new();
        for (label, path) in [
            ("State", "/state/name"),
            ("Assignee", "/assignee/displayName"),
            ("Project", "/project/name"),
            ("Cycle", "/cycle/name"),
            ("Milestone", "/projectMilestone/name"),
            ("Due", "dueDate"),
        ] {
            if let Some(value) = node.pointer(path).and_then(Value::as_str) {
                if !value.is_empty() {
                    facts.push(format!("- {label}: {value}"));
                }
            }
        }
        if let Some(priority) = node.get("priority").and_then(Value::as_i64) {
            facts.push(format!("- Priority: {}", priority_label(priority)));
        }
        if let Some(estimate) = node.get("estimate").and_then(Value::as_i64) {
            facts.push(format!("- Estimate: {estimate}"));
        }
        let mut labels: Vec<String> = node
            .pointer("/labels/nodes")
            .and_then(Value::as_array)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(|label| label.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        // Sorted, so the document reads the same way the CSV column is written.
        labels.sort();
        if !labels.is_empty() {
            facts.push(format!("- Labels: {}", labels.join(", ")));
        }
        if let Some(url) = node.get("url").and_then(Value::as_str) {
            if !url.is_empty() {
                facts.push(format!("- URL: {url}"));
            }
        }
        if !facts.is_empty() {
            text.push_str(&facts.join("\n"));
            text.push_str("\n\n");
        }

        let description = node
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !description.is_empty() {
            text.push_str(description.trim_end());
            text.push_str("\n\n");
        }
    }
    text
}

/// Linear's priority scale, spelled out.
fn priority_label(priority: i64) -> String {
    match priority {
        1 => "Urgent".to_string(),
        2 => "High".to_string(),
        3 => "Medium".to_string(),
        4 => "Low".to_string(),
        _ => "No priority".to_string(),
    }
}
