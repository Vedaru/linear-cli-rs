//! `linear issue attachment get` — one attachment, by its id.
//!
//! `attachment(id:)` is non-null in the schema, so an id that does not exist comes back as
//! Linear's own `Entity not found: Attachment` refusal; that error is passed through, because it
//! says more than a not-found this command could invent.

use clap::Args;
use serde_json::Value;

use crate::errors::Result;
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct AttachmentGetArgs {
    /// Attachment id (see `linear issue attachment list <issue>`)
    #[arg(value_name = "attachmentId")]
    pub attachment_id: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: AttachmentGetArgs) -> Result<()> {
    let attachment = linear::get_attachment(&args.attachment_id)?;

    if args.json {
        output::print_json(&attachment);
        return Ok(());
    }

    output::line(&field(&attachment, "title"));
    let subtitle = field(&attachment, "subtitle");
    if !subtitle.is_empty() {
        output::line(&subtitle);
    }
    output::line(&format!("  URL: {}", field(&attachment, "url")));
    let issue = attachment
        .pointer("/issue/identifier")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !issue.is_empty() {
        output::line(&format!("  Issue: {issue}"));
    }
    output::line(&format!("  Id: {}", field(&attachment, "id")));
    Ok(())
}

fn field(node: &Value, name: &str) -> String {
    node.get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
