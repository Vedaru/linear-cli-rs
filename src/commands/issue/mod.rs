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
pub mod issue_subscribe;
pub mod issue_title;
pub mod issue_unarchive;
pub mod issue_unsubscribe;
pub mod issue_update;
pub mod issue_url;
pub mod issue_view;

use crate::errors::{CliError, Result};
use crate::linear;
use crate::output;
use crate::prompt;

/// `ENG-9: Title` from an issue payload, falling back to the identifier the
/// caller passed when the payload omits the fields.
pub(crate) fn issue_label(issue: &serde_json::Value, fallback: &str) -> String {
    let identifier = issue
        .get("identifier")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback);
    match issue.get("title").and_then(serde_json::Value::as_str) {
        Some(title) if !title.is_empty() => format!("{identifier}: {title}"),
        _ => identifier.to_string(),
    }
}

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
    /// Print the issue based on the current git branch
    Id(issue_id::IssueIdArgs),
    /// List your issues
    #[command(alias = "list", alias = "l")]
    Mine(issue_mine::IssueMineArgs),
    /// Query issues with structured filters
    #[command(alias = "q")]
    Query(issue_query::IssueQueryArgs),
    /// Print the issue title
    Title(issue_title::IssueTitleArgs),
    /// Start working on an issue
    Start(issue_start::IssueStartArgs),
    /// View issue details (default) or open in browser/app
    #[command(alias = "v")]
    View(issue_view::IssueViewArgs),
    /// Print the issue URL
    Url(issue_url::IssueUrlArgs),
    /// Print the issue title and Linear-issue trailer
    Describe(issue_describe::IssueDescribeArgs),
    /// Show all commits for a Linear issue (jj only)
    Commits(issue_commits::IssueCommitsArgs),
    /// Create a GitHub pull request with issue details
    #[command(name = "pull-request", alias = "pr")]
    PullRequest(issue_pull_request::IssuePullRequestArgs),
    /// Archive an issue
    Archive(issue_archive::IssueArchiveArgs),
    /// Delete an issue
    Delete(issue_delete::IssueDeleteArgs),
    /// Unarchive an issue: restore it from the archive or the trash (the API's
    /// issueUnarchive; upstream has no issue unarchive)
    Unarchive(issue_unarchive::IssueUnarchiveArgs),
    /// Subscribe to an issue (the API's issueSubscribe; upstream cannot follow
    /// an issue at all)
    Subscribe(issue_subscribe::IssueSubscribeArgs),
    /// Unsubscribe from an issue
    Unsubscribe(issue_unsubscribe::IssueUnsubscribeArgs),
    /// Create a linear issue
    Create(issue_create::IssueCreateArgs),
    /// Update a linear issue
    Update(issue_update::IssueUpdateArgs),
    /// Manage issue comments
    Comment(issue_comment::IssueCommentArgs),
    /// Create a sidebar link attachment on an issue (images do not render inline)
    Attach(issue_attach::IssueAttachArgs),
    /// Link a URL to an issue
    Link(issue_link::IssueLinkArgs),
    /// Manage issue relations (dependencies)
    Relation(issue_relation::IssueRelationArgs),
    /// Manage agent sessions for an issue
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
        IssueCommand::Unarchive(a) => issue_unarchive::run(a),
        IssueCommand::Subscribe(a) => issue_subscribe::run(a),
        IssueCommand::Unsubscribe(a) => issue_unsubscribe::run(a),
        IssueCommand::Create(a) => issue_create::run(a),
        IssueCommand::Update(a) => issue_update::run(a),
        IssueCommand::Comment(a) => issue_comment::run(a),
        IssueCommand::Attach(a) => issue_attach::run(a),
        IssueCommand::Link(a) => issue_link::run(a),
        IssueCommand::Relation(a) => issue_relation::run(a),
        IssueCommand::AgentSession(a) => issue_agent_session::run(a),
    }
}
