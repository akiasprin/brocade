//! Bounded HTTP requests over reqwest/rustls, plus the shared WebSocket TLS transport.
//!
//! Each request has one deadline through DNS, TCP, TLS, upload and complete response body.
//! Retries belong to durable/latest-value reporters, never to the transport. Console traffic
//! and optional public services have separate clients and bounded resolver budgets.
//! Trust anchors are compiled into the Agent; host certificate stores and environment proxies
//! cannot change control-plane connectivity or expose the node credential.
use rustls::{pki_types::ServerName, ClientConfig, RootCertStore};
use std::{
    collections::BTreeMap,
    io::{self, Read},
    net::SocketAddr,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

pub(crate) fn diagnostic_family_client(
    family: AddressFamily,
) -> Result<reqwest::blocking::Client, String> {
    client::diagnostic_family(family)
}
pub(crate) fn diagnostic_socks_client(port: u16) -> Result<reqwest::blocking::Client, String> {
    client::socks(port)
}

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
    decode_gzip: bool,
}
impl Default for RequestTransport {
    fn default() -> Self {
        Self {
            family: None,
            timeout: HTTP_REQUEST_TIMEOUT,
            decode_gzip: false,
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
    diagnostic_proxy: Option<reqwest::blocking::Client>,
}
impl HttpClient {
    pub(crate) fn channel_origin(&self) -> String {
        format!(
            "{}://{}{}",
            if self.tls { "https" } else { "http" },
            self.host_header,
            self.prefix
        )
    }
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
            diagnostic_proxy: None,
        })
    }

    pub(crate) fn with_diagnostic_proxy(
        mut self,
        proxy: Option<&reqwest::blocking::Client>,
    ) -> Self {
        self.diagnostic_proxy = proxy.cloned();
        self
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
                ..RequestTransport::default()
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
                ..RequestTransport::default()
            },
        )
    }

    /// Browser-like public diagnostics retain the public DNS budget, disabled proxies and
    /// redirect policy. No Console credential is accepted at this boundary.
    pub(crate) fn request_public_diagnostic(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        content_type: &str,
        extra_headers: &[(&str, String)],
        timeout: Duration,
    ) -> Result<HttpResponse, String> {
        let mut headers = vec![
                ("Accept-Encoding", "gzip".into()),
                ("Accept", "text/html,application/json;q=0.9,*/*;q=0.8".into()),
                ("Accept-Language", "en-US,en;q=0.9".into()),
                ("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36".into()),
        ];
        headers.extend_from_slice(extra_headers);
        self.request_inner(
            method,
            path,
            None,
            RequestBody {
                bytes: body.unwrap_or("").as_bytes(),
                content_type,
            },
            &headers,
            RequestTransport {
                family: None,
                timeout,
                decode_gzip: true,
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
        if let Some(token) = token {
            if let Some(result) = crate::channel::request(
                &self.channel_origin(),
                token,
                method,
                path,
                body.bytes,
                extra_headers,
                deadline,
            ) {
                return result;
            }
        }
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
        let client = match (token, self.diagnostic_proxy.as_ref()) {
            (None, Some(proxy)) => Ok(proxy.clone()),
            _ => client::shared(class, transport.family, deadline),
        }
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
        let mut headers: BTreeMap<String, Vec<String>> = BTreeMap::new();
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
            headers
                .entry(name.as_str().to_owned())
                .or_default()
                .push(value.to_owned());
        }
        // Hyper owns framing. The application only supports the identity/chunked transfer coding;
        // do not accidentally accept a gzip transfer-coding as an uncompressed JSON response.
        if headers.get("transfer-encoding").is_some_and(|values| {
            values
                .iter()
                .any(|value| !value.trim().eq_ignore_ascii_case("chunked"))
        }) {
            return Err(progress.message("headers", "unsupported_transfer_encoding"));
        }
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            remaining(deadline).map_err(|_| progress.message("body", "timeout"))?;
            let available = (MAX_HTTP_RESPONSE_BYTES + 1 - bytes.len()).min(buffer.len());
            let length = response.read(&mut buffer[..available]).map_err(|error| {
                // Blocking body reads can wrap the expired timer without a typed
                // timeout source. The shared deadline is still authoritative.
                if Instant::now() >= deadline {
                    progress.message("body", "timeout")
                } else {
                    progress.error("body", &error)
                }
            })?;
            progress.received_bytes += length;
            if length == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..length]);
            if bytes.len() > MAX_HTTP_RESPONSE_BYTES {
                return Err(progress.message("body", "body_exceeds_limit"));
            }
        }
        let bytes = if transport.decode_gzip {
            decode_diagnostic_body(
                bytes,
                headers.get("content-encoding").map(Vec::as_slice),
                deadline,
                &progress,
            )?
        } else {
            bytes
        };
        let body =
            String::from_utf8(bytes).map_err(|_| progress.message("body", "invalid_utf8"))?;
        Ok(HttpResponse {
            status: response.status().as_u16(),
            body,
            headers,
        })
    }

    pub(crate) async fn connect_stream(&self, deadline: Instant) -> Result<Stream, String> {
        // This lookup runs only while the dedicated live-channel runtime is connecting. The
        // resolver owns the bounded worker set and absolute deadline, just as for HTTP requests.
        let addresses = resolver::control()
            .resolve_blocking(&self.host, self.port, deadline)
            .map_err(|error| format!("WebSocket DNS failed: {}", diagnostic::category(&error)))?;
        let count = u32::try_from(addresses.len()).map_err(|_| "too many resolved addresses")?;
        let mut last_error = None;
        for (index, address) in addresses.into_iter().enumerate() {
            let budget = remaining(deadline).map_err(|_| "WebSocket connect timeout")?;
            let left = count - u32::try_from(index).map_err(|_| "too many resolved addresses")?;
            let tcp = match tokio::time::timeout(
                (budget / left).min(HTTP_CONNECT_TIMEOUT),
                tokio::net::TcpStream::connect(address),
            )
            .await
            {
                Ok(Ok(tcp)) => tcp,
                Ok(Err(error)) => {
                    last_error = Some(diagnostic::category(&error));
                    continue;
                }
                Err(_) => {
                    last_error = Some("connect_timeout".to_owned());
                    continue;
                }
            };
            if !self.tls {
                return Ok(Stream::Plain(tcp));
            }
            let server_name =
                ServerName::try_from(self.host.clone()).map_err(|_| "invalid TLS server name")?;
            let connector = tokio_rustls::TlsConnector::from(tls_config()?);
            let tls = tokio::time::timeout_at(deadline.into(), connector.connect(server_name, tcp))
                .await
                .map_err(|_| "WebSocket TLS handshake timeout")?
                .map_err(|error| {
                    format!("WebSocket TLS failed: {}", diagnostic::category(&error))
                })?;
            return Ok(Stream::Tls(tls));
        }
        Err(format!(
            "WebSocket connect failed: {}",
            last_error.unwrap_or_else(|| "no_addresses".to_owned())
        ))
    }
}

