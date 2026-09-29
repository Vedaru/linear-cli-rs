//! Linear GraphQL transport. Port of `src/utils/graphql.ts`.
//!
//! Replaces `graphql-request` with a small `ureq` client. Two behaviours the
//! original gets from its library are reproduced explicitly here because
//! agents depend on them:
//!
//! * **Errors are data, not exceptions.** A 200 response carrying an `errors`
//!   array, or a 4xx with a JSON body, produces Linear's own message
//!   (`extensions.userPresentableMessage` preferred) rather than a generic
//!   "request failed".
//! * **Every request has a deadline.** `timeout_global` is set on the agent so
//!   a stalled API call cannot hang a cron job or an agent session forever.

use std::time::Duration;

use serde_json::{json, Value};

use crate::config;
use crate::consts;
use crate::credentials;
use crate::errors::{CliError, GraphQlError, Result};

/// Wall-clock budget for one HTTP request, including connect and body read.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

const USER_AGENT: &str = consts::USER_AGENT_PREFIX;

/// GraphQL endpoint, overridable for tests and self-hosted environments.
pub fn endpoint() -> String {
    std::env::var("LINEAR_GRAPHQL_ENDPOINT")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| consts::LINEAR_API_ENDPOINT.to_string())
}

/// Resolve the API key following upstream's precedence chain:
///
/// 1. `LINEAR_API_KEY` (conflicts with `--workspace`)
/// 2. `api_key` in project/global config
/// 3. `--workspace` → credentials lookup (hard error if unknown)
/// 4. config `workspace` → credentials lookup
/// 5. default workspace from credentials
///
/// Same precedence as [`resolve_api_key`], but returns `None` instead of a
/// "no key configured" error when nothing is found — mirroring
/// `getResolvedApiKey()`, whose return type is `string | undefined`.
///
/// `linear auth token` needs the `None` case to raise an `AuthError` with a
/// suggestion, while `linear auth whoami` (via [`client`]) lets the plain
/// message surface. The two hard failures — `--workspace` combined with
/// `LINEAR_API_KEY`, and an unknown `--workspace` — are errors in both cases.
pub fn resolve_api_key_opt() -> Result<Option<String>> {
    credentials::ensure_loaded()?;
    let cli_workspace = config::cli_workspace();
    let env_api_key = std::env::var("LINEAR_API_KEY")
        .ok()
        .filter(|value| !value.is_empty());

    if let (Some(_), Some(_)) = (&env_api_key, &cli_workspace) {
        return Err(CliError::cli(
            "Cannot use --workspace flag when LINEAR_API_KEY environment variable is set. \
             Either unset LINEAR_API_KEY or remove the --workspace flag.",
        ));
    }

    if let Some(key) = env_api_key {
        return Ok(Some(key));
    }
    if let Some(key) = config::api_key().filter(|value| !value.is_empty()) {
        return Ok(Some(key));
    }
    if let Some(workspace) = cli_workspace {
        return credentials::get_credential_api_key(Some(&workspace))
            .map(Some)
            .ok_or_else(|| {
                CliError::cli(format!(
                    "Workspace \"{workspace}\" not found in credentials. \
                     Run `linear auth login` to add it, or `linear auth list` to see configured workspaces."
                ))
            });
    }
    if let Some(workspace) = config::workspace() {
        if let Some(key) = credentials::get_credential_api_key(Some(&workspace)) {
            return Ok(Some(key));
        }
    }
    Ok(credentials::get_credential_api_key(None))
}

pub fn resolve_api_key() -> Result<String> {
    resolve_api_key_opt()?.ok_or_else(|| {
        CliError::cli(
            "No API key configured. Set LINEAR_API_KEY, add api_key to .linear.toml, \
             or run `linear auth login`.",
        )
    })
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // Read 4xx/5xx bodies ourselves so Linear's error payload survives.
        .http_status_as_error(false)
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .new_agent()
}

/// A GraphQL client bound to one API key. Build with [`client`] or
/// [`client_with_key`].
pub struct Client {
    endpoint: String,
    api_key: String,
    agent: ureq::Agent,
}

impl Client {
    /// Execute `query` with `variables`, returning the `data` object.
    pub fn request(&self, query: &str, variables: Value) -> Result<Value> {
        let variables_json = serde_json::to_string_pretty(&variables).ok();
        let payload = json!({ "query": query, "variables": variables });

        let mut response = self
            .agent
            .post(&self.endpoint)
            .header("Authorization", &self.api_key)
            .header("User-Agent", USER_AGENT)
            .header("Content-Type", "application/json")
            .send_json(&payload)
            .map_err(|error| CliError::cli(format!("Failed to reach Linear API: {error}")))?;

        let status = response.status().as_u16();
        let body = response.body_mut().read_to_string().map_err(|error| {
            CliError::cli(format!("Failed to read Linear API response: {error}"))
        })?;

        let parsed: Value = serde_json::from_str(&body).map_err(|_| {
            if status >= 400 {
                CliError::cli(format!("HTTP {status}: {}", body.trim())).with_http_status(status)
            } else {
                CliError::cli(format!(
                    "Invalid JSON from Linear API (HTTP {status}): {}",
                    body.trim()
                ))
            }
        })?;

        if let Some(errors) = parsed.get("errors").and_then(|value| value.as_array()) {
            if !errors.is_empty() {
                let error = self.graphql_error(errors, query, variables_json);
                return Err(if status >= 400 {
                    error.with_http_status(status)
                } else {
                    error
                });
            }
        }

        if status >= 400 {
            return Err(
                CliError::cli(format!("HTTP {status}: {}", body.trim())).with_http_status(status)
            );
        }

        // Move `data` out of the parsed response instead of cloning it. The
        // response can be large - Linear's introspection document is ~5 MB and
        // expands into tens of MB of tree - and a deep clone of the payload
        // doubled that peak for a copy that was dropped immediately after.
        let data = match parsed {
            Value::Object(mut root) => root.remove("data"),
            _ => None,
        };
        match data {
            Some(Value::Null) | None => Err(CliError::cli(
                "Linear API returned an empty response with no data and no errors.",
            )),
            Some(data) => Ok(data),
        }
    }

