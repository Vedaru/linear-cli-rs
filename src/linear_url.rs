//! Parse Linear web URLs into things the CLI can look up.
//! Port of `src/utils/linear-url.ts`.
//!
//! A user pastes `https://linear.app/acme/issue/ENG-123/...` where a command
//! expects `ENG-123`. This module classifies that input so callers either
//! extract the reference, or report a clear error instead of looking up a
//! string starting with `https://`.
//!
//! The URL is only ever read, never fetched.

use regex::Regex;
use std::sync::LazyLock;
use url::Url;

use crate::config;
use crate::credentials;
use crate::errors::{CliError, Result};
use crate::issue_identifier::normalize_issue_identifier;

/// Linear serves its app from this host only; it offers no custom domains.
const LINEAR_APP_HOSTS: [&str; 2] = ["linear.app", "www.linear.app"];

/// Project, document and initiative URLs end in `{name-slug}-{slugId}`, where
/// the slug ID is twelve hex characters. The name part is decorative.
static SLUG_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{12}$").expect("valid regex"));

/// Comment anchors carry the first eight characters of a UUID.
static COMMENT_ANCHOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^comment-([0-9a-f]{8})$").expect("valid regex"));
static PROJECT_UPDATE_ANCHOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^project-update-[0-9a-f]{8}$").expect("valid regex"));

/// Team pages that still mean "this team". Linear puts other entities under
/// `/team/{KEY}/` too — a cycle lives at `/team/{KEY}/cycle/5` — so an unknown
/// descendant is refused rather than assumed to denote the team.
const TEAM_SUBPAGES: [&str; 10] = [
    "overview",
    "all",
    "active",
    "backlog",
    "triage",
    "cycles",
    "projects",
    "projects/all",
    "views/issues",
    "settings",
];

/// Pages under a project that still mean the project.
const PROJECT_SUBPAGES: [&str; 4] = ["overview", "issues", "updates", "activity"];

/// A cycle addressed by number or by the relative form the app uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleRef {
    Number(u64),
    Active,
    Next,
}

/// What a Linear URL named, when it named something the CLI can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearUrlRef {
    Issue {
        workspace: String,
        identifier: String,
        /// Only the first eight characters of a comment's UUID survive in the
        /// anchor, so this explains the problem but cannot identify a comment.
        comment_id_prefix: Option<String>,
    },
    Project {
        workspace: String,
        slug_id: String,
    },
    Document {
        workspace: String,
        slug_id: String,
    },
    Initiative {
        workspace: String,
        slug_id: String,
    },
    Team {
        workspace: String,
        team_key: String,
    },
    Cycle {
        workspace: String,
        team_key: String,
        cycle: CycleRef,
    },
}

impl LinearUrlRef {
    pub fn kind(&self) -> &'static str {
        match self {
            LinearUrlRef::Issue { .. } => "issue",
            LinearUrlRef::Project { .. } => "project",
            LinearUrlRef::Document { .. } => "document",
            LinearUrlRef::Initiative { .. } => "initiative",
            LinearUrlRef::Team { .. } => "team",
            LinearUrlRef::Cycle { .. } => "cycle",
        }
    }

    pub fn workspace(&self) -> &str {
        match self {
            LinearUrlRef::Issue { workspace, .. }
            | LinearUrlRef::Project { workspace, .. }
            | LinearUrlRef::Document { workspace, .. }
            | LinearUrlRef::Initiative { workspace, .. }
            | LinearUrlRef::Team { workspace, .. }
            | LinearUrlRef::Cycle { workspace, .. } => workspace,
        }
    }
}

/// The result of classifying user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearUrlParse {
    /// Ordinary input. Callers fall through to their existing lookup.
    NotLinearUrl,
    /// Recognisably a Linear URL, but not one that names something usable.
    /// Callers must report this rather than retrying it as a name.
    Unsupported { reason: String },
    Ok { reference: LinearUrlRef },
}

fn entity_label_of(kind: &str) -> &'static str {
    match kind {
        "issue" => "an issue",
        "project" => "a project",
        "document" => "a document",
        "initiative" => "an initiative",
        "team" => "a team",
        "cycle" => "a cycle",
        _ => "an entity",
    }
}

