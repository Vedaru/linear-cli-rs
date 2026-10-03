//! `linear template show` — what a template pre-fills, local or workspace.
//!
//! `template view` already renders a workspace template; this is the same question asked of the
//! **unified** lookup, which is what makes a name mean one thing across the group. When both kinds
//! carry the name, the local one wins - and the workspace template it is shadowing is named rather
//! than silently losing, because "which of my two `bug` templates just ran" is the question a
//! shadowed name creates.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::Result;
use crate::output;

use super::{local, template_name};

#[derive(Args, Debug)]
pub struct TemplateShowArgs {
    /// Template name or ID
    pub template: String,
    /// Read the workspace template of this name instead of the local one
    #[arg(long)]
    pub workspace: bool,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: TemplateShowArgs) -> Result<()> {
    let local_template = if args.workspace {
        None
    } else {
        local::find(&args.template)?
    };

    // The workspace lookup is skipped when the local file is what was asked for and no name is
    // shadowed - there is nothing to report and it costs a request.
    // Best-effort on purpose: reading a *local* template must not need Linear at all, and the
    // shadow note is a convenience. A failed lookup is silence here, not a failed command.
    let workspace = if local_template.is_some() && !args.workspace {
        super::find_workspace_by_name(&args.template)
            .ok()
            .flatten()
            .map(|template| template_name(&template))
    } else {
        None
    };

    if let Some(local_template) = &local_template {
        if args.json {
            output::print_json(&json!({
                "kind": "local",
                "name": local_template.name,
                "fields": Value::Object(local_template.fields.clone()),
                "shadowedWorkspaceTemplate": workspace,
            }));
            return Ok(());
        }

        output::line(&format!("{} (local template)", local_template.name));
        for (key, value) in &local_template.fields {
            output::line(&format!("  {key}: {}", render(value)));
        }
        if let Some(workspace) = &workspace {
            output::line(&format!(
                "  note: a workspace template named \"{workspace}\" also exists and is shadowed by this file"
            ));
        }
        return Ok(());
    }

    // Not local (or `--workspace`): Linear's own template.
    let template = super::resolve_template(&args.template)?;
    if args.json {
        output::print_json(&json!({
            "kind": "workspace",
            "name": template_name(&template),
            "template": template,
        }));
        return Ok(());
    }

    output::line(&format!(
        "{} (workspace template)",
        template_name(&template)
    ));
    output::line(&format!("  id: {}", super::template_id(&template)));
    if let Some(description) = template.get("description").and_then(Value::as_str) {
        if !description.is_empty() {
            output::line(&format!("  description: {description}"));
        }
    }
    output::line("  Run `linear template view <name>` for what it pre-fills.");
    Ok(())
}

/// A field as one line: strings as they are, everything else as compact JSON.
fn render(value: &Value) -> String {
    match value {
        Value::String(text) => text.replace('\n', "\\n"),
        other => other.to_string(),
    }
}