    fn graphql_error(
        &self,
        errors: &[Value],
        query: &str,
        variables_json: Option<String>,
    ) -> CliError {
        let first = errors.first();
        let message = first
            .and_then(|error| error.get("message"))
            .and_then(|value| value.as_str())
            .unwrap_or("Unknown GraphQL error")
            .to_string();
        let user_presentable_message = first
            .and_then(|error| error.get("extensions"))
            .and_then(|extensions| extensions.get("userPresentableMessage"))
            .and_then(|value| value.as_str())
            .map(str::to_string);

        let graphql_error = GraphQlError {
            user_presentable_message,
            message,
            query: Some(query.to_string()),
            variables: variables_json,
        };
        graphql_error.into()
    }

    /// Follow a paginated connection to exhaustion, returning every `nodes`
    /// entry.
    ///
    /// `path` names the connection inside `data` (e.g. `["issues"]` or
    /// `["team", "states"]`). `variables` must already carry the page size
    /// (`first`); `after` is managed here.
    ///
    /// A cursor that fails to advance is a stalled pagination bug — the same
    /// guard upstream uses — so it is an error rather than an infinite loop.
    pub fn paginate_connection(
        &self,
        query: &str,
        variables: serde_json::Map<String, Value>,
        path: &[&str],
    ) -> Result<Vec<Value>> {
        self.paginate_connection_page(query, variables, path)
            .map(|(nodes, _)| nodes)
    }

    /// Like [`Client::paginate_connection`], but also returns the final
    /// `pageInfo` object. `--json` output preserves the connection shape, so a
    /// caller that echoes a connection needs the server's own pagination
    /// metadata rather than a synthesized one.
    pub fn paginate_connection_page(
        &self,
        query: &str,
        mut variables: serde_json::Map<String, Value>,
        path: &[&str],
    ) -> Result<(Vec<Value>, Value)> {
        let mut nodes = Vec::new();
        let mut after: Option<String> = None;
        let mut last_page_info = json!({ "hasNextPage": false, "endCursor": null });

        loop {
            match &after {
                Some(cursor) => {
                    variables.insert("after".to_string(), json!(cursor));
                }
                None => {
                    variables.remove("after");
                }
            }

            let data = self.request(query, Value::Object(variables.clone()))?;
            let connection = dig(&data, path).ok_or_else(|| {
                CliError::cli(format!(
                    "Linear API response did not contain {}",
                    path.join(".")
                ))
            })?;

            if let Some(array) = connection.get("nodes").and_then(Value::as_array) {
                nodes.extend(array.iter().cloned());
            }

            let page_info = connection.get("pageInfo");
            if let Some(info) = page_info {
                last_page_info = info.clone();
            }
            let has_next = page_info
                .and_then(|info| info.get("hasNextPage"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !has_next {
                break;
            }

            let end_cursor = page_info
                .and_then(|info| info.get("endCursor"))
                .and_then(Value::as_str)
                .map(str::to_string);
            match end_cursor {
                Some(cursor) if Some(cursor.as_str()) != after.as_deref() => {
                    after = Some(cursor);
                }
                _ => {
                    return Err(CliError::cli(
                        "Pagination stalled: Linear did not return a new cursor.",
                    ))
                }
            }
        }

        Ok((nodes, last_page_info))
    }
}

/// Walk a JSON object by key path, returning `None` when any step is absent.
fn dig<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(key)?;
    }
    Some(current)
}

/// A client using an explicit API key (used by `auth login` to validate).
pub fn client_with_key(api_key: &str) -> Client {
    Client {
        endpoint: endpoint(),
        api_key: api_key.to_string(),
        agent: agent(),
    }
}

/// A client using the resolved API key.
pub fn client() -> Result<Client> {
    Ok(client_with_key(&resolve_api_key()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_defaults_to_linear() {
        std::env::remove_var("LINEAR_GRAPHQL_ENDPOINT");
        assert_eq!(endpoint(), consts::LINEAR_API_ENDPOINT);
    }

    #[test]
    fn graphql_errors_prefer_user_presentable_message() {
        let client = client_with_key("test-key");
        let errors = vec![json!({
            "message": "raw message",
            "extensions": { "userPresentableMessage": "friendly message" }
        })];
        let error = client.graphql_error(&errors, "query { x }", None);
        assert_eq!(error.user_message, "friendly message");
    }

    #[test]
    fn graphql_errors_fall_back_to_message() {
        let client = client_with_key("test-key");
        let errors = vec![json!({ "message": "raw message" })];
        let error = client.graphql_error(&errors, "query { x }", None);
        assert_eq!(error.user_message, "raw message");
    }

    #[test]
    fn requests_hit_the_endpoint_and_surface_transport_errors() {
        // Port 1 is reserved and refuses connections, so this exercises the
        // error path without needing a live server or network access.
        let client = Client {
            endpoint: "http://127.0.0.1:1/graphql".to_string(),
            api_key: "test-key".to_string(),
            agent: agent(),
        };
        let error = client
            .request("query { viewer { id } }", json!({}))
            .unwrap_err();
        assert!(!error.user_message.is_empty());
    }
}