fn unsupported(reason: impl Into<String>) -> LinearUrlParse {
    LinearUrlParse::Unsupported {
        reason: reason.into(),
    }
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decode one percent-encoded path segment, mirroring `decodeURIComponent`.
fn decode_segment(segment: &str) -> std::result::Result<String, ()> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(());
            }
            let high = hex_val(bytes[index + 1]).ok_or(())?;
            let low = hex_val(bytes[index + 2]).ok_or(())?;
            out.push(high * 16 + low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

/// Parse `value` as a URL, adding `https://` when a Linear host was written
/// without a scheme. Only a host we would accept gets the scheme, so an
/// ordinary name is never mistaken for a URL.
fn to_url(value: &str) -> Option<Url> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();
    let with_scheme = if lower.starts_with("http://") || lower.starts_with("https://") {
        trimmed.to_string()
    } else if LINEAR_APP_HOSTS
        .iter()
        .any(|host| lower.starts_with(&format!("{host}/")))
    {
        format!("https://{trimmed}")
    } else {
        trimmed.to_string()
    };
    Url::parse(&with_scheme).ok()
}

/// Pull the twelve-hex slug ID off a `{name-slug}-{slugId}` segment, or accept
/// a segment that is itself a bare slug ID.
fn extract_slug_id(segment: &str) -> Option<String> {
    let lower = segment.to_lowercase();
    if SLUG_ID_RE.is_match(&lower) {
        return Some(lower);
    }
    let last_dash = lower.rfind('-')?;
    let candidate = &lower[last_dash + 1..];
    SLUG_ID_RE.is_match(candidate).then(|| candidate.to_string())
}

fn parse_cycle_path(workspace: &str, team_key: &str, rest: &[String]) -> LinearUrlParse {
    let Some((segment, extra)) = rest.split_first() else {
        return unsupported("it does not name a cycle");
    };
    if !extra.is_empty() {
        return unsupported(format!("\"{}\" is not a cycle page", rest.join("/")));
    }

    let team_key = team_key.to_uppercase();
    match segment.to_lowercase().as_str() {
        "active" => {
            return LinearUrlParse::Ok {
                reference: LinearUrlRef::Cycle {
                    workspace: workspace.to_string(),
                    team_key,
                    cycle: CycleRef::Active,
                },
            }
        }
        "upcoming" => {
            return LinearUrlParse::Ok {
                reference: LinearUrlRef::Cycle {
                    workspace: workspace.to_string(),
                    team_key,
                    cycle: CycleRef::Next,
                },
            }
        }
        _ => {}
    }

    if let Some(number) = parse_positive_integer(segment) {
        return LinearUrlParse::Ok {
            reference: LinearUrlRef::Cycle {
                workspace: workspace.to_string(),
                team_key,
                cycle: CycleRef::Number(number),
            },
        };
    }

    unsupported(format!("\"{segment}\" is not a cycle number"))
}

/// A positive integer with no leading zero, within JavaScript's safe-integer
/// range (which is what upstream checks).
fn parse_positive_integer(segment: &str) -> Option<u64> {
    if segment.is_empty()
        || segment.starts_with('0')
        || !segment.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    segment.parse::<u64>().ok().filter(|n| *n <= 9_007_199_254_740_991)
}

