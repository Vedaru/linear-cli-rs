//! `linear team id` — print the configured team id.

use crate::errors::{CliError, Result};
use crate::linear::get_team_key;
use crate::output;

pub fn run() -> Result<()> {
    match get_team_key() {
        Some(team_id) => {
            output::line(&team_id);
            Ok(())
        }
        None => Err(
            CliError::validation("No team id configured")
                .suggestion("Run `linear config` to set a team."),
        ),
    }
}
