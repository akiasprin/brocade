//! VPN Gate catalogue validation and sanitization.
//!
//! The official CSV is data, including its Base64 OpenVPN field. It is never written to disk or
//! handed to OpenVPN verbatim: selected Agents only transport a bounded gzip response, then the
//! Console rebuilds profiles from a small option allow-list before the store sees them.

use std::{collections::BTreeMap, io::Read as _, net::IpAddr};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use brocade_deployment::protocol::VpngateTransport;
use brocade_store::{VpngateServerInput, VpngateSyncBatch};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

pub(crate) const MAX_CATALOG_UPLOAD_BYTES: usize = 4 * 1024 * 1024;
const MAX_FEED_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROFILE_BYTES: usize = 64 * 1024;
const MAX_OPTION_LINE_BYTES: usize = 4 * 1024;
const UNKNOWN_COUNTRY_CODE: &str = "ZZ";

#[derive(Debug)]
pub(crate) enum SyncError {
    TooLarge,
    Feed(String),
}

impl SyncError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => "feed-too-large",
            Self::Feed(_) => "feed-invalid",
        }
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Feed(detail) => formatter.write_str(detail),
            Self::TooLarge => formatter.write_str("VPN Gate feed exceeds the 16 MiB safety limit"),
        }
    }
}

pub(crate) fn parse_compressed_feed(compressed: &[u8]) -> Result<VpngateSyncBatch, SyncError> {
    if compressed.len() > MAX_CATALOG_UPLOAD_BYTES {
        return Err(SyncError::TooLarge);
    }
    let mut decoder = GzDecoder::new(compressed);
    let mut bytes = Vec::new();
    decoder
        .by_ref()
        .take(u64::try_from(MAX_FEED_BYTES).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| SyncError::Feed(format!("VPN Gate gzip response is invalid: {error}")))?;
    if bytes.len() > MAX_FEED_BYTES {
        return Err(SyncError::TooLarge);
    }
    parse_feed(&bytes)
}

fn parse_feed(bytes: &[u8]) -> Result<VpngateSyncBatch, SyncError> {
    let content_sha256 = sha256_hex(bytes);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(bytes);
    let mut fetched_rows = 0_u32;
    let mut rejected_rows = 0_u32;
    let mut servers = BTreeMap::<String, VpngateServerInput>::new();
    let mut opening_marker = false;
    let mut header_seen = false;
    let mut closing_marker = false;
    for record in reader.records() {
        let record = match record {
            Ok(record) => record,
            Err(_) => {
                fetched_rows = fetched_rows.saturating_add(1);
                rejected_rows = rejected_rows.saturating_add(1);
                continue;
            }
        };
        let first = record.get(0).unwrap_or_default().trim();
        if first == "*vpn_servers" {
            opening_marker = true;
            continue;
        }
        if first == "*" {
            closing_marker = true;
            continue;
        }
        if first == "#HostName" {
            header_seen = valid_header(&record);
            continue;
        }
        if first.is_empty() || first.starts_with('*') || first.starts_with('#') {
            continue;
        }
        fetched_rows = fetched_rows.saturating_add(1);
        match parse_record(&record) {
            Ok(server) => {
                if let Some(previous) = servers.get(&server.id) {
                    rejected_rows = rejected_rows.saturating_add(1);
                    if previous.score >= server.score {
                        continue;
                    }
                }
                servers.insert(server.id.clone(), server);
            }
            Err(_) => rejected_rows = rejected_rows.saturating_add(1),
        }
    }
    if !opening_marker || !header_seen || !closing_marker {
        return Err(SyncError::Feed(
            "VPN Gate feed is missing its opening marker, expected header, or closing marker"
                .to_owned(),
        ));
    }
    let servers = servers.into_values().collect::<Vec<_>>();
    let accepted = u32::try_from(servers.len())
        .map_err(|_| SyncError::Feed("VPN Gate feed contains too many rows".to_owned()))?;
    let accounted = accepted
        .checked_add(rejected_rows)
        .ok_or_else(|| SyncError::Feed("VPN Gate row count overflow".to_owned()))?;
    // A duplicate replacement counted the discarded copy as rejected while retaining one copy.
    // `fetched_rows` still counts both rows, so this remains exact.
    if accounted != fetched_rows {
        return Err(SyncError::Feed(
            "VPN Gate row accounting became inconsistent".to_owned(),
        ));
    }
    if fetched_rows >= 10 && accepted.saturating_mul(2) < fetched_rows {
        return Err(SyncError::Feed(format!(
            "VPN Gate feed rejected {rejected_rows} of {fetched_rows} rows; preserving the last complete snapshot"
        )));
    }
    if servers.is_empty() {
        return Err(SyncError::Feed(
            "VPN Gate feed contained no usable OpenVPN profiles".to_owned(),
        ));
    }
    Ok(VpngateSyncBatch {
        content_sha256,
        fetched_rows,
        rejected_rows,
        servers,
    })
}