/// Classify a user-supplied reference. See the module docs.
pub fn parse_linear_url(value: &str) -> LinearUrlParse {
    let Some(url) = to_url(value) else {
        return LinearUrlParse::NotLinearUrl;
    };
    if url.scheme() != "https" && url.scheme() != "http" {
        return LinearUrlParse::NotLinearUrl;
    }
    let Some(host) = url.host_str() else {
        return LinearUrlParse::NotLinearUrl;
    };
    let hostname = host.to_lowercase();
    let hostname = hostname.trim_end_matches('.');
    if !LINEAR_APP_HOSTS.contains(&hostname) {
        return LinearUrlParse::NotLinearUrl;
    }
    if url.port().is_some() || !url.username().is_empty() || url.password().is_some() {
        return LinearUrlParse::NotLinearUrl;
    }

    let mut segments = Vec::new();
    for segment in url.path_segments().map(|s| s.collect::<Vec<_>>()).unwrap_or_default() {
        if segment.is_empty() {
            continue;
        }
        match decode_segment(segment) {
            Ok(decoded) => segments.push(decoded),
            Err(()) => return unsupported("its path could not be decoded"),
        }
    }
    if segments.iter().any(|s| s == "." || s == "..") {
        return unsupported("its path contains relative segments");
    }

    let (workspace, entity) = match (segments.first(), segments.get(1)) {
        (Some(workspace), Some(entity)) => (workspace.as_str(), entity.as_str()),
        _ => return unsupported("it does not name a workspace and an entity"),
    };
    let rest = &segments[2..];
    let anchor = url.fragment().unwrap_or("");

    match entity {
        "issue" => {
            let Some(raw) = rest.first() else {
                return unsupported("it does not name an issue");
            };
            let Some(identifier) = normalize_issue_identifier(raw) else {
                return unsupported(format!("\"{raw}\" is not an issue identifier"));
            };
            if anchor.is_empty() {
                return LinearUrlParse::Ok {
                    reference: LinearUrlRef::Issue {
                        workspace: workspace.to_string(),
                        identifier,
                        comment_id_prefix: None,
                    },
                };
            }
            if let Some(captures) = COMMENT_ANCHOR_RE.captures(anchor) {
                return LinearUrlParse::Ok {
                    reference: LinearUrlRef::Issue {
                        workspace: workspace.to_string(),
                        identifier,
                        comment_id_prefix: Some(captures[1].to_string()),
                    },
                };
            }
            unsupported(format!("\"#{anchor}\" is not a comment link"))
        }
        "project" | "document" | "initiative" => {
            let Some(raw) = rest.first() else {
                return unsupported(format!("it does not name {}", entity_label_of(entity)));
            };
            let Some(slug_id) = extract_slug_id(raw) else {
                return unsupported(format!("\"{raw}\" does not end in a Linear slug ID"));
            };
            let tail = rest[1..].join("/");
            if !tail.is_empty() && !(entity == "project" && PROJECT_SUBPAGES.contains(&tail.as_str())) {
                return unsupported(format!("\"{tail}\" is not a page this command can use"));
            }
            if !anchor.is_empty() && !PROJECT_UPDATE_ANCHOR_RE.is_match(anchor) {
                return unsupported(format!("\"#{anchor}\" is not a link this command can use"));
            }
            let reference = match entity {
                "project" => LinearUrlRef::Project {
                    workspace: workspace.to_string(),
                    slug_id,
                },
                "document" => LinearUrlRef::Document {
                    workspace: workspace.to_string(),
                    slug_id,
                },
                _ => LinearUrlRef::Initiative {
                    workspace: workspace.to_string(),
                    slug_id,
                },
            };
            LinearUrlParse::Ok { reference }
        }
        "team" => {
            let Some(team_key) = rest.first() else {
                return unsupported("it does not name a team");
            };
            if rest.get(1).map(String::as_str) == Some("cycle") {
                return parse_cycle_path(workspace, team_key, &rest[2..]);
            }
            let tail = rest[1..].join("/");
            if !tail.is_empty() && !TEAM_SUBPAGES.contains(&tail.as_str()) {
                return unsupported(format!("\"{tail}\" is not a team page this command can use"));
            }
            LinearUrlParse::Ok {
                reference: LinearUrlRef::Team {
                    workspace: workspace.to_string(),
                    team_key: team_key.to_uppercase(),
                },
            }
        }
        _ => unsupported(format!(
            "\"{entity}\" is not an entity this command can use"
        )),
    }
}

/// The workspace this invocation is working in, as far as can be told without a
/// request: the `--workspace` flag, then the configured workspace, then the
/// default credential.
fn get_effective_workspace_slug() -> Option<String> {
    let configured = config::cli_workspace()
        .or_else(config::workspace)
        .or_else(credentials::get_default_workspace);
    configured
        .map(|workspace| workspace.trim().to_string())
        .filter(|workspace| !workspace.is_empty())
}

