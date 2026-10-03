//! `linear export` — write issues and projects out as CSV, JSON, NDJSON or Markdown.
//!
//! An addition: upstream advertises import and export but ships neither. The JSON is the API's own
//! document (`{nodes, pageInfo}`, the shape `issue query --json` prints) rather than a second
//! serialiser, so export → import is identity by construction; NDJSON exists because a large team
//! should not have to be buffered before the first line is written.

pub mod export_issues;
pub mod export_projects;

use clap::{Args, Subcommand};

use crate::errors::{CliError, Result};
use crate::output;

#[derive(Args, Debug)]
pub struct ExportArgs {
    #[command(subcommand)]
    pub command: Option<ExportCommand>,
}

#[derive(Subcommand, Debug)]
pub enum ExportCommand {
    /// Export issues as CSV, JSON, NDJSON or Markdown
    Issues(export_issues::ExportIssuesArgs),
    /// Export projects as CSV, JSON or Markdown
    Projects(export_projects::ExportProjectsArgs),
}

pub fn run(args: ExportArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <ExportArgs as clap::Args>::augment_args(clap::Command::new("export"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        ExportCommand::Issues(args) => {
            export_issues::run(args).map_err(|error| error.with_context("Failed to export issues"))
        }
        ExportCommand::Projects(args) => export_projects::run(args)
            .map_err(|error| error.with_context("Failed to export projects")),
    }
}

/// The formats an export can write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Csv,
    Json,
    Ndjson,
    Markdown,
}

impl Format {
    /// Parse the `--format` value, naming the alternatives when it is not one of them.
    pub fn parse(value: &str) -> Result<Format> {
        match value.to_ascii_lowercase().as_str() {
            "csv" => Ok(Format::Csv),
            "json" => Ok(Format::Json),
            "ndjson" => Ok(Format::Ndjson),
            "markdown" | "md" => Ok(Format::Markdown),
            other => Err(CliError::validation(format!("Unknown format: {other}"))
                .suggestion("Use csv, json, ndjson or markdown.")),
        }
    }
}

/// A writer over `--output`, or stdout.
pub fn open_output(path: Option<&str>) -> Result<Box<dyn std::io::Write>> {
    match path {
        Some(path) if path != "-" => {
            let file = std::fs::File::create(path).map_err(|error| {
                CliError::cli(format!("Failed to create {path}: {error}")).suggestion(
                    "Check the directory exists and is writable, or drop --output to print instead.",
                )
            })?;
            Ok(Box::new(std::io::BufWriter::new(file)))
        }
        _ => Ok(Box::new(std::io::BufWriter::new(std::io::stdout()))),
    }
}

/// An I/O failure on the way out, with the command's own context.
pub fn write_error(error: &std::io::Error) -> CliError {
    CliError::cli(format!("Failed to write the export: {error}"))
}
