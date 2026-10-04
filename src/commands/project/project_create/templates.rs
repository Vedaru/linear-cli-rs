use serde_json::Value;

use crate::commands::template as tmpl;
use crate::errors::{CliError, Result};
use crate::linear;

// ---------------------------------------------------------------------------
// Scoped template resolution (ported from utils/templates.ts, project scope)
// ---------------------------------------------------------------------------

pub(super) fn resolve_template_scoped(
    reference: &str,
    template_type: &str,
    team_ids: &[String],
) -> Result<Value> {
    crate::linear_url::reject_linear_url(reference, "a template name or UUID")?;
    if linear::is_linear_uuid(reference) {
        let template = tmpl::fetch_template(reference)?;
        assert_template_in_scope(&template, template_type, team_ids)?;
        return Ok(template);
    }

    let all = tmpl::fetch_templates()?;
    let wanted = reference.to_lowercase();
    let by_name: Vec<Value> = all
        .iter()
        .filter(|template| tmpl::template_name(template).to_lowercase() == wanted)
        .cloned()
        .collect();
    let in_scope = |template: &Value| {
        tmpl::template_type(template) == template_type
            && tmpl::template_is_available_to(template, team_ids)
    };
    let candidates: Vec<Value> = by_name.iter().filter(|t| in_scope(t)).cloned().collect();

    if candidates.len() == 1 {
        return Ok(candidates[0].clone());
    }

    if candidates.is_empty() {
        if !by_name.is_empty() {
            return Err(scope_mismatch_error(&by_name, template_type));
        }
        let names = available_names(&all, template_type, team_ids);
        let what = format!("{template_type} templates");
        let suggestion = if names.is_empty() {
            format!(
                "No {what} are available here. Run `linear template list` to see every template."
            )
        } else {
            format!(
                "Available {what}: {}. Run `linear template list` to see every template.",
                names
                    .iter()
                    .map(|name| format!("\"{name}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        return Err(CliError::not_found("Template", reference).suggestion(suggestion));
    }

    let ids = candidates
        .iter()
        .map(|template| {
            format!(
                "{} ({}, {})",
                tmpl::template_id(template),
                tmpl::template_type(template),
                tmpl::template_scope_label(template)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(CliError::validation(format!(
        "Template name \"{reference}\" is ambiguous: it matches {} templates",
        candidates.len()
    ))
    .suggestion(format!("Pass the template ID instead: {ids}")))
}

fn available_names(all: &[Value], template_type: &str, team_ids: &[String]) -> Vec<String> {
    let mut names: Vec<String> = all
        .iter()
        .filter(|template| {
            tmpl::template_type(template) == template_type
                && tmpl::template_is_available_to(template, team_ids)
        })
        .map(tmpl::template_name)
        .collect();
    names.sort();
    names.dedup();
    names
}

fn assert_template_in_scope(
    template: &Value,
    template_type: &str,
    team_ids: &[String],
) -> Result<()> {
    if tmpl::template_type(template) != template_type {
        return Err(wrong_type_error(template, template_type));
    }
    if !tmpl::template_is_available_to(template, team_ids) {
        return match template.get("team").filter(|team| !team.is_null()) {
            None => Err(CliError::cli(format!(
                "Template \"{}\" is not available here",
                tmpl::template_name(template)
            ))),
            Some(team) => {
                let key = team.get("key").and_then(Value::as_str).unwrap_or("");
                Err(other_team_error(
                    &tmpl::template_name(template),
                    &[key.to_string()],
                    template_type,
                ))
            }
        };
    }
    Ok(())
}

fn scope_mismatch_error(matches: &[Value], template_type: &str) -> CliError {
    let same_type: Vec<&Value> = matches
        .iter()
        .filter(|template| tmpl::template_type(template) == template_type)
        .collect();
    if !same_type.is_empty() {
        let mut team_keys: Vec<String> = Vec::new();
        for template in &same_type {
            if let Some(key) = template
                .get("team")
                .filter(|team| !team.is_null())
                .and_then(|team| team.get("key"))
                .and_then(Value::as_str)
            {
                if !team_keys.iter().any(|existing| existing == key) {
                    team_keys.push(key.to_string());
                }
            }
        }
        if !team_keys.is_empty() {
            return other_team_error(
                &tmpl::template_name(same_type[0]),
                &team_keys,
                template_type,
            );
        }
    }
    wrong_type_error(&matches[0], template_type)
}

fn wrong_type_error(template: &Value, template_type: &str) -> CliError {
    CliError::validation(format!(
        "Template \"{}\" is {}, not {}",
        tmpl::template_name(template),
        describe_type(&tmpl::template_type(template)),
        describe_type(template_type)
    ))
    .suggestion(format!(
        "Run `linear template list --type {template_type}` to see the {template_type} templates."
    ))
}

fn other_team_error(name: &str, team_keys: &[String], template_type: &str) -> CliError {
    let teams = team_keys.join(", ");
    let plural = if team_keys.len() == 1 { "" } else { "s" };
    CliError::validation(format!(
        "Template \"{name}\" belongs to team{plural} {teams} and cannot be applied here"
    ))
    .suggestion(format!(
        "Pass --team {}, or pick a workspace template or one from the target team with `linear template list --type {template_type} --team <team>`.",
        team_keys.first().map(String::as_str).unwrap_or("")
    ))
}

fn describe_type(template_type: &str) -> String {
    let article = match template_type.chars().next() {
        Some(first) if "aeiouAEIOU".contains(first) => "an",
        _ => "a",
    };
    format!("{article} {template_type} template")
}
