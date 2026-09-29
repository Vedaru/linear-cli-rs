//! `linear document comment` — port of `src/commands/document/document-comment.ts`.
//!
//! The group itself has no action; with no subcommand it prints help,
//! matching upstream's `this.showHelp()`.

use clap::{Args, Subcommand};

use crate::commands::document::document_comment_add::{self, DocumentCommentAddArgs};
use crate::commands::document::document_comment_list::{self, DocumentCommentListArgs};
use crate::errors::Result;
use crate::output;

#[derive(Args, Debug)]
pub struct DocumentCommentArgs {
    #[command(subcommand)]
    pub command: Option<DocumentCommentCommand>,
}

#[derive(Subcommand, Debug)]
pub enum DocumentCommentCommand {
    /// Add a comment or reply to a document
    Add(DocumentCommentAddArgs),
    /// List comments on a document
    List(DocumentCommentListArgs),
}

pub fn run(args: DocumentCommentArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd =
            <DocumentCommentArgs as clap::Args>::augment_args(clap::Command::new("comment"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        DocumentCommentCommand::Add(a) => document_comment_add::run(a)
            .map_err(|error| error.with_context("Failed to add comment")),
        DocumentCommentCommand::List(a) => document_comment_list::run(a)
            .map_err(|error| error.with_context("Failed to list comments")),
    }
}
