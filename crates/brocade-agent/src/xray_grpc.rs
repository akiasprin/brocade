//! Minimal client for Xray's local gRPC API: HandlerService and StatsService.
//!
//! The agent deliberately keeps its network stack synchronous and dependency-light.  These calls
//! are unary, carry small protobuf messages, and run over the loopback-only h2c listener, so the
//! small HTTP/2 subset below is enough: one request per connection and no dynamic HPACK table.
//! Success still requires a complete gRPC response message; a trailers-only response is an error,
//! even if its HPACK block is opaque to this client.
//!
//! Everything here used to be `xray api …` subprocesses.  Shelling out cost a fork per call —
//! every 30 seconds for the counters, once per inbound per convergence round for the users — but
//! the reason it had to go is sharper than that: those children are named `xray` too, and the
//! agent had no way to tell them from the server whose counters it was reading.  See
//! `xray_started_at_unix_secs` in main.rs for what that cost.

use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv6Addr, SocketAddr, TcpStream},
    time::Duration,
};

use brocade_deployment::protocol::{
    ForwardingProtocol, OnlineSource, OnlineSourceProtocol, UserOnlineSources,
    MAX_ONLINE_SOURCE_ENTRIES, MAX_ONLINE_SOURCE_PROTOCOLS, MAX_ONLINE_SOURCE_USERS,
};

mod transport;

pub(crate) use transport::deadline_after;

const ADD_INBOUND_PATH: &str = "/xray.app.proxyman.command.HandlerService/AddInbound";
const FORWARDING_POLICY_PATH: &str =
    "/xray.app.proxyman.command.HandlerService/GetForwardingTargetPolicy";
const REMOVE_INBOUND_PATH: &str = "/xray.app.proxyman.command.HandlerService/RemoveInbound";
const LIST_INBOUNDS_PATH: &str = "/xray.app.proxyman.command.HandlerService/ListInbounds";
const ALTER_INBOUND_PATH: &str = "/xray.app.proxyman.command.HandlerService/AlterInbound";
const GET_INBOUND_USERS_PATH: &str = "/xray.app.proxyman.command.HandlerService/GetInboundUsers";
const QUERY_STATS_PATH: &str = "/xray.app.stats.command.StatsService/QueryStats";
const GET_USERS_STATS_PATH: &str = "/xray.app.stats.command.StatsService/GetUsersStats";
const MUX_SNAPSHOT_PATH: &str = "/xray.app.stats.command.StatsService/GetMuxSnapshot";
const REVERSE_HEALTH_SNAPSHOT_PATH: &str =
    "/xray.app.stats.command.StatsService/GetReverseHealthSnapshot";
const ADD_USER_OPERATION: &str = "xray.app.proxyman.command.AddUserOperation";
const REMOVE_USER_OPERATION: &str = "xray.app.proxyman.command.RemoveUserOperation";
const VLESS_ACCOUNT: &str = "xray.proxy.vless.Account";
const HYSTERIA2_ACCOUNT: &str = "xray.proxy.hysteria.account.Account";
const ANYTLS_ACCOUNT: &str = "xray.proxy.anytls.Account";
const MTPROTO_ACCOUNT: &str = "xray.proxy.mtproto.Account";
const RECEIVER_CONFIG: &str = "xray.app.proxyman.ReceiverConfig";
const FORWARDING_CONFIG: &str = "xray.proxy.dokodemo.ForwardingConfig";
const CLIENT_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const END_STREAM: u8 = 0x1;
const ACK: u8 = 0x1;
const END_HEADERS: u8 = 0x4;
const SETTINGS_INITIAL_WINDOW_SIZE: u16 = 0x4;
/// Both flow-control windows are opened this far before the request goes out. The authorization
/// calls never came close to the 64 KiB default, but a `QueryStats` answer is one line per counter
/// per direction, so a machine with a few hundred grants can outrun it — and outrunning it does
/// not fail, it stalls until the read timeout, which reads like an unreachable xray.
// Billing plus the independent site journal contain at most 32K bounded names.
// Cover the worst-case encoded snapshot, and enforce the same bound on reads.
const WINDOW_SIZE: u32 = 32 * 1024 * 1024;
const DEFAULT_WINDOW_SIZE: u32 = 65_535;
/// The connection-level WINDOW_UPDATE is sent as the difference of the two, so shrinking the
/// window below the protocol default has to fail here rather than underflow at runtime.
const _: () = assert!(WINDOW_SIZE > DEFAULT_WINDOW_SIZE);

/// One row of `QueryStats`, named exactly as xray names it (`user>>>…>>>traffic>>>uplink`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct XrayStat {
    pub(crate) name: String,
    pub(crate) value: i64,
}

