//! `linear user` — manage Linear users. Port of `src/commands/user/user.ts`.
//!
//! The group has no action of its own: with no subcommand it prints help,
//! matching upstream's `this.showHelp()`.

pub mod user_list;

use crate::errors::Result;
use crate::output;

#[derive(clap::Args, Debug)]
pub struct UserArgs {
    #[command(subcommand)]
    pub command: Option<UserCommand>,
}

#[derive(clap::Subcommand, Debug)]
pub enum UserCommand {
    /// List members of the workspace
    List(user_list::UserListArgs),
}

pub fn run(args: UserArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <UserArgs as clap::Args>::augment_args(clap::Command::new("user"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        UserCommand::List(a) => user_list::run(a)
            .map_err(|error| error.with_context("Failed to fetch workspace members")),
    }
}
