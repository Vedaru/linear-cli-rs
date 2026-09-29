//! `linear milestone` — port of `src/commands/milestone/`.
//!
//! Five subcommands, `list`, `view` (alias `v`), `create`, `update`, and
//! `delete`, matching the upstream cliffy tree. `linear milestone` with no
//! subcommand prints the group help, as upstream's no-op action calls
//! `this.showHelp()`.
//!
//! Each action wraps its failure with the same context string upstream passes
//! to `handleError`, so error output matches the TypeScript CLI.

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

mod milestone_create;
mod milestone_delete;
mod milestone_list;
mod milestone_update;
mod milestone_view;

#[derive(Args, Debug)]
pub struct MilestoneArgs {
    #[command(subcommand)]
    pub command: Option<MilestoneCommand>,
}

#[derive(Subcommand, Debug)]
pub enum MilestoneCommand {
    /// List milestones for a project
    List(milestone_list::ListArgs),
    /// View milestone details. By default lists the first 10 attached issues from the first page of 50; use --all to paginate the full set.
    #[command(alias = "v")]
    View(milestone_view::ViewArgs),
    /// Create a new project milestone
    Create(milestone_create::CreateArgs),
    /// Update an existing project milestone
    Update(milestone_update::UpdateArgs),
    /// Delete a project milestone
    Delete(milestone_delete::DeleteArgs),
}

pub fn run(args: MilestoneArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <MilestoneArgs as clap::Args>::augment_args(clap::Command::new("milestone"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        MilestoneCommand::List(args) => milestone_list::run(args)
            .map_err(|error| error.with_context("Failed to fetch milestones")),
        MilestoneCommand::View(args) => milestone_view::run(args)
            .map_err(|error| error.with_context("Failed to fetch milestone details")),
        MilestoneCommand::Create(args) => milestone_create::run(args)
            .map_err(|error| error.with_context("Failed to create milestone")),
        MilestoneCommand::Update(args) => milestone_update::run(args)
            .map_err(|error| error.with_context("Failed to update milestone")),
        MilestoneCommand::Delete(args) => milestone_delete::run(args)
            .map_err(|error| error.with_context("Failed to delete milestone")),
    }
}
