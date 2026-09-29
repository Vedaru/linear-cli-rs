//! `linear team autolinks` — configure GitHub repository autolinks for the
//! configured team prefix via the `gh` CLI.

use crate::config;
use crate::consts;
use crate::errors::{CliError, Result};
use crate::linear::get_team_key;
use crate::proc::{self, DEFAULT_TIMEOUT};

pub fn run() -> Result<()> {
    let Some(team_id) = get_team_key() else {
        return Err(
            CliError::validation("Could not determine team id from directory name")
                .suggestion("Run `linear config` to set a team."),
        );
    };

    let workspace = config::cli_workspace().or_else(config::workspace);
    let Some(workspace) = workspace else {
        return Err(CliError::validation(
            "workspace is not set via command line, configuration file, or environment",
        ));
    };

    let key_prefix = format!("key_prefix={team_id}-");
    let url_template = format!(
        "url_template={}/{}/issue/{}-<num>",
        consts::LINEAR_WEB_BASE_URL,
        workspace,
        team_id
    );
    let args = [
        "api",
        "repos/{owner}/{repo}/autolinks",
        "-f",
        key_prefix.as_str(),
        "-f",
        url_template.as_str(),
    ];

    // stdin/stdout/stderr are inherited so `gh` can prompt for auth exactly as
    // it would in a terminal, matching the upstream Deno.Command invocation.
    match proc::run_inherit("gh", &args, None, DEFAULT_TIMEOUT) {
        Some(true) => Ok(()),
        Some(false) => Err(CliError::cli("Failed to configure autolinks")),
        None => Err(CliError::cli("Failed to run gh")),
    }
}
