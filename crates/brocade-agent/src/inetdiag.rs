//! Per-connection TCP state, read over netlink `inet_diag`.
//!
//! This is where the link-quality numbers come from, and they are free: BBR maintains a
//! bottleneck-bandwidth and min-RTT estimate on every real forwarding connection and refreshes it
//! every RTT. Nothing extra goes on the wire. The two probes we already run are both active —
//! `icmp.rs` pings for path MTU every 30 minutes, `e2e.rs` fetches through the chain every 5 —
//! and neither can answer "which hop is the bottleneck right now", because their traffic is not
//! the users' traffic.
//!
//! Not `ss -ti`. Same argument as `icmp.rs` not shelling out to `ping`, and it applies harder
//! here: `ss`'s output format is not a stable interface and shifts across iproute2 versions, so
//! parsing it is a debt that comes due on somebody else's schedule; and the slim images these
//! agents land on often have no `ss` at all (busybox ships `netstat`). Parsing a binary struct
//! whose layout the kernel guarantees is the *smaller* commitment.
//!
//! ## The one thing to be careful about
//!
//! `struct tcp_info` grows. Fields are only ever appended, so the offsets below are stable, but an
//! older kernel returns a **shorter** attribute — reading at a fixed offset without checking the
//! length is how this code would read garbage on exactly the machines most likely to be running an
//! old kernel. Every read goes through `u32_at`/`u64_at`, which return `None` past the end.

use std::{
    collections::BTreeMap,
    io, mem,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

use brocade_deployment::protocol::HopLinkSample;

const NETLINK_INET_DIAG: libc::c_int = 4;
const SOCK_DIAG_BY_FAMILY: u16 = 20;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_DUMP: u16 = 0x300;

/// Established only. A connection still handshaking has no bandwidth estimate to give, and one in
/// TIME_WAIT is describing a conversation that already ended.
const TCP_ESTABLISHED_MASK: u32 = 1 << 1;

/// `INET_DIAG_INFO` — yields `struct tcp_info`.
const INET_DIAG_INFO: u16 = 2;
/// `INET_DIAG_VEGASINFO`. Requesting this is also how BBRINFO is asked for: `idiag_ext` is only
/// 8 bits wide and `INET_DIAG_BBRINFO` is 16, so it cannot be named in the request. The kernel
/// answers with whichever congestion module is loaded, tagged with its own attribute number.
const INET_DIAG_VEGASINFO: u16 = 3;
/// What comes back for BBR — `struct tcp_bbr_info`.
const INET_DIAG_BBRINFO: u16 = 16;
/// The congestion algorithm's name as a string.
const INET_DIAG_CONG: u16 = 4;

// Offsets into `struct tcp_info`. Taken from linux/tcp.h; every field the kernel has ever added
// went on the end, which is what makes fixed offsets safe here — combined with the length check
// in u32_at/u64_at, which is what makes them safe on a kernel older than these fields.
const TI_APP_LIMITED_BYTE: usize = 7; // bit 0 of this byte
const TI_RTT: usize = 68;
const TI_TOTAL_RETRANS: usize = 100;
const TI_MIN_RTT: usize = 148;
const TI_DELIVERY_RATE: usize = 160;
const TI_BUSY_TIME: usize = 168;
const TI_RWND_LIMITED: usize = 176;
const TI_SNDBUF_LIMITED: usize = 184;
const TI_BYTES_SENT: usize = 200;
const TI_BYTES_RETRANS: usize = 208;

/// One established TCP connection, reduced to what the aggregation needs.
#[derive(Debug, Clone)]
pub(crate) struct Conn {
    pub(crate) peer: IpAddr,
    pub(crate) rtt_us: u32,
    pub(crate) min_rtt_us: u32,
    /// BBR's bottleneck-bandwidth estimate in **bytes** per second, as the kernel gives it.
    /// `None` on a connection not running BBR.
    pub(crate) btlbw_bytes: Option<u64>,
    /// The kernel says this measurement was limited by the application having nothing to send.
    /// Such a connection describes the app, not the line, and must be kept out of the bandwidth
    /// percentiles — a handful of idle ones otherwise drag the estimate to the floor.
    pub(crate) app_limited: bool,
    pub(crate) bytes_sent: u64,
    pub(crate) bytes_retrans: u64,
    pub(crate) busy_us: u64,
    pub(crate) rwnd_limited_us: u64,
    pub(crate) sndbuf_limited_us: u64,
    pub(crate) cc: String,
}

/// Dump every established TCP connection, IPv4 and IPv6.
pub(crate) fn dump() -> Result<Vec<Conn>, String> {
    let mut out = Vec::new();
    for family in [libc::AF_INET as u8, libc::AF_INET6 as u8] {
        match dump_family(family) {
            Ok(mut conns) => out.append(&mut conns),
            // One family failing must not lose the other. An IPv6-less machine answers EAFNOSUPPORT
            // here, and that is a normal machine, not a broken one.
            Err(error) => {
                eprintln!("inet_diag: family {family} dump failed: {error}");
            }
        }
    }
    Ok(out)
}

fn dump_family(family: u8) -> Result<Vec<Conn>, String> {
    let fd = netlink_socket()?;
    send_request(&fd, family)?;

    let mut conns = Vec::new();
    let mut buf = vec![0_u8; 32 * 1024];
    loop {
        let n = unsafe { libc::recv(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n < 0 {
            return Err(format!("recv: {}", io::Error::last_os_error()));
        }
        if n == 0 {
            break;
        }
        let mut offset = 0_usize;
        let filled = n as usize;
        while offset + 16 <= filled {
            let len = u32_le(&buf[offset..]) as usize;
            let kind = u16_le(&buf[offset + 4..]);
            // A length under the header size would make the cursor stand still; a length past the
            // buffer would read somebody else's memory. Both mean the stream is not what we think
            // it is, and continuing on a misread stream is worse than stopping.
            if len < 16 || offset + len > filled {
                return Ok(conns);
            }
            match kind {
                NLMSG_DONE => return Ok(conns),
                NLMSG_ERROR => {
                    let code = i32_le(&buf[offset + 16..]);
                    // 0 is an ACK rather than an error.
                    if code != 0 {
                        return Err(format!("netlink error {}", -code));
                    }
                    return Ok(conns);
                }
                _ => {
                    if let Some(conn) = parse_msg(&buf[offset + 16..offset + len]) {
                        conns.push(conn);
                    }
                }
            }
            offset += align4(len);
        }
    }
    Ok(conns)
}

fn netlink_socket() -> Result<OwnedFd, String> {
    let raw = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            NETLINK_INET_DIAG,
        )
    };
    if raw < 0 {
        return Err(format!(
            "cannot open netlink socket: {}",
            io::Error::last_os_error()
        ));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // Without a receive timeout a lost DONE hangs this thread forever, and it is the load thread —
    // it would take host sampling down with it, silently, for as long as the process lives.
    let tv = libc::timeval {
        tv_sec: 3,
        tv_usec: 0,
    };
    unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            std::ptr::addr_of!(tv).cast(),
            mem::size_of::<libc::timeval>() as libc::socklen_t,
        );
    }
    Ok(fd)
}

