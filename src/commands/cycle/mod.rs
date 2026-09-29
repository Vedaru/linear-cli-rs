//! `linear cycle` — port of `src/commands/cycle/`.
//!
//! `list` and `view` (alias `v`) match the upstream cliffy tree; `update` and
//! `archive` are additions, because upstream leaves the group read-only while the
//! API can change a cycle (`cycleUpdate`) and retire one (`cycleArchive`, with no
//! unarchive — Linear archives cycles automatically, but never restores them).
//! `linear cycle` with no subcommand prints the group help, as upstream's no-op
//! action calls `this.showHelp()`.
//!
//! Each action wraps its failure with the same context string upstream passes
//! to `handleError`, so error output matches the TypeScript CLI.

use clap::{Args, Subcommand};

use crate::errors::{CliError, Result};
use crate::linear;
use crate::output;

mod cycle_archive;
mod cycle_list;
mod cycle_update;
mod cycle_view;

#[derive(Args, Debug)]
pub struct CycleArgs {
    #[command(subcommand)]
    pub command: Option<CycleCommand>,
}

#[derive(Subcommand, Debug)]
pub enum CycleCommand {
    /// List cycles for a team
    List(cycle_list::ListArgs),
    /// View cycle details
    #[command(alias = "v")]
    View(cycle_view::ViewArgs),
    /// Update a cycle's name, description, or dates
    Update(cycle_update::CycleUpdateArgs),
    /// Archive a cycle (the API's cycleArchive; there is no unarchive)
    Archive(cycle_archive::CycleArchiveArgs),
}

pub fn run(args: CycleArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <CycleArgs as clap::Args>::augment_args(clap::Command::new("cycle"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        CycleCommand::List(args) => {
            cycle_list::run(args).map_err(|error| error.with_context("Failed to list cycles"))
        }
        CycleCommand::View(args) => cycle_view::run(args)
            .map_err(|error| error.with_context("Failed to fetch cycle details")),
        CycleCommand::Update(args) => {
            cycle_update::run(args).map_err(|error| error.with_context("Failed to update cycle"))
        }
        CycleCommand::Archive(args) => {
            cycle_archive::run(args).map_err(|error| error.with_context("Failed to archive cycle"))
        }
    }
}

/// `name`, else `Cycle <number>`, else the reference the caller typed.
pub(crate) fn cycle_label(cycle: &serde_json::Value, fallback: &str) -> String {
    match cycle.get("name").and_then(serde_json::Value::as_str) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => match cycle.get("number").and_then(serde_json::Value::as_i64) {
            Some(number) => format!("Cycle {number}"),
            None => fallback.to_string(),
        },
    }
}

/// Resolve a cycle reference for the mutating subcommands.
///
/// A UUID is taken as-is (no team needed); anything else is a cycle number, name,
/// or URL, which Linear can only look up within one team: `--team`, else the
/// configured team. Shared so `update` and `archive` resolve identically.
pub(crate) fn resolve_cycle_id(cycle_ref: &str, team: Option<&str>) -> Result<String> {
    if linear::is_linear_uuid(cycle_ref) {
        return Ok(cycle_ref.to_string());
    }

    let team_key = match team {
        Some(team) => linear::resolve_team(team)?.key,
        None => linear::get_team_key()?.ok_or_else(|| {
            CliError::validation("Could not determine team key from directory name or team flag")
        })?,
    };
    let team_id = linear::resolve_team(&team_key)?.id;
    linear::get_cycle_id_by_name_or_number(&team_id, cycle_ref)
}
