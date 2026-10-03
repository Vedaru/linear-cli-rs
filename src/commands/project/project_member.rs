//! `linear project member add|remove` — edit a project's members without dropping the others.
//!
//! `ProjectUpdateInput.memberIds` is the **whole** set, not a delta, so the obvious implementation
//! (send the ids the caller named) would silently remove everyone else. Both verbs read the
//! current set first and send it back with one name added or removed, and a name that is already
//! on the project - or not yet on it - is reported rather than treated as success.

use clap::Args;
use serde_json::Value;

use crate::errors::{CliError, Result};
use crate::{linear, output};

#[derive(Args, Debug)]
pub struct ProjectMemberArgs {
    #[command(subcommand)]
    pub command: Option<ProjectMemberCommand>,
}

#[derive(clap::Subcommand, Debug)]
pub enum ProjectMemberCommand {
    /// Add one or more members to a project
    Add(MemberEditArgs),
    /// Remove one or more members from a project
    Remove(MemberEditArgs),
}

#[derive(Args, Debug)]
pub struct MemberEditArgs {
    /// Project ID, slug, or name
    #[arg(value_name = "projectId")]
    pub project_id: String,
    /// User name, email, or ID
    #[arg(value_name = "user", required = true)]
    pub users: Vec<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: ProjectMemberArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <ProjectMemberArgs as clap::Args>::augment_args(clap::Command::new("member"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        ProjectMemberCommand::Add(args) => {
            edit(args, false).map_err(|error| error.with_context("Failed to add project member"))
        }
        ProjectMemberCommand::Remove(args) => {
            edit(args, true).map_err(|error| error.with_context("Failed to remove project member"))
        }
    }
}

/// Add (or remove) the named members, sending the whole resulting set.
fn edit(args: MemberEditArgs, removing: bool) -> Result<()> {
    let project_id = linear::resolve_project_id(&args.project_id)?;
    let (current, _) = linear::get_project_members(&project_id)?;
    let mut ids: Vec<String> = current
        .iter()
        .filter_map(|member| member.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();

    let mut acted: Vec<String> = Vec::new();
    let mut unchanged: Vec<String> = Vec::new();
    for reference in &args.users {
        let Some(user_id) = linear::lookup_user_id(reference)? else {
            return Err(CliError::not_found("User", reference));
        };
        let present = ids.iter().any(|id| id == &user_id);
        if removing == present {
            if removing {
                ids.retain(|id| id != &user_id);
            } else {
                ids.push(user_id);
            }
            acted.push(reference.clone());
        } else {
            unchanged.push(reference.clone());
        }
    }

    // A member list where nothing moved is a no-op, and reporting success for one would make the
    // CLI's exit code a worse signal than the API's own answer.
    if acted.is_empty() {
        let verb = if removing { "not on" } else { "already on" };
        return Err(CliError::validation(format!(
            "Nothing to change: {} {verb} this project",
            unchanged.join(", ")
        )));
    }

    let document = linear::update_project_set(&project_id, None, Some(ids))?;

    if args.json {
        output::print_json(&document);
        return Ok(());
    }

    let (verb, preposition) = if removing {
        ("Removed", "from")
    } else {
        ("Added", "to")
    };
    output::line(&format!(
        "✓ {verb} {} {preposition} {}",
        acted.join(", "),
        args.project_id
    ));
    for reference in &unchanged {
        output::line(&format!(
            "  {reference} was already {}",
            if removing { "absent" } else { "a member" }
        ));
    }
    Ok(())
}
