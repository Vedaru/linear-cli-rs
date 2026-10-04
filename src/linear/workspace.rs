//! The workspace slug, for the commands that build a `linear.app` URL.

use super::prelude::*;
use super::*;

const WORKSPACE_URL_KEY_QUERY: &str = r#"
query WorkspaceUrlKey {
  viewer {
    organization {
      urlKey
    }
  }
}
"#;

/// The workspace slug (`organization.urlKey`) for the configured API key.
///
/// A deployment whose `linear.toml` holds only an API key names no workspace,
/// and the browser helpers need a slug to build a `linear.app` URL. Linear knows
/// it, so ask rather than make the user pass `--workspace`.
pub fn workspace_url_key() -> Result<Option<String>> {
    let client = graphql::client()?;
    let data = client.request(WORKSPACE_URL_KEY_QUERY, json!({}))?;
    Ok(data
        .pointer("/viewer/organization/urlKey")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
        .map(str::to_string))
}
