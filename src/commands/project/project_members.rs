//! `linear project members` — who is on a project.
//!
//! The write half is `project member add|remove`, which edits the set incrementally; this command
//! is the read that makes those ids and names visible in the first place.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::Result;
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct ProjectMembersArgs {
    /// Project ID, slug, or name
    #[arg(value_name = "projectId")]
    pub project_id: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ProjectMembersArgs) -> Result<()> {
    let project_id = linear::resolve_project_id(&args.project_id)?;
    let (members, page_info) = linear::get_project_members(&project_id)?;

    if args.json {
        output::print_json(&json!({ "nodes": members, "pageInfo": page_info }));
        return Ok(());
    }

    if members.is_empty() {
        output::line("No members on this project.");
        return Ok(());
    }

    output::line(&format!("Members ({}):", members.len()));
    for member in &members {
        output::line(&format!(
            "  {}  {}",
            field(member, "name"),
            field(member, "email")
        ));
    }
    Ok(())
}

fn field(node: &Value, name: &str) -> String {
    node.get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
