use super::*;

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

pub fn valid_public_key(value: &str) -> bool {
    if value.len() > 8192 || value.contains(['\n', '\r', '\0']) {
        return false;
    }
    let mut fields = value.split_whitespace();
    let (Some(kind), Some(encoded)) = (fields.next(), fields.next()) else {
        return false;
    };
    let Ok(bytes) = general_purpose::STANDARD
        .decode(encoded)
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(encoded))
    else {
        return false;
    };
    let mut remaining = bytes.as_slice();
    fn field<'a>(remaining: &mut &'a [u8]) -> Option<&'a [u8]> {
        let size = u32::from_be_bytes(remaining.get(..4)?.try_into().ok()?) as usize;
        let value = remaining.get(4..4usize.checked_add(size)?)?;
        *remaining = &remaining[4 + size..];
        Some(value)
    }
    if field(&mut remaining) != Some(kind.as_bytes()) {
        return false;
    }
    let valid = match kind {
        "ssh-ed25519" => {
            field(&mut remaining).is_some_and(|key| key.len() == 32 && key.iter().any(|b| *b != 0))
        }
        "ssh-rsa" => {
            let exponent = field(&mut remaining);
            let modulus = field(&mut remaining);
            exponent.is_some_and(|e| {
                !e.is_empty() && e.len() <= 5 && e.last().is_some_and(|b| b & 1 == 1)
            }) && modulus.is_some_and(|n| {
                let n = n.strip_prefix(&[0]).unwrap_or(n);
                (256..=1024).contains(&n.len()) && n[0] & 0x80 != 0
            })
        }
        "ecdsa-sha2-nistp256" => {
            field(&mut remaining) == Some(b"nistp256".as_slice())
                && field(&mut remaining).is_some_and(|point| point.len() == 65 && point[0] == 4)
        }
        _ => false,
    };
    valid && remaining.is_empty()
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
