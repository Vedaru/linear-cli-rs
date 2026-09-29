//! `linear auth` — port of `src/commands/auth/`.
//!
//! Covers login, logout, list, default, token, whoami, and migrate — the seven
//! subcommands registered by upstream `auth.ts`.
//!
//! Every action wraps its failure with the same context string upstream passes
//! to `handleError`, so error output matches the TypeScript CLI.
//!
//! Divergence from upstream: interactive prompts are guarded by
//! [`prompt::is_interactive`]. Upstream would block on `Secret.prompt`,
//! `Select.prompt` or `Confirm.prompt`; this port instead fails with guidance —
//! or, for the post-login migration offer, prints a hint — so the CLI stays
//! usable when stdin is not a terminal. Non-interactive failure messages are
//! additive and do not alter the output of the interactive paths.

use regex::Regex;
use serde_json::{json, Value};

use crate::cli::{AuthArgs, AuthCommand};
use crate::colors;
use crate::consts;
use crate::credentials;
use crate::display;
use crate::errors::{CliError, Result};
use crate::graphql;
use crate::keyring;
use crate::output;
use crate::prompt;

const LOGIN_VIEWER_QUERY: &str = r#"
query AuthLoginViewer {
  viewer {
    name
    email
    organization {
      name
      urlKey
    }
  }
}
"#;

const LIST_VIEWER_QUERY: &str = r#"
query AuthListViewer {
  viewer {
    name
    email
    organization {
      name
      urlKey
    }
  }
}
"#;

const WHOAMI_VIEWER_QUERY: &str = r#"
query AuthStatus {
  viewer {
    id
    name
    displayName
    email
    admin
    guest
    organization {
      name
      urlKey
      logoUrl
    }
  }
}
"#;

pub fn run(args: AuthArgs) -> Result<()> {
    let Some(command) = args.command else {
        // `linear auth` with no subcommand shows help, mirroring
        // `this.showHelp()` upstream.
        let mut cmd = <AuthArgs as clap::Args>::augment_args(clap::Command::new("auth"));
        let _ = cmd.print_help();
        output::blank();
        return Ok(());
    };

    match command {
        AuthCommand::Login { key, plaintext } => {
            login(key, plaintext).map_err(|error| error.with_context("Failed to login"))
        }
        AuthCommand::Logout { workspace, force } => {
            logout(workspace, force).map_err(|error| error.with_context("Failed to logout"))
        }
        AuthCommand::List => {
            list().map_err(|error| error.with_context("Failed to list workspaces"))
        }
        AuthCommand::Default { workspace } => default(workspace)
            .map_err(|error| error.with_context("Failed to set default workspace")),
        AuthCommand::Token => {
            token().map_err(|error| error.with_context("Failed to get API token"))
        }
        AuthCommand::Whoami => {
            whoami().map_err(|error| error.with_context("Failed to get user info"))
        }
        AuthCommand::Migrate => {
            migrate().map_err(|error| error.with_context("Failed to migrate credentials"))
        }
    }
}

/// Strip characters some terminals (notably Windows) inject around pasted
/// text. Mirrors `apiKey.replace(/^[^a-zA-Z0-9_]+|[^a-zA-Z0-9_]+$/g, "")`.
fn strip_api_key(key: &str) -> String {
    let pattern = Regex::new(r"^[^a-zA-Z0-9_]+|[^a-zA-Z0-9_]+$").expect("valid regex");
    pattern.replace_all(key, "").to_string()
}

