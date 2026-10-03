//! `linear issue start` — port of `src/commands/issue/issue-start.ts`.
//!
//! Picks an unstarted issue (or accepts one), starts a branch for it, and
//! moves it to the team's started state. The issue picker is guarded by
//! [`crate::prompt::is_interactive`]; non-interactive runs must pass the issue
//! identifier as an argument instead of blocking.

use clap::Args;
use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::linear;
use crate::{graphql, output, prompt, vcs};

/// Start working on an issue
#[derive(Args, Debug)]
pub struct IssueStartArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
    /// Show issues for all assignees
    #[arg(short = 'A', long = "all-assignees")]
    pub all_assignees: bool,
    /// Show only unassigned issues
    #[arg(short = 'U', long)]
    pub unassigned: bool,
    /// Git ref to create new branch from
    #[arg(short = 'f', long = "from-ref", value_name = "fromRef")]
    pub from_ref: Option<String>,
    /// Custom branch name to use instead of the issue identifier
    #[arg(short = 'b', long, value_name = "branch")]
    pub branch: Option<String>,
    /// Accepted for compatibility: this IS the default - `start` always creates and switches to
    /// the issue's branch. The other Rust CLI only does it when asked, so a script written for it
    /// passes this flag; here it changes nothing and can be omitted.
    #[arg(long = "checkout", conflicts_with = "no_checkout")]
    pub checkout: bool,
    /// Do not touch git: set the issue's state without creating or switching a branch, which is
    /// what the other CLI does unless it is given `--checkout`.
    #[arg(long = "no-checkout")]
    pub no_checkout: bool,
}

/// `GET_ISSUE_DETAILS_QUERY` in the shared data layer omits `branchName`, so
/// the branch name is fetched with a small dedicated query here.
const GET_ISSUE_BRANCH_NAME_QUERY: &str = r#"
query GetIssueBranchName($id: String!) {
  issue(id: $id) {
    branchName
  }
}
"#;

const UPDATE_ISSUE_STATE_MUTATION: &str = r#"
mutation UpdateIssueState($issueId: String!, $stateId: String!) {
  issueUpdate(id: $issueId, input: { stateId: $stateId }) {
    success
  }
}
"#;

pub fn run(args: IssueStartArgs) -> Result<()> {
    run_inner(args).map_err(|error| error.with_context("Failed to start issue"))
}

fn run_inner(args: IssueStartArgs) -> Result<()> {
    let Some(team_id) = linear::get_team_key()? else {
        return Err(CliError::validation("Could not determine team ID"));
    };

    // Validate that conflicting flags are not used together.
    if args.all_assignees && args.unassigned {
        return Err(CliError::validation(
            "Cannot specify both --all-assignees and --unassigned",
        ));
    }

    // Only resolve the provided issueId, don't infer from VCS (start should
    // pick from a list, not continue on the current issue).
    let mut resolved_id = linear::get_issue_identifier(args.issue_id.as_deref())?;

    if resolved_id.is_none() {
        let state = linear::StateSelection {
            types: vec!["unstarted".to_string()],
            state_ids: Vec::new(),
        };
        let options = linear::FetchIssuesForStateOptions {
            unassigned: args.unassigned,
            all_assignees: args.all_assignees,
            ..Default::default()
        };
        let result = linear::fetch_issues_for_state(&team_id, Some(&state), &options)?;
        let issues: Vec<Value> = result
            .get("issues")
            .and_then(|issues| issues.get("nodes"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if issues.is_empty() {
            return Err(CliError::not_found("Unstarted issues", &team_id));
        }

        let labels: Vec<String> = issues
            .iter()
            .map(|issue| {
                let priority = issue.get("priority").and_then(Value::as_i64).unwrap_or(0);
                let identifier = issue
                    .get("identifier")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
                format!(
                    "{} {identifier}: {title}",
                    crate::display::get_priority_display(priority)
                )
            })
            .collect();

        let selected = prompt::select("Select an issue to start:", &labels)?;
        resolved_id = issues[selected]
            .get("identifier")
            .and_then(Value::as_str)
            .map(str::to_string);
    }

    let Some(resolved_id) = resolved_id else {
        return Err(CliError::validation("No issue ID resolved"));
    };

    start_work_on_issue(
        &resolved_id,
        &team_id,
        args.from_ref.as_deref(),
        args.branch.as_deref(),
        !args.no_checkout,
        false,
    )
}

pub(crate) fn start_work_on_issue(
    issue_id: &str,
    team_id: &str,
    git_source_ref: Option<&str>,
    custom_branch_name: Option<&str>,
    checkout: bool,
    quiet: bool,
) -> Result<()> {
    let client = graphql::client()?;

    // The branch is only worked out when it is going to be used, and that is not just tidiness:
    // resolving it costs a request (`branchName` is not in the shared issue query), so a caller who
    // asked not to touch git should not pay for the answer either.
    if checkout {
        let default_branch_name = fetch_branch_name(&client, issue_id)?;
        let branch_name = custom_branch_name
            .map(str::to_string)
            .or(default_branch_name)
            .unwrap_or_else(|| issue_id.to_string());

        vcs::start_vcs_work(issue_id, &branch_name, git_source_ref)?;
    }

    // Best-effort: failure to move the issue to a started state is logged,
    // not fatal, matching upstream's `startWorkOnIssue`.
    match linear::get_started_state(team_id) {
        Ok(state) => {
            let result = client.request(
                UPDATE_ISSUE_STATE_MUTATION,
                json!({ "issueId": issue_id, "stateId": state.id }),
            );
            match result {
                Ok(_) if !quiet => {
                    output::line(&format!("✓ Issue state updated to '{}'", state.name));
                }
                Ok(_) => {}
                Err(error) => eprintln!("Failed to update issue state: {error}"),
            }
        }
        Err(error) => eprintln!("Failed to update issue state: {error}"),
    }

    Ok(())
}

fn fetch_branch_name(client: &graphql::Client, issue_id: &str) -> Result<Option<String>> {
    let data = client.request(GET_ISSUE_BRANCH_NAME_QUERY, json!({ "id": issue_id }))?;
    Ok(data
        .get("issue")
        .and_then(|issue| issue.get("branchName"))
        .and_then(Value::as_str)
        .map(str::to_string))
}