fn valid_header(record: &csv::StringRecord) -> bool {
    const EXPECTED: [&str; 15] = [
        "#HostName",
        "IP",
        "Score",
        "Ping",
        "Speed",
        "CountryLong",
        "CountryShort",
        "NumVpnSessions",
        "Uptime",
        "TotalUsers",
        "TotalTraffic",
        "LogType",
        "Operator",
        "Message",
        "OpenVPN_ConfigData_Base64",
    ];
    record.len() >= EXPECTED.len()
        && EXPECTED.iter().enumerate().all(|(index, expected)| {
            record
                .get(index)
                .is_some_and(|value| value.trim() == *expected)
        })
}

fn parse_record(record: &csv::StringRecord) -> Result<VpngateServerInput, String> {
    if record.len() < 15 {
        return Err("record has fewer than 15 columns".to_owned());
    }
    let hostname = required(record, 0, "HostName")?;
    let ip = required(record, 1, "IP")?;
    let catalog_ip = ip
        .parse::<IpAddr>()
        .map_err(|_| "IP is not a literal address".to_owned())?;
    let country_code = required(record, 6, "CountryShort")?.to_ascii_uppercase();
    if country_code.len() != 2 || !country_code.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err("CountryShort is not an alpha-2 code".to_owned());
    }
    if country_code == UNKNOWN_COUNTRY_CODE {
        return Err("CountryShort does not identify a routable country or region".to_owned());
    }
    let encoded_profile = required(record, 14, "OpenVPN_ConfigData_Base64")?;
    let decoded = BASE64
        .decode(encoded_profile.as_bytes())
        .map_err(|_| "OpenVPN profile is not Base64".to_owned())?;
    if decoded.len() > MAX_PROFILE_BYTES {
        return Err("OpenVPN profile exceeds 64 KiB".to_owned());
    }
    let decoded =
        std::str::from_utf8(&decoded).map_err(|_| "OpenVPN profile is not UTF-8".to_owned())?;
    let sanitized = sanitize_profile(decoded, catalog_ip)?;
    Ok(VpngateServerInput {
        id: hostname.to_ascii_lowercase(),
        hostname: hostname.to_owned(),
        ip: catalog_ip.to_string(),
        country_code,
        country_name: bounded(required(record, 5, "CountryLong")?, 128),
        score: parse_u64(record, 2, "Score")?,
        ping_ms: parse_optional_u32(record, 3, "Ping")?,
        speed_bps: parse_u64(record, 4, "Speed")?,
        vpn_sessions: parse_u32(record, 7, "NumVpnSessions")?,
        uptime_millis: parse_u64(record, 8, "Uptime")?,
        total_users: parse_u64(record, 9, "TotalUsers")?,
        total_traffic_bytes: parse_u64(record, 10, "TotalTraffic")?,
        log_type: bounded(record.get(11).unwrap_or_default(), 64),
        operator_name: bounded(record.get(12).unwrap_or_default(), 256),
        message: bounded(record.get(13).unwrap_or_default(), 2_000),
        profile_sha256: sha256_hex(sanitized.config.as_bytes()),
        remote_address: sanitized.remote_address,
        remote_port: sanitized.remote_port,
        transport: sanitized.transport,
        openvpn_config: sanitized.config,
    })
}

struct SanitizedProfile {
    config: String,
    remote_address: String,
    remote_port: u16,
    transport: VpngateTransport,
}

