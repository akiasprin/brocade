//! HTTP client, hand-written rather than pulling in reqwest: the agent links
//! statically against musl and ships to other people's machines, and it needs
//! only GET/POST, one timeout, and one body limit.
//!
//! TLS is the one layer we do not write ourselves. This subset of HTTP/1.1 fits
//! on a screen; TLS does not. Hand-rolling it fails differently, too: not by
//! missing features, but by completing a handshake that never established who
//! the peer is. So this reaches for rustls (with the ring provider — the
//! reasoning and the measurements are at the top of `brocade-console/build.rs`,
//! in the section on zig).
//!
//! Trust anchors come from `webpki-roots`, compiled into the binary, rather than
//! from the target machine's `/etc/ssl/certs`. The agent runs on other people's
//! machines, and how old their root store is — or whether they have one at all,
//! which trimmed Alpine images often do not — must not decide whether the agent
//! can reach the control plane.
//!
//! One consequence worth stating: a node whose clock is badly wrong cannot reach
//! the control plane, because certificate validity is checked against system
//! time. That is deliberate and not worked around — working around it would mean
//! not checking validity at all.
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::{Arc, OnceLock},
    time::Duration,
};

use rustls::{pki_types::ServerName, ClientConfig, ClientConnection, RootCertStore, StreamOwned};

const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MAX_HTTP_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

const HTTP_DEFAULT_PORT: u16 = 80;
const HTTPS_DEFAULT_PORT: u16 = 443;

#[derive(Debug)]
pub(crate) struct HttpClient {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) host_header: String,
    pub(crate) prefix: String,
    /// Whether this connection runs over TLS. Decided by the URL scheme and by
    /// nothing else: there must be no state where the configuration claims
    /// encryption and the wire does not have it.
    pub(crate) tls: bool,
}

