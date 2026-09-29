//! `linear team delete` — delete a Linear team, optionally moving its issues
//! to another team first.

use serde_json::{json, Map, Value};

use crate::errors::{handle_error, CliError, Result};
use crate::graphql::Client;
use crate::{graphql, output, prompt};

const GET_TEAM_DETAILS_QUERY: &str = r#"
  query GetTeamDetails($id: String!) {
    team(id: $id) {
      id
      key
      name
      issues {
        nodes {
          id
        }
      }
    }
  }
"#;

const GET_TEAM_ISSUES_FOR_MOVE_QUERY: &str = r#"
  query GetTeamIssuesForMove($teamId: String!, $first: Int, $after: String) {
    team(id: $teamId) {
      issues(first: $first, after: $after) {
        nodes {
          id
          identifier
        }
        pageInfo {
          hasNextPage
          endCursor
        }
      }
    }
  }
"#;

const DELETE_TEAM_MUTATION: &str = r#"
  mutation DeleteTeam($id: String!) {
    teamDelete(id: $id) {
      success
    }
  }
"#;

const MOVE_ISSUE_TO_TEAM_MUTATION: &str = r#"
  mutation MoveIssueToTeam($id: String!, $teamId: String!) {
    issueUpdate(id: $id, input: { teamId: $teamId }) {
      success
    }
  }
"#;

#[derive(clap::Args, Debug)]
pub struct DeleteArgs {
    /// Team key, name, or ID
    pub team: String,
    /// Move all issues to another team (key, name, or ID) before deletion
    #[arg(long)]
    pub move_issues: Option<String>,
    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    pub force: bool,
}

pub fn run(args: DeleteArgs) -> Result<()> {
    let client = graphql::client()?;

    // Resolve the team ID from the key, name, or ID.
    let team_id = crate::linear::resolve_team(&args.team)?.id;

    // Get team details for the confirmation message.
    let team_details = client.request(GET_TEAM_DETAILS_QUERY, json!({ "id": team_id }))?;
    let Some(team) = team_details.get("team").filter(|team| !team.is_null()) else {
        return Err(CliError::not_found("Team", &args.team));
    };

    let team_key = team.get("key").and_then(Value::as_str).unwrap_or("");
    let team_name = team.get("name").and_then(Value::as_str).unwrap_or("");
    let issue_count = team
        .get("issues")
        .and_then(|issues| issues.get("nodes"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);

    // If the team has issues, require --move-issues or prompt.
    if issue_count > 0 && args.move_issues.is_none() {
        output::line(&format!(
            "\n⚠️  Team {team_key} ({team_name}) has {issue_count} issue(s)."
        ));
        output::line("You must move these issues to another team before deletion.\n");

        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive selection required")
                .suggestion("Use --move-issues <teamKey> to specify target team."));
        }

        let all_teams = crate::linear::get_all_teams()?;
        let other_teams: Vec<_> = all_teams
            .into_iter()
            .filter(|team| team.id != team_id)
            .collect();

        if other_teams.is_empty() {
            return Err(CliError::cli("No other teams available to move issues to"));
        }

        let labels: Vec<String> = other_teams
            .iter()
            .map(|team| format!("{} ({})", team.name, team.key))
            .collect();
        let choice = prompt::select("Select a team to move issues to:", &labels)?;
        let target_team_id = other_teams[choice].id.clone();

        // Move all issues to the target team.
        if let Err(error) = move_issues_to_team(&client, &team_id, &target_team_id, issue_count) {
            handle_error(&error, Some("Failed to move issues"));
        }
    } else if issue_count > 0 && args.move_issues.is_some() {
        // Resolve the target team.
        let target_team_id = crate::linear::resolve_team(args.move_issues.as_deref().unwrap())?.id;

        if target_team_id == team_id {
            return Err(CliError::validation("Cannot move issues to the same team"));
        }

        if let Err(error) = move_issues_to_team(&client, &team_id, &target_team_id, issue_count) {
            handle_error(&error, Some("Failed to move issues"));
        }
    }

    // Confirm deletion.
    if !args.force {
        if !prompt::is_interactive() {
            return Err(CliError::validation("Interactive confirmation required")
                .suggestion("Use --force to skip."));
        }

        let confirmed = prompt::confirm(
            &format!("Are you sure you want to delete team \"{team_key}: {team_name}\"?"),
            false,
        )?;
        if !confirmed {
            output::line("Delete cancelled.");
            return Ok(());
        }
    }

    // Delete the team.
    let result = client.request(DELETE_TEAM_MUTATION, json!({ "id": team_id }))?;
    let success = result
        .get("teamDelete")
        .and_then(|delete| delete.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if !success {
        return Err(CliError::cli("Failed to delete team"));
    }

    output::line(&format!(
        "✓ Successfully deleted team: {team_key}: {team_name}"
    ));
    Ok(())
}

/// Fetch every issue in the source team and reassign each to the target team.
fn move_issues_to_team(
    client: &Client,
    source_team_id: &str,
    target_team_id: &str,
    issue_count: usize,
) -> Result<()> {
    let _ = issue_count;

    let mut variables = Map::new();
    variables.insert("teamId".to_string(), json!(source_team_id));
    variables.insert("first".to_string(), json!(100));
    let all_issues = client.paginate_connection(
        GET_TEAM_ISSUES_FOR_MOVE_QUERY,
        variables,
        &["team", "issues"],
    )?;

    let mut moved_count = 0usize;
    for issue in &all_issues {
        let Some(issue_id) = issue.get("id").and_then(Value::as_str) else {
            continue;
        };
        client.request(
            MOVE_ISSUE_TO_TEAM_MUTATION,
            json!({ "id": issue_id, "teamId": target_team_id }),
        )?;
        moved_count += 1;
    }

    output::line(&format!("✓ Moved {moved_count} issue(s) to target team"));
    Ok(())
}
