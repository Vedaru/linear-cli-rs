//! `linear webhook` — run and inspect the bridge service.
//!
//! An addition beyond upstream: upstream's CLI is one-shot, so there is nothing
//! to port here. This group is the service half of the same binary, and it is why
//! the bridge lives in a library crate rather than inside a command module - the
//! reconcile loop must be usable without argv.

pub mod replay;
pub mod serve;

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct WebhookArgs {
    #[command(subcommand)]
    pub command: Option<WebhookCommand>,
}

#[derive(Subcommand, Debug)]
pub enum WebhookCommand {
    /// Accept webhook deliveries from the configured platforms
    Serve(serve::ServeArgs),
    /// Run a stored delivery again, against the body the provider sent
    Replay(replay::ReplayArgs),
}

pub fn run(args: WebhookArgs) -> Result<()> {
    match args.command {
        Some(WebhookCommand::Serve(args)) => serve::run(args),
        Some(WebhookCommand::Replay(args)) => replay::run(args),
        None => {
            output::line("Use `linear webhook serve --help` for the available options");
            Ok(())
        }
    }
}
