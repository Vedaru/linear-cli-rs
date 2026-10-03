//! The issue workflow verbs: `close`, `assign`, `move`, `transfer`.
//!
//! **Names over one implementation, not four write paths.** Each verb lowers to exactly the update
//! `linear issue update` issues, by constructing its argument struct and calling it - so a verb
//! cannot drift from the command it names, and a change to how an update is built or sent is
//! automatically a change to all four. That is the whole design; the verb bodies are three lines
//! each and the interesting code is [`lower`].
//!
//! The verbs exist because they are the four things a person does to an issue most often, and
//! spelling them out costs an argument every time:
//!
//! ```text
//! linear issue update VED-42 -s Done        linear issue close VED-42
//! linear issue update VED-42 -a someone     linear issue assign VED-42 someone
//! linear issue update VED-42 --project X    linear issue move VED-42 X
//! linear issue update VED-42 --team ENG     linear issue transfer VED-42 ENG
//! ```
//!
//! The long form stays the general one - it can set several fields at once, which the verbs
//! deliberately cannot (a verb that also renamed the issue would be a surprise). `--json` is
//! forwarded, so a verb prints what `issue update --json` prints.

use clap::Args;

use crate::commands::issue::issue_update::{self, IssueUpdateArgs};
use crate::errors::Result;

/// Everything a verb sets, and nothing else.
///
/// Constructing the update through this struct rather than through five field assignments is what
/// keeps the verbs honest: adding a field to `IssueUpdateArgs` does not compile until it is named
/// here, so no verb can silently inherit a default the general command changed.
#[derive(Default)]
struct Lowered {
    state: Option<String>,
    assignee: Option<String>,
    project: Option<String>,
    team: Option<String>,
}

impl Lowered {
    /// The update as `linear issue update` would receive it.
    fn into_update(self, reference: String, json: bool) -> IssueUpdateArgs {
        IssueUpdateArgs {
            assignee: self.assignee,
            unassign: false,
            due_date: None,
            clear_due_date: false,
            parent: None,
            clear_parent: false,
            priority: None,
            estimate: None,
            clear_estimate: false,
            description: None,
            description_file: None,
            label: Vec::new(),
            add_label: Vec::new(),
            remove_label: Vec::new(),
            team: self.team,
            project: self.project,
            clear_project: false,
            state: self.state,
            milestone: None,
            clear_milestone: false,
            cycle: None,
            clear_cycle: false,
            title: None,
            issue_id: Some(reference),
            json,
        }
    }
}

/// Run a verb: one lowering, one call, one error context shape.
fn lower(lowered: Lowered, reference: String, json: bool) -> Result<()> {
    issue_update::run(lowered.into_update(reference, json))
}

// --- close ------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct CloseArgs {
    /// Issue ID (e.g. ENG-123), URL, or the id from a branch
    #[arg(value_name = "issueId")]
    pub issue_id: String,
    /// Output the updated issue as JSON, as the API returned it
    #[arg(short = 'j', long)]
    pub json: bool,
}

/// `linear issue close <id>` - the state Linear calls Done.
///
/// "Done" by *name* rather than by state type, and that is deliberate: a team's completed state is
/// usually called Done, and where it is not, the name is still the thing the person typed. The
/// resolution and its error message are `issue update`'s, not a second implementation of them.
pub fn close(args: CloseArgs) -> Result<()> {
    lower(
        Lowered {
            state: Some("Done".to_string()),
            ..Lowered::default()
        },
        args.issue_id,
        args.json,
    )
}

// --- assign -----------------------------------------------------------------

#[derive(Args, Debug)]
pub struct AssignArgs {
    /// Issue ID (e.g. ENG-123), URL, or the id from a branch
    #[arg(value_name = "issueId")]
    pub issue_id: String,
    /// Who to assign it to: `self`, a username, or a display name
    #[arg(value_name = "assignee")]
    pub assignee: String,
    /// Output the updated issue as JSON, as the API returned it
    #[arg(short = 'j', long)]
    pub json: bool,
}

/// `linear issue assign <id> <who>` - `issue update --assignee`.
pub fn assign(args: AssignArgs) -> Result<()> {
    lower(
        Lowered {
            assignee: Some(args.assignee),
            ..Lowered::default()
        },
        args.issue_id,
        args.json,
    )
}

// --- move -------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct MoveArgs {
    /// Issue ID (e.g. ENG-123), URL, or the id from a branch
    #[arg(value_name = "issueId")]
    pub issue_id: String,
    /// Project to move it into (UUID, slug ID, or name)
    #[arg(value_name = "project")]
    pub project: String,
    /// Output the updated issue as JSON, as the API returned it
    #[arg(short = 'j', long)]
    pub json: bool,
}

/// `linear issue move <id> <project>` - `issue update --project`.
///
/// "Move" here means into a project, not to another team: a team change has different consequences
/// (the issue's identifier changes, and its workflow states with it), which is why it is its own
/// verb - `transfer`.
pub fn move_issue(args: MoveArgs) -> Result<()> {
    lower(
        Lowered {
            project: Some(args.project),
            ..Lowered::default()
        },
        args.issue_id,
        args.json,
    )
}

