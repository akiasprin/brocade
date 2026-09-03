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
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream},
    time::Duration,
};

const ALTER_INBOUND_PATH: &str = "/xray.app.proxyman.command.HandlerService/AlterInbound";
const GET_INBOUND_USERS_PATH: &str = "/xray.app.proxyman.command.HandlerService/GetInboundUsers";
const QUERY_STATS_PATH: &str = "/xray.app.stats.command.StatsService/QueryStats";
const ADD_USER_OPERATION: &str = "xray.app.proxyman.command.AddUserOperation";
const REMOVE_USER_OPERATION: &str = "xray.app.proxyman.command.RemoveUserOperation";
const VLESS_ACCOUNT: &str = "xray.proxy.vless.Account";
const HYSTERIA2_ACCOUNT: &str = "xray.proxy.hysteria.account.Account";
const ANYTLS_ACCOUNT: &str = "xray.proxy.anytls.Account";
const CLIENT_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const END_STREAM: u8 = 0x1;
const ACK: u8 = 0x1;
const END_HEADERS: u8 = 0x4;
const SETTINGS_INITIAL_WINDOW_SIZE: u16 = 0x4;
/// Both flow-control windows are opened this far before the request goes out. The authorization
/// calls never came close to the 64 KiB default, but a `QueryStats` answer is one line per counter
/// per direction, so a machine with a few hundred grants can outrun it — and outrunning it does
/// not fail, it stalls until the read timeout, which reads like an unreachable xray.
const WINDOW_SIZE: u32 = 4 * 1024 * 1024;
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
}

pub(crate) fn add_user(
    api_port: u16,
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
    };

    let mut user = Vec::new();
    if level != 0 {
        varint_field(&mut user, 1, u64::from(level));
    }
    bytes_field(&mut user, 2, email.as_bytes());
    message_field(&mut user, 3, &account);

    let mut operation = Vec::new();
    message_field(&mut operation, 1, &user);
    alter_inbound(api_port, tag, ADD_USER_OPERATION, &operation)
}

pub(crate) fn remove_user(api_port: u16, tag: &str, email: &str) -> Result<(), String> {
    let mut operation = Vec::new();
    bytes_field(&mut operation, 1, email.as_bytes());
    alter_inbound(api_port, tag, REMOVE_USER_OPERATION, &operation)
}

fn alter_inbound(
    api_port: u16,
    tag: &str,
    operation_type: &str,
    operation: &[u8],
) -> Result<(), String> {
    let operation = typed_message(operation_type, operation);
    let mut request = Vec::new();
    bytes_field(&mut request, 1, tag.as_bytes());
    message_field(&mut request, 2, &operation);

    // The response carries nothing: AlterInboundResponse is empty. Its arrival is the answer.
    grpc_unary(api_port, ALTER_INBOUND_PATH, &request).map(|_| ())
}

/// Every counter whose name starts with `pattern`, cumulative since xray started.
///
/// `reset` stays unset (field 2 omitted). The control plane differences successive readings and
/// needs the counters to survive being read; a reset here would hand every other reader — a human
/// with `xray api statsquery`, most of all — a zeroed machine.
pub(crate) fn query_stats(api_port: u16, pattern: &str) -> Result<Vec<XrayStat>, String> {
    let mut request = Vec::new();
    bytes_field(&mut request, 1, pattern.as_bytes());
    let response = grpc_unary(api_port, QUERY_STATS_PATH, &request)?;
    decode_stats(&response)
}

