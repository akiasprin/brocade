//! Normalization and parsing of the short strings that appear in both the model and the store.
//!
//! These are shared rather than copied because a difference between two copies is invisible: the
//! store writes what it normalized, the compiler reads it back and normalizes again, and the two
//! only ever disagree in production, on the one input where their implementations drifted apart.

use base64::{engine::general_purpose, Engine as _};

/// Fingerprints accepted by the pinned Xray v26.4.25 REALITY client, excluding the two values
/// (`unsafe` and `hellogolang`) which Xray itself rejects for REALITY.
pub const REALITY_FINGERPRINTS: &[&str] = &[
    "chrome",
    "firefox",
    "safari",
    "ios",
    "android",
    "edge",
    "360",
    "qq",
    "random",
    "randomized",
];

pub fn is_reality_public_key(value: &str) -> bool {
    !value.is_empty()
        && general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|decoded| decoded.len() == 32)
}

/// Brocade exposes one to eight complete bytes. Xray's empty-ID special mode is intentionally not
/// represented by the control plane.
pub fn is_reality_short_id(value: &str) -> bool {
    (2..=16).contains(&value.len())
        && value.len().is_multiple_of(2)
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn is_reality_fingerprint(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    REALITY_FINGERPRINTS.contains(&value.as_str())
}

pub fn is_reality_server_name(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('*')
        && !value.chars().any(char::is_whitespace)
        && !value.contains(':')
}

pub fn is_nonzero_host_port(value: &str) -> bool {
    let Some((host, port)) = value.rsplit_once(':') else {
        return false;
    };
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    !host.is_empty() && port.parse::<u16>().is_ok_and(|port| port != 0)
}

/// `host:port` with the whitespace taken out and any IPv6 brackets preserved.
///
/// Leaves anything that is not host:port shaped alone. Validation belongs to whoever knows what
/// the field is for; this only decides where the spaces go.
pub fn normalize_host_port(value: &str) -> String {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('[') {
        if let Some((host, port)) = rest.split_once("]:") {
            return format!("[{}]:{}", host.trim(), port.trim());
        }
    }
    if let Some((host, port)) = value.rsplit_once(':') {
        return format!("{}:{}", host.trim(), port.trim());
    }
    value.to_owned()
}

/// `x.y.z` into its three numbers, or `None` for anything else.
///
/// Deliberately narrow: three parts, digits only, no `v` prefix and no pre-release suffix. Both
/// callers compare the result against a minimum, and a version they cannot parse is one they
/// refuse rather than guess at.
pub fn parse_semver3(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.split('.');
    let major = parse_version_part(parts.next()?)?;
    let minor = parse_version_part(parts.next()?)?;
    let patch = parse_version_part(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn parse_version_part(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_host_port_keeps_ipv6_brackets() {
        assert_eq!(
            normalize_host_port("[2001:db8::1] : 443"),
            "[2001:db8::1]:443"
        );
    }

    #[test]
    fn normalize_host_port_strips_the_spaces_around_the_colon() {
        assert_eq!(
            normalize_host_port(" apps.apple.com : 443 "),
            "apps.apple.com:443"
        );
    }

    #[test]
    fn normalize_host_port_leaves_a_bare_host_alone() {
        assert_eq!(normalize_host_port(" apps.apple.com "), "apps.apple.com");
    }

    #[test]
    fn parse_semver3_wants_exactly_three_numeric_parts() {
        assert_eq!(parse_semver3("1.9.4"), Some((1, 9, 4)));
        assert_eq!(parse_semver3("1.9"), None);
        assert_eq!(parse_semver3("1.9.4.1"), None);
        assert_eq!(parse_semver3("v1.9.4"), None);
        assert_eq!(parse_semver3("1.9.4-beta"), None);
    }

    #[test]
    fn reality_values_use_the_product_boundary() {
        assert!(is_reality_public_key(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        ));
        assert!(!is_reality_public_key(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        ));

        for short_id in ["00", "8337a0bf", "0123456789abcdef", "ABCDEF"] {
            assert!(is_reality_short_id(short_id), "{short_id}");
        }
        for short_id in ["", "0", "abc", "0123456789abcdef00", "not-hex"] {
            assert!(!is_reality_short_id(short_id), "{short_id}");
        }

        assert!(is_reality_fingerprint("chrome"));
        assert!(is_reality_fingerprint("randomized"));
        assert!(!is_reality_fingerprint("unsafe"));
        assert!(!is_reality_fingerprint("hellochrome_120"));
        assert!(!is_reality_fingerprint("not-a-fingerprint"));

        assert!(is_reality_server_name("www.example.com"));
        assert!(!is_reality_server_name("*.example.com"));
        assert!(!is_reality_server_name("www.example.com:443"));

        assert!(is_nonzero_host_port("example.com:443"));
        assert!(is_nonzero_host_port("[2001:db8::1]:443"));
        assert!(!is_nonzero_host_port("example.com:0"));
    }
}
