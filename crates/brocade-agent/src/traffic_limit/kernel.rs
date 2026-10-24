//! Read exact HTB rates through Linux's stable rtnetlink ABI. Neither tc's JSON
//! layout nor its rounded, human-readable units are a verification boundary.
use std::{
    ffi::CString,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::{Duration, Instant},
};

const HEADER: usize = 16;
const TCMSG: usize = 20;
const SEQUENCE: u32 = 1;
const MAX_BYTES: usize = 1024 * 1024;
const MAX_CLASSES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Class {
    pub handle: u32,
    pub kind: String,
    pub rate: Option<u64>,
    pub ceil: Option<u64>,
}

fn error(context: &str) -> String {
    format!("HTB netlink {context}: {}", io::Error::last_os_error())
}

pub(super) fn classes(interface: &str) -> Result<Vec<Class>, String> {
    let interface = CString::new(interface).map_err(|_| "invalid HTB interface")?;
    // SAFETY: interface is a live NUL-terminated interface name.
    let index = unsafe { libc::if_nametoindex(interface.as_ptr()) };
    if index == 0 {
        return Err(error("interface"));
    }
    // SAFETY: socket arguments request a nonblocking route-netlink descriptor.
    let raw = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_ROUTE,
        )
    };
    if raw < 0 {
        return Err(error("socket"));
    }
    // SAFETY: raw is a new descriptor owned exclusively by this function.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: all-zero sockaddr_nl is valid once its address family is assigned.
    let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    // SAFETY: address and its length describe a live sockaddr_nl; pid 0 is the kernel.
    if unsafe {
        libc::connect(
            fd.as_raw_fd(),
            std::ptr::addr_of!(address).cast(),
            std::mem::size_of_val(&address) as libc::socklen_t,
        )
    } < 0
    {
        return Err(error("connect"));
    }
    let mut request = [0_u8; HEADER + TCMSG];
    request[0..4].copy_from_slice(&((HEADER + TCMSG) as u32).to_ne_bytes());
    request[4..6].copy_from_slice(&42_u16.to_ne_bytes()); // RTM_GETTCLASS
    request[6..8].copy_from_slice(&0x301_u16.to_ne_bytes()); // REQUEST | DUMP
    request[8..12].copy_from_slice(&SEQUENCE.to_ne_bytes());
    request[20..24].copy_from_slice(&index.to_ne_bytes());
    // SAFETY: request is a live buffer; send cannot outlive this synchronous call.
    if unsafe { libc::send(fd.as_raw_fd(), request.as_ptr().cast(), request.len(), 0) }
        != request.len() as isize
    {
        return Err(error("send"));
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut buffer = [0_u8; 65536];
    let mut total = 0;
    let mut classes = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("HTB netlink read timed out after 3s".into());
        }
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll references exactly one live descriptor entry.
        let ready = unsafe { libc::poll(&mut poll, 1, remaining.as_millis().max(1) as i32) };
        if ready < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if ready < 0 || poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(error("poll"));
        }
        if ready == 0 {
            continue;
        }
        // MSG_TRUNC returns the original datagram length, so truncation cannot look complete.
        // SAFETY: buffer is writable for its advertised length and fd remains live.
        let mut sender = address;
        let mut sender_length = std::mem::size_of_val(&sender) as libc::socklen_t;
        let received = unsafe {
            libc::recvfrom(
                fd.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_TRUNC,
                std::ptr::addr_of_mut!(sender).cast(),
                &mut sender_length,
            )
        };
        if received < 0 {
            if matches!(
                io::Error::last_os_error().kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(error("recv"));
        }
        if sender_length as usize != std::mem::size_of_val(&sender)
            || sender.nl_family != libc::AF_NETLINK as libc::sa_family_t
            || sender.nl_pid != 0
        {
            return Err("HTB netlink response is not from the kernel".into());
        }
        let length = received as usize;
        total += length;
        if length == 0 || length > buffer.len() || total > MAX_BYTES {
            return Err("HTB netlink dump truncated or exceeds size limit".into());
        }
        if datagram(&buffer[..length], index, &mut classes)? {
            return Ok(classes);
        }
    }
}

fn u16_at(data: &[u8], offset: usize) -> Result<u16, String> {
    data.get(offset..offset + 2)
        .and_then(|v| v.try_into().ok())
        .map(u16::from_ne_bytes)
        .ok_or_else(|| "truncated HTB netlink u16".into())
}
fn u32_at(data: &[u8], offset: usize) -> Result<u32, String> {
    data.get(offset..offset + 4)
        .and_then(|v| v.try_into().ok())
        .map(u32::from_ne_bytes)
        .ok_or_else(|| "truncated HTB netlink u32".into())
}
fn align(length: usize) -> usize {
    (length + 3) & !3
}