/// How to reach another workspace depends on where the API key comes from,
/// mirroring the precedence in `resolve_api_key`.
fn switch_workspace_suggestion(url_workspace: &str, current: &str) -> String {
    let from_url = format!("or use a URL from \"{current}\".");
    if std::env::var_os("LINEAR_API_KEY").is_some() {
        return format!(
            "LINEAR_API_KEY is set, and the CLI won't combine it with --workspace. \
             Unset it and pass --workspace {url_workspace}, {from_url}"
        );
    }
    if config::api_key().is_some() {
        return format!(
            "The api_key in your config outranks --workspace. Remove it to pass \
             --workspace {url_workspace}, {from_url}"
        );
    }
    format!("Pass --workspace {url_workspace}, {from_url}")
}

fn assert_same_workspace(url_workspace: &str) -> Result<()> {
    let Some(current) = get_effective_workspace_slug() else {
        return Ok(());
    };
    if current.to_lowercase() == url_workspace.to_lowercase() {
        return Ok(());
    }
    Err(CliError::validation(format!(
        "That URL is for the \"{url_workspace}\" workspace, but this is the \"{current}\" workspace."
    ))
    .suggestion(switch_workspace_suggestion(url_workspace, &current)))
}

/// Extract a reference of the expected `kind` from a URL.
///
/// `Ok(None)` when the input is not a Linear URL at all, so callers keep their
/// existing UUID / slug / name handling. `Err` when the input is a Linear URL
/// that names something else, names nothing usable, or belongs to another
/// workspace.
pub fn expect_linear_url_kind(
    input: &str,
    kind: &str,
    entity_label: &str,
) -> Result<Option<LinearUrlRef>> {
    match parse_linear_url(input) {
        LinearUrlParse::NotLinearUrl => Ok(None),
        LinearUrlParse::Unsupported { reason } => Err(CliError::validation(format!(
            "\"{input}\" is a Linear URL, but {reason}."
        ))
        .suggestion(format!("Pass {entity_label}."))),
        LinearUrlParse::Ok { reference } => {
            assert_same_workspace(reference.workspace())?;
            if reference.kind() != kind {
                return Err(CliError::validation(format!(
                    "\"{input}\" is {} URL, not {} URL.",
                    entity_label_of(reference.kind()),
                    entity_label_of(kind)
                ))
                .suggestion(format!("Pass {entity_label}.")));
            }
            Ok(Some(reference))
        }
    }
}

/// For commands whose identifiers have no Linear URL at all. A pasted URL is
/// reported plainly instead of becoming a lookup for a `https://` string.
pub fn reject_linear_url(input: &str, entity_label: &str) -> Result<()> {
    match parse_linear_url(input) {
        LinearUrlParse::NotLinearUrl => Ok(()),
        _ => Err(CliError::validation(format!(
            "\"{input}\" is a Linear URL, and this command does not take one."
        ))
        .suggestion(format!("Pass {entity_label}."))),
    }
}

