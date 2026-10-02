//! Issue references mined out of free text: commit messages, pull-request
//! titles and bodies.
//!
//! This mirrors Linear's first-party git integrations: a closing keyword in
//! front of an identifier moves the issue to a completed state, while a bare
//! mention only links it.
//!
//! Hand-written rather than `regex`, deliberately: the pattern is a small,
//! fixed grammar (an uppercase team key, a hyphen, digits) and the crate that
//! ships to servers does not need the regex engine's cost for it. The rules it
//! implements are stated in full below, and tested against the same cases the
//! `regex`-based original was.

use std::collections::HashSet;

/// How a reference was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceKind {
    /// `fixes VED-12`: also closes the issue.
    Close,
    /// `VED-12`: links only.
    Mention,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    /// As written, e.g. `VED-12`.
    pub identifier: String,
    /// `VED`.
    pub team_key: String,
    /// `12`.
    pub number: u64,
    pub kind: ReferenceKind,
}

impl Reference {
    pub fn is_closing(&self) -> bool {
        self.kind == ReferenceKind::Close
    }
}

/// Closing keywords, as the forge platforms spell them.
pub const CLOSING_KEYWORDS: &[&str] = &[
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// Extract every issue reference from a block of text.
///
/// An identifier appears at most once, in first-seen order, and a closing
/// reference always wins over a bare mention of the same identifier.
pub fn extract(text: &str) -> Vec<Reference> {
    if text.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();

    let mut found: Vec<Reference> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Closing references first, so they take precedence over bare mentions.
    let mut index = 0;
    while index < chars.len() {
        let Some(keyword_len) = closing_keyword_at(&chars, &lower, index) else {
            index += 1;
            continue;
        };
        let mut cursor = index + keyword_len;
        if !skip_word_boundary(&chars, cursor) {
            index += 1;
            continue;
        }
        cursor = skip_whitespace(&chars, cursor);
        if cursor == index + keyword_len {
            // The keyword must be *followed* by whitespace: `fixesVED-1` is not
            // a reference.
            index += 1;
            continue;
        }
        if let Some(after_issue) = skip_word(&chars, &lower, cursor, "issue") {
            let spaced = skip_whitespace(&chars, after_issue);
            if spaced > after_issue {
                cursor = spaced;
            }
        }
        if let Some(reference) = identifier_at(&chars, cursor) {
            if seen.insert(reference.identifier.clone()) {
                found.push(Reference {
                    kind: ReferenceKind::Close,
                    ..reference
                });
            }
            index = cursor;
            continue;
        }
        index += 1;
    }

    // Then every bare mention.
    let mut index = 0;
    while index < chars.len() {
        if !is_identifier_start(&chars, index) {
            index += 1;
            continue;
        }
        match identifier_at(&chars, index) {
            Some(reference) => {
                let width = reference.identifier.chars().count();
                if seen.insert(reference.identifier.clone()) {
                    found.push(reference);
                }
                index += width.max(1);
            }
            None => index += 1,
        }
    }

    found
}

/// Keep only the references whose team key is one this deployment owns.
///
/// An unknown team key is someone quoting an unrelated ticket; acting on it would
/// edit an issue the deployment has no business touching.
pub fn filter_by_team_keys(references: Vec<Reference>, team_keys: &[String]) -> Vec<Reference> {
    references
        .into_iter()
        .filter(|reference| {
            team_keys
                .iter()
                .any(|key| key.eq_ignore_ascii_case(&reference.team_key))
        })
        .collect()
}

/// `\b(?:close|fixes|…)\b` at `index`, case-insensitively.
fn closing_keyword_at(chars: &[char], lower: &[char], index: usize) -> Option<usize> {
    if index > 0 && is_word_char(chars[index - 1]) {
        return None;
    }
    // Longest first, so `closes` is not shadowed by `close`.
    let mut keywords: Vec<&&str> = CLOSING_KEYWORDS.iter().collect();
    keywords.sort_by_key(|keyword| std::cmp::Reverse(keyword.len()));
    for keyword in keywords {
        let length = keyword.chars().count();
        if index + length > chars.len() {
            continue;
        }
        let matches = keyword
            .chars()
            .enumerate()
            .all(|(offset, expected)| lower[index + offset] == expected);
        if !matches {
            continue;
        }
        // `\b` after the keyword: the next character must end the word.
        if chars
            .get(index + length)
            .is_some_and(|character| is_word_char(*character))
        {
            continue;
        }
        return Some(length);
    }
    None
}

/// The word `expected` at `index`, followed by a dimension of whitespace.
fn skip_word(chars: &[char], lower: &[char], index: usize, expected: &str) -> Option<usize> {
    let length = expected.chars().count();
    if index + length > chars.len() {
        return None;
    }
    if !expected
        .chars()
        .enumerate()
        .all(|(offset, character)| lower[index + offset] == character)
    {
        return None;
    }
    if chars
        .get(index + length)
        .is_some_and(|character| is_word_char(*character))
    {
        return None;
    }
    Some(index + length)
}

/// `([A-Z][A-Z0-9]*)-(\d+)` at `index`, with word boundaries.
fn identifier_at(chars: &[char], index: usize) -> Option<Reference> {
    if index > 0 && is_word_char(chars[index - 1]) {
        return None;
    }
    if !chars[index].is_ascii_uppercase() {
        return None;
    }
    let mut cursor = index;
    while cursor < chars.len()
        && (chars[cursor].is_ascii_uppercase() || chars[cursor].is_ascii_digit())
    {
        cursor += 1;
    }
    let team_key: String = chars[index..cursor].iter().collect();
    if chars.get(cursor) != Some(&'-') {
        return None;
    }
    let digits_start = cursor + 1;
    cursor = digits_start;
    while cursor < chars.len() && chars[cursor].is_ascii_digit() {
        cursor += 1;
    }
    if cursor == digits_start {
        return None;
    }
    if chars
        .get(cursor)
        .is_some_and(|character| is_word_char(*character))
    {
        return None;
    }
    let digits: String = chars[digits_start..cursor].iter().collect();
    let number = digits.parse::<u64>().ok()?;
    Some(Reference {
        identifier: format!("{team_key}-{number}"),
        team_key,
        number,
        kind: ReferenceKind::Mention,
    })
}

fn is_identifier_start(chars: &[char], index: usize) -> bool {
    chars[index].is_ascii_uppercase()
        && chars
            .get(index + 1)
            .is_some_and(|c| *c == '-' || c.is_ascii_digit() || c.is_ascii_uppercase())
}

/// JavaScript's `\b`: word characters are `[A-Za-z0-9_]`.
fn is_word_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// The `\b` that must follow a keyword: the character after it exists and is not
/// a word character (or the text ends there).
fn skip_word_boundary(chars: &[char], index: usize) -> bool {
    chars
        .get(index)
        .is_none_or(|character| !is_word_char(*character))
}

fn skip_whitespace(chars: &[char], mut index: usize) -> usize {
    while chars.get(index).is_some_and(|c| c.is_whitespace()) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identifiers(text: &str) -> Vec<String> {
        extract(text)
            .into_iter()
            .map(|reference| {
                format!(
                    "{}:{}",
                    reference.identifier,
                    match reference.kind {
                        ReferenceKind::Close => "close",
                        ReferenceKind::Mention => "mention",
                    }
                )
            })
            .collect()
    }

    #[test]
    fn a_closing_keyword_closes_and_a_bare_mention_links() {
        assert_eq!(identifiers("fixes VED-12"), vec!["VED-12:close"]);
        assert_eq!(identifiers("Closes VED-12"), vec!["VED-12:close"]);
        assert_eq!(identifiers("resolved issue VED-12"), vec!["VED-12:close"]);
        assert_eq!(identifiers("VED-12 was mentioned"), vec!["VED-12:mention"]);
        assert_eq!(identifiers("no references here"), Vec::<String>::new());
        assert_eq!(identifiers(""), Vec::<String>::new());
    }

    #[test]
    fn a_closing_reference_wins_over_a_mention_of_the_same_identifier() {
        let found = identifiers("VED-12 needs work; fixes VED-12 today");
        assert_eq!(found, vec!["VED-12:close"]);
    }

    #[test]
    fn an_identifier_is_reported_once_in_first_seen_order() {
        assert_eq!(
            identifiers("see VED-1 and VED-2, also VED-1"),
            vec!["VED-1:mention", "VED-2:mention"]
        );
    }

    #[test]
    fn a_keyword_without_whitespace_is_not_a_closing_reference() {
        // `fixesVED-1` has no word boundary after the keyword, and the mention
        // pattern cannot match either: `V` is preceded by a word character. The
        // original `regex` behaves the same way, so this is parity, not a gap.
        assert_eq!(identifiers("fixesVED-1"), Vec::<String>::new());
        assert_eq!(identifiers("prefixes VED-1"), vec!["VED-1:mention"]);
        assert_eq!(identifiers("fix VED-1"), vec!["VED-1:close"]);
    }

    #[test]
    fn multi_commit_messages_behave_like_plain_text() {
        let message = "feat: something\n\nFixes VED-3\nRefs VED-4";
        assert_eq!(identifiers(message), vec!["VED-3:close", "VED-4:mention"]);
    }

    #[test]
    fn a_lowercase_identifier_is_not_a_reference() {
        assert_eq!(identifiers("ved-12"), Vec::<String>::new());
        assert_eq!(identifiers("Ved-12"), Vec::<String>::new());
    }

    #[test]
    fn identifiers_need_a_delimited_boundary() {
        // The `\b` rules from the original. The team key is greedy, so a word
        // glued to an identifier becomes part of the *key* (`WORDVED-1`), which
        // is why filtering by team key matters: `WORDVED` is not a team we own.
        assert_eq!(identifiers("WORDVED-1"), vec!["WORDVED-1:mention"]);
        assert_eq!(identifiers("VED-1WORD"), Vec::<String>::new());
        assert_eq!(identifiers("VED-1-2"), vec!["VED-1:mention"]);
        assert_eq!(identifiers("(VED-9)"), vec!["VED-9:mention"]);
        assert_eq!(identifiers("VED-"), Vec::<String>::new());
    }

    #[test]
    fn team_keys_with_digits_and_an_overflowing_number() {
        assert_eq!(identifiers("V2-3"), vec!["V2-3:mention"]);
        // A number too large for u64 is not a reference rather than a panic.
        assert_eq!(
            identifiers("VED-99999999999999999999999999"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn filtering_keeps_only_the_team_keys_this_deployment_owns() {
        let references = extract("fixes VED-1, closes ENG-2, mentions OPS-3");
        let filtered = filter_by_team_keys(references, &["ved".to_string()]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].identifier, "VED-1");
        assert!(filtered[0].is_closing());
    }

    #[test]
    fn non_ascii_text_does_not_confuse_the_scanner() {
        assert_eq!(
            identifiers("修复 VED-7 —— 见 ENG-8"),
            vec!["VED-7:mention", "ENG-8:mention"]
        );
    }
}
