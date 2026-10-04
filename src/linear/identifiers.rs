use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Issue identifiers
// ---------------------------------------------------------------------------

/// Normalise an identifier, or upper-case the input when it cannot be parsed.
/// Mirrors `formatIssueIdentifier`.
pub fn format_issue_identifier(provided_id: &str) -> String {
    normalize_issue_identifier(provided_id).unwrap_or_else(|| provided_id.to_uppercase())
}

/// The configured team reference (`team_id`) and where it came from.
///
/// Upstream's `config` command writes a team KEY into this field, but every
/// consumer reads it back through `resolveTeam(...)` - so the value is a
/// *reference*: a key, a team name, or a team UUID all resolve. Nothing here may
/// assume it is already a key, which is exactly the assumption that made a UUID
/// in the config filter every team-scoped listing down to nothing. Callers
/// resolve it with [`resolve_configured_team`].
pub fn configured_team_reference() -> Option<Resolved<String>> {
    let resolved = config::team_id_resolved(None)?;
    let reference = resolved.value.trim();
    if reference.is_empty() {
        return None;
    }
    Some(Resolved {
        value: reference.to_string(),
        source: resolved.source,
    })
}

/// The configured team, resolved.
///
/// This is the port of the `resolveTeam(teamKey)` call upstream wraps the
/// configured value in, and `resolve_team` is the right resolver for it: it
/// matches a UUID through `teamById`, and keys and names case-insensitively. So
/// a UUID, a key or a name in `team_id` all work, and an unknown reference is an
/// error instead of a listing that quietly comes back empty.
pub fn resolve_configured_team() -> Result<Option<ResolvedTeam>> {
    match configured_team_reference() {
        Some(reference) => Ok(Some(resolve_team(&reference.value)?)),
        None => Ok(None),
    }
}

/// The configured team's canonical key, resolved.
///
/// Fallible on purpose: resolving a reference can fail (unknown team, no
/// network), and swallowing that would put back the silent-empty behaviour this
/// replaced.
pub fn get_team_key() -> Result<Option<String>> {
    Ok(resolve_configured_team()?.map(|team| team.key))
}

/// The configured team's canonical key, with where the reference came from.
pub fn get_team_key_with_source() -> Result<Option<Resolved<String>>> {
    let Some(reference) = configured_team_reference() else {
        return Ok(None);
    };
    let team = resolve_team(&reference.value)?;
    Ok(Some(Resolved {
        value: team.key,
        source: reference.source,
    }))
}

/// Turn loose input into a canonical issue identifier like `ABC-123`.
///
/// Accepts a pasted issue URL, a `TEAMKEY-NUMBER` identifier, or a bare
/// integer when a team is configured. When `provided_id` is `None` the current
/// issue is read from VCS state: the branch name for git, or the `Linear-issue`
/// trailer for jj.
pub fn get_issue_identifier(provided_id: Option<&str>) -> Result<Option<String>> {
    // Nothing on the command line: the working copy names the issue. This is what
    // makes "start an issue, then read it back" work, and it is only the `None`
    // arm - an explicit id that does not parse must not silently fall back to the
    // branch and ignore the argument the caller typed.
    let Some(provided) = provided_id else {
        return crate::vcs::get_current_issue_from_vcs();
    };

    // A pasted URL carries the identifier in its path; reading it here
    // covers every command and flag that funnels through this function.
    if let Some(LinearUrlRef::Issue { identifier, .. }) = expect_linear_url_kind(
        provided,
        "issue",
        "an issue URL or an identifier like ENG-123",
    )? {
        return Ok(Some(identifier));
    }

    if let Some(normalized) = normalize_issue_identifier(provided) {
        return Ok(Some(normalized));
    }

    if is_bare_integer(provided) {
        let Some(team_key) = get_team_key()? else {
            return Err(
                CliError::validation("an integer id was provided, but no team is set")
                    .suggestion("Run `linear config` to set a team."),
            );
        };
        return Ok(normalize_issue_identifier(&format!(
            "{team_key}-{provided}"
        )));
    }

    Ok(None)
}

/// `true` for a positive integer with no leading zero, matching upstream's
/// `/^[1-9][0-9]*$/`.
pub(crate) fn is_bare_integer(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('0') && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// `true` for a `+N`/`-N` cycle offset token.
pub(crate) fn is_signed_integer(value: &str) -> bool {
    let bytes = value.as_bytes();
    matches!(bytes.first(), Some(b'+') | Some(b'-'))
        && bytes.len() > 1
        && bytes[1..].iter().all(u8::is_ascii_digit)
}

/// The issue's UUID for an identifier, or `None` when it does not exist.
pub fn get_issue_id(identifier: &str) -> Result<Option<String>> {
    let client = graphql::client()?;
    let data = client.request(GET_ISSUE_ID_QUERY, json!({ "id": identifier }))?;
    Ok(data
        .get("issue")
        .and_then(|issue| issue.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string))
}
