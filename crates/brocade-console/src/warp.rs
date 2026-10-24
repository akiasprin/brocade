//! Cloudflare WARP device registration and MASQUE key enrollment.
//!
//! This is deliberately a small adapter rather than a Cloudflare account model. The endpoint is
//! undocumented and wgcf is unaffiliated with Cloudflare, so every assumption is kept in this
//! file and failures are returned with enough provider detail to diagnose a changed response.
//! A bootstrap device is enrolled with a fresh P-256 identity before it reaches the store.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rcgen::PublicKeyData as _;
use reqwest::{header, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const API_ORIGIN: &str = "https://api.cloudflareclient.com";
// The bootstrap and MASQUE enrollment requests use the same Android API version.
const API_VERSION: &str = "v0a4616";
const CLIENT_VERSION: &str = "a-6.81-4616";
const RESPONSE_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub device_id: String,
    pub account_id: String,
    pub access_token: String,
    pub peer_public_key: String,
    pub local_addresses: Vec<String>,
    pub suggested_endpoint: Option<String>,
}

#[derive(Debug, Serialize)]
struct RegisterRequest<'a> {
    fcm_token: &'a str,
    install_id: &'a str,
    key: &'a str,
    key_type: &'a str,
    tunnel_type: &'a str,
    locale: &'a str,
    model: &'a str,
    tos: &'a str,
    #[serde(rename = "type")]
    device_type: &'a str,
}

#[derive(Debug, Deserialize)]
struct RegisterResponse {
    id: String,
    token: String,
    account: Account,
    config: Config,
}

#[derive(Debug, Deserialize)]
struct Account {
    id: String,
}

#[derive(Debug, Deserialize)]
struct Config {
    interface: Interface,
    peers: Vec<Peer>,
}

#[derive(Debug, Deserialize)]
struct Interface {
    addresses: Addresses,
}

#[derive(Debug, Deserialize)]
struct Addresses {
    v4: String,
    v6: String,
}

#[derive(Debug, Deserialize)]
struct Peer {
    public_key: String,
    endpoint: Endpoint,
}

#[derive(Debug, Deserialize)]
struct Endpoint {
    #[serde(default)]
    host: String,
    #[serde(default)]
    v4: String,
    #[serde(default)]
    v6: String,
}

pub async fn register(public_key: &str, node_name: &str) -> Result<Registration, String> {
    register_masque_at(API_ORIGIN, public_key, node_name).await
}

pub struct MasqueKeys {
    pub private_key: String,
    pub public_key: String,
}

pub fn generate_masque_keys() -> Result<MasqueKeys, String> {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .map_err(|_| "无法生成 WARP P-256 设备密钥".to_owned())?;
    Ok(MasqueKeys {
        private_key: BASE64_STANDARD.encode(key.serialize_der()),
        public_key: BASE64_STANDARD.encode(key.subject_public_key_info()),
    })
}

async fn register_masque_at(
    origin: &str,
    public_key: &str,
    node_name: &str,
) -> Result<Registration, String> {
    // The provider bootstraps devices through its legacy registration endpoint, then enrolls
    // the actual MASQUE key. This throwaway key is never used by the data plane or persisted.
    use ring::rand::SecureRandom as _;
    let mut bootstrap = [0_u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bootstrap)
        .map_err(|_| "无法生成 WARP 注册随机值".to_owned())?;
    let registered = register_at(origin, &BASE64_STANDARD.encode(bootstrap), node_name).await?;
    match enroll_masque(origin, &registered, public_key).await {
        Ok(value) => Ok(value),
        Err(error) => {
            if let Err(cleanup) =
                unregister_at(origin, &registered.device_id, &registered.access_token).await
            {
                return Err(format!(
                    "{error}；临时 WARP 设备 {} 清理失败：{cleanup}",
                    registered.device_id
                ));
            }
            Err(error)
        }
    }
}

async fn enroll_masque(
    origin: &str,
    registered: &Registration,
    public_key: &str,
) -> Result<Registration, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(25))
        .http1_only()
        .build()
        .map_err(|_| "无法建立 MASQUE 注册客户端".to_owned())?;
    let mut url = reqwest::Url::parse(origin).map_err(|_| "WARP API 地址无效".to_owned())?;
    url.path_segments_mut()
        .map_err(|_| "WARP API 地址无效".to_owned())?
        .push(API_VERSION)
        .push("reg")
        .push(&registered.device_id);
    let response = client.patch(url).header(header::USER_AGENT, "WARP for Android")
        .header("CF-Client-Version", CLIENT_VERSION).bearer_auth(&registered.access_token)
        .json(&serde_json::json!({"key": public_key, "key_type": "secp256r1", "tunnel_type": "masque", "warp_enabled": true}))
        .send().await.map_err(|_| "Cloudflare MASQUE 密钥注册请求失败".to_owned())?;
    let status = response.status();
    let bytes = response_bytes(response).await?;
    if status != StatusCode::OK {
        return Err(format!(
            "Cloudflare MASQUE 注册失败（HTTP {}）",
            status.as_u16()
        ));
    }
    let mut value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "Cloudflare MASQUE 响应不是有效 JSON".to_owned())?;
    if value.get("warp_enabled").and_then(Value::as_bool) == Some(false) {
        return Err("Cloudflare 未启用 WARP".to_owned());
    }
    // PATCH responses may omit the creation-only bearer token.
    value["token"] = Value::String(registered.access_token.clone());
    let mut result = parse_registration(
        &serde_json::to_vec(&value).map_err(|_| "无法解析 MASQUE 身份".to_owned())?,
    )?;
    if result.device_id != registered.device_id || result.account_id != registered.account_id {
        return Err("Cloudflare MASQUE 返回了不匹配的设备身份".to_owned());
    }
    result.peer_public_key = normalize_masque_public_key(&result.peer_public_key)?;
    Ok(result)
}

