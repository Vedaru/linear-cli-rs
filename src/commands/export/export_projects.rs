//! `linear export projects` — the project half of the transfer pair.
//!
//! Projects are read-only here: `linear import` covers issues only, as the ticket asks, so this is
//! a report (CSV for a spreadsheet, Markdown for a document) rather than half of a round trip.

use std::io::Write;

use clap::Args;
use serde_json::Value;

use super::{open_output, write_error, Format};
use crate::errors::{CliError, Result};
use crate::transfer;
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct ExportProjectsArgs {
    /// Team key, name, or ID (defaults to the whole workspace)
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// csv (default), json or markdown (ndjson would be one JSON object per project)
    #[arg(long, value_name = "format", default_value = "csv")]
    pub format: String,
    /// Write to this file instead of stdout (`-` is stdout)
    #[arg(short = 'o', long, value_name = "path")]
    pub output: Option<String>,
}

pub fn run(args: ExportProjectsArgs) -> Result<()> {
    let format = Format::parse(&args.format)?;
    if format == Format::Ndjson {
        return Err(
            CliError::validation("Projects export as csv, json or markdown, not ndjson")
                .suggestion(
                    "NDJSON is the streaming shape for the issue export; use --format json here.",
                ),
        );
    }

    let team_key = match args.team.as_deref() {
        Some(team) => Some(linear::resolve_team(team)?.key),
        None => None,
    };
    let document = linear::fetch_projects_for_export(team_key.as_deref())?;
    let nodes = document
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut out = open_output(args.output.as_deref())?;
    match format {
        Format::Json => {
            let text = output::to_pretty(&document);
            writeln!(out, "{text}").map_err(|error| write_error(&error))?;
        }
        Format::Csv => {
            writeln!(out, "{}", transfer::project_header()).map_err(|error| write_error(&error))?;
            for node in &nodes {
                let row = transfer::project_row(node);
                writeln!(out, "{}", crate::csv::encode_row(&row))
                    .map_err(|error| write_error(&error))?;
            }
        }
        Format::Markdown => {
            write!(out, "{}", render_markdown(&nodes)).map_err(|error| write_error(&error))?;
        }
        Format::Ndjson => unreachable!("refused above"),
    }

    out.flush().map_err(|error| write_error(&error))?;
    Ok(())
}

fn render_markdown(nodes: &[Value]) -> String {
    let mut text = String::new();
    for node in nodes {
        let name = node.get("name").and_then(Value::as_str).unwrap_or("");
        text.push_str(&format!("## {name}\n\n"));

        let mut facts: Vec<String> = Vec::new();
        for (label, path) in [
            ("Status", "/status/name"),
            ("Health", "health"),
            ("Lead", "/lead/displayName"),
            ("Start", "startDate"),
            ("Target", "targetDate"),
        ] {
            if let Some(value) = node.pointer(path).and_then(Value::as_str) {
                if !value.is_empty() {
                    facts.push(format!("- {label}: {value}"));
                }
            }
        }
        if let Some(priority) = node.get("priority").and_then(Value::as_i64) {
            facts.push(format!("- Priority: {priority}"));
        }
        let mut teams: Vec<String> = node
            .pointer("/teams/nodes")
            .and_then(Value::as_array)
            .map(|teams| {
                teams
                    .iter()
                    .filter_map(|team| team.get("key").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        teams.sort();
        if !teams.is_empty() {
            facts.push(format!("- Teams: {}", teams.join(", ")));
        }
        if let Some(url) = node.get("url").and_then(Value::as_str) {
            if !url.is_empty() {
                facts.push(format!("- URL: {url}"));
            }
        }
        text.push_str(&facts.join("\n"));
        text.push_str("\n\n");
    }
    text
}
