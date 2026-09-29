//! `linear issue query` — port of `src/commands/issue/issue-query.ts`.
//!
//! Full-text search or filtered listing across one or many teams. `--json`
//! emits the raw GraphQL shape so agent prompts can parse it; otherwise the
//! shared [`crate::issue_table`] renderer prints the table.

use serde_json::Value;

use crate::commands::issue;
use crate::config;
use crate::errors::{CliError, Result};
use crate::issue_table;
use crate::linear;
use crate::output;
use crate::pager;

#[derive(clap::Args, Debug)]
pub struct IssueQueryArgs {
    /// Search term; switches to full-text search mode
    #[arg(long = "search")]
    pub search: Option<String>,
    /// Also match comment bodies (requires --search)
    #[arg(long = "search-comments")]
    pub search_comments: bool,
    /// Restrict to one or more teams (key, name, or ID)
    #[arg(long = "team")]
    pub team: Vec<String>,
    /// Query the whole workspace instead of a single team
    #[arg(long = "all-teams")]
    pub all_teams: bool,
    /// Filter by state type(s), e.g. unstarted, started, completed
    #[arg(short = 's', long = "state")]
    pub state: Vec<String>,
    /// Include issues in all states
    #[arg(long = "all-states")]
    pub all_states: bool,
    /// Filter by assignee (name, email, or ID)
    #[arg(long = "assignee")]
    pub assignee: Option<String>,
    /// Include issues assigned to anyone
    #[arg(short = 'A', long = "all-assignees")]
    pub all_assignees: bool,
    /// Only unassigned issues
    #[arg(short = 'U', long = "unassigned")]
    pub unassigned: bool,
    /// Sort order for results
    #[arg(long = "sort")]
    pub sort: Option<String>,
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
    pub limit: i64,
    /// Filter to issues created after this date
    #[arg(long = "created-after")]
    pub created_after: Option<String>,
    /// Filter to issues updated after this date
    #[arg(long = "updated-after")]
    pub updated_after: Option<String>,
    /// Include archived issues
    #[arg(long = "include-archived")]
    pub include_archived: bool,
    /// Output results as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
    /// Disable automatic paging for long output
    #[arg(long = "no-pager", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub pager: bool,
}

pub fn run(args: IssueQueryArgs) -> Result<()> {
    let result = query(&args);
    result.map_err(|error| error.with_context("Failed to query issues"))
}

