//! Content hashing, in one place.

use sha2::{Digest, Sha256};

/// Hex-encoded SHA-256.
///
/// Used for the field signature (the "has anything actually changed?" check
/// behind echo suppression) and for the delivery id of a provider that sends
/// none.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_is_64_hex_characters_and_stable() {
        let hash = sha256_hex(b"hello");
        assert_eq!(hash.len(), 64);
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(sha256_hex(b"hello"), hash);
        assert_ne!(sha256_hex(b"hellp"), hash);
    }
}
