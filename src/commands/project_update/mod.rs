//! `linear project-update` — port of
//! `src/commands/project-update/project-update.ts` and its subcommands.
//!
//! The group itself has no action; with no subcommand it prints help,
//! matching upstream's `this.showHelp()`.
//!
//! Error context lives here: upstream wraps both actions in
//! `handleError(error, "Failed to <action>")`, so the subcommands return bare
//! errors and this module supplies the prefix.

mod project_update_create;
mod project_update_list;

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct ProjectUpdateArgs {
    #[command(subcommand)]
    pub command: Option<ProjectUpdateCommand>,
}

#[derive(Subcommand, Debug)]
pub enum ProjectUpdateCommand {
    /// Create a new status update for a project
    #[command(alias = "c")]
    Create(project_update_create::ProjectUpdateCreateArgs),
    /// List status updates for a project
    #[command(alias = "l")]
    List(project_update_list::ProjectUpdateListArgs),
}

pub fn run(args: ProjectUpdateArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd =
            <ProjectUpdateArgs as clap::Args>::augment_args(clap::Command::new("project-update"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        ProjectUpdateCommand::Create(a) => project_update_create::run(a)
            .map_err(|error| error.with_context("Failed to create project update")),
        ProjectUpdateCommand::List(a) => project_update_list::run(a)
            .map_err(|error| error.with_context("Failed to fetch project updates")),
    }
}
