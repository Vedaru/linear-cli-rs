use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Milestones
// ---------------------------------------------------------------------------

/// Resolve a milestone to its UUID by URL rejection, UUID passthrough, or exact
/// name within a project.
pub fn resolve_milestone_id(name_or_id: &str, project_id: Option<&str>) -> Result<String> {
    reject_linear_url(name_or_id, "a milestone name or UUID")?;
    if is_linear_uuid(name_or_id) {
        return Ok(name_or_id.to_string());
    }
    let Some(project_id) = project_id else {
        return Err(
            CliError::validation("Cannot resolve milestone by name without a project.")
                .suggestion("Pass --project, or use the milestone's UUID."),
        );
    };
    get_milestone_id_by_name(name_or_id, project_id)?.ok_or_else(|| {
        CliError::not_found("Milestone", name_or_id)
            .suggestion("Pass a milestone UUID or an exact milestone name.")
    })
}

/// Rewrite Linear's internal not-found for a milestone mutation.
///
/// A mutation on a missing (or name-shaped) milestone answers
/// "Could not find referenced ProjectMilestone." - an internal GraphQL type that
/// means nothing to the user and suggests nothing. `view` already reports
/// `Milestone not found`; this gives `update` and `delete` the same answer.
pub fn missing_milestone(error: CliError, reference: &str) -> CliError {
    if error.is_not_found() {
        CliError::not_found("Milestone", reference).suggestion(
            "Pass the milestone UUID (run `linear milestone list`), or view it by name with `linear milestone view <name> --project <project>`.",
        )
    } else {
        error
    }
}

/// A milestone ID for an exact, case-insensitive name within a project.
pub fn get_milestone_id_by_name(name: &str, project_id: &str) -> Result<Option<String>> {
    let client = graphql::client()?;
    let data = client.request(
        GET_MILESTONE_BY_NAME_QUERY,
        json!({ "projectId": project_id, "name": name }),
    )?;
    let Some(project) = data.get("project") else {
        return Err(CliError::not_found("Project", project_id));
    };
    let project = if project.is_null() {
        return Err(CliError::not_found("Project", project_id));
    } else {
        project
    };
    Ok(first_id(project.get("projectMilestones")))
}
