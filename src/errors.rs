//! User-facing error handling for the Linear CLI.
//!
//! Port of `src/utils/errors.ts`. Design rules carried over verbatim:
//!
//! - Messages are clean and actionable; stack traces only under `LINEAR_DEBUG=1`
//! - Errors explain what went wrong and how to fix it
//! - GraphQL errors are unwrapped to Linear's `userPresentableMessage`
//! - Errors go to stderr with a `✗` prefix, never to stdout, so `--json`
//!   output on stdout is never corrupted

use crate::colors;
use std::fmt;

/// Every fallible operation in this CLI returns this. `?` therefore works
/// uniformly: [`From`] impls in this module adapt foreign errors, and command
/// code wraps the result in [`handle_error`] at the top level.
pub type Result<T> = std::result::Result<T, CliError>;

/// Which constructor produced an error. Upstream models this with subclasses;
/// the distinction only matters for tests and for callers that want to react to
/// a lookup failure without parsing a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Generic CLI error carrying a user-facing message.
    Cli,
    /// An entity the caller asked for does not exist.
    NotFound,
    /// Bad user input: arguments, flags, config values.
    Validation,
    /// Missing or rejected credentials.
    Auth,
}

/// Extra detail printed only in debug mode.
#[derive(Debug, Default, Clone)]
pub struct DebugInfo {
    /// The GraphQL document that produced the error.
    pub query: Option<String>,
    /// Request variables, already JSON-encoded for display.
    pub variables: Option<String>,
}

#[derive(Debug)]
pub struct CliError {
    pub kind: ErrorKind,
    /// The clean, user-facing message.
    pub user_message: String,
    /// How to fix the issue, printed dimmed beneath the message.
    pub suggestion: Option<String>,
    /// Underlying error, shown only in debug mode.
    pub source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    /// GraphQL request context, shown only in debug mode.
    pub debug: Option<DebugInfo>,
    /// HTTP status, when the error came from a non-2xx response. Lets callers
    /// (e.g. `auth list`) tell an invalid credential (401/403) apart from
    /// other failures without parsing the message.
    pub http_status: Option<u16>,
}

impl CliError {
    pub fn new(kind: ErrorKind, user_message: impl Into<String>) -> Self {
        CliError {
            kind,
            user_message: user_message.into(),
            suggestion: None,
            source: None,
            debug: None,
            http_status: None,
        }
    }

    /// General CLI error.
    pub fn cli(user_message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Cli, user_message)
    }

    /// Invalid user input.
    pub fn validation(user_message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Validation, user_message)
    }

    /// Missing entity. Mirrors `NotFoundError(entityType, identifier)`.
    pub fn not_found(entity_type: &str, identifier: &str) -> Self {
        Self::new(
            ErrorKind::NotFound,
            format!("{entity_type} not found: {identifier}"),
        )
    }

    /// Authentication/authorization problem. Defaults to the same suggestion
    /// `AuthError` uses upstream.
    pub fn auth(user_message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Auth, user_message)
            .suggestion("Run `linear auth login` to authenticate.")
    }

    /// Attach guidance on how to fix the problem.
    pub fn suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    /// Attach guidance unless `suggestion` is `None` or empty.
    pub fn maybe_suggestion(mut self, suggestion: Option<impl Into<String>>) -> Self {
        if let Some(s) = suggestion {
            let s = s.into();
            if !s.is_empty() {
                self.suggestion = Some(s);
            }
        }
        self
    }

    /// Attach the underlying cause.
    pub fn cause(mut self, cause: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(cause));
        self
    }

    /// Attach GraphQL request context for debug output.
    pub fn debug_info(mut self, info: DebugInfo) -> Self {
        self.debug = Some(info);
        self
    }

    /// Record the HTTP status that produced this error.
    pub fn with_http_status(mut self, status: u16) -> Self {
        self.http_status = Some(status);
        self
    }

    /// Prefix the message with context, as `withContext()` does upstream.
    pub fn with_context(mut self, context: &str) -> Self {
        self.user_message = format!("{context}: {}", self.user_message);
        self
    }

    /// True when this error means "lookup found nothing". Linear answers a
    /// missing root entity with a GraphQL error whose message is either
    /// "Entity not found: <Type>" or the friendlier "Could not find referenced
    /// <Type>.", so match both spellings.
    pub fn is_not_found(&self) -> bool {
        let message = self.user_message.to_lowercase();
        message.contains("not found") || message.contains("could not find")
    }

    /// The chain of causes, outermost first, for debug output.
    fn cause_chain(&self) -> Vec<String> {
        let mut chain = Vec::new();
        let mut current: Option<&(dyn std::error::Error + 'static)> = self
            .source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static));
        while let Some(err) = current {
            chain.push(err.to_string());
            current = err.source();
        }
        chain
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.user_message)
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

// --- Conversions so `?` works in command code ---

macro_rules! from_error {
    ($ty:ty, $context:literal) => {
        impl From<$ty> for CliError {
            fn from(error: $ty) -> Self {
                CliError::cli(format!("{}: {error}", $context)).cause(error)
            }
        }
    };
}

from_error!(std::io::Error, "I/O error");
from_error!(serde_json::Error, "Failed to process JSON");
from_error!(toml::de::Error, "Failed to parse TOML");
from_error!(toml::ser::Error, "Failed to serialize TOML");