/// A user xray is actually serving on an inbound, as read back for reconciliation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct XrayUser {
    pub(crate) email: String,
    /// The VLESS id or the Hysteria 2 auth string — whichever the account carries.
    pub(crate) credential: String,
    pub(crate) flow: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum XrayAccount<'a> {
    Vless { id: &'a str, flow: Option<&'a str> },
    Hysteria2 { auth: &'a str },
    AnyTls { password: &'a str },
    MtProto { secret: &'a str },
}

pub(crate) fn add_user(
    api_address: SocketAddr,
    tag: &str,
    email: &str,
    level: u32,
    account: XrayAccount<'_>,
) -> Result<(), String> {
    let account = match account {
        XrayAccount::Vless { id, flow } => {
            let mut value = Vec::new();
            bytes_field(&mut value, 1, id.as_bytes());
            if let Some(flow) = flow.filter(|flow| !flow.is_empty()) {
                bytes_field(&mut value, 2, flow.as_bytes());
            }
            typed_message(VLESS_ACCOUNT, &value)
        }
        XrayAccount::Hysteria2 { auth } => {
            let mut value = Vec::new();
            bytes_field(&mut value, 1, auth.as_bytes());
            typed_message(HYSTERIA2_ACCOUNT, &value)
        }
        XrayAccount::AnyTls { password } => {
            let mut value = Vec::new();
            bytes_field(&mut value, 1, password.as_bytes());
            typed_message(ANYTLS_ACCOUNT, &value)
        }
        XrayAccount::MtProto { secret } => {
            let mut value = Vec::new();
            bytes_field(&mut value, 1, secret.as_bytes());
            typed_message(MTPROTO_ACCOUNT, &value)
        }
    };

    let mut user = Vec::new();
    if level != 0 {
        varint_field(&mut user, 1, u64::from(level));
    }
    bytes_field(&mut user, 2, email.as_bytes());
    message_field(&mut user, 3, &account);

    let mut operation = Vec::new();
    message_field(&mut operation, 1, &user);
    alter_inbound(api_address, tag, ADD_USER_OPERATION, &operation)
}

pub(crate) fn remove_user(api_address: SocketAddr, tag: &str, email: &str) -> Result<(), String> {
    let mut operation = Vec::new();
    bytes_field(&mut operation, 1, email.as_bytes());
    alter_inbound(api_address, tag, REMOVE_USER_OPERATION, &operation)
}

pub(crate) fn add_forwarding_inbound(
    api_address: SocketAddr,
    tag: &str,
    protocol: ForwardingProtocol,
    listen_port: u16,
    target_host: &str,
    target_port: u16,
    allow_private_targets: bool,
) -> Result<(), String> {
    let request = forwarding_inbound_request(
        tag,
        protocol,
        listen_port,
        target_host,
        target_port,
        allow_private_targets,
    );
    grpc_unary(api_address, ADD_INBOUND_PATH, &request).map(|_| ())
}

pub(crate) fn forwarding_target_policy_supported(api_address: SocketAddr) -> bool {
    let Ok(response) = grpc_unary(api_address, FORWARDING_POLICY_PATH, &[]) else {
        return false;
    };
    let mut cursor = 0;
    while let Ok(Some((number, field))) = next_field(&response, &mut cursor) {
        if let (1, Field::Varint(version)) = (number, field) {
            return version >= 1;
        }
    }
    false
}

/// Missing checkpoints cannot prove the destination of a live listener. Inspect the actual
/// proxy settings before preserving a public-IP listener during recovery.
pub(crate) fn forwarding_target_is_public(api_address: SocketAddr, tag: &str) -> bool {
    let inspect = || -> Result<bool, String> {
        let response = grpc_unary(api_address, LIST_INBOUNDS_PATH, &[])?;
        let mut cursor = 0;
        while let Some((number, field)) = next_field(&response, &mut cursor)? {
            if let (1, Field::Bytes(inbound)) = (number, field) {
                let mut inbound_cursor = 0;
                let mut found = false;
                let mut proxy = None;
                while let Some((number, field)) = next_field(inbound, &mut inbound_cursor)? {
                    match (number, field) {
                        (1, Field::Bytes(value)) => found = value == tag.as_bytes(),
                        (3, Field::Bytes(value)) => proxy = Some(value),
                        _ => {}
                    }
                }
                if found {
                    return proxy
                        .map(decode_public_forwarding_target)
                        .transpose()
                        .map(|value| value.unwrap_or(false));
                }
            }
        }
        Ok(false)
    };
    // Failure to inspect never authorizes preserving an unproven destination.
    inspect().unwrap_or(false)
}

fn decode_public_forwarding_target(message: &[u8]) -> Result<bool, String> {
    fn bytes(message: &[u8], wanted: u32) -> Result<Option<&[u8]>, String> {
        let mut cursor = 0;
        while let Some((number, field)) = next_field(message, &mut cursor)? {
            if let Field::Bytes(value) = field {
                if number == wanted {
                    return Ok(Some(value));
                }
            }
        }
        Ok(None)
    }
    let name = bytes(message, 1)?.unwrap_or_default();
    let mut proxy = bytes(message, 2)?.unwrap_or_default();
    if name == FORWARDING_CONFIG.as_bytes() {
        proxy = bytes(proxy, 1)?.unwrap_or_default();
    } else if name != b"xray.proxy.dokodemo.Config" {
        return Ok(false);
    }
    let address = bytes(proxy, 1)?.unwrap_or_default();
    let ip = bytes(address, 1)?;
    let domain = bytes(address, 2)?;
    if ip.is_some() && domain.is_some() {
        return Ok(false);
    }
    if let Some(ip) = ip {
        let ip = match ip.len() {
            4 => IpAddr::V4(std::net::Ipv4Addr::from(
                <[u8; 4]>::try_from(ip).map_err(|_| "invalid IPv4 address")?,
            )),
            16 => IpAddr::V6(Ipv6Addr::from(
                <[u8; 16]>::try_from(ip).map_err(|_| "invalid IPv6 address")?,
            )),
            _ => return Ok(false),
        };
        return Ok(!brocade_core::forwarding::target_ip_is_blocked(ip));
    }
    Ok(domain
        .and_then(|value| std::str::from_utf8(value).ok())
        .is_some_and(brocade_core::forwarding::target_host_is_public_ip))
}

fn forwarding_inbound_request(
    tag: &str,
    protocol: ForwardingProtocol,
    listen_port: u16,
    target_host: &str,
    target_port: u16,
    allow_private_targets: bool,
) -> Vec<u8> {
    let mut port_range = Vec::new();
    varint_field(&mut port_range, 1, u64::from(listen_port));
    varint_field(&mut port_range, 2, u64::from(listen_port));
    let mut port_list = Vec::new();
    message_field(&mut port_list, 1, &port_range);
    let mut receiver = Vec::new();
    message_field(&mut receiver, 1, &port_list);

    let mut address = Vec::new();
    match target_host.trim_matches(['[', ']']).parse::<IpAddr>() {
        Ok(IpAddr::V4(value)) => bytes_field(&mut address, 1, &value.octets()),
        Ok(IpAddr::V6(value)) => bytes_field(&mut address, 1, &value.octets()),
        Err(_) => bytes_field(&mut address, 2, target_host.as_bytes()),
    }
    let mut proxy = Vec::new();
    message_field(&mut proxy, 1, &address);
    varint_field(&mut proxy, 2, u64::from(target_port));
    let mut policy = Vec::new();
    for cidr in brocade_core::forwarding::blocked_target_cidrs(allow_private_targets) {
        bytes_field(&mut policy, 1, cidr.as_bytes());
    }
    message_field(&mut proxy, 8, &policy);
    if matches!(
        protocol,
        ForwardingProtocol::Tcp | ForwardingProtocol::TcpUdp
    ) {
        varint_field(&mut proxy, 7, 2);
    }
    if matches!(
        protocol,
        ForwardingProtocol::Udp | ForwardingProtocol::TcpUdp
    ) {
        varint_field(&mut proxy, 7, 3);
    }

    let mut inbound = Vec::new();
    bytes_field(&mut inbound, 1, tag.as_bytes());
    message_field(&mut inbound, 2, &typed_message(RECEIVER_CONFIG, &receiver));
    let mut protected_proxy = Vec::new();
    message_field(&mut protected_proxy, 1, &proxy);
    message_field(
        &mut inbound,
        3,
        &typed_message(FORWARDING_CONFIG, &protected_proxy),
    );
    let mut request = Vec::new();
    message_field(&mut request, 1, &inbound);
    request
}

pub(crate) fn remove_inbound(api_address: SocketAddr, tag: &str) -> Result<(), String> {
    let mut request = Vec::new();
    bytes_field(&mut request, 1, tag.as_bytes());
    grpc_unary(api_address, REMOVE_INBOUND_PATH, &request).map(|_| ())
}

pub(crate) fn inbound_tags(api_address: SocketAddr) -> Result<Vec<String>, String> {
    inbound_tags_until(api_address, None)
}

pub(crate) fn inbound_tags_until(
    api_address: SocketAddr,
    deadline: Option<std::time::Instant>,
) -> Result<Vec<String>, String> {
    let mut request = Vec::new();
    varint_field(&mut request, 1, 1);
    let response = transport::unary(api_address, LIST_INBOUNDS_PATH, &request, deadline)?;
    decode_inbound_tags(&response)
}

fn alter_inbound(
    api_address: SocketAddr,
    tag: &str,
    operation_type: &str,
    operation: &[u8],
) -> Result<(), String> {
    let operation = typed_message(operation_type, operation);
    let mut request = Vec::new();
    bytes_field(&mut request, 1, tag.as_bytes());
    message_field(&mut request, 2, &operation);

    // The response carries nothing: AlterInboundResponse is empty. Its arrival is the answer.
    grpc_unary(api_address, ALTER_INBOUND_PATH, &request).map(|_| ())
}

/// Every counter whose name starts with `pattern`, cumulative since xray started.
///
/// `reset` stays unset (field 2 omitted). The control plane differences successive readings and
/// needs the counters to survive being read; a reset here would hand every other reader — a human
/// with `xray api statsquery`, most of all — a zeroed machine.
pub(crate) fn query_stats(api_address: SocketAddr, pattern: &str) -> Result<Vec<XrayStat>, String> {
    let mut request = Vec::new();
    bytes_field(&mut request, 1, pattern.as_bytes());
    let response = grpc_unary(api_address, QUERY_STATS_PATH, &request)?;
    decode_stats(&response)
}

/// Every source address currently held by Xray's ref-counted online maps.
///
/// Both request booleans remain false by sending an empty protobuf: aggregate user traffic already
/// has its own durable accounting path, while each online entry carries read-only current-session
/// counters. Resetting either structure would interfere with other readers.
pub(crate) fn online_sources(api_address: SocketAddr) -> Result<Vec<UserOnlineSources>, String> {
    let response = grpc_unary(api_address, GET_USERS_STATS_PATH, &[])?;
    decode_online_sources(&response)
}

/// The users xray is serving on `tag` right now, account payloads decoded.
///
/// An empty email in the request means "all of them" — the same thing `xray api inbounduser -tag`
/// asked for.
pub(crate) fn inbound_users(api_address: SocketAddr, tag: &str) -> Result<Vec<XrayUser>, String> {
    let mut request = Vec::new();
    bytes_field(&mut request, 1, tag.as_bytes());
    let response = grpc_unary(api_address, GET_INBOUND_USERS_PATH, &request)?;
    decode_inbound_users(&response)
}

fn typed_message(type_name: &str, value: &[u8]) -> Vec<u8> {
    let mut message = Vec::new();
    bytes_field(&mut message, 1, type_name.as_bytes());
    bytes_field(&mut message, 2, value);
    message
}

fn varint_field(output: &mut Vec<u8>, field: u32, value: u64) {
    varint(output, u64::from(field) << 3);
    varint(output, value);
}

fn message_field(output: &mut Vec<u8>, field: u32, value: &[u8]) {
    bytes_field(output, field, value);
}

fn bytes_field(output: &mut Vec<u8>, field: u32, value: &[u8]) {
    varint(output, (u64::from(field) << 3) | 2);
    varint(output, value.len() as u64);
    output.extend_from_slice(value);
}

fn varint(output: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        output.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

/// Returns the response protobuf, framing stripped. Each attempt owns one connection.
fn grpc_unary(api_address: SocketAddr, path: &str, request: &[u8]) -> Result<Vec<u8>, String> {
    transport::unary(api_address, path, request, None)
}

fn settings_payload() -> Vec<u8> {
    let mut payload = Vec::with_capacity(6);
    payload.extend_from_slice(&SETTINGS_INITIAL_WINDOW_SIZE.to_be_bytes());
    payload.extend_from_slice(&WINDOW_SIZE.to_be_bytes());
    payload
}

fn request_headers(api_address: SocketAddr, path: &str) -> Vec<u8> {
    let mut block = vec![0x83, 0x86]; // :method POST, :scheme http
    hpack_literal_indexed_name(&mut block, 4, path); // :path
    hpack_literal_indexed_name(&mut block, 1, &api_address.to_string()); // :authority
    hpack_literal_indexed_name(&mut block, 31, "application/grpc"); // content-type
    hpack_literal_new_name(&mut block, "te", "trailers");
    block
}

/// HPACK literal header field without indexing, using a static-table name.
fn hpack_literal_indexed_name(output: &mut Vec<u8>, name_index: usize, value: &str) {
    hpack_integer(output, name_index, 4, 0);
    hpack_string(output, value);
}

/// HPACK literal header field without indexing, with a literal name.
fn hpack_literal_new_name(output: &mut Vec<u8>, name: &str, value: &str) {
    output.push(0);
    hpack_string(output, name);
    hpack_string(output, value);
}

fn hpack_string(output: &mut Vec<u8>, value: &str) {
    // Huffman flag remains clear. Xray/grpc-go accepts ordinary UTF-8 strings.
    hpack_integer(output, value.len(), 7, 0);
    output.extend_from_slice(value.as_bytes());
}

fn hpack_integer(output: &mut Vec<u8>, mut value: usize, prefix_bits: u8, first_byte_flags: u8) {
    let maximum = (1usize << prefix_bits) - 1;
    if value < maximum {
        output.push(first_byte_flags | value as u8);
        return;
    }
    output.push(first_byte_flags | maximum as u8);
    value -= maximum;
    while value >= 128 {
        output.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

fn write_frame(
    stream: &mut impl Write,
    kind: u8,
    flags: u8,
    stream_id: u32,
    payload: &[u8],
) -> std::io::Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| std::io::Error::other("HTTP/2 frame is too large"))?;
    if length > 0x00ff_ffff {
        return Err(std::io::Error::other("HTTP/2 frame is too large"));
    }
    let mut header = [0u8; 9];
    header[0] = (length >> 16) as u8;
    header[1] = (length >> 8) as u8;
    header[2] = length as u8;
    header[3] = kind;
    header[4] = flags;
    header[5..9].copy_from_slice(&(stream_id & 0x7fff_ffff).to_be_bytes());
    stream.write_all(&header)?;
    stream.write_all(payload)
}

struct Frame {
    kind: u8,
    flags: u8,
    stream_id: u32,
    payload: Vec<u8>,
}

fn read_frame(stream: &mut impl Read) -> std::io::Result<Frame> {
    let mut header = [0u8; 9];
    stream.read_exact(&mut header)?;
    let length =
        (usize::from(header[0]) << 16) | (usize::from(header[1]) << 8) | usize::from(header[2]);
    if length > 1024 * 1024 {
        return Err(std::io::Error::other("xray gRPC frame exceeds 1 MiB"));
    }
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload)?;
    Ok(Frame {
        kind: header[3],
        flags: header[4],
        stream_id: u32::from_be_bytes(header[5..9].try_into().expect("four bytes")) & 0x7fff_ffff,
        payload,
    })
}

/// Strip gRPC's length-prefixed framing.
///
/// A response with no DATA at all is the trailers-only shape gRPC uses to report an error. This
/// client cannot read the `grpc-status` in it — the trailers are HPACK and there is no decoder
/// here — so the absence of a message is itself the failure. Better a blunt error than a silent
/// success on a call that never ran.
fn grpc_response_message(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 5 {
        return Err("xray gRPC call failed without a response message".to_owned());
    }
    if data[0] != 0 {
        return Err("xray gRPC returned a compressed response unexpectedly".to_owned());
    }
    let length = u32::from_be_bytes(data[1..5].try_into().expect("four bytes")) as usize;
    if data.len() != length + 5 {
        return Err("xray gRPC returned an incomplete response".to_owned());
    }
    Ok(data[5..].to_vec())
}

/// One protobuf field. Fixed-width wire types are read and dropped: nothing decoded here uses
/// them, but they have to be stepped over correctly or the rest of the message misparses.
enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

/// Advance one field. `Ok(None)` at the end of the message.
fn next_field<'a>(data: &'a [u8], cursor: &mut usize) -> Result<Option<(u32, Field<'a>)>, String> {
    if *cursor >= data.len() {
        return Ok(None);
    }
    let key = read_varint(data, cursor)?;
    let number =
        u32::try_from(key >> 3).map_err(|_| "protobuf field number too large".to_owned())?;
    let value = match key & 0x7 {
        0 => Field::Varint(read_varint(data, cursor)?),
        1 => {
            take(data, cursor, 8)?;
            Field::Fixed
        }
        2 => {
            let length = usize::try_from(read_varint(data, cursor)?)
                .map_err(|_| "protobuf length too large".to_owned())?;
            Field::Bytes(take(data, cursor, length)?)
        }
        5 => {
            take(data, cursor, 4)?;
            Field::Fixed
        }
        other => return Err(format!("unsupported protobuf wire type {other}")),
    };
    Ok(Some((number, value)))
}

fn take<'a>(data: &'a [u8], cursor: &mut usize, length: usize) -> Result<&'a [u8], String> {
    let end = cursor
        .checked_add(length)
        .filter(|end| *end <= data.len())
        .ok_or_else(|| "protobuf message ends mid-field".to_owned())?;
    let slice = &data[*cursor..end];
    *cursor = end;
    Ok(slice)
}

