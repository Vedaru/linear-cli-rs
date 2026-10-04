//! Port of `src/utils/actions.ts` — open Linear resources in the browser or
//! the Linear desktop app.
//!
//! Upstream prints its "Opening …" line itself; the Rust port does the same so
//! callers only branch on `--web` / `--app`.

use crate::config;
use crate::consts;
use crate::errors::{CliError, Result};
use crate::linear;
use crate::output;
use crate::proc::{self, RunOptions, DEFAULT_TIMEOUT};
use crate::vcs;

/// Open the issue page for `provided_id` (an identifier, URL, or the current
/// branch's issue) in the web browser, or the Linear desktop app when `app`.
pub fn open_issue_page(provided_id: Option<&str>, app: bool) -> Result<()> {
    let Some(issue_id) = linear::get_issue_identifier(provided_id)? else {
        return Err(CliError::cli(vcs::get_no_issue_found_message()));
    };

    let workspace = workspace_slug()?;

    let url = format!(
        "{}/{}/issue/{}",
        consts::LINEAR_WEB_BASE_URL,
        workspace,
        issue_id
    );
    let destination = if app { "Linear.app" } else { "web browser" };
    output::line(&format!("Opening {url} in {destination}"));
    open_url(&url, app)
}

/// Open a project's page in the web browser, or the Linear desktop app when
/// `app`. Port of `openProjectPage`.
pub fn open_project_page(project_id: &str, app: bool) -> Result<()> {
    let workspace = workspace_slug()?;

    let url = format!(
        "{}/{}/project/{}",
        consts::LINEAR_WEB_BASE_URL,
        workspace,
        project_id
    );
    let destination = if app { "Linear.app" } else { "web browser" };
    output::line(&format!("Opening {url} in {destination}"));
    open_url(&url, app)
}

/// Open the team's "active issues assigned to me" view in the browser or app.
///
/// Port of `openTeamAssigneeView`: the pre-set filter is the JSON
/// `{"and":[{"assignee":{"or":[{"isMe":{"eq":true}}]}}]}` base64-encoded with
/// padding stripped and appended as a `filter` query parameter. Unlike
/// [`open_issue_page`], upstream prints no "Opening …" line, so neither does
/// this.
pub fn open_team_assignee_view(app: bool) -> Result<()> {
    let Some(team_id) = linear::get_team_key()? else {
        return Err(CliError::validation(
            "Could not determine team id from configuration or directory name.",
        ));
    };

    let workspace = workspace_slug()?;

    let filter_obj = serde_json::json!({
        "and": [{ "assignee": { "or": [{ "isMe": { "eq": true } }] } }],
    });
    let filter = base64_encode(filter_obj.to_string().as_bytes());
    let url = format!(
        "{}/{}/team/{}/active?filter={}",
        consts::LINEAR_WEB_BASE_URL,
        workspace,
        team_id,
        filter
    );
    open_url(&url, app)
}

/// Standard-alphabet base64 with `=` padding stripped, matching upstream's
/// `encodeBase64(...).replace(/=/g, "")`.
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 63) as usize] as char);
        out.push(TABLE[((triple >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 63) as usize] as char);
        }
    }
    out
}

/// The workspace slug for a `linear.app` URL: the configured one when there is
/// one, otherwise the one Linear reports for the key - so `--web`/`-a` work in a
/// single-workspace setup that names no workspace.
fn workspace_slug() -> Result<String> {
    if let Some(workspace) = config::cli_workspace().or_else(config::workspace) {
        return Ok(workspace);
    }
    if let Some(workspace) = linear::workspace_url_key()? {
        return Ok(workspace);
    }
    Err(CliError::validation(
        "workspace is not set via command line, configuration file, or environment",
    )
    .suggestion("Pass --workspace <slug>, or add `workspace = \"<slug>\"` to linear.toml."))
}

/// Open a URL with the platform opener. `app` asks for the Linear desktop app
/// where the platform supports it, mirroring `@opensrc/deno-open`.
pub fn open_url(url: &str, app: bool) -> Result<()> {
    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        if app {
            ("open", vec!["-a", "Linear", url])
        } else {
            ("open", vec![url])
        }
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", "", url])
    } else if app {
        ("linear", vec![url])
    } else {
        ("xdg-open", vec![url])
    };

    match proc::run(program, &args, &RunOptions::default(), DEFAULT_TIMEOUT) {
        Some(output) if output.success => Ok(()),
        Some(_) => Err(CliError::cli(format!("Failed to open {url}"))),
        None => Err(CliError::cli(format!(
            "Failed to open {url}: `{program}` is not available"
        ))),
    }
}
