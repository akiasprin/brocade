//! Bounded HTTP requests over reqwest/rustls, plus the shared WebSocket TLS transport.
//!
//! Each request has one deadline through DNS, TCP, TLS, upload and complete response body.
//! Retries belong to durable/latest-value reporters, never to the transport. Console traffic
//! and optional public services have separate clients and bounded resolver budgets.
//! Trust anchors are compiled into the Agent; host certificate stores and environment proxies
//! cannot change control-plane connectivity or expose the node credential.
use rustls::{pki_types::ServerName, ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    os::fd::AsRawFd,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

mod client;
mod diagnostic;
mod resolver;
#[cfg(test)]
pub(crate) mod tests;

const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MAX_HTTP_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
const HTTP_DEFAULT_PORT: u16 = 80;
const HTTPS_DEFAULT_PORT: u16 = 443;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddressFamily {
    V4,
    V6,
}
impl AddressFamily {
    fn accepts(self, address: &SocketAddr) -> bool {
        match self {
            Self::V4 => address.is_ipv4(),
            Self::V6 => address.is_ipv6(),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::V4 => "IPv4",
            Self::V6 => "IPv6",
        }
    }
}
struct RequestBody<'a> {
    bytes: &'a [u8],
    content_type: &'a str,
}
#[derive(Clone, Copy)]
struct RequestTransport {
    family: Option<AddressFamily>,
    timeout: Duration,
}
impl Default for RequestTransport {
    fn default() -> Self {
        Self {
            family: None,
            timeout: HTTP_REQUEST_TIMEOUT,
        }
    }
}
#[derive(Debug)]
pub(crate) struct HttpClient {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) host_header: String,
    pub(crate) prefix: String,
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
            return Err("server URL must start with http:// or https://".to_owned());
        };
        if rest.chars().any(|c| c.is_control() || c.is_whitespace()) || rest.contains('#') {
            return Err(
                "invalid server URL: credentials, fragments and whitespace are forbidden"
                    .to_owned(),
            );
        }
        let default_port = if tls {
            HTTPS_DEFAULT_PORT
        } else {
            HTTP_DEFAULT_PORT
        };
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        if authority.contains('@') {
            return Err("server URL credentials are forbidden".to_owned());
        }
        let (host, port, host_header) = parse_http_authority(authority, default_port)?;
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
        self.request_inner(
            method,
            path,
            Some(token),
            RequestBody {
                bytes: body.unwrap_or("").as_bytes(),
                content_type: "application/json",
            },
            extra_headers,
            RequestTransport::default(),
        )
    }

    /// Send an unauthenticated request through one address family. The configured Console token
    /// is deliberately not accepted by this API because the destination may be an operator-set
    /// public trace service rather than the Console origin.
    pub(crate) fn request_public_on_family(
        &self,
        family: AddressFamily,
        method: &str,
        path: &str,
        timeout: Duration,
    ) -> Result<HttpResponse, String> {
        self.request_inner(
            method,
            path,
            None,
            RequestBody {
                bytes: &[],
                content_type: "application/json",
            },
            &[],
            RequestTransport {
                family: Some(family),
                timeout,
            },
        )
    }

    pub(crate) fn request_bytes_with_headers(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: &[u8],
        content_type: &str,
        extra_headers: &[(&str, String)],
    ) -> Result<HttpResponse, String> {
        self.request_inner(
            method,
            path,
            Some(token),
            RequestBody {
                bytes: body,
                content_type,
            },
            extra_headers,
            RequestTransport::default(),
        )
    }

    /// Send an unauthenticated JSON request to a public service. Keeping this separate from
    /// `request` is a security boundary: the node bearer token and Brocade protocol headers must
    /// never leave the configured Console origin.
    pub(crate) fn request_public(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<HttpResponse, String> {
        self.request_inner(
            method,
            path,
            None,
            RequestBody {
                bytes: body.unwrap_or("").as_bytes(),
                content_type: "application/json",
            },
            &[],
            RequestTransport::default(),
        )
    }

    pub(crate) fn request_timeout(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<&str>,
        timeout: Duration,
    ) -> Result<HttpResponse, String> {
        self.request_inner(
            method,
            path,
            Some(token),
            RequestBody {
                bytes: body.unwrap_or("").as_bytes(),
                content_type: "application/json",
            },
            &[],
            RequestTransport {
                family: None,
                timeout,
            },
        )
    }

    fn request_inner(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: RequestBody<'_>,
        extra_headers: &[(&str, String)],
        transport: RequestTransport,
    ) -> Result<HttpResponse, String> {
        let started = Instant::now();
        let mut progress = diagnostic::Progress::new(started);
        let deadline = started
            .checked_add(transport.timeout)
            .ok_or("invalid HTTP request timeout")?;
        if let (Some(family), Ok(ip)) = (transport.family, self.host.parse::<std::net::IpAddr>()) {
            if !family.accepts(&SocketAddr::new(ip, self.port)) {
                return Err(format!("resolve returned no {} addresses", family.label()));
            }
        }
        let class = if token.is_some() {
            client::TrafficClass::Control
        } else {
            client::TrafficClass::Public
        };
        let client = client::shared(class, transport.family, deadline)
            .map_err(|error| progress.message("client", &error))?;
        let url = format!(
            "{}://{}{}{}",
            if self.tls { "https" } else { "http" },
            self.host_header,
            self.prefix,
            path
        );
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| progress.message("request", "invalid_method"))?;
        let mut request = client
            .request(method, url)
            .header("Accept", "application/json")
            .header("Content-Type", body.content_type)
            .header("Content-Length", body.bytes.len())
            .body(body.bytes.to_vec());
        if let Some(token) = token {
            request = request.bearer_auth(token).header(
                "X-Brocade-Protocol-Version",
                brocade_deployment::protocol::AGENT_PROTOCOL_VERSION,
            );
        }
        for (name, value) in extra_headers {
            request = request.header(*name, value);
        }
        // Client creation and copying the bounded application payload spend the same budget.
        // Setting only ClientBuilder::timeout does not carry a total timer through blocking reads.
        let request_budget =
            remaining(deadline).map_err(|_| progress.message("request", "timeout"))?;
        let mut response = request
            .timeout(request_budget)
            .send()
            .map_err(|error| progress.error("send", &error))?;
        progress.status = Some(response.status().as_u16());
        progress.content_length = response.content_length();
        if response.status() == reqwest::StatusCode::SWITCHING_PROTOCOLS {
            return Err(progress.message("headers", "unsupported_upgrade"));
        }
        if progress
            .content_length
            .is_some_and(|size| size > MAX_HTTP_RESPONSE_BYTES as u64)
        {
            return Err(progress.message("headers", "body_exceeds_limit"));
        }
        let mut headers = BTreeMap::new();
        let mut header_bytes = 0usize;
        for (name, value) in response.headers() {
            header_bytes = header_bytes
                .saturating_add(name.as_str().len())
                .saturating_add(value.len())
                .saturating_add(4);
            if header_bytes > MAX_HTTP_HEADER_BYTES {
                return Err(progress.message("headers", "headers_exceed_limit"));
            }
            let value = std::str::from_utf8(value.as_bytes())
                .map_err(|_| progress.message("headers", "invalid_header_encoding"))?;
            headers.insert(name.as_str().to_owned(), value.to_owned());
        }
        // Hyper owns framing. The application only supports the identity/chunked transfer coding;
        // do not accidentally accept a gzip transfer-coding as an uncompressed JSON response.
        if headers
            .get("transfer-encoding")
            .is_some_and(|value| !value.trim().eq_ignore_ascii_case("chunked"))
        {
            return Err(progress.message("headers", "unsupported_transfer_encoding"));
        }
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            remaining(deadline).map_err(|_| progress.message("body", "timeout"))?;
            let available = (MAX_HTTP_RESPONSE_BYTES + 1 - bytes.len()).min(buffer.len());
            let length = response
                .read(&mut buffer[..available])
                .map_err(|error| progress.error("body", &error))?;
            progress.received_bytes += length;
            if length == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..length]);
            if bytes.len() > MAX_HTTP_RESPONSE_BYTES {
                return Err(progress.message("body", "body_exceeds_limit"));
            }
        }
        let body =
            String::from_utf8(bytes).map_err(|_| progress.message("body", "invalid_utf8"))?;
        Ok(HttpResponse {
            status: response.status().as_u16(),
            body,
            headers,
        })
    }

    pub(crate) fn connect_stream(&self, deadline: Instant) -> Result<Stream, String> {
        let addresses = resolver::control()
            .resolve_blocking(&self.host, self.port, deadline)
            .map_err(|error| format!("WebSocket DNS failed: {}", diagnostic::category(&error)))?;
        let count = u32::try_from(addresses.len()).map_err(|_| "too many resolved addresses")?;
        let mut last_error = None;
        for (index, address) in addresses.into_iter().enumerate() {
            let budget = remaining(deadline).map_err(|_| "WebSocket connect timeout")?;
            let left = count - u32::try_from(index).map_err(|_| "too many resolved addresses")?;
            match TcpStream::connect_timeout(&address, (budget / left).min(HTTP_CONNECT_TIMEOUT)) {
                Ok(tcp) => {
                    return self.wrap_tls(DeadlineTcpStream {
                        tcp,
                        deadline: Some(deadline),
                    })
                }
                Err(error) => last_error = Some(diagnostic::category(&error)),
            }
        }
        Err(format!(
            "WebSocket connect failed: {}",
            last_error.unwrap_or_else(|| "no_addresses".to_owned())
        ))
    }

    fn wrap_tls(&self, tcp: DeadlineTcpStream) -> Result<Stream, String> {
        if !self.tls {
            return Ok(Stream::Plain(tcp));
        }
        let server_name =
            ServerName::try_from(self.host.clone()).map_err(|_| "invalid TLS server name")?;
        let connection = ClientConnection::new(tls_config()?, server_name)
            .map_err(|_| "TLS client setup failed")?;
        Ok(Stream::Tls(Box::new(StreamOwned::new(connection, tcp))))
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|value| !value.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "request deadline exceeded"))
}

