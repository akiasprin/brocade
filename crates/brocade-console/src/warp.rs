//! Cloudflare WARP's WireGuard-compatible registration boundary.
//!
//! This is deliberately a small adapter rather than a Cloudflare account model. The endpoint is
//! undocumented and wgcf is unaffiliated with Cloudflare, so every assumption is kept in this
//! file and failures are returned with enough provider detail to diagnose a changed response.
//! The rest of Brocade sees only an ordinary per-machine WireGuard identity.

use std::time::Duration;

use base64::{
    engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use reqwest::{header, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const API_ORIGIN: &str = "https://api.cloudflareclient.com";
// Current wgcf v2 identifies itself as the Android 6.3 client against this API shape. Keeping
// both tokens together is important: Cloudflare has rejected mismatched fingerprints with 1020.
const API_VERSION: &str = "v0a1922";
const CLIENT_VERSION: &str = "a-6.3-1922";
const RESPONSE_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub device_id: String,
    pub account_id: String,
    pub access_token: String,
    pub peer_public_key: String,
    pub local_addresses: Vec<String>,
    pub reserved: Vec<u8>,
    pub suggested_endpoint: Option<String>,
}

#[derive(Debug, Serialize)]
struct RegisterRequest<'a> {
    fcm_token: &'a str,
    install_id: &'a str,
    key: &'a str,
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
    #[serde(default)]
    client_id: String,
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
    register_at(API_ORIGIN, public_key, node_name).await
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
        .header(header::USER_AGENT, "okhttp/3.12.1")
        .header("CF-Client-Version", CLIENT_VERSION)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| format!("Cloudflare WARP 注销请求失败：{error}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("无法读取 Cloudflare WARP 注销响应：{error}"))?;
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
        .header(header::USER_AGENT, "okhttp/3.12.1")
        .header("CF-Client-Version", CLIENT_VERSION)
        .json(&RegisterRequest {
            fcm_token: "",
            install_id: "",
            key: public_key,
            locale: "en_US",
            model: &model,
            tos: &tos,
            device_type: "Android",
        })
        .send()
        .await
        .map_err(|error| format!("Cloudflare WARP 注册请求失败：{error}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("无法读取 Cloudflare WARP 响应：{error}"))?;
    if bytes.len() > RESPONSE_LIMIT {
        return Err(format!(
            "Cloudflare WARP 返回异常大的响应（{} bytes）",
            bytes.len()
        ));
    }
    if status != StatusCode::OK {
        let detail = provider_error(&bytes);
        return Err(format!(
            "Cloudflare WARP 注册失败（HTTP {}）：{detail}。该接口为非官方兼容接口，可稍后重试或导入普通 WireGuard 配置",
            status.as_u16()
        ));
    }

    parse_registration(&bytes)
}

fn parse_registration(bytes: &[u8]) -> Result<Registration, String> {
    let mut value = serde_json::from_slice::<Value>(bytes)
        .map_err(|error| format!("Cloudflare WARP 返回的不是有效 JSON：{error}"))?;
    // Older compatible endpoints wrapped the same object in `result`; the current wgcf OpenAPI
    // describes the direct form. Accepting both costs no ambiguity and makes provider rollbacks
    // non-disruptive.
    if let Some(result) = value.get_mut("result") {
        value = result.take();
    }
    let response = serde_json::from_value::<RegisterResponse>(value)
        .map_err(|error| format!("Cloudflare WARP 响应结构已变化：{error}"))?;
    let peer = response
        .config
        .peers
        .into_iter()
        .next()
        .ok_or_else(|| "Cloudflare WARP 响应没有 WireGuard peer".to_owned())?;
    let local_addresses = [
        response.config.interface.addresses.v4,
        response.config.interface.addresses.v6,
    ]
    .into_iter()
    .filter(|address| !address.trim().is_empty())
    .map(normalize_interface_address)
    .collect::<Vec<_>>();
    if local_addresses.is_empty() {
        return Err("Cloudflare WARP 响应没有隧道地址".to_owned());
    }
    let reserved = decode_client_id(&response.config.client_id)?;
    let suggested_endpoint = preferred_endpoint(peer.endpoint);

    Ok(Registration {
        device_id: response.id,
        account_id: response.account.id,
        access_token: response.token,
        peer_public_key: peer.public_key,
        local_addresses,
        reserved,
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

fn decode_client_id(client_id: &str) -> Result<Vec<u8>, String> {
    if client_id.trim().is_empty() {
        return Ok(Vec::new());
    }
    let bytes = BASE64_STANDARD
        .decode(client_id)
        .or_else(|_| URL_SAFE_NO_PAD.decode(client_id))
        .map_err(|_| "Cloudflare WARP client_id 不是有效 Base64".to_owned())?;
    if bytes.len() != 3 {
        return Err(format!(
            "Cloudflare WARP client_id 解码后应为 3 bytes，实际为 {}",
            bytes.len()
        ));
    }
    Ok(bytes)
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
            GeodataSettings, RealityClientPolicy,
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
                    "public_key": "bmXOC+F1FxEMF9dyiK2H5/1SUtzH0JuVo51h2wPfgyo=",
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
    async fn registration_request_matches_the_wgcf_contract() {
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
        assert_eq!(registration.reserved, vec![168, 26, 21]);
        assert_eq!(
            registration.suggested_endpoint.as_deref(),
            Some("162.159.193.1:2408")
        );

        let guard = seen.lock().unwrap();
        let (headers, body) = guard.as_ref().expect("注册请求到达本地供应商替身");
        assert_eq!(headers[header::USER_AGENT], "okhttp/3.12.1");
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
        assert_eq!(headers[header::USER_AGENT], "okhttp/3.12.1");
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
        let private_key = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";
        let socks_port = free_tcp_port().unwrap();
        let config = live_xray_config(private_key, &registration, socks_port).unwrap();

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
        assert_eq!(outbound["protocol"], "wireguard");
        assert_eq!(outbound["settings"]["secretKey"], private_key);
        assert_eq!(outbound["settings"]["address"][0], "172.16.0.2/32");
        assert_eq!(outbound["settings"]["address"][1], "2606:4700::1/128");
        assert_eq!(
            outbound["settings"]["peers"][0]["allowedIPs"],
            serde_json::json!(["0.0.0.0/0", "::/0"])
        );
        assert_eq!(outbound["settings"]["peers"][0]["keepAlive"], 25);
        assert_eq!(
            outbound["settings"]["reserved"],
            serde_json::json!([168, 26, 21])
        );
        assert_eq!(
            outbound["settings"]["peers"][0]["endpoint"],
            "162.159.193.1:2408"
        );
        assert_eq!(
            config["routing"]["rules"][0]["outboundTag"],
            "out:external/warp-live-e2e"
        );

        if let Some(binary) = xray_binary() {
            // Geodata is part of the production node artifact, but unrelated to this WARP
            // compiler check. Keep the unit test independent from local geoip/geosite assets.
            let mut config = config;
            config
                .as_object_mut()
                .expect("Xray config object")
                .remove("geodata");
            let dir = PrivateTempDir::create(
                std::env::temp_dir().join(format!("brocade-warp-config-test-{socks_port}")),
            )
            .unwrap();
            let config_path = dir.path().join("xray.json");
            write_private_json(&config_path, &config).unwrap();
            let checked = Command::new(binary)
                .args(["-test", "-c"])
                .arg(config_path)
                .output()
                .expect("跑得起 xray -test");
            assert!(
                checked.status.success(),
                "注册身份生成的配置 Xray 不认：{}{}",
                String::from_utf8_lossy(&checked.stdout),
                String::from_utf8_lossy(&checked.stderr)
            );
        }
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
        let binary = xray_binary()
            .expect("真实 WARP E2E 需要 BROCADE_XRAY_BIN，或工作区根目录下可执行的 .tools/xray");
        assert!(
            Command::new("curl").arg("--version").output().is_ok(),
            "真实 WARP E2E 需要 curl"
        );

        let keys = brocade_store::generate_wireguard_keypair().unwrap();
        let registration = register(&keys.public_key, "brocade-live-e2e")
            .await
            .unwrap_or_else(|error| panic!("Cloudflare WARP 真实注册失败：{error}"));
        let outcome = run_live_data_plane(&binary, &keys.private_key, &registration);
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
        if let Err(error) = outcome {
            panic!("Cloudflare 设备已清理，但 WARP 数据面没有通过：{error}");
        }
    }

    fn run_live_data_plane(
        binary: &Path,
        private_key: &str,
        registration: &Registration,
    ) -> Result<(), String> {
        let socks_port = free_tcp_port()?;
        let config = live_xray_config(private_key, registration, socks_port)?;

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
        // `socks5` (without the `h`) makes curl resolve the target locally. Combined with
        // --ipv4/--ipv6 this sends an address literal through SOCKS, so both inner address
        // families are genuinely exercised instead of asking Xray to choose one for a domain.
        let proxy = format!("socks5://127.0.0.1:{socks_port}");
        let curls = ready.then(|| {
            [("IPv4", "--ipv4", true), ("IPv6", "--ipv6", false)].map(|(_, family_arg, _)| {
                Command::new("curl")
                    .args([
                        "--silent",
                        "--show-error",
                        "--max-time",
                        "20",
                        family_arg,
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
        for ((label, _, expect_ipv4), curl) in [("IPv4", "--ipv4", true), ("IPv6", "--ipv6", false)]
            .into_iter()
            .zip(curls)
        {
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
            if exit_ip.is_ipv4() != expect_ipv4 {
                return Err(format!("{label} WARP 返回了错误地址族的出口 IP：{exit_ip}"));
            }
            eprintln!("{label} WARP 数据面通过：warp={warp}，出口 IP={exit_ip}");
        }
        Ok(())
    }

    fn live_xray_config(
        private_key: &str,
        registration: &Registration,
        socks_port: u16,
    ) -> Result<Value, String> {
        let (address, port) = split_endpoint(
            registration
                .suggested_endpoint
                .as_deref()
                .unwrap_or("engage.cloudflareclient.com:2408"),
        )?;
        let plan = NodePlan {
            node_id: "warp-live-e2e".to_owned(),
            wireguard: None,
            phantun: None,
            xray: Some(XrayPlan {
                node_id: "warp-live-e2e".to_owned(),
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
                dns_route: None,
                inbounds: Vec::new(),
                hop_inbounds: Vec::new(),
                forward_outbounds: Vec::new(),
                egress_outbounds: Vec::new(),
                egress_dns: Vec::new(),
                external_outbounds: vec![XrayExternalOutboundPlan {
                    tag: "out:external/warp-live-e2e".to_owned(),
                    address,
                    port,
                    protocol: ExternalOutboundProtocol::Wireguard {
                        credential: private_key.to_owned(),
                        peer_public_key: registration.peer_public_key.clone(),
                        local_addresses: registration.local_addresses.clone(),
                        mtu: 1280,
                        reserved: registration.reserved.clone(),
                        keep_alive: 25,
                        allowed_ips: vec!["0.0.0.0/0".to_owned(), "::/0".to_owned()],
                        no_kernel_tun: true,
                        domain_strategy: "ForceIP".to_owned(),
                    },
                    security: ExternalOutboundSecurity::None,
                    wireguard_workers: 0,
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

    fn split_endpoint(endpoint: &str) -> Result<(String, u16), String> {
        let endpoint = endpoint.trim();
        let (address, port) = if let Some(rest) = endpoint.strip_prefix('[') {
            let (address, port) = rest
                .split_once("]:")
                .ok_or_else(|| format!("WARP 返回的 IPv6 Endpoint 无效：{endpoint}"))?;
            (address, port)
        } else {
            endpoint
                .rsplit_once(':')
                .ok_or_else(|| format!("WARP 返回的 Endpoint 缺少端口：{endpoint}"))?
        };
        if address.trim().is_empty() {
            return Err("WARP 返回的 Endpoint 地址为空".to_owned());
        }
        let port = port
            .parse::<u16>()
            .map_err(|error| format!("WARP 返回的 Endpoint 端口无效（{port}）：{error}"))?;
        if port == 0 {
            return Err("WARP 返回的 Endpoint 端口不能为 0".to_owned());
        }
        Ok((address.to_owned(), port))
    }

    fn xray_binary() -> Option<PathBuf> {
        let path = match std::env::var_os("BROCADE_XRAY_BIN") {
            Some(value) => PathBuf::from(value),
            None => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()?
                .parent()?
                .join(".tools/xray"),
        };
        path.is_file().then_some(path)
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
    fn current_and_wrapped_registration_shapes_parse() {
        let direct = registration_response();
        for value in [direct.clone(), serde_json::json!({ "result": direct })] {
            let registration = parse_registration(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(registration.local_addresses[0], "172.16.0.2/32");
            assert_eq!(registration.local_addresses[1], "2606:4700::1/128");
            assert_eq!(registration.reserved, vec![168, 26, 21]);
            assert_eq!(
                registration.suggested_endpoint.as_deref(),
                Some("162.159.193.1:2408")
            );
        }
    }

    #[test]
    fn malformed_client_id_is_not_silently_discarded() {
        assert!(decode_client_id("AQI=").unwrap_err().contains("3 bytes"));
    }
}
