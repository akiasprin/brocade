use crate::artifacts::subscription::{
    Subscription, SubscriptionEntry, SubscriptionSecurity, SubscriptionStream,
};
use crate::model::{XhttpTuning, XhttpXmux, XhttpXmuxRange};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UriRenderOptions {
    /// Explicit, one-request opt-in from the connection-address dialog. It is never used by a
    /// public subscription URL and never persisted.
    pub allow_insecure: bool,
}

pub fn subscription(subscription: &Subscription) -> String {
    subscription_with_options(subscription, UriRenderOptions::default())
}

pub fn subscription_with_options(subscription: &Subscription, options: UriRenderOptions) -> String {
    let mut lines = Vec::new();
    let mut skipped = Vec::new();
    let mut hidden_self_signed = Vec::new();
    let mut unsupported_self_signed = Vec::new();

    for entry in &subscription.entries {
        if entry.front_name.is_some() {
            skipped.push(entry.name.clone());
            continue;
        }
        let self_signed = match &entry.security {
            SubscriptionSecurity::Tls(tls) => tls.self_signed,
            SubscriptionSecurity::AnyTls(anytls) => anytls.self_signed,
            SubscriptionSecurity::Hysteria2(hysteria) => hysteria.self_signed,
            SubscriptionSecurity::Reality(_) => false,
        };
        if self_signed {
            // VLESS has no interoperable URI field for a self-signed certificate pin, and current Xray
            // rejects allowInsecure. Never emit a link that imports successfully and cannot dial.
            if matches!(&entry.security, SubscriptionSecurity::Tls(_)) {
                unsupported_self_signed.push(entry.name.clone());
                continue;
            }
            if !options.allow_insecure {
                hidden_self_signed.push(entry.name.clone());
                continue;
            }
        }
        lines.push(entry_uri(entry, options));
    }

    if lines.is_empty() {
        lines.push("# （这个用户没有能用纯 URI 表达的接入面）".to_owned());
    }
    if !skipped.is_empty() {
        lines.push(String::new());
        lines.push("# 以下条目带前置代理，URI 列表表达不了，已跳过：".to_owned());
        lines.push(format!("# {}", skipped.join("、")));
        lines.push("# 换 Clash 目标即可。".to_owned());
    }
    if !hidden_self_signed.is_empty() {
        lines.push(String::new());
        lines.push(
            "# 以下自签证书地址默认隐藏；在连接地址窗口明确允许 insecure 后才会显示：".to_owned(),
        );
        lines.push(format!("# {}", hidden_self_signed.join("、")));
    }
    if !unsupported_self_signed.is_empty() {
        lines.push(String::new());
        lines.push(
            "# 以下 VLESS 自签入口没有可靠的跨客户端 URI 表达，已跳过；请使用 Clash 订阅："
                .to_owned(),
        );
        lines.push(format!("# {}", unsupported_self_signed.join("、")));
    }

    format!("{}\n", lines.join("\n"))
}