fn read_varint(data: &[u8], cursor: &mut usize) -> Result<u64, String> {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let byte = *data
            .get(*cursor)
            .ok_or_else(|| "protobuf varint ends mid-value".to_owned())?;
        *cursor += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("protobuf varint is longer than 10 bytes".to_owned())
}

fn utf8(bytes: &[u8], what: &str) -> Result<String, String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| format!("xray sent a non-UTF-8 {what}"))
}

/// `QueryStatsResponse { repeated Stat stat = 1 }`, `Stat { string name = 1; int64 value = 2 }`.
fn decode_stats(message: &[u8]) -> Result<Vec<XrayStat>, String> {
    let mut stats = Vec::new();
    let mut cursor = 0;
    while let Some((number, field)) = next_field(message, &mut cursor)? {
        if let (1, Field::Bytes(bytes)) = (number, field) {
            stats.push(decode_stat(bytes)?);
        }
    }
    Ok(stats)
}

fn decode_stat(message: &[u8]) -> Result<XrayStat, String> {
    let mut name = None;
    let mut value = 0_i64;
    let mut cursor = 0;
    while let Some((number, field)) = next_field(message, &mut cursor)? {
        match (number, field) {
            (1, Field::Bytes(bytes)) => name = Some(utf8(bytes, "counter name")?),
            // int64 on the wire is a plain varint; negative values arrive as their two's
            // complement, which this cast restores.
            (2, Field::Varint(raw)) => value = raw as i64,
            _ => {}
        }
    }
    Ok(XrayStat {
        name: name.ok_or_else(|| "xray sent a counter without a name".to_owned())?,
        value,
    })
}

/// `GetUsersStatsResponse { repeated UserStat users = 1 }`.
fn decode_online_sources(message: &[u8]) -> Result<Vec<UserOnlineSources>, String> {
    let mut users = Vec::new();
    let mut total_sources = 0_usize;
    let mut cursor = 0;
    while let Some((number, field)) = next_field(message, &mut cursor)? {
        if let (1, Field::Bytes(bytes)) = (number, field) {
            if users.len() >= MAX_ONLINE_SOURCE_USERS {
                return Err(format!(
                    "xray online snapshot exceeds {MAX_ONLINE_SOURCE_USERS} users"
                ));
            }
            let mut user = decode_online_user(bytes)?;
            total_sources = total_sources
                .checked_add(user.sources.len())
                .ok_or_else(|| "xray online source count overflow".to_owned())?;
            if total_sources > MAX_ONLINE_SOURCE_ENTRIES {
                return Err(format!(
                    "xray online snapshot exceeds {MAX_ONLINE_SOURCE_ENTRIES} sources"
                ));
            }
            user.sources.sort_by(|left, right| left.ip.cmp(&right.ip));
            users.push(user);
        }
    }
    users.sort_by(|left, right| left.label.cmp(&right.label));
    Ok(users)
}

/// `UserStat { string email = 1; repeated OnlineIPEntry ips = 2; ... }`.
fn decode_online_user(message: &[u8]) -> Result<UserOnlineSources, String> {
    let mut label = None;
    let mut sources = Vec::new();
    let mut cursor = 0;
    while let Some((number, field)) = next_field(message, &mut cursor)? {
        match (number, field) {
            (1, Field::Bytes(bytes)) => label = Some(utf8(bytes, "online user label")?),
            (2, Field::Bytes(bytes)) => sources.push(decode_online_source(bytes)?),
            _ => {}
        }
    }
    let label = label.ok_or_else(|| "xray sent online sources without a user label".to_owned())?;
    if label.is_empty() {
        return Err("xray sent online sources with an empty user label".to_owned());
    }
    Ok(UserOnlineSources { label, sources })
}

