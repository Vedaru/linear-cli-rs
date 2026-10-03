//! `linear issue mine` — port of `src/commands/issue/issue-mine.ts`.
//!
//! Lists the issues assigned to the current user, scoped to the configured
//! team. `--web` / `--app` hand off to the team assignee view and return before
//! the rest of the command runs (upstream's call sits outside the `try`).

use serde_json::Value;

use crate::commands::issue;
use crate::config;
use crate::errors::{CliError, Result};
use crate::git;
use crate::issue_table;
use crate::linear;
use crate::output;
use crate::pager;

#[derive(clap::Args, Debug)]
pub struct IssueMineArgs {
    /// Filter by state type(s), e.g. unstarted, started, completed
    #[arg(short = 's', long = "state", default_value = "unstarted")]
    pub state: Vec<String>,
    /// Include issues in all states
    #[arg(long = "all-states")]
    pub all_states: bool,
    /// Sort order for results
    #[arg(long = "sort")]
    pub sort: Option<String>,
    /// Team key, name, or ID
    #[arg(long = "team")]
    pub team: Option<String>,
    /// Project name, ID, or URL
    #[arg(long = "project")]
    pub project: Option<String>,
    /// Filter by a project label
    #[arg(long = "project-label")]
    pub project_label: Option<String>,
    /// Cycle name, number, or ID
    #[arg(long = "cycle")]
    pub cycle: Option<String>,
    /// Milestone name or ID
    #[arg(long = "milestone")]
    pub milestone: Option<String>,
    /// Filter by label(s)
    #[arg(short = 'l', long = "label")]
    pub labels: Vec<String>,
    /// Maximum number of issues to return (0 for no limit)
    #[arg(long = "limit", default_value_t = 50)]
    pub limit: u32,
    /// Filter to issues created after this date
    #[arg(long = "created-after")]
    pub created_after: Option<String>,
    /// Filter to issues updated after this date
    #[arg(long = "updated-after")]
    pub updated_after: Option<String>,
    /// Removed from 'issue mine'; use 'issue query --assignee'
    #[arg(long = "assignee", hide = true)]
    pub assignee: Option<String>,
    /// Removed from 'issue mine'; use 'issue query --all-assignees'
    #[arg(short = 'A', long = "all-assignees", hide = true)]
    pub all_assignees: bool,
    /// Removed from 'issue mine'; use 'issue query --unassigned'
    #[arg(short = 'U', long = "unassigned", hide = true)]
    pub unassigned: bool,
    /// Open in web browser
    #[arg(short = 'w', long)]
    pub web: bool,
    /// Open in Linear.app
    #[arg(short = 'a', long)]
    pub app: bool,
    /// Disable automatic paging for long output
    #[arg(long = "no-pager", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub pager: bool,
    /// Output issue data as JSON (an addition: upstream's `issue mine` has no
    /// machine-readable form, so the shape is ours and pinned by a fixture)
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: IssueMineArgs) -> Result<()> {
    if args.web || args.app {
        return crate::actions::open_team_assignee_view(args.app);
    }

    let result = mine(&args);
    result.map_err(|error| error.with_context("Failed to list issues"))
}