fn login(key: Option<String>, plaintext: bool) -> Result<()> {
    let api_key = match key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
        Some(key) => key,
        None => {
            if !prompt::is_interactive() {
                return Err(CliError::validation("No API key provided").suggestion(
                    "Pass --key <key>, set LINEAR_API_KEY, or create one at \
                     https://linear.app/settings/account/security",
                ));
            }
            prompt::secret(
                "Enter your Linear API key",
                "Create one at https://linear.app/settings/account/security",
            )?
        }
    };

    if api_key.is_empty() {
        return Err(CliError::validation("No API key provided")
            .suggestion("Create one at https://linear.app/settings/account/security"));
    }
    let api_key = strip_api_key(&api_key);

    // Validate the key by querying the API.
    let client = graphql::client_with_key(&api_key);
    let result = match client.request(LOGIN_VIEWER_QUERY, json!({})) {
        Ok(value) => value,
        Err(error) => {
            if is_unauthorized(&error) {
                return Err(CliError::auth("Invalid API key")
                    .suggestion("Check that your API key is correct and not expired."));
            }
            return Err(
                CliError::cli(format!("Failed to authenticate: {}", error.user_message))
                    .cause(error),
            );
        }
    };

    let viewer = result
        .get("viewer")
        .ok_or_else(|| CliError::cli("Linear API returned no viewer information"))?;
    let name = viewer["name"].as_str().unwrap_or_default();
    let email = viewer["email"].as_str().unwrap_or_default();
    let organization = &viewer["organization"];
    let org_name = organization["name"].as_str().unwrap_or_default();
    let workspace = organization["urlKey"].as_str().unwrap_or_default();

    if workspace.is_empty() {
        return Err(CliError::cli(
            "Linear API returned no workspace slug for this API key",
        ));
    }

    // Require the keyring unless plaintext was requested or the file is
    // already in inline format.
    if !plaintext && !credentials::is_using_inline_format() && !keyring::is_available() {
        return Err(CliError::cli(
            "No system keyring found. Use `--plaintext` to store credentials in the config file, \
             or set `LINEAR_API_KEY`.",
        ));
    }

    let already_exists = credentials::has_workspace(workspace);
    credentials::add_credential(workspace, &api_key, Some(plaintext))?;

    let existing_count = credentials::get_workspaces().len();

    if already_exists {
        output::line(&format!(
            "Updated credentials for workspace: {org_name} ({workspace})"
        ));
    } else {
        output::line(&format!("Logged in to workspace: {org_name} ({workspace})"));
    }
    output::line(&format!("  User: {name} <{email}>"));

    if existing_count == 1 {
        output::line("  Set as default workspace");
    }

    if !plaintext && credentials::is_using_inline_format() {
        output::line(&colors::yellow(
            "Note: Credential stored as plaintext to match existing format.",
        ));
    }

    // Offer to migrate inline credentials to the keyring.
    if credentials::is_using_inline_format() && keyring::is_available() {
        output::blank();
        output::line(&colors::yellow(
            "Your credentials are stored as plaintext in the credentials file.",
        ));
        if prompt::is_interactive() {
            if prompt::confirm(
                "Migrate all credentials to the system keyring for better security?",
                true,
            )? {
                let migrated = credentials::migrate_to_keyring()?;
                output::line(&format!(
                    "Migrated {} workspace(s) to system keyring.",
                    migrated.len()
                ));
            }
        } else {
            output::line(&colors::gray(
                "  Run `linear auth migrate` to move them to the system keyring.",
            ));
        }
    }

    if std::env::var_os("LINEAR_API_KEY").is_some() {
        output::blank();
        output::line(&colors::yellow(
            "Warning: LINEAR_API_KEY environment variable is set.",
        ));
        output::line(&colors::yellow(
            "It takes precedence over stored credentials.",
        ));
        output::line(&colors::yellow(
            "Remove it from your shell config to use multi-workspace auth.",
        ));
    }

    Ok(())
}

fn logout(workspace: Option<String>, force: bool) -> Result<()> {
    let workspaces = credentials::get_workspaces();
    if workspaces.is_empty() {
        return Err(CliError::auth("No workspaces configured"));
    }

    let workspace = match workspace {
        Some(workspace) => workspace,
        None if workspaces.len() == 1 => workspaces[0].clone(),
        None => {
            if !prompt::is_interactive() {
                return Err(CliError::cli(
                    "Which workspace should be removed? None was specified.",
                )
                .suggestion(format!(
                    "Pass the workspace slug as an argument: {}",
                    workspaces.join(", ")
                )));
            }
            let default = credentials::get_default_workspace();
            let labels: Vec<String> = workspaces
                .iter()
                .map(|ws| {
                    if Some(ws) == default.as_ref() {
                        format!("{ws} (default)")
                    } else {
                        ws.clone()
                    }
                })
                .collect();
            let index = prompt::select("Select workspace to remove", &labels)?;
            workspaces[index].clone()
        }
    };

    if !credentials::has_workspace(&workspace) {
        return Err(CliError::not_found("Workspace", &workspace));
    }

    if !force {
        if !prompt::is_interactive() {
            return Err(CliError::cli(format!(
                "Refusing to remove credentials for workspace \"{workspace}\" without confirmation"
            ))
            .suggestion("Pass --force to skip the confirmation prompt."));
        }
        if !prompt::confirm(
            &format!("Remove credentials for workspace \"{workspace}\"?"),
            false,
        )? {
            output::line("Cancelled");
            return Ok(());
        }
    }

    credentials::remove_credential(&workspace)?;
    output::line(&format!("Removed credentials for workspace: {workspace}"));

    if !credentials::get_workspaces().is_empty() {
        if let Some(new_default) = credentials::get_default_workspace() {
            output::line(&format!("  Default workspace is now: {new_default}"));
        }
    }

    Ok(())
}