fn entry_uri(entry: &SubscriptionEntry, options: UriRenderOptions) -> String {
    if let SubscriptionSecurity::AnyTls(anytls) = &entry.security {
        return anytls_uri(entry, anytls, options);
    }
    if let SubscriptionSecurity::Hysteria2(hysteria) = &entry.security {
        return hysteria2_uri(entry, hysteria, options);
    }
    // `type` names the network layer, and it is the field a client uses to decide how to dial.
    // XHTTP additionally needs the path: the server matches it and refuses anything else, so a
    // URI missing it imports cleanly and never connects.
    let (kind, path, host, download, xmux, tuning, mode) = match &entry.stream {
        SubscriptionStream::Tcp => ("tcp", None, None, None, None, None, None),
        SubscriptionStream::Xhttp {
            path,
            host,
            download,
            xmux,
            tuning,
            mode,
        } => (
            "xhttp",
            Some(path.as_str()),
            host.as_deref(),
            download.as_ref(),
            xmux.as_ref(),
            tuning.as_ref(),
            *mode,
        ),
    };
    let mut query = vec![("encryption", "none".to_owned()), ("type", kind.to_owned())];
    // `pbk` and `sid` exist only under REALITY: they are the borrowed site's proof, and a client
    // handed them alongside `security=tls` has been told two contradictory things about what it
    // is connecting to. `sni` and `fp` mean something under both, but not the same thing — under
    // TLS the name is checked against a certificate, under REALITY it is only a label.
    let flow = match &entry.security {
        SubscriptionSecurity::Reality(reality) => {
            query.push(("security", "reality".to_owned()));
            query.push(("sni", reality.server_name.clone()));
            query.push(("fp", reality.fingerprint.clone()));
            query.push(("pbk", reality.public_key.clone()));
            query.push(("sid", reality.short_id.clone()));
            reality.flow.clone()
        }
        SubscriptionSecurity::Tls(tls) => {
            query.push(("security", "tls".to_owned()));
            query.push(("sni", tls.server_name.clone()));
            tls.flow.clone()
        }
        SubscriptionSecurity::AnyTls(_) | SubscriptionSecurity::Hysteria2(_) => {
            unreachable!("AnyTLS / Hysteria 已在上方单独渲染")
        }
    };
    if let Some(flow) = &flow {
        query.push(("flow", flow.clone()));
    }
    if let Some(path) = path {
        query.push(("path", path.to_owned()));
    }
    if let Some(host) = host {
        query.push(("host", host.to_owned()));
    }
    // Everything XHTTP beyond `path`, `host` and `mode` rides in `extra`, and it has to be all of
    // it rather than some of it. `SplitHTTPConfig::Build` (`infra/conf/transport_method.go`)
    // unmarshals `extra` into a *fresh* config, copies exactly those three keys back onto it and
    // then does `c = &extra` — so anything written beside `extra` instead of inside it is dropped
    // without a word.
    //
    // A scalar `mux=` is not the alternative for the concurrency: in VLESS share links that name
    // is widely read as the outer mux.cool switch, which XHTTP must not turn on. `extra` is what
    // both client families actually parse — xray's own config builder above, and mihomo's link
    // converter, which maps `extra.xmux.maxConcurrency` onto its `reuse-settings.max-concurrency`
    // (`parseXHTTPExtra`, `common/convert/v.go`).
    if let Some(path) = path {
        let mut extra = serde_json::Map::new();
        if let Some(xmux) = xmux {
            extra.insert("xmux".to_owned(), xmux_json(xmux));
        }
        if let Some(tuning) = tuning {
            extra.extend(xhttp_tuning_json(tuning));
        }
        if let Some(download) = download {
            let mut down_xhttp = serde_json::json!({
                "host": download.http_host.as_deref().unwrap_or(&download.server_name),
                "path": path,
            });
            if let Some(concurrency) = download.mux {
                down_xhttp["xmux"] = xmux_json(&XhttpXmux::with_concurrency(concurrency));
            }
            let tls_settings = serde_json::json!({
                "serverName": download.server_name,
            });
            extra.insert(
                "downloadSettings".to_owned(),
                serde_json::json!({
                    "address": download.server,
                    "port": download.port,
                    "network": "xhttp",
                    "security": "tls",
                    "tlsSettings": tls_settings,
                    "xhttpSettings": down_xhttp,
                }),
            );
        }
        if !extra.is_empty() {
            query.push(("extra", serde_json::Value::Object(extra).to_string()));
        }
    }
    // The upload shape stays a scalar: it is one of the three keys `extra` cannot swallow, and it
    // has to travel — a server told to expect one shape refuses every client that guesses
    // another. Omitted when the operator chose nothing, so both ends resolve it themselves.
    if let Some(mode) = mode {
        query.push(("mode", mode.to_owned()));
    }

    let query = query
        .into_iter()
        .map(|(key, value)| format!("{key}={}", pct_encode(&value)))
        .collect::<Vec<_>>()
        .join("&");

    format!(
        "vless://{}@{}:{}?{}#{}",
        pct_encode(&entry.uuid),
        uri_host(&entry.server),
        entry.port,
        query,
        pct_encode(&entry.name)
    )
}

