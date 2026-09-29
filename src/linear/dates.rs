use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

fn iso_date_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\d{4}-\d{2}-\d{2}(T[\d:.]+Z?([+-]\d{2}:?\d{2})?)?$")
            .expect("valid ISO date regex")
    })
}

fn compact_offset_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([+-]\d{2})(\d{2})$").expect("valid offset regex"))
}

/// Parse an ISO-8601-ish date the way `new Date(value)` does for the shapes the
/// CLI accepts, normalised to UTC. Timezone-less datetimes are treated as UTC
/// rather than the host's local zone so a script produces the same filter
/// everywhere.
fn parse_iso_utc(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }

    // RFC 3339 wants `+HH:MM`; callers may paste `+HHMM`.
    if let Some(captures) = compact_offset_regex().captures(value) {
        let whole = captures.get(0)?;
        let normalized = format!(
            "{}{}:{}",
            &value[..whole.start()],
            captures.get(1)?.as_str(),
            captures.get(2)?.as_str()
        );
        if let Ok(parsed) = DateTime::parse_from_rfc3339(&normalized) {
            return Some(parsed.with_timezone(&Utc));
        }
    }

    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(value, format) {
            return Some(Utc.from_utc_datetime(&naive));
        }
    }

    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        let midnight = date.and_hms_opt(0, 0, 0)?;
        return Some(Utc.from_utc_datetime(&midnight));
    }

    None
}

/// Validate and normalise a date filter value to an ISO timestamp string.
pub fn parse_date_filter(value: &str, flag_name: &str) -> Result<String> {
    if !iso_date_regex().is_match(value) {
        return Err(CliError::validation(format!(
            "Invalid date format for {flag_name}: \"{value}\""
        ))
        .suggestion(
            "Use YYYY-MM-DD or ISO 8601 format (e.g. 2024-01-15 or 2024-01-15T09:00:00Z).",
        ));
    }
    match parse_iso_utc(value) {
        Some(parsed) => Ok(parsed.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()),
        None => Err(CliError::validation(format!(
            "Invalid date for {flag_name}: \"{value}\""
        ))
        .suggestion(
            "Use YYYY-MM-DD or ISO 8601 format (e.g. 2024-01-15 or 2024-01-15T09:00:00Z).",
        )),
    }
}

