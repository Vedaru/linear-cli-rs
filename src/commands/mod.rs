//! Command dispatch. Each subcommand group mirrors one upstream `src/commands`
//! directory and owns its own argument parsing and error context.

pub mod api;
pub mod auth;
pub mod completions;
pub mod config;
pub mod cycle;
pub mod document;
pub mod initiative;
pub mod initiative_update;
pub mod issue;
pub mod label;
pub mod markdown;
pub mod milestone;
pub mod project;
pub mod project_update;
pub mod roadmap;
pub mod schema;
#[cfg(feature = "service")]
pub mod sync;
pub mod team;
pub mod template;
pub mod user;
pub mod view;
#[cfg(feature = "service")]
pub mod webhook;

use crate::cli::Command;
use crate::errors::Result;
use crate::output;

/// Run the parsed command tree. Returns an error already wrapped with the
/// context the user should see (e.g. "Failed to login"); `main` prints it.
pub fn run(command: Option<Command>) -> Result<()> {
    match command {
        None => {
            output::line("Use --help to see available commands");
            Ok(())
        }
        Some(Command::Auth(args)) => auth::run(args),
        Some(Command::Issue(args)) => issue::run(args),
        Some(Command::Project(args)) => project::run(args),
        Some(Command::ProjectUpdate(args)) => project_update::run(args),
        Some(Command::Roadmap(args)) => roadmap::run(args),
        Some(Command::Team(args)) => team::run(args),
        Some(Command::User(args)) => user::run(args),
        Some(Command::Cycle(args)) => cycle::run(args),
        Some(Command::Milestone(args)) => milestone::run(args),
        Some(Command::Initiative(args)) => initiative::run(args),
        Some(Command::InitiativeUpdate(args)) => initiative_update::run(args),
        Some(Command::Label(args)) => label::run(args),
        Some(Command::Template(args)) => template::run(args),
        Some(Command::Document(args)) => document::run(args),
        Some(Command::View(args)) => view::run(args),
        Some(Command::Config(args)) => config::run(args),
        Some(Command::Schema(args)) => schema::run(args),
        Some(Command::Api(args)) => api::run(args),
        Some(Command::Markdown(args)) => markdown::run(args),
        Some(Command::Completions(args)) => completions::run(args),
        #[cfg(feature = "service")]
        Some(Command::Webhook(args)) => webhook::run(args),
        #[cfg(feature = "service")]
        Some(Command::Sync(args)) => sync::run(args),
    }
}