/// `OnlineIPEntry { string ip = 1; int64 last_seen = 2; repeated string protocols = 3;
///                  int64 online_since = 4; int64 uplink_bytes = 5; int64 downlink_bytes = 6 }`.
fn decode_online_source(message: &[u8]) -> Result<OnlineSource, String> {
    let mut ip = None;
    let mut last_seen_unix_secs = 0_i64;
    let mut protocols = Vec::new();
    let mut online_since_unix_secs = None;
    let mut session_uplink_bytes = None;
    let mut session_downlink_bytes = None;
    let mut cursor = 0;
    while let Some((number, field)) = next_field(message, &mut cursor)? {
        match (number, field) {
            (1, Field::Bytes(bytes)) => ip = Some(utf8(bytes, "online source IP")?),
            (2, Field::Varint(raw)) => last_seen_unix_secs = raw as i64,
            (3, Field::Bytes(bytes)) => {
                if protocols.len() >= MAX_ONLINE_SOURCE_PROTOCOLS {
                    return Err("xray online source protocol count exceeds limit".to_owned());
                }
                let protocol = match bytes {
                    b"vless" => OnlineSourceProtocol::Vless,
                    b"anytls" => OnlineSourceProtocol::AnyTls,
                    b"hysteria" => OnlineSourceProtocol::Hysteria2,
                    b"mtproto" => OnlineSourceProtocol::MtProto,
                    _ => OnlineSourceProtocol::Unknown,
                };
                protocols.push(protocol);
            }
            (4, Field::Varint(raw)) => online_since_unix_secs = Some(raw as i64),
            (5, Field::Varint(raw)) => session_uplink_bytes = Some(raw as i64),
            (6, Field::Varint(raw)) => session_downlink_bytes = Some(raw as i64),
            (4..=6, _) => {
                return Err(
                    "xray sent an invalid online source session metric wire type".to_owned(),
                )
            }
            _ => {}
        }
    }
    let raw_ip = ip.ok_or_else(|| "xray sent an online source without an IP".to_owned())?;
    // Xray's Address.String() wraps IPv6 in brackets even in its online-IP field. Accept that
    // representation as well as bare IPs, but not socket addresses, domains or broken wrappers.
    let ip = match raw_ip.strip_prefix('[').and_then(|ip| ip.strip_suffix(']')) {
        Some(ip) => ip.parse::<Ipv6Addr>().map(IpAddr::V6),
        None => raw_ip.parse::<IpAddr>(),
    }
    .map_err(|_| "xray sent an invalid online source IP".to_owned())?
    .to_string();
    if last_seen_unix_secs <= 0 {
        return Err("xray sent an online source with a non-positive last_seen".to_owned());
    }
    let (online_since_unix_secs, session_uplink_bytes, session_downlink_bytes) = match (
        online_since_unix_secs,
        session_uplink_bytes,
        session_downlink_bytes,
    ) {
        (None, None, None) => (None, None, None),
        (Some(online_since), uplink, downlink)
            if online_since > 0 && uplink.unwrap_or(0) >= 0 && downlink.unwrap_or(0) >= 0 =>
        {
            // These are ordinary proto3 scalars: Go omits a byte counter when it is zero.
            // A positive epoch identifies session-capable Xray; only within that epoch may
            // omitted byte fields mean zero. Legacy sources still retain unknown metrics.
            (
                Some(online_since),
                Some(uplink.unwrap_or(0) as u64),
                Some(downlink.unwrap_or(0) as u64),
            )
        }
        _ => {
            return Err("xray sent incomplete or invalid online source session metrics".to_owned())
        }
    };
    protocols.sort_unstable();
    protocols.dedup();
    Ok(OnlineSource {
        ip,
        last_seen_unix_secs,
        protocols: (!protocols.is_empty()).then_some(protocols),
        online_since_unix_secs,
        session_uplink_bytes,
        session_downlink_bytes,
    })
}

/// `GetInboundUserResponse { repeated xray.common.protocol.User users = 1 }`.
fn decode_inbound_users(message: &[u8]) -> Result<Vec<XrayUser>, String> {
    let mut users = Vec::new();
    let mut cursor = 0;
    while let Some((number, field)) = next_field(message, &mut cursor)? {
        if let (1, Field::Bytes(bytes)) = (number, field) {
            users.push(decode_user(bytes)?);
        }
    }
    Ok(users)
}

/// `ListInboundsResponse { repeated core.InboundHandlerConfig inbounds = 1 }`; only the tag field
/// is returned because the request sets `isOnlyTags`.
fn decode_inbound_tags(message: &[u8]) -> Result<Vec<String>, String> {
    let mut tags = Vec::new();
    let mut cursor = 0;
    while let Some((number, field)) = next_field(message, &mut cursor)? {
        let (1, Field::Bytes(inbound)) = (number, field) else {
            continue;
        };
        let mut inbound_cursor = 0;
        while let Some((number, field)) = next_field(inbound, &mut inbound_cursor)? {
            if let (1, Field::Bytes(tag)) = (number, field) {
                tags.push(utf8(tag, "inbound tag")?);
            }
        }
    }
    tags.sort();
    tags.dedup();
    Ok(tags)
}

/// `User { uint32 level = 1; string email = 2; TypedMessage account = 3 }`.
fn decode_user(message: &[u8]) -> Result<XrayUser, String> {
    let mut email = None;
    let mut account = None;
    let mut cursor = 0;
    while let Some((number, field)) = next_field(message, &mut cursor)? {
        match (number, field) {
            (2, Field::Bytes(bytes)) => email = Some(utf8(bytes, "user email")?),
            (3, Field::Bytes(bytes)) => account = decode_account(bytes)?,
            _ => {}
        }
    }
    let email = email.ok_or_else(|| "xray sent an inbound user without an email".to_owned())?;
    let (credential, flow) = account.ok_or_else(|| {
        format!("xray sent inbound user {email} without a readable VLESS or Hysteria 2 account")
    })?;
    Ok(XrayUser {
        email,
        credential,
        flow,
    })
}

/// `TypedMessage { string type = 1; bytes value = 2 }`, then the account the type names.
///
/// An account of any other type is not an error to decode — it is a protocol this agent does not
/// write, so it has nothing to compare against and reports no credential.
fn decode_account(message: &[u8]) -> Result<Option<(String, Option<String>)>, String> {
    let mut type_name = None;
    let mut value = None;
    let mut cursor = 0;
    while let Some((number, field)) = next_field(message, &mut cursor)? {
        match (number, field) {
            (1, Field::Bytes(bytes)) => type_name = Some(utf8(bytes, "account type")?),
            (2, Field::Bytes(bytes)) => value = Some(bytes),
            _ => {}
        }
    }
    let (Some(type_name), Some(value)) = (type_name, value) else {
        return Ok(None);
    };

    let mut id = None;
    let mut flow = None;
    let mut cursor = 0;
    while let Some((number, field)) = next_field(value, &mut cursor)? {
        match (type_name.as_str(), number, field) {
            // xray.proxy.vless.Account { string id = 1; string flow = 2 }
            (VLESS_ACCOUNT, 1, Field::Bytes(bytes)) => id = Some(utf8(bytes, "VLESS id")?),
            (VLESS_ACCOUNT, 2, Field::Bytes(bytes)) => flow = Some(utf8(bytes, "VLESS flow")?),
            // xray.proxy.hysteria.account.Account { string auth = 1 }
            (HYSTERIA2_ACCOUNT, 1, Field::Bytes(bytes)) => {
                id = Some(utf8(bytes, "Hysteria 2 auth")?)
            }
            // xray.proxy.anytls.Account { string password = 1 }
            (ANYTLS_ACCOUNT, 1, Field::Bytes(bytes)) => id = Some(utf8(bytes, "AnyTLS password")?),
            (MTPROTO_ACCOUNT, 1, Field::Bytes(bytes)) => id = Some(utf8(bytes, "MTProxy secret")?),
            _ => {}
        }
    }
    // An empty flow is how xray says "none", and it must not read back as a flow of "".
    Ok(id.map(|id| (id, flow.filter(|flow| !flow.is_empty()))))
}

pub(crate) fn reverse_health(
    api_address: SocketAddr,
) -> Result<brocade_deployment::protocol::ReverseHealthReport, String> {
    let response = grpc_unary(api_address, REVERSE_HEALTH_SNAPSHOT_PATH, &[])?;
    let mut cursor = 0;
    while let Some((number, field)) = next_field(&response, &mut cursor)? {
        if let (1, Field::Bytes(bytes)) = (number, field) {
            if bytes.len() > 256 * 1024 {
                return Err("reverse health snapshot exceeds telemetry limit".into());
            }
            return serde_json::from_slice(bytes)
                .map_err(|err| format!("invalid reverse health snapshot: {err}"));
        }
    }
    Err("missing reverse health snapshot".into())
}

