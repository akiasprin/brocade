mod stats;
mod vless;

use std::net::IpAddr;

use axum::{extract::State, http::HeaderMap, response::IntoResponse, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    addr::{echo_ipv4, preview_ipv4, preview_ipv6, CLIENT_SLOT, ECHO_SLOT},
    config::Config,
    console::get_console_json,
    docker::{
        docker_ok, docker_output, docker_output_owned, preview_container_by_ip, CLIENT_CONTAINER,
        ECHO_CONTAINER, LABEL_IPV4, LABEL_IPV6, LABEL_PREVIEW, LABEL_ROLE,
    },
    error::PreviewError,
    param::{percent_encode_path_segment, required_slug},
    provision::{ensure_network, ensure_node_image, shell_quote},
    AppState,
};

use stats::{read_user_stats, UserStats};
use vless::VlessEntry;

#[derive(Debug, Deserialize)]
pub(crate) struct VerifySubscriptionRequest {
    tenant_id: String,
    user_id: String,
}

/// Fetch a user's subscription, really run traffic through each entry, then check back on the node
/// whether that user's counters grew.
pub(crate) async fn preview_verify_subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<VerifySubscriptionRequest>,
) -> Result<impl IntoResponse, PreviewError> {
    let tenant = required_slug(&request.tenant_id, "tenant_id")?;
    let user = required_slug(&request.user_id, "user_id")?;
    let target = format!("{tenant}:{user}");
    let uri = get_console_json(
        &state,
        &format!(
            "/artifacts/content/user/{}/uri",
            percent_encode_path_segment(&target)
        ),
        &headers,
    )
    .await
    .map_err(PreviewError::upstream)?;
    let content = uri
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();

    // Every request uses the same client container, fixed SOCKS port, process name, and temp
    // files. Keep the complete probe transaction exclusive across HTTP requests as well as
    // serializing entries below, otherwise concurrent callers can kill each other's Xray.
    let _probe_guard = state.subscription_probe_lock.lock().await;
    ensure_probe_containers(&state.config).await?;

    // One entry at a time for the same shared resources.
    let user_stat_prefix = format!("{user}@{tenant}#");
    let mut results = Vec::new();
    for line in content.lines().filter(|line| line.starts_with("vless://")) {
        results.push(match VlessEntry::parse(line) {
            Ok(entry) => run_subscription_probe(&state.config, &entry, &user_stat_prefix).await,
            Err(error) => json!({
                "name": "",
                "uri_parse": "error",
                "connect": "not-run",
                "http": "not-run",
                "stats_changed": false,
                "stats_scope": "not-run",
                "error": error
            }),
        });
    }

    let ok = !results.is_empty()
        && results.iter().all(|entry| {
            entry.get("http").and_then(Value::as_str) == Some("ok")
                && entry
                    .get("stats_changed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        });
    Ok(Json(json!({
        "user": target,
        "ok": ok,
        "stage": "traffic-probed",
        "message": "subscription entries were run through a preview xray client and verified against preview server user stats",
        "entries": results
    })))
}

/// Prepare the probe's two ends: the client container that originates traffic, and the echo
/// container that reflects the source IP.
async fn ensure_probe_containers(config: &Config) -> Result<(), PreviewError> {
    ensure_network(config).await?;
    ensure_node_image(config).await?;
    let client_ipv4 = preview_ipv4(config, CLIENT_SLOT)?;
    let client_ipv6 = preview_ipv6(config, CLIENT_SLOT)?;
    if !docker_ok(config, &["container", "inspect", CLIENT_CONTAINER]).await {
        docker_output_owned(
            config,
            vec![
                "run".to_owned(),
                "-d".to_owned(),
                "--name".to_owned(),
                CLIENT_CONTAINER.to_owned(),
                "--hostname".to_owned(),
                "preview-client".to_owned(),
                "--network".to_owned(),
                config.network.clone(),
                "--ip".to_owned(),
                client_ipv4.to_string(),
                "--ip6".to_owned(),
                client_ipv6.to_string(),
                "--add-host".to_owned(),
                "host.docker.internal:host-gateway".to_owned(),
                "--label".to_owned(),
                format!("{LABEL_PREVIEW}=1"),
                "--label".to_owned(),
                format!("{LABEL_ROLE}=client"),
                "--label".to_owned(),
                format!("{LABEL_IPV4}={client_ipv4}"),
                "--label".to_owned(),
                format!("{LABEL_IPV6}={client_ipv6}"),
                "--entrypoint".to_owned(),
                "sleep".to_owned(),
                config.node_image.clone(),
                "infinity".to_owned(),
            ],
        )
        .await?;
    }
    ensure_probe_xray(config).await?;

    let echo_ipv4_addr = preview_ipv4(config, ECHO_SLOT)?;
    let echo_ipv6_addr = preview_ipv6(config, ECHO_SLOT)?;
    if !docker_ok(config, &["container", "inspect", ECHO_CONTAINER]).await {
        let code = r#"
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"source": self.client_address[0]}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass
HTTPServer(("0.0.0.0", 80), H).serve_forever()
"#;
        docker_output_owned(
            config,
            vec![
                "run".to_owned(),
                "-d".to_owned(),
                "--name".to_owned(),
                ECHO_CONTAINER.to_owned(),
                "--hostname".to_owned(),
                "preview-echo".to_owned(),
                "--network".to_owned(),
                config.network.clone(),
                "--ip".to_owned(),
                echo_ipv4_addr.to_string(),
                "--ip6".to_owned(),
                echo_ipv6_addr.to_string(),
                "--label".to_owned(),
                format!("{LABEL_PREVIEW}=1"),
                "--label".to_owned(),
                format!("{LABEL_ROLE}=echo"),
                "--label".to_owned(),
                format!("{LABEL_IPV4}={echo_ipv4_addr}"),
                "--label".to_owned(),
                format!("{LABEL_IPV6}={echo_ipv6_addr}"),
                "python:3.12-alpine".to_owned(),
                "python3".to_owned(),
                "-u".to_owned(),
                "-c".to_owned(),
                code.to_owned(),
            ],
        )
        .await?;
    }
    Ok(())
}

/// Keep the probe client on the same Brocade Xray distributed to managed nodes. The helper
/// container deliberately starts from the bare node image, so it has not run the enrollment
/// installer that normally supplies this binary.
async fn ensure_probe_xray(config: &Config) -> Result<(), PreviewError> {
    docker_output_owned(
        config,
        vec![
            "exec".to_owned(),
            CLIENT_CONTAINER.to_owned(),
            "sh".to_owned(),
            "-lc".to_owned(),
            probe_xray_install_script(config),
        ],
    )
    .await?;
    Ok(())
}

fn probe_xray_install_script(config: &Config) -> String {
    let configured_url = config.xray_bin_url.as_deref().unwrap_or("");
    let configured_sha256 = config.xray_bin_sha256.as_deref().unwrap_or("");
    format!(
        r#"set -eu
XRAY_URL={configured_url}
XRAY_SHA256={configured_sha256}
if [ -z "$XRAY_URL" ]; then
  case "$(uname -m)" in
    x86_64|amd64) XRAY_ARCH=x86_64 ;;
    aarch64|arm64) XRAY_ARCH=aarch64 ;;
    *) echo "unsupported preview probe architecture: $(uname -m)" >&2; exit 1 ;;
  esac
  curl -fsSL {dist_url}/enroll/dist -o /tmp/brocade-probe-dist.json
  XRAY_URL=$(jq -er --arg key "xray_bin_url_$XRAY_ARCH" '.xray_bin_url // .[$key] // empty' /tmp/brocade-probe-dist.json)
  XRAY_SHA256=$(jq -er --arg key "xray_bin_sha256_$XRAY_ARCH" '.xray_bin_sha256 // .[$key] // empty' /tmp/brocade-probe-dist.json)
  [ -n "$XRAY_SHA256" ] || {{ echo "control-plane Xray digest is missing" >&2; exit 1; }}
fi
if [ -x /usr/local/bin/xray ]; then
  if [ -z "$XRAY_SHA256" ] || printf '%s  %s\n' "$XRAY_SHA256" /usr/local/bin/xray | sha256sum -c - >/dev/null 2>&1; then
    rm -f /tmp/brocade-probe-dist.json
    exit 0
  fi
fi
curl -fsSL "$XRAY_URL" -o /tmp/brocade-probe-xray
if [ -n "$XRAY_SHA256" ]; then
  printf '%s  %s\n' "$XRAY_SHA256" /tmp/brocade-probe-xray | sha256sum -c -
fi
install -m 0755 /tmp/brocade-probe-xray /usr/local/bin/xray
rm -f /tmp/brocade-probe-xray /tmp/brocade-probe-dist.json
xray version >/dev/null
"#,
        configured_url = shell_quote(configured_url),
        configured_sha256 = shell_quote(configured_sha256),
        dist_url = shell_quote(&config.agent_url),
    )
}

