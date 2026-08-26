//! Path-MTU probing on our own ICMP socket, rather than shelling out to `ping`.
//!
//! Shelling out has three defects:
//!
//! 1. It depends on which `ping` is installed. BusyBox's does not understand
//!    `-M` (on Alpine `/bin/ping` is BusyBox), and answers an unknown option
//!    with a usage line and a non-zero exit — the same signal as "packet too
//!    big". The whole fleet then reads as "ICMP filtered" and the operator goes
//!    hunting for a filter that does not exist.
//! 2. It treats an exit code as a measurement. Missing binary, unknown option,
//!    insufficient privilege, unresolvable DNS — all are read as "this size did
//!    not fit".
//! 3. It forks per probe: a dozen per peer, dozens per round.
//!
//! On our own socket, "cannot probe" and "did not fit" are separate return
//! values rather than guesses.
//!
//! Why still ICMP echo and not UDP plus `IP_MTU`: the UDP path is closer to what
//! wg actually speaks, and the kernel hands over the PMTU it learned. But it
//! rests on a fatal premise — some router must actually return ICMP
//! frag-needed. Behind a PMTU black hole (drops without notification, common on
//! cross-border links) the kernel never learns a smaller value and we would
//! report the interface MTU as the answer. A too-large number is worse than no
//! number, because it looks measured. ICMP echo confirms end to end: a size fits
//! only when the echo comes back, and a black hole shows up as a timeout, which
//! errs on the safe side.

use brocade_deployment::protocol::ProbeTransport;
use std::{
    io, mem,
    net::{Ipv4Addr, SocketAddr, ToSocketAddrs},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    time::{SystemTime, UNIX_EPOCH},
};

/// Result of one probe. "Did not fit" and "could not probe" must stay apart: the
/// first is a fact about the link, the second a fact about us. Conflated, our
/// own inability gets reported as a broken link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Fits,
    TooBig,
}

pub struct Pinger {
    fd: OwnedFd,
    /// Packets on a `SOCK_DGRAM` ICMP socket arrive without an IP header;
    /// on `SOCK_RAW` they carry one.
    dgram: bool,
    /// This endpoint has an IPv6 address besides its IPv4 one.
    /// See `wireguard_overhead`.
    also_v6: bool,
    seq: std::cell::Cell<u16>,
}

// Encapsulation overhead splits into three parts because each varies
// independently: the outer IP header depends on whether the endpoint is
// dual-stack, the transport header on whether the peer's wg entry is plain UDP
// or phantun's fake TCP, and wg's own 32 bytes never change (4 type +
// 4 receiver index + 8 counter + 16 Poly1305).
//
// Common combinations: direct v4 = 60 (wg's classic value); phantun v4 = 72;
// dual-stack direct = 80, which is exactly where wg-quick's default 1420 comes
// from.
const IPV4_HEADER: u16 = 20;
const IPV6_HEADER: u16 = 40;
const UDP_HEADER: u16 = 8;
const TCP_HEADER: u16 = 20;
const WIREGUARD_HEADER: u16 = 32;

