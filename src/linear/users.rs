use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

/// `@me` and `self` both mean "the authenticated viewer".
fn is_self_reference(value: &str) -> bool {
    value == "@me" || value == "self"
}

/// Resolve a user reference to an ID: `@me`/`self` is the viewer, otherwise an
/// exact email then display-name match. `None` when nothing matches.
pub fn lookup_user_id(input: &str) -> Result<Option<String>> {
    reject_linear_url(input, "an assignee name or email")?;

    let client = graphql::client()?;
    if is_self_reference(input) {
        let data = client.request(GET_VIEWER_ID_QUERY, json!({}))?;
        return Ok(data
            .get("viewer")
            .and_then(|viewer| viewer.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string));
    }

    let data = client.request(LOOKUP_USER_QUERY, json!({ "input": input }))?;
    let empty = Vec::new();
    let nodes = data
        .get("users")
        .and_then(|users| users.get("nodes"))
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let needle = input.to_lowercase();

    let by_email = nodes.iter().find(|user| {
        user.get("email")
            .and_then(Value::as_str)
            .map(str::to_lowercase)
            .as_deref()
            == Some(needle.as_str())
    });
    let by_display_name = nodes.iter().find(|user| {
        user.get("displayName")
            .and_then(Value::as_str)
            .map(str::to_lowercase)
            .as_deref()
            == Some(needle.as_str())
    });
    let picked = by_email.or(by_display_name).or_else(|| nodes.first());
    Ok(picked
        .and_then(|user| user.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string))
}
