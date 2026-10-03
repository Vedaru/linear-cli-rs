//! `linear project unarchive` — the way back from `project archive` *and* from `project delete`.
//!
//! The API's `projectUnarchive` restores a project that was archived or trashed, so this is the
//! command that makes `project delete` reversible - and it is why `archive` asks for no
//! confirmation: the pair is a round trip, not a decision.

use clap::Args;
use serde_json::Value;

use crate::errors::Result;
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct ProjectUnarchiveArgs {
    /// Project ID, slug, or name
    #[arg(value_name = "projectId")]
    pub project_id: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ProjectUnarchiveArgs) -> Result<()> {
    let project_id = linear::resolve_project_id(&args.project_id)?;
    let document = linear::unarchive_project(&project_id)?;

    if args.json {
        output::print_json(&document);
        return Ok(());
    }

    let name = document
        .pointer("/projectUnarchive/entity/name")
        .and_then(Value::as_str)
        .unwrap_or(&args.project_id);
    output::line(&format!("✓ Unarchived project: {name}"));
    Ok(())
}