fn anytls_uri(
    entry: &SubscriptionEntry,
    anytls: &crate::artifacts::subscription::SubscriptionAnyTls,
    options: UriRenderOptions,
) -> String {
    // There is no upstream Xray share-link parser for AnyTLS. Keep the URI deliberately small
    // and interoperable with clients that use the conventional anytls:// form; the server-side
    // padding scheme is negotiated after the TLS session starts and masquerade is server-only.
    let mut query = vec![("sni", anytls.server_name.clone())];
    if let Some(reality) = &anytls.reality {
        query.extend([
            ("security", "reality".to_owned()),
            ("fp", reality.fingerprint.clone()),
            ("pbk", reality.public_key.clone()),
            ("sid", reality.short_id.clone()),
        ]);
    } else if anytls.self_signed && options.allow_insecure {
        query.push(("insecure", "1".to_owned()));
    }
    let query = query
        .into_iter()
        .map(|(key, value)| format!("{key}={}", pct_encode(&value)))
        .collect::<Vec<_>>()
        .join("&");
    format!(
        "anytls://{}@{}:{}?{}#{}",
        pct_encode(&entry.uuid),
        uri_host(&entry.server),
        entry.port,
        query,
        pct_encode(&entry.name)
    )
}

fn xmux_json(xmux: &XhttpXmux) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    if let Some(concurrency) = xmux.max_concurrency {
        value.insert("maxConcurrency".to_owned(), serde_json::json!(concurrency));
    }
    if let Some(connections) = xmux.max_connections {
        value.insert("maxConnections".to_owned(), serde_json::json!(connections));
    }
    value.insert(
        "hMaxRequestTimes".to_owned(),
        xray_range(&xmux.h_max_request_times),
    );
    value.insert(
        "hMaxReusableSecs".to_owned(),
        xray_range(&xmux.h_max_reusable_secs),
    );
    if let Some(period) = xmux.h_keep_alive_period_secs {
        value.insert("hKeepAlivePeriod".to_owned(), serde_json::json!(period));
    }
    serde_json::Value::Object(value)
}

fn xhttp_tuning_json(tuning: &XhttpTuning) -> serde_json::Map<String, serde_json::Value> {
    let mut value = serde_json::Map::new();
    if let Some(range) = &tuning.x_padding_bytes {
        value.insert("xPaddingBytes".to_owned(), xray_range(range));
    }
    value
}

fn xray_range(range: &XhttpXmuxRange) -> serde_json::Value {
    if range.from == range.to {
        serde_json::json!(range.from)
    } else {
        serde_json::json!(format!("{}-{}", range.from, range.to))
    }
}

fn hysteria2_uri(
    entry: &SubscriptionEntry,
    hysteria: &crate::artifacts::subscription::SubscriptionHysteria2,
    options: UriRenderOptions,
) -> String {
    // `insecure` is emitted only by the explicit one-request escape hatch. The durable model and
    // ordinary subscription output never weaken certificate verification.
    let mut query = vec![("sni", hysteria.server_name.clone())];
    if hysteria.self_signed && options.allow_insecure {
        query.push(("insecure", "1".to_owned()));
    }
    if let Some(crate::model::HysteriaObfs::Salamander { password }) = &hysteria.settings.obfs {
        query.push(("obfs", "salamander".to_owned()));
        query.push(("obfs-password", password.clone()));
    }
    let query = query
        .into_iter()
        .map(|(key, value)| format!("{key}={}", pct_encode(&value)))
        .collect::<Vec<_>>()
        .join("&");
    // Port hopping is spelled in the port component, not as a query parameter — the scheme's
    // "multi-port format" (`hysteria2://auth@host:5000-6000/`), checked against the upstream URI
    // scheme document rather than recalled. A client that does not understand the range sees a
    // malformed port and refuses the entry, which is the loud failure; inventing a query
    // parameter for it would instead be dropped in silence by every client.
    let port = match &hysteria.settings.hop {
        Some(hop) => format!("{}-{}", hop.start, hop.end),
        None => entry.port.to_string(),
    };
    format!(
        "hysteria2://{}@{}:{}/?{}#{}",
        pct_encode(&entry.uuid),
        uri_host(&entry.server),
        port,
        query,
        pct_encode(&entry.name)
    )
}

fn uri_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

fn pct_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
