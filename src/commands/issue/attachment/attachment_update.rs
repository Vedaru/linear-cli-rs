//! `linear issue attachment update` — a title, a subtitle, or a new URL.
//!
//! Two schema facts shape this command. `AttachmentUpdateInput.title` is **required**, so an
//! invocation that only names a subtitle still has to send the title the attachment already has -
//! there is no partial update that leaves it alone. And the update input has no `url` field: an
//! attachment's URL is its identity within an issue, so `--url` re-links instead - create at the
//! new URL, then delete the old, in that order so a failed create leaves the link in place.

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct AttachmentUpdateArgs {
    /// Attachment id (see `linear issue attachment list <issue>`)
    #[arg(value_name = "attachmentId")]
    pub attachment_id: String,
    /// New title
    #[arg(short = 't', long, value_name = "title")]
    pub title: Option<String>,
    /// New subtitle (an empty string clears it)
    #[arg(short = 's', long, value_name = "subtitle")]
    pub subtitle: Option<String>,
    /// New URL. A URL is an attachment's identity, so this re-links: the new one is created
    /// first and the old one deleted only after that succeeds
    #[arg(short = 'u', long, value_name = "url")]
    pub url: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: AttachmentUpdateArgs) -> Result<()> {
    if args.title.is_none() && args.subtitle.is_none() && args.url.is_none() {
        return Err(CliError::validation("Nothing to update").suggestion(
            "Pass --title, --subtitle or --url; an attachment has no other writable field.",
        ));
    }

    let current = linear::get_attachment(&args.attachment_id)?;
    let current_title = field(&current, "title");
    let current_subtitle = field(&current, "subtitle");
    let current_url = field(&current, "url");

    if let Some(url) = args.url.clone() {
        if url != current_url {
            return relink(&args, &current, &current_title, &current_subtitle);
        }
    }

    // The API requires a title, so an update that does not name one sends the one it found.
    let mut input = Map::new();
    input.insert(
        "title".to_string(),
        json!(args.title.clone().unwrap_or_else(|| current_title.clone())),
    );
    let mut changed: Vec<String> = Vec::new();
    if let Some(title) = &args.title {
        if title != &current_title {
            changed.push(format!("title: \"{current_title}\" → \"{title}\""));
        }
    }
    if let Some(subtitle) = &args.subtitle {
        input.insert("subtitle".to_string(), json!(subtitle));
        if subtitle != &current_subtitle {
            changed.push(format!("subtitle: \"{current_subtitle}\" → \"{subtitle}\""));
        }
    }

    // An invocation whose fields all already hold those values is a no-op, and reporting success
    // for one would be a lie about what the API was asked to do.
    if changed.is_empty() {
        return Err(CliError::validation(format!(
            "Attachment {} already has every value given",
            args.attachment_id
        ))
        .suggestion("Change a value, or pass --json to read the attachment as it is."));
    }

    let document = linear::update_attachment(&args.attachment_id, Value::Object(input))?;
    let updated = linear::attachment_from(&document, "attachmentUpdate")?;

    if args.json {
        output::print_json(&document);
        return Ok(());
    }

    output::line(&format!("✓ Updated attachment {}", field(&updated, "id")));
    for change in &changed {
        output::line(&format!("  {change}"));
    }
    Ok(())
}

/// `--url` on an attachment whose URL changed: create the new link, then drop the old one.
fn relink(
    args: &AttachmentUpdateArgs,
    current: &Value,
    current_title: &str,
    current_subtitle: &str,
) -> Result<()> {
    let Some(new_url) = args.url.clone() else {
        return Err(CliError::cli("Re-link without a URL"));
    };
    // A URL cannot be carried over from the node being replaced, so a create with no issue to
    // hang it on would make the old link disappear and put nothing in its place.
    let issue_id = current
        .pointer("/issue/id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::cli("The attachment names no issue, so there is nothing to re-link it to")
        })?
        .to_string();

    let mut input = Map::new();
    input.insert("issueId".to_string(), json!(issue_id));
    input.insert("url".to_string(), json!(new_url));
    input.insert(
        "title".to_string(),
        json!(args
            .title
            .clone()
            .unwrap_or_else(|| current_title.to_string())),
    );
    let subtitle = args
        .subtitle
        .clone()
        .unwrap_or_else(|| current_subtitle.to_string());
    if !subtitle.is_empty() {
        input.insert("subtitle".to_string(), json!(subtitle));
    }

    let document = linear::create_attachment(Value::Object(input))?;
    let created = linear::attachment_from(&document, "attachmentCreate")?;
    let new_id = field(&created, "id");

    // Only now is the old link removed: the create is the step that can fail, and failing with
    // the old attachment still there is the recoverable half of this operation.
    linear::delete_attachment(&args.attachment_id)?;

    if args.json {
        output::print_json(&document);
        return Ok(());
    }

    let issue = current
        .pointer("/issue/identifier")
        .and_then(Value::as_str)
        .unwrap_or("");
    output::line(&format!("✓ Re-linked {issue}: {new_id}"));
    output::line(&format!(
        "  url: \"{}\" → \"{new_url}\"",
        field(current, "url")
    ));
    output::line(&format!("  Deleted attachment {}", args.attachment_id));
    Ok(())
}

fn field(node: &Value, name: &str) -> String {
    node.get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
