//! `linear issue attachment delete` — remove a sidebar link.
//!
//! Confirmation is only offered on a real terminal; a non-interactive caller must pass `--force`,
//! the same contract `label delete` and `project delete` keep. There is nothing to undo with: the
//! schema carries an `attachmentDelete` and no `attachmentUnarchive`, so this one is permanent
//! and says so.

use clap::Args;

use crate::errors::{CliError, Result};
use crate::{linear, output, prompt};

#[derive(Args, Debug)]
pub struct AttachmentDeleteArgs {
    /// Attachment id (see `linear issue attachment list <issue>`)
    #[arg(value_name = "attachmentId")]
    pub attachment_id: String,
    /// Skip confirmation prompt
    #[arg(short = 'f', long)]
    pub force: bool,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: AttachmentDeleteArgs) -> Result<()> {
    // Read it first for two reasons: the confirmation should name what is about to go, and an id
    // that does not exist should fail before a prompt rather than after one.
    let attachment = linear::get_attachment(&args.attachment_id)?;
    let title = attachment
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let url = attachment
        .get("url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    if !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --force to skip confirmation."));
        }
        let confirmed = prompt::confirm(&format!("Delete attachment \"{title}\" ({url})?"), false)?;
        if !confirmed {
            output::line("Deletion canceled");
            return Ok(());
        }
    }

    let payload = linear::delete_attachment(&args.attachment_id)?;

    if args.json {
        output::print_json(&payload);
        return Ok(());
    }

    output::line(&format!("✓ Deleted attachment {}", args.attachment_id));
    Ok(())
}
