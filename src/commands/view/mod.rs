//! `linear view` — custom views: Linear's saved filters.
//!
//! The group has no action of its own: with no subcommand it prints help, like every other
//! group here. *Applying* a view is deliberately not a member - `issue query --view <name|id>`
//! is where a saved filter belongs, because that command already builds the `issues(filter:)`
//! document a view's `filterData` drops into unchanged.

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

mod view_create;
mod view_delete;
mod view_list;
mod view_update;
mod view_view;

#[derive(Args, Debug)]
pub struct ViewArgs {
    #[command(subcommand)]
    pub command: Option<ViewCommand>,
}

#[derive(Subcommand, Debug)]
pub enum ViewCommand {
    /// List custom views
    List(view_list::ViewListArgs),
    /// Show one custom view, including the filter it saves
    View(view_view::ViewViewArgs),
    /// Create a custom view from a filter
    Create(view_create::ViewCreateArgs),
    /// Rename, redescribe, refilter or share an existing custom view
    Update(view_update::ViewUpdateArgs),
    /// Delete a custom view
    Delete(view_delete::ViewDeleteArgs),
}

pub fn run(args: ViewArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <ViewArgs as clap::Args>::augment_args(clap::Command::new("view"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        ViewCommand::List(args) => {
            view_list::run(args).map_err(|error| error.with_context("Failed to fetch views"))
        }
        ViewCommand::View(args) => {
            view_view::run(args).map_err(|error| error.with_context("Failed to fetch view"))
        }
        ViewCommand::Create(args) => {
            view_create::run(args).map_err(|error| error.with_context("Failed to create view"))
        }
        ViewCommand::Update(args) => {
            view_update::run(args).map_err(|error| error.with_context("Failed to update view"))
        }
        ViewCommand::Delete(args) => {
            view_delete::run(args).map_err(|error| error.with_context("Failed to delete view"))
        }
    }
}
