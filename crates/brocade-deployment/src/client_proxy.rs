//! Pure client rendering shared by subscriber-path probes and managed clients.
//! Runtime process and network ownership remain with the caller.
use crate::protocol::{E2eProbeSecurity, E2eProbeTarget};

/// An ordinary xray client config: socks in, vless out.
///
/// Its fields come from strictly the same source as the ones handed to users in a
/// subscription (projected by the control plane via `physical::probe`). Any
/// parameter that diverges measures the health of a different path — and presents
/// it as "the chain is fine", which is worse than not probing at all. That is why both
/// `stream_settings` below branch on what the target says rather than on a default: this
/// dialed plain TCP at every target once, and an ingress carried inside HTTP was refused at
/// the server's path check and reported down while it was carrying traffic perfectly well.
pub fn client_config(target: &E2eProbeTarget, socks_port: u16, log_path: &str) -> String {
    match &target.security {
        E2eProbeSecurity::Hysteria2(hysteria) => {
            return hysteria_client_config(target, hysteria, socks_port, log_path);
        }
        E2eProbeSecurity::AnyTls { settings, reality } => {
            return anytls_client_config(target, settings, reality.as_ref(), socks_port, log_path);
        }
        _ => {}
    }
    let (encryption, flow) = match &target.security {
        E2eProbeSecurity::VlessEncryption { encryption } => (encryption.as_str(), None),
        E2eProbeSecurity::Reality(reality) => ("none", reality.flow.as_deref()),
        E2eProbeSecurity::Tls(tls) => ("none", tls.flow.as_deref()),
        E2eProbeSecurity::AnyTls { .. } | E2eProbeSecurity::Hysteria2(_) => unreachable!(),
    };
    let mut settings = serde_json::json!({
        "vnext": [{
            "address": target.dial_host,
            "port": target.port,
            "users": [{
                "id": target.uuid,
                "encryption": encryption,
            }],
        }],
    });
    if let Some(flow) = flow {
        settings["vnext"][0]["users"][0]["flow"] = serde_json::json!(flow);
    }

    let config = serde_json::json!({
        // Logs go to a file rather than a pipe: xray writes `[Info]` to stdout, so
        // capturing stderr alone yields nothing, and a full pipe wedges the probe
        // process in write, which looks exactly like a dead chain. Level info,
        // not error — a failed REALITY handshake and rejected credentials are
        // both [Info]. Access logs off: the probe makes one connection and it can
        // say nothing the error log does not.
        "log": {
            "loglevel": "info",
            "error": log_path,
            "access": "none",
        },
        "inbounds": [{
            "tag": "probe-in",
            // Loopback only. Exposed, the probe port is a proxy anyone can use
            // without credentials.
            "listen": "127.0.0.1",
            "port": socks_port,
            "protocol": "socks",
            "settings": { "auth": "noauth", "udp": false },
        }],
        "outbounds": [{
            "tag": "probe-out",
            "protocol": "vless",
            "settings": settings,
            "streamSettings": stream_settings(target),
        }],
    });
    serde_json::to_string(&config).unwrap_or_default()
}