fn sanitize_profile(source: &str, catalog_ip: IpAddr) -> Result<SanitizedProfile, String> {
    let mut remote = None::<(IpAddr, u16, Option<VpngateTransport>)>;
    let mut protocol = None::<VpngateTransport>;
    let mut options = BTreeMap::<String, Vec<String>>::new();
    let mut blocks = BTreeMap::<String, String>::new();
    let mut active_block = None::<(String, Vec<String>)>;

    for raw in source.lines() {
        let line = raw.trim();
        if let Some((name, lines)) = &mut active_block {
            if line.eq_ignore_ascii_case(&format!("</{name}>")) {
                let name = name.clone();
                let content = format!("{}\n", lines.join("\n"));
                if blocks.insert(name, content).is_some() {
                    return Err("OpenVPN profile repeats an inline block".to_owned());
                }
                active_block = None;
            } else {
                if line.starts_with('<') || raw.as_bytes().contains(&0) {
                    return Err("OpenVPN inline block is malformed".to_owned());
                }
                lines.push(raw.to_owned());
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('<')
            .and_then(|value| value.strip_suffix('>'))
        {
            let name = name.to_ascii_lowercase();
            if !matches!(
                name.as_str(),
                "ca" | "cert" | "key" | "tls-auth" | "tls-crypt" | "tls-crypt-v2"
            ) {
                return Err(format!("OpenVPN inline block <{name}> is not allowed"));
            }
            active_block = Some((name, Vec::new()));
            continue;
        }
        if line.len() > MAX_OPTION_LINE_BYTES || line.as_bytes().contains(&0) {
            return Err("OpenVPN option line is too large or contains NUL".to_owned());
        }
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        let name = fields[0].to_ascii_lowercase();
        match name.as_str() {
            "proto" => {
                if fields.len() != 2 || protocol.replace(parse_transport(fields[1])?).is_some() {
                    return Err("OpenVPN profile has an invalid or repeated proto".to_owned());
                }
            }
            "remote" => {
                if !(fields.len() == 3 || fields.len() == 4) || remote.is_some() {
                    return Err("OpenVPN profile must contain exactly one remote".to_owned());
                }
                let address = fields[1]
                    .parse::<IpAddr>()
                    .map_err(|_| "OpenVPN remote must be a literal IP".to_owned())?;
                let port = fields[2]
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port > 0)
                    .ok_or_else(|| "OpenVPN remote port is invalid".to_owned())?;
                let inline_transport = fields
                    .get(3)
                    .map(|value| parse_transport(value))
                    .transpose()?;
                remote = Some((address, port, inline_transport));
            }
            "client" | "dev" | "nobind" | "persist-key" | "persist-tun" | "auth-nocache"
            | "resolv-retry" | "verb" | "script-security" => {}
            name if safe_option(name) => {
                if fields.len() < 2 {
                    return Err(format!("OpenVPN option {name} has no value"));
                }
                options
                    .entry(name.to_owned())
                    .or_default()
                    .push(fields[1..].join(" "));
            }
            name if dangerous_option(name) => {
                return Err(format!("OpenVPN executable option {name} is forbidden"));
            }
            _ => {}
        }
    }
    if active_block.is_some() {
        return Err("OpenVPN inline block is not closed".to_owned());
    }
    for required in ["ca", "cert", "key"] {
        if !blocks.contains_key(required) {
            return Err(format!("OpenVPN profile is missing <{required}>"));
        }
    }
    let (remote_address, remote_port, inline_transport) =
        remote.ok_or_else(|| "OpenVPN profile is missing remote".to_owned())?;
    if remote_address != catalog_ip {
        return Err("OpenVPN remote does not match the catalogue IP".to_owned());
    }
    let transport = inline_transport
        .or(protocol)
        .ok_or_else(|| "OpenVPN profile is missing proto".to_owned())?;
    if inline_transport.is_some() && protocol.is_some() && inline_transport != protocol {
        return Err("OpenVPN remote transport conflicts with proto".to_owned());
    }
    if let Some(cipher) = options
        .get("cipher")
        .and_then(|values| values.first())
        .cloned()
    {
        options
            .entry("data-ciphers-fallback".to_owned())
            .or_insert_with(|| vec![cipher]);
    }

    let protocol_name = match transport {
        VpngateTransport::Udp => "udp",
        VpngateTransport::Tcp => "tcp-client",
    };
    let mut config = vec![
        "client".to_owned(),
        "dev tun".to_owned(),
        format!("proto {protocol_name}"),
        format!("remote {remote_address} {remote_port}"),
        "nobind".to_owned(),
        "persist-key".to_owned(),
        "persist-tun".to_owned(),
        "auth-nocache".to_owned(),
        "resolv-retry 0".to_owned(),
        "verb 3".to_owned(),
        "script-security 1".to_owned(),
    ];
    for (name, values) in options {
        for value in values {
            config.push(format!("{name} {value}"));
        }
    }
    for name in ["ca", "cert", "key", "tls-auth", "tls-crypt", "tls-crypt-v2"] {
        if let Some(content) = blocks.remove(name) {
            config.push(format!("<{name}>\n{content}</{name}>"));
        }
    }
    Ok(SanitizedProfile {
        config: format!("{}\n", config.join("\n")),
        remote_address: remote_address.to_string(),
        remote_port,
        transport,
    })
}

fn parse_transport(value: &str) -> Result<VpngateTransport, String> {
    match value.to_ascii_lowercase().as_str() {
        "udp" | "udp4" | "udp6" => Ok(VpngateTransport::Udp),
        "tcp" | "tcp4" | "tcp6" | "tcp-client" | "tcp4-client" | "tcp6-client" => {
            Ok(VpngateTransport::Tcp)
        }
        _ => Err("OpenVPN transport is neither UDP nor TCP client".to_owned()),
    }
}

fn safe_option(name: &str) -> bool {
    matches!(
        name,
        "allow-compression"
            | "auth"
            | "cipher"
            | "comp-lzo"
            | "compress"
            | "data-ciphers"
            | "data-ciphers-fallback"
            | "disable-dco"
            | "explicit-exit-notify"
            | "key-direction"
            | "peer-fingerprint"
            | "remote-cert-eku"
            | "remote-cert-ku"
            | "remote-cert-tls"
            | "reneg-sec"
            | "tls-cipher"
            | "tls-ciphersuites"
            | "tls-version-max"
            | "tls-version-min"
            | "verify-x509-name"
    )
}

fn dangerous_option(name: &str) -> bool {
    matches!(
        name,
        "up" | "down"
            | "route-up"
            | "route-pre-down"
            | "ipchange"
            | "plugin"
            | "learn-address"
            | "client-connect"
            | "client-disconnect"
            | "auth-user-pass-verify"
    )
}

fn required<'a>(
    record: &'a csv::StringRecord,
    index: usize,
    field: &str,
) -> Result<&'a str, String> {
    record
        .get(index)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} is empty"))
}