fn mine(args: &IssueMineArgs) -> Result<()> {
    if args.assignee.is_some() || args.all_assignees || args.unassigned {
        let flag = if args.assignee.is_some() {
            "--assignee"
        } else if args.all_assignees {
            "--all-assignees"
        } else {
            "--unassigned"
        };
        return Err(
            CliError::validation(format!("{flag} has been removed from 'issue mine'")).suggestion(
                format!("Use 'linear issue query {flag}' for assignee filtering."),
            ),
        );
    }

    let state_array = args.state.clone();
    if args.all_states
        && (state_array.len() > 1 || state_array.first().map(String::as_str) != Some("unstarted"))
    {
        return Err(CliError::validation(
            "Cannot use --all-states with --state flag",
        ));
    }

    let sort = config::resolve_issue_sort(args.sort.as_deref())?;

    let explicit_team = args.team.as_deref().map(linear::resolve_team).transpose()?;
    let team_key = match explicit_team.as_ref().map(|team| team.key.clone()) {
        Some(team_key) => Some(team_key),
        None => linear::get_team_key()?,
    };
    let Some(team_key) = team_key else {
        let suggestion = if git::is_inside_git_repo() {
            "Use --team <key, name, or ID> to specify a team, or run `linear config` to link this repository to a team."
        } else {
            "Use --team <key, name, or ID> to specify a team."
        };
        return Err(
            CliError::validation("No default team configured and no team scope provided")
                .suggestion(suggestion),
        );
    };

    if args.project.is_some() && args.project_label.is_some() {
        return Err(CliError::validation(
            "Cannot use --project and --project-label together",
        )
        .suggestion(
            "Use --project to filter by a single project, or --project-label to filter by all projects with a given label.",
        ));
    }

    let project_id = issue::resolve_project_id(args.project.as_deref())?;

    let cycle_id = match args.cycle.as_deref() {
        None => None,
        Some(cycle) => {
            let team_id = match &explicit_team {
                Some(team) => team.id.clone(),
                None => linear::resolve_team(&team_key)?.id,
            };
            Some(linear::get_cycle_id_by_name_or_number(&team_id, cycle)?)
        }
    };

    let milestone_id = match args.milestone.as_deref() {
        None => None,
        Some(milestone) => {
            if args.project_label.is_some() {
                return Err(CliError::validation(
                    "--milestone cannot be used with --project-label",
                )
                .suggestion(
                    "Use --project to specify a single project when filtering by milestone.",
                ));
            }
            if linear::is_linear_uuid(milestone) {
                Some(milestone.to_string())
            } else {
                let Some(project_id) = project_id.as_deref() else {
                    return Err(CliError::validation("--milestone requires --project to be set")
                        .suggestion(
                            "Use --project to specify which project the milestone belongs to, or pass a milestone UUID directly.",
                        ));
                };
                Some(linear::resolve_milestone_id(milestone, Some(project_id))?)
            }
        }
    };

    let label_names = if args.labels.is_empty() {
        None
    } else {
        Some(args.labels.clone())
    };

    let state_selection = if args.all_states {
        None
    } else {
        Some(linear::resolve_state_selection(
            &state_array,
            &linear::StateScope::TeamKeys(vec![team_key.clone()]),
        )?)
    };

    let options = linear::FetchIssuesForStateOptions {
        assignee: None,
        unassigned: false,
        all_assignees: false,
        limit: if args.limit == 0 {
            None
        } else {
            Some(args.limit)
        },
        project_id,
        sort: Some(sort),
        cycle_id,
        milestone_id,
        project_label: args.project_label.clone(),
        label_names,
        created_after: args.created_after.clone(),
        updated_after: args.updated_after.clone(),
    };

    let result = linear::fetch_issues_for_state(&team_key, state_selection.as_ref(), &options)?;

    // The raw GraphQL shape, exactly as `issue query --json` emits it: the two
    // commands answer the same question ("the issues in this window") and an
    // agent that can parse one can parse the other. That also means an empty
    // result is an empty `nodes` array rather than the sentence the table path
    // prints, because a caller that asked for JSON is parsing, not reading.
    if args.json {
        output::print_json(&result);
        return Ok(());
    }

    let issues: Vec<Value> = result
        .pointer("/issues/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if issues.is_empty() {
        output::line("No issues found.");
        return Ok(());
    }

    let lines = issue_table::render(
        &issues,
        &issue_table::Options {
            show_team_column: false,
            show_assignee_column: false,
            min_title_width: 0,
            padding: 1,
        },
    );

    if pager::should_use_pager(lines.len(), args.pager) {
        pager::pipe_to_user_pager(&lines.join("\n"));
    } else {
        for line in &lines {
            output::line(line);
        }
    }

    Ok(())
}