pub(crate) fn mux_snapshot(
    api_address: SocketAddr,
) -> Result<brocade_deployment::protocol::MuxReport, String> {
    let response = grpc_unary(api_address, MUX_SNAPSHOT_PATH, &[])?;
    let mut cursor = 0;
    while let Some((number, field)) = next_field(&response, &mut cursor)? {
        if let (1, Field::Bytes(bytes)) = (number, field) {
            // Leave room for the NIC fields and WebSocket framing under the realtime channel's
            // 512 KiB message ceiling. A pathological number of pools becomes "no Mux report"
            // for this frame instead of repeatedly tearing down the Agent connection.
            if bytes.len() > 384 * 1024 {
                return Err("mux snapshot exceeds telemetry limit".into());
            }
            return serde_json::from_slice(bytes)
                .map_err(|err| format!("invalid mux snapshot: {err}"));
        }
    }
    Err("missing mux snapshot".into())
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    #[test]
    fn protobuf_keeps_hysteria_auth_separate_from_vless_id() {
        let hysteria = {
            let mut account = Vec::new();
            bytes_field(&mut account, 1, b"hy-secret");
            typed_message(HYSTERIA2_ACCOUNT, &account)
        };
        assert!(hysteria
            .windows(HYSTERIA2_ACCOUNT.len())
            .any(|window| window == HYSTERIA2_ACCOUNT.as_bytes()));
        assert!(!hysteria
            .windows(VLESS_ACCOUNT.len())
            .any(|window| window == VLESS_ACCOUNT.as_bytes()));
        assert!(hysteria.windows(9).any(|window| window == b"hy-secret"));
    }

    #[test]
    fn request_headers_use_grpc_h2c_shape() {
        let headers = request_headers(crate::DEFAULT_XRAY_API_ADDRESS, ALTER_INBOUND_PATH);
        assert_eq!(&headers[..2], &[0x83, 0x86]);
        assert!(headers
            .windows(ALTER_INBOUND_PATH.len())
            .any(|window| window == ALTER_INBOUND_PATH.as_bytes()));
        assert!(headers
            .windows("application/grpc".len())
            .any(|window| window == b"application/grpc"));

        let stats = request_headers(crate::DEFAULT_XRAY_API_ADDRESS, QUERY_STATS_PATH);
        assert!(stats
            .windows(QUERY_STATS_PATH.len())
            .any(|window| window == QUERY_STATS_PATH.as_bytes()));
        let online = request_headers(crate::DEFAULT_XRAY_API_ADDRESS, GET_USERS_STATS_PATH);
        assert!(online
            .windows(GET_USERS_STATS_PATH.len())
            .any(|window| window == GET_USERS_STATS_PATH.as_bytes()));
        for path in [ADD_INBOUND_PATH, REMOVE_INBOUND_PATH, LIST_INBOUNDS_PATH] {
            let headers = request_headers(crate::DEFAULT_XRAY_API_ADDRESS, path);
            assert!(headers
                .windows(path.len())
                .any(|window| window == path.as_bytes()));
        }
    }

    #[test]
    fn grpc_dials_the_configured_ip_and_sends_its_authority() {
        use std::{net::TcpListener, thread, time::Instant};

        // 127.0.0.2 also catches accidental fallback to the usual IPv4 localhost address.
        for ip in [
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ] {
            let listener = TcpListener::bind((ip, 0)).unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let server = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "no connection to {address}");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept {address}: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut preface = [0_u8; 24];
                stream.read_exact(&mut preface).unwrap();
                assert_eq!(&preface, CLIENT_PREFACE);
                let mut headers = Vec::new();
                loop {
                    let frame = read_frame(&mut stream).unwrap();
                    if frame.kind == 1 {
                        headers = frame.payload;
                    } else if frame.kind == 0 && frame.flags & END_STREAM != 0 {
                        break;
                    }
                }
                write_frame(&mut stream, 4, 0, 0, &[]).unwrap();
                write_frame(&mut stream, 1, END_HEADERS, 1, &[0x88]).unwrap();
                write_frame(&mut stream, 0, END_STREAM, 1, &[0; 5]).unwrap();
                // Drain the client's SETTINGS ACK before closing, otherwise unread receive
                // data can make TCP send RST and discard the response we just wrote.
                let acknowledgement = read_frame(&mut stream).unwrap();
                assert_eq!((acknowledgement.kind, acknowledgement.flags), (4, ACK));
                headers
            });
            let response = inbound_tags(address);
            let headers = server.join().unwrap();
            assert_eq!(response.unwrap(), Vec::<String>::new());
            let authority = address.to_string();
            assert!(headers
                .windows(authority.len())
                .any(|bytes| bytes == authority.as_bytes()));
            assert!(headers
                .windows(LIST_INBOUNDS_PATH.len())
                .any(|bytes| bytes == LIST_INBOUNDS_PATH.as_bytes()));
        }
    }

    fn bytes_fields(message: &[u8], wanted: u32) -> Vec<Vec<u8>> {
        let mut fields = Vec::new();
        let mut cursor = 0;
        while let Some((number, field)) = next_field(message, &mut cursor).unwrap() {
            if number == wanted {
                if let Field::Bytes(bytes) = field {
                    fields.push(bytes.to_vec());
                }
            }
        }
        fields
    }

    fn varint_fields(message: &[u8], wanted: u32) -> Vec<u64> {
        let mut fields = Vec::new();
        let mut cursor = 0;
        while let Some((number, field)) = next_field(message, &mut cursor).unwrap() {
            if number == wanted {
                if let Field::Varint(value) = field {
                    fields.push(value);
                }
            }
        }
        fields
    }

    fn typed_value(message: &[u8]) -> (String, Vec<u8>) {
        let type_name = bytes_fields(message, 1).remove(0);
        let value = bytes_fields(message, 2).remove(0);
        (String::from_utf8(type_name).unwrap(), value)
    }

    #[test]
    fn forwarding_inbound_encodes_port_target_and_both_networks() {
        let request = forwarding_inbound_request(
            "fwd:42",
            ForwardingProtocol::TcpUdp,
            20_042,
            "example.com",
            8443,
            false,
        );
        let inbound = bytes_fields(&request, 1).remove(0);
        assert_eq!(bytes_fields(&inbound, 1), [b"fwd:42".to_vec()]);

        let receiver = bytes_fields(&inbound, 2).remove(0);
        let (receiver_type, receiver) = typed_value(&receiver);
        assert_eq!(receiver_type, RECEIVER_CONFIG);
        let port_list = bytes_fields(&receiver, 1).remove(0);
        let port_range = bytes_fields(&port_list, 1).remove(0);
        assert_eq!(varint_fields(&port_range, 1), [20_042]);
        assert_eq!(varint_fields(&port_range, 2), [20_042]);

        let proxy = bytes_fields(&inbound, 3).remove(0);
        let (proxy_type, proxy) = typed_value(&proxy);
        assert_eq!(proxy_type, FORWARDING_CONFIG);
        let proxy = bytes_fields(&proxy, 1).remove(0);
        let address = bytes_fields(&proxy, 1).remove(0);
        assert_eq!(bytes_fields(&address, 2), [b"example.com".to_vec()]);
        assert_eq!(varint_fields(&proxy, 2), [8443]);
        assert_eq!(varint_fields(&proxy, 7), [2, 3]);
        let policy = bytes_fields(&proxy, 8).remove(0);
        let blocked = bytes_fields(&policy, 1);
        assert!(blocked.contains(&b"127.0.0.0/8".to_vec()));
        assert!(blocked.contains(&b"10.0.0.0/8".to_vec()));
    }

    #[test]
    fn recovery_proves_the_actual_forwarding_target_instead_of_trusting_its_tag() {
        for (host, public) in [
            ("1.1.1.1", true),
            ("2606:4700:4700::1111", true),
            ("127.0.0.1", false),
            ("::ffff:127.0.0.1", false),
            ("localhost", false),
            ("10.0.0.1", false),
        ] {
            let request = forwarding_inbound_request(
                "fwd:42",
                ForwardingProtocol::Tcp,
                40042,
                host,
                443,
                false,
            );
            let inbound = bytes_fields(&request, 1).remove(0);
            let proxy = bytes_fields(&inbound, 3).remove(0);
            assert_eq!(
                decode_public_forwarding_target(&proxy).unwrap(),
                public,
                "{host}"
            );
            let (_, protected) = typed_value(&proxy);
            let legacy = bytes_fields(&protected, 1).remove(0);
            assert_eq!(
                decode_public_forwarding_target(&typed_message(
                    "xray.proxy.dokodemo.Config",
                    &legacy
                ))
                .unwrap(),
                public,
                "legacy {host}"
            );
        }
    }

    #[test]
    fn inbound_tag_snapshot_is_sorted_and_deduplicated() {
        let mut response = Vec::new();
        for tag in ["fwd:9", "api", "fwd:2", "fwd:9"] {
            let mut inbound = Vec::new();
            bytes_field(&mut inbound, 1, tag.as_bytes());
            message_field(&mut response, 1, &inbound);
        }
        assert_eq!(
            decode_inbound_tags(&response).unwrap(),
            ["api", "fwd:2", "fwd:9"]
        );
    }

    #[test]
    fn grpc_success_requires_a_complete_unary_response_message() {
        assert!(grpc_response_message(&[0, 0, 0, 0, 0]).unwrap().is_empty());
        assert_eq!(
            grpc_response_message(&[0, 0, 0, 0, 2, 7, 8]).unwrap(),
            vec![7, 8]
        );
        // Trailers-only: the call failed and said so in HPACK this client cannot read.
        assert!(grpc_response_message(&[]).is_err());
        assert!(grpc_response_message(&[0, 0, 0, 0, 1]).is_err());
    }

    /// Both windows are opened before the request, or a large `QueryStats` answer stalls at
    /// 64 KiB and comes back as a read timeout.
    #[test]
    fn flow_control_windows_are_opened_past_the_default() {
        let payload = settings_payload();
        assert_eq!(&payload[..2], &SETTINGS_INITIAL_WINDOW_SIZE.to_be_bytes());
        assert_eq!(&payload[2..], &WINDOW_SIZE.to_be_bytes());
    }

    fn encode_stat(name: &str, value: i64) -> Vec<u8> {
        let mut stat = Vec::new();
        bytes_field(&mut stat, 1, name.as_bytes());
        varint_field(&mut stat, 2, value as u64);
        stat
    }

    #[test]
    fn decodes_a_query_stats_response() {
        let mut response = Vec::new();
        message_field(
            &mut response,
            1,
            &encode_stat("user>>>alice@platform#i-main>>>traffic>>>uplink", 12),
        );
        message_field(
            &mut response,
            1,
            &encode_stat("user>>>alice@platform#i-main>>>traffic>>>downlink", 34),
        );

        let stats = decode_stats(&response).unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(
            stats[0].name,
            "user>>>alice@platform#i-main>>>traffic>>>uplink"
        );
        assert_eq!(stats[0].value, 12);
        assert_eq!(stats[1].value, 34);

        // No counter matched the pattern: an empty message, not a failure.
        assert!(decode_stats(&[]).unwrap().is_empty());
    }

    #[test]
    fn decodes_and_sorts_an_online_source_snapshot() {
        fn source(ip: &str, last_seen: i64) -> Vec<u8> {
            let mut source = Vec::new();
            bytes_field(&mut source, 1, ip.as_bytes());
            varint_field(&mut source, 2, last_seen as u64);
            source
        }
        fn user(label: &str, sources: &[(&str, i64)]) -> Vec<u8> {
            let mut user = Vec::new();
            bytes_field(&mut user, 1, label.as_bytes());
            for (ip, last_seen) in sources {
                message_field(&mut user, 2, &source(ip, *last_seen));
            }
            user
        }

        let mut response = Vec::new();
        message_field(
            &mut response,
            1,
            &user(
                "bob@platform#i-main",
                &[("[2606:4700:4700::1111]", 1_767_225_602)],
            ),
        );
        message_field(
            &mut response,
            1,
            &user(
                "alice@platform#i-main",
                &[("8.8.8.8", 1_767_225_601), ("1.1.1.1", 1_767_225_600)],
            ),
        );

        assert_eq!(
            decode_online_sources(&response).unwrap(),
            vec![
                UserOnlineSources {
                    label: "alice@platform#i-main".to_owned(),
                    sources: vec![
                        OnlineSource {
                            ip: "1.1.1.1".to_owned(),
                            last_seen_unix_secs: 1_767_225_600,
                            protocols: None,
                            online_since_unix_secs: None,
                            session_uplink_bytes: None,
                            session_downlink_bytes: None,
                        },
                        OnlineSource {
                            ip: "8.8.8.8".to_owned(),
                            last_seen_unix_secs: 1_767_225_601,
                            protocols: None,
                            online_since_unix_secs: None,
                            session_uplink_bytes: None,
                            session_downlink_bytes: None,
                        },
                    ],
                },
                UserOnlineSources {
                    label: "bob@platform#i-main".to_owned(),
                    sources: vec![OnlineSource {
                        ip: "2606:4700:4700::1111".to_owned(),
                        last_seen_unix_secs: 1_767_225_602,
                        protocols: None,
                        online_since_unix_secs: None,
                        session_uplink_bytes: None,
                        session_downlink_bytes: None,
                    }],
                },
            ]
        );
        assert!(decode_online_sources(&[]).unwrap().is_empty());
    }

    #[test]
    fn decodes_bounded_protocol_sets_without_guessing_legacy_sources() {
        let mut source = Vec::new();
        bytes_field(&mut source, 1, b"1.1.1.1");
        varint_field(&mut source, 2, 1_767_225_600);
        assert_eq!(decode_online_source(&source).unwrap().protocols, None);
        for name in [
            "vless",
            "anytls",
            "hysteria",
            "mtproto",
            "vless",
            "future-protocol",
            "",
        ] {
            bytes_field(&mut source, 3, name.as_bytes());
        }
        assert_eq!(
            decode_online_source(&source).unwrap().protocols,
            Some(vec![
                OnlineSourceProtocol::Vless,
                OnlineSourceProtocol::AnyTls,
                OnlineSourceProtocol::Hysteria2,
                OnlineSourceProtocol::MtProto,
                OnlineSourceProtocol::Unknown,
            ])
        );
        for _ in 0..3 {
            bytes_field(&mut source, 3, b"vless");
        }
        assert!(decode_online_source(&source).is_err());
    }

    #[test]
    fn decodes_optional_online_session_metrics_with_proto3_zero_defaults() {
        let base = || {
            let mut source = Vec::new();
            bytes_field(&mut source, 1, b"1.1.1.1");
            varint_field(&mut source, 2, 1_767_225_600);
            source
        };
        let legacy = decode_online_source(&base()).unwrap();
        assert_eq!(legacy.online_since_unix_secs, None);
        assert_eq!(legacy.session_uplink_bytes, None);
        assert_eq!(legacy.session_downlink_bytes, None);

        let mut measured = base();
        varint_field(&mut measured, 4, 1_767_225_500);
        varint_field(&mut measured, 5, 123);
        varint_field(&mut measured, 6, 456);
        let measured = decode_online_source(&measured).unwrap();
        assert_eq!(measured.online_since_unix_secs, Some(1_767_225_500));
        assert_eq!(measured.session_uplink_bytes, Some(123));
        assert_eq!(measured.session_downlink_bytes, Some(456));

        for (uplink, downlink) in [
            (None, None),
            (Some(123), None),
            (None, Some(456)),
            (Some(0), Some(0)),
        ] {
            let mut source = base();
            varint_field(&mut source, 4, 1_767_225_500);
            if let Some(value) = uplink {
                varint_field(&mut source, 5, value);
            }
            if let Some(value) = downlink {
                varint_field(&mut source, 6, value);
            }
            let decoded = decode_online_source(&source).expect("proto3 omits zero-valued counters");
            assert_eq!(decoded.online_since_unix_secs, Some(1_767_225_500));
            assert_eq!(decoded.session_uplink_bytes, Some(uplink.unwrap_or(0)));
            assert_eq!(decoded.session_downlink_bytes, Some(downlink.unwrap_or(0)));
        }

        for fields in [
            vec![(5, 123)],
            vec![(6, 456)],
            vec![(5, 0), (6, 0)],
            vec![(4, 0)],
            vec![(4, (-1_i64) as u64)],
            vec![(4, 1_767_225_500), (5, (-1_i64) as u64)],
            vec![(4, 1_767_225_500), (6, (-1_i64) as u64)],
        ] {
            let mut invalid = base();
            for (field, value) in fields {
                varint_field(&mut invalid, field, value);
            }
            assert!(
                decode_online_source(&invalid).is_err(),
                "reject invalid session metadata"
            );
        }
        for field in 4..=6 {
            let mut invalid = base();
            varint_field(&mut invalid, 4, 1_767_225_500);
            bytes_field(&mut invalid, field, b"not-a-counter");
            assert!(
                decode_online_source(&invalid).is_err(),
                "a malformed field is not an omitted zero"
            );
        }
    }

    #[test]
    fn normalizes_xray_online_source_ip_formats() {
        for (raw, expected) in [
            ("192.0.2.1", "192.0.2.1"),
            ("2001:0DB8:0:0:0:0:0:1", "2001:db8::1"),
            ("[2001:0DB8:0:0:0:0:0:1]", "2001:db8::1"),
            ("[::ffff:192.0.2.1]", "::ffff:192.0.2.1"),
        ] {
            let mut source = Vec::new();
            bytes_field(&mut source, 1, raw.as_bytes());
            varint_field(&mut source, 2, 1_767_225_600);
            let decoded = decode_online_source(&source).unwrap();
            assert_eq!(decoded.ip, expected);
            assert_eq!(decoded.last_seen_unix_secs, 1_767_225_600);
        }
    }

    #[test]
    fn rejects_malformed_online_source_address_wrappers() {
        for raw in [
            "[]",
            "[2001:db8::1",
            "2001:db8::1]",
            "[[2001:db8::1]]",
            "[2001:db8::1]:443",
            "192.0.2.1:443",
            "[192.0.2.1]",
            "[example.com]",
            "[fe80::1%eth0]",
            " [2001:db8::1] ",
        ] {
            let mut source = Vec::new();
            bytes_field(&mut source, 1, raw.as_bytes());
            varint_field(&mut source, 2, 1_767_225_600);
            assert!(decode_online_source(&source).is_err(), "accepted {raw}");
        }
    }

    #[test]
    fn rejects_invalid_online_source_entries() {
        let mut source = Vec::new();
        bytes_field(&mut source, 1, b"not-an-ip");
        varint_field(&mut source, 2, 1);
        let mut user = Vec::new();
        bytes_field(&mut user, 1, b"alice@platform#i-main");
        message_field(&mut user, 2, &source);
        let mut response = Vec::new();
        message_field(&mut response, 1, &user);
        assert!(decode_online_sources(&response).is_err());
    }

    /// The field this reads is the one this module writes. Encoding an account and decoding it
    /// back is the only check here that would catch a field number drifting apart between the two
    /// halves — and a drift in `add_user` writes credentials xray never sees.
    #[test]
    fn decodes_the_accounts_it_encodes() {
        let mut vless = Vec::new();
        bytes_field(&mut vless, 1, b"11111111-2222-3333-4444-555555555555");
        bytes_field(&mut vless, 2, b"xtls-rprx-vision");
        let vless = typed_message(VLESS_ACCOUNT, &vless);
        assert_eq!(
            decode_account(&vless).unwrap(),
            Some((
                "11111111-2222-3333-4444-555555555555".to_owned(),
                Some("xtls-rprx-vision".to_owned())
            ))
        );

        let mut hysteria = Vec::new();
        bytes_field(&mut hysteria, 1, b"hy-secret");
        let hysteria = typed_message(HYSTERIA2_ACCOUNT, &hysteria);
        assert_eq!(
            decode_account(&hysteria).unwrap(),
            Some(("hy-secret".to_owned(), None))
        );

        let mut mtproto = Vec::new();
        bytes_field(&mut mtproto, 1, b"11111111-2222-3333-4444-555555555555");
        let mtproto = typed_message(MTPROTO_ACCOUNT, &mtproto);
        assert_eq!(
            decode_account(&mtproto).unwrap(),
            Some(("11111111-2222-3333-4444-555555555555".to_owned(), None,))
        );

        // An empty flow is xray saying "none"; it must not read back as a flow named "".
        let mut flowless = Vec::new();
        bytes_field(&mut flowless, 1, b"an-id");
        bytes_field(&mut flowless, 2, b"");
        let flowless = typed_message(VLESS_ACCOUNT, &flowless);
        assert_eq!(
            decode_account(&flowless).unwrap(),
            Some(("an-id".to_owned(), None))
        );

        // A protocol this agent does not write: readable, but no credential to compare.
        let other = typed_message("xray.proxy.trojan.Account", &[]);
        assert_eq!(decode_account(&other).unwrap(), None);
    }

    #[test]
    fn decodes_an_inbound_user_response_and_skips_unknown_fields() {
        let mut account = Vec::new();
        bytes_field(&mut account, 1, b"11111111-2222-3333-4444-555555555555");
        let account = typed_message(VLESS_ACCOUNT, &account);

        let mut user = Vec::new();
        // level: a varint field this decoder does not read, and has to step over.
        varint_field(&mut user, 1, 7);
        bytes_field(&mut user, 2, b"alice@platform");
        message_field(&mut user, 3, &account);

        let mut response = Vec::new();
        message_field(&mut response, 1, &user);

        let users = decode_inbound_users(&response).unwrap();
        assert_eq!(
            users,
            vec![XrayUser {
                email: "alice@platform".to_owned(),
                credential: "11111111-2222-3333-4444-555555555555".to_owned(),
                flow: None,
            }]
        );
    }

    #[test]
    fn a_truncated_message_is_an_error_rather_than_a_short_read() {
        // Field 1, length 9, three bytes of payload.
        assert!(decode_stats(&[0x0a, 0x09, 1, 2, 3]).is_err());
        // A varint that never terminates.
        assert!(decode_stats(&[0xff; 12]).is_err());
    }

    /// Release-time compatibility check against the selected Xray, falling back to `.tools`.
    #[test]
    #[ignore = "requires the repository's pinned Xray binary"]
    fn native_grpc_round_trips_against_xray() {
        native_grpc_round_trips_on(IpAddr::V4(Ipv4Addr::LOCALHOST), false);
    }

    #[test]
    #[ignore = "requires the repository's pinned Xray binary and IPv6 loopback"]
    fn native_grpc_round_trips_against_direct_ipv6_xray_api() {
        native_grpc_round_trips_on(IpAddr::V6(Ipv6Addr::LOCALHOST), true);
    }

    fn native_grpc_round_trips_on(api_ip: IpAddr, direct_api: bool) {
        use std::{
            fs,
            net::TcpListener,
            path::{Path, PathBuf},
            process::{Command, Stdio},
            thread,
            time::{SystemTime, UNIX_EPOCH},
        };

        let binary = std::env::var_os("BROCADE_XRAY_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tools/xray"));
        assert!(binary.exists(), "{} is missing", binary.display());
        let api_address = TcpListener::bind((api_ip, 0))
            .unwrap()
            .local_addr()
            .unwrap();
        let inbound_port = free_port();
        let hysteria_port = free_port();
        let mtproto_port = free_port();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("brocade-xray-grpc-{}-{unique}", std::process::id()));
        fs::create_dir(&directory).expect("create test directory");
        let certificate = directory.join("certificate.pem");
        let private_key = directory.join("private-key.pem");
        let openssl = Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=localhost",
                "-keyout",
                private_key.to_str().expect("utf-8 path"),
                "-out",
                certificate.to_str().expect("utf-8 path"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run openssl");
        assert!(openssl.success(), "generate test certificate");
        let config = directory.join("xray.json");
        let mut content = serde_json::json!({
            "log": { "loglevel": "warning" },
            "api": { "tag": "api", "services": ["HandlerService", "StatsService", "RoutingService"] },
            "stats": {},
            "policy": { "levels": { "0": {
                "statsUserUplink": true,
                "statsUserDownlink": true,
                "statsUserOnline": true
            } } },
            "inbounds": [
                {
                    "tag": "api",
                    "listen": api_ip.to_string(),
                    "port": api_address.port(),
                    "protocol": "dokodemo-door",
                    "settings": { "address": "127.0.0.1" }
                },
                {
                    "tag": "test-vless",
                    "listen": "127.0.0.1",
                    "port": inbound_port,
                    "protocol": "vless",
                    "settings": { "clients": [], "decryption": "none" },
                    // A local-only PROXY header supplies a non-loopback fixture source,
                    // since Xray deliberately excludes localhost from presence statistics.
                    "streamSettings": { "sockopt": { "acceptProxyProtocol": true } }
                },
                {
                    "tag": "test-hysteria2",
                    "listen": "127.0.0.1",
                    "port": hysteria_port,
                    "protocol": "hysteria",
                    "settings": { "clients": [] },
                    "streamSettings": {
                        "network": "hysteria",
                        "security": "tls",
                        "tlsSettings": {
                            "alpn": ["h3"],
                            "certificates": [{
                                "certificateFile": certificate,
                                "keyFile": private_key
                            }]
                        },
                        "hysteriaSettings": {
                            "version": 2,
                            "masquerade": { "type": "404" }
                        },
                        "finalmask": {
                            "quicParams": { "congestion": "bbr" }
                        }
                    }
                },
                {
                    "tag": "test-mtproto",
                    "listen": "127.0.0.1",
                    "port": mtproto_port,
                    "protocol": "mtproto",
                    "settings": { "users": [] },
                    "streamSettings": { "network": "tcp", "security": "none" },
                    "sniffing": { "enabled": false }
                }
            ],
            // The integration target is loopback; override the normal private-IP deny only
            // inside this isolated fixture, never in generated production configuration.
            "outbounds": [{ "tag": "out:forward", "protocol": "freedom", "settings": { "ipsBlocked": [] } }],
            "routing": { "rules": [{
                "type": "field",
                "inboundTag": ["api"],
                "outboundTag": "api"
            }] }
        });
        if direct_api {
            content["api"]["listen"] = api_address.to_string().into();
            content["inbounds"].as_array_mut().unwrap().remove(0);
            content["routing"]["rules"] = serde_json::json!([]);
        }
        let encoded = serde_json::to_string(&content).unwrap();
        assert_eq!(crate::xray_api_address(&encoded), Some(api_address));
        fs::write(&config, encoded).expect("write config");

        struct TestXray(std::process::Child);
        impl Drop for TestXray {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = TestXray(
            Command::new(&binary)
                .args(["run", "-config", config.to_str().expect("utf-8 path")])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("start xray"),
        );
        for _ in 0..50 {
            if TcpStream::connect(api_address).is_ok() {
                break;
            }
            if let Some(status) = child.0.try_wait().expect("poll xray") {
                panic!("xray exited before API became ready: {status}");
            }
            thread::sleep(Duration::from_millis(100));
        }

        for pattern in ["otherdomain>>>", "sniffingfailed>>>"] {
            let stats = query_stats(api_address, &format!("{pattern}v2"))
                .expect("native detail checkpoint snapshot");
            assert!(stats
                .iter()
                .any(|row| row.name == format!("{pattern}version") && row.value == 2));
            let checkpoints = (1..=256)
                .map(|id| format!("{id}:0:0"))
                .collect::<Vec<_>>()
                .join(",");
            assert!(query_stats(
                api_address,
                &format!("{pattern}ack>>>{}>>>{checkpoints}", "0".repeat(32))
            )
            .expect("native empty checkpoint acknowledgement")
            .is_empty());
        }

        add_user(
            api_address,
            "test-vless",
            "alice@example.test",
            0,
            XrayAccount::Vless {
                id: "b831381d-6324-4d53-ad4f-8cda48b30811",
                flow: None,
            },
        )
        .expect("native add user");
        // Read back natively: this is the decoder under test facing a real GetInboundUsers
        // response, protobuf and HPACK and all.
        assert_eq!(
            inbound_users(api_address, "test-vless").expect("native list users"),
            vec![XrayUser {
                email: "alice@example.test".to_owned(),
                credential: "b831381d-6324-4d53-ad4f-8cda48b30811".to_owned(),
                flow: None,
            }]
        );

        add_user(
            api_address,
            "test-hysteria2",
            "bob@example.test",
            0,
            XrayAccount::Hysteria2 {
                auth: "hysteria-secret",
            },
        )
        .expect("native add Hysteria 2 user");
        assert_eq!(
            inbound_users(api_address, "test-hysteria2").expect("native list Hysteria 2 users"),
            vec![XrayUser {
                email: "bob@example.test".to_owned(),
                credential: "hysteria-secret".to_owned(),
                flow: None,
            }]
        );
        // The CLI kept as an independent oracle exactly once: if both halves of this module
        // agreed on a wrong field number, every native assertion above would still pass.
        let listed_hysteria = Command::new(&binary)
            .args([
                "api",
                "inbounduser",
                &format!("--server={api_address}"),
                "-tag=test-hysteria2",
            ])
            .output()
            .expect("list Hysteria 2 users");
        assert!(listed_hysteria.status.success());
        let listed_hysteria = String::from_utf8_lossy(&listed_hysteria.stdout);
        assert!(listed_hysteria.contains("bob@example.test"));
        assert!(listed_hysteria.contains("hysteria-secret"));

        add_user(
            api_address,
            "test-mtproto",
            "carol@example.test",
            0,
            XrayAccount::MtProto {
                secret: "00112233-4455-6677-8899-aabbccddeeff",
            },
        )
        .expect("native add MTProxy user");
        assert_eq!(
            inbound_users(api_address, "test-mtproto").expect("native list MTProxy users"),
            vec![XrayUser {
                email: "carol@example.test".to_owned(),
                credential: "00112233-4455-6677-8899-aabbccddeeff".to_owned(),
                flow: None,
            }]
        );

        // No traffic has flowed, so no user counter exists yet. What this proves is the call
        // itself: path, framing, flow-control windows, and an empty response decoding to an
        // empty list rather than an error.
        let health = reverse_health(api_address).expect("read reverse health API");
        assert!(!health.boot_id.is_empty());
        assert!(health.workers.is_empty());
        let mux = mux_snapshot(api_address).expect("read Mux snapshot API");
        assert!(!mux.boot_id.is_empty());
        assert!(mux.pools.is_empty());
        assert!(mux.workers.is_empty());
        assert!(query_stats(api_address, "user>>>")
            .expect("native query stats")
            .is_empty());
        assert!(online_sources(api_address)
            .expect("native online sources")
            .is_empty());

        // Exercise the real authenticated VLESS -> dispatcher -> protobuf -> Agent path,
        // not just a hand-built protobuf message that could agree on the wrong field number.
        let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        target.set_nonblocking(true).unwrap();
        let target_port = target.local_addr().unwrap().port();
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, inbound_port)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request =
            format!("PROXY TCP4 203.0.113.10 127.0.0.1 23456 {inbound_port}\r\n").into_bytes();
        request.push(0); // VLESS version
        request.extend_from_slice(&[
            0xb8, 0x31, 0x38, 0x1d, 0x63, 0x24, 0x4d, 0x53, 0xad, 0x4f, 0x8c, 0xda, 0x48, 0xb3,
            0x08, 0x11,
        ]);
        request.extend_from_slice(&[0, 1]); // no addons, TCP
        request.extend_from_slice(&target_port.to_be_bytes());
        request.extend_from_slice(&[1, 127, 0, 0, 1]); // IPv4 destination
        request.extend_from_slice(b"ping");
        client.write_all(&request).unwrap();
        let mut accepted = None;
        for _ in 0..100 {
            match target.accept() {
                Ok((stream, _)) => {
                    accepted = Some(stream);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(error) => panic!("accept VLESS target: {error}"),
            }
        }
        let mut accepted = accepted.expect("VLESS reaches the local target");
        accepted
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        accepted
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut payload = [0; 4];
        accepted.read_exact(&mut payload).unwrap();
        assert_eq!(&payload, b"ping");
        // No target response has been sent: the real Go protobuf encoder omits downlink=0.
        // A single one-way source must not invalidate the entire node's presence snapshot.
        let one_way =
            online_sources(api_address).expect("read real VLESS source with zero downlink");
        assert_eq!(one_way.len(), 1);
        assert_eq!(one_way[0].sources.len(), 1);
        assert!(one_way[0].sources[0].online_since_unix_secs.is_some());
        assert_eq!(one_way[0].sources[0].session_downlink_bytes, Some(0));
        accepted.write_all(b"pong").unwrap();
        let mut response = [0; 6];
        client.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"\0\0pong");
        let sources = online_sources(api_address).expect("read real VLESS presence");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].label, "alice@example.test");
        assert_eq!(sources[0].sources.len(), 1);
        assert_eq!(sources[0].sources[0].ip, "203.0.113.10");
        assert_eq!(
            sources[0].sources[0].protocols,
            Some(vec![OnlineSourceProtocol::Vless])
        );
        assert!(sources[0].sources[0].online_since_unix_secs.is_some());
        assert!(
            sources[0].sources[0]
                .session_uplink_bytes
                .unwrap_or_default()
                >= 4
        );
        assert!(
            sources[0].sources[0]
                .session_downlink_bytes
                .unwrap_or_default()
                >= 4
        );
        drop(accepted);
        drop(client);
        let mut cleared = false;
        for _ in 0..100 {
            if online_sources(api_address).unwrap().is_empty() {
                cleared = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            cleared,
            "closed VLESS connection must retire its protocol reference"
        );

        remove_user(api_address, "test-vless", "alice@example.test").expect("native remove user");
        assert!(inbound_users(api_address, "test-vless")
            .expect("native list users after removal")
            .is_empty());
        remove_user(api_address, "test-hysteria2", "bob@example.test")
            .expect("native remove Hysteria 2 user");
        remove_user(api_address, "test-mtproto", "carol@example.test")
            .expect("native remove MTProxy user");
        assert!(
            remove_user(api_address, "missing-inbound", "nobody@example.test").is_err(),
            "a trailers-only gRPC error must not be mistaken for success"
        );
        assert!(
            inbound_users(api_address, "missing-inbound").is_err(),
            "listing an inbound that does not exist is an error, not an empty list"
        );

        drop(child);
        let _ = fs::remove_dir_all(directory);

        fn free_port() -> u16 {
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .expect("reserve port")
                .local_addr()
                .expect("local address")
                .port()
        }
    }
}
