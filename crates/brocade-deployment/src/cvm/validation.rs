pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(super) fn valid_mac(value: &str) -> bool {
    let parts: Vec<_> = value.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub(super) fn valid_interface_address(value: &str) -> bool {
    if value.len() > 80 {
        return false;
    }
    let (ip, prefix) = value
        .split_once('/')
        .map_or((value, None), |(ip, prefix)| (ip, Some(prefix)));
    let Ok(ip) = ip.parse::<std::net::IpAddr>() else {
        return false;
    };
    prefix.is_none_or(|p| {
        p.parse::<u8>()
            .is_ok_and(|n| n <= if ip.is_ipv4() { 32 } else { 128 })
    })
}
