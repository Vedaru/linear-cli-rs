//! Shared project-description resolution — port of
//! `src/commands/project/project-description.ts`.
//!
//! The projectCreate / projectUpdate mutations are bound to Linear's
//! 255-character description cap, so the length is enforced here before a
//! request is built.

use crate::errors::{CliError, Result};

/// Linear's API rejects project descriptions longer than this.
pub const PROJECT_DESCRIPTION_MAX_LENGTH: usize = 255;

/// Resolve `--description` / `--description-file` into one value, enforcing
/// the API length cap. Used by both `project create` and `project update`.
pub fn resolve_project_description(
    description: Option<&str>,
    description_file: Option<&str>,
) -> Result<Option<String>> {
    if description.is_some() && description_file.is_some() {
        return Err(CliError::validation(
            "Cannot use --description and --description-file together",
        )
        .suggestion("Pass only one of --description or --description-file."));
    }

    let value = if let Some(description) = description {
        Some(description.to_string())
    } else if let Some(path) = description_file {
        match std::fs::read_to_string(path) {
            Ok(contents) => Some(contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CliError::not_found("File", path));
            }
            Err(error) => {
                return Err(
                    CliError::cli(format!("Failed to read description file: {error}")).cause(error),
                );
            }
        }
    } else {
        None
    };

    if let Some(value) = &value {
        let length = value.chars().count();
        if length > PROJECT_DESCRIPTION_MAX_LENGTH {
            return Err(CliError::validation(format!(
                "Project description is {length} characters, exceeds the {PROJECT_DESCRIPTION_MAX_LENGTH}-character limit enforced by Linear's API"
            ))
            .suggestion(format!(
                "Shorten the description to {PROJECT_DESCRIPTION_MAX_LENGTH} characters or fewer, or move the long content into an attached document via `linear document create --project <slug>`."
            )));
        }
    }

    Ok(value)
}
