//! sha256 digests rendered as lowercase hex.
//!
//! Six copies of this lived across the workspace, written four different ways — a hex lookup
//! table, `write!("{byte:02x}")`, `format!("{:x}", finalize())`, and the table inlined into the
//! digest loop. All four agreed, which is the point: these strings cross process boundaries and
//! are compared byte for byte. The agent decides whether to replace its own binary by comparing
//! the sha of what it has against the sha the console distributes, and artifact blobs are keyed
//! by content sha. A copy that rendered uppercase, or dropped a leading zero, would not be a
//! formatting difference — it would be a machine deciding it holds the wrong bytes.

use sha2::{Digest, Sha256};

/// Lowercase hex, two digits per byte, leading zeros kept.
pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// The sha256 of `bytes` as 64 lowercase hex digits.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_lower_pads_every_byte_to_two_digits() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }

    #[test]
    fn sha256_hex_matches_the_known_digest_of_the_empty_input() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