fn normalize_masque_public_key(key: &str) -> Result<String, String> {
    // Cloudflare returns a PEM public key; persist the same canonical Base64 DER that the
    // compiler validates and the Xray renderer consumes.
    let key = key.trim();
    let der = if key.starts_with("-----BEGIN ") {
        let (remaining, pem) = x509_parser::pem::parse_x509_pem(key.as_bytes())
            .map_err(|_| "MASQUE 服务端公钥不是有效 PEM".to_owned())?;
        if pem.label != "PUBLIC KEY" || !remaining.iter().all(u8::is_ascii_whitespace) {
            return Err("MASQUE 服务端公钥不是单个 PUBLIC KEY".to_owned());
        }
        pem.contents
    } else {
        BASE64_STANDARD
            .decode(key)
            .map_err(|_| "MASQUE 服务端公钥不是 PEM 或 Base64 DER".to_owned())?
    };
    const PREFIX: &[u8] = &[
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
        0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00, 0x04,
    ];
    if der.len() != 91 || !der.starts_with(PREFIX) {
        return Err("MASQUE 服务端未返回 P-256 公钥".to_owned());
    }
    Ok(BASE64_STANDARD.encode(der))
}

async fn response_bytes(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "无法读取 WARP 响应".to_owned())?
    {
        if bytes.len() + chunk.len() > RESPONSE_LIMIT {
            return Err("WARP 响应超过 64 KiB".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Retires one source registration with the device-scoped token returned at creation time.
///
/// Provider deletion precedes the database delete. Treating 404 as success makes the operation
/// repairable if Cloudflare accepted the first request but the local commit failed: retrying can
/// still clear the dead local row instead of getting stuck behind an already-gone device.
pub async fn unregister(device_id: &str, access_token: &str) -> Result<(), String> {
    unregister_at(API_ORIGIN, device_id, access_token).await
}

async fn unregister_at(
    api_origin: &str,
    device_id: &str,
    access_token: &str,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(25))
        .min_tls_version(reqwest::tls::Version::TLS_1_2)
        .max_tls_version(reqwest::tls::Version::TLS_1_2)
        .http1_only()
        .build()
        .map_err(|error| format!("无法建立 WARP 注销客户端：{error}"))?;
    let mut url = reqwest::Url::parse(api_origin.trim_end_matches('/'))
        .map_err(|error| format!("WARP API 地址无效：{error}"))?;
    url.path_segments_mut()
        .map_err(|_| "WARP API 地址不能作为路径基础".to_owned())?
        .push(API_VERSION)
        .push("reg")
        .push(device_id);
    let response = client
        .delete(url)
        .header(header::USER_AGENT, "WARP for Android")
        .header("CF-Client-Version", CLIENT_VERSION)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| format!("Cloudflare WARP 注销请求失败：{error}"))?;
    let status = response.status();
    let bytes = response_bytes(response).await?;
    if !matches!(status, StatusCode::NO_CONTENT | StatusCode::NOT_FOUND) {
        return Err(format!(
            "Cloudflare WARP 注销失败（HTTP {}）：{}",
            status.as_u16(),
            provider_error(&bytes)
        ));
    }
    Ok(())
}

async fn register_at(
    api_origin: &str,
    public_key: &str,
    node_name: &str,
) -> Result<Registration, String> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(25))
        .min_tls_version(reqwest::tls::Version::TLS_1_2)
        .max_tls_version(reqwest::tls::Version::TLS_1_2)
        .http1_only()
        .build()
        .map_err(|error| format!("无法建立 WARP 注册客户端：{error}"))?;
    let tos = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| format!("无法生成 WARP ToS 时间戳：{error}"))?;
    let model = format!("Brocade {node_name}");
    let response = client
        .post(format!(
            "{}/{API_VERSION}/reg",
            api_origin.trim_end_matches('/')
        ))
        .header(header::USER_AGENT, "WARP for Android")
        .header("CF-Client-Version", CLIENT_VERSION)
        .json(&RegisterRequest {
            fcm_token: "",
            install_id: "",
            key: public_key,
            key_type: "curve25519",
            tunnel_type: "wireguard",
            locale: "en_US",
            model: &model,
            tos: &tos,
            device_type: "Android",
        })
        .send()
        .await
        .map_err(|error| format!("Cloudflare WARP 注册请求失败：{error}"))?;
    let status = response.status();
    let bytes = response_bytes(response).await?;
    if bytes.len() > RESPONSE_LIMIT {
        return Err(format!(
            "Cloudflare WARP 返回异常大的响应（{} bytes）",
            bytes.len()
        ));
    }
    if status != StatusCode::OK {
        let detail = provider_error(&bytes);
        return Err(format!(
            "Cloudflare WARP 注册失败（HTTP {}）：{detail}。该接口为非官方兼容接口，可稍后重试",
            status.as_u16()
        ));
    }

    parse_registration(&bytes)
}