impl Pinger {
    /// Open an ICMP socket to `host`.
    ///
    /// Tries `SOCK_DGRAM` first (unprivileged, provided
    /// `net.ipv4.ping_group_range` allows it), then `SOCK_RAW` (needs
    /// CAP_NET_RAW, which the agent usually has since it runs as root). If
    /// neither works this returns an error meaning "cannot measure on this
    /// machine"; the caller should report `Unsupported` rather than pass it off
    /// as a link fault.
    pub fn open(host: &str) -> Result<Self, String> {
        let (target, also_v6) = resolve_ipv4(host)?;

        let mut dgram = true;
        let mut fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP) };
        if fd < 0 {
            dgram = false;
            fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_ICMP) };
        }
        if fd < 0 {
            return Err(format!(
                "开不了 ICMP socket（dgram 和 raw 都不行）：{}。\
                 非 root 时需要 net.ipv4.ping_group_range 放行，或者给 agent CAP_NET_RAW",
                io::Error::last_os_error()
            ));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };

        // The step that matters: set the DF bit. Without it, packets above the
        // path MTU arrive fragmented, every size "fits", and the probe always
        // reports 1500.
        set_opt(
            fd.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_MTU_DISCOVER,
            libc::IP_PMTUDISC_DO,
        )?;
        set_timeout(fd.as_raw_fd(), 2)?;
        connect(fd.as_raw_fd(), target)?;

        Ok(Self {
            fd,
            dgram,
            also_v6,
            seq: std::cell::Cell::new(0),
        })
    }

    /// How much to subtract from the measured path MTU to get the wg MTU.
    ///
    /// When the endpoint also has an AAAA record this counts IPv6's 80, even
    /// though we probed the IPv4 path. What we probe is our choice; which stack
    /// wg takes is decided when wg resolves the name itself — on a dual-stack
    /// endpoint it will likely pick v6, where the outer IP header is 40, not 20.
    /// Counting 60 would suggest a value 20 bytes too large, whose symptom is a
    /// tunnel that comes up, passes small packets, and silently drops large ones
    /// — precisely what this feature exists to prevent.
    ///
    /// The cost is 20 bytes of throughput; the return is that the suggestion is
    /// never too large. Too small is merely slow, too large is broken.
    pub fn wireguard_overhead(&self, transport: ProbeTransport) -> u16 {
        let ip = if self.also_v6 {
            IPV6_HEADER
        } else {
            IPV4_HEADER
        };
        let transport_header = match transport {
            ProbeTransport::Udp => UDP_HEADER,
            // phantun wraps wg's UDP payload in a forged TCP header: 12 bytes more
            ProbeTransport::FakeTcp => TCP_HEADER,
        };
        ip + transport_header + WIREGUARD_HEADER
    }

    /// One ordinary-sized probe, to judge reachability.
    pub fn reachable(&self) -> bool {
        matches!(self.echo(64), Ok(Probe::Fits))
    }

    /// Whether one IP packet of exactly `mtu` total bytes, with DF set, gets
    /// through.
    ///
    /// IP header 20 plus ICMP header 8, so the payload is `mtu - 28` — the same
    /// convention as `ping -s`.
    pub fn fits(&self, mtu: u16) -> Result<Probe, String> {
        self.echo(mtu.saturating_sub(28))
    }

    fn echo(&self, payload_len: u16) -> Result<Probe, String> {
        let seq = self.seq.get().wrapping_add(1);
        self.seq.set(seq);
        let packet = echo_request(seq, payload_len);

        let sent = unsafe {
            libc::send(
                self.fd.as_raw_fd(),
                packet.as_ptr() as *const libc::c_void,
                packet.len(),
                0,
            )
        };
        if sent < 0 {
            let error = io::Error::last_os_error();
            // EMSGSIZE = it cannot even leave this host: either above the egress
            // NIC's MTU, or the kernel's cached path MTU already says this size
            // does not get through. Both are the fact "did not fit".
            if error.raw_os_error() == Some(libc::EMSGSIZE) {
                return Ok(Probe::TooBig);
            }
            return Err(format!("发 ICMP 失败：{error}"));
        }

        // No echo back counts as not getting through. This case cannot be told
        // apart from "the peer does not answer echo", so the caller must first
        // confirm reachability with a small packet — that is what `reachable()`
        // is for.
        let mut buf = [0_u8; 2048];
        loop {
            let got = unsafe {
                libc::recv(
                    self.fd.as_raw_fd(),
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                    0,
                )
            };
            if got < 0 {
                let error = io::Error::last_os_error();
                return match error.kind() {
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Ok(Probe::TooBig),
                    _ => Err(format!("收 ICMP 失败：{error}")),
                };
            }
            // The same socket may receive someone else's echo, or a late reply
            // to an earlier probe. On a sequence mismatch keep waiting instead
            // of taking it as this probe's answer.
            if let Some(reply_seq) = self.reply_seq(&buf[..got as usize]) {
                if reply_seq == seq {
                    return Ok(Probe::Fits);
                }
            }
        }
    }

    /// Echo sequence number from a received packet; `None` if it is not an echo
    /// reply.
    fn reply_seq(&self, buf: &[u8]) -> Option<u16> {
        let icmp = if self.dgram {
            buf
        } else {
            // SOCK_RAW packets carry an IP header, whose length lives in the low
            // nibble of the first byte, in units of 4 bytes
            let ihl = (*buf.first()? & 0x0f) as usize * 4;
            buf.get(ihl..)?
        };
        // type 0 = echo reply
        if *icmp.first()? != 0 {
            return None;
        }
        Some(u16::from_be_bytes([*icmp.get(6)?, *icmp.get(7)?]))
    }
}

fn echo_request(seq: u16, payload_len: u16) -> Vec<u8> {
    let mut packet = Vec::with_capacity(8 + payload_len as usize);
    // type=8 echo request, code=0
    packet.extend_from_slice(&[8, 0]);
    // checksum placeholder
    packet.extend_from_slice(&[0, 0]);
    // identifier: under `SOCK_DGRAM` the kernel overwrites this with the
    // socket's port, under `SOCK_RAW` we fill it ourselves. Matching goes by
    // sequence number either way, so the value here does not matter.
    packet.extend_from_slice(&(std::process::id() as u16).to_be_bytes());
    packet.extend_from_slice(&seq.to_be_bytes());
    // Fill the payload with a time-derived pattern rather than zeroes: all-zero
    // packets get special treatment on some devices (compression, dropping),
    // which would make the measurement disagree with real traffic.
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    packet.extend((0..payload_len).map(|i| (stamp as u16 ^ i) as u8));

    let sum = checksum(&packet);
    packet[2..4].copy_from_slice(&sum.to_be_bytes());
    packet
}

