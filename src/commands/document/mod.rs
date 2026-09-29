//! `linear document` — port of `src/commands/document/document.ts` and its
//! subcommands.
//!
//! With no subcommand the group prints its own help (`augment_args` +
//! `print_help`), mirroring upstream's `Use --help to see available
//! subcommands` action.
//!
//! Error context lives here, with one exception: [`document_view::run`]
//! reports a missing document against the *raw* id, so it self-wraps with
//! `Failed to view document` and must not be wrapped again.

mod attachment_target;
mod document_comment;
mod document_comment_add;
mod document_comment_list;
mod document_create;
mod document_delete;
mod document_list;
mod document_update;
mod document_view;

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct DocumentArgs {
    #[command(subcommand)]
    pub command: Option<DocumentCommand>,
}

#[derive(Subcommand, Debug)]
pub enum DocumentCommand {
    /// List documents
    List(document_list::DocumentListArgs),
    /// View a document
    View(document_view::DocumentViewArgs),
    /// Create a document
    Create(document_create::DocumentCreateArgs),
    /// Update a document
    Update(document_update::DocumentUpdateArgs),
    /// Delete a document
    Delete(document_delete::DocumentDeleteArgs),
    /// Manage document comments
    Comment(document_comment::DocumentCommentArgs),
}

pub fn run(args: DocumentArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <DocumentArgs as clap::Args>::augment_args(clap::Command::new("document"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        DocumentCommand::List(a) => {
            document_list::run(a).map_err(|error| error.with_context("Failed to fetch documents"))
        }
        // Self-wraps with `Failed to view document`; do not double-wrap.
        DocumentCommand::View(a) => document_view::run(a),
        DocumentCommand::Create(a) => document_create::run(a)
            .map_err(|error| error.with_context("Failed to create document")),
        DocumentCommand::Update(a) => document_update::run(a)
            .map_err(|error| error.with_context("Failed to update document")),
        DocumentCommand::Delete(a) => document_delete::run(a)
            .map_err(|error| error.with_context("Failed to delete document")),
        // The comment subgroup supplies its own per-subcommand context.
        DocumentCommand::Comment(a) => document_comment::run(a),
    }
}
