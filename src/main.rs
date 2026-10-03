//! `linear` — a Rust port of `schpet-linear-cli`, built for headless agent use.
//!
//! Startup order mirrors upstream: load config (which also reads `.env`),
//! decide color from the environment, record the global `--workspace` flag,
//! then dispatch. Any error is printed by [`errors::handle_error`], which puts
//! it on stderr with an `✗` prefix and exits non-zero.
//!
//! One thing happens before all of that, because nothing else can be trusted
//! until it does: [`restore_default_sigpipe`].

// The port lands one command group at a time, so helpers for not-yet-wired
// commands are deliberately present ahead of their callers. Remove this once
// every command port is complete (verification pass, todo #9).
#![allow(dead_code)]

mod actions;
mod atomic;
mod bulk;
mod cli;
mod colors;
mod commands;
mod comments;
mod config;
mod consts;
mod credentials;
mod csv;
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
mod net;
mod output;
mod pager;
mod paths;
mod proc;
mod prompt;
mod prosemirror;
mod transfer;
mod upload;
mod vcs;

use clap::Parser;

/// Rust's runtime ignores `SIGPIPE` at startup, so a write to a closed pipe
/// returns `EPIPE` instead of ending the process. Every write site then has to
/// decide what a closed reader means, and the answers drift apart: `output.rs`
/// discards the error, the ~60 bare `println!` calls elsewhere panic, and
/// `clap_complete`'s generators `.expect("failed to write completion file")` -
/// which is how `linear completions bash | head` came to exit 101 with a panic.
///
/// Restoring the default disposition fixes the whole class at the one place it
/// can be fixed once. The kernel ends the process on the first write after the
/// reader goes away, before any of those per-site decisions run, which is what
/// every other Unix tool does (`ls | head`, `git log | head`) and what the shell
/// expects from a pipeline: no panic, no spurious error, no stderr noise.
#[cfg(unix)]
fn restore_default_sigpipe() {
    // `signal(2)` and two constants are all this needs, so it is declared here
    // rather than adding `libc` as a direct dependency for three symbols.
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }

    // From `signal.h`; both are portable across the libcs this builds against.
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;

    // SAFETY: `signal` is async-signal-safe, is called before any other thread
    // exists, and `SIG_DFL` is a valid disposition for `SIGPIPE`. The previous
    // handler is deliberately discarded - Rust installed it, nothing else wants
    // it back.
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

/// Windows has no `SIGPIPE`; a closed pipe is an ordinary write error there.
#[cfg(not(unix))]
fn restore_default_sigpipe() {}

fn main() {
    restore_default_sigpipe();
    colors::init();
    config::init();

    let cli = cli::Cli::parse();
    config::set_cli_workspace(cli.workspace.clone());

    if let Err(error) = commands::run(cli.command) {
        errors::handle_error(&error, None);
    }
}
