//! `linear` — a Rust port of `schpet-linear-cli`, built for headless agent use.
//!
//! Startup order mirrors upstream: load config (which also reads `.env`),
//! decide color from the environment, record the global `--workspace` flag,
//! then dispatch. Any error is printed by [`errors::handle_error`], which puts
//! it on stderr with an `✗` prefix and exits non-zero.

// The port lands one command group at a time, so helpers for not-yet-wired
// commands are deliberately present ahead of their callers. Remove this once
// every command port is complete (verification pass, todo #9).
#![allow(dead_code)]

mod actions;
mod cli;
mod colors;
mod commands;
mod comments;
mod config;
mod consts;
mod credentials;
mod display;
mod editor;
mod errors;
mod fsutil;
mod git;
mod graphql;
mod hyperlink;
mod issue_identifier;
mod issue_table;
mod jj;
mod keyring;
mod linear;
mod linear_url;
mod markdown;
mod output;
mod pager;
mod paths;
mod proc;
mod prompt;
mod prosemirror;
mod upload;
mod vcs;

use clap::Parser;

fn main() {
    colors::init();
    config::init();

    let cli = cli::Cli::parse();
    config::set_cli_workspace(cli.workspace.clone());

    if let Err(error) = commands::run(cli.command) {
        errors::handle_error(&error, None);
    }
}
