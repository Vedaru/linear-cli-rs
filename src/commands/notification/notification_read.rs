//! `linear notification read <id|VED-42>` — mark one notification read, for the token's user.
//!
//! Read state is per user, so the sentence names the user whose state changed: "marked read" alone
//! is exactly the claim a person cannot check. The viewer comes back in the same round trip as the
//! mutation (see `src/linear/notifications.rs`), so saying whose is free.
//!
//! The reference may be a notification id or the identifier of the issue the notification is about;
//! a reference matching more than one notification is refused rather than guessed at.

use clap::Args;

use crate::errors::Result;
use crate::{linear, output};

use super::{print_payload, subject_of, whose};

#[derive(Args, Debug)]
pub struct NotificationReadArgs {
    /// Notification id, or the identifier of the issue it is about (VED-42)
    #[arg(value_name = "ID|ISSUE")]
    pub reference: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: NotificationReadArgs) -> Result<()> {
    let notification = linear::resolve_notification(&args.reference)?;
    let id = notification
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();

    let (payload, viewer) = linear::mark_read(&id)?;

    if args.json {
        print_payload(&payload, &viewer);
        return Ok(());
    }

    output::line(&format!(
        "Marked read for {}: {} ({}).",
        whose(&viewer),
        subject_of(&notification),
        id
    ));
    Ok(())
}
