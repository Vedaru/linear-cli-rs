//! `linear notification` — what Linear told this user, and the read state that belongs to them.
//!
//! Not a port: the crate `linear-cli` has a notifications group and upstream `schpet/linear-cli`
//! has none. The group exists because notifications are the one place an agent cannot see what a
//! human is being told - a mention, a comment, an assignment - and read state is the only writable
//! part of it.
//!
//! The group has no action of its own: with no subcommand it prints help, like every other group
//! here. `list` reads; `read` and `archive` write, and both name the user whose state they change,
//! because Linear's read state is per user and a bare "marked read" leaves the reader guessing
//! whether it was theirs.

use clap::{Args, Subcommand};
use serde_json::{Map, Value};

use crate::errors::Result;
use crate::output;

mod notification_archive;
mod notification_list;
mod notification_read;

#[derive(Args, Debug)]
pub struct NotificationArgs {
    #[command(subcommand)]
    pub command: Option<NotificationCommand>,
}

#[derive(Subcommand, Debug)]
pub enum NotificationCommand {
    /// List notifications, newest first
    List(notification_list::NotificationListArgs),
    /// Mark one notification read
    Read(notification_read::NotificationReadArgs),
    /// Archive one notification
    Archive(notification_archive::NotificationArchiveArgs),
}

pub fn run(args: NotificationArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd =
            <NotificationArgs as clap::Args>::augment_args(clap::Command::new("notification"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        NotificationCommand::List(args) => notification_list::run(args)
            .map_err(|error| error.with_context("Failed to fetch notifications")),
        NotificationCommand::Read(args) => notification_read::run(args)
            .map_err(|error| error.with_context("Failed to mark notification read")),
        NotificationCommand::Archive(args) => notification_archive::run(args)
            .map_err(|error| error.with_context("Failed to archive notification")),
    }
}

/// Print a mutation's payload with the viewer folded into it.
///
/// The payload arrives as `{success, notification}` or `{success, entity}`, and the viewer is a
/// sibling in the same response. Flattening rather than nesting keeps the shape the API's own, so
/// `--json` here reads like the mutation's answer with one extra key, not like a wrapper someone
/// has to learn.
pub(crate) fn print_payload(payload: &Value, viewer: &Value) {
    let mut object = match payload {
        Value::Object(map) => map.clone(),
        other => {
            let mut map = Map::new();
            map.insert("payload".to_string(), other.clone());
            map
        }
    };
    object.insert("viewer".to_string(), viewer.clone());
    output::print_json(&Value::Object(object));
}

/// The name and email of the viewer, for a sentence that says whose state changed.
pub(crate) fn whose(viewer: &Value) -> String {
    let name = viewer.get("name").and_then(Value::as_str);
    let email = viewer.get("email").and_then(Value::as_str);
    match (name, email) {
        (Some(name), Some(email)) => format!("{name} <{email}>"),
        (Some(name), None) => name.to_string(),
        (None, Some(email)) => email.to_string(),
        (None, None) => "the token's user".to_string(),
    }
}

/// A one-line summary of what a notification is about, for the human table.
pub(crate) fn subject_of(notification: &Value) -> String {
    if let Some(issue) = notification.get("issue") {
        let identifier = issue
            .get("identifier")
            .and_then(Value::as_str)
            .unwrap_or("");
        let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
        if !identifier.is_empty() {
            return format!("{identifier} {title}").trim_end().to_string();
        }
    }
    notification
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)")
        .to_string()
}

/// A notification is unread when it has no `readAt`. `archivedAt` is a separate axis - an archived
/// notification can still be unread - so this deliberately does not look at it.
pub(crate) fn is_unread(notification: &Value) -> bool {
    notification
        .get("readAt")
        .map(Value::is_null)
        .unwrap_or(true)
}