impl HttpClient {
    pub(crate) fn new(base_url: &str) -> Result<Self, String> {
        // Both prefixes are required; a missing scheme is not defaulted to http.
        // Defaulting turns a slip like `relay.example:8081` into host `relay.example` on
        // port 8081 over plaintext — the token goes out in the clear and nothing
        // about it looks wrong.
        let (tls, rest) = if let Some(rest) = base_url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = base_url.strip_prefix("http://") {
            (false, rest)
        } else {
            return Err(format!(
                "server URL must start with http:// or https://: {base_url}"
            ));
        };
        let default_port = if tls {
            HTTPS_DEFAULT_PORT
        } else {
            HTTP_DEFAULT_PORT
        };
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port, host_header) = parse_http_authority(base_url, authority, default_port)?;
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("/{}", path.trim_end_matches('/'))
        };
        Ok(Self {
            host,
            port,
            host_header,
            prefix,
            tls,
        })
    }

    pub(crate) fn request(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<&str>,
    ) -> Result<HttpResponse, String> {
        self.request_with_headers(method, path, token, body, &[])
    }

    pub(crate) fn request_with_headers(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<&str>,
        extra_headers: &[(&str, String)],
    ) -> Result<HttpResponse, String> {
        let full_path = format!("{}{}", self.prefix, path);
        let body = body.unwrap_or("");
        let mut extra = String::new();
        for (name, value) in extra_headers {
            extra.push_str(name);
            extra.push_str(": ");
            extra.push_str(value);
            extra.push_str("\r\n");
        }
        let tcp = self.connect()?;
        // Timeouts go on the TcpStream, and they go on before TLS wraps it: the
        // handshake is itself a round of reads and writes, so without them a peer
        // that never answers blocks forever. A stalled convergence loop is
        // invisible from the console — that node merely stops reporting.
        tcp.set_read_timeout(Some(HTTP_IO_TIMEOUT))
            .map_err(|error| error.to_string())?;
        tcp.set_write_timeout(Some(HTTP_IO_TIMEOUT))
            .map_err(|error| error.to_string())?;
        let mut stream = self.wrap_tls(tcp)?;
        let request = format!(
            "{method} {full_path} HTTP/1.1\r\n\
             Host: {host}\r\n\
             Authorization: Bearer {token}\r\n\
             User-Agent: brocade-agent/{identity}\r\n\
             X-Brocade-Protocol-Version: {protocol}\r\n\
             Accept: application/json\r\n\
             Content-Type: application/json\r\n\
             {extra}\
             Content-Length: {len}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}",
            host = self.host_header,
            // The sha256 of this very binary, not a version number somebody has to remember to
            // bump — the reasoning is at the top of `identity.rs`. It lands in
            // `node_agent_state.agent_version`, whose sole writer is the poll path.
            identity = crate::identity::self_identity(),
            protocol = brocade_deployment::protocol::AGENT_PROTOCOL_VERSION,
            len = body.len(),
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;

        let mut raw = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let read = match stream.read(&mut buffer) {
                Ok(read) => read,
                // rustls reports UnexpectedEof when the peer drops the connection
                // without sending close_notify, which is common under
                // `Connection: close` (on the plaintext path the kernel just
                // hands up an EOF and the two are indistinguishable). Treat it as
                // end of response: whether the response is complete is decided by
                // the parsing below, which rejects a missing `\r\n\r\n` and a
                // truncated chunked body — the same bar as the plaintext path.
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error.to_string()),
            };
            if read == 0 {
                break;
            }
            let new_len = raw
                .len()
                .checked_add(read)
                .ok_or("HTTP response length overflow")?;
            if new_len > MAX_HTTP_RESPONSE_BYTES {
                return Err(format!(
                    "HTTP response exceeds {MAX_HTTP_RESPONSE_BYTES} byte limit"
                ));
            }
            raw.extend_from_slice(&buffer[..read]);
        }
        parse_http_response(&raw)
    }

    pub(crate) fn connect(&self) -> Result<TcpStream, String> {
        let addresses = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|error| format!("resolve {}:{} failed: {error}", self.host, self.port))?;
        let mut last_error = None;
        for address in addresses {
            match TcpStream::connect_timeout(&address, HTTP_CONNECT_TIMEOUT) {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = Some(error),
            }
        }
        Err(match last_error {
            Some(error) => format!("connect {}:{} failed: {error}", self.host, self.port),
            None => format!("resolve {}:{} returned no addresses", self.host, self.port),
        })
    }

    /// Plaintext passes through untouched; https gets a rustls layer on top.
    pub(crate) fn wrap_tls(&self, tcp: TcpStream) -> Result<Stream, String> {
        if !self.tls {
            return Ok(Stream::Plain(tcp));
        }
        let config = tls_config()?;
        // The name being verified comes from the URL, not from the address we
        // connected to. Verifying against the resolved IP when the URL names a
        // host would void the name field of the certificate entirely.
        let server_name = ServerName::try_from(self.host.clone())
            .map_err(|_| format!("{} is not a valid TLS server name", self.host))?;
        let connection = ClientConnection::new(config, server_name)
            .map_err(|error| format!("TLS setup for {} failed: {error}", self.host))?;
        Ok(Stream::Tls(Box::new(StreamOwned::new(connection, tcp))))
    }
}

/// The two paths a request can take: plaintext or TLS.
///
/// An enum rather than `Box<dyn ...>`, because there are exactly two of them and
/// a trait object would first need a hand-written trait that is both `Read` and
/// `Write`, in exchange for one more indirection on every read and write.
///
/// The TLS variant is boxed because `ClientConnection` carries its record
/// buffers and is large. Unboxed, the whole enum would be sized for it, and every
/// plaintext request would pay for that much stack it never uses.
pub(crate) enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Stream {
    pub(crate) fn set_nonblocking(&self, nonblocking: bool) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_nonblocking(nonblocking),
            Self::Tls(stream) => stream.sock.set_nonblocking(nonblocking),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(stream) => stream.read(buffer),
            Stream::Tls(stream) => stream.read(buffer),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(stream) => stream.write(buffer),
            Stream::Tls(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(stream) => stream.flush(),
            Stream::Tls(stream) => stream.flush(),
        }
    }
}

/// The TLS client configuration, built once per process.
///
/// Rebuilding it per request would re-parse all ~150 trust anchors every 15
/// seconds (the APPLY interval), burning node CPU for nothing.
///
/// The failure is cached along with the success: everything that could make this
/// fail is fixed at compile time — both the provider and the trust anchors live
/// in the binary — so a second attempt returns the same error as the first.
fn tls_config() -> Result<Arc<ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            // The provider is named explicitly instead of going through
            // `ClientConfig::builder()`, which reads the process-wide default.
            // rustls is built with default-features off (see Cargo.toml), so
            // nothing installs that default and this would panic at runtime.
            let config = ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map_err(|error| format!("TLS provider setup failed: {error}"))?
            .with_root_certificates(roots)
            .with_no_client_auth();
            Ok(Arc::new(config))
        })
        .clone()
}

