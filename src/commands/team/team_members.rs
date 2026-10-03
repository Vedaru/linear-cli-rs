//! `linear team members` — list a team's members.
//!
//! Port of `src/commands/team/team-members.ts` plus the inline query from
//! `getTeamMembers` in `src/utils/linear.ts`. The query is duplicated here
//! rather than calling the shared `linear::get_team_members` because that
//! helper does not accept `includeDisabled` and does not select `active`,
//! both of which this command needs.

use serde_json::{json, Map, Value};

use crate::display;
use crate::errors::{CliError, Result};
use crate::graphql;
use crate::linear::{get_team_key, resolve_team};
use crate::output;

const GET_TEAM_MEMBERS_QUERY: &str = r#"
  query GetTeamMembers(
    $teamKey: String!
    $includeDisabled: Boolean!
    $first: Int
    $after: String
  ) {
    team(id: $teamKey) {
      members(
        includeDisabled: $includeDisabled
        first: $first
        after: $after
      ) {
        nodes {
          id
          name
          displayName
          email
          active
          initials
          description
          timezone
          lastSeen
          statusEmoji
          statusLabel
          guest
          isAssignable
          admin
          owner
          isMe
          url
          # The shared copy in `linear/queries.rs` fetched this and the command copy did not, so
          # collapsing the two in either direction used to lose a field. Added here first: the
          # collapse is VED-111, and it must not be a commit that breaks a caller on the way.
          avatarUrl
        }
        pageInfo {
          hasNextPage
          endCursor
        }
      }
    }
  }
"#;

#[derive(clap::Args, Debug)]
pub struct MembersArgs {
    /// Team key, name, or ID (defaults to the configured team)
    pub team: Option<String>,
    /// Include inactive members
    #[arg(short = 'a', long)]
    pub all: bool,
    /// Output as JSON; a member's url mentions them when pasted into Markdown
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: MembersArgs) -> Result<()> {
    let resolved_team_key = match &args.team {
        Some(team) => resolve_team(team)?.key,
        None => get_team_key()?.ok_or_else(|| {
            CliError::validation("Could not determine team key from directory name")
                .suggestion("Please specify a team key, name, or ID as an argument.")
        })?,
    };

    let include_disabled = args.all;
    let (nodes, page_info) = get_team_members(&resolved_team_key, include_disabled)?;

    // --json is an output format, not a raw dump: it must respect --all just
    // as the human output does.
    let members: Vec<Value> = if include_disabled {
        nodes.clone()
    } else {
        nodes
            .iter()
            .filter(|member| {
                member
                    .get("active")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    };

    if args.json {
        output::print_json(&json!({ "nodes": members, "pageInfo": page_info }));
        return Ok(());
    }

    if nodes.is_empty() {
        output::line("No members found for this team.");
        return Ok(());
    }

    if members.is_empty() {
        output::line(
            "No active members found for this team. Use --all to include inactive members.",
        );
        return Ok(());
    }

    display::print_members(&members, "Team Members");
    Ok(())
}

/// Port of `getTeamMembers`: paginate `team.members`, guarding against a
/// non-advancing cursor, then sort globally by `displayName` case-insensitively.
fn get_team_members(team_key: &str, include_disabled: bool) -> Result<(Vec<Value>, Value)> {
    let client = graphql::client()?;

    let mut nodes: Vec<Value> = Vec::new();
    // Describes the exhausted source connection, so hasNextPage is always false
    // once pagination completes. Matches label list and project list.
    let mut page_info = json!({ "hasNextPage": false, "endCursor": null });
    let mut has_next_page = true;
    let mut after: Option<String> = None;

    while has_next_page {
        let mut variables = Map::new();
        variables.insert("teamKey".to_string(), json!(team_key));
        variables.insert("includeDisabled".to_string(), json!(include_disabled));
        variables.insert("first".to_string(), json!(100)); // Fetch 100 members per page
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }

        let data = client.request(GET_TEAM_MEMBERS_QUERY, Value::Object(variables))?;
        let members = data
            .get("team")
            .and_then(|team| team.get("members"))
            .ok_or_else(|| CliError::cli("Failed to fetch team members"))?;

        if let Some(page_nodes) = members.get("nodes").and_then(Value::as_array) {
            nodes.extend(page_nodes.iter().cloned());
        }

        let current = members
            .get("pageInfo")
            .cloned()
            .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));
        page_info = current;

        has_next_page = page_info
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let next_cursor = page_info
            .get("endCursor")
            .and_then(Value::as_str)
            .map(str::to_string);

        if has_next_page && (next_cursor.is_none() || next_cursor == after) {
            return Err(CliError::cli(
                "Linear reported more team members but did not advance the page cursor",
            ));
        }
        after = next_cursor;
    }

    // Sort after all pages are fetched so ordering is global, not per-page.
    nodes.sort_by_key(|node| {
        node.get("displayName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase()
    });

    Ok((nodes, page_info))
}
