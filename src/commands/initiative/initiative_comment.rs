//! `linear initiative comment` — port of
//! `src/commands/initiative/initiative-comment.ts`.
//!
//! The group itself has no action; with no subcommand it prints help,
//! matching upstream's `this.showHelp()`.
//!
//! Error context lives here, as in the project comment subgroup: upstream wraps
//! each subcommand's action in `handleError(error, "Failed to ...")`, so
//! [`initiative_comment_add::run`] and [`initiative_comment_list::run`] return
//! bare errors and this module supplies the prefix. The parent `initiative`
//! group therefore dispatches to [`run`] without wrapping.

use clap::{Args, Subcommand};

use crate::commands::initiative::initiative_comment_add::{self, InitiativeCommentAddArgs};
use crate::commands::initiative::initiative_comment_list::{self, InitiativeCommentListArgs};
use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct InitiativeCommentArgs {
    #[command(subcommand)]
    pub command: Option<InitiativeCommentCommand>,
}

#[derive(Subcommand, Debug)]
pub enum InitiativeCommentCommand {
    /// Add a comment or reply to an initiative's discussion (by ID, slug, or name)
    Add(InitiativeCommentAddArgs),
    /// List comments on an initiative (by ID, slug, or name)
    List(InitiativeCommentListArgs),
}

pub fn run(args: InitiativeCommentArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd =
            <InitiativeCommentArgs as clap::Args>::augment_args(clap::Command::new("comment"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        InitiativeCommentCommand::Add(a) => initiative_comment_add::run(a)
            .map_err(|error| error.with_context("Failed to add comment")),
        InitiativeCommentCommand::List(a) => initiative_comment_list::run(a)
            .map_err(|error| error.with_context("Failed to list comments")),
    }
}
