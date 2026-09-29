//! `linear cycle` — port of `src/commands/cycle/`.
//!
//! Two subcommands, `list` and `view` (alias `v`), matching the upstream
//! cliffy tree. `linear cycle` with no subcommand prints the group help, as
//! upstream's no-op action calls `this.showHelp()`.
//!
//! Each action wraps its failure with the same context string upstream passes
//! to `handleError`, so error output matches the TypeScript CLI.

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

mod cycle_list;
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
    }
}
