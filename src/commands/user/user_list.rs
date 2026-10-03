//! `linear user list` — port of `src/commands/user/user-list.ts`.
//!
//! The workspace member listing reads the whole organization rather than one
//! team, so it embeds its own query (`viewer.organization.users`) instead of
//! reusing the team-scoped helper in `linear::members`. It selects every field
//! the renderer and `--json` output need, including `active`, which the shared
//! helper does not fetch.

use clap::Args;
use serde_json::{json, Map, Value};

use crate::errors::Result;
use crate::{display, graphql, output};

// One definition, in `linear/queries.rs`; see `team_members` for the same collapse. The shared
// member lookup reads the same document through the same path, so the two cannot disagree about
// which fields a member has.
const GET_ORGANIZATION_MEMBERS_QUERY: &str = crate::linear::GET_ORGANIZATION_MEMBERS_QUERY;

#[derive(Args, Debug)]
pub struct UserListArgs {
    /// Include inactive members
    #[arg(short = 'a', long)]
    pub all: bool,
    /// Output as JSON; a member's url mentions them when pasted into Markdown. This searches the whole workspace — prefer `linear team members <TEAM>`, and confirm before mentioning someone outside the team
    #[arg(short = 'j', long)]
    pub json: bool,
}

pub fn run(args: UserListArgs) -> Result<()> {
    let client = graphql::client()?;

    let mut variables = Map::new();
    variables.insert("includeDisabled".to_string(), json!(args.all));
    variables.insert("first".to_string(), json!(100));

    let (nodes, page_info) = client.paginate_connection_page(
        GET_ORGANIZATION_MEMBERS_QUERY,
        variables,
        &["viewer", "organization", "users"],
    )?;

    let members: Vec<Value> = if args.all {
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
        output::line("No members found in this workspace.");
        return Ok(());
    }

    if members.is_empty() {
        output::line(
            "No active members found in this workspace. Use --all to include inactive members.",
        );
        return Ok(());
    }

    display::print_members(&members, "Workspace Members");
    Ok(())
}
