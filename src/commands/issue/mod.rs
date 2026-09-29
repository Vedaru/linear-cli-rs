//! `linear issue` — manage Linear issues. Port of `src/commands/issue/issue.ts`.
//!
//! The group itself has no action: with no subcommand it prints help, matching
//! upstream's `this.showHelp()`. Subcommand groups (`comment`, `agent-session`)
//! own their own nested dispatch in their module.

pub mod issue_agent_session;
pub mod issue_archive;
pub mod issue_attach;
pub mod issue_comment;
pub mod issue_commits;
pub mod issue_create;
pub mod issue_delete;
pub mod issue_describe;
pub mod issue_id;
pub mod issue_link;
pub mod issue_mine;
pub mod issue_pull_request;
pub mod issue_query;
pub mod issue_relation;
pub mod issue_start;
pub mod issue_title;
pub mod issue_update;
pub mod issue_url;
pub mod issue_view;

use crate::errors::{CliError, Result};
use crate::linear;
use crate::output;
use crate::prompt;

/// Shared project resolution for `issue mine` / `issue query`.
///
/// Resolves an exact name/URL/UUID, then falls back to a fuzzy option list:
/// an empty list is a not-found error, a non-interactive run reports the close
/// matches, and an interactive run lets the caller pick one.
pub(crate) fn resolve_project_id(project: Option<&str>) -> Result<Option<String>> {
    let Some(project) = project else {
        return Ok(None);
    };

    if let Some(id) = linear::get_project_id_by_name(project)? {
        return Ok(Some(id));
    }

    let options = linear::get_project_options_by_name(project)?;
    if options.is_empty() {
        return Err(CliError::not_found("Project", project));
    }

    if !prompt::is_interactive() {
        let names = options
            .iter()
            .map(|(_, name)| name.clone())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CliError::validation(format!(
            "Project \"{project}\" not found. Similar projects: {names}"
        )));
    }

    linear::select_option("Project", project, &options)
}

#[derive(clap::Args, Debug)]
pub struct IssueArgs {
    #[command(subcommand)]
    pub command: Option<IssueCommand>,
}

#[derive(clap::Subcommand, Debug)]
pub enum IssueCommand {
    /// Print the issue id for the current branch
    Id(issue_id::IssueIdArgs),
    /// List issues assigned to you
    #[command(alias = "list", alias = "l")]
    Mine(issue_mine::IssueMineArgs),
    /// Search issues with a query
    #[command(alias = "q")]
    Query(issue_query::IssueQueryArgs),
    /// Print the issue title for the current branch
    Title(issue_title::IssueTitleArgs),
    /// Move an issue to a started state
    Start(issue_start::IssueStartArgs),
    /// View issue details (default) or open in browser/app
    #[command(alias = "v")]
    View(issue_view::IssueViewArgs),
    /// Print the issue URL for the current branch
    Url(issue_url::IssueUrlArgs),
    /// Print a context/description payload for an issue
    Describe(issue_describe::IssueDescribeArgs),
    /// List commits linked to an issue
    Commits(issue_commits::IssueCommitsArgs),
    /// Show the pull request linked to an issue
    #[command(name = "pull-request", alias = "pr")]
    PullRequest(issue_pull_request::IssuePullRequestArgs),
    /// Archive an issue
    Archive(issue_archive::IssueArchiveArgs),
    /// Delete an issue
    Delete(issue_delete::IssueDeleteArgs),
    /// Create a new issue
    Create(issue_create::IssueCreateArgs),
    /// Update an issue
    Update(issue_update::IssueUpdateArgs),
    /// Manage issue comments
    Comment(issue_comment::IssueCommentArgs),
    /// Attach a file to an issue
    Attach(issue_attach::IssueAttachArgs),
    /// Manage issue links
    Link(issue_link::IssueLinkArgs),
    /// Manage issue relations
    Relation(issue_relation::IssueRelationArgs),
    /// View agent sessions for an issue
    #[command(name = "agent-session")]
    AgentSession(issue_agent_session::IssueAgentSessionArgs),
}

pub fn run(args: IssueArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <IssueArgs as clap::Args>::augment_args(clap::Command::new("issue"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        IssueCommand::Id(a) => issue_id::run(a),
        IssueCommand::Mine(a) => issue_mine::run(a),
        IssueCommand::Query(a) => issue_query::run(a),
        IssueCommand::Title(a) => issue_title::run(a),
        IssueCommand::Start(a) => issue_start::run(a),
        IssueCommand::View(a) => issue_view::run(a),
        IssueCommand::Url(a) => issue_url::run(a),
        IssueCommand::Describe(a) => issue_describe::run(a),
        IssueCommand::Commits(a) => issue_commits::run(a),
        IssueCommand::PullRequest(a) => issue_pull_request::run(a),
        IssueCommand::Archive(a) => issue_archive::run(a),
        IssueCommand::Delete(a) => issue_delete::run(a),
        IssueCommand::Create(a) => issue_create::run(a),
        IssueCommand::Update(a) => issue_update::run(a),
        IssueCommand::Comment(a) => issue_comment::run(a),
        IssueCommand::Attach(a) => issue_attach::run(a),
        IssueCommand::Link(a) => issue_link::run(a),
        IssueCommand::Relation(a) => issue_relation::run(a),
        IssueCommand::AgentSession(a) => issue_agent_session::run(a),
    }
}
