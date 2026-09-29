//! `linear initiative-update` — port of
//! `src/commands/initiative-update/initiative-update.ts` and its subcommands.
//!
//! The group itself has no action; with no subcommand it prints help,
//! matching upstream's `this.showHelp()`.
//!
//! Error context lives here: upstream wraps each action in
//! `handleError(error, "Failed to <action>")`, so the subcommands return bare
//! errors and this module supplies the prefix.

mod initiative_update_create;
mod initiative_update_list;

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct InitiativeUpdateArgs {
    #[command(subcommand)]
    pub command: Option<InitiativeUpdateCommand>,
}

#[derive(Subcommand, Debug)]
pub enum InitiativeUpdateCommand {
    /// Create a new status update for an initiative
    #[command(alias = "c")]
    Create(initiative_update_create::InitiativeUpdateCreateArgs),
    /// List status updates for an initiative
    #[command(alias = "l")]
    List(initiative_update_list::InitiativeUpdateListArgs),
}

pub fn run(args: InitiativeUpdateArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <InitiativeUpdateArgs as clap::Args>::augment_args(clap::Command::new(
            "initiative-update",
        ));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        InitiativeUpdateCommand::Create(a) => initiative_update_create::run(a)
            .map_err(|error| error.with_context("Failed to create initiative status update")),
        InitiativeUpdateCommand::List(a) => initiative_update_list::run(a)
            .map_err(|error| error.with_context("Failed to fetch initiative updates")),
    }
}
