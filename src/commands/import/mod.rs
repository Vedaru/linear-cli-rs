//! `linear import` — read an export back into Linear.
//!
//! An addition: upstream advertises import but ships none. The contract is the one the ticket
//! asks for - a dry run by default, and only what differs is written, so re-importing an unchanged
//! export is a no-op that says so rather than a rewrite nobody asked for.

pub mod import_issues;

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct ImportArgs {
    #[command(subcommand)]
    pub command: Option<ImportCommand>,
}

#[derive(Subcommand, Debug)]
pub enum ImportCommand {
    /// Import issues from an export (CSV or JSON); dry run unless --apply
    Issues(import_issues::ImportIssuesArgs),
}

pub fn run(args: ImportArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <ImportArgs as clap::Args>::augment_args(clap::Command::new("import"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        ImportCommand::Issues(args) => {
            import_issues::run(args).map_err(|error| error.with_context("Failed to import issues"))
        }
    }
}
