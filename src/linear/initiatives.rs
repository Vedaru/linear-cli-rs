use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Initiatives
// ---------------------------------------------------------------------------

/// Look up an initiative by slug ID and nothing else. A URL's slug must not
/// fall through to a name lookup that a same-named initiative could win.
pub fn find_initiative_id_by_slug(
    slug_id: &str,
    include_archived: bool,
) -> Result<Option<String>> {
    let client = graphql::client()?;
    let data = client.request(
        RESOLVE_INITIATIVE_BY_SLUG_QUERY,
        json!({ "slugId": slug_id, "includeArchived": include_archived }),
    )?;
    Ok(data
        .get("initiatives")
        .and_then(|initiatives| initiatives.get("nodes"))
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.first())
        .and_then(|node| node.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string))
}

/// Resolve an initiative to its UUID by URL, UUID, slug ID, or exact name.
/// An ambiguous name is an error rather than a silent pick.
pub fn resolve_initiative_id(input: &str) -> Result<String> {
    if let Some(LinearUrlRef::Initiative { slug_id, .. }) =
        expect_linear_url_kind(input, "initiative", "an initiative URL, UUID, slug ID, or exact name")?
    {
        return find_initiative_id_by_slug(&slug_id, false)?.ok_or_else(|| {
            CliError::not_found("Initiative", input).suggestion(
                "The initiative in that URL may have been deleted, or be in a workspace this key cannot see.",
            )
        });
    }

    if is_linear_uuid(input) {
        return Ok(input.to_string());
    }

    if let Some(slug_match) = find_initiative_id_by_slug(input, false)? {
        return Ok(slug_match);
    }

    let client = graphql::client()?;
    let data = client.request(RESOLVE_INITIATIVE_BY_NAME_QUERY, json!({ "name": input }))?;
    let name_matches = data
        .get("initiatives")
        .and_then(|initiatives| initiatives.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if name_matches.len() > 1 {
        let listing = name_matches
            .iter()
            .map(|node| {
                format!(
                    "  {} — {} ({})",
                    node.get("name").and_then(Value::as_str).unwrap_or(""),
                    node.get("slugId").and_then(Value::as_str).unwrap_or(""),
                    node.get("id").and_then(Value::as_str).unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(CliError::validation(format!(
            "Initiative \"{input}\" is ambiguous; it matches multiple initiatives:\n{listing}"
        ))
        .suggestion("Pass the initiative's slug ID or UUID instead."));
    }
    if let Some(node) = name_matches.first() {
        return node
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| CliError::cli("Initiative match had no id"));
    }

    Err(CliError::not_found("Initiative", input)
        .suggestion("Pass an initiative UUID, slug ID, or exact initiative name."))
}

/// Resolve an initiative to its UUID, archived entities included.
///
/// Upstream gives `initiative unarchive` and `initiative delete` each their own
/// local `resolveInitiativeId`, because the shared resolver excludes archived
/// initiatives and those two commands exist precisely to address one. This is
/// that resolver, with upstream's four steps:
///
/// 1. an initiative URL -> its slug ID, read with `includeArchived: true`
///    (a URL must not fall through to a name lookup another initiative wins);
/// 2. a bare UUID -> the UUID itself;
/// 3. a slug ID -> [`find_initiative_id_by_slug`] with `includeArchived: true`;
/// 4. an exact name -> `RESOLVE_INITIATIVE_BY_NAME_INCLUDE_ARCHIVED_QUERY`.
///
/// Same ambiguity handling as [`resolve_initiative_id`]: more than one name
/// match is an error rather than a silent pick.
pub fn resolve_initiative_id_including_archived(input: &str) -> Result<String> {
    if let Some(LinearUrlRef::Initiative { slug_id, .. }) =
        expect_linear_url_kind(input, "initiative", "an initiative URL, UUID, slug ID, or exact name")?
    {
        return find_initiative_id_by_slug(&slug_id, true)?.ok_or_else(|| {
            CliError::not_found("Initiative", input).suggestion(
                "The initiative in that URL may have been deleted, or be in a workspace this key cannot see.",
            )
        });
    }

    if is_linear_uuid(input) {
        return Ok(input.to_string());
    }

    if let Some(slug_match) = find_initiative_id_by_slug(input, true)? {
        return Ok(slug_match);
    }

    let client = graphql::client()?;
    let data = client.request(
        RESOLVE_INITIATIVE_BY_NAME_INCLUDE_ARCHIVED_QUERY,
        json!({ "name": input }),
    )?;
    let name_matches = data
        .get("initiatives")
        .and_then(|initiatives| initiatives.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if name_matches.len() > 1 {
        let listing = name_matches
            .iter()
            .map(|node| {
                format!(
                    "  {} — {} ({})",
                    node.get("name").and_then(Value::as_str).unwrap_or(""),
                    node.get("slugId").and_then(Value::as_str).unwrap_or(""),
                    node.get("id").and_then(Value::as_str).unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(CliError::validation(format!(
            "Initiative \"{input}\" is ambiguous; it matches multiple initiatives:\n{listing}"
        ))
        .suggestion("Pass the initiative's slug ID or UUID instead."));
    }
    if let Some(node) = name_matches.first() {
        return node
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| CliError::cli("Initiative match had no id"));
    }

    Err(CliError::not_found("Initiative", input)
        .suggestion("Pass an initiative UUID, slug ID, or exact initiative name."))
}

