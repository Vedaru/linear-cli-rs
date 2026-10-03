//! `linear view delete` — delete a custom view.
//!
//! Confirmation follows `label delete`: `--force` skips the prompt, and a non-interactive run
//! without it fails with the flag to pass rather than blocking on a question nobody can answer.
//! The API's `customViewDelete` is a hard delete, so the confirmation names the view it is about
//! to remove rather than asking about "this".

use serde_json::Value;

use clap::Args;

use crate::errors::{CliError, Result};
use crate::{linear, output, prompt};

#[derive(Args, Debug)]
pub struct ViewDeleteArgs {
    /// View name or ID
    pub name_or_id: String,
    /// Skip confirmation prompt
    #[arg(short = 'f', long)]
    pub force: bool,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ViewDeleteArgs) -> Result<()> {
    let view = linear::resolve_view(&args.name_or_id)?;
    let name = view.get("name").and_then(Value::as_str).unwrap_or("");
    let id = view
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::cli("The view has no id"))?
        .to_string();

    if !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --force to skip confirmation."));
        }
        let confirmed = prompt::confirm(
            &format!("Are you sure you want to delete the view \"{name}\"?"),
            false,
        )?;
        if !confirmed {
            output::line("Deletion canceled");
            return Ok(());
        }
    }

    let result = linear::delete_view(&id)?;

    if args.json {
        output::print_json(&result);
        return Ok(());
    }

    output::line(&format!("✓ Deleted view: {name}"));
    Ok(())
}
