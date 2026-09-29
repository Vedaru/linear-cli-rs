//! `linear label` — manage Linear issue labels. Port of
//! `src/commands/label/label.ts`.
//!
//! The group has no action of its own: with no subcommand it prints help,
//! matching upstream's `this.showHelp()`. Each action wraps its failure with
//! the same context string upstream passes to `handleError`, so error output
//! matches the TypeScript CLI.

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

mod label_create;
mod label_delete;
mod label_list;
mod label_update;
mod support;

#[derive(Args, Debug)]
pub struct LabelArgs {
    #[command(subcommand)]
    pub command: Option<LabelCommand>,
}

#[derive(Subcommand, Debug)]
pub enum LabelCommand {
    /// List issue labels
    List(label_list::LabelListArgs),
    /// Create a new issue label
    Create(label_create::LabelCreateArgs),
    /// Delete an issue label
    Delete(label_delete::LabelDeleteArgs),
    /// Update an issue label (rename, recolour, redescribe — the API's
    /// issueLabelUpdate; upstream cannot change a label once created)
    Update(label_update::LabelUpdateArgs),
}

pub fn run(args: LabelArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <LabelArgs as clap::Args>::augment_args(clap::Command::new("label"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        LabelCommand::List(args) => {
            label_list::run(args).map_err(|error| error.with_context("Failed to fetch labels"))
        }
        LabelCommand::Create(args) => {
            label_create::run(args).map_err(|error| error.with_context("Failed to create label"))
        }
        LabelCommand::Delete(args) => {
            label_delete::run(args).map_err(|error| error.with_context("Failed to delete label"))
        }
        LabelCommand::Update(args) => {
            label_update::run(args).map_err(|error| error.with_context("Failed to update label"))
        }
    }
}