fn anytls_client_config(
    target: &E2eProbeTarget,
    anytls: &crate::protocol::E2eProbeAnyTls,
    reality: Option<&crate::protocol::E2eProbeReality>,
    socks_port: u16,
    log_path: &str,
) -> String {
    let mut settings = serde_json::json!({
        "address": target.dial_host,
        "port": target.port,
        "password": target.uuid,
    });
    if let Some(value) = anytls.idle_session_check_interval_secs {
        settings["idleSessionCheckInterval"] = serde_json::json!(value);
    }
    if let Some(value) = anytls.idle_session_timeout_secs {
        settings["idleSessionTimeout"] = serde_json::json!(value);
    }
    if let Some(value) = anytls.min_idle_session {
        settings["minIdleSession"] = serde_json::json!(value);
    }
    let mut stream_settings = if let Some(reality) = reality {
        serde_json::json!({
            "security": "reality",
            "realitySettings": {
                "serverName": reality.server_name,
                "fingerprint": reality.fingerprint,
                "publicKey": reality.public_key,
                "shortId": reality.short_id,
            },
        })
    } else {
        let mut tls_settings = serde_json::json!({
            "serverName": anytls.server_name,
        });
        if let Some(pin) = &anytls.pinned_peer_cert_sha256 {
            tls_settings["pinnedPeerCertSha256"] = serde_json::json!(pin);
        }
        serde_json::json!({
            "security": "tls",
            "tlsSettings": tls_settings,
        })
    };
    stream_settings["sockopt"] = serde_json::json!({ "tcpFastOpen": true });
    let config = serde_json::json!({
        "log": {
            "loglevel": "info",
            "error": log_path,
            "access": "none",
        },
        "inbounds": [{
            "tag": "probe-in",
            "listen": "127.0.0.1",
            "port": socks_port,
            "protocol": "socks",
            "settings": { "auth": "noauth", "udp": false },
        }],
        "outbounds": [{
            "tag": "probe-out",
            "protocol": "anytls",
            "settings": settings,
            "streamSettings": stream_settings,
        }],
    });
    serde_json::to_string(&config).unwrap_or_default()
}

fn hysteria_client_config(
    target: &E2eProbeTarget,
    hysteria: &crate::protocol::E2eProbeHysteria2,
    socks_port: u16,
    log_path: &str,
) -> String {
    let mut quic = serde_json::Map::new();
    quic.insert(
        "congestion".to_owned(),
        serde_json::json!(hysteria.congestion),
    );
    if hysteria.congestion != "bbr" {
        if let Some(up) = &hysteria.up {
            quic.insert("brutalUp".to_owned(), serde_json::json!(up));
        }
        if let Some(down) = &hysteria.down {
            quic.insert("brutalDown".to_owned(), serde_json::json!(down));
        }
    }
    if let Some(profile) = &hysteria.bbr_profile {
        quic.insert("bbrProfile".to_owned(), serde_json::json!(profile));
    }
    for (key, value) in [
        (
            "initStreamReceiveWindow",
            hysteria.init_stream_receive_window,
        ),
        ("maxStreamReceiveWindow", hysteria.max_stream_receive_window),
        (
            "initConnectionReceiveWindow",
            hysteria.init_connection_receive_window,
        ),
        (
            "maxConnectionReceiveWindow",
            hysteria.max_connection_receive_window,
        ),
    ] {
        if let Some(value) = value {
            quic.insert(key.to_owned(), serde_json::json!(value));
        }
    }
    for (key, value) in [
        ("maxIdleTimeout", hysteria.max_idle_timeout_secs),
        ("keepAlivePeriod", hysteria.keep_alive_period_secs),
    ] {
        if let Some(value) = value {
            quic.insert(key.to_owned(), serde_json::json!(value));
        }
    }
    if hysteria.disable_path_mtu_discovery {
        quic.insert(
            "disablePathMTUDiscovery".to_owned(),
            serde_json::json!(true),
        );
    }
    let mut finalmask = serde_json::Map::new();
    finalmask.insert("quicParams".to_owned(), serde_json::Value::Object(quic));
    if let Some(password) = &hysteria.salamander_password {
        finalmask.insert(
            "udp".to_owned(),
            serde_json::json!([{
                "type": "salamander",
                "settings": { "password": password },
            }]),
        );
    }
    let mut tls_settings = serde_json::json!({
        "serverName": hysteria.server_name,
    });
    if let Some(pin) = &hysteria.pinned_peer_cert_sha256 {
        tls_settings["pinnedPeerCertSha256"] = serde_json::json!(pin);
    }
    let config = serde_json::json!({
        "log": {
            "loglevel": "info",
            "error": log_path,
            "access": "none",
        },
        "inbounds": [{
            "tag": "probe-in",
            "listen": "127.0.0.1",
            "port": socks_port,
            "protocol": "socks",
            "settings": { "auth": "noauth", "udp": false },
        }],
        "outbounds": [{
            "tag": "probe-out",
            "protocol": "hysteria",
            "settings": {
                "version": 2,
                "address": target.dial_host,
                "port": target.port,
            },
            "streamSettings": {
                "network": "hysteria",
                "security": "tls",
                "tlsSettings": tls_settings,
                "hysteriaSettings": {
                    "version": 2,
                    "auth": target.uuid,
                },
                "finalmask": serde_json::Value::Object(finalmask),
            },
        }],
    });
    serde_json::to_string(&config).unwrap_or_default()
}

