//! Issue identifier parsing. Port of `src/utils/issue-identifier.ts`.
//!
//! Linear issue identifiers are `TEAMKEY-NUMBER`, where the team key is
//! alphanumeric and the number is a positive integer with no leading zero
//! (Linear never issues `ENG-0` or `ENG-007`). The team key is normalised to
//! upper case, matching the API's own spelling.

use regex::Regex;
use std::sync::LazyLock;

/// A whole string that is exactly `TEAMKEY-NUMBER`.
static LINEAR_IDENTIFIER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([a-zA-Z0-9]+)-([1-9][0-9]*)$").expect("valid regex"));

/// The first `TEAMKEY-NUMBER`-shaped token anywhere in a larger string.
static LINEAR_IDENTIFIER_IN_TEXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([a-zA-Z0-9]+)-([1-9][0-9]*)\b").expect("valid regex"));

/// A parsed `TEAMKEY-NUMBER` reference with the team key upper-cased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedIssueIdentifier {
    pub identifier: String,
    pub team_key: String,
    pub issue_number: String,
}

fn build(team_key: &str, issue_number: &str) -> ParsedIssueIdentifier {
    let team_key = team_key.to_uppercase();
    ParsedIssueIdentifier {
        identifier: format!("{team_key}-{issue_number}"),
        team_key,
        issue_number: issue_number.to_string(),
    }
}

fn parse_with(regex: &Regex, value: &str) -> Option<ParsedIssueIdentifier> {
    let captures = regex.captures(value)?;
    let team_key = captures.get(1)?.as_str();
    let issue_number = captures.get(2)?.as_str();
    Some(build(team_key, issue_number))
}

/// Parse a value that must be exactly one issue identifier.
pub fn parse_issue_identifier(value: &str) -> Option<ParsedIssueIdentifier> {
    parse_with(&LINEAR_IDENTIFIER_RE, value)
}

/// Find the first issue identifier embedded in free text.
pub fn find_issue_identifier_in_text(value: &str) -> Option<ParsedIssueIdentifier> {
    parse_with(&LINEAR_IDENTIFIER_IN_TEXT_RE, value)
}

/// The upper-cased team key of an issue identifier, when `value` is one.
pub fn get_team_key_from_issue_identifier(value: &str) -> Option<String> {
    parse_issue_identifier(value).map(|parsed| parsed.team_key)
}

/// The normalised `TEAMKEY-NUMBER` spelling, when `value` is an identifier.
pub fn normalize_issue_identifier(value: &str) -> Option<String> {
    parse_issue_identifier(value).map(|parsed| parsed.identifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_uppercases_team_key() {
        let parsed = parse_issue_identifier("eng-123").unwrap();
        assert_eq!(parsed.identifier, "ENG-123");
        assert_eq!(parsed.team_key, "ENG");
        assert_eq!(parsed.issue_number, "123");
    }

    #[test]
    fn rejects_non_identifiers() {
        assert!(parse_issue_identifier("ENG-0").is_none());
        assert!(parse_issue_identifier("ENG-007").is_none());
        assert!(parse_issue_identifier("ENG-").is_none());
        assert!(parse_issue_identifier("ENG 123").is_none());
        assert!(parse_issue_identifier("-123").is_none());
    }

    #[test]
    fn finds_identifier_in_text() {
        let parsed = find_issue_identifier_in_text("see abc-9 for details").unwrap();
        assert_eq!(parsed.identifier, "ABC-9");
        assert!(find_issue_identifier_in_text("no identifier here").is_none());
    }

    #[test]
    fn helpers_normalize_and_extract() {
        assert_eq!(
            normalize_issue_identifier("eng-1").as_deref(),
            Some("ENG-1")
        );
        assert_eq!(
            get_team_key_from_issue_identifier("eng-1").as_deref(),
            Some("ENG")
        );
        assert!(normalize_issue_identifier("nope").is_none());
    }
}
