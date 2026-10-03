//! `linear project` — port of `src/commands/project/project.ts` and its
//! subcommands.
//!
//! With no subcommand the group prints its own help (`augment_args` +
//! `print_help`), mirroring upstream's `this.showHelp()` action.
//!
//! Error context lives here, with two exceptions. [`project_list::run`] has
//! two contexts of its own (`Failed to fetch projects` for the listing,
//! `Failed to open projects` for `--web`/`--app`), and [`project_view::run`]
//! reports a missing project against the *raw* id and self-wraps with
//! `Failed to view project`. Neither must be wrapped again.

mod project_archive;
mod project_comment;
mod project_comment_add;
mod project_comment_list;
mod project_create;
mod project_delete;
mod project_description;
mod project_label;
mod project_list;
mod project_member;
mod project_members;
mod project_unarchive;
mod project_update;
mod project_view;

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: Option<ProjectCommand>,
}

#[derive(Subcommand, Debug)]
pub enum ProjectCommand {
    /// List projects
    List(project_list::ProjectListArgs),
    /// View a project
    View(project_view::ProjectViewArgs),
    /// Create a project
    Create(project_create::ProjectCreateArgs),
    /// Update a project
    Update(project_update::ProjectUpdateArgs),
    /// Delete a project (the API's projectDelete, which trashes: `unarchive` restores it)
    Delete(project_delete::ProjectDeleteArgs),
    /// Archive a project (reversible; `--trash` uses the successor's behaviour)
    Archive(project_archive::ProjectArchiveArgs),
    /// Restore an archived or trashed project
    Unarchive(project_unarchive::ProjectUnarchiveArgs),
    /// List a project's members
    Members(project_members::ProjectMembersArgs),
    /// Add or remove a project's members (incremental: the others are kept)
    Member(project_member::ProjectMemberArgs),
    /// List, add, remove or replace a project's labels
    Label(project_label::ProjectLabelArgs),
    /// Manage project comments
    Comment(project_comment::ProjectCommentArgs),
}

pub fn run(args: ProjectArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <ProjectArgs as clap::Args>::augment_args(clap::Command::new("project"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        // Self-wraps with `Failed to fetch projects` / `Failed to open projects`.
        ProjectCommand::List(a) => project_list::run(a),
        // Self-wraps with `Failed to view project`; do not double-wrap.
        ProjectCommand::View(a) => project_view::run(a),
        ProjectCommand::Create(a) => {
            project_create::run(a).map_err(|error| error.with_context("Failed to create project"))
        }
        ProjectCommand::Update(a) => {
            project_update::run(a).map_err(|error| error.with_context("Failed to update project"))
        }
        ProjectCommand::Delete(a) => {
            project_delete::run(a).map_err(|error| error.with_context("Failed to delete project"))
        }
        ProjectCommand::Archive(a) => {
            project_archive::run(a).map_err(|error| error.with_context("Failed to archive project"))
        }
        ProjectCommand::Unarchive(a) => project_unarchive::run(a)
            .map_err(|error| error.with_context("Failed to unarchive project")),
        ProjectCommand::Members(a) => project_members::run(a)
            .map_err(|error| error.with_context("Failed to fetch project members")),
        // The member and label groups supply their own per-subcommand context.
        ProjectCommand::Member(a) => project_member::run(a),
        ProjectCommand::Label(a) => project_label::run(a),
        // The comment subgroup supplies its own per-subcommand context.
        ProjectCommand::Comment(a) => project_comment::run(a),
    }
}
