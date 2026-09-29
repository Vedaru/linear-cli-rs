//! `linear initiative` — port of `src/commands/initiative/initiative.ts` and its
//! subcommands.
//!
//! With no subcommand the group prints its own help (`augment_args` +
//! `print_help`), mirroring upstream's `this.showHelp()` action.
//!
//! Error context is owned here, with two exceptions. [`initiative_list::run`]
//! and [`initiative_view::run`] each supply their own context (`Failed to fetch
//! initiatives`, `Failed to fetch initiative details`) and are not wrapped
//! again. Everything else is wrapped with the same message upstream uses, so the
//! subcommand modules must return errors unwrapped.

mod bulk;
mod initiative_add_project;
mod initiative_archive;
mod initiative_comment;
mod initiative_comment_add;
mod initiative_comment_list;
mod initiative_create;
mod initiative_delete;
mod initiative_list;
mod initiative_remove_project;
mod initiative_unarchive;
mod initiative_update;
mod initiative_view;

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct InitiativeArgs {
    #[command(subcommand)]
    pub command: Option<InitiativeCommand>,
}

#[derive(Subcommand, Debug)]
pub enum InitiativeCommand {
    /// List initiatives
    #[command(alias = "ls")]
    List(initiative_list::InitiativeListArgs),
    /// View initiative details
    View(initiative_view::InitiativeViewArgs),
    /// Create a new Linear initiative
    Create(initiative_create::InitiativeCreateArgs),
    /// Update a Linear initiative
    Update(initiative_update::InitiativeUpdateArgs),
    /// Archive a Linear initiative
    Archive(initiative_archive::InitiativeArchiveArgs),
    /// Unarchive a Linear initiative
    Unarchive(initiative_unarchive::InitiativeUnarchiveArgs),
    /// Permanently delete a Linear initiative
    Delete(initiative_delete::InitiativeDeleteArgs),
    /// Link a project to an initiative
    AddProject(initiative_add_project::InitiativeAddProjectArgs),
    /// Unlink a project from an initiative
    RemoveProject(initiative_remove_project::InitiativeRemoveProjectArgs),
    /// Manage initiative comments
    Comment(initiative_comment::InitiativeCommentArgs),
}

pub fn run(args: InitiativeArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd =
            <InitiativeArgs as clap::Args>::augment_args(clap::Command::new("initiative"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        // Self-wraps with `Failed to fetch initiatives`.
        InitiativeCommand::List(args) => initiative_list::run(args),
        // Self-wraps with `Failed to fetch initiative details`.
        InitiativeCommand::View(args) => initiative_view::run(args),
        InitiativeCommand::Create(args) => initiative_create::run(args)
            .map_err(|error| error.with_context("Failed to create initiative")),
        InitiativeCommand::Update(args) => initiative_update::run(args)
            .map_err(|error| error.with_context("Failed to update initiative")),
        InitiativeCommand::Archive(args) => initiative_archive::run(args)
            .map_err(|error| error.with_context("Failed to archive initiative")),
        InitiativeCommand::Unarchive(args) => initiative_unarchive::run(args)
            .map_err(|error| error.with_context("Failed to unarchive initiative")),
        InitiativeCommand::Delete(args) => initiative_delete::run(args)
            .map_err(|error| error.with_context("Failed to delete initiative")),
        InitiativeCommand::AddProject(args) => initiative_add_project::run(args)
            .map_err(|error| error.with_context("Failed to add project to initiative")),
        InitiativeCommand::RemoveProject(args) => initiative_remove_project::run(args)
            .map_err(|error| error.with_context("Failed to remove project from initiative")),
        // The comment subgroup supplies its own per-subcommand context.
        InitiativeCommand::Comment(args) => initiative_comment::run(args),
    }
}
