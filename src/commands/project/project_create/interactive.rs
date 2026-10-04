use serde_json::json;

use crate::errors::{CliError, Result};
use crate::linear;
use crate::{graphql, output, prompt};

use super::{project_statuses, GET_PROJECT_STATUSES_QUERY};

// ---------------------------------------------------------------------------
// Interactive create (gated on a real terminal)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub(super) fn interactive_prompt(
    client: &graphql::Client,
    name: &mut Option<String>,
    description: &mut Option<String>,
    description_file: Option<&str>,
    teams: &mut Vec<String>,
    lead: &mut Option<String>,
    status: &mut Option<String>,
    start_date: &mut Option<String>,
    target_date: &mut Option<String>,
) -> Result<()> {
    output::line("");
    output::line("Create a new project");
    output::line("");

    if name.is_none() {
        *name = Some(prompt_text_required("Project name:")?);
    }

    if description.is_none() && description_file.is_none() {
        let value = prompt_text("Description (optional):", "")?;
        *description = if value.is_empty() { None } else { Some(value) };
    }

    if teams.is_empty() {
        let all_teams = linear::get_all_teams()?;
        let options: Vec<(String, String)> = all_teams
            .iter()
            .map(|team| (team.key.clone(), format!("{} ({})", team.name, team.key)))
            .collect();
        if !options.is_empty() {
            let default_index = linear::get_team_key()?
                .and_then(|key| options.iter().position(|(value, _)| value == &key))
                .unwrap_or(0);
            let selected = prompt_select("Team:", &options, default_index)?;
            *teams = vec![selected];
        }
    }

    if status.is_none() {
        let data = client.request(GET_PROJECT_STATUSES_QUERY, json!({}))?;
        let statuses = project_statuses(&data);
        let options: Vec<(String, String)> = statuses
            .iter()
            .filter_map(|node| {
                Some((
                    node.get("type")?.as_str()?.to_string(),
                    node.get("name")?.as_str()?.to_string(),
                ))
            })
            .collect();
        if !options.is_empty() {
            let default_index = options
                .iter()
                .position(|(value, _)| value == "planned")
                .unwrap_or(0);
            *status = Some(prompt_select("Status:", &options, default_index)?);
        }
    }

    if lead.is_none() {
        let value = prompt_text("Lead (username, email, or @me - press Enter to skip):", "")?;
        *lead = if value.is_empty() { None } else { Some(value) };
    }

    if start_date.is_none() {
        let value = prompt_text("Start date (YYYY-MM-DD - press Enter to skip):", "")?;
        *start_date = if value.is_empty() { None } else { Some(value) };
    }

    if target_date.is_none() {
        let value = prompt_text("Target date (YYYY-MM-DD - press Enter to skip):", "")?;
        *target_date = if value.is_empty() { None } else { Some(value) };
    }

    Ok(())
}

fn prompt_text(message: &str, default: &str) -> Result<String> {
    eprint!("{message}");
    if !default.is_empty() {
        eprint!(" [{default}]");
    }
    eprint!(" ");
    let _ = std::io::Write::flush(&mut std::io::stderr());

    let mut line = String::new();
    let read = std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)
        .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
    if read == 0 {
        return Ok(default.to_string());
    }
    let value = line.trim().to_string();
    Ok(if value.is_empty() {
        default.to_string()
    } else {
        value
    })
}

fn prompt_text_required(message: &str) -> Result<String> {
    loop {
        let value = prompt_text(message, "")?;
        if !value.is_empty() {
            return Ok(value);
        }
        if !prompt::is_interactive() {
            return Err(CliError::validation("No project name provided"));
        }
    }
}

fn prompt_select(
    message: &str,
    options: &[(String, String)],
    default_index: usize,
) -> Result<String> {
    if options.is_empty() {
        return Err(CliError::validation("No options available"));
    }
    eprintln!("{message}");
    for (index, (_, display)) in options.iter().enumerate() {
        let marker = if index == default_index {
            " (default)"
        } else {
            ""
        };
        eprintln!("  {}. {display}{marker}", index + 1);
    }

    let stdin = std::io::stdin();
    loop {
        eprint!(
            "Enter a number (1-{}) [{}]: ",
            options.len(),
            default_index + 1
        );
        let _ = std::io::Write::flush(&mut std::io::stderr());

        let mut line = String::new();
        let read = std::io::BufRead::read_line(&mut stdin.lock(), &mut line)
            .map_err(|error| CliError::cli(format!("Failed to read input: {error}")))?;
        if read == 0 {
            return Ok(options[default_index].0.clone());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(options[default_index].0.clone());
        }
        if let Ok(choice) = trimmed.parse::<usize>() {
            if choice >= 1 && choice <= options.len() {
                return Ok(options[choice - 1].0.clone());
            }
        }
        eprintln!("Please enter a number between 1 and {}.", options.len());
    }
}

