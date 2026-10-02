//! `linear config` — interactively generate `.linear.toml`.
//!
//! Port of `src/commands/config.ts`. The command resolves which workspace to
//! use (honouring an explicit `--workspace`, `LINEAR_API_KEY`, or an `api_key`
//! in config), asks Linear for the organization slug and the teams, prompts for
//! a team and an issue sort order, then writes the TOML file.
//!
//! Upstream relies entirely on interactive prompts. This port keeps that flow
//! but routes it through [`crate::prompt`], so a headless run gets an actionable
//! error instead of hanging when no explicit key/workspace and more than one
//! stored workspace require a choice.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::config;
use crate::credentials;
use crate::errors::{CliError, Result};
use crate::graphql;
use crate::output;
use crate::proc::{self, RunOptions, DEFAULT_TIMEOUT};
use crate::prompt;

mod service;

#[derive(clap::Args, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: Option<ConfigCommand>,
}

#[derive(clap::Subcommand, Debug)]
pub enum ConfigCommand {
    /// Print the sections the bridge reads, for appending to this same file
    Service(service::ServiceArgs),
}

const CONFIG_QUERY: &str = r#"query Config {
  viewer {
    organization {
      urlKey
    }
  }
  teams {
    nodes {
      id
      key
      name
    }
  }
}"#;

const BANNER: &str = "\
██      ██ ███    ██ ███████  █████  ██████      ██████ ██      ██
██      ██ ████   ██ ██      ██   ██ ██   ██    ██      ██      ██
██      ██ ██ ██  ██ █████   ███████ ██████     ██      ██      ██
██      ██ ██  ██ ██ ██      ██   ██ ██   ██    ██      ██      ██
███████ ██ ██   ████ ███████ ██   ██ ██   ██     ██████ ███████ ██";

pub fn run(args: ConfigArgs) -> Result<()> {
    // The interactive generator is what plain `linear config` has always meant; the service
    // sections are a second thing the same file holds, and they are printed rather than asked
    // for, because they are the same in every deployment and worth being scriptable.
    if let Some(ConfigCommand::Service(args)) = args.command {
        return service::run(args);
    }
    run_inner().map_err(|error| error.with_context("Failed to generate configuration"))
}

fn run_inner() -> Result<()> {
    output::blank();
    for line in BANNER.lines() {
        output::line(line);
    }
    output::blank();

    resolve_workspace()?;

    let client = graphql::client()?;
    let data = client.request(CONFIG_QUERY, json!({}))?;

    let workspace = data
        .pointer("/viewer/organization/urlKey")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::cli("Could not read the organization from the Linear response"))?
        .to_string();

    let mut teams: Vec<(String, String, String)> = data
        .pointer("/teams/nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| {
                    let id = node.get("id")?.as_str()?.to_string();
                    let key = node.get("key")?.as_str()?.to_string();
                    let name = node.get("name")?.as_str()?.to_string();
                    Some((id, key, name))
                })
                .collect()
        })
        .unwrap_or_default();

    teams.sort_by_key(|team| team.2.to_lowercase());

    if teams.is_empty() {
        return Err(CliError::cli("No teams found in this workspace"));
    }

    let team_labels: Vec<String> = teams
        .iter()
        .map(|(_, key, name)| format!("{name} ({key})"))
        .collect();
    let team_index = prompt::select("Select a team:", &team_labels)?;
    let team_key = teams[team_index].1.clone();

    let sort_options = ["manual".to_string(), "priority".to_string()];
    let sort_index = prompt::select("Select sort order:", &sort_options)?;
    let sort_choice = &sort_options[sort_index];

    let file_path = config_file_path();

    let toml_content = format!(
        "# linear cli\n\
         # https://github.com/schpet/linear-cli\n\
         \n\
         workspace = \"{workspace}\"\n\
         team_id = \"{team_key}\"\n\
         issue_sort = \"{sort_choice}\"\n"
    );

    std::fs::write(&file_path, toml_content).map_err(|error| {
        CliError::cli(format!("Failed to write {}: {error}", file_path.display())).cause(error)
    })?;

    output::line(&format!("Configuration written to {}", file_path.display()));
    Ok(())
}

/// Mirror `hasExplicitApiKey`: an environment key, a configured `api_key`, or
/// the global `--workspace` flag means the user already chose; otherwise fall
/// back to the stored credentials, prompting only when there is a real choice.
fn resolve_workspace() -> Result<()> {
    let has_env_key = std::env::var("LINEAR_API_KEY")
        .ok()
        .filter(|value| !value.is_empty())
        .is_some();
    let has_config_key = config::api_key()
        .filter(|value| !value.is_empty())
        .is_some();
    if has_env_key || has_config_key || config::cli_workspace().is_some() {
        return Ok(());
    }

    let workspaces = credentials::get_workspaces();
    if workspaces.is_empty() {
        return Err(CliError::auth("No authentication configured")
            .suggestion("Run `linear auth login` to add a workspace."));
    }

    if workspaces.len() == 1 {
        config::set_cli_workspace(Some(workspaces[0].clone()));
        return Ok(());
    }

    let default_workspace = credentials::get_default_workspace();
    let labels: Vec<String> = workspaces
        .iter()
        .map(|workspace| {
            if Some(workspace) == default_workspace.as_ref() {
                format!("{workspace} (default)")
            } else {
                workspace.clone()
            }
        })
        .collect();
    let index = prompt::select("Select workspace:", &labels)?;
    config::set_cli_workspace(Some(workspaces[index].clone()));
    Ok(())
}

/// Prefer `<git root>/.config/linear.toml` when that directory exists, then
/// `<git root>/.linear.toml`, then `./.linear.toml` when not in a repository.
fn config_file_path() -> PathBuf {
    let git_root = proc::run(
        "git",
        &["rev-parse", "--show-toplevel"],
        &RunOptions::default(),
        DEFAULT_TIMEOUT,
    )
    .filter(|output| output.success)
    .map(|output| output.stdout_trimmed())
    .filter(|root| !root.is_empty());

    match git_root {
        Some(root) => {
            let root = PathBuf::from(root);
            let config_dir = root.join(".config");
            if config_dir.is_dir() {
                config_dir.join("linear.toml")
            } else {
                root.join(".linear.toml")
            }
        }
        None => PathBuf::from("./.linear.toml"),
    }
}