/// Rustls may consume many TLS records inside one Read. Enforce the absolute handshake deadline
/// at the underlying socket on every operation so control messages cannot renew its budget.
pub(crate) struct DeadlineTcpStream {
    tcp: TcpStream,
    deadline: Option<Instant>,
}
impl DeadlineTcpStream {
    fn prepare_read(&self) -> io::Result<()> {
        if let Some(deadline) = self.deadline {
            self.tcp.set_read_timeout(Some(remaining(deadline)?))?;
        }
        Ok(())
    }
    fn prepare_write(&self) -> io::Result<()> {
        if let Some(deadline) = self.deadline {
            self.tcp.set_write_timeout(Some(remaining(deadline)?))?;
        }
        Ok(())
    }
}
impl Read for DeadlineTcpStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.prepare_read()?;
        self.tcp.read(bytes)
    }
}
impl Write for DeadlineTcpStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.prepare_write()?;
        self.tcp.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.prepare_write()?;
        self.tcp.flush()
    }
}
pub(crate) enum Stream {
    Plain(DeadlineTcpStream),
    Tls(Box<StreamOwned<ClientConnection, DeadlineTcpStream>>),
}
impl Stream {
    /// Wait on the socket without changing the nonblocking transport or poisoning tungstenite
    /// with a timeout error. Callers drain buffered frames before waiting for kernel readiness.
    pub(crate) fn wait_for_io(&self, writable: bool, timeout: Option<Duration>) -> io::Result<()> {
        let tcp = match self {
            Self::Plain(stream) => &stream.tcp,
            Self::Tls(stream) => &stream.sock.tcp,
        };
        let mut descriptor = libc::pollfd {
            fd: tcp.as_raw_fd(),
            events: libc::POLLIN | if writable { libc::POLLOUT } else { 0 },
            revents: 0,
        };
        let millis = timeout.map_or(-1, |duration| {
            let rounded =
                duration.as_millis() + u128::from(duration.subsec_nanos() % 1_000_000 != 0);
            rounded.min(i32::MAX as u128) as i32
        });
        // SAFETY: descriptor references one live socket owned by self for the whole call.
        let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        if descriptor.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        Ok(())
    }