fn parse_http_authority(
    base_url: &str,
    authority: &str,
    default_port: u16,
) -> Result<(String, u16, String), String> {
    if authority.is_empty() {
        return Err(format!("invalid server URL {base_url}"));
    }
    if authority.starts_with('[') {
        let end = authority
            .find(']')
            .ok_or_else(|| format!("invalid bracketed IPv6 address in {base_url}"))?;
        let host = &authority[1..end];
        if host.is_empty() {
            return Err(format!("invalid server URL {base_url}"));
        }
        let rest = &authority[end + 1..];
        let port = if rest.is_empty() {
            default_port
        } else {
            parse_http_port(
                base_url,
                rest.strip_prefix(':')
                    .ok_or_else(|| format!("invalid server URL {base_url}"))?,
            )?
        };
        let bracketed_host = format!("[{host}]");
        let host_header = host_header(&bracketed_host, port, default_port);
        return Ok((host.to_owned(), port, host_header));
    }
    if authority.contains(']') {
        return Err(format!("invalid server URL {base_url}"));
    }

    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host, parse_http_port(base_url, port)?),
        Some(_) => {
            return Err(
                "IPv6 server URLs must use brackets, for example http://[::1]:8081".to_owned(),
            )
        }
        None => (authority, default_port),
    };
    if host.is_empty() {
        return Err(format!("invalid server URL {base_url}"));
    }
    Ok((host.to_owned(), port, host_header(host, port, default_port)))
}

fn parse_http_port(base_url: &str, port: &str) -> Result<u16, String> {
    if port.is_empty() {
        return Err(format!("invalid port in {base_url}"));
    }
    port.parse::<u16>()
        .map_err(|_| format!("invalid port in {base_url}"))
}

/// The Host header carries no port when the port is the default for the scheme.
///
/// That default moves with the scheme — 80 for http, 443 for https — so it
/// cannot be hard-coded to 80 here. Hard-coded, `https://host` would send
/// `Host: host:443`, a name-based reverse proxy on the far side would find no
/// matching site, and the agent would connect fine and get 404 on everything.
pub(crate) fn host_header(host: &str, port: u16, default_port: u16) -> String {
    if port == default_port {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    }
}

#[derive(Debug)]
pub(crate) struct HttpResponse {
    pub(crate) status: u16,
    pub(crate) body: String,
    headers: BTreeMap<String, String>,
}