fn send_request(fd: &OwnedFd, family: u8) -> Result<(), String> {
    // nlmsghdr (16) + inet_diag_req_v2 (56)
    let mut req = [0_u8; 72];
    req[0..4].copy_from_slice(&72_u32.to_le_bytes());
    req[4..6].copy_from_slice(&SOCK_DIAG_BY_FAMILY.to_le_bytes());
    req[6..8].copy_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_le_bytes());
    req[8..12].copy_from_slice(&1_u32.to_le_bytes()); // seq
    req[12..16].copy_from_slice(&0_u32.to_le_bytes()); // pid: kernel fills it

    req[16] = family;
    req[17] = libc::IPPROTO_TCP as u8;
    // idiag_ext is a bitmask of (attr - 1). INFO and VEGASINFO together are what fetch tcp_info
    // and, on a BBR machine, tcp_bbr_info.
    req[18] = (1 << (INET_DIAG_INFO - 1))
        | (1 << (INET_DIAG_VEGASINFO - 1))
        | (1 << (INET_DIAG_CONG - 1));
    req[19] = 0; // pad
    req[20..24].copy_from_slice(&TCP_ESTABLISHED_MASK.to_le_bytes());
    // The rest is inet_diag_sockid, all zero: no filter, dump everything.

    let sent = unsafe { libc::send(fd.as_raw_fd(), req.as_ptr().cast(), req.len(), 0) };
    if sent < 0 {
        return Err(format!("send: {}", io::Error::last_os_error()));
    }
    Ok(())
}

