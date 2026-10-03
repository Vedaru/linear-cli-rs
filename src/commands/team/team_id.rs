//! `linear team id` — print the configured team id.

use serde_json::json;

use crate::errors::{CliError, Result};
use crate::linear::get_team_key;
use crate::output;

pub fn run(as_json: bool) -> Result<()> {
    match get_team_key()? {
        Some(team_id) => {
            if as_json {
                // The key, named for what it is: the configured value is a team
                // *key* (VED), not the team's UUID, and a caller that needs the
                // UUID asks `team list`.
                output::print_json(&json!({ "key": team_id }));
                return Ok(());
            }
            output::line(&team_id);
            Ok(())
        }
        None => Err(CliError::validation("No team id configured")
            .suggestion("Run `linear config` to set a team.")),
    }
}
