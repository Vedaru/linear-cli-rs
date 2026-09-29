use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// UUIDs and documents
// ---------------------------------------------------------------------------

/// `true` for a Linear UUID, which every resolve step can pass through
/// untouched.
pub fn is_linear_uuid(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
            .expect("valid uuid regex")
    });
    re.is_match(value)
}

/// Resolve a document reference to a slug ID: a pasted document URL yields its
/// slug, anything else (a slug ID or UUID) is returned as-is. A URL for another
/// entity type is refused rather than treated as a slug.
pub fn resolve_document_reference(input: &str) -> Result<String> {
    if let Some(LinearUrlRef::Document { slug_id, .. }) =
        expect_linear_url_kind(input, "document", "a document URL, UUID, or slug ID")?
    {
        return Ok(slug_id);
    }
    Ok(input.to_string())
}