fn parse_registration(bytes: &[u8]) -> Result<Registration, String> {
    let value = serde_json::from_slice::<Value>(bytes)
        .map_err(|error| format!("Cloudflare WARP 返回的不是有效 JSON：{error}"))?;
    let response = serde_json::from_value::<RegisterResponse>(value)
        .map_err(|error| format!("Cloudflare WARP 响应结构已变化：{error}"))?;
    let peer = response
        .config
        .peers
        .into_iter()
        .next()
        .ok_or_else(|| "Cloudflare WARP 响应没有 peer".to_owned())?;
    let local_addresses = [
        response.config.interface.addresses.v4,
        response.config.interface.addresses.v6,
    ]
    .into_iter()
    .filter(|address| !address.trim().is_empty())
    .map(normalize_interface_address)
    .collect::<Vec<_>>();
    if local_addresses.iter().any(|address| {
        let Some((ip, prefix)) = address.split_once('/') else {
            return true;
        };
        match (ip.parse::<std::net::IpAddr>(), prefix.parse::<u8>()) {
            (Ok(ip), Ok(prefix)) => prefix > if ip.is_ipv4() { 32 } else { 128 },
            _ => true,
        }
    }) {
        return Err("Cloudflare WARP 返回了无效隧道地址".to_owned());
    }
    if local_addresses.is_empty() {
        return Err("Cloudflare WARP 响应没有隧道地址".to_owned());
    }
    let suggested_endpoint = preferred_endpoint(peer.endpoint);

    Ok(Registration {
        device_id: response.id,
        account_id: response.account.id,
        access_token: response.token,
        peer_public_key: peer.public_key,
        local_addresses,
        suggested_endpoint,
    })
}