fn attributes(mut data: &[u8]) -> Result<Vec<(u16, &[u8])>, String> {
    let mut result = Vec::new();
    while !data.is_empty() {
        let length = usize::from(u16_at(data, 0)?);
        let kind = u16_at(data, 2)?;
        if length < 4 || length > data.len() {
            return Err("invalid HTB netlink attribute length".into());
        }
        result.push((kind, &data[4..length]));
        if length == data.len() {
            break;
        }
        data = data
            .get(align(length)..)
            .ok_or("truncated HTB netlink attribute padding")?;
    }
    Ok(result)
}

fn unique<'a>(attributes: &[(u16, &'a [u8])], kind: u16) -> Result<Option<&'a [u8]>, String> {
    let mut matches = attributes.iter().filter(|(k, _)| k & 0x3fff == kind);
    let value = matches.next();
    if matches.next().is_some() {
        return Err("duplicate HTB netlink attribute".into());
    }
    match value {
        Some((kind, _)) if kind & 0x4000 != 0 => {
            Err("unexpected HTB netlink network-byte-order attribute".into())
        }
        Some((_, value)) => Ok(Some(value)),
        None => Ok(None),
    }
}

fn class(body: &[u8], index: u32) -> Result<Option<Class>, String> {
    if body.len() < TCMSG {
        return Err("truncated HTB netlink tcmsg".into());
    }
    if u32_at(body, 4)? != index {
        return Ok(None);
    }
    let attributes = attributes(&body[TCMSG..])?;
    let kind = unique(&attributes, 1)?.ok_or("missing HTB netlink class kind")?;
    let kind = kind
        .strip_suffix(&[0])
        .ok_or("unterminated HTB netlink class kind")?;
    let kind = std::str::from_utf8(kind).map_err(|_| "invalid HTB netlink class kind")?;
    let mut result = Class {
        handle: u32_at(body, 8)?,
        kind: kind.into(),
        rate: None,
        ceil: None,
    };
    if kind == "htb" {
        let options =
            self::attributes(unique(&attributes, 2)?.ok_or("missing HTB netlink options")?)?;
        let parameters = unique(&options, 1)?.ok_or("missing HTB netlink parameters")?;
        // tc_htb_opt contains two 12-byte tc_ratespec values, followed by five u32s.
        if parameters.len() < 44 {
            return Err("truncated HTB netlink parameters".into());
        }
        let rate = |kind, offset| -> Result<u64, String> {
            if let Some(value) = unique(&options, kind)? {
                let bytes: [u8; 8] = value.try_into().map_err(|_| "invalid HTB 64-bit rate")?;
                Ok(u64::from_ne_bytes(bytes))
            } else {
                Ok(u64::from(u32_at(parameters, offset)?))
            }
        };
        result.rate = Some(rate(6, 8)?); // TCA_HTB_RATE64, otherwise ratespec.rate
        result.ceil = Some(rate(7, 20)?); // TCA_HTB_CEIL64
    }
    Ok(Some(result))
}

