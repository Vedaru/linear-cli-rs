//! `linear issue attachment` — read back, correct and remove a sidebar link.
//!
//! `issue attach` and `issue link` create attachments, and that used to be the whole surface: a
//! title typo, a subtitle-only integration link, or a URL that moved could only be fixed in the
//! app. The API's `attachment(id:)`, `attachmentUpdate` and `attachmentDelete` are the other
//! three quarters. `list` is here because an attachment is addressed by an id that nothing else
//! in the CLI printed.
//!
//! An attachment's **URL is its identity** within an issue (`attachmentCreate` "creates a new
//! attachment, or updates existing if the same `url` and `issueId` is used"), and the update
//! input has no `url` field at all - so `update --url` is a re-link: create at the new URL first,
//! then delete the old one, in that order, so a failed create never leaves the link gone.

pub mod attachment_delete;
pub mod attachment_get;
pub mod attachment_list;
pub mod attachment_update;

use clap::{Args, Subcommand};

use crate::errors::Result;
use crate::output;

/// `linear issue attachment` — add-on to the create-only `attach`/`link` pair.
#[derive(Args, Debug)]
pub struct AttachmentArgs {
    #[command(subcommand)]
    pub command: Option<AttachmentCommand>,
}

#[derive(Subcommand, Debug)]
pub enum AttachmentCommand {
    /// List the sidebar links on an issue
    List(attachment_list::AttachmentListArgs),
    /// Show one attachment by its id
    Get(attachment_get::AttachmentGetArgs),
    /// Change an attachment's title, subtitle, or URL
    Update(attachment_update::AttachmentUpdateArgs),
    /// Delete an attachment (permanent: the API has no un-delete for one)
    Delete(attachment_delete::AttachmentDeleteArgs),
}

pub fn run(args: AttachmentArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd =
            <AttachmentArgs as clap::Args>::augment_args(clap::Command::new("attachment"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        AttachmentCommand::List(args) => attachment_list::run(args)
            .map_err(|error| error.with_context("Failed to list attachments")),
        AttachmentCommand::Get(args) => attachment_get::run(args)
            .map_err(|error| error.with_context("Failed to fetch attachment")),
        AttachmentCommand::Update(args) => attachment_update::run(args)
            .map_err(|error| error.with_context("Failed to update attachment")),
        AttachmentCommand::Delete(args) => attachment_delete::run(args)
            .map_err(|error| error.with_context("Failed to delete attachment")),
    }
}