impl HttpResponse {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

pub(crate) fn parse_http_response(raw: &[u8]) -> Result<HttpResponse, String> {
    if raw.len() > MAX_HTTP_RESPONSE_BYTES {
        return Err(format!(
            "HTTP response exceeds {MAX_HTTP_RESPONSE_BYTES} byte limit"
        ));
    }
    let header_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("HTTP response missing header terminator")?;
    let head = String::from_utf8_lossy(&raw[..header_end]);
    let mut body = raw[header_end + 4..].to_vec();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or("HTTP response missing status")?
        .parse::<u16>()
        .map_err(|_| "HTTP response has invalid status".to_owned())?;
    let headers = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let transfer_encoding = headers
        .get("transfer-encoding")
        .map(|value| value.to_ascii_lowercase());
    if transfer_encoding
        .as_deref()
        .is_some_and(|value| value.split(',').any(|part| part.trim() == "chunked"))
    {
        body = decode_chunked_body(&body)?;
    }
    Ok(HttpResponse {
        status,
        body: String::from_utf8(body).map_err(|error| error.to_string())?,
        headers,
    })
}

pub(crate) fn decode_chunked_body(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoded = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or("chunked response missing chunk size")?;
        let size_line =
            std::str::from_utf8(&body[..line_end]).map_err(|error| error.to_string())?;
        let size_text = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| "chunked response has invalid chunk size".to_owned())?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(decoded);
        }
        let new_len = decoded
            .len()
            .checked_add(size)
            .ok_or("chunked response body length overflow")?;
        if new_len > MAX_HTTP_RESPONSE_BYTES {
            return Err(format!(
                "chunked response body exceeds {MAX_HTTP_RESPONSE_BYTES} byte limit"
            ));
        }
        let chunk_with_crlf = size
            .checked_add(2)
            .ok_or("chunked response chunk size overflow")?;
        if body.len() < chunk_with_crlf {
            return Err("chunked response ended inside a chunk".to_owned());
        }
        decoded.extend_from_slice(&body[..size]);
        if &body[size..size + 2] != b"\r\n" {
            return Err("chunked response chunk missing trailing CRLF".to_owned());
        }
        body = &body[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_chunked_body, parse_http_response, HttpClient};

    #[test]
    fn response_headers_are_case_insensitive() {
        let response = parse_http_response(
            b"HTTP/1.1 204 No Content\r\nX-Brocade-Log-Max-MiB: 256\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(response.header("x-brocade-log-max-mib"), Some("256"));
        assert_eq!(response.header("X-BROCADE-LOG-MAX-MIB"), Some("256"));
    }

    /// Without an explicit port the Host header must not carry one either. With
    /// it, a name-based reverse proxy on the far side finds no matching site and
    /// the agent connects fine but gets a 404.
    #[test]
    fn the_default_port_is_left_out_of_the_host_header() {
        let client = HttpClient::new("http://example.test").unwrap();
        assert_eq!(client.port, 80);
        assert_eq!(client.host_header, "example.test");
        assert_eq!(client.prefix, "", "没有路径就没有前缀");

        let client = HttpClient::new("http://[::1]").unwrap();
        assert_eq!(client.host, "::1");
        assert_eq!(client.port, 80);
        assert_eq!(client.host_header, "[::1]");
    }

    /// A trailing slash on the path prefix must go. Kept, the joined path reads
    /// `/base//agent/v1/desired`, which some reverse proxies route elsewhere.
    #[test]
    fn a_trailing_slash_in_the_prefix_is_trimmed() {
        assert_eq!(HttpClient::new("http://h/base/").unwrap().prefix, "/base");
        assert_eq!(HttpClient::new("http://h/base").unwrap().prefix, "/base");
        assert_eq!(HttpClient::new("http://h/").unwrap().prefix, "");
    }

    /// A broken port must be reported on the spot. Falling back to 80 sends the
    /// agent at a port nobody listens on, and the error reads "cannot connect" —
    /// which sends the operator to the network, not to their own URL.
    #[test]
    fn a_broken_port_is_reported_instead_of_falling_back_to_80() {
        for url in [
            "http://example.test:",
            "http://example.test:http",
            "http://example.test:70000",
            "http://[::1]:70000",
        ] {
            let error = HttpClient::new(url).unwrap_err();
            assert!(
                error.contains("invalid port"),
                "{url} 该报端口无效，实际 {error}"
            );
        }
    }

    /// https means TLS, port 443, and no 443 in the Host header. A name-based
    /// reverse proxy matches on that header; carrying the port finds no site,
    /// and the symptom is a connection that works and 404s on everything.
    #[test]
    fn https_urls_get_tls_and_port_443() {
        let client = HttpClient::new("https://example.test").unwrap();
        assert!(client.tls);
        assert_eq!(client.port, 443);
        assert_eq!(client.host_header, "example.test");

        let client = HttpClient::new("https://example.test:8443").unwrap();
        assert!(client.tls);
        assert_eq!(client.port, 8443);
        assert_eq!(client.host_header, "example.test:8443", "非默认端口要带上");

        let client = HttpClient::new("http://example.test").unwrap();
        assert!(!client.tls, "http 不该被悄悄升级成 TLS");
        assert_eq!(client.port, 80);

        // 443 is not the default under http, and 80 is not the default under
        // https, so both must keep their port in the header.
        assert_eq!(
            HttpClient::new("http://example.test:443")
                .unwrap()
                .host_header,
            "example.test:443"
        );
        assert_eq!(
            HttpClient::new("https://example.test:80")
                .unwrap()
                .host_header,
            "example.test:80"
        );
    }

    /// A URL with no scheme is an error, not something to default to http.
    /// Defaulted, a slip like `relay.example:8081` parses as host `relay.example` on port
    /// 8081 over plaintext: the token goes out in the clear and everything about
    /// it looks normal.
    #[test]
    fn a_url_without_a_scheme_is_rejected() {
        for url in [
            "relay.example:8081",
            "example.test",
            "ftp://example.test",
            "",
        ] {
            let error = HttpClient::new(url).unwrap_err();
            assert!(
                error.contains("must start with http:// or https://"),
                "{url} 实际 {error}"
            );
        }
    }

    /// The TLS configuration builds, and twice asking yields the same one.
    /// Rebuilt per request, all ~150 trust anchors would be re-parsed every 15
    /// seconds for nothing.
    #[test]
    fn the_tls_config_is_built_once_and_reused() {
        let first = super::tls_config().expect("信任锚和 provider 都在二进制里，不该建不起来");
        let second = super::tls_config().unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &second), "该是同一份");
    }

    /// A URL with no host must not pass. An empty host survives all the way to
    /// connect, where the error talks about resolution failure — several layers
    /// away from "your URL is missing a host".
    #[test]
    fn an_empty_host_is_rejected_early() {
        for url in ["http://", "http://:8080", "http://[]:8080", "http://]bad"] {
            assert!(HttpClient::new(url).is_err(), "{url} 不该被接受");
        }
    }

    /// An unclosed bracket is an error too. Taking everything before `]` as the
    /// host would connect somewhere whose name does not match.
    #[test]
    fn an_unclosed_bracket_is_rejected() {
        let error = HttpClient::new("http://[::1:8080").unwrap_err();
        assert!(error.contains("bracketed IPv6"), "实际 {error}");
    }

    /// A chunked response cut short is an error; the part already received must
    /// not pass as a complete body. Truncated JSON may well parse into a valid
    /// object with less in it, which is far worse than a parse failure.
    #[test]
    fn a_truncated_chunked_body_is_an_error_not_a_short_read() {
        let error = decode_chunked_body(b"5\r\nhel").unwrap_err();
        assert!(error.contains("ended inside a chunk"), "实际 {error}");

        let error = decode_chunked_body(b"5\r\nhelloXX").unwrap_err();
        assert!(error.contains("missing trailing CRLF"), "实际 {error}");

        let error = decode_chunked_body(b"5").unwrap_err();
        assert!(error.contains("missing chunk size"), "实际 {error}");

        let error = decode_chunked_body(b"zz\r\n").unwrap_err();
        assert!(error.contains("invalid chunk size"), "实际 {error}");
    }

    /// A chunk-size line with an extension (`5;foo=bar`) is legal; everything
    /// after the semicolon is ignored.
    #[test]
    fn a_chunk_extension_is_ignored() {
        let decoded = decode_chunked_body(b"5;name=value\r\nhello\r\n0\r\n\r\n").unwrap();
        assert_eq!(decoded, b"hello");
    }

    /// A response missing the `\r\n\r\n` terminator, or with a non-numeric status
    /// line, is an error. Guessing 200 would hand the layer above a piece of
    /// garbage to parse as the control plane's reply.
    #[test]
    fn a_malformed_status_line_is_rejected() {
        let error = parse_http_response(b"HTTP/1.1 200 OK\r\nno terminator").unwrap_err();
        assert!(error.contains("missing header terminator"), "实际 {error}");

        let error = parse_http_response(b"HTTP/1.1 zzz OK\r\n\r\n").unwrap_err();
        assert!(error.contains("invalid status"), "实际 {error}");

        let error = parse_http_response(b"HTTP/1.1\r\n\r\n").unwrap_err();
        assert!(error.contains("missing status"), "实际 {error}");
    }

    /// Case and spacing in Transfer-Encoding are the far side's choice, so one
    /// spelling is not enough. Missing it hands up the raw bytes, chunk-length
    /// prefixes and all, as the body.
    #[test]
    fn chunked_is_recognized_regardless_of_spelling() {
        for head in [
            "Transfer-Encoding: chunked",
            "transfer-encoding: Chunked",
            "Transfer-Encoding:  gzip, chunked ",
        ] {
            let raw = format!("HTTP/1.1 200 OK\r\n{head}\r\n\r\n5\r\nhello\r\n0\r\n\r\n");
            let response = parse_http_response(raw.as_bytes()).unwrap();
            assert_eq!(response.body, "hello", "{head} 该被认成分块");
        }
    }
}
