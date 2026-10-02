//! Hidden markers on mirrored content.
//!
//! A marker lets this service recognise text it wrote itself, which is the
//! second line of defence against sync feedback loops (the first being the link
//! store, which knows what has already been mirrored). They are HTML comments so
//! they render invisibly in both Linear and the forges.
//!
//! Only *comments* carry a marker. Linear renders an HTML comment inside an issue
//! description as visible text, so a marked issue body would leak the marker to
//! the reader - and the link store already covers issues.

/// Marker namespace. Versioned by name so text written by a previous bridge is
/// recognisable rather than mistaken for a user's own words.
pub const MARKER_PREFIX: &str = "linear-bridge";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginMarker {
    /// Connector the content came from, e.g. `forgejo`.
    pub connector: String,
    /// Stable id of the originating object (a comment id, typically).
    pub id: String,
}

impl OriginMarker {
    pub fn new(connector: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            connector: connector.into(),
            id: id.into(),
        }
    }
}

pub fn render(marker: &OriginMarker) -> String {
    format!(
        "<!-- {MARKER_PREFIX}:{}:{} -->",
        marker.connector, marker.id
    )
}

/// The first marker in `text`, if any.
pub fn parse(text: &str) -> Option<OriginMarker> {
    let start = text.find("<!--")?;
    let rest = &text[start + 4..];
    let end = rest.find("-->")?;
    let body = rest[..end].trim();
    let body = body.strip_prefix(MARKER_PREFIX)?;
    let body = body.strip_prefix(':')?;
    let (connector, id) = body.split_once(':')?;
    let connector = connector.trim();
    let id = id.trim();
    if connector.is_empty() || id.is_empty() {
        return None;
    }
    Some(OriginMarker::new(connector, id))
}

pub fn has_marker(text: &str) -> bool {
    parse(text).is_some()
}

/// Append a marker to a body, separated so it stays out of the way.
pub fn with_marker(body: &str, marker: &OriginMarker) -> String {
    format!("{body}\n\n{}", render(marker))
}

/// Remove every marker from a body.
///
/// Used before hashing: a body that differs from the last synced one only by its
/// marker is not a change, and treating it as one is how a mirror starts echoing
/// itself.
pub fn strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        let after = &rest[start + 4..];
        match after.find("-->") {
            Some(end) if after[..end].trim().starts_with(MARKER_PREFIX) => {
                out.push_str(&rest[..start]);
                rest = &after[end + 3..];
            }
            _ => {
                // Not one of ours (a user's own HTML comment): keep it verbatim.
                let keep = start + 4;
                out.push_str(&rest[..keep]);
                rest = &rest[keep..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_round_trips() {
        let marker = OriginMarker::new("forgejo", "12");
        let body = with_marker("**vedaru** wrote:\n\nhello", &marker);
        assert!(body.starts_with("**vedaru** wrote:"));
        assert!(has_marker(&body));
        assert_eq!(parse(&body), Some(marker));
    }

    #[test]
    fn markers_are_recognised_only_for_this_bridge() {
        assert_eq!(parse("<!-- something-else:x:1 -->"), None);
        assert_eq!(parse("<!-- linear-bridge:forgejo: -->"), None, "no id");
        assert_eq!(parse("no comment here"), None);
        assert!(!has_marker("<!--  linforge:linear:9  -->"));
    }

    #[test]
    fn stripping_removes_ours_and_keeps_everything_else() {
        let marked = "body\n\n<!-- linear-bridge:linear:abc -->";
        assert_eq!(strip(marked).trim_end(), "body");

        let user_comment = "body\n\n<!-- keep me -->";
        assert_eq!(strip(user_comment), user_comment);

        let two = "a\n<!-- linear-bridge:linear:1 -->\nb\n<!-- linear-bridge:forgejo:2 -->";
        assert_eq!(strip(two).trim(), "a\n\nb".trim());
    }

    #[test]
    fn stripping_handles_an_unterminated_comment() {
        let text = "body <!-- linear-bridge:linear:1";
        assert_eq!(strip(text), text, "an unterminated comment is left alone");
    }
}
