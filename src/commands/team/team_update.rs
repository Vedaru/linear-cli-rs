//! `linear team update` — rename a team, redescribe it, change its key or timezone, or move it
//! between public and private.
//!
//! The API's `teamUpdate`. Upstream's team group is create/list/delete only, so a team could be
//! made and destroyed but never corrected - a typo in a name, or a description that outlived the
//! work it described, was permanent from the CLI.
//!
//! Only the fields given are sent: the API's update input is partial, so an omitted field is
//! "leave it" rather than "clear it", and an invocation with nothing to change is refused here
//! rather than sent as an empty input that would report success for a no-op.

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::{CliError, Result};
use crate::{graphql, linear, output};

const UPDATE_TEAM_MUTATION: &str = r#"
mutation UpdateTeam($id: String!, $input: TeamUpdateInput!) {
  teamUpdate(id: $id, input: $input) {
    success
    team {
      id
      key
      name
      description
      private
      timezone
    }
  }
}
"#;

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Team key, name, or ID
    pub team: String,
    /// New name
    #[arg(short = 'n', long, value_name = "name")]
    pub name: Option<String>,
    /// New description (an empty string clears it)
    #[arg(short = 'd', long, value_name = "description")]
    pub description: Option<String>,
    /// New key. This is the prefix of every issue identifier in the team, so existing issues
    /// are renamed from it
    #[arg(short = 'k', long, value_name = "key")]
    pub key: Option<String>,
    /// Make the team private
    #[arg(long)]
    pub private: bool,
    /// Make the team public
    #[arg(long)]
    pub public: bool,
    /// New IANA timezone (e.g. Europe/Berlin)
    #[arg(long, value_name = "timezone")]
    pub timezone: Option<String>,
    /// Output as JSON
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: UpdateArgs) -> Result<()> {
    let nothing_to_do = args.name.is_none()
        && args.description.is_none()
        && args.key.is_none()
        && args.timezone.is_none()
        && !args.private
        && !args.public;
    if nothing_to_do {
        return Err(CliError::validation("Nothing to update").suggestion(
            "Pass --name, --description, --key, --timezone or --private/--public; the fields you leave out are left alone.",
        ));
    }
    if let Some(key) = &args.key {
        // Only the empty case is refused here: the API is the authority on a key's shape, and a
        // rule invented on this side would reject keys it accepts.
        if key.trim().is_empty() {
            return Err(CliError::validation("--key cannot be empty").suggestion(
                "Use letters and digits, e.g. `ENG`; every issue in the team is renamed from it.",
            ));
        }
    }

    let team = linear::resolve_team(&args.team)?;

    // `description` is sent even when empty - the API reads that as "clear it", which is a
    // different request from leaving the field out.
    let mut input = Map::new();
    if let Some(name) = &args.name {
        input.insert("name".to_string(), json!(name));
    }
    if let Some(description) = &args.description {
        input.insert("description".to_string(), json!(description));
    }
    if let Some(key) = &args.key {
        input.insert("key".to_string(), json!(key));
    }
    if let Some(timezone) = &args.timezone {
        input.insert("timezone".to_string(), json!(timezone));
    }
    if args.private || args.public {
        input.insert("private".to_string(), json!(args.private));
    }

    let client = graphql::client()?;
    let document = client.request(
        UPDATE_TEAM_MUTATION,
        json!({ "id": team.id, "input": Value::Object(input) }),
    )?;

    let updated = document
        .get("teamUpdate")
        .ok_or_else(|| CliError::cli("Linear API response did not contain teamUpdate"))?;
    if updated.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::cli("Failed to update team"));
    }
    let node = updated.get("team").cloned().unwrap_or(Value::Null);

    if args.json {
        output::print_json(&document);
        return Ok(());
    }

    let key = node
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or(team.key.as_str());
    let name = node.get("name").and_then(Value::as_str).unwrap_or("");
    output::line(&format!("✓ Updated team {key}: {name}"));

    // The key is resolved back from the API's answer, not echoed from the input: a create or a
    // rename can land on a key other than the one asked for, and a script should read what it got.
    let mut changed: Vec<&str> = Vec::new();
    if args.name.is_some() {
        changed.push("name");
    }
    if args.description.is_some() {
        changed.push("description");
    }
    if args.key.is_some() {
        changed.push("key");
    }
    if args.timezone.is_some() {
        changed.push("timezone");
    }
    if args.private || args.public {
        changed.push(if args.private {
            "visibility (private)"
        } else {
            "visibility (public)"
        });
    }
    output::line(&format!("  Changed: {}", changed.join(", ")));
    Ok(())
}