/// The client half of what the ingress's own artifact says, assembled from the two axes the
/// target names: which certificate to expect, and whether the stream is carried inside HTTP.
pub fn stream_settings(target: &E2eProbeTarget) -> serde_json::Value {
    if matches!(&target.security, E2eProbeSecurity::VlessEncryption { .. }) {
        return serde_json::json!({ "network": "tcp", "security": "none" });
    }
    let mut settings = match &target.security {
        // A certificate of the machine's own: nothing to configure but the name, which the
        // client checks the certificate against — so a wrong one fails here rather than at the
        // far end.
        // Managed ingress probes keep certificate verification enabled. Self-signed certificates
        // carry their pinned digest in the frozen target; external tunnel opt-ins do not apply
        // to this independently generated client config.
        E2eProbeSecurity::Tls(tls) => {
            let mut tls_settings = serde_json::json!({
                "serverName": tls.server_name,
            });
            if let Some(pin) = &tls.pinned_peer_cert_sha256 {
                tls_settings["pinnedPeerCertSha256"] = serde_json::json!(pin);
            }
            serde_json::json!({
                "security": "tls",
                "tlsSettings": tls_settings,
            })
        }
        E2eProbeSecurity::Reality(reality) => serde_json::json!({
            "security": "reality",
            "realitySettings": {
                "serverName": reality.server_name,
                "fingerprint": reality.fingerprint,
                "publicKey": reality.public_key,
                "shortId": reality.short_id,
            },
        }),
        E2eProbeSecurity::VlessEncryption { .. }
        | E2eProbeSecurity::AnyTls { .. }
        | E2eProbeSecurity::Hysteria2(_) => unreachable!(),
    };
    match &target.xhttp {
        None => settings["network"] = serde_json::json!("tcp"),
        Some(xhttp) => {
            settings["network"] = serde_json::json!("xhttp");
            let mut options = serde_json::json!({ "path": xhttp.path });
            if let Some(host) = &xhttp.host {
                options["host"] = serde_json::json!(host);
            }
            if let Some(xmux) = &xhttp.xmux {
                let mut value = serde_json::Map::new();
                if let Some(concurrency) = xmux.max_concurrency {
                    value.insert("maxConcurrency".to_owned(), serde_json::json!(concurrency));
                }
                if let Some(connections) = xmux.max_connections {
                    value.insert("maxConnections".to_owned(), serde_json::json!(connections));
                }
                value.insert(
                    "hMaxRequestTimes".to_owned(),
                    xhttp_range(&xmux.h_max_request_times),
                );
                value.insert(
                    "hMaxReusableSecs".to_owned(),
                    xhttp_range(&xmux.h_max_reusable_secs),
                );
                if let Some(period) = xmux.h_keep_alive_period_secs {
                    value.insert("hKeepAlivePeriod".to_owned(), serde_json::json!(period));
                }
                options["xmux"] = serde_json::Value::Object(value);
            }
            if let Some(range) = &xhttp.x_padding_bytes {
                options["xPaddingBytes"] = xhttp_range(range);
            }
            // Absent means both ends resolve it the same way by themselves. Named, it has to
            // match: a server told to expect one upload shape refuses every client naming another.
            if let Some(mode) = &xhttp.mode {
                options["mode"] = serde_json::json!(mode);
            }
            settings["xhttpSettings"] = options;
        }
    }
    settings
}

fn xhttp_range(range: &crate::protocol::E2eProbeXhttpRange) -> serde_json::Value {
    if range.from == range.to {
        serde_json::json!(range.from)
    } else {
        serde_json::json!(format!("{}-{}", range.from, range.to))
    }
}
