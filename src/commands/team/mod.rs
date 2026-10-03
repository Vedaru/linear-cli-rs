//! `linear team` — port of `src/commands/team/`.
//!
//! Covers create, delete, list, id, autolinks, members, and states. Each
//! action wraps its failure with the same context string upstream passes to
//! `handleError`, so error output matches the TypeScript CLI.
//!
//! Interactive prompts are guarded by [`crate::prompt::is_interactive`] so a
//! headless run never blocks waiting for input: the flag-based path is used
//! where it exists, and a validation error with upstream's own wording is
//! returned where upstream itself refuses to prompt non-interactively.

pub mod team_autolinks;
pub mod team_create;
pub mod team_delete;
pub mod team_id;
pub mod team_list;
pub mod team_members;
pub mod team_states;

use crate::errors::Result;
use crate::output;

/// `linear team` — manage Linear teams.
#[derive(clap::Args, Debug)]
pub struct TeamArgs {
    #[command(subcommand)]
    pub command: Option<TeamCommand>,
}

#[derive(clap::Subcommand, Debug)]
pub enum TeamCommand {
    /// Create a linear team
    Create(team_create::CreateArgs),
    /// Delete a Linear team
    Delete(team_delete::DeleteArgs),
    /// List teams
    List(team_list::ListArgs),
    /// Print the configured team id
    Id {
        /// Output the team key as JSON (an addition to upstream)
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Configure GitHub repository autolinks for Linear issues with this team prefix
    Autolinks,
    /// List team members (team by key, name, or ID)
    Members(team_members::MembersArgs),
    /// List workflow states for a team (by key, name, or ID)
    States(team_states::StatesArgs),
}

/// Run the parsed `team` command tree.
pub fn run(args: TeamArgs) -> Result<()> {
    let Some(command) = args.command else {
        // `linear team` with no subcommand shows help, mirroring
        // `this.showHelp()` upstream.
        let mut cmd = <TeamArgs as clap::Args>::augment_args(clap::Command::new("team"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        TeamCommand::Create(args) => {
            team_create::run(args).map_err(|error| error.with_context("Failed to create team"))
        }
        TeamCommand::Delete(args) => {
            team_delete::run(args).map_err(|error| error.with_context("Failed to delete team"))
        }
        TeamCommand::List(args) => {
            team_list::run(args).map_err(|error| error.with_context("Failed to fetch teams"))
        }
        TeamCommand::Id { json } => {
            team_id::run(json).map_err(|error| error.with_context("Failed to get team id"))
        }
        TeamCommand::Autolinks => team_autolinks::run()
            .map_err(|error| error.with_context("Failed to configure autolinks")),
        TeamCommand::Members(args) => team_members::run(args)
            .map_err(|error| error.with_context("Failed to fetch team members")),
        TeamCommand::States(args) => team_states::run(args)
            .map_err(|error| error.with_context("Failed to fetch workflow states")),
    }
}
