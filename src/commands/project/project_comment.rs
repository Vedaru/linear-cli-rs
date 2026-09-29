//! `linear project comment` — port of `src/commands/project/project-comment.ts`.
//!
//! The group itself has no action; with no subcommand it prints help,
//! matching upstream's `this.showHelp()`.

use clap::{Args, Subcommand};

use crate::commands::project::project_comment_add::{self, ProjectCommentAddArgs};
use crate::commands::project::project_comment_list::{self, ProjectCommentListArgs};
use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct ProjectCommentArgs {
    #[command(subcommand)]
    pub command: Option<ProjectCommentCommand>,
}

#[derive(Subcommand, Debug)]
pub enum ProjectCommentCommand {
    /// Add a comment or reply to a project
    Add(ProjectCommentAddArgs),
    /// List comments on a project
    List(ProjectCommentListArgs),
}

pub fn run(args: ProjectCommentArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd =
            <ProjectCommentArgs as clap::Args>::augment_args(clap::Command::new("comment"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        ProjectCommentCommand::Add(a) => {
            project_comment_add::run(a).map_err(|error| error.with_context("Failed to add comment"))
        }
        ProjectCommentCommand::List(a) => project_comment_list::run(a)
            .map_err(|error| error.with_context("Failed to list comments")),
    }
}
