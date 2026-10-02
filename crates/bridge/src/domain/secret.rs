//! Secrets, wrapped so they cannot be printed by accident.

use std::fmt;

use crate::domain::ConnectorId;

/// A signing secret or API token.
///
/// The `Debug` impl redacts, which makes every `format!("{config:?}")`, log line
/// and `--json` rendering safe by default - the alternative (remembering to
/// redact at each call site) is how credentials end up in transcripts.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Read the secret. Named `expose` so that a call site is visible in review.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Length is the only property of a secret that is safe to report.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(<{} bytes, redacted>)", self.0.len())
    }
}

/// Parses a `connector:name` reference used by mappings (`linear:VED`).
pub fn parse_connector_ref(value: &str) -> Result<(ConnectorId, String), String> {
    let (connector, scope) = value
        .split_once(':')
        .ok_or_else(|| format!("`{value}` must be `connector:scope`, e.g. `linear:VED`"))?;
    if connector.is_empty() || scope.is_empty() {
        return Err(format!(
            "`{value}` must be `connector:scope`, e.g. `linear:VED`"
        ));
    }
    Ok((ConnectorId::new(connector), scope.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_is_redacted() {
        let secret = Secret::new("0123456789abcdef");
        let rendered = format!("{secret:?}");
        assert!(!rendered.contains("0123456789abcdef"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
        assert!(rendered.contains("16 bytes"), "{rendered}");
    }

    #[test]
    fn connector_refs_parse_and_reject() {
        let (id, scope) = parse_connector_ref("linear:VED").unwrap();
        assert_eq!(id.as_str(), "linear");
        assert_eq!(scope, "VED");
        assert!(parse_connector_ref("nope").is_err());
        assert!(parse_connector_ref(":VED").is_err());
        assert!(parse_connector_ref("linear:").is_err());
    }
}