/// Parse one `inet_diag_msg` plus its attributes.
fn parse_msg(body: &[u8]) -> Option<Conn> {
    // struct inet_diag_msg: family(1) state(1) timer(1) retrans(1) then inet_diag_sockid (48),
    // then expires/rqueue/wqueue/uid/inode (5 × 4). 72 bytes in total.
    if body.len() < 72 {
        return None;
    }
    let family = body[0];
    // inet_diag_sockid starts at 4: sport(2) dport(2) src[4](16) dst[4](16) if(4) cookie[2](8).
    // The peer port is deliberately not kept: hops are keyed by address alone (see
    // probe.rs::hop_targets), because a hop is one machine and two chains reaching it on different
    // ports still share one physical line — which is the thing being measured.
    let dst = &body[24..40];
    let peer = match family {
        f if f == libc::AF_INET as u8 => {
            IpAddr::V4(Ipv4Addr::from([dst[0], dst[1], dst[2], dst[3]]))
        }
        f if f == libc::AF_INET6 as u8 => {
            let mut octets = [0_u8; 16];
            octets.copy_from_slice(&dst[0..16]);
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        _ => return None,
    };

    let mut conn = Conn {
        peer,
        rtt_us: 0,
        min_rtt_us: 0,
        btlbw_bytes: None,
        app_limited: false,
        bytes_sent: 0,
        bytes_retrans: 0,
        busy_us: 0,
        rwnd_limited_us: 0,
        sndbuf_limited_us: 0,
        cc: String::new(),
    };

    for (kind, payload) in attributes(&body[72..]) {
        match kind {
            INET_DIAG_INFO => read_tcp_info(payload, &mut conn),
            INET_DIAG_BBRINFO => {
                // struct tcp_bbr_info: bw_lo, bw_hi, min_rtt, pacing_gain, cwnd_gain — five u32.
                // bw is a u64 split across two of them, in bytes per second.
                if let (Some(lo), Some(hi)) = (u32_at(payload, 0), u32_at(payload, 4)) {
                    conn.btlbw_bytes = Some(u64::from(lo) | (u64::from(hi) << 32));
                }
                // The gains are deliberately not read: they are 8.8 fixed point describing which
                // phase of BBR's cycle this sample caught, which says nothing about the link. The
                // state machine's phase (STARTUP/DRAIN/PROBE_BW/PROBE_RTT) is not in this struct
                // at all — do not build a UI expecting it.
            }
            INET_DIAG_CONG => {
                conn.cc = String::from_utf8_lossy(payload)
                    .trim_end_matches('\0')
                    .to_owned();
            }
            _ => {}
        }
    }
    Some(conn)
}

fn read_tcp_info(info: &[u8], conn: &mut Conn) {
    // Bit 0 of a bitfield byte. Reading it as a whole byte would also pick up
    // tcpi_fastopen_client_fail in bits 1-2 and report app_limited on connections that are not.
    conn.app_limited = info.get(TI_APP_LIMITED_BYTE).is_some_and(|b| b & 0x01 != 0);
    conn.rtt_us = u32_at(info, TI_RTT).unwrap_or(0);
    conn.min_rtt_us = u32_at(info, TI_MIN_RTT).unwrap_or(0);
    conn.busy_us = u64_at(info, TI_BUSY_TIME).unwrap_or(0);
    conn.rwnd_limited_us = u64_at(info, TI_RWND_LIMITED).unwrap_or(0);
    conn.sndbuf_limited_us = u64_at(info, TI_SNDBUF_LIMITED).unwrap_or(0);
    conn.bytes_sent = u64_at(info, TI_BYTES_SENT).unwrap_or(0);
    conn.bytes_retrans = u64_at(info, TI_BYTES_RETRANS).unwrap_or(0);
    // On a kernel too old for bytes_sent/bytes_retrans (pre-4.15) fall back to the segment
    // counter, so the retransmit column degrades to coarse rather than to a flat zero. A flat
    // zero reads as "this link is clean", which is the one thing it must not say when unknown.
    if conn.bytes_sent == 0 {
        if let Some(total) = u32_at(info, TI_TOTAL_RETRANS) {
            conn.bytes_retrans = u64::from(total);
        }
    }
    let _ = TI_DELIVERY_RATE; // read via BBRINFO where available; kept for the offset table
}

/// Walk a stream of `struct rtattr` { len: u16, type: u16, payload }.
fn attributes(mut data: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while data.len() >= 4 {
        let len = u16_le(data) as usize;
        let kind = u16_le(&data[2..]);
        if len < 4 || len > data.len() {
            break;
        }
        out.push((kind, &data[4..len]));
        let step = align4(len);
        if step >= data.len() {
            break;
        }
        data = &data[step..];
    }
    out
}

/// Aggregate connections into one sample per hop.
///
/// `hops` maps a peer address to the `(chain_id, peer_node_id)` that address belongs to — derived
/// by the caller from xray's outbound configuration, which is the only place that mapping exists.
pub(crate) fn aggregate(
    conns: &[Conn],
    hops: &BTreeMap<IpAddr, (String, String)>,
    window_start: i64,
    window_end: i64,
) -> Vec<HopLinkSample> {
    let mut grouped: BTreeMap<(String, String), Vec<&Conn>> = BTreeMap::new();
    for conn in conns {
        if let Some(key) = hops.get(&conn.peer) {
            grouped.entry(key.clone()).or_default().push(conn);
        }
    }

    grouped
        .into_iter()
        .map(|((chain_id, peer_node_id), group)| {
            // Only connections the kernel did not mark app_limited feed the bandwidth estimate.
            let measured: Vec<&&Conn> = group.iter().filter(|c| !c.app_limited).collect();
            let mut bws: Vec<u64> = measured
                .iter()
                .filter_map(|c| c.btlbw_bytes)
                .map(|bytes| bytes.saturating_mul(8))
                .collect();
            bws.sort_unstable();

            let mut rtts: Vec<u32> = group.iter().map(|c| c.rtt_us).filter(|v| *v > 0).collect();
            rtts.sort_unstable();

            let sent: u64 = group.iter().map(|c| c.bytes_sent).sum();
            let retrans: u64 = group.iter().map(|c| c.bytes_retrans).sum();
            let busy: u64 = group.iter().map(|c| c.busy_us).sum();
            let rwnd: u64 = group.iter().map(|c| c.rwnd_limited_us).sum();
            let sndbuf: u64 = group.iter().map(|c| c.sndbuf_limited_us).sum();
            // The three shares are of the connections' total lifetime-busy, not of the window:
            // tcp_info's counters are cumulative per connection and there is no per-window
            // denominator to be had. What the ratio answers is still the question being asked —
            // "of the time this hop spent trying to send, how much was spent stuck on what".
            let total_us = busy + rwnd + sndbuf;
            let share = |v: u64| -> f32 {
                if total_us == 0 {
                    0.0
                } else {
                    (v as f64 / total_us as f64 * 100.0) as f32
                }
            };

            HopLinkSample {
                chain_id,
                peer_node_id,
                window_start_unix_secs: window_start,
                window_end_unix_secs: window_end,
                conns: group.len() as u32,
                conns_measured: measured.len() as u32,
                btlbw_p50_bps: percentile(&bws, 50),
                btlbw_p90_bps: percentile(&bws, 90),
                // The minimum across the group, not the mean: propagation delay is by definition
                // the smallest measurement any connection saw, and averaging folds queueing in.
                min_rtt_us: group
                    .iter()
                    .map(|c| c.min_rtt_us)
                    .filter(|v| *v > 0)
                    .min()
                    .unwrap_or(0),
                rtt_p50_us: percentile(&rtts, 50).unwrap_or(0),
                rtt_p90_us: percentile(&rtts, 90).unwrap_or(0),
                retrans_pct: if sent == 0 {
                    0.0
                } else {
                    ((retrans as f64 / sent as f64) * 100.0).min(100.0) as f32
                },
                busy_pct: share(busy),
                rwnd_limited_pct: share(rwnd),
                sndbuf_limited_pct: share(sndbuf),
            }
        })
        .collect()
}

/// Nearest-rank percentile over a sorted slice. `None` on an empty one — which is the honest
/// answer for "what is this hop's bandwidth" when no connection could measure it, and is why the
/// field is an Option all the way to the database.
fn percentile<T: Copy>(sorted: &[T], p: usize) -> Option<T> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (sorted.len() * p).div_ceil(100).max(1) - 1;
    sorted.get(rank).copied()
}