/// A comment URL keeps only the first eight characters of the comment's UUID,
/// so it cannot identify a comment. Say so directly.
pub fn reject_comment_url(input: &str) -> Result<()> {
    if let LinearUrlParse::Ok {
        reference:
            LinearUrlRef::Issue {
                comment_id_prefix: Some(_),
                ..
            },
    } = parse_linear_url(input)
    {
        return Err(CliError::validation(format!(
            "\"{input}\" links to a comment, but a comment URL only carries the first \
             eight characters of its ID."
        ))
        .suggestion(
            "Pass the comment's full UUID, from `linear issue comment list <issue> --json`.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(input: &str) -> LinearUrlRef {
        match parse_linear_url(input) {
            LinearUrlParse::Ok { reference } => reference,
            other => panic!("expected ok for {input}, got {other:?}"),
        }
    }

    #[test]
    fn non_urls_fall_through() {
        assert_eq!(parse_linear_url("ENG-123"), LinearUrlParse::NotLinearUrl);
        assert_eq!(parse_linear_url("my-project"), LinearUrlParse::NotLinearUrl);
        assert_eq!(parse_linear_url(""), LinearUrlParse::NotLinearUrl);
        assert_eq!(
            parse_linear_url("https://example.com/acme/issue/ENG-1"),
            LinearUrlParse::NotLinearUrl
        );
    }

    #[test]
    fn scheme_optional_for_linear_hosts() {
        assert_eq!(
            ok("linear.app/acme/issue/ENG-1"),
            LinearUrlRef::Issue {
                workspace: "acme".into(),
                identifier: "ENG-1".into(),
                comment_id_prefix: None,
            }
        );
    }

    #[test]
    fn parses_issue_with_and_without_comment_anchor() {
        assert_eq!(
            ok("https://linear.app/acme/issue/eng-42/title-here"),
            LinearUrlRef::Issue {
                workspace: "acme".into(),
                identifier: "ENG-42".into(),
                comment_id_prefix: None,
            }
        );
        assert_eq!(
            ok("https://linear.app/acme/issue/ENG-42#comment-deadbeef"),
            LinearUrlRef::Issue {
                workspace: "acme".into(),
                identifier: "ENG-42".into(),
                comment_id_prefix: Some("deadbeef".into()),
            }
        );
    }

    #[test]
    fn parses_slug_entities_and_team_and_cycle() {
        assert_eq!(
            ok("https://linear.app/acme/project/my-project-0123456789ab/issues"),
            LinearUrlRef::Project {
                workspace: "acme".into(),
                slug_id: "0123456789ab".into(),
            }
        );
        assert_eq!(
            ok("https://linear.app/acme/team/eng/cycle/5"),
            LinearUrlRef::Cycle {
                workspace: "acme".into(),
                team_key: "ENG".into(),
                cycle: CycleRef::Number(5),
            }
        );
        assert_eq!(
            ok("https://linear.app/acme/team/eng/cycle/upcoming"),
            LinearUrlRef::Cycle {
                workspace: "acme".into(),
                team_key: "ENG".into(),
                cycle: CycleRef::Next,
            }
        );
        assert_eq!(
            ok("https://linear.app/acme/team/eng/all"),
            LinearUrlRef::Team {
                workspace: "acme".into(),
                team_key: "ENG".into(),
            }
        );
    }

    #[test]
    fn refuses_unknown_entities_and_pages() {
        assert!(matches!(
            parse_linear_url("https://linear.app/acme/settings/profile"),
            LinearUrlParse::Unsupported { .. }
        ));
        assert!(matches!(
            parse_linear_url("https://linear.app/acme/project/abc123/bogus"),
            LinearUrlParse::Unsupported { .. }
        ));
        assert!(matches!(
            parse_linear_url("https://linear.app/acme/team/eng/cycle/nope"),
            LinearUrlParse::Unsupported { .. }
        ));
    }

    #[test]
    fn rejects_lookalike_hosts_ports_and_credentials() {
        assert_eq!(
            parse_linear_url("https://linear.app.example.com/acme/issue/ENG-1"),
            LinearUrlParse::NotLinearUrl
        );
        assert_eq!(
            parse_linear_url("https://linear.app:8443/acme/issue/ENG-1"),
            LinearUrlParse::NotLinearUrl
        );
        assert_eq!(
            parse_linear_url("https://user@linear.app/acme/issue/ENG-1"),
            LinearUrlParse::NotLinearUrl
        );
    }

    #[test]
    fn reject_comment_url_reports_comment_links() {
        assert!(reject_comment_url("https://linear.app/acme/issue/ENG-42#comment-deadbeef").is_err());
        assert!(reject_comment_url("https://linear.app/acme/issue/ENG-42").is_ok());
        assert!(reject_comment_url("ENG-42").is_ok());
    }

    #[test]
    fn reject_linear_url_only_fires_on_linear_urls() {
        assert!(reject_linear_url("https://linear.app/acme/issue/ENG-1", "a label").is_err());
        assert!(reject_linear_url("ENG-1", "a label").is_ok());
    }

    #[test]
    fn expect_kind_returns_none_for_plain_input() {
        assert!(expect_linear_url_kind("ENG-1", "issue", "an issue")
            .unwrap()
            .is_none());
    }
}