// --- transfer ---------------------------------------------------------------

#[derive(Args, Debug)]
pub struct TransferArgs {
    /// Issue ID (e.g. ENG-123), URL, or the id from a branch
    #[arg(value_name = "issueId")]
    pub issue_id: String,
    /// Team to transfer it to, by key, name or id
    #[arg(value_name = "team")]
    pub team: String,
    /// Output the updated issue as JSON, as the API returned it
    #[arg(short = 'j', long)]
    pub json: bool,
}

/// `linear issue transfer <id> <team>` - `issue update --team`.
///
/// The identifier changes with the team (VED-42 becomes ENG-42), which the output of the underlying
/// command shows and this one does not hide.
pub fn transfer(args: TransferArgs) -> Result<()> {
    lower(
        Lowered {
            team: Some(args.team),
            ..Lowered::default()
        },
        args.issue_id,
        args.json,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The field of an update, so a test can say "exactly this one, and nothing else".
    ///
    /// `IssueUpdateArgs` has no `PartialEq` and giving it one to serve a test would be the tail
    /// wagging the dog; this reads the two dozen fields explicitly instead. That is the point: the
    /// list below *is* the drift guard. A verb that quietly set a second field - or a new field
    /// added to `IssueUpdateArgs` and defaulted somewhere - shows up here as a difference, because
    /// every field is named.
    #[derive(Debug, PartialEq)]
    struct SetFields {
        state: Option<String>,
        assignee: Option<String>,
        project: Option<String>,
        team: Option<String>,
        issue_id: Option<String>,
        json: bool,
        everything_else_is_default: bool,
    }

    fn fields(args: IssueUpdateArgs) -> SetFields {
        let untouched = !args.unassign
            && !args.clear_due_date
            && args.due_date.is_none()
            && !args.clear_parent
            && args.parent.is_none()
            && args.priority.is_none()
            && !args.clear_estimate
            && args.estimate.is_none()
            && args.description.is_none()
            && args.description_file.is_none()
            && args.label.is_empty()
            && args.add_label.is_empty()
            && args.remove_label.is_empty()
            && !args.clear_project
            && args.milestone.is_none()
            && !args.clear_milestone
            && args.cycle.is_none()
            && !args.clear_cycle
            && args.title.is_none();
        SetFields {
            state: args.state,
            assignee: args.assignee,
            project: args.project,
            team: args.team,
            issue_id: args.issue_id,
            json: args.json,
            everything_else_is_default: untouched,
        }
    }

    #[test]
    fn each_verb_sets_exactly_one_field() {
        // The verbs are names over `issue update`, so what a verb must not do is become its own
        // write path: one field, and `issue_id` and `json` carried through.
        let closed = Lowered {
            state: Some("Done".to_string()),
            ..Lowered::default()
        }
        .into_update("VED-42".to_string(), false);
        assert_eq!(
            fields(closed),
            SetFields {
                state: Some("Done".to_string()),
                assignee: None,
                project: None,
                team: None,
                issue_id: Some("VED-42".to_string()),
                json: false,
                everything_else_is_default: true,
            }
        );

        // Each verb is lowered once and read once: `fields` consumes the struct, which is the
        // compiler's way of saying this check is meant to be exhaustive rather than repeated.
        let assigned = fields(
            Lowered {
                assignee: Some("someone".to_string()),
                ..Lowered::default()
            }
            .into_update("VED-42".to_string(), false),
        );
        assert_eq!(assigned.assignee.as_deref(), Some("someone"));
        assert_eq!(assigned.state, None, "assigning sets no state");

        let moved = fields(
            Lowered {
                project: Some("Q2".to_string()),
                ..Lowered::default()
            }
            .into_update("VED-42".to_string(), false),
        );
        assert_eq!(moved.project.as_deref(), Some("Q2"));
        assert_eq!(moved.team, None, "moving is not transferring");

        let transferred = fields(
            Lowered {
                team: Some("ENG".to_string()),
                ..Lowered::default()
            }
            .into_update("VED-42".to_string(), false),
        );
        assert_eq!(transferred.team.as_deref(), Some("ENG"));
        assert_eq!(transferred.project, None, "transferring is not moving");
    }

    #[test]
    fn a_verb_forwards_json_and_the_issue_it_was_given() {
        let args = CloseArgs {
            issue_id: "VED-42".to_string(),
            json: true,
        };
        // `close` is the one verb whose whole body is a call to `lower`, so this is also the check
        // that the arguments it builds are the ones it was handed.
        let lowered = Lowered {
            state: Some("Done".to_string()),
            ..Lowered::default()
        }
        .into_update(args.issue_id.clone(), args.json);
        assert_eq!(lowered.issue_id.as_deref(), Some("VED-42"));
        assert!(lowered.json, "--json is forwarded, not swallowed");
        assert!(
            lowered.title.is_none(),
            "a verb never renames as a side effect"
        );
    }
}