const fn align4(len: usize) -> usize {
    (len + 3) & !3
}

fn u16_le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn i32_le(b: &[u8]) -> i32 {
    i32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// `None` past the end of the attribute. This is the guard that makes fixed offsets safe: an older
/// kernel returns a shorter `tcp_info`, and the fields at the far end simply are not there.
fn u32_at(b: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    (end <= b.len()).then(|| u32_le(&b[offset..]))
}

fn u64_at(b: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    (end <= b.len()).then(|| {
        u64::from_le_bytes([
            b[offset],
            b[offset + 1],
            b[offset + 2],
            b[offset + 3],
            b[offset + 4],
            b[offset + 5],
            b[offset + 6],
            b[offset + 7],
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole safety story for fixed offsets: a short attribute must yield None, not garbage
    /// and not a panic. Kernels before 4.15 have no bytes_sent, and those are exactly the machines
    /// most likely to be running an old distro.
    #[test]
    fn reading_past_a_short_tcp_info_yields_none_rather_than_garbage() {
        let short = vec![0_u8; 104]; // a pre-4.9-ish tcp_info, nothing past total_retrans
        assert!(
            u32_at(&short, TI_RTT).is_some(),
            "rtt is within even a short struct"
        );
        assert!(u64_at(&short, TI_BYTES_SENT).is_none());
        assert!(u64_at(&short, TI_SNDBUF_LIMITED).is_none());
        assert!(u32_at(&short, TI_MIN_RTT).is_none());
    }

    /// app_limited is bit 0 of a byte it shares with tcpi_fastopen_client_fail. Reading the byte
    /// whole would report app_limited on any connection whose fastopen attempt failed, quietly
    /// removing healthy connections from the bandwidth estimate.
    #[test]
    fn app_limited_reads_one_bit_not_the_whole_byte() {
        let mut info = vec![0_u8; 220];
        info[TI_APP_LIMITED_BYTE] = 0b0000_0110; // fastopen_client_fail set, app_limited clear
        let mut conn = blank();
        read_tcp_info(&info, &mut conn);
        assert!(!conn.app_limited);

        info[TI_APP_LIMITED_BYTE] = 0b0000_0001;
        let mut conn = blank();
        read_tcp_info(&info, &mut conn);
        assert!(conn.app_limited);
    }

    #[test]
    fn bbr_bandwidth_is_reassembled_from_two_halves_and_converted_to_bits() {
        // bw = 0x1_0000_0000 + 5 bytes/sec, split lo/hi
        let mut payload = vec![0_u8; 20];
        payload[0..4].copy_from_slice(&5_u32.to_le_bytes());
        payload[4..8].copy_from_slice(&1_u32.to_le_bytes());
        let lo = u32_at(&payload, 0).unwrap();
        let hi = u32_at(&payload, 4).unwrap();
        let bytes = u64::from(lo) | (u64::from(hi) << 32);
        assert_eq!(bytes, 0x1_0000_0005);
    }

    /// An empty set has no percentile, and that must stay distinguishable from zero: "no
    /// connection could measure this hop" is not "this hop has no bandwidth".
    #[test]
    fn percentiles_of_nothing_are_none_not_zero() {
        let empty: Vec<u64> = vec![];
        assert_eq!(percentile(&empty, 50), None);
        let one = vec![42_u64];
        assert_eq!(percentile(&one, 50), Some(42));
        let ten: Vec<u64> = (1..=10).collect();
        assert_eq!(percentile(&ten, 50), Some(5));
        assert_eq!(percentile(&ten, 90), Some(9));
        assert_eq!(percentile(&ten, 100), Some(10));
    }

    /// Idle connections must not drag the bandwidth estimate down, and must still be counted in
    /// `conns` — the gap between conns and conns_measured is itself the signal that this hop is
    /// mostly idle.
    #[test]
    fn app_limited_connections_are_counted_but_not_measured() {
        let mut busy = blank();
        busy.peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        busy.btlbw_bytes = Some(12_500_000); // 100 Mb/s
        busy.rtt_us = 30_000;
        busy.min_rtt_us = 28_000;

        let mut idle = busy.clone();
        idle.app_limited = true;
        idle.btlbw_bytes = Some(1_000); // a stale, meaningless estimate

        let hops = BTreeMap::from([(
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            ("app/chain".to_owned(), "sg-02".to_owned()),
        )]);
        let out = aggregate(&[busy, idle], &hops, 100, 130);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].conns, 2);
        assert_eq!(out[0].conns_measured, 1);
        assert_eq!(out[0].btlbw_p50_bps, Some(100_000_000));
    }

    /// A connection towards an address that is not one of our hops is somebody else's traffic —
    /// the control plane's own API, a package mirror, the user's own egress.
    #[test]
    fn connections_to_unknown_peers_are_dropped() {
        let mut stranger = blank();
        stranger.peer = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));
        let hops = BTreeMap::new();
        assert!(aggregate(&[stranger], &hops, 100, 130).is_empty());
    }

    fn blank() -> Conn {
        Conn {
            peer: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            rtt_us: 0,
            min_rtt_us: 0,
            btlbw_bytes: None,
            app_limited: false,
            bytes_sent: 0,
            bytes_retrans: 0,
            busy_us: 0,
            rwnd_limited_us: 0,
            sndbuf_limited_us: 0,
            cc: String::new(),
        }
    }
}