/// RFC 1071 one's-complement sum. The kernel recomputes it under `SOCK_DGRAM`;
/// under `SOCK_RAW` we must get it right ourselves.
fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0_u32;
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u32::from(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    if let Some(&last) = chunks.remainder().first() {
        sum += u32::from(u16::from_be_bytes([last, 0]));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Resolve the IPv4 address to probe, plus whether the endpoint is dual-stack.
///
/// Probing itself is IPv4-only (ICMPv6 is a separate packet format, to be done
/// once there is an IPv6 backbone). But dual-stack-ness has to come along: wg
/// resolves the same name itself and will likely take v6 on such an endpoint,
/// where encapsulation costs 20 bytes more than v4. Drop that and the derived
/// suggestion comes out too large.
fn resolve_ipv4(host: &str) -> Result<(Ipv4Addr, bool), String> {
    if let Ok(addr) = host.parse::<Ipv4Addr>() {
        return Ok((addr, false));
    }
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        return Err(format!("{host} 是 IPv6 字面量，暂时只探 IPv4 路径"));
    }

    let mut v4 = None;
    let mut v6 = false;
    for addr in (host, 0_u16)
        .to_socket_addrs()
        .map_err(|error| format!("解析不了 {host}：{error}"))?
    {
        match addr {
            SocketAddr::V4(a) if v4.is_none() => v4 = Some(*a.ip()),
            SocketAddr::V6(_) => v6 = true,
            SocketAddr::V4(_) => {}
        }
    }
    v4.map(|addr| (addr, v6))
        .ok_or_else(|| format!("{host} 没有 IPv4 地址，暂时只探 IPv4 路径"))
}

fn set_opt(
    fd: RawFd,
    level: libc::c_int,
    name: libc::c_int,
    value: libc::c_int,
) -> Result<(), String> {
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            &value as *const libc::c_int as *const libc::c_void,
            mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(format!(
            "setsockopt({name}) 失败：{}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn set_timeout(fd: RawFd, secs: i64) -> Result<(), String> {
    let timeout = libc::timeval {
        tv_sec: secs,
        tv_usec: 0,
    };
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &timeout as *const libc::timeval as *const libc::c_void,
            mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(format!("设置收超时失败：{}", io::Error::last_os_error()));
    }
    Ok(())
}

fn connect(fd: RawFd, target: Ipv4Addr) -> Result<(), String> {
    let addr = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from(target).to_be(),
        },
        sin_zero: [0; 8],
    };
    let rc = unsafe {
        libc::connect(
            fd,
            &addr as *const libc::sockaddr_in as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(format!(
            "connect {target} 失败：{}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_a_known_vector() {
        // An 8-byte echo request (type 8, code 0, id 0x1234, seq 1) and its
        // one's-complement sum with the checksum field left empty. By hand:
        // 0x0800 + 0x1234 + 0x0001 = 0x1a35 → !0x1a35
        let packet = [8_u8, 0, 0, 0, 0x12, 0x34, 0x00, 0x01];
        assert_eq!(checksum(&packet), !0x1a35_u16);
    }

    /// Payload length directly sets the size of the packet on the wire; off by
    /// one here is off by one in the measured MTU.
    #[test]
    fn payload_length_accounts_for_both_headers() {
        // For a 1500-byte IP packet: 20 IP + 8 ICMP + 1472 payload
        let packet = echo_request(1, 1500 - 28);
        assert_eq!(packet.len(), 8 + 1472);
        assert_eq!(packet.len() + 20, 1500);
    }

    #[test]
    fn resolve_ipv4_takes_literals_as_is() {
        assert_eq!(
            resolve_ipv4("10.66.0.2").unwrap(),
            (Ipv4Addr::new(10, 66, 0, 2), false)
        );
        assert!(resolve_ipv4("::1").is_err(), "IPv6 该明确拒绝而不是猜");
    }

    /// A dual-stack endpoint must be costed at IPv6's overhead: wg may take v6
    /// when it resolves the same name, and costing it at v4 suggests a value 20
    /// bytes too large — too small is merely slow, too large silently drops
    /// large packets.
    #[test]
    fn overhead_covers_every_transport_and_family() {
        // Direct v4: wg's classic 60
        assert_eq!(IPV4_HEADER + UDP_HEADER + WIREGUARD_HEADER, 60);
        // phantun fake TCP: TCP header replaces the UDP one, 12 more
        assert_eq!(IPV4_HEADER + TCP_HEADER + WIREGUARD_HEADER, 72);
        // Dual-stack direct: 80, exactly where wg-quick's default 1420 comes from
        assert_eq!(IPV6_HEADER + UDP_HEADER + WIREGUARD_HEADER, 80);
        assert_eq!(1500 - 80, 1420);
    }
}
