//! `linear issue agent-session` — port of `src/commands/issue/issue-agent-session*.ts`.
//!
//! `list` and `view`. The group itself has no action; with no subcommand it
//! prints help, matching upstream's `this.showHelp()`. `view` is aliased `v`.

use std::io::IsTerminal;

use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::colors;
use crate::display;
use crate::errors::{CliError, Result};
use crate::hyperlink;
use crate::linear;
use crate::{graphql, output};

#[derive(Args, Debug)]
pub struct IssueAgentSessionArgs {
    #[command(subcommand)]
    pub command: Option<AgentSessionCommand>,
}

#[derive(Subcommand, Debug)]
pub enum AgentSessionCommand {
    /// List agent sessions for an issue
    List(AgentSessionListArgs),
    /// View agent session details
    #[command(alias = "v")]
    View(AgentSessionViewArgs),
}

const AGENT_SESSION_STATUSES: [&str; 6] = [
    "pending",
    "active",
    "complete",
    "awaitingInput",
    "error",
    "stale",
];

#[derive(Args, Debug)]
pub struct AgentSessionListArgs {
    /// Issue ID (e.g., ENG-123)
    #[arg(value_name = "issueId")]
    pub issue_id: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
    /// Filter by session status
    #[arg(
        long = "status",
        value_name = "status",
        value_parser = clap::builder::PossibleValuesParser::new(AGENT_SESSION_STATUSES)
    )]
    pub status: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentSessionViewArgs {
    /// Agent session ID
    #[arg(value_name = "sessionId")]
    pub session_id: String,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

const GET_ISSUE_AGENT_SESSIONS_QUERY: &str = r#"
query GetIssueAgentSessions($issueId: String!) {
  issue(id: $issueId) {
    comments(first: 100) {
      nodes {
        agentSession {
          id
          status
          type
          createdAt
          startedAt
          endedAt
          summary
          creator {
            name
          }
          appUser {
            name
          }
        }
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

const GET_AGENT_SESSION_DETAILS_QUERY: &str = r#"
query GetAgentSessionDetails($id: String!) {
  agentSession(id: $id) {
    id
    status
    type
    createdAt
    updatedAt
    startedAt
    endedAt
    dismissedAt
    summary
    externalLink
    creator {
      name
    }
    appUser {
      name
    }
    dismissedBy {
      name
    }
    issue {
      identifier
      title
      url
    }
    activities(first: 20) {
      nodes {
        id
        createdAt
        content {
          ... on AgentActivityThoughtContent {
            type
            body
          }
          ... on AgentActivityActionContent {
            type
            action
            parameter
            result
          }
          ... on AgentActivityResponseContent {
            type
            body
          }
          ... on AgentActivityPromptContent {
            type
            body
          }
          ... on AgentActivityErrorContent {
            type
            body
          }
          ... on AgentActivityElicitationContent {
            type
            body
          }
        }
      }
    }
  }
}
"#;

pub fn run(args: IssueAgentSessionArgs) -> Result<()> {
    let Some(command) = args.command else {
        let mut cmd = <IssueAgentSessionArgs as clap::Args>::augment_args(clap::Command::new(
            "agent-session",
        ));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        AgentSessionCommand::List(a) => {
            list_sessions(a).map_err(|error| error.with_context("Failed to list agent sessions"))
        }
        AgentSessionCommand::View(a) => view_session(a)
            .map_err(|error| error.with_context("Failed to fetch agent session details")),
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

/// `formatStatus`: colour by state, padded to 13 display columns.
fn format_status(status: &str) -> String {
    const WIDTH: usize = 13;
    match status {
        "active" => colors::green(&display::pad_display("active", WIDTH)),
        "pending" => colors::yellow(&display::pad_display("pending", WIDTH)),
        "awaitingInput" => colors::yellow(&display::pad_display("awaitingInput", WIDTH)),
        "complete" => colors::muted(&display::pad_display("complete", WIDTH)),
        "error" => display::pad_display("error", WIDTH),
        "stale" => colors::muted(&display::pad_display("stale", WIDTH)),
        other => display::pad_display(other, WIDTH),
    }
}

/// `formatDate`: the first ten characters of an ISO timestamp (the date part).
fn format_date(date_string: &str) -> String {
    date_string.chars().take(10).collect()
}

fn list_sessions(args: AgentSessionListArgs) -> Result<()> {
    let Some(resolved_identifier) = linear::get_issue_identifier(args.issue_id.as_deref())? else {
        return Err(CliError::validation("Could not determine issue ID")
            .suggestion("Please provide an issue ID like 'ENG-123'."));
    };

    let client = graphql::client()?;
    let result = client.request(
        GET_ISSUE_AGENT_SESSIONS_QUERY,
        json!({ "issueId": resolved_identifier }),
    )?;

    // `result.issue?.comments ?? { nodes: [], pageInfo: { hasNextPage: false, endCursor: null } }`
    let comments = result
        .get("issue")
        .filter(|value| !value.is_null())
        .and_then(|issue| issue.get("comments"))
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "nodes": [],
                "pageInfo": { "hasNextPage": false, "endCursor": null },
            })
        });

    let nodes: Vec<Value> = comments
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut sessions: Vec<Value> = nodes
        .iter()
        .filter_map(|comment| comment.get("agentSession"))
        .filter(|session| !session.is_null())
        .cloned()
        .collect();

    // JSON output spreads the original connection and, when filtering, replaces
    // nodes with only those whose session matches the requested status.
    let json_comments = match &args.status {
        Some(status) => {
            let filtered: Vec<Value> = nodes
                .iter()
                .filter(|comment| {
                    comment
                        .get("agentSession")
                        .and_then(|session| session.get("status"))
                        .and_then(Value::as_str)
                        == Some(status.as_str())
                })
                .cloned()
                .collect();
            let mut object = comments.clone();
            if let Value::Object(ref mut map) = object {
                map.insert("nodes".to_string(), Value::Array(filtered));
            }
            object
        }
        None => comments.clone(),
    };

    if let Some(status) = &args.status {
        sessions
            .retain(|session| session.get("status").and_then(Value::as_str) == Some(status.as_str()));
    }

    if args.json {
        output::print_json(&json_comments);
        return Ok(());
    }

    if sessions.is_empty() {
        output::line("No agent sessions found for this issue.");
        return Ok(());
    }

    let columns = terminal_columns();

    const STATUS_WIDTH: usize = 13;
    const DATE_WIDTH: usize = 10;
    let agent_width = 5usize.max(
        sessions
            .iter()
            .map(|session| display_width_of(session, "appUser"))
            .max()
            .unwrap_or(0),
    );
    const SPACE_WIDTH: usize = 3;

    let fixed = STATUS_WIDTH + DATE_WIDTH + agent_width + SPACE_WIDTH;
    const PADDING: usize = 1;
    let available_width = columns.saturating_sub(PADDING + fixed).max(10);

    let header_cells = [
        display::pad_display("STATUS", STATUS_WIDTH),
        display::pad_display("AGENT", agent_width),
        display::pad_display("CREATED", DATE_WIDTH),
        "SUMMARY".to_string(),
    ];
    output::line(&colors::header(&header_cells.join(" ")));

    for session in &sessions {
        let summary_text = match session
            .get("summary")
            .and_then(Value::as_str)
            .filter(|summary| !summary.is_empty())
        {
            Some(summary) => display::truncate_text(&summary.replace('\n', " "), available_width),
            None => colors::muted("--"),
        };

        let line = format!(
            "{} {} {} {}",
            format_status(session.get("status").and_then(Value::as_str).unwrap_or("")),
            display::pad_display(
                session
                    .get("appUser")
                    .and_then(|user| user.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                agent_width,
            ),
            display::pad_display(
                &format_date(session.get("createdAt").and_then(Value::as_str).unwrap_or("")),
                DATE_WIDTH,
            ),
            summary_text,
        );
        output::line(&line);
    }

    // Upstream starts a spinner on stderr while `shouldShowSpinner()` holds; the
    // call keeps that guard wired even though this build prints no spinner.
    let _ = hyperlink::should_show_spinner();

    Ok(())
}

// ---------------------------------------------------------------------------
// view
// ---------------------------------------------------------------------------

fn view_session(args: AgentSessionViewArgs) -> Result<()> {
    crate::linear_url::reject_linear_url(&args.session_id, "an agent session ID")?;

    let client = graphql::client()?;
    let result = client.request(
        GET_AGENT_SESSION_DETAILS_QUERY,
        json!({ "id": args.session_id }),
    )?;

    let Some(session) = result.get("agentSession").filter(|value| !value.is_null()) else {
        return Err(CliError::not_found("Agent session", &args.session_id));
    };

    if args.json {
        output::print_json(session);
        return Ok(());
    }

    let field = |name: &str| -> String {
        session
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let nested = |object: &str, field_name: &str| -> Option<String> {
        session
            .get(object)
            .filter(|value| !value.is_null())
            .and_then(|value| value.get(field_name))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };

    let mut lines: Vec<String> = Vec::new();

    lines.push("# Agent Session".to_string());
    lines.push(String::new());

    lines.push(format!("**ID:** {}", field("id")));
    lines.push(format!("**Status:** {}", field("status")));
    lines.push(format!("**Type:** {}", field("type")));
    lines.push(format!(
        "**Agent:** {}",
        nested("appUser", "name").unwrap_or_default()
    ));

    if let Some(creator) = nested("creator", "name") {
        lines.push(format!("**Creator:** {creator}"));
    }

    if let Some(issue) = session.get("issue").filter(|value| !value.is_null()) {
        let identifier = issue.get("identifier").and_then(Value::as_str).unwrap_or("");
        let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
        lines.push(format!("**Issue:** {identifier} - {title}"));
    }

    lines.push(String::new());
    lines.push(format!(
        "**Created:** {}",
        display::format_relative_time(&field("createdAt"))
    ));
    for (label, key) in [("Started", "startedAt"), ("Ended", "endedAt")] {
        if let Some(value) = session
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            lines.push(format!(
                "**{label}:** {}",
                display::format_relative_time(value)
            ));
        }
    }
    if let Some(dismissed) = session
        .get("dismissedAt")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!(
            "**Dismissed:** {}",
            display::format_relative_time(dismissed)
        ));
        if let Some(by) = nested("dismissedBy", "name") {
            lines.push(format!("**Dismissed by:** {by}"));
        }
    }

    if let Some(link) = session
        .get("externalLink")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        lines.push(String::new());
        lines.push(format!("**External Link:** {link}"));
    }

    if let Some(summary) = session
        .get("summary")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        lines.push(String::new());
        lines.push("## Summary".to_string());
        lines.push(String::new());
        lines.push(summary.to_string());
    }

    let activities: Vec<Value> = session
        .get("activities")
        .and_then(|activities| activities.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if !activities.is_empty() {
        lines.push(String::new());
        lines.push("## Activities".to_string());
        lines.push(String::new());
        for activity in &activities {
            let time = display::format_relative_time(
                activity.get("createdAt").and_then(Value::as_str).unwrap_or(""),
            );
            let content = activity.get("content").cloned().unwrap_or(Value::Null);
            let activity_type = content
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let mut detail = String::new();
            if let Some(body) = content
                .get("body")
                .and_then(Value::as_str)
                .filter(|body| !body.is_empty())
            {
                detail = format!(" - {}", body.replace('\n', " "));
            } else if let Some(action) = content
                .get("action")
                .and_then(Value::as_str)
                .filter(|action| !action.is_empty())
            {
                let parameter = content.get("parameter").and_then(Value::as_str).unwrap_or("");
                detail = format!(" - {action}: {parameter}");
            }
            lines.push(format!("- **{activity_type}** ({time}){detail}"));
        }
    }

    // Known deviation: there is no terminal markdown renderer equivalent to
    // `@littletof/charmd`, so the non-TTY raw-markdown path is used throughout.
    output::line(&lines.join("\n"));
    Ok(())
}

fn display_width_of(session: &Value, object: &str) -> usize {
    display::display_width(
        session
            .get(object)
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(""),
    )
}

fn terminal_columns() -> usize {
    if std::io::stdout().is_terminal() {
        if let Some((width, _)) = terminal_size::terminal_size() {
            return width.0 as usize;
        }
    }
    120
}
