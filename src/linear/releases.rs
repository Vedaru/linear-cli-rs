use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Releases
// ---------------------------------------------------------------------------

/// Resolve a release to its UUID by UUID, exact name, or exact version. An
/// ambiguous name/version is an error rather than a silent pick.
pub fn resolve_release_id(input: &str) -> Result<String> {
    reject_linear_url(input, "a release name, version, or UUID")?;
    if is_linear_uuid(input) {
        return Ok(input.to_string());
    }

    let client = graphql::client()?;
    // Paginate to exhaustion: ambiguity detection is only trustworthy when the
    // full candidate set has been seen.
    let mut candidates: Vec<(String, String, Option<String>)> = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let mut variables = Map::new();
        variables.insert("input".to_string(), json!(input));
        if let Some(cursor) = &after {
            variables.insert("after".to_string(), json!(cursor));
        }
        let data = client.request(RESOLVE_RELEASES_QUERY, Value::Object(variables))?;
        let connection = data
            .get("releases")
            .ok_or_else(|| CliError::cli("Linear API response did not contain releases"))?;
        if let Some(nodes) = connection.get("nodes").and_then(Value::as_array) {
            for node in nodes {
                let Some(id) = node.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let name = node
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let version = node
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if !candidates.iter().any(|(existing, _, _)| existing == id) {
                    candidates.push((id.to_string(), name, version));
                }
            }
        }
        let page_info = connection.get("pageInfo");
        let has_next = page_info
            .and_then(|info| info.get("hasNextPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !has_next {
            break;
        }
        after = page_info
            .and_then(|info| info.get("endCursor"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if after.is_none() {
            return Err(CliError::cli(
                "Pagination stalled: Linear did not return a new cursor.",
            ));
        }
    }

    if candidates.is_empty() {
        return Err(CliError::not_found("Release", input)
            .suggestion("Pass a release UUID, exact release name, or exact version."));
    }
    if candidates.len() > 1 {
        let listing = candidates
            .iter()
            .map(|(id, name, version)| match version {
                Some(version) => format!("  {name} ({version}) — {id}"),
                None => format!("  {name} — {id}"),
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(CliError::validation(format!(
            "Release \"{input}\" is ambiguous; it matches multiple releases:\n{listing}"
        ))
        .suggestion("Pass the release UUID instead."));
    }
    Ok(candidates[0].0.clone())
}