/// The users xray is serving on `tag` right now, account payloads decoded.
///
/// An empty email in the request means "all of them" — the same thing `xray api inbounduser -tag`
/// asked for.
pub(crate) fn inbound_users(api_port: u16, tag: &str) -> Result<Vec<XrayUser>, String> {
    let mut request = Vec::new();
    bytes_field(&mut request, 1, tag.as_bytes());
    let response = grpc_unary(api_port, GET_INBOUND_USERS_PATH, &request)?;
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

/// One request, one response, one connection. Returns the response protobuf, framing stripped.
fn grpc_unary(api_port: u16, path: &str, request: &[u8]) -> Result<Vec<u8>, String> {
    let mut message = Vec::with_capacity(request.len() + 5);
    message.push(0);
    message.extend_from_slice(
        &u32::try_from(request.len())
            .map_err(|_| "xray gRPC request is too large".to_owned())?
            .to_be_bytes(),
    );
    message.extend_from_slice(request);
    let message = &message[..];

    let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, api_port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
        .map_err(|error| format!("connect to xray gRPC api {address}: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| format!("set xray gRPC read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| format!("set xray gRPC write timeout: {error}"))?;
    let _ = stream.set_nodelay(true);

    stream
        .write_all(CLIENT_PREFACE)
        .and_then(|_| write_frame(&mut stream, 4, 0, 0, &settings_payload()))
        // Connection-level flow control is not covered by SETTINGS and starts at 64 KiB whatever
        // the stream window says, so it is opened by hand.
        .and_then(|_| {
            write_frame(
                &mut stream,
                8,
                0,
                0,
                &(WINDOW_SIZE - DEFAULT_WINDOW_SIZE).to_be_bytes(),
            )
        })
        .and_then(|_| {
            write_frame(
                &mut stream,
                1,
                END_HEADERS,
                1,
                &request_headers(api_port, path),
            )
        })
        .and_then(|_| write_frame(&mut stream, 0, END_STREAM, 1, message))
        .and_then(|_| stream.flush())
        .map_err(|error| format!("write xray gRPC request: {error}"))?;

    let mut response_data = Vec::new();
    loop {
        let frame =
            read_frame(&mut stream).map_err(|error| format!("read xray gRPC response: {error}"))?;
        match frame.kind {
            // DATA
            0 if frame.stream_id == 1 => response_data.extend_from_slice(&frame.payload),
            // RST_STREAM
            3 if frame.stream_id == 1 => return Err("xray gRPC reset the stream".to_owned()),
            // SETTINGS: every non-ACK SETTINGS frame must be acknowledged.
            4 if frame.flags & ACK == 0 => write_frame(&mut stream, 4, ACK, 0, &[])
                .map_err(|error| format!("acknowledge xray gRPC settings: {error}"))?,
            // PING
            6 if frame.flags & ACK == 0 && frame.payload.len() == 8 => {
                write_frame(&mut stream, 6, ACK, 0, &frame.payload)
                    .map_err(|error| format!("acknowledge xray gRPC ping: {error}"))?
            }
            // GOAWAY
            7 => return Err("xray gRPC closed the connection".to_owned()),
            _ => {}
        }

        if frame.stream_id == 1 && frame.flags & END_STREAM != 0 {
            return grpc_response_message(&response_data);
        }
    }
}

fn settings_payload() -> Vec<u8> {
    let mut payload = Vec::with_capacity(6);
    payload.extend_from_slice(&SETTINGS_INITIAL_WINDOW_SIZE.to_be_bytes());
    payload.extend_from_slice(&WINDOW_SIZE.to_be_bytes());
    payload
}

fn request_headers(api_port: u16, path: &str) -> Vec<u8> {
    let mut block = vec![0x83, 0x86]; // :method POST, :scheme http
    hpack_literal_indexed_name(&mut block, 4, path); // :path
    hpack_literal_indexed_name(&mut block, 1, &format!("127.0.0.1:{api_port}")); // :authority
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
    stream: &mut TcpStream,
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

fn read_frame(stream: &mut TcpStream) -> std::io::Result<Frame> {
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
            _ => {}
        }
    }
    // An empty flow is how xray says "none", and it must not read back as a flow of "".
    Ok(id.map(|id| (id, flow.filter(|flow| !flow.is_empty()))))
}

#[cfg(test)]
mod tests {
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
        let headers = request_headers(10085, ALTER_INBOUND_PATH);
        assert_eq!(&headers[..2], &[0x83, 0x86]);
        assert!(headers
            .windows(ALTER_INBOUND_PATH.len())
            .any(|window| window == ALTER_INBOUND_PATH.as_bytes()));
        assert!(headers
            .windows("application/grpc".len())
            .any(|window| window == b"application/grpc"));

        let stats = request_headers(10085, QUERY_STATS_PATH);
        assert!(stats
            .windows(QUERY_STATS_PATH.len())
            .any(|window| window == QUERY_STATS_PATH.as_bytes()));
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

    /// Release-time compatibility check against the Xray binary pinned in `.tools`.
    #[test]
    #[ignore = "requires the repository's pinned Xray binary"]
    fn native_grpc_round_trips_against_xray() {
        use std::{
            fs,
            net::TcpListener,
            path::Path,
            process::{Command, Stdio},
            thread,
            time::{SystemTime, UNIX_EPOCH},
        };

        let binary = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tools/xray");
        assert!(binary.exists(), "{} is missing", binary.display());
        let api_port = free_port();
        let inbound_port = free_port();
        let hysteria_port = free_port();
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
        fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "log": { "loglevel": "warning" },
                "api": { "tag": "api", "services": ["HandlerService", "StatsService"] },
                "stats": {},
                "policy": { "levels": { "0": {
                    "statsUserUplink": true,
                    "statsUserDownlink": true
                } } },
                "inbounds": [
                    {
                        "tag": "api",
                        "listen": "127.0.0.1",
                        "port": api_port,
                        "protocol": "dokodemo-door",
                        "settings": { "address": "127.0.0.1" }
                    },
                    {
                        "tag": "test-vless",
                        "listen": "127.0.0.1",
                        "port": inbound_port,
                        "protocol": "vless",
                        "settings": { "clients": [], "decryption": "none" }
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
                    }
                ],
                "outbounds": [{ "tag": "direct", "protocol": "freedom" }],
                "routing": { "rules": [{
                    "type": "field",
                    "inboundTag": ["api"],
                    "outboundTag": "api"
                }] }
            }))
            .expect("serialize config"),
        )
        .expect("write config");

        let mut child = Command::new(&binary)
            .args(["run", "-config", config.to_str().expect("utf-8 path")])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start xray");
        for _ in 0..50 {
            if TcpStream::connect((Ipv4Addr::LOCALHOST, api_port)).is_ok() {
                break;
            }
            if let Some(status) = child.try_wait().expect("poll xray") {
                panic!("xray exited before API became ready: {status}");
            }
            thread::sleep(Duration::from_millis(100));
        }

        add_user(
            api_port,
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
            inbound_users(api_port, "test-vless").expect("native list users"),
            vec![XrayUser {
                email: "alice@example.test".to_owned(),
                credential: "b831381d-6324-4d53-ad4f-8cda48b30811".to_owned(),
                flow: None,
            }]
        );

        add_user(
            api_port,
            "test-hysteria2",
            "bob@example.test",
            0,
            XrayAccount::Hysteria2 {
                auth: "hysteria-secret",
            },
        )
        .expect("native add Hysteria 2 user");
        assert_eq!(
            inbound_users(api_port, "test-hysteria2").expect("native list Hysteria 2 users"),
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
                &format!("--server=127.0.0.1:{api_port}"),
                "-tag=test-hysteria2",
            ])
            .output()
            .expect("list Hysteria 2 users");
        assert!(listed_hysteria.status.success());
        let listed_hysteria = String::from_utf8_lossy(&listed_hysteria.stdout);
        assert!(listed_hysteria.contains("bob@example.test"));
        assert!(listed_hysteria.contains("hysteria-secret"));

        // No traffic has flowed, so no user counter exists yet. What this proves is the call
        // itself: path, framing, flow-control windows, and an empty response decoding to an
        // empty list rather than an error.
        assert!(query_stats(api_port, "user>>>")
            .expect("native query stats")
            .is_empty());

        remove_user(api_port, "test-vless", "alice@example.test").expect("native remove user");
        assert!(inbound_users(api_port, "test-vless")
            .expect("native list users after removal")
            .is_empty());
        remove_user(api_port, "test-hysteria2", "bob@example.test")
            .expect("native remove Hysteria 2 user");
        assert!(
            remove_user(api_port, "missing-inbound", "nobody@example.test").is_err(),
            "a trailers-only gRPC error must not be mistaken for success"
        );
        assert!(
            inbound_users(api_port, "missing-inbound").is_err(),
            "listing an inbound that does not exist is an error, not an empty list"
        );

        let _ = child.kill();
        let _ = child.wait();
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
