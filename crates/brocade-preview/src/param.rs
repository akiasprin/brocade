use crate::error::PreviewError;

/// Validate an identifier from the browser, on the same character set as brocade's slugs.
pub(crate) fn required_slug<'a>(value: &'a str, field: &str) -> Result<&'a str, PreviewError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(PreviewError::bad_request(format!("{field} is required")));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(PreviewError::bad_request(format!(
            "{field} must use the brocade slug charset"
        )));
    }
    Ok(value)
}

/// Encode a value as a URL path segment. `:` is left unencoded, since it joins a tenant to a
/// user.
pub(crate) fn percent_encode_path_segment(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'~' | b':') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_slug_trims_and_accepts_the_brocade_charset() {
        assert_eq!(required_slug("  hk-01  ", "id").unwrap(), "hk-01");
        assert_eq!(
            required_slug("platform.acme", "id").unwrap(),
            "platform.acme"
        );
        assert_eq!(required_slug("a_b.c-1", "id").unwrap(), "a_b.c-1");
    }

    #[test]
    fn required_slug_rejects_empty_and_out_of_charset_values() {
        assert!(required_slug("   ", "id").is_err());
        assert!(required_slug("hk 01", "id").is_err());
        assert!(required_slug("hk/01", "id").is_err());
        assert!(required_slug("hk;rm -rf /", "id").is_err());
    }

    #[test]
    fn percent_encode_path_segment_keeps_the_tenant_user_separator() {
        assert_eq!(
            percent_encode_path_segment("platform.acme:alice"),
            "platform.acme:alice"
        );
        assert_eq!(percent_encode_path_segment("a/b c"), "a%2Fb%20c");
    }
}