/// A GraphQL response that carried one or more `errors` entries.
#[derive(Debug, Clone)]
pub struct GraphQlError {
    /// Linear's `extensions.userPresentableMessage`, when present.
    pub user_presentable_message: Option<String>,
    /// First error's `message`.
    pub message: String,
    /// The document that produced the error, for debug output.
    pub query: Option<String>,
    /// Request variables, JSON-encoded, for debug output.
    pub variables: Option<String>,
}

impl GraphQlError {
    pub fn new(message: impl Into<String>) -> Self {
        GraphQlError {
            user_presentable_message: None,
            message: message.into(),
            query: None,
            variables: None,
        }
    }

    /// Prefer Linear's user-facing message, then the raw GraphQL message.
    /// Mirrors `extractGraphQLMessage()`.
    pub fn display_message(&self) -> String {
        self.user_presentable_message
            .clone()
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| self.message.clone())
    }
}

impl fmt::Display for GraphQlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_message())
    }
}

impl std::error::Error for GraphQlError {}

impl From<GraphQlError> for CliError {
    fn from(error: GraphQlError) -> Self {
        let message = error.display_message();
        CliError::cli(message).debug_info(DebugInfo {
            query: error.query.clone(),
            variables: error.variables.clone(),
        })
    }
}

/// Debug mode is opt-in via `LINEAR_DEBUG`, matching the TypeScript CLI.
pub fn is_debug_mode() -> bool {
    matches!(
        std::env::var("LINEAR_DEBUG").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Print an error and exit with status 1.
///
/// Normal mode prints the clean message and optional suggestion. Debug mode
/// also prints the cause chain and any GraphQL document/variables.
pub fn handle_error(error: &CliError, context: Option<&str>) -> ! {
    colors::init_stderr();

    let prefix = context.map(|c| format!("{c}: ")).unwrap_or_default();
    eprintln!(
        "{}",
        colors::red(&format!("✗ {prefix}{}", error.user_message))
    );

    if let Some(suggestion) = &error.suggestion {
        eprintln!("{}", colors::gray(&format!("  {suggestion}")));
    }

    if is_debug_mode() {
        let chain = error.cause_chain();
        if !chain.is_empty() {
            eprintln!("{}", colors::gray("\nStack trace (LINEAR_DEBUG=1):"));
            for (depth, cause) in chain.iter().enumerate() {
                eprintln!(
                    "{}",
                    colors::gray(&format!("  {}caused by: {cause}", "  ".repeat(depth)))
                );
            }
        }
        if let Some(debug) = &error.debug {
            if let Some(query) = &debug.query {
                eprintln!("{}", colors::gray("\nQuery:"));
                eprintln!("{}", colors::gray(query.trim()));
            }
            if let Some(variables) = &debug.variables {
                eprintln!("{}", colors::gray("\nVariables:"));
                eprintln!("{}", colors::gray(variables));
            }
        }
    }

    std::process::exit(1);
}

/// Turn a "not found" GraphQL failure into a [`ErrorKind::NotFound`] error and
/// pass every other error through untouched. Mirrors `translateNotFound()`.
///
/// Use for requests whose root field is a non-null entity lookup
/// (`issue(id:)`, `document(id:)`, `project(id:)`, `initiative(id:)`): Linear
/// answers a missing entity with a GraphQL error rather than a null field.
pub fn translate_not_found<T>(
    entity_type: &str,
    identifier: &str,
    request: impl FnOnce() -> Result<T>,
) -> Result<T> {
    match request() {
        Ok(value) => Ok(value),
        Err(error) => {
            if error.kind != ErrorKind::NotFound && error.is_not_found() {
                Err(CliError::not_found(entity_type, identifier))
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_message_shape() {
        let error = CliError::not_found("Issue", "ENG-123");
        assert_eq!(error.user_message, "Issue not found: ENG-123");
        assert_eq!(error.kind, ErrorKind::NotFound);
        assert!(error.is_not_found());
    }

    #[test]
    fn auth_defaults_to_login_suggestion() {
        let error = CliError::auth("Not authenticated");
        assert_eq!(
            error.suggestion.as_deref(),
            Some("Run `linear auth login` to authenticate.")
        );
    }

    #[test]
    fn recognizes_linear_not_found_spellings() {
        assert!(CliError::cli("Entity not found: Issue").is_not_found());
        assert!(CliError::cli("Could not find referenced Project.").is_not_found());
        assert!(!CliError::cli("Field 'foo' doesn't exist").is_not_found());
    }

    #[test]
    fn graphql_prefers_user_presentable_message() {
        let mut error = GraphQlError::new("raw message");
        error.user_presentable_message = Some("friendly".into());
        assert_eq!(error.display_message(), "friendly");
        let converted: CliError = error.into();
        assert_eq!(converted.user_message, "friendly");
    }

    #[test]
    fn with_context_prefixes_message_and_keeps_suggestion() {
        let error = CliError::validation("bad value").suggestion("use --sort");
        let wrapped = error.with_context("Failed to list issues");
        assert_eq!(wrapped.user_message, "Failed to list issues: bad value");
        assert_eq!(wrapped.suggestion.as_deref(), Some("use --sort"));
    }

    #[test]
    fn translate_not_found_rewrites_lookup_failures_only() {
        let result: Result<()> = translate_not_found("Issue", "ENG-1", || {
            Err(CliError::cli("Entity not found: Issue"))
        });
        let error = result.unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotFound);
        assert_eq!(error.user_message, "Issue not found: ENG-1");

        let result: Result<()> =
            translate_not_found("Issue", "ENG-1", || Err(CliError::validation("bad input")));
        assert_eq!(result.unwrap_err().kind, ErrorKind::Validation);
    }
}