fn datagram(mut data: &[u8], index: u32, classes: &mut Vec<Class>) -> Result<bool, String> {
    while !data.is_empty() {
        let length = u32_at(data, 0)? as usize;
        if length < HEADER || length > data.len() {
            return Err("invalid HTB netlink message length".into());
        }
        if u32_at(data, 8)? != SEQUENCE {
            return Err("unexpected HTB netlink sequence".into());
        }
        if u16_at(data, 6)? & 0x10 != 0 {
            return Err("HTB netlink dump interrupted; retry next round".into());
        }
        let body = &data[HEADER..length];
        match u16_at(data, 4)? {
            1 => {} // NLMSG_NOOP
            2 | 3 => {
                // NLMSG_ERROR / NLMSG_DONE
                if !body.is_empty() {
                    let code = u32_at(body, 0)? as i32;
                    if code != 0 {
                        return Err(format!("HTB netlink kernel error {code}"));
                    }
                } else if u16_at(data, 4)? == 2 {
                    return Err("truncated HTB netlink error".into());
                }
                if u16_at(data, 4)? == 3 {
                    return Ok(true);
                }
            }
            40 => {
                // RTM_NEWTCLASS
                if let Some(class) = class(body, index)? {
                    classes.push(class);
                }
                if classes.len() > MAX_CLASSES {
                    return Err("HTB netlink class count limit exceeded".into());
                }
            }
            _ => return Err("unexpected HTB netlink response type".into()),
        }
        if length == data.len() {
            break;
        }
        data = data
            .get(align(length)..)
            .ok_or("truncated HTB netlink message padding")?;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attribute(kind: u16, payload: &[u8]) -> Vec<u8> {
        let mut result = Vec::new();
        result.extend_from_slice(&((4 + payload.len()) as u16).to_ne_bytes());
        result.extend_from_slice(&kind.to_ne_bytes());
        result.extend_from_slice(payload);
        result.resize(align(result.len()), 0);
        result
    }
    fn message(kind: u16, flags: u16, body: &[u8]) -> Vec<u8> {
        let mut result = vec![0; HEADER];
        result[..4].copy_from_slice(&((HEADER + body.len()) as u32).to_ne_bytes());
        result[4..6].copy_from_slice(&kind.to_ne_bytes());
        result[6..8].copy_from_slice(&flags.to_ne_bytes());
        result[8..12].copy_from_slice(&SEQUENCE.to_ne_bytes());
        result.extend_from_slice(body);
        result.resize(align(result.len()), 0);
        result
    }
    fn class_body(rate: u64, ceil: u64, extended: bool) -> Vec<u8> {
        let mut body = vec![0; TCMSG];
        body[4..8].copy_from_slice(&7_u32.to_ne_bytes());
        body[8..12].copy_from_slice(&0xbcd0_0010_u32.to_ne_bytes());
        let mut parameters = vec![0; 44];
        parameters[8..12].copy_from_slice(&(rate.min(u64::from(u32::MAX)) as u32).to_ne_bytes());
        parameters[20..24].copy_from_slice(&(ceil.min(u64::from(u32::MAX)) as u32).to_ne_bytes());
        let mut options = attribute(1, &parameters);
        if extended {
            options.extend(attribute(7, &ceil.to_ne_bytes()));
            options.extend(attribute(6, &rate.to_ne_bytes()));
        }
        // The kernel ABI permits arbitrary ordering and new attributes. Match ids, not position.
        options.extend(attribute(111, &[1, 2, 3]));
        body.extend(attribute(2 | 0x8000, &options));
        body.extend(attribute(222 | 0x4000, &[4, 5]));
        body.extend(attribute(1, b"htb\0"));
        body
    }

    #[test]
    fn traffic_limit_netlink_matches_exact_32_and_64_bit_rates() {
        for (rate, ceil, extended) in [
            (125, 125, false),
            (12_500_000, 12_500_000, false),
            (154_320_875, 154_320_875, false),
            (5_000_000_125, 125_000_000_000, true),
        ] {
            let expected = Class {
                handle: 0xbcd0_0010,
                kind: "htb".into(),
                rate: Some(rate),
                ceil: Some(ceil),
            };
            let body = class_body(rate, ceil, extended);
            assert_eq!(class(&body, 7).unwrap(), Some(expected.clone()));
            assert_eq!(class(&body, 8).unwrap(), None);
            let mut wire = message(40, 2, &body);
            wire.extend(message(3, 2, &0_i32.to_ne_bytes()));
            let mut classes = Vec::new();
            assert!(datagram(&wire, 7, &mut classes).unwrap());
            assert_eq!(classes, vec![expected]);
        }
    }

    #[test]
    fn traffic_limit_netlink_never_confirms_partial_or_interrupted_dump() {
        let body = class_body(12_500_000, 12_500_000, true);
        let wire = message(40, 2, &body);
        assert!(!datagram(&wire, 7, &mut Vec::new()).unwrap());
        for size in 1..wire.len() {
            assert!(
                datagram(&wire[..size], 7, &mut Vec::new()).is_err(),
                "truncation at {size}"
            );
        }
        assert!(datagram(&message(3, 0x10, &[]), 7, &mut Vec::new()).is_err());
        assert!(datagram(
            &message(2, 0, &(-libc::EPERM).to_ne_bytes()),
            7,
            &mut Vec::new()
        )
        .is_err());
        assert!(datagram(
            &message(3, 0, &(-libc::EINTR).to_ne_bytes()),
            7,
            &mut Vec::new()
        )
        .is_err());
        assert!(datagram(&message(2, 0, &[]), 7, &mut Vec::new()).is_err());
        assert!(datagram(&message(4, 0, &[]), 7, &mut Vec::new()).is_err());
        let mut wrong_sequence = wire;
        wrong_sequence[8..12].copy_from_slice(&2_u32.to_ne_bytes());
        assert!(datagram(&wrong_sequence, 7, &mut Vec::new()).is_err());
    }

    #[test]
    fn traffic_limit_netlink_rejects_duplicate_missing_and_truncated_rates() {
        let body = class_body(12_500_000, 12_500_000, true);
        let mut duplicate = body.clone();
        duplicate.extend(attribute(1, b"htb\0"));
        assert!(class(&duplicate, 7).is_err());
        for options in [
            Vec::new(),
            attribute(1, &[0; 43]),
            [attribute(1, &[0; 44]), attribute(6, &[0; 7])].concat(),
            [attribute(1, &[0; 44]), attribute(7, &[0; 9])].concat(),
            [
                attribute(1, &[0; 44]),
                attribute(6, &[0; 8]),
                attribute(6, &[0; 8]),
            ]
            .concat(),
        ] {
            let mut invalid = body[..TCMSG].to_vec();
            invalid.extend(attribute(1, b"htb\0"));
            invalid.extend(attribute(2, &options));
            assert!(class(&invalid, 7).is_err());
        }
        assert!(attributes(&[3, 0, 1, 0]).is_err());
        assert!(unique(&attributes(&attribute(0x4001, &[0; 4])).unwrap(), 1).is_err());
        let mut classes = vec![
            Class {
                handle: 0,
                kind: "foreign".into(),
                rate: None,
                ceil: None
            };
            MAX_CLASSES
        ];
        assert!(datagram(&message(40, 2, &body), 7, &mut classes).is_err());
    }
}
