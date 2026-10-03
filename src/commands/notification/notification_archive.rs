//! `linear notification archive <id|VED-42>` — archive one notification for the token's user.
//!
//! Archiving is per user too, and it is the same axis as read: an archived notification can still be
//! unread, which is why `list --include-archived` shows the `READ` column rather than assuming
//! archived means read.

use clap::Args;

use crate::errors::Result;
use crate::{linear, output};

use super::{print_payload, subject_of, whose};

#[derive(Args, Debug)]
pub struct NotificationArchiveArgs {
    /// Notification id, or the identifier of the issue it is about (VED-42)
    #[arg(value_name = "ID|ISSUE")]
    pub reference: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: NotificationArchiveArgs) -> Result<()> {
    let notification = linear::resolve_notification(&args.reference)?;
    let id = notification
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();

    let (payload, viewer) = linear::archive(&id)?;

    if args.json {
        print_payload(&payload, &viewer);
        return Ok(());
    }

    output::line(&format!(
        "Archived for {}: {} ({}).",
        whose(&viewer),
        subject_of(&notification),
        id
    ));
    Ok(())
}
