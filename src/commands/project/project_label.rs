//! `linear project label list|add|remove|set` — the labels on a project.
//!
//! `ProjectUpdateInput.labelIds` is the **whole** set, so `add` and `remove` read what the project
//! has first and send that set back with one name added or dropped; a script cannot lose a
//! project's other labels by naming one. `set` is the verb that *does* replace the set, and it is
//! the only one that asks for confirmation - that asymmetry is the point of having both.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::{linear, output, prompt};

#[derive(Args, Debug)]
pub struct ProjectLabelArgs {
    #[command(subcommand)]
    pub command: Option<ProjectLabelCommand>,
}

#[derive(clap::Subcommand, Debug)]
pub enum ProjectLabelCommand {
    /// List the labels on a project
    List(ProjectLabelListArgs),
    /// Add labels to a project, keeping the ones it has
    Add(LabelEditArgs),
    /// Remove labels from a project, keeping the others
    Remove(LabelEditArgs),
    /// Replace a project's labels with exactly these (asks for confirmation)
    Set(LabelEditArgs),
}

#[derive(Args, Debug)]
pub struct ProjectLabelListArgs {
    /// Project ID, slug, or name
    #[arg(value_name = "projectId")]
    pub project_id: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct LabelEditArgs {
    /// Project ID, slug, or name
    #[arg(value_name = "projectId")]
    pub project_id: String,
    /// Project label name or id
    #[arg(value_name = "label", required = true)]
    pub labels: Vec<String>,
    /// Skip the confirmation `set` asks for
    #[arg(short = 'f', long)]
    pub force: bool,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ProjectLabelArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <ProjectLabelArgs as clap::Args>::augment_args(clap::Command::new("label"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        ProjectLabelCommand::List(args) => {
            list(args).map_err(|error| error.with_context("Failed to list project labels"))
        }
        ProjectLabelCommand::Add(args) => {
            edit(args, Edit::Add).map_err(|error| error.with_context("Failed to add labels"))
        }
        ProjectLabelCommand::Remove(args) => {
            edit(args, Edit::Remove).map_err(|error| error.with_context("Failed to remove labels"))
        }
        ProjectLabelCommand::Set(args) => {
            edit(args, Edit::Set).map_err(|error| error.with_context("Failed to set labels"))
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Edit {
    Add,
    Remove,
    Set,
}

fn list(args: ProjectLabelListArgs) -> Result<()> {
    let project_id = linear::resolve_project_id(&args.project_id)?;
    let (labels, page_info) = linear::get_project_labels(&project_id)?;

    if args.json {
        output::print_json(&json!({ "nodes": labels, "pageInfo": page_info }));
        return Ok(());
    }

    if labels.is_empty() {
        output::line("No labels on this project.");
        return Ok(());
    }

    output::line(&format!("Labels ({}):", labels.len()));
    for label in &labels {
        output::line(&format!(
            "  {}  {}",
            field(label, "name"),
            field(label, "color")
        ));
    }
    Ok(())
}

fn edit(args: LabelEditArgs, edit: Edit) -> Result<()> {
    // `set` is the one verb here that can drop a label the caller did not name, so it is the one
    // that asks. `add` and `remove` cannot, which is why they do not.
    if edit == Edit::Set && !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --force to replace the project's labels without confirmation."));
        }
        let confirmed = prompt::confirm(
            &format!(
                "Replace the labels on {} with exactly: {}?",
                args.project_id,
                args.labels.join(", ")
            ),
            false,
        )?;
        if !confirmed {
            output::line("Canceled; the project's labels are unchanged.");
            return Ok(());
        }
    }

    let project_id = linear::resolve_project_id(&args.project_id)?;
    let current: Vec<String> = linear::get_project_label_ids(&project_id)?;

    // Resolve every name first: a typo should fail the whole edit, not leave half of it applied.
    let mut named: Vec<(String, String)> = Vec::new();
    for reference in &args.labels {
        let id = linear::resolve_project_label_id(reference)?;
        named.push((reference.clone(), id));
    }

    let (ids, changed, unchanged) = match edit {
        Edit::Set => {
            let ids: Vec<String> = named.iter().map(|(_, id)| id.clone()).collect();
            let changed: Vec<String> = named.iter().map(|(name, _)| name.clone()).collect();
            (ids, changed, Vec::new())
        }
        Edit::Add => {
            let mut ids = current.clone();
            let mut changed = Vec::new();
            let mut unchanged = Vec::new();
            for (name, id) in &named {
                if ids.iter().any(|existing| existing == id) {
                    unchanged.push(name.clone());
                } else {
                    ids.push(id.clone());
                    changed.push(name.clone());
                }
            }
            (ids, changed, unchanged)
        }
        Edit::Remove => {
            let mut ids = current.clone();
            let mut changed = Vec::new();
            let mut unchanged = Vec::new();
            for (name, id) in &named {
                if ids.iter().any(|existing| existing == id) {
                    ids.retain(|existing| existing != id);
                    changed.push(name.clone());
                } else {
                    unchanged.push(name.clone());
                }
            }
            (ids, changed, unchanged)
        }
    };

    if changed.is_empty() {
        return Err(CliError::validation(format!(
            "Nothing to change: {} is already in that state",
            unchanged.join(", ")
        )));
    }

    let document = linear::update_project_set(&project_id, Some(ids), None)?;

    if args.json {
        output::print_json(&document);
        return Ok(());
    }

    let verb = match edit {
        Edit::Add => "Added",
        Edit::Remove => "Removed",
        Edit::Set => "Set",
    };
    output::line(&format!(
        "✓ {verb} {} on {}",
        changed.join(", "),
        args.project_id
    ));
    for name in &unchanged {
        output::line(&format!("  {name} was already on the project"));
    }
    Ok(())
}

fn field(node: &Value, name: &str) -> String {
    node.get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
