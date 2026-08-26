use brocade_core::hash::{hex_lower, sha256_hex};

use crate::{Result, StoreError};
use base64::{
    engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use x25519_dalek::{PublicKey, StaticSecret};

pub const REALITY_SHORT_ID_BYTES: usize = 8;
pub const REALITY_SHORT_ID_HEX_LEN: usize = REALITY_SHORT_ID_BYTES * 2;
pub const NODE_TOKEN_PREFIX: &str = "broc_node_";
pub const ADMIN_TOKEN_PREFIX: &str = "broc_admin_";
pub const ADMIN_SESSION_TOKEN_PREFIX: &str = "broc_session_";
pub const ENROLLMENT_TOKEN_PREFIX: &str = "broc_enroll_";
pub const NODE_TOKEN_BYTES: usize = 32;
pub const NODE_TOKEN_HEX_LEN: usize = NODE_TOKEN_BYTES * 2;
pub const NODE_TOKEN_LEN: usize = NODE_TOKEN_PREFIX.len() + NODE_TOKEN_HEX_LEN;
pub const NODE_TOKEN_DISPLAY_PREFIX_LEN: usize = NODE_TOKEN_PREFIX.len() + 16;
pub const ADMIN_TOKEN_LEN: usize = ADMIN_TOKEN_PREFIX.len() + NODE_TOKEN_HEX_LEN;
pub const ADMIN_SESSION_TOKEN_LEN: usize = ADMIN_SESSION_TOKEN_PREFIX.len() + NODE_TOKEN_HEX_LEN;
pub const ADMIN_TOKEN_DISPLAY_PREFIX_LEN: usize = ADMIN_TOKEN_PREFIX.len() + 16;
pub const ADMIN_PASSWORD_BYTES: usize = 12;
pub const ENROLLMENT_TOKEN_LEN: usize = ENROLLMENT_TOKEN_PREFIX.len() + NODE_TOKEN_HEX_LEN;
pub const ENROLLMENT_TOKEN_DISPLAY_PREFIX_LEN: usize = ENROLLMENT_TOKEN_PREFIX.len() + 16;
pub const WIREGUARD_KEY_BYTES: usize = 32;
pub const WIREGUARD_KEY_BASE64_LEN: usize = 44;
/// Fixed by the method: `2022-blake3-aes-128-gcm` takes a 128-bit key.
const SHADOWSOCKS_PSK_BYTES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireGuardKeypair {
    pub private_key: String,
    pub public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealityKeypair {
    pub private_key: String,
    pub public_key: String,
}

pub fn generate_uuid_v4() -> Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    ))
}

