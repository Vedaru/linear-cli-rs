//! Signature verification, in one place.
//!
//! Every connector's deliveries are authenticated here, whatever platform they
//! come from: the platform supplies the header names, the algorithm and an
//! optional prefix, and this module is the only code that compares anything. The
//! alternative - each connector doing its own comparison - is how one of them ends
//! up using `==` instead of a constant-time comparison, or verifying a
//! re-serialised body instead of the bytes that were signed.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::connector::{Algorithm, HeaderMap, Reject, SignatureScheme};
use crate::domain::Secret;

type HmacSha256 = Hmac<Sha256>;

/// Verify a delivery against its connector's scheme.
pub fn verify(
    secret: &Secret,
    scheme: &SignatureScheme,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), Reject> {
    let (_, provided) = scheme.lookup(headers).ok_or_else(|| {
        Reject::MissingHeader(scheme.headers.first().cloned().unwrap_or_default())
    })?;
    match scheme.algorithm {
        Algorithm::HmacSha256 => verify_hmac_hex(secret, scheme, provided, body),
        Algorithm::Token => verify_token(secret, provided),
    }
}

/// Verify a hex-encoded HMAC-SHA256 signature over the raw request body.
///
/// The comparison is constant time (`verify_slice`), so a wrong signature cannot
/// be discovered byte-by-byte. A signature of the wrong length, or one that is
/// not hex, is refused before any comparison happens.
fn verify_hmac_hex(
    secret: &Secret,
    scheme: &SignatureScheme,
    provided: &str,
    body: &[u8],
) -> Result<(), Reject> {
    let provided = match &scheme.prefix {
        Some(prefix) => provided
            .trim()
            .strip_prefix(prefix.as_str())
            .ok_or(Reject::BadSignature)?,
        None => provided.trim(),
    };
    let provided = decode_hex(provided).ok_or(Reject::BadSignature)?;

    // `new_from_slice` accepts a key of any length for HMAC; it cannot fail.
    let mut mac = HmacSha256::new_from_slice(secret.expose().as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(body);
    mac.verify_slice(&provided)
        .map_err(|_| Reject::BadSignature)
}

/// Verify a shared secret sent verbatim in a header (GitLab's `X-Gitlab-Token`).
fn verify_token(secret: &Secret, provided: &str) -> Result<(), Reject> {
    let expected = secret.expose().as_bytes();
    let provided = provided.trim().as_bytes();
    // Length is compared first, which leaks only the length of the secret - and a
    // secret whose length is guessable from a timing probe is not the problem
    // worth solving here. The byte comparison itself is constant time.
    let mut difference = (expected.len() ^ provided.len()) as u8;
    for index in 0..expected.len().min(provided.len()) {
        difference |= expected[index] ^ provided[index];
    }
    if difference == 0 && expected.len() == provided.len() {
        Ok(())
    } else {
        Err(Reject::BadSignature)
    }
}

/// Deterministic delivery id for providers that send none: a truncated SHA-256
/// of the raw body. Truncation is safe here - the id only has to be stable and
/// unique per delivery, and the full body is stored alongside it.
pub fn body_digest(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Strict hex decoding: odd length or a non-hex digit is a failure, never a
/// best-effort guess.
pub fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() % 2 != 0 {
        return None;
    }
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(value.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        out.push((high << 4) | low);
    }
    Some(out)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::SignatureScheme;

    const SECRET: &str = "0123456789abcdef";
    const BODY: &[u8] = br#"{"action":"create","type":"Issue"}"#;

    fn scheme() -> SignatureScheme {
        SignatureScheme::hmac_sha256(&["linear-signature"])
    }

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn headers(signature: &str) -> HeaderMap {
        HeaderMap::from_pairs([("Linear-Signature", signature)])
    }

    #[test]
    fn accepts_a_correct_signature() {
        let secret = Secret::new(SECRET);
        assert_eq!(
            verify(&secret, &scheme(), &headers(&sign(SECRET, BODY)), BODY),
            Ok(())
        );
    }

    #[test]
    fn accepts_uppercase_hex() {
        let secret = Secret::new(SECRET);
        let signature = sign(SECRET, BODY).to_uppercase();
        assert_eq!(
            verify(&secret, &scheme(), &headers(&signature), BODY),
            Ok(())
        );
    }

    #[test]
    fn rejects_a_tampered_body() {
        let secret = Secret::new(SECRET);
        let signature = sign(SECRET, BODY);
        let tampered = br#"{"action":"create","type":"Comment"}"#;
        assert_eq!(
            verify(&secret, &scheme(), &headers(&signature), tampered),
            Err(Reject::BadSignature)
        );
    }

    #[test]
    fn rejects_the_wrong_secret() {
        let secret = Secret::new("ffffffffffffffff");
        assert_eq!(
            verify(&secret, &scheme(), &headers(&sign(SECRET, BODY)), BODY),
            Err(Reject::BadSignature)
        );
    }

    #[test]
    fn rejects_missing_empty_short_and_non_hex_signatures() {
        let secret = Secret::new(SECRET);
        assert_eq!(
            verify(&secret, &scheme(), &HeaderMap::default(), BODY),
            Err(Reject::MissingHeader("linear-signature".into()))
        );
        assert_eq!(
            verify(&secret, &scheme(), &headers(""), BODY),
            Err(Reject::BadSignature)
        );
        assert_eq!(
            verify(&secret, &scheme(), &headers("abcd"), BODY),
            Err(Reject::BadSignature)
        );
        assert_eq!(
            verify(&secret, &scheme(), &headers("zzzz"), BODY),
            Err(Reject::BadSignature)
        );
    }

    #[test]
    fn a_configured_prefix_is_required_and_stripped() {
        let scheme = SignatureScheme {
            headers: vec!["x-hub-signature-256".into()],
            algorithm: Algorithm::HmacSha256,
            prefix: Some("sha256=".into()),
        };
        let secret = Secret::new(SECRET);
        let prefixed = format!("sha256={}", sign(SECRET, BODY));
        let headers = HeaderMap::from_pairs([("X-Hub-Signature-256", prefixed.clone())]);
        assert_eq!(verify(&secret, &scheme, &headers, BODY), Ok(()));

        // The same digest without its prefix is refused rather than accepted by
        // a lenient fallback.
        let bare = HeaderMap::from_pairs([("X-Hub-Signature-256", sign(SECRET, BODY))]);
        assert_eq!(
            verify(&secret, &scheme, &bare, BODY),
            Err(Reject::BadSignature)
        );
    }

    #[test]
    fn a_token_scheme_compares_the_secret_verbatim() {
        let scheme = SignatureScheme {
            headers: vec!["x-gitlab-token".into()],
            algorithm: Algorithm::Token,
            prefix: None,
        };
        let secret = Secret::new(SECRET);
        let right = HeaderMap::from_pairs([("X-Gitlab-Token", SECRET.to_string())]);
        assert_eq!(verify(&secret, &scheme, &right, BODY), Ok(()));

        let wrong = HeaderMap::from_pairs([("X-Gitlab-Token", "aaaaaaaaaaaaaaaa".to_string())]);
        assert_eq!(
            verify(&secret, &scheme, &wrong, BODY),
            Err(Reject::BadSignature)
        );

        let short = HeaderMap::from_pairs([("X-Gitlab-Token", "0123".to_string())]);
        assert_eq!(
            verify(&secret, &scheme, &short, BODY),
            Err(Reject::BadSignature)
        );

        let missing = HeaderMap::default();
        assert_eq!(
            verify(&secret, &scheme, &missing, BODY),
            Err(Reject::MissingHeader("x-gitlab-token".into()))
        );
    }

    #[test]
    fn hex_decoding_is_strict() {
        assert_eq!(decode_hex("00ff"), Some(vec![0x00, 0xff]));
        assert_eq!(decode_hex("abc"), None, "odd length");
        assert_eq!(decode_hex(""), None);
        assert_eq!(decode_hex("0g"), None);
    }

    #[test]
    fn body_digest_is_stable_and_distinguishing() {
        assert_eq!(body_digest(BODY), body_digest(BODY));
        assert_ne!(body_digest(BODY), body_digest(b"{}"));
        assert_eq!(body_digest(BODY).len(), 16);
    }
}