/// Run one subscription entry: read the user's counters, pass traffic, read them again.
///
/// Where the machine the subscription points at is not on the preview network the counters cannot
/// be read, and `stats_scope` says which case it is.
async fn run_subscription_probe(
    config: &Config,
    entry: &VlessEntry,
    user_stat_prefix: &str,
) -> Value {
    let mut stats_scope = "not-preview-ip";
    let mut stats_container = None::<String>;
    let mut stats_before = None::<UserStats>;
    let mut stats_after = None::<UserStats>;
    let mut stats_error = None::<String>;

    if let Ok(host_ip) = entry.host.parse::<IpAddr>() {
        match preview_container_by_ip(config, host_ip).await {
            Ok(Some(container)) => {
                stats_scope = "user-prefix";
                match read_user_stats(config, &container, user_stat_prefix).await {
                    Ok(stats) => stats_before = Some(stats),
                    Err(error) => {
                        stats_error = Some(format!("before stats read failed: {}", error.message))
                    }
                }
                stats_container = Some(container);
            }
            Ok(None) => {
                stats_scope = "container-not-found";
            }
            Err(error) => {
                stats_scope = "container-lookup-error";
                stats_error = Some(error.message);
            }
        }
    }

    let output = docker_output(
        config,
        &[
            "exec",
            CLIENT_CONTAINER,
            "sh",
            "-lc",
            &probe_script(config, entry),
        ],
    )
    .await;

    if let Some(container) = &stats_container {
        match read_user_stats(config, container, user_stat_prefix).await {
            Ok(stats) => stats_after = Some(stats),
            Err(error) => {
                let message = format!("after stats read failed: {}", error.message);
                stats_error = Some(match stats_error {
                    Some(existing) => format!("{existing}; {message}"),
                    None => message,
                });
            }
        }
    }

    let stats_changed = stats_before
        .as_ref()
        .zip(stats_after.as_ref())
        .is_some_and(|(before, after)| after.total_bytes > before.total_bytes);
    let stats_before_total = stats_before.as_ref().map(|stats| stats.total_bytes);
    let stats_after_total = stats_after.as_ref().map(|stats| stats.total_bytes);
    let stats_labels_after = stats_after.as_ref().map(|stats| stats.labels.clone());

    match output {
        Ok(output) => {
            let (body, log) = split_probe_output(&output);
            let source = serde_json::from_str::<Value>(body).ok().and_then(|value| {
                value
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            json!({
                "name": entry.name,
                "uri_parse": "ok",
                "connect": if source.is_some() { "ok" } else { "failed" },
                "http": if source.is_some() { "ok" } else { "failed" },
                "egress_ip": source,
                "stats_changed": stats_changed,
                "stats_scope": stats_scope,
                "stats_container": stats_container,
                "stats_label_prefix": user_stat_prefix,
                "stats_before": stats_before_total,
                "stats_after": stats_after_total,
                "stats_labels_after": stats_labels_after,
                "stats_error": stats_error,
                "log": log
            })
        }
        Err(error) => json!({
            "name": entry.name,
            "uri_parse": "ok",
            "connect": "failed",
            "http": "failed",
            "stats_changed": stats_changed,
            "stats_scope": stats_scope,
            "stats_container": stats_container,
            "stats_label_prefix": user_stat_prefix,
            "stats_before": stats_before_total,
            "stats_after": stats_after_total,
            "stats_labels_after": stats_labels_after,
            "stats_error": stats_error,
            "error": error.message
        }),
    }
}

/// Start a one-shot xray in the client container and reach echo once through its local SOCKS.
///
/// The port, config path, and process name are all fixed, on the assumption that only one probe
/// runs at a time.
fn probe_script(config: &Config, entry: &VlessEntry) -> String {
    let mut user = json!({
        "id": entry.uuid,
        "encryption": "none"
    });
    if let Some(flow) = &entry.flow {
        user["flow"] = json!(flow);
    }
    let config_json = json!({
        "log": { "loglevel": "info" },
        "inbounds": [{
            "tag": "in",
            "listen": "127.0.0.1",
            "port": 10800,
            "protocol": "socks",
            "settings": { "auth": "noauth", "udp": false }
        }],
        "outbounds": [{
            "tag": "out",
            "protocol": "vless",
            "settings": {
                "vnext": [{
                    "address": entry.host,
                    "port": entry.port,
                    "users": [user]
                }]
            },
            "streamSettings": {
                "network": "tcp",
                "security": "reality",
                "realitySettings": {
                    "serverName": entry.sni,
                    "fingerprint": entry.fingerprint,
                    "publicKey": entry.public_key,
                    "shortId": entry.short_id
                }
            }
        }]
    });
    format!(
        r#"cat > /tmp/brocade-sub-probe.json <<'JSON'
{config_json}
JSON
pkill -x xray 2>/dev/null || true
for i in $(seq 1 40); do pgrep -x xray >/dev/null 2>&1 || break; sleep 0.25; done
xray run -config /tmp/brocade-sub-probe.json >/tmp/brocade-sub-probe.log 2>&1 &
for i in $(seq 1 40); do ss -ltn | grep -q '127.0.0.1:10800' && break; sleep 0.25; done
OUT=$(curl -s --max-time 10 --socks5-hostname 127.0.0.1:10800 http://{echo_ip}/ || true)
pkill -x xray 2>/dev/null || true
printf '{BODY_MARKER}%s\n{LOG_MARKER}\n' "$OUT"
tail -80 /tmp/brocade-sub-probe.log 2>/dev/null || true
"#,
        config_json = serde_json::to_string_pretty(&config_json).unwrap_or_default(),
        echo_ip = echo_ipv4(config)
    )
}

// The probe script prints curl's response body and xray's log to one stream, separated by these
// two markers.
const BODY_MARKER: &str = "__OUT__";
const LOG_MARKER: &str = "__LOG__";

fn split_probe_output(output: &str) -> (&str, &str) {
    let body = output
        .split(LOG_MARKER)
        .next()
        .unwrap_or("")
        .trim()
        .strip_prefix(BODY_MARKER)
        .unwrap_or("")
        .trim();
    let log = output.split(LOG_MARKER).nth(1).unwrap_or("").trim();
    (body, log)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_probe_output_separates_the_response_body_from_the_xray_log() {
        let (body, log) =
            split_probe_output("__OUT__{\"source\":\"172.31.90.11\"}\n__LOG__\nstarting xray\n");

        assert_eq!(body, "{\"source\":\"172.31.90.11\"}");
        assert_eq!(log, "starting xray");
    }

    #[test]
    fn split_probe_output_yields_an_empty_body_when_curl_produced_nothing() {
        let (body, log) = split_probe_output("__OUT__\n__LOG__\nfailed to dial\n");

        assert_eq!(body, "");
        assert_eq!(log, "failed to dial");
    }

    #[test]
    fn probe_client_installs_the_control_plane_xray_with_its_digest() {
        let script = probe_xray_install_script(&Config::for_test());

        assert!(script.contains("curl -fsSL 'http://host.docker.internal:8081'/enroll/dist"));
        assert!(script.contains("xray_bin_url_$XRAY_ARCH"));
        assert!(script.contains("xray_bin_sha256_$XRAY_ARCH"));
        assert!(script.contains("sha256sum -c -"));
        assert!(script.contains("install -m 0755"));
    }

    #[test]
    fn probe_client_prefers_an_explicit_xray_distribution() {
        let mut config = Config::for_test();
        config.xray_bin_url = Some("https://example.test/brocade xray".to_owned());
        config.xray_bin_sha256 = Some("abc123".to_owned());

        let script = probe_xray_install_script(&config);

        assert!(script.contains("XRAY_URL='https://example.test/brocade xray'"));
        assert!(script.contains("XRAY_SHA256='abc123'"));
    }
}