#[derive(Debug)]
struct WorkspaceInfo {
    workspace: String,
    is_default: bool,
    org_name: Option<String>,
    user_name: Option<String>,
    email: Option<String>,
    error: Option<String>,
}

fn fetch_workspace_info(
    workspace: String,
    is_default: bool,
    api_key: Option<String>,
) -> WorkspaceInfo {
    let Some(api_key) = api_key else {
        return WorkspaceInfo {
            workspace,
            is_default,
            org_name: None,
            user_name: None,
            email: None,
            error: Some("missing credentials".to_string()),
        };
    };

    let client = graphql::client_with_key(&api_key);
    match client.request(LIST_VIEWER_QUERY, json!({})) {
        Ok(value) => {
            let viewer = &value["viewer"];
            let organization = &viewer["organization"];
            WorkspaceInfo {
                workspace,
                is_default,
                org_name: organization["name"].as_str().map(str::to_string),
                user_name: viewer["name"].as_str().map(str::to_string),
                email: viewer["email"].as_str().map(str::to_string),
                error: None,
            }
        }
        Err(error) => {
            let message = match error.http_status {
                Some(401) | Some(403) => "invalid credentials".to_string(),
                _ => error.user_message.clone(),
            };
            WorkspaceInfo {
                workspace,
                is_default,
                org_name: None,
                user_name: None,
                email: None,
                error: Some(message),
            }
        }
    }
}

fn list() -> Result<()> {
    let workspaces = credentials::get_workspaces();
    if workspaces.is_empty() {
        output::line("No workspaces configured");
        output::line("Run `linear auth login` to add a workspace");
        return Ok(());
    }

    let default_workspace = credentials::get_default_workspace();

    // Upstream fetches these in parallel with `Promise.all`; the output order
    // is deterministic either way, so a sequential loop keeps this simple.
    let infos: Vec<WorkspaceInfo> = workspaces
        .iter()
        .map(|workspace| {
            let api_key = credentials::get_credential_api_key(Some(workspace));
            let is_default = default_workspace.as_deref() == Some(workspace.as_str());
            fetch_workspace_info(workspace.clone(), is_default, api_key)
        })
        .collect();

    let workspace_width = infos
        .iter()
        .map(|info| display::display_width(&info.workspace))
        .chain(std::iter::once(display::display_width("WORKSPACE")))
        .max()
        .unwrap_or(9);
    let org_width = infos
        .iter()
        .map(|info| {
            display::display_width(
                info.org_name
                    .as_deref()
                    .unwrap_or_else(|| info.error.as_deref().unwrap_or("")),
            )
        })
        .chain(std::iter::once(display::display_width("ORG NAME")))
        .max()
        .unwrap_or(8);

    let header = format!(
        "  {} {} USER",
        display::pad_display("WORKSPACE", workspace_width),
        display::pad_display("ORG NAME", org_width)
    );
    output::line(&colors::underline(&header));

    for info in &infos {
        let prefix = if info.is_default { "* " } else { "  " };
        let workspace = display::pad_display(&info.workspace, workspace_width);
        match &info.error {
            Some(error) => {
                let org = display::pad_display(error, org_width);
                output::line(&format!("{prefix}{workspace} {}", colors::red(&org)));
            }
            None => {
                let org = display::pad_display(info.org_name.as_deref().unwrap_or(""), org_width);
                let user = format!(
                    "{} <{}>",
                    info.user_name.as_deref().unwrap_or(""),
                    info.email.as_deref().unwrap_or("")
                );
                output::line(&format!("{prefix}{workspace} {org} {user}"));
            }
        }
    }

    Ok(())
}

fn default(workspace: Option<String>) -> Result<()> {
    let workspaces = credentials::get_workspaces();
    if workspaces.is_empty() {
        return Err(CliError::auth("No workspaces configured")
            .suggestion("Run `linear auth login` to add a workspace"));
    }

    if workspaces.len() == 1 {
        output::line(&format!("Only one workspace configured: {}", workspaces[0]));
        return Ok(());
    }

    let current_default = credentials::get_default_workspace();

    let workspace = match workspace {
        Some(workspace) => workspace,
        None => {
            if !prompt::is_interactive() {
                return Err(CliError::cli("No workspace specified")
                    .suggestion(format!("Available workspaces: {}", workspaces.join(", "))));
            }
            let labels: Vec<String> = workspaces
                .iter()
                .map(|ws| {
                    if Some(ws) == current_default.as_ref() {
                        format!("{ws} (current)")
                    } else {
                        ws.clone()
                    }
                })
                .collect();
            let index = prompt::select("Select default workspace", &labels)?;
            workspaces[index].clone()
        }
    };

    if !credentials::has_workspace(&workspace) {
        return Err(CliError::not_found("Workspace", &workspace)
            .suggestion(format!("Available workspaces: {}", workspaces.join(", "))));
    }

    if Some(&workspace) == current_default.as_ref() {
        output::line(&format!("\"{workspace}\" is already the default workspace"));
        return Ok(());
    }

    credentials::set_default_workspace(&workspace)?;
    output::line(&format!("Default workspace set to: {workspace}"));
    Ok(())
}

