//! `linear cycle archive` — the API's `cycleArchive`.
//!
//! There is no `cycleUnarchive` in the API and none here: Linear archives a cycle
//! when the next one starts and never restores one, so this is irreversible and
//! asks for confirmation unless `--confirm` is passed.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, output, prompt};

use super::{cycle_label, resolve_cycle_id};

const ARCHIVE_CYCLE_MUTATION: &str = r#"
mutation ArchiveCycle($id: String!) {
  cycleArchive(id: $id) {
    success
    entity {
      id
      number
      name
    }
  }
}
"#;

/// Archive a cycle
#[derive(Args, Debug)]
pub struct CycleArchiveArgs {
    /// Cycle UUID, or a cycle number or name (which needs a team)
    #[arg(value_name = "cycleRef")]
    pub cycle_ref: String,
    /// Team key, name, or ID (required for a cycle number or name)
    #[arg(long, value_name = "team")]
    pub team: Option<String>,
    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    pub confirm: bool,
}

pub fn run(args: CycleArchiveArgs) -> Result<()> {
    let cycle_id = resolve_cycle_id(&args.cycle_ref, args.team.as_deref())?;

    if !args.confirm {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --confirm to skip."));
        }
        let confirmed = prompt::confirm(
            &format!(
                "Are you sure you want to archive cycle \"{}\"?",
                args.cycle_ref
            ),
            false,
        )?;
        if !confirmed {
            output::line("Archive cancelled.");
            return Ok(());
        }
    }

    let client = graphql::client()?;
    let result = client.request(ARCHIVE_CYCLE_MUTATION, json!({ "id": cycle_id }))?;
    let archived = result.get("cycleArchive").cloned().unwrap_or(Value::Null);
    if !archived
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CliError::cli("Failed to archive cycle"));
    }

    let cycle = archived.get("entity").cloned().unwrap_or(Value::Null);
    output::line(&format!(
        "✓ Archived cycle: {}",
        cycle_label(&cycle, &args.cycle_ref)
    ));
    Ok(())
}
