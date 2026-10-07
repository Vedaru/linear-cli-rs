//! `linear project archive` — retire a project without destroying it, and without the word
//! "delete" anywhere near the command.
//!
//! Both retirements are reversible: `projectArchive` (what this runs) archives, and
//! `projectDelete` - what `project delete` runs - *trashes* rather than destroys, which the API's
//! own description says outright ("Deletes (trashes) a project. The project can be restored later
//! with projectUnarchive."). `project unarchive` is the way back from either, which is why this
//! command asks for no confirmation: nothing it does needs one.
//!
//! `--trash` is for a caller that wants the successor's behaviour by name; `projectDelete` is
//! marked as the replacement for `projectArchive` in the schema, but only this call archives
//! without trashing.

use clap::Args;
use serde_json::Value;

use crate::errors::Result;
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct ProjectArchiveArgs {
    /// Project ID, slug, or name
    #[arg(value_name = "projectId")]
    pub project_id: String,
    /// Trash the project instead of archiving it (both are reversible)
    #[arg(long)]
    pub trash: bool,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ProjectArchiveArgs) -> Result<()> {
    let project_id = linear::resolve_project_id(&args.project_id)?;
    let document = linear::archive_project(&project_id, args.trash)?;

    if args.json {
        output::print_json(&document);
        return Ok(());
    }

    let name = document
        .pointer("/projectArchive/entity/name")
        .and_then(Value::as_str)
        .unwrap_or(&args.project_id);
    let verb = if args.trash { "Trashed" } else { "Archived" };
    output::line(&format!("✓ {verb} project: {name}"));
    // The UUID, not the input: slug and name only resolve while the project is unarchived
    // (`projects(filter:)` omits `includeArchived`), so a hint echoing a slug would describe a
    // command that cannot work. See VED-483.
    output::line(&format!(
        "  Restore it with `linear project unarchive {project_id}`"
    ));
    Ok(())
}