fn preferred_endpoint(endpoint: Endpoint) -> Option<String> {
    let host = endpoint.host.trim();
    let host_port = host
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .filter(|port| *port != 0);
    let v4 = endpoint.v4.trim();
    if let Some((address, raw_port)) = v4.rsplit_once(':') {
        if !address.is_empty() {
            if let Some(port) = raw_port.parse::<u16>().ok().filter(|port| *port != 0) {
                return Some(format!("{address}:{port}"));
            }
            // Current registrations can return a usable v4 address with port 0 while the
            // hostname carries the actual WireGuard port. Combining the two avoids both an
            // invalid target port and DNS interception of engage.cloudflareclient.com.
            if let Some(port) = host_port {
                return Some(format!("{address}:{port}"));
            }
        }
    }
    [host, endpoint.v6.trim()]
        .into_iter()
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

fn normalize_interface_address(address: String) -> String {
    if address.contains('/') {
        return address;
    }
    if address.contains(':') {
        format!("{address}/128")
    } else {
        format!("{address}/32")
    }
}

fn provider_error(bytes: &[u8]) -> String {
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        for pointer in ["/message", "/error", "/errors/0/message"] {
            if let Some(message) = value.pointer(pointer).and_then(Value::as_str) {
                return message.chars().take(400).collect();
            }
        }
    }
    let body = String::from_utf8_lossy(bytes);
    let body = body.trim();
    if body.is_empty() {
        "没有错误详情".to_owned()
    } else {
        body.chars().take(400).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        net::{IpAddr, Ipv4Addr, TcpListener, TcpStream},
        os::unix::fs::OpenOptionsExt,
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };

    use axum::{
        extract::State,
        http::HeaderMap,
        routing::{delete, post},
        Json, Router,
    };
    use brocade_core::{
        artifacts::xray as xray_artifact,
        format::json as json_format,
        model::{
            Dns, DomainStrategy, ExternalOutboundProtocol, ExternalOutboundSecurity,
            GeodataSettings, RealityClientPolicy, WarpHttpVersion,
        },
        physical::node::{
            GrantSyncPlan, NodePlan, ResolvedConnection, XrayExternalOutboundPlan, XrayPlan,
        },
    };

    use super::*;

    #[derive(Clone)]
    struct MockRegistrationState {
        seen: Arc<Mutex<Option<(HeaderMap, Value)>>>,
        status: StatusCode,
        response: Value,
    }

    async fn mock_registration(
        State(state): State<MockRegistrationState>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        *state.seen.lock().unwrap() = Some((headers, body));
        (state.status, Json(state.response))
    }

    async fn registration_server(
        status: StatusCode,
        response: Value,
    ) -> (
        String,
        Arc<Mutex<Option<(HeaderMap, Value)>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(&format!("/{API_VERSION}/reg"), post(mock_registration))
            .with_state(MockRegistrationState {
                seen: Arc::clone(&seen),
                status,
                response,
            });
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (origin, seen, server)
    }

    fn registration_response() -> Value {
        serde_json::json!({
            "id": "device-1",
            "token": "token-1",
            "account": { "id": "account-1" },
            "config": {
                "client_id": "qBoV",
                "interface": {
                    "addresses": {
                        "v4": "172.16.0.2",
                        "v6": "2606:4700::1/128"
                    }
                },
                "peers": [{
                    "public_key": include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../brocade-core/tests/fixtures/warp/public.b64")).trim().to_owned(),
                    "endpoint": {
                        "host": "engage.cloudflareclient.com:2408",
                        "v4": "162.159.193.1:0",
                        "v6": "[2606:4700:d0::a29f:c001]:2408"
                    }
                }]
            }
        })
    }

    #[tokio::test]
    async fn bootstrap_registration_uses_the_provider_contract() {
        let (origin, seen, server) =
            registration_server(StatusCode::OK, registration_response()).await;
        let registration = register_at(&origin, "test-public-key", "hk-01")
            .await
            .unwrap();
        server.abort();

        assert_eq!(registration.device_id, "device-1");
        assert_eq!(registration.account_id, "account-1");
        assert_eq!(registration.access_token, "token-1");
        assert_eq!(registration.local_addresses[0], "172.16.0.2/32");
        assert_eq!(registration.local_addresses[1], "2606:4700::1/128");
        assert_eq!(
            registration.suggested_endpoint.as_deref(),
            Some("162.159.193.1:2408")
        );

        let guard = seen.lock().unwrap();
        let (headers, body) = guard.as_ref().expect("注册请求到达本地供应商替身");
        assert_eq!(headers[header::USER_AGENT], "WARP for Android");
        assert_eq!(headers["CF-Client-Version"], CLIENT_VERSION);
        assert_eq!(body["fcm_token"], "");
        assert_eq!(body["install_id"], "");
        assert_eq!(body["key"], "test-public-key");
        assert_eq!(body["locale"], "en_US");
        assert_eq!(body["model"], "Brocade hk-01");
        assert_eq!(body["type"], "Android");
        let tos = body["tos"].as_str().expect("ToS 时间戳");
        assert!(OffsetDateTime::parse(tos, &Rfc3339).is_ok(), "{tos}");
    }

    #[tokio::test]
    async fn registration_failure_keeps_the_provider_detail() {
        let (origin, _seen, server) = registration_server(
            StatusCode::FORBIDDEN,
            serde_json::json!({ "errors": [{ "message": "fingerprint rejected" }] }),
        )
        .await;
        let error = register_at(&origin, "test-public-key", "hk-01")
            .await
            .unwrap_err();
        server.abort();

        assert!(error.contains("HTTP 403"), "{error}");
        assert!(error.contains("fingerprint rejected"), "{error}");
        assert!(error.contains("非官方兼容接口"), "{error}");
    }

    async fn mock_delete_registration(
        State(seen): State<Arc<Mutex<Option<HeaderMap>>>>,
        headers: HeaderMap,
    ) -> StatusCode {
        *seen.lock().unwrap() = Some(headers);
        StatusCode::NO_CONTENT
    }

    #[tokio::test]
    async fn registration_cleanup_uses_the_device_path_and_bearer_token() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                &format!("/{API_VERSION}/reg/device-1"),
                delete(mock_delete_registration),
            )
            .with_state(Arc::clone(&seen));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let registration = parse_registration(
            &serde_json::to_vec(&registration_response()).expect("mock registration JSON"),
        )
        .unwrap();

        unregister_at(&origin, &registration.device_id, &registration.access_token)
            .await
            .unwrap();
        server.abort();

        let guard = seen.lock().unwrap();
        let headers = guard.as_ref().expect("注销请求到达本地供应商替身");
        assert_eq!(headers[header::AUTHORIZATION], "Bearer token-1");
        assert_eq!(headers[header::USER_AGENT], "WARP for Android");
        assert_eq!(headers["CF-Client-Version"], CLIENT_VERSION);
    }

    #[tokio::test]
    async fn registration_cleanup_treats_an_already_missing_device_as_success() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route(
            &format!("/{API_VERSION}/reg/device-1"),
            delete(|| async { StatusCode::NOT_FOUND }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        unregister_at(&origin, "device-1", "token-1").await.unwrap();
        server.abort();
    }

    #[test]
    fn registered_identity_becomes_a_private_core_xray_data_plane() {
        let registration = parse_registration(
            &serde_json::to_vec(&registration_response()).expect("mock registration JSON"),
        )
        .unwrap();
        let private_key = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../brocade-core/tests/fixtures/warp/private.b64"
        ))
        .trim()
        .to_owned();
        let socks_port = free_tcp_port().unwrap();
        let config =
            live_xray_config(&private_key, &registration, socks_port, WarpHttpVersion::H3).unwrap();

        let inbound = config["inbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|inbound| inbound["tag"] == "in:warp-live-e2e")
            .expect("private SOCKS inbound");
        assert_eq!(inbound["listen"], "127.0.0.1");
        assert_eq!(inbound["port"], socks_port);
        let outbound = config["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|outbound| outbound["tag"] == "out:external/warp-live-e2e")
            .expect("compiled WARP outbound");
        assert_eq!(outbound["protocol"], "masque");
        assert_eq!(
            outbound["streamSettings"]["masqueSettings"]["warp"]["privateKey"],
            private_key
        );
        assert_eq!(
            outbound["streamSettings"]["tlsSettings"]["alpn"],
            serde_json::json!(["h3"])
        );
        assert_eq!(
            config["routing"]["rules"][0]["outboundTag"],
            "out:external/warp-live-e2e"
        );

        // Geodata is part of the production node artifact, but unrelated to this WARP compiler
        // check. Keep the unit test independent from local geoip/geosite assets while executing
        // the exact Xray bytes this Console build embeds and distributes to nodes.
        let mut config = config;
        config
            .as_object_mut()
            .expect("Xray config object")
            .remove("geodata");
        let dir = PrivateTempDir::create(
            std::env::temp_dir().join(format!("brocade-warp-config-test-{socks_port}")),
        )
        .unwrap();
        let binary = embedded_xray_binary(dir.path());
        let config_path = dir.path().join("xray.json");
        write_private_json(&config_path, &config).unwrap();
        let checked = Command::new(binary)
            .args(["-test", "-c"])
            .arg(config_path)
            .output()
            .expect("跑得起内嵌 xray -test");
        assert!(
            checked.status.success(),
            "注册身份生成的配置不被当前内嵌 Xray 接受：{}{}",
            String::from_utf8_lossy(&checked.stdout),
            String::from_utf8_lossy(&checked.stderr)
        );
    }

    /// Live acceptance for the complete provider/data-plane boundary.
    ///
    /// This is deliberately ignored even when ordinary integration tests are enabled. It creates
    /// a real Cloudflare device and therefore needs two explicit acknowledgements:
    ///
    /// ```text
    /// BROCADE_RUN_WARP_LIVE_E2E=1 \
    /// BROCADE_ACCEPT_CLOUDFLARE_APPLICATION_TERMS=1 \
    /// cargo test -p brocade-console warp::tests::live_cloudflare_registration_reaches_the_internet_through_xray -- --ignored --exact --nocapture
    /// ```
    ///
    /// The source registration deletes itself at the end with the bearer token returned by
    /// registration.
    /// A cleanup failure prints the device id, so it can be removed separately instead of becoming
    /// an invisible provider-side leak.
    #[tokio::test]
    #[ignore = "creates and deletes a real Cloudflare WARP device"]
    async fn live_cloudflare_registration_reaches_the_internet_through_xray() {
        if std::env::var("BROCADE_RUN_WARP_LIVE_E2E").as_deref() != Ok("1") {
            eprintln!("跳过：设置 BROCADE_RUN_WARP_LIVE_E2E=1 才会创建真实 Cloudflare WARP 设备");
            return;
        }
        assert_eq!(
            std::env::var("BROCADE_ACCEPT_CLOUDFLARE_APPLICATION_TERMS").as_deref(),
            Ok("1"),
            "真实 WARP E2E 会提交 ToS 时间戳；确认后设置 BROCADE_ACCEPT_CLOUDFLARE_APPLICATION_TERMS=1"
        );
        let binary_dir = PrivateTempDir::create(
            std::env::temp_dir().join(format!("brocade-warp-live-xray-{}", std::process::id())),
        )
        .unwrap();
        let binary = live_xray_binary(binary_dir.path());
        assert!(
            Command::new("curl").arg("--version").output().is_ok(),
            "真实 WARP E2E 需要 curl"
        );
        let targets = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::lookup_host(("www.cloudflare.com", 443)),
        )
        .await
        .expect("trace 目标 DNS 查询超时")
        .expect("trace 目标 DNS 查询成功")
        .map(|address| address.ip())
        .collect::<Vec<_>>();
        let targets = [
            *targets.iter().find(|ip| ip.is_ipv4()).expect("trace IPv4"),
            *targets.iter().find(|ip| ip.is_ipv6()).expect("trace IPv6"),
        ];

        let keys = generate_masque_keys().unwrap();
        let registration = register(&keys.public_key, "brocade-live-e2e")
            .await
            .unwrap_or_else(|error| panic!("Cloudflare WARP 真实注册失败：{error}"));
        eprintln!("Cloudflare 真实申请与 MASQUE 密钥登记通过");
        let failures = [WarpHttpVersion::H3, WarpHttpVersion::H2]
            .into_iter()
            .filter_map(|version| {
                eprintln!("开始 {} 数据面测试", version.alpn());
                run_live_data_plane(&binary, &keys.private_key, &registration, version, targets)
                    .err()
                    .map(|error| format!("{}: {error}", version.alpn()))
            })
            .collect::<Vec<_>>();
        let outcome: Result<(), String> = if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("；"))
        };
        let cleanup = unregister(&registration.device_id, &registration.access_token).await;

        if let Err(error) = cleanup {
            panic!(
                "WARP E2E 清理失败；设备 {} 可能需要手工删除：{error}{}",
                registration.device_id,
                outcome
                    .as_ref()
                    .err()
                    .map(|data_error| format!("；数据面同时失败：{data_error}"))
                    .unwrap_or_default()
            );
        }
        eprintln!("Cloudflare 测试设备已注销");
        if let Err(error) = outcome {
            panic!("Cloudflare 设备已清理，但 WARP 数据面没有通过：{error}");
        }
    }

    fn run_live_data_plane(
        binary: &Path,
        private_key: &str,
        registration: &Registration,
        version: WarpHttpVersion,
        targets: [IpAddr; 2],
    ) -> Result<(), String> {
        let socks_port = free_tcp_port()?;
        let config = live_xray_config(private_key, registration, socks_port, version)?;

        let dir = PrivateTempDir::create(
            std::env::temp_dir().join(format!("brocade-warp-live-e2e-{socks_port}")),
        )?;
        let config_path = dir.path().join("xray.json");
        write_private_json(&config_path, &config)?;

        let checked = Command::new(binary)
            .args(["-test", "-c"])
            .arg(&config_path)
            .output()
            .map_err(|error| format!("无法执行 xray -test：{error}"))?;
        if !checked.status.success() {
            return Err(format!(
                "Core 生成的真实 WARP 配置未通过 xray -test：{}{}",
                String::from_utf8_lossy(&checked.stdout),
                String::from_utf8_lossy(&checked.stderr)
            ));
        }

        let mut child = Command::new(binary)
            .args(["run", "-c"])
            .arg(&config_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("无法启动真实 Xray：{error}"))?;
        let ready = (0..100).any(|_| {
            if TcpStream::connect((Ipv4Addr::LOCALHOST, socks_port)).is_ok() {
                true
            } else {
                thread::sleep(Duration::from_millis(100));
                false
            }
        });
        // Pin the target with --resolve, leaving the localhost SOCKS connection IPv4. curl's
        // --ipv6 also constrains the proxy connection and cannot reach a 127.0.0.1 listener.
        // socks5h separately exercises DNS resolution inside the MASQUE tunnel.
        let probes = [
            ("IPv4", Some(targets[0]), "socks5", Some(true)),
            ("IPv6", Some(targets[1]), "socks5", Some(false)),
            ("隧道 DNS", None, "socks5h", None),
        ];
        let curls = ready.then(|| {
            probes.map(|(_, address, scheme, _)| {
                let proxy = format!("{scheme}://127.0.0.1:{socks_port}");
                let mut curl = Command::new("curl");
                if let Some(address) = address {
                    curl.args(["--resolve", &format!("www.cloudflare.com:443:[{address}]")]);
                }
                curl.args([
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "20",
                    "--noproxy",
                    "",
                    "--proxy",
                    &proxy,
                    "https://www.cloudflare.com/cdn-cgi/trace",
                ])
                .output()
            })
        });
        let _ = child.kill();
        let xray_output = child
            .wait_with_output()
            .map_err(|error| format!("无法回收真实 Xray：{error}"))?;

        if !ready {
            return Err(format!(
                "Xray 没有监听测试 SOCKS 端口：{}{}",
                String::from_utf8_lossy(&xray_output.stdout),
                String::from_utf8_lossy(&xray_output.stderr)
            ));
        }
        let curls = curls.expect("ready 时一定运行 curl");
        for ((label, _, _, expect_ipv4), curl) in probes.into_iter().zip(curls) {
            let curl = curl.map_err(|error| format!("无法运行 {label} curl：{error}"))?;
            if !curl.status.success() {
                return Err(format!(
                    "{label} curl 未能通过 WARP：{}；Xray：{}{}",
                    String::from_utf8_lossy(&curl.stderr),
                    String::from_utf8_lossy(&xray_output.stdout),
                    String::from_utf8_lossy(&xray_output.stderr)
                ));
            }
            let trace = String::from_utf8_lossy(&curl.stdout);
            let warp = trace_field(&trace, "warp")
                .ok_or_else(|| format!("{label} Cloudflare trace 没有 warp 字段：{trace}"))?;
            if !matches!(warp, "on" | "plus") {
                return Err(format!(
                    "{label} Cloudflare trace 没确认 WARP（warp={warp}）：{trace}"
                ));
            }
            let exit_ip = trace_field(&trace, "ip")
                .ok_or_else(|| format!("{label} Cloudflare trace 没有出口 IP：{trace}"))?;
            let exit_ip = exit_ip.parse::<IpAddr>().map_err(|error| {
                format!("{label} Cloudflare trace 出口 IP 无效（{exit_ip}）：{error}")
            })?;
            if expect_ipv4.is_some_and(|v4| exit_ip.is_ipv4() != v4) {
                return Err(format!("{label} WARP 返回了错误地址族的出口 IP：{exit_ip}"));
            }
            eprintln!(
                "{} / {label} WARP 数据面通过：warp={warp}，出口 IP={exit_ip}",
                version.alpn()
            );
        }
        Ok(())
    }

    fn live_xray_config(
        private_key: &str,
        registration: &Registration,
        socks_port: u16,
        version: WarpHttpVersion,
    ) -> Result<Value, String> {
        // Exercise the default endpoint offered by Console, not the provider's suggestion.
        let (address, port) = ("162.159.198.2".to_owned(), 443);
        let plan = NodePlan {
            node_id: "warp-live-e2e".to_owned(),
            wireguard: None,
            phantun: None,
            xray: Some(XrayPlan {
                node_id: "warp-live-e2e".to_owned(),
                certificate_group_id: None,
                certificate_track: None,
                api_port: None,
                reality_client: RealityClientPolicy::default(),
                dns: Dns::System,
                domain_strategy: DomainStrategy::UseIp,
                connection: ResolvedConnection {
                    conn_idle_secs: 300,
                    uplink_only_secs: 2,
                    downlink_only_secs: 5,
                    buffer_size_kb: None,
                    handshake_secs: 60,
                    stats_user_online: false,
                },
                relay_mux: Default::default(),
                dns_route: None,
                inbounds: Vec::new(),
                hop_inbounds: Vec::new(),
                forward_outbounds: Vec::new(),
                egress_outbounds: Vec::new(),
                egress_dns: Vec::new(),
                external_outbounds: vec![XrayExternalOutboundPlan {
                    target_strategy: None,
                    tag: "out:external/warp-live-e2e".to_owned(),
                    address,
                    port,
                    protocol: ExternalOutboundProtocol::Warp {
                        http_version: version,
                        remote_dns: brocade_core::model::WarpMasqueOptions::default().remote_dns,
                    },
                    security: ExternalOutboundSecurity::None,
                    warp_masque: Some(brocade_core::physical::node::XrayWarpMasquePlan {
                        options: brocade_core::model::WarpMasqueOptions {
                            http_version: version,
                            ..Default::default()
                        },
                        private_key: private_key.to_owned(),
                        peer_public_key: registration.peer_public_key.clone(),
                        local_addresses: registration.local_addresses.clone(),
                    }),
                }],
                block_outbound: false,
                routing_rules: Vec::new(),
                reverse_portals: Vec::new(),
                reverse_bridges: Vec::new(),
                geodata: GeodataSettings::default(),
            }),
            grant_sync: GrantSyncPlan {
                node_id: "warp-live-e2e".to_owned(),
                updates: Vec::new(),
            },
            hy2_port_hops: Vec::new(),
        };
        let artifact = xray_artifact::build(&plan);
        let mut config = serde_json::from_str::<Value>(&json_format::xray(&artifact))
            .map_err(|error| format!("无法解析 Core 生成的 Xray 产物：{error}"))?;
        config["log"] = serde_json::json!({ "loglevel": "warning" });
        config["inbounds"]
            .as_array_mut()
            .ok_or_else(|| "Core Xray 产物没有 inbounds 数组".to_owned())?
            .push(serde_json::json!({
                "tag": "in:warp-live-e2e",
                "listen": "127.0.0.1",
                "port": socks_port,
                "protocol": "socks",
                "settings": { "auth": "noauth", "udp": false }
            }));
        config["routing"]["rules"]
            .as_array_mut()
            .ok_or_else(|| "Core Xray 产物没有 routing.rules 数组".to_owned())?
            .insert(
                0,
                serde_json::json!({
                    "type": "field",
                    "inboundTag": ["in:warp-live-e2e"],
                    "outboundTag": "out:external/warp-live-e2e"
                }),
            );
        Ok(config)
    }

    struct PrivateTempDir(PathBuf);

    impl PrivateTempDir {
        fn create(path: PathBuf) -> Result<Self, String> {
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path)
                .map_err(|error| format!("无法创建 WARP E2E 临时目录：{error}"))?;
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for PrivateTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn embedded_xray_binary(cache_root: &Path) -> PathBuf {
        crate::embedded_xray::prepare(cache_root, std::env::consts::ARCH)
            .expect("当前 Console 构建必须能发布并执行它内嵌的 Xray")
    }

    fn live_xray_binary(cache_root: &Path) -> PathBuf {
        let Some(value) = std::env::var_os("BROCADE_XRAY_BIN") else {
            return embedded_xray_binary(cache_root);
        };
        let path = PathBuf::from(value);
        assert!(
            path.is_file(),
            "BROCADE_XRAY_BIN 必须显式指向普通 Xray 文件：{}",
            path.display()
        );
        path
    }

    fn free_tcp_port() -> Result<u16, String> {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .and_then(|listener| listener.local_addr())
            .map(|address| address.port())
            .map_err(|error| format!("无法分配 WARP E2E SOCKS 端口：{error}"))
    }

    fn write_private_json(path: &Path, value: &Value) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| format!("无法创建私密 Xray 配置：{error}"))?;
        serde_json::to_writer_pretty(&mut file, value)
            .map_err(|error| format!("无法写入私密 Xray 配置：{error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("无法结束私密 Xray 配置：{error}"))
    }

    fn trace_field<'a>(trace: &'a str, key: &str) -> Option<&'a str> {
        trace
            .lines()
            .filter_map(|line| line.split_once('='))
            .find_map(|(name, value)| (name == key).then_some(value))
    }

    #[test]
    fn masque_public_key_normalizes_pem_and_rejects_invalid_envelopes() {
        let expected = registration_response()["config"]["peers"][0]["public_key"]
            .as_str()
            .unwrap()
            .to_owned();
        let pem = format!("-----BEGIN PUBLIC KEY-----\n{expected}\n-----END PUBLIC KEY-----");
        assert_eq!(normalize_masque_public_key(&expected).unwrap(), expected);
        assert_eq!(normalize_masque_public_key(&pem).unwrap(), expected);
        for invalid in [
            pem.replace("PUBLIC KEY", "CERTIFICATE"),
            format!("{pem}\n{pem}"),
            "-----BEGIN PUBLIC KEY-----\ninvalid\n-----END PUBLIC KEY-----".to_owned(),
            BASE64_STANDARD.encode([0_u8; 32]),
        ] {
            assert!(normalize_masque_public_key(&invalid).is_err());
        }
    }

    #[tokio::test]
    async fn masque_registration_enrolls_p256_and_cleans_up_failed_enrollment() {
        for reject in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let enrollment = Arc::new(Mutex::new(None));
            let cleanup = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let seen = Arc::clone(&enrollment);
            let deleted = Arc::clone(&cleanup);
            let app = Router::new()
                .route(
                    &format!("/{API_VERSION}/reg"),
                    post(|| async { Json(registration_response()) }),
                )
                .route(
                    &format!("/{API_VERSION}/reg/device-1"),
                    axum::routing::patch(move |headers: HeaderMap, Json(body): Json<Value>| {
                        let seen = Arc::clone(&seen);
                        async move {
                            *seen.lock().unwrap() = Some((headers, body));
                            let mut response = registration_response();
                            response.as_object_mut().unwrap().remove("token");
                            response["warp_enabled"] = Value::Bool(true);
                            let public_key = response["config"]["peers"][0]["public_key"]
                                .as_str()
                                .unwrap();
                            response["config"]["peers"][0]["public_key"] = Value::String(
                                format!("-----BEGIN PUBLIC KEY-----\n{public_key}\n-----END PUBLIC KEY-----\n"),
                            );
                            response["config"]["peers"][0]["endpoint"] =
                                serde_json::json!({"v4":"162.159.198.2:443"});
                            (
                                if reject {
                                    StatusCode::FORBIDDEN
                                } else {
                                    StatusCode::OK
                                },
                                Json(response),
                            )
                        }
                    })
                    .delete(move || {
                        let deleted = Arc::clone(&deleted);
                        async move {
                            deleted.store(true, std::sync::atomic::Ordering::SeqCst);
                            StatusCode::NO_CONTENT
                        }
                    }),
                );
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let keys = generate_masque_keys().unwrap();
            let result = register_masque_at(&origin, &keys.public_key, "fixture-node").await;
            server.abort();
            let guard = enrollment.lock().unwrap();
            let (headers, body) = guard.as_ref().expect("MASQUE enrollment request");
            assert_eq!(headers[header::AUTHORIZATION], "Bearer token-1");
            assert_eq!(body["key"], keys.public_key);
            assert_eq!(body["key_type"], "secp256r1");
            assert_eq!(body["tunnel_type"], "masque");
            assert_eq!(body["warp_enabled"], true);
            assert_eq!(cleanup.load(std::sync::atomic::Ordering::SeqCst), reject);
            if reject {
                assert!(result.is_err());
            } else {
                let registration = result.unwrap();
                assert_eq!(registration.access_token, "token-1");
                assert_eq!(
                    registration.peer_public_key,
                    registration_response()["config"]["peers"][0]["public_key"]
                        .as_str()
                        .unwrap()
                );
                assert_eq!(
                    registration.suggested_endpoint.as_deref(),
                    Some("162.159.198.2:443")
                );
                let private = BASE64_STANDARD.decode(keys.private_key).unwrap();
                let parsed = rcgen::KeyPair::try_from(private).unwrap();
                assert_eq!(
                    BASE64_STANDARD.encode(parsed.subject_public_key_info()),
                    keys.public_key
                );
            }
        }
    }

    #[test]
    fn registration_shape_parses() {
        let registration =
            parse_registration(&serde_json::to_vec(&registration_response()).unwrap()).unwrap();
        assert_eq!(registration.local_addresses[0], "172.16.0.2/32");
        assert_eq!(registration.local_addresses[1], "2606:4700::1/128");
        assert_eq!(
            registration.suggested_endpoint.as_deref(),
            Some("162.159.193.1:2408")
        );
    }
}