    pub(crate) fn finish_handshake(&mut self) -> io::Result<()> {
        let stream = match self {
            Self::Plain(stream) => stream,
            Self::Tls(stream) => &mut stream.sock,
        };
        if let Some(deadline) = stream.deadline {
            remaining(deadline)?;
        }
        stream.tcp.set_read_timeout(None)?;
        stream.tcp.set_write_timeout(None)?;
        stream.tcp.set_nonblocking(true)?;
        stream.deadline = None;
        Ok(())
    }
}
impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(bytes),
            Self::Tls(stream) => stream.read(bytes),
        }
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(bytes),
            Self::Tls(stream) => stream.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
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
pub(super) fn tls_config() -> Result<Arc<ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            #[cfg(test)]
            roots
                .add(rustls::pki_types::CertificateDer::from(
                    include_bytes!("http/testdata/ca.der").to_vec(),
                ))
                .expect("the checked-in loopback test CA is valid");
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
    authority: &str,
    default_port: u16,
) -> Result<(String, u16, String), String> {
    if authority.ends_with(':') {
        return Err("invalid port".to_owned());
    }
    if authority.starts_with('[') && !authority.contains(']') {
        return Err("invalid bracketed IPv6 address".to_owned());
    }
    if !authority.starts_with('[') && authority.matches(':').count() > 1 {
        return Err("IPv6 server URLs must use brackets, for example http://[::1]:8081".to_owned());
    }
    let scheme = if default_port == HTTPS_DEFAULT_PORT {
        "https"
    } else {
        "http"
    };
    let url = reqwest::Url::parse(&format!("{scheme}://{authority}"))
        .map_err(|_| "invalid server URL or invalid port".to_owned())?;
    let host = url
        .host_str()
        .ok_or("invalid server URL: missing host")?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = url.port().unwrap_or(default_port);
    let authority_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    Ok((
        host.to_owned(),
        port,
        host_header(&authority_host, port, default_port),
    ))
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