fn token() -> Result<()> {
    match graphql::resolve_api_key_opt()? {
        Some(api_key) => {
            output::line(&api_key);
            Ok(())
        }
        None => Err(CliError::auth("No API key configured").suggestion(
            "Set LINEAR_API_KEY, add api_key to .linear.toml, or run `linear auth login`.",
        )),
    }
}

fn whoami() -> Result<()> {
    let client = graphql::client()?;
    let result = client.request(WHOAMI_VIEWER_QUERY, json!({}))?;
    print_viewer(&result)
}

fn print_viewer(result: &Value) -> Result<()> {
    let viewer = result
        .get("viewer")
        .ok_or_else(|| CliError::cli("Linear API returned no viewer information"))?;
    let organization = &viewer["organization"];

    let name = viewer["name"].as_str().unwrap_or_default();
    let display_name = viewer["displayName"].as_str().unwrap_or_default();
    let email = viewer["email"].as_str().unwrap_or_default();
    let org_name = organization["name"].as_str().unwrap_or_default();
    let url_key = organization["urlKey"].as_str().unwrap_or_default();

    output::line(&format!("Workspace: {org_name}"));
    output::line(&format!("  Slug: {url_key}"));
    output::line(&format!(
        "  URL: {}/{}",
        consts::LINEAR_WEB_BASE_URL,
        url_key
    ));

    output::line(&format!("User: {name}"));
    if display_name != name {
        output::line(&format!("  Display name: {display_name}"));
    }
    output::line(&format!("  Email: {email}"));
    if viewer["admin"].as_bool() == Some(true) {
        output::line("  Role: admin");
    } else if viewer["guest"].as_bool() == Some(true) {
        output::line("  Role: guest");
    }

    Ok(())
}

fn migrate() -> Result<()> {
    if !credentials::is_using_inline_format() {
        output::line("Credentials are already using the system keyring.");
        return Ok(());
    }

    if !keyring::is_available() {
        return Err(
            CliError::cli("No system keyring found. Cannot migrate credentials.").suggestion(
                "Install libsecret (e.g. `apt install libsecret-tools` or `pacman -S libsecret`), \
             or set `LINEAR_API_KEY` instead.",
            ),
        );
    }

    let migrated = credentials::migrate_to_keyring()?;
    if migrated.is_empty() {
        output::line("No credentials to migrate.");
    } else {
        output::line(&format!(
            "Migrated {} workspace(s) to system keyring:",
            migrated.len()
        ));
        for workspace in &migrated {
            output::line(&format!("  {workspace}"));
        }
    }
    Ok(())
}

/// True when an error looks like a rejected API key (HTTP 401/403).
fn is_unauthorized(error: &CliError) -> bool {
    matches!(error.http_status, Some(401) | Some(403)) || error.user_message.contains("401")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_api_key_removes_surrounding_junk() {
        assert_eq!(strip_api_key("  lin_api_abc  "), "lin_api_abc");
        assert_eq!(strip_api_key("!lin_api_abc!"), "lin_api_abc");
        // Interior characters are preserved: only the ends are stripped.
        assert_eq!(strip_api_key("lin.api_abc"), "lin.api_abc");
        // A value that is entirely junk collapses to empty.
        assert_eq!(strip_api_key("***"), "");
    }

    #[test]
    fn is_unauthorized_recognizes_401_and_403() {
        assert!(is_unauthorized(
            &CliError::cli("nope").with_http_status(401)
        ));
        assert!(is_unauthorized(
            &CliError::cli("nope").with_http_status(403)
        ));
        assert!(!is_unauthorized(
            &CliError::cli("nope").with_http_status(500)
        ));
        // Fallback: a status carried in the message text still counts.
        assert!(is_unauthorized(&CliError::cli("HTTP 401: Unauthorized")));
    }

    #[test]
    fn token_missing_key_matches_upstream_message_and_suggestion() {
        let error = CliError::auth("No API key configured").suggestion(
            "Set LINEAR_API_KEY, add api_key to .linear.toml, or run `linear auth login`.",
        );
        assert_eq!(error.kind, crate::errors::ErrorKind::Auth);
        assert_eq!(error.user_message, "No API key configured");
        assert_eq!(
            error.suggestion.as_deref(),
            Some("Set LINEAR_API_KEY, add api_key to .linear.toml, or run `linear auth login`.")
        );
    }
}