fn query(args: &IssueQueryArgs) -> Result<()> {
    // --- validation, in upstream order -------------------------------------
    if !args.team.is_empty() && args.all_teams {
        return Err(CliError::validation(
            "Cannot use both --team and --all-teams flags",
        ));
    }

    let assignee_filters = [
        args.assignee.is_some(),
        args.all_assignees,
        args.unassigned,
    ]
    .iter()
    .filter(|set| **set)
    .count();
    if assignee_filters > 1 {
        return Err(CliError::validation(
            "Cannot specify multiple assignee filters (--assignee, --all-assignees, --unassigned)",
        ));
    }

    let state_array = args.state.clone();
    if args.all_states && !state_array.is_empty() {
        return Err(CliError::validation(
            "Cannot use --all-states with --state flag",
        ));
    }

    if args.project.is_some() && args.project_label.is_some() {
        return Err(CliError::validation(
            "Cannot use --project and --project-label together",
        )
        .suggestion(
            "Use --project to filter by a single project, or --project-label to filter by all projects with a given label.",
        ));
    }

    if let Some(milestone) = args.milestone.as_deref() {
        if args.project.is_none() && !linear::is_linear_uuid(milestone) {
            return Err(CliError::validation("--milestone requires --project to be set")
                .suggestion(
                    "Use --project to specify which project the milestone belongs to, or pass a milestone UUID directly.",
                ));
        }
        if args.project_label.is_some() {
            return Err(CliError::validation(
                "--milestone cannot be used with --project-label",
            )
            .suggestion("Use --project to specify a single project when filtering by milestone."));
        }
    }

    if args.search_comments && args.search.is_none() {
        return Err(CliError::validation("--search-comments requires --search to be set")
            .suggestion(
                "Use --search to provide a search term, e.g. --search \"oauth timeout\" --search-comments.",
            ));
    }

    if args.sort.is_some() && args.search.is_some() {
        return Err(CliError::validation("--sort cannot be used with --search").suggestion(
            "Search results use relevance ordering. Remove --sort when using --search.",
        ));
    }

    if args.limit < 0 {
        return Err(CliError::validation("--limit must be 0 or greater"));
    }

    // --- team scope --------------------------------------------------------
    let mut is_multi_team = false;
    let mut explicit_team_id: Option<String> = None;
    let team_keys: Option<Vec<String>> = if args.all_teams {
        is_multi_team = true;
        None
    } else if !args.team.is_empty() {
        let teams = linear::resolve_teams(&args.team)?;
        is_multi_team = teams.len() > 1;
        if teams.len() == 1 {
            explicit_team_id = Some(teams[0].id.clone());
        }
        Some(teams.into_iter().map(|team| team.key).collect())
    } else {
        let Some(resolved) = linear::get_team_key_with_source()? else {
            return Err(CliError::validation(
                "No default team configured and no team scope provided",
            )
            .suggestion(
                "Use --team <key, name, or ID> to specify a team, or --all-teams to query the whole workspace.",
            ));
        };
        if should_show_default_team_note(resolved.source) {
            eprintln!(
                "Note: using default team {}. Pass --team <key, name, or ID> or --all-teams to be explicit.",
                resolved.value
            );
        }
        Some(vec![resolved.value])
    };

    // --- entity resolution -------------------------------------------------
    let state_selection = if state_array.is_empty() {
        None
    } else {
        let scope = match &team_keys {
            Some(keys) => linear::StateScope::TeamKeys(keys.clone()),
            None => linear::StateScope::AllTeams,
        };
        Some(linear::resolve_state_selection(&state_array, &scope)?)
    };

    let project_id = issue::resolve_project_id(args.project.as_deref())?;

    let cycle_id = match args.cycle.as_deref() {
        None => None,
        Some(cycle) => {
            if team_keys.as_ref().map(|keys| keys.len() == 1) != Some(true) {
                return Err(CliError::validation("--cycle requires a single team scope").suggestion(
                    "Use --team <key, name, or ID> to specify exactly one team when filtering by cycle.",
                ));
            }
            let keys = team_keys.as_ref().expect("checked single team scope");
            let team_id = match &explicit_team_id {
                Some(id) => id.clone(),
                None => linear::resolve_team(&keys[0])?.id,
            };
            Some(linear::get_cycle_id_by_name_or_number(&team_id, cycle)?)
        }
    };

    let milestone_id = match args.milestone.as_deref() {
        None => None,
        Some(milestone) if linear::is_linear_uuid(milestone) => Some(milestone.to_string()),
        Some(milestone) => Some(linear::resolve_milestone_id(milestone, project_id.as_deref())?),
    };

    let label_names = if args.labels.is_empty() {
        None
    } else {
        Some(args.labels.clone())
    };

    // --- fetch -------------------------------------------------------------
    let limit = Some(args.limit as u32);
    let result = if args.search.is_some() {
        let term = args.search.as_deref().unwrap_or("").trim();
        if term.is_empty() {
            return Err(CliError::validation("--search term cannot be empty"));
        }
        let options = linear::SearchIssuesByTermOptions {
            team_keys,
            state: state_selection,
            assignee: args.assignee.clone(),
            unassigned: args.unassigned,
            limit,
            project_id,
            project_label: args.project_label.clone(),
            cycle_id,
            label_names,
            created_after: args.created_after.clone(),
            updated_after: args.updated_after.clone(),
            include_archived: Some(args.include_archived),
            include_comments: Some(args.search_comments),
            order_by: None,
        };
        linear::search_issues_by_term(term, &options)?
    } else {
        let sort = Some(config::resolve_issue_sort(args.sort.as_deref())?);
        let options = linear::FetchIssuesForQueryOptions {
            team_keys,
            all_teams: args.all_teams,
            state: state_selection,
            assignee: args.assignee.clone(),
            unassigned: args.unassigned,
            sort,
            limit,
            project_id,
            project_label: args.project_label.clone(),
            cycle_id,
            milestone_id,
            label_names,
            created_after: args.created_after.clone(),
            updated_after: args.updated_after.clone(),
            include_archived: Some(args.include_archived),
        };
        linear::fetch_issues_for_query(&options)?
    };

    if args.json {
        output::print_json(&result);
        return Ok(());
    }

    let nodes: Vec<Value> = result
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if nodes.is_empty() {
        output::line("No issues found.");
        return Ok(());
    }

    let show_assignee = args.assignee.is_none() && !args.unassigned;
    let lines = issue_table::render(
        &nodes,
        &issue_table::Options {
            show_team_column: is_multi_team,
            show_assignee_column: show_assignee,
            min_title_width: 10,
            padding: 0,
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

/// `shouldShowDefaultTeamNote`: only warn when the team came from a broad
/// source (env or the global config), not when it was explicit or per-project.
fn should_show_default_team_note(source: config::OptionSource) -> bool {
    matches!(
        source,
        config::OptionSource::Env | config::OptionSource::GlobalConfig
    )
}