pub fn generate_reality_short_id() -> Result<String> {
    let mut bytes = [0_u8; REALITY_SHORT_ID_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(hex_lower(&bytes))
}

/// One Shadowsocks 2022 pre-shared key: 16 random bytes in standard base64, which is the
/// length `2022-blake3-aes-128-gcm` requires and the encoding xray reads.
///
/// Standard base64 with padding rather than the URL-safe unpadded form the X25519 keys use.
/// That is not a style choice — shadowsocks tooling everywhere emits and accepts this form
/// (`openssl rand -base64 16`), and a key that will not paste into the other implementations
/// somebody is checking against is a key that gets worked around.
///
/// A relay port needs two of these, and they must be drawn separately: the port-wide key and
/// the account key under it are layered, and reusing one for both collapses the layering that
/// gives arriving traffic an identity.
pub fn generate_shadowsocks_psk() -> Result<String> {
    let mut bytes = [0_u8; SHADOWSOCKS_PSK_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(BASE64_STANDARD.encode(bytes))
}

pub fn generate_node_token() -> Result<String> {
    let mut bytes = [0_u8; NODE_TOKEN_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(format!("{}{}", NODE_TOKEN_PREFIX, hex_lower(&bytes)))
}

pub fn generate_admin_token() -> Result<String> {
    let mut bytes = [0_u8; NODE_TOKEN_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(format!("{}{}", ADMIN_TOKEN_PREFIX, hex_lower(&bytes)))
}

pub fn generate_admin_session_token() -> Result<String> {
    let mut bytes = [0_u8; NODE_TOKEN_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(format!(
        "{}{}",
        ADMIN_SESSION_TOKEN_PREFIX,
        hex_lower(&bytes)
    ))
}

// The one-time password generated on a reset: returned once in the response, with only an
// argon2 hash kept in the database as usual. URL-safe base64 so that it can be copied out of
// a terminal without error.
pub fn generate_admin_password() -> Result<String> {
    let mut bytes = [0_u8; ADMIN_PASSWORD_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn generate_enrollment_token() -> Result<String> {
    let mut bytes = [0_u8; NODE_TOKEN_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(format!("{}{}", ENROLLMENT_TOKEN_PREFIX, hex_lower(&bytes)))
}

pub fn generate_wireguard_keypair() -> Result<WireGuardKeypair> {
    let mut private = [0_u8; WIREGUARD_KEY_BYTES];
    getrandom::fill(&mut private)?;
    clamp_wireguard_private_key(&mut private);
    let secret = StaticSecret::from(private);
    let public = PublicKey::from(&secret);

    Ok(WireGuardKeypair {
        private_key: BASE64_STANDARD.encode(private),
        public_key: BASE64_STANDARD.encode(public.as_bytes()),
    })
}

pub fn generate_reality_keypair() -> Result<RealityKeypair> {
    let mut private = [0_u8; WIREGUARD_KEY_BYTES];
    getrandom::fill(&mut private)?;
    let secret = StaticSecret::from(private);
    let public = PublicKey::from(&secret);

    Ok(RealityKeypair {
        private_key: URL_SAFE_NO_PAD.encode(private),
        public_key: URL_SAFE_NO_PAD.encode(public.as_bytes()),
    })
}

pub fn reality_public_key(private_key: &str) -> Result<String> {
    let bytes = URL_SAFE_NO_PAD.decode(private_key).map_err(|error| {
        StoreError::InvalidData(format!("invalid REALITY private key: {error}"))
    })?;
    let private: [u8; WIREGUARD_KEY_BYTES] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        StoreError::InvalidData(format!(
            "invalid REALITY private key length: expected {WIREGUARD_KEY_BYTES}, got {}",
            bytes.len()
        ))
    })?;
    let public = PublicKey::from(&StaticSecret::from(private));
    Ok(URL_SAFE_NO_PAD.encode(public.as_bytes()))
}

pub fn node_token_hash(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

pub fn admin_token_hash(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

pub fn admin_session_token_hash(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

pub fn enrollment_token_hash(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

pub fn node_token_display_prefix(token: &str) -> String {
    token
        .chars()
        .take(NODE_TOKEN_DISPLAY_PREFIX_LEN.min(token.len()))
        .collect()
}

pub fn admin_token_display_prefix(token: &str) -> String {
    token
        .chars()
        .take(ADMIN_TOKEN_DISPLAY_PREFIX_LEN.min(token.len()))
        .collect()
}

pub fn enrollment_token_display_prefix(token: &str) -> String {
    token
        .chars()
        .take(ENROLLMENT_TOKEN_DISPLAY_PREFIX_LEN.min(token.len()))
        .collect()
}

pub fn is_reality_short_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= REALITY_SHORT_ID_HEX_LEN
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn clamp_wireguard_private_key(private: &mut [u8; WIREGUARD_KEY_BYTES]) {
    private[0] &= 248;
    private[31] &= 127;
    private[31] |= 64;
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn generated_reality_short_id_is_lowercase_hex_at_full_length() {
        let short_id = generate_reality_short_id().unwrap();

        assert_eq!(short_id.len(), REALITY_SHORT_ID_HEX_LEN);
        assert!(short_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(short_id, short_id.to_ascii_lowercase());
        assert!(is_reality_short_id(&short_id));
    }

    #[test]
    fn reality_public_key_is_recovered_from_its_private_half() {
        let pair = generate_reality_keypair().unwrap();

        assert_eq!(
            reality_public_key(&pair.private_key).unwrap(),
            pair.public_key
        );
    }

    #[test]
    fn generated_uuid_v4_has_version_and_variant_bits() {
        let uuid = generate_uuid_v4().unwrap();

        assert_eq!(uuid.len(), 36);
        assert_eq!(&uuid[14..15], "4");
        assert!(matches!(&uuid[19..20], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn generated_reality_short_ids_are_not_deterministic() {
        let values = (0..16)
            .map(|_| generate_reality_short_id().unwrap())
            .collect::<HashSet<_>>();

        assert!(values.len() > 1);
    }

    #[test]
    fn generated_node_token_has_stable_prefix_and_entropy() {
        let token = generate_node_token().unwrap();

        assert_eq!(token.len(), NODE_TOKEN_LEN);
        assert!(token.starts_with(NODE_TOKEN_PREFIX));
        assert!(token[NODE_TOKEN_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        assert_eq!(
            node_token_display_prefix(&token).len(),
            NODE_TOKEN_DISPLAY_PREFIX_LEN
        );
    }

    #[test]
    fn generated_admin_token_has_stable_prefix_and_entropy() {
        let token = generate_admin_token().unwrap();

        assert_eq!(token.len(), ADMIN_TOKEN_LEN);
        assert!(token.starts_with(ADMIN_TOKEN_PREFIX));
        assert!(token[ADMIN_TOKEN_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        assert_eq!(
            admin_token_display_prefix(&token).len(),
            ADMIN_TOKEN_DISPLAY_PREFIX_LEN
        );
    }

    #[test]
    fn generated_admin_session_token_has_stable_prefix_and_entropy() {
        let token = generate_admin_session_token().unwrap();

        assert_eq!(token.len(), ADMIN_SESSION_TOKEN_LEN);
        assert!(token.starts_with(ADMIN_SESSION_TOKEN_PREFIX));
        assert!(token[ADMIN_SESSION_TOKEN_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
    }

    #[test]
    fn generated_enrollment_token_has_stable_prefix_and_entropy() {
        let token = generate_enrollment_token().unwrap();

        assert_eq!(token.len(), ENROLLMENT_TOKEN_LEN);
        assert!(token.starts_with(ENROLLMENT_TOKEN_PREFIX));
        assert!(token[ENROLLMENT_TOKEN_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        assert_eq!(
            enrollment_token_display_prefix(&token).len(),
            ENROLLMENT_TOKEN_DISPLAY_PREFIX_LEN
        );
    }

    #[test]
    fn generated_admin_password_is_url_safe_and_not_deterministic() {
        let values = (0..16)
            .map(|_| generate_admin_password().unwrap())
            .collect::<HashSet<_>>();

        assert!(values.len() > 1);
        for password in &values {
            assert_eq!(
                URL_SAFE_NO_PAD.decode(password).unwrap().len(),
                ADMIN_PASSWORD_BYTES
            );
            assert!(password
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
        }
    }

    #[test]
    fn generated_wireguard_keypair_has_clamped_private_key_and_public_key() {
        let keypair = generate_wireguard_keypair().unwrap();

        assert_eq!(keypair.private_key.len(), WIREGUARD_KEY_BASE64_LEN);
        assert_eq!(keypair.public_key.len(), WIREGUARD_KEY_BASE64_LEN);

        let private = BASE64_STANDARD.decode(&keypair.private_key).unwrap();
        let public = BASE64_STANDARD.decode(&keypair.public_key).unwrap();
        assert_eq!(private.len(), WIREGUARD_KEY_BYTES);
        assert_eq!(public.len(), WIREGUARD_KEY_BYTES);
        assert_eq!(private[0] & 0b0000_0111, 0);
        assert_eq!(private[31] & 0b1000_0000, 0);
        assert_eq!(private[31] & 0b0100_0000, 0b0100_0000);

        let private: [u8; WIREGUARD_KEY_BYTES] = private.try_into().unwrap();
        let expected_public = PublicKey::from(&StaticSecret::from(private));
        assert_eq!(public.as_slice(), expected_public.as_bytes());
    }

    /// The length is not cosmetic: `2022-blake3-aes-128-gcm` takes a 128-bit key and xray
    /// refuses anything else, so a generator that drifted would fail on the machine rather than
    /// here. The encoding differs deliberately from the X25519 keys next door — standard base64
    /// with padding, which is what shadowsocks tooling everywhere reads and writes.
    #[test]
    fn generated_shadowsocks_psk_is_sixteen_bytes_of_standard_base64() {
        let psk = generate_shadowsocks_psk().unwrap();

        assert_eq!(psk.len(), 24, "16 bytes padded to 24 characters");
        assert!(
            psk.ends_with('='),
            "standard base64 keeps its padding: {psk}"
        );
        assert_eq!(BASE64_STANDARD.decode(&psk).unwrap().len(), 16);
    }

    /// Two ports must not share a key. The bug this catches is a generator that seeds itself
    /// once — every relay hop in the fleet would then hold one key, and taking any machine
    /// would open all of them.
    #[test]
    fn generated_shadowsocks_psks_are_not_deterministic() {
        assert_ne!(
            generate_shadowsocks_psk().unwrap(),
            generate_shadowsocks_psk().unwrap()
        );
    }

    #[test]
    fn generated_reality_keypair_uses_url_safe_x25519_shape() {
        let keypair = generate_reality_keypair().unwrap();

        assert_eq!(keypair.private_key.len(), 43);
        assert_eq!(keypair.public_key.len(), 43);
        assert!(keypair
            .private_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
        assert!(keypair
            .public_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
    }

    #[test]
    fn generated_node_tokens_are_not_deterministic() {
        let values = (0..16)
            .map(|_| generate_node_token().unwrap())
            .collect::<HashSet<_>>();

        assert!(values.len() > 1);
    }

    #[test]
    fn node_token_hash_is_plain_sha256_hex() {
        assert_eq!(
            node_token_hash("broc_node_test"),
            "02da4918359b00fca0021f0e5a31ff05b4f326eb2e3a5c1982a4264119cd1fa6"
        );
    }

    #[test]
    fn admin_token_hash_is_plain_sha256_hex() {
        assert_eq!(
            admin_token_hash("broc_admin_test"),
            "bcb2e3df16391cd5ab716cb7d926b56e538484b39f494266f5c8948e9ecd80dd"
        );
    }

    #[test]
    fn admin_session_token_hash_is_plain_sha256_hex() {
        assert_eq!(
            admin_session_token_hash("broc_session_test"),
            "3f193852e25f0c51da5b2b49315d74b83f2017f5436f9c40d96789814252a762"
        );
    }

    #[test]
    fn enrollment_token_hash_is_plain_sha256_hex() {
        assert_eq!(
            enrollment_token_hash("broc_enroll_test"),
            "d941a15deeefb19f341d90546ba1210826b4732a3d2031da09c0e4dbf5824d3e"
        );
    }

    #[test]
    fn reality_short_id_validation_matches_core_shape() {
        assert!(is_reality_short_id("0"));
        assert!(is_reality_short_id("8337a0bf"));
        assert!(is_reality_short_id("0123456789abcdef"));
        assert!(is_reality_short_id("ABCDEF"));

        assert!(!is_reality_short_id(""));
        assert!(!is_reality_short_id("0123456789abcdef00"));
        assert!(!is_reality_short_id("not-hex"));
    }
}