fn parse_u64(record: &csv::StringRecord, index: usize, field: &str) -> Result<u64, String> {
    required(record, index, field)?
        .parse()
        .map_err(|_| format!("{field} is not an unsigned integer"))
}

fn parse_u32(record: &csv::StringRecord, index: usize, field: &str) -> Result<u32, String> {
    required(record, index, field)?
        .parse()
        .map_err(|_| format!("{field} is not an unsigned integer"))
}

fn parse_optional_u32(
    record: &csv::StringRecord,
    index: usize,
    field: &str,
) -> Result<Option<u32>, String> {
    let value = record.get(index).unwrap_or_default().trim();
    if value.is_empty() || value == "-" {
        Ok(None)
    } else {
        value
            .parse()
            .map(Some)
            .map_err(|_| format!("{field} is not an unsigned integer"))
    }
}

fn bounded(value: &str, max_chars: usize) -> String {
    value.trim().chars().take(max_chars).collect()
}

fn sha256_hex(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use flate2::{write::GzEncoder, Compression};

    use super::*;

    fn profile(extra: &str) -> String {
        format!(
            "client\ndev tun\nproto udp\nremote 192.0.2.10 1194\n{extra}\n\
             cipher AES-128-CBC\nauth SHA1\nremote-cert-tls server\n\
             <ca>\nCA\n</ca>\n<cert>\nCERT\n</cert>\n<key>\nKEY\n</key>\n"
        )
    }

    #[test]
    fn sanitizer_rebuilds_only_the_safe_profile_surface() {
        let result = sanitize_profile(&profile("sndbuf 0"), "192.0.2.10".parse().unwrap()).unwrap();
        assert_eq!(result.transport, VpngateTransport::Udp);
        assert_eq!(result.remote_port, 1194);
        assert!(result.config.contains("script-security 1\n"));
        assert!(result
            .config
            .contains("data-ciphers-fallback AES-128-CBC\n"));
        assert!(!result.config.contains("sndbuf"));
    }

    #[test]
    fn sanitizer_rejects_executable_hooks_and_remote_mismatch() {
        assert!(sanitize_profile(
            &profile("up /tmp/provider-script"),
            "192.0.2.10".parse().unwrap()
        )
        .is_err());
        assert!(sanitize_profile(&profile(""), "192.0.2.11".parse().unwrap()).is_err());
    }

    #[test]
    fn official_csv_shape_preserves_commas_and_decodes_the_profile() {
        let encoded = BASE64.encode(profile(""));
        let feed = format!(
            "*vpn_servers\n#HostName,IP,Score,Ping,Speed,CountryLong,CountryShort,NumVpnSessions,Uptime,TotalUsers,TotalTraffic,LogType,Operator,Message,OpenVPN_ConfigData_Base64\n\
             vpn1.example,192.0.2.10,123,21,30000000,Japan,JP,4,5000,10,2000,2weeks,Volunteer,\"Tokyo, fast\",{encoded}\n*\n"
        );
        let batch = parse_feed(feed.as_bytes()).unwrap();
        assert_eq!(batch.fetched_rows, 1);
        assert_eq!(batch.rejected_rows, 0);
        assert_eq!(batch.servers[0].message, "Tokyo, fast");
        assert_eq!(batch.servers[0].remote_port, 1194);
    }

    #[test]
    fn feed_excludes_the_reserved_unknown_country_bucket() {
        let encoded = BASE64.encode(profile(""));
        let feed = format!(
            "*vpn_servers\n#HostName,IP,Score,Ping,Speed,CountryLong,CountryShort,NumVpnSessions,Uptime,TotalUsers,TotalTraffic,LogType,Operator,Message,OpenVPN_ConfigData_Base64\n\
             vpn1.example,192.0.2.10,123,21,30000000,Japan,JP,4,5000,10,2000,2weeks,Volunteer,Tokyo,{encoded}\n\
             unknown.example,192.0.2.10,100,22,20000000,Reserved,ZZ,2,4000,5,1000,2weeks,Volunteer,Unknown,{encoded}\n*\n"
        );

        let batch = parse_feed(feed.as_bytes()).unwrap();

        assert_eq!(batch.fetched_rows, 2);
        assert_eq!(batch.rejected_rows, 1);
        assert_eq!(batch.servers.len(), 1);
        assert_eq!(batch.servers[0].country_code, "JP");
    }

    #[test]
    fn compressed_agent_snapshot_is_decompressed_before_central_sanitization() {
        let encoded = BASE64.encode(profile(""));
        let feed = format!(
            "*vpn_servers\n#HostName,IP,Score,Ping,Speed,CountryLong,CountryShort,NumVpnSessions,Uptime,TotalUsers,TotalTraffic,LogType,Operator,Message,OpenVPN_ConfigData_Base64\n\
             vpn1.example,192.0.2.10,123,21,30000000,Japan,JP,4,5000,10,2000,2weeks,Volunteer,Tokyo,{encoded}\n*\n"
        );
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(feed.as_bytes()).unwrap();

        let batch = parse_compressed_feed(&encoder.finish().unwrap()).unwrap();

        assert_eq!(batch.servers.len(), 1);
        assert_eq!(batch.servers[0].id, "vpn1.example");
        assert!(batch.servers[0]
            .openvpn_config
            .contains("script-security 1"));
    }

    #[test]
    fn incomplete_or_shape_changed_feed_cannot_replace_the_current_catalogue() {
        let encoded = BASE64.encode(profile(""));
        let without_footer = format!(
            "*vpn_servers\n#HostName,IP,Score,Ping,Speed,CountryLong,CountryShort,NumVpnSessions,Uptime,TotalUsers,TotalTraffic,LogType,Operator,Message,OpenVPN_ConfigData_Base64\n\
             vpn1.example,192.0.2.10,123,21,30000000,Japan,JP,4,5000,10,2000,2weeks,Volunteer,Tokyo,{encoded}\n"
        );
        assert!(matches!(
            parse_feed(without_footer.as_bytes()),
            Err(SyncError::Feed(_))
        ));

        let changed_header = without_footer
            .replace("CountryShort", "CountryCode")
            .replace("\nvpn1", "\n*\nvpn1");
        assert!(matches!(
            parse_feed(changed_header.as_bytes()),
            Err(SyncError::Feed(_))
        ));
    }

    #[test]
    fn feed_with_a_large_rejection_ratio_is_not_publishable() {
        let encoded = BASE64.encode(profile(""));
        let mut feed = format!(
            "*vpn_servers\n#HostName,IP,Score,Ping,Speed,CountryLong,CountryShort,NumVpnSessions,Uptime,TotalUsers,TotalTraffic,LogType,Operator,Message,OpenVPN_ConfigData_Base64\n\
             vpn1.example,192.0.2.10,123,21,30000000,Japan,JP,4,5000,10,2000,2weeks,Volunteer,Tokyo,{encoded}\n"
        );
        for _ in 0..9 {
            feed.push_str("broken-row\n");
        }
        feed.push_str("*\n");
        assert!(matches!(
            parse_feed(feed.as_bytes()),
            Err(SyncError::Feed(_))
        ));
    }
}