/// Optional website probes negotiate gzip without changing control/binary HTTP semantics.
/// Framing must finish first; a truncated stream or invalid gzip trailer is never evidence.
fn decode_diagnostic_body(
    bytes: Vec<u8>,
    encodings: Option<&[String]>,
    deadline: Instant,
    progress: &diagnostic::Progress,
) -> Result<Vec<u8>, String> {
    if bytes.is_empty() || encodings.is_none() {
        return Ok(bytes);
    }
    match encodings {
        Some([encoding]) if encoding.trim().eq_ignore_ascii_case("identity") => return Ok(bytes),
        Some([encoding]) if encoding.trim().eq_ignore_ascii_case("gzip") => {}
        _ => return Err(progress.message("decode", "unsupported_content_encoding")),
    }
    let mut decoder = flate2::read::MultiGzDecoder::new(bytes.as_slice());
    let mut decoded = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        remaining(deadline).map_err(|_| progress.message("decode", "timeout"))?;
        let available = (MAX_HTTP_RESPONSE_BYTES + 1 - decoded.len()).min(buffer.len());
        let count = decoder
            .read(&mut buffer[..available])
            .map_err(|_| progress.message("decode", "invalid_gzip"))?;
        if count == 0 {
            break;
        }
        decoded.extend_from_slice(&buffer[..count]);
        if decoded.len() > MAX_HTTP_RESPONSE_BYTES {
            return Err(progress.message("decode", "decoded_body_exceeds_limit"));
        }
    }
    Ok(decoded)
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|value| !value.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "request deadline exceeded"))
}

pub(crate) type Stream = yawc::MaybeTlsStream<tokio::net::TcpStream>;

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
    pub(crate) headers: BTreeMap<String, Vec<String>>,
}

impl HttpResponse {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .and_then(|values| values.last())
            .map(String::as_str)
    }

    /// Set-Cookie cannot be comma-joined: Expires itself contains commas. Keep every value
    /// within the shared header budget while preserving the single-header accessor above.
    pub(crate) fn header_values(&self, name: &str) -> impl Iterator<Item = &str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .into_iter()
            .flatten()
            .map(String::as_str)
    }
}
