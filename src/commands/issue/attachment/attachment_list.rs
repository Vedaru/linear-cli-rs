//! `linear issue attachment list` — the sidebar links on one issue.
//!
//! Nothing else in the CLI printed an attachment's id, and every write in this group addresses
//! one by id, so this is the command that makes the other three usable rather than a convenience.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::Result;
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct AttachmentListArgs {
    /// Issue ID (e.g., ENG-123) or URL
    #[arg(value_name = "issueId")]
    pub issue_id: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: AttachmentListArgs) -> Result<()> {
    let (identifier, nodes, page_info) = linear::list_issue_attachments(&args.issue_id)?;

    if args.json {
        output::print_json(&json!({ "nodes": nodes, "pageInfo": page_info }));
        return Ok(());
    }

    if nodes.is_empty() {
        output::line(&format!("No attachments on {identifier}."));
        return Ok(());
    }

    output::line(&format!("Attachments on {identifier}:"));
    for node in &nodes {
        output::line(&format!(
            "  {}  {}",
            field(node, "id"),
            field(node, "title")
        ));
        let subtitle = field(node, "subtitle");
        if !subtitle.is_empty() {
            output::line(&format!("      {subtitle}"));
        }
        output::line(&format!("      {}", field(node, "url")));
    }

    Ok(())
}

/// A string field, or the empty string: an attachment's subtitle and title are both nullable in
/// the schema, and a missing one is a blank line rather than a panic.
fn field(node: &Value, name: &str) -> String {
    node.get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
