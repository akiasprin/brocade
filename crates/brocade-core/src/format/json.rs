use std::net::IpAddr;

use serde_json::{json, Map, Value};

use crate::artifacts::{
    grants::{GrantClient, GrantSyncBatch},
    phantun::{PhantunArtifact, PhantunConfig},
    xray::{
        XrayArtifact, XrayConfig, XrayDnsServer, XrayHopInboundWire, XrayHopOutboundWire,
        XrayInbound, XrayIngressSecurity, XrayMatchCondition, XrayMux, XrayOutbound, XrayPolicy,
        XrayRoutingRule, XrayStream,
    },
};
use crate::model::{
    AnyTlsMasquerade, ExternalOutboundProtocol, ExternalOutboundSecurity, ExternalVlessTransport,
    ExternalVlessXhttp, ExternalVlessXhttpDownload, HysteriaBbrProfile, HysteriaCongestion,
    HysteriaMasquerade, HysteriaObfs, XhttpTuning, XhttpXmux, XhttpXmuxRange,
};

/// Backlog offered by TCP listeners for TCP Fast Open requests.
///
/// Relay peers and subscriber endpoints have stable addresses and ports, so subsequent
/// connections can carry the protocol header and first application bytes in the SYN instead of
/// paying another TCP RTT. The kernel-wide `net.ipv4.tcp_fastopen` switch still controls whether
/// the socket option takes effect. A peer or path that does not support TFO falls back to the
/// ordinary handshake.
const TCP_FAST_OPEN_BACKLOG: u16 = 256;

pub fn xray(artifact: &XrayArtifact) -> String {
    let value = match artifact {
        XrayArtifact::Disabled { .. } => json!({}),
        XrayArtifact::Config(config) => xray_config(config),
    };
    format!("{}\n", serde_json::to_string_pretty(&value).unwrap())
}

/// The phantun instance table. The agent starts processes from it; it is not a
/// config file phantun reads itself — phantun takes command-line arguments only, so
/// this says which instances to start and with which arguments.
///
/// Key order is fixed (the Map `serde_json::json!` builds is a BTreeMap), which is
/// what makes byte-for-byte comparison meaningful.
pub fn phantun(artifact: &PhantunArtifact) -> String {
    let value = match artifact {
        PhantunArtifact::Disabled { .. } => json!({}),
        PhantunArtifact::Config(config) => phantun_config(config),
    };
    format!("{}\n", serde_json::to_string_pretty(&value).unwrap())
}

fn tun_json(tun: &crate::artifacts::phantun::Tun) -> Value {
    json!({
        "name": tun.name,
        "local": tun.local.to_string(),
        "peer": tun.peer.to_string(),
    })
}

fn phantun_config(config: &PhantunConfig) -> Value {
    let mut root = Map::new();
    // `servers` is always present (possibly an empty array), unlike the old format
    // where an absent server meant an absent key. One machine may host a server for
    // each of several peers behind NAT, which does not fit in a single object.
    root.insert(
        "servers".to_owned(),
        Value::Array(
            config
                .servers
                .iter()
                .map(|server| {
                    json!({
                        "tcp_port": server.tcp_port,
                        "forward_to_udp_port": server.forward_to_udp_port,
                        "peers": server.peers,
                        "tun": tun_json(&server.tun),
                    })
                })
                .collect(),
        ),
    );
    root.insert(
        "clients".to_owned(),
        Value::Array(
            config
                .clients
                .iter()
                .map(|client| {
                    json!({
                        "peer": client.peer_node_id,
                        "listen_udp_port": client.listen_udp_port,
                        "remote_tcp_endpoint": client.remote_tcp_endpoint,
                        "tun": tun_json(&client.tun),
                    })
                })
                .collect(),
        ),
    );
    Value::Object(root)
}

pub fn hy2_port_hop(artifact: &crate::artifacts::hy2_port_hop::Hy2PortHopArtifact) -> String {
    use crate::artifacts::hy2_port_hop::Hy2PortHopArtifact;
    let value = match artifact {
        // An empty object rather than an empty list: the agent reads "no ranges here, take the
        // table down", and a machine that never had one converges to the same content either way.
        Hy2PortHopArtifact::Disabled { .. } => json!({}),
        Hy2PortHopArtifact::Config(config) => json!({
            "node_id": config.node_id,
            "redirects": config.redirects.iter().map(|redirect| json!({
                "start": redirect.start,
                "end": redirect.end,
                "to": redirect.to,
            })).collect::<Vec<_>>(),
        }),
    };
    format!("{}\n", serde_json::to_string_pretty(&value).unwrap())
}

pub fn grant_sync_batch(batch: &GrantSyncBatch) -> String {
    let value = json!({
        "inbounds": batch.inbounds.iter().map(|inbound| {
            json!({
                "tag": inbound.inbound_tag,
                "clients": inbound.clients.iter().map(grant_client).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>()
    });
    format!("{}\n", serde_json::to_string_pretty(&value).unwrap())
}

fn xray_config(config: &XrayConfig) -> Value {
    let mut value = json!({
        "log": { "loglevel": config.log_level },
        "stats": {},
        "api": {
            "tag": config.api.tag,
            "services": config.api.services,
        },
        "policy": policy_config(&config.policy),
        "dns": dns_config(config),
        "inbounds": config.inbounds.iter().map(inbound).collect::<Vec<_>>(),
        "outbounds": config.outbounds.iter().map(outbound).collect::<Vec<_>>(),
        "routing": routing_config(config),
        // Older xray does not know this key and silently skips it (verified
        // `Configuration OK` on v26.3.27), so it can be written unconditionally —
        // newer versions use it, older ones do not see it.
        "geodata": {
            "cron": config.geodata.cron,
            "outbound": config.geodata.outbound,
            "assets": config.geodata.assets.iter()
                .map(|a| json!({ "url": a.url, "file": a.file }))
                .collect::<Vec<_>>(),
        },
    });
    // No top-level `reverse` block: xray removed it (`infra/conf/xray.go` returns
    // "The feature legacy reverse has been removed" the moment the key is present, so
    // writing it does not degrade — it stops the node's xray from starting at all).
    // Both ends now ride on the credential; see the client and outbound renderers.
    if let Some(health) = &config.hop_health {
        // `burstObservatory` is the key this version knows, and it only starts once
        // a rule references the balancer — writing this section alone is not enough
        // (`XrayRouting::balancers`).
        value.as_object_mut().unwrap().insert(
            "burstObservatory".to_owned(),
            json!({
                "subjectSelector": health.subjects,
                "pingConfig": {
                    "destination": health.probe_url,
                    "interval": format!("{}s", health.probe_interval_secs),
                    "timeout": "5s",
                    "samplingCount": 2,
                },
            }),
        );
    }
    value
}

fn routing_config(config: &XrayConfig) -> Value {
    let mut routing = Map::new();
    routing.insert(
        "domainStrategy".to_owned(),
        json!(config.routing.domain_strategy),
    );
    if !config.routing.balancers.is_empty() {
        routing.insert(
            "balancers".to_owned(),
            json!(config
                .routing
                .balancers
                .iter()
                .map(|balancer| json!({
                    "tag": balancer.tag,
                    "selector": balancer.selector,
                    // leastPing is the only strategy that consumes the observatory's
                    // results. We do not read its choice (the counters are the
                    // test), but the strategy must match or the observatory runs for
                    // nothing.
                    "strategy": { "type": "leastPing" },
                }))
                .collect::<Vec<_>>()),
        );
    }
    routing.insert(
        "rules".to_owned(),
        json!(config
            .routing
            .rules
            .iter()
            .map(routing_rule)
            .collect::<Vec<_>>()),
    );
    Value::Object(routing)
}

fn grant_client(client: &GrantClient) -> Value {
    let mut object = Map::new();
    object.insert("id".to_owned(), json!(client.uuid));
    object.insert("email".to_owned(), json!(client.label));
    if let Some(flow) = &client.flow {
        object.insert("flow".to_owned(), json!(flow));
    }
    object.insert("level".to_owned(), json!(client.level));
    Value::Object(object)
}

fn dns_config(config: &XrayConfig) -> Value {
    let mut object = Map::new();
    if let Some(tag) = &config.dns.tag {
        object.insert("tag".to_owned(), json!(tag));
    }
    object.insert(
        "servers".to_owned(),
        Value::Array(
            config
                .dns
                .servers
                .iter()
                .map(|server| match server {
                    XrayDnsServer::Address(address) => json!(address),
                    XrayDnsServer::Scoped {
                        address,
                        port,
                        domains,
                        query_strategy,
                        tag,
                        final_query,
                    } => json!({
                        "address": address,
                        "port": port,
                        "domains": domains,
                        "queryStrategy": query_strategy,
                        "skipFallback": true,
                        "finalQuery": final_query,
                        "tag": tag,
                    }),
                })
                .collect(),
        ),
    );
    Value::Object(object)
}

fn inbound(inbound: &XrayInbound) -> Value {
    match inbound {
        XrayInbound::BackboneShadowsocks {
            tag,
            listen,
            port,
            method,
            password,
            users,
        } => json!({
            "tag": tag,
            "listen": listen,
            "port": port,
            "protocol": "shadowsocks",
            "settings": {
                "method": method,
                "password": password,
                // Both, and stated rather than left out. xray reads an absent `network` as TCP
                // alone (`NetworkList::Build` returns TCP for nil), so silence here would drop
                // every UDP packet the chain carries — DNS and QUIC among them — while the
                // relay looked healthy and TCP kept working. VLESS needs no equivalent because
                // it tunnels UDP inside its own connection; shadowsocks dials the relay over
                // UDP for UDP, so the port has to be listening on it.
                "network": "tcp,udp",
                // `clients` rather than `users`, which xray accepts as the same thing
                // (`ShadowsocksServerConfig::Build` copies one onto the other). It is spelled
                // the way the VLESS inbounds above spell it, so somebody comparing two relay
                // ports in one file is not made to notice a difference that is not there.
                //
                // The accounts carry no method of their own. xray refuses one outright
                // ("users must have empty method") because the port already declared it, and
                // a second answer could only ever disagree.
                "clients": users.iter().map(|user| json!({
                    "email": user.email,
                    "password": user.password,
                })).collect::<Vec<_>>(),
            },
            "streamSettings": {
                "network": "tcp",
                "security": "none",
                "sockopt": { "tcpFastOpen": TCP_FAST_OPEN_BACKLOG },
            },
        }),
        XrayInbound::Api {
            tag,
            listen,
            port,
            address,
        } => json!({
            "tag": tag,
            "listen": listen,
            "port": port,
            "protocol": "dokodemo-door",
            "settings": { "address": address },
        }),
        XrayInbound::Vless {
            tag,
            listen,
            port,
            security,
            sniff,
            stream,
        } => {
            // Deliberately without `routeOnly`, which is the half of this block worth
            // explaining: it reads like the more restrained choice and is not.
            //
            // xray branches on it in one place (`app/dispatcher/default.go`):
            //
            //     if sniffingRequest.RouteOnly { ob.RouteTarget = destination }
            //     else                         { ob.Target = destination }
            //
            // Without it the sniffed name replaces the outbound's target, so the exit's
            // freedom outbound is handed a **domain** and resolves it itself. With it,
            // routing still sees the name but the target stays the address the client
            // supplied, and the exit connects to whatever the client's resolver returned.
            //
            // Which is wanted here is not a matter of taste. Subscribers sit behind
            // censorship equipment that answers their queries with forged addresses;
            // resolving at the exit is the entire point of carrying their traffic there.
            //
            // And it is load-bearing for a second reason that leaves no trace when broken.
            // `domainStrategy` on the freedom outbound (the machine's own choice, see
            // `model::DomainStrategy`) governs how a domain becomes an address — handed an
            // address there is nothing for it to govern. Turning `routeOnly` on therefore
            // switches that setting off for every sniffed connection while the artifact
            // still says `UseIP`, `xray -test` still passes, and the console still shows
            // the strategy the operator picked.
            let sniffing = if *sniff {
                json!({ "enabled": true, "destOverride": ["tls", "http", "quic"] })
            } else {
                json!({ "enabled": false })
            };
            let mut stream_settings = stream_settings(stream, security);
            enable_tcp_fast_open(&mut stream_settings, json!(TCP_FAST_OPEN_BACKLOG));
            json!({
                "tag": tag,
                "listen": listen,
                "port": port,
                "protocol": "vless",
                "settings": {
                    "clients": [],
                    "decryption": "none",
                },
                "streamSettings": stream_settings,
                "sniffing": sniffing,
            })
        }
        XrayInbound::Hysteria2 {
            tag,
            listen,
            port,
            security,
            sniff,
            settings,
        } => {
            let sniffing = if *sniff {
                json!({ "enabled": true, "destOverride": ["tls", "http", "quic"] })
            } else {
                json!({ "enabled": false })
            };
            json!({
                "tag": tag,
                "listen": listen,
                "port": port,
                "protocol": "hysteria",
                "settings": {
                    // Written even though the pinned v26.4.25 ignores it: that build parses the
                    // field and never looks at it, while v26.5.3 onward refuses the whole config
                    // with `version != 2` when it is missing. Omitting it would make the artifact
                    // valid on exactly one release and fail every machine on the next one — and
                    // an artifact whose correctness depends on which binary happens to be
                    // installed is no longer a function of the snapshot alone. Same reasoning as
                    // the `alpn` on the TLS side.
                    "version": 2,
                    // Empty here as everywhere: accounts are pushed into the running process,
                    // never written into a config file.
                    "clients": [],
                },
                "streamSettings": hysteria_stream_settings(settings, security),
                "sniffing": sniffing,
            })
        }
        XrayInbound::AnyTls {
            tag,
            listen,
            port,
            security,
            sniff,
            settings,
        } => {
            let sniffing = if *sniff {
                json!({ "enabled": true, "destOverride": ["tls", "http", "quic"] })
            } else {
                json!({ "enabled": false })
            };
            let mut anytls_stream_settings = stream_settings(&XrayStream::Tcp, security);
            enable_tcp_fast_open(&mut anytls_stream_settings, json!(TCP_FAST_OPEN_BACKLOG));
            json!({
                "tag": tag,
                "listen": listen,
                "port": port,
                "protocol": "anytls",
                "settings": {
                    "users": [],
                    "paddingScheme": settings.padding_scheme,
                    "masquerade": anytls_masquerade(&settings.masquerade),
                },
                "streamSettings": anytls_stream_settings,
                "sniffing": sniffing,
            })
        }
        XrayInbound::Dokodemo {
            tag,
            listen,
            port,
            target_port,
            security,
        } => json!({
            "tag": tag,
            "listen": listen,
            "port": port,
            "protocol": "dokodemo-door",
            "settings": {
                "address": "127.0.0.1",
                "port": target_port,
                "network": "tcp",
            },
            "streamSettings": stream_settings(&XrayStream::Tcp, security),
            "sniffing": { "enabled": false },
        }),
        XrayInbound::RealityGuard {
            tag,
            listen,
            port,
            target_address,
            target_port,
        } => json!({
            "tag": tag,
            "listen": listen,
            "port": port,
            "protocol": "dokodemo-door",
            "settings": {
                "address": target_address,
                "port": target_port,
                "network": "tcp",
            },
            // No security block: what crosses this door is the stranger's own TLS, addressed to
            // the borrowed site. Terminating it here is not possible — we hold no key for that
            // name — and is not wanted either.
            "streamSettings": { "network": "tcp", "security": "none" },
            // `tls` alone, and `routeOnly` on. See `XrayInbound::RealityGuard`: the sniff exists to
            // tell routing which name was asked for, and must not become the address dialled.
            "sniffing": {
                "enabled": true,
                "destOverride": ["tls"],
                "routeOnly": true,
            },
        }),
        XrayInbound::Backbone {
            tag,
            listen,
            port,
            security,
            clients,
        } => {
            // Each position states one thing: `decryption` is the protocol layer
            // (where VLESS Encryption lands) and `streamSettings.security` the
            // transport layer (where REALITY lands). Across the three settings only
            // one is ever other than none.
            let (decryption, mut stream_settings) = match security {
                XrayHopInboundWire::None => (
                    "none".to_owned(),
                    json!({ "network": "tcp", "security": "none" }),
                ),
                XrayHopInboundWire::Encryption { decryption } => (
                    decryption.clone(),
                    json!({ "network": "tcp", "security": "none" }),
                ),
                XrayHopInboundWire::Reality {
                    dest,
                    server_names,
                    private_key,
                    short_ids,
                } => (
                    "none".to_owned(),
                    json!({
                        "network": "tcp",
                        "security": "reality",
                        "realitySettings": {
                            "dest": dest,
                            "privateKey": private_key,
                            "serverNames": server_names,
                            "shortIds": short_ids,
                        },
                    }),
                ),
            };
            enable_tcp_fast_open(&mut stream_settings, json!(TCP_FAST_OPEN_BACKLOG));

            json!({
                "tag": tag,
                "listen": listen,
                "port": port,
                "protocol": "vless",
                "settings": {
                    "clients": clients.iter().map(|client| {
                        let mut object = Map::new();
                        object.insert("id".to_owned(), json!(client.id));
                        object.insert("email".to_owned(), json!(client.email));
                        object.insert("level".to_owned(), json!(client.level));
                        // Only the portal end carries this, so it is written only where
                        // present — an empty `reverse` object is not the same as none:
                        // xray rejects a blank tag (`"tag" can't be empty for reverse`).
                        if let Some(tag) = &client.reverse_tag {
                            object.insert("reverse".to_owned(), json!({ "tag": tag }));
                        }
                        Value::Object(object)
                    }).collect::<Vec<_>>(),
                    "decryption": decryption,
                },
                "streamSettings": stream_settings,
            })
        }
    }
}

fn hysteria_stream_settings(
    settings: &crate::model::Hysteria2,
    security: &XrayIngressSecurity,
) -> Value {
    let (security_name, settings_key, tls_settings) = ingress_security(security);
    let masquerade = match &settings.masquerade {
        HysteriaMasquerade::NotFound => json!({ "type": "404" }),
        HysteriaMasquerade::Proxy { url } => json!({
            "type": "proxy",
            "url": url,
            "rewriteHost": true,
        }),
    };
    let mut quic = Map::new();
    quic.insert("congestion".to_owned(), json!(settings.congestion.as_str()));
    /* `bbr` 与 `reno` 都不看带宽：前者自己估，后者根本没有目标速率这个概念。
    写进去的话 xray 会解析并忽略，而读产物的人得自己知道这一条。 */
    if !matches!(
        settings.congestion,
        HysteriaCongestion::Bbr | HysteriaCongestion::Reno
    ) {
        if let Some(up) = &settings.bandwidth.up {
            quic.insert("brutalUp".to_owned(), json!(up));
        }
        if let Some(down) = &settings.bandwidth.down {
            quic.insert("brutalDown".to_owned(), json!(down));
        }
    }
    /* 只在不是默认档时写。xray 把缺省的 `bbrProfile` 当作 `standard`，两者等价——
    写出来只会让每一份产物都多一行，而那一行不带信息。 */
    if settings.bbr_profile != HysteriaBbrProfile::default() {
        quic.insert(
            "bbrProfile".to_owned(),
            json!(settings.bbr_profile.as_str()),
        );
    }
    /* 同一条规矩：缺省不写，让 xray 用它自己那一版的默认值。把今天的默认值固化进产物，
    产物就不再跟着它部署的那个版本走了。 */
    let quic_params = &settings.quic;
    for (key, value) in [
        (
            "initStreamReceiveWindow",
            quic_params.init_stream_receive_window,
        ),
        (
            "maxStreamReceiveWindow",
            quic_params.max_stream_receive_window,
        ),
        (
            "initConnectionReceiveWindow",
            quic_params.init_connection_receive_window,
        ),
        (
            "maxConnectionReceiveWindow",
            quic_params.max_connection_receive_window,
        ),
    ] {
        if let Some(value) = value {
            quic.insert(key.to_owned(), json!(value));
        }
    }
    for (key, value) in [
        ("maxIdleTimeout", quic_params.max_idle_timeout_secs),
        ("keepAlivePeriod", quic_params.keep_alive_period_secs),
        ("maxIncomingStreams", quic_params.max_incoming_streams),
    ] {
        if let Some(value) = value {
            quic.insert(key.to_owned(), json!(value));
        }
    }
    if quic_params.disable_path_mtu_discovery {
        quic.insert("disablePathMTUDiscovery".to_owned(), json!(true));
    }
    let mut finalmask = Map::new();
    finalmask.insert("quicParams".to_owned(), Value::Object(quic));
    if let Some(HysteriaObfs::Salamander { password }) = &settings.obfs {
        finalmask.insert(
            "udp".to_owned(),
            json!([{
                "type": "salamander",
                "settings": { "password": password },
            }]),
        );
    }
    json!({
        "network": "hysteria",
        "security": security_name,
        settings_key: tls_settings,
        "hysteriaSettings": {
            "version": 2,
            "masquerade": masquerade,
        },
        "finalmask": Value::Object(finalmask),
    })
}

fn anytls_masquerade(masquerade: &AnyTlsMasquerade) -> Value {
    match masquerade {
        AnyTlsMasquerade::NotFound { headers } => {
            let mut value = Map::new();
            value.insert("type".to_owned(), json!("404"));
            if !headers.is_empty() {
                value.insert("headers".to_owned(), json!(headers));
            }
            Value::Object(value)
        }
        AnyTlsMasquerade::String {
            content,
            headers,
            status_code,
        } => {
            let mut value = Map::new();
            value.insert("type".to_owned(), json!("string"));
            value.insert("content".to_owned(), json!(content));
            value.insert("statusCode".to_owned(), json!(status_code));
            if !headers.is_empty() {
                value.insert("headers".to_owned(), json!(headers));
            }
            Value::Object(value)
        }
    }
}

/// Add the `mux` block, or leave the outbound as it was.
///
/// Written by mutation rather than as a field in each `json!`, because absent and
/// `"enabled": false` have to stay distinguishable in the artifact: a machine that was never
/// asked to pool should read identically to how it read before this feature existed, so that
/// enabling pooling on one hop is the only line the golden diff shows.
/// An ingress's `streamSettings`, whichever network it is carried over.
///
/// The TCP branch writes exactly the object that was written before XHTTP existed, key for key —
/// which is what keeps every ingress that has not asked for XHTTP producing byte-identical
/// artifacts, and what makes the golden files a real check rather than a formality.
fn stream_settings(stream: &XrayStream, security: &XrayIngressSecurity) -> Value {
    if matches!(security, XrayIngressSecurity::None) {
        return match stream {
            XrayStream::Tcp => json!({ "network": "tcp", "security": "none" }),
            XrayStream::Xhttp { path, mode, tuning } => json!({
                "network": "xhttp",
                "security": "none",
                "xhttpSettings": xhttp_settings(path, *mode, tuning.as_ref()),
            }),
        };
    }
    let (security_name, settings_key, settings) = ingress_security(security);
    match stream {
        XrayStream::Tcp => json!({
            "network": "tcp",
            "security": security_name,
            settings_key: settings,
        }),
        XrayStream::Xhttp { path, mode, tuning } => json!({
            "network": "xhttp",
            "security": security_name,
            settings_key: settings,
            "xhttpSettings": xhttp_settings(path, *mode, tuning.as_ref()),
        }),
    }
}

/// No `xmux` is written here, and the omission is the point: this block only ever lands on an
/// inbound, and xray reads XMUX on the dialing side alone (`XrayStream::Xhttp`). The concurrency
/// an operator sets reaches the client through the subscription instead.
fn xhttp_settings(path: &str, mode: Option<&str>, tuning: Option<&XhttpTuning>) -> Value {
    let mut xhttp = Map::new();
    xhttp.insert("path".to_owned(), json!(path));
    // Absent leaves the server accepting every upload shape. A value turns it into a filter.
    if let Some(mode) = mode {
        xhttp.insert("mode".to_owned(), json!(mode));
    }
    if let Some(tuning) = tuning {
        if let Some(range) = &tuning.x_padding_bytes {
            xhttp.insert("xPaddingBytes".to_owned(), xhttp_range(range));
        }
    }
    Value::Object(xhttp)
}

/// The security layer as xray spells it: the name, the key its settings hang off, and the
/// settings themselves.
///
/// Returned as a triple rather than written in place because the two networks above put the same
/// block in the same position, and writing it twice is how one of them ends up a version behind.
fn ingress_security(security: &XrayIngressSecurity) -> (&'static str, &'static str, Value) {
    match security {
        XrayIngressSecurity::None => unreachable!("无安全层不带设置块"),
        XrayIngressSecurity::Reality {
            dest,
            server_names,
            private_key,
            short_ids,
            min_client_ver,
            max_client_ver,
            max_time_diff_ms,
            fallback_limits,
        } => {
            let mut settings = Map::new();
            settings.insert("dest".to_owned(), json!(dest));
            if let Some(limits) = fallback_limits {
                settings.insert(
                    "limitFallbackDownload".to_owned(),
                    json!({
                        "afterBytes": limits.download.after_bytes,
                        "bytesPerSec": limits.download.bytes_per_sec,
                        "burstBytesPerSec": limits.download.burst_bytes_per_sec,
                    }),
                );
                settings.insert(
                    "limitFallbackUpload".to_owned(),
                    json!({
                        "afterBytes": limits.upload.after_bytes,
                        "bytesPerSec": limits.upload.bytes_per_sec,
                        "burstBytesPerSec": limits.upload.burst_bytes_per_sec,
                    }),
                );
            }
            if let Some(max_client_ver) = max_client_ver {
                settings.insert("maxClientVer".to_owned(), json!(max_client_ver));
            }
            if let Some(max_time_diff_ms) = max_time_diff_ms {
                settings.insert("maxTimeDiff".to_owned(), json!(max_time_diff_ms));
            }
            if let Some(min_client_ver) = min_client_ver {
                settings.insert("minClientVer".to_owned(), json!(min_client_ver));
            }
            settings.insert("privateKey".to_owned(), json!(private_key));
            settings.insert("serverNames".to_owned(), json!(server_names));
            settings.insert("shortIds".to_owned(), json!(short_ids));
            ("reality", "realitySettings", Value::Object(settings))
        }
        // No `serverName`: an inbound is not told its own name, the certificate carries it. ALPN
        // stays absent for normal TLS inbounds and is present only on the local 403 cover, whose
        // built-in response is HTTP/1.1 rather than HTTP/2 frames.
        XrayIngressSecurity::Tls {
            certificate_file,
            key_file,
            alpn,
        } => {
            let mut settings = json!({
                "certificates": [{
                    "certificateFile": certificate_file,
                    "keyFile": key_file,
                }],
            });
            if let Some(alpn) = alpn {
                settings
                    .as_object_mut()
                    .unwrap()
                    .insert("alpn".to_owned(), json!(alpn));
            }
            ("tls", "tlsSettings", settings)
        }
    }
}

fn insert_mux(value: &mut Value, mux: Option<&XrayMux>) {
    let Some(mux) = mux else {
        return;
    };
    let object = value
        .as_object_mut()
        .expect("xray outbound must be an object before inserting mux");
    object.insert(
        "mux".to_owned(),
        json!({ "enabled": true, "concurrency": mux.concurrency }),
    );
}

fn outbound(outbound: &XrayOutbound) -> Value {
    match outbound {
        XrayOutbound::Vless {
            tag,
            address,
            port,
            uuid,
            security,
            reverse_tag,
            mux,
        } => {
            let (encryption, mut stream_settings) = match security {
                XrayHopOutboundWire::None => (
                    "none".to_owned(),
                    json!({ "network": "tcp", "security": "none" }),
                ),
                XrayHopOutboundWire::Encryption { encryption } => (
                    encryption.clone(),
                    json!({ "network": "tcp", "security": "none" }),
                ),
                XrayHopOutboundWire::Reality {
                    server_name,
                    public_key,
                    short_id,
                    fingerprint,
                } => (
                    "none".to_owned(),
                    json!({
                        "network": "tcp",
                        "security": "reality",
                        "realitySettings": {
                            "fingerprint": fingerprint,
                            "publicKey": public_key,
                            "serverName": server_name,
                            "shortId": short_id,
                        },
                    }),
                ),
            };
            enable_tcp_fast_open(&mut stream_settings, json!(true));

            // Two shapes for one protocol, and the choice is not stylistic. xray honours
            // `reverse` only in the flat form: under `vnext` it refuses outright with
            // "please use simplified outbound's config style to use reverse"
            // (`infra/conf/vless.go`, the `c.Address != nil` branch). So the tunnel's
            // dialling outbound is written flat and every other one keeps `vnext`.
            //
            // Deliberately not flattening all of them: `vnext` is the form the rest of
            // the fleet's tooling reads, and one shape per purpose makes it visible in
            // the artifact which outbound is the tunnel.
            let settings = match reverse_tag {
                Some(reverse_tag) => json!({
                    "address": address,
                    "port": port,
                    "id": uuid,
                    "encryption": encryption,
                    "reverse": { "tag": reverse_tag },
                }),
                None => json!({
                    "vnext": [{
                        "address": address,
                        "port": port,
                        "users": [{ "id": uuid, "encryption": encryption }],
                    }]
                }),
            };
            let mut value = json!({
                "tag": tag,
                "protocol": "vless",
                "settings": settings,
                "streamSettings": stream_settings,
            });
            insert_mux(&mut value, mux.as_ref());
            value
        }
        XrayOutbound::Shadowsocks {
            tag,
            address,
            port,
            method,
            password,
            mux,
        } => {
            let mut value = json!({
                "tag": tag,
                "protocol": "shadowsocks",
                "settings": {
                    "servers": [{
                        "address": address,
                        "port": port,
                        "method": method,
                        "password": password,
                    }]
                },
                "streamSettings": {
                    "network": "tcp",
                    "security": "none",
                    "sockopt": { "tcpFastOpen": true },
                },
            });
            insert_mux(&mut value, mux.as_ref());
            value
        }
        XrayOutbound::External {
            tag,
            address,
            port,
            protocol,
            security,
            wireguard_workers,
        } => {
            let supports_stream_settings = !matches!(
                protocol,
                ExternalOutboundProtocol::Wireguard { .. } | ExternalOutboundProtocol::Warp { .. }
            );
            let mut vless_transport = None;
            let (protocol_name, settings) = match protocol {
                ExternalOutboundProtocol::Vless {
                    credential,
                    encryption,
                    flow,
                    transport,
                } => {
                    vless_transport = Some(transport);
                    let mut settings = json!({
                        "address": address,
                        "port": port,
                        "id": credential,
                        "encryption": encryption,
                    });
                    if let Some(flow) = flow {
                        settings
                            .as_object_mut()
                            .unwrap()
                            .insert("flow".to_owned(), json!(flow));
                    }
                    ("vless", settings)
                }
                ExternalOutboundProtocol::Shadowsocks2022 { credential, method } => (
                    "shadowsocks",
                    json!({
                        "address": address,
                        "port": port,
                        "method": method,
                        "password": credential,
                    }),
                ),
                ExternalOutboundProtocol::Socks5 {
                    username,
                    credential,
                } => (
                    "socks",
                    external_authenticated_proxy_settings(address, *port, username, credential),
                ),
                ExternalOutboundProtocol::HttpConnect {
                    username,
                    credential,
                } => (
                    "http",
                    external_authenticated_proxy_settings(address, *port, username, credential),
                ),
                ExternalOutboundProtocol::Wireguard {
                    credential,
                    peer_public_key,
                    local_addresses,
                    mtu,
                    reserved,
                    keep_alive,
                    allowed_ips,
                    no_kernel_tun,
                    domain_strategy,
                } => {
                    let mut settings = json!({
                        "secretKey": credential,
                        "address": local_addresses,
                        "peers": [{
                            "endpoint": external_endpoint(address, *port),
                            "publicKey": peer_public_key,
                            "keepAlive": keep_alive,
                            "allowedIPs": allowed_ips,
                        }],
                        "noKernelTun": no_kernel_tun,
                        "mtu": mtu,
                        "domainStrategy": domain_strategy,
                    });
                    if !reserved.is_empty() {
                        settings
                            .as_object_mut()
                            .unwrap()
                            .insert("reserved".to_owned(), json!(reserved));
                    }
                    if *wireguard_workers != 0 {
                        settings
                            .as_object_mut()
                            .unwrap()
                            .insert("workers".to_owned(), json!(wireguard_workers));
                    }
                    ("wireguard", settings)
                }
                ExternalOutboundProtocol::Warp { .. } => {
                    unreachable!("managed WARP must be lowered to WireGuard for its target node")
                }
            };
            let stream_settings = match vless_transport {
                Some(transport) => external_vless_stream_settings(transport, security),
                None => external_stream_settings("tcp", security),
            };
            let mut value = json!({
                "tag": tag,
                "protocol": protocol_name,
                "settings": settings,
            });
            if supports_stream_settings {
                value
                    .as_object_mut()
                    .unwrap()
                    .insert("streamSettings".to_owned(), stream_settings);
            }
            value
        }
        XrayOutbound::Freedom {
            tag,
            send_through,
            domain_strategy,
        } => {
            let mut object = Map::new();
            object.insert("tag".to_owned(), json!(tag));
            object.insert("protocol".to_owned(), json!("freedom"));
            if let Some(send_through) = send_through {
                object.insert("sendThrough".to_owned(), json!(send_through.to_string()));
            }
            // Emitted under the older key rather than `targetStrategy`: xray reads
            // `domainStrategy` as the fallback when `targetStrategy` is absent
            // (`infra/conf/freedom.go`), so the older key works on both sides of that
            // rename while the newer one would not.
            object.insert(
                "settings".to_owned(),
                json!({ "domainStrategy": domain_strategy }),
            );
            Value::Object(object)
        }
        XrayOutbound::Blackhole { tag, http_response } => {
            let mut value = json!({
                "tag": tag,
                "protocol": "blackhole",
            });
            if *http_response {
                value.as_object_mut().unwrap().insert(
                    "settings".to_owned(),
                    json!({ "response": { "type": "http" } }),
                );
            }
            value
        }
    }
}

/// Add TFO without rebuilding or duplicating the transport's security settings.
///
/// The caller chooses listener backlog (`256`) or client-side enablement (`true`). External
/// provider outbounds use separate rendering paths and remain untouched because Brocade does not
/// control the far endpoint.
fn enable_tcp_fast_open(stream_settings: &mut Value, value: Value) {
    stream_settings
        .as_object_mut()
        .expect("hop streamSettings must be an object")
        .insert("sockopt".to_owned(), json!({ "tcpFastOpen": value }));
}

fn external_vless_stream_settings(
    transport: &ExternalVlessTransport,
    security: &ExternalOutboundSecurity,
) -> Value {
    match transport {
        ExternalVlessTransport::Raw => external_stream_settings("tcp", security),
        ExternalVlessTransport::Xhttp(xhttp) => {
            let mut stream = external_stream_settings("xhttp", security);
            stream
                .as_object_mut()
                .unwrap()
                .insert("xhttpSettings".to_owned(), external_xhttp_settings(xhttp));
            stream
        }
    }
}

fn external_stream_settings(network: &str, security: &ExternalOutboundSecurity) -> Value {
    let mut stream = Map::new();
    stream.insert("network".to_owned(), json!(network));
    match security {
        ExternalOutboundSecurity::None => {
            stream.insert("security".to_owned(), json!("none"));
        }
        ExternalOutboundSecurity::Tls {
            server_name,
            fingerprint,
        } => {
            stream.insert("security".to_owned(), json!("tls"));
            stream.insert(
                "tlsSettings".to_owned(),
                json!({
                    "serverName": server_name,
                    "fingerprint": fingerprint,
                }),
            );
        }
        ExternalOutboundSecurity::Reality {
            server_name,
            public_key,
            short_id,
            fingerprint,
        } => {
            stream.insert("security".to_owned(), json!("reality"));
            stream.insert(
                "realitySettings".to_owned(),
                json!({
                    "serverName": server_name,
                    "publicKey": public_key,
                    "shortId": short_id,
                    "fingerprint": fingerprint,
                }),
            );
        }
    }
    Value::Object(stream)
}

fn external_xhttp_settings(xhttp: &ExternalVlessXhttp) -> Value {
    let mut settings = external_xhttp_base(
        &xhttp.path,
        xhttp.host.as_deref(),
        xhttp.mux,
        xhttp.mode.as_str(),
    );
    if let Some(download) = &xhttp.download {
        settings.as_object_mut().unwrap().insert(
            "downloadSettings".to_owned(),
            external_xhttp_download_settings(download),
        );
    }
    settings
}

fn external_xhttp_download_settings(download: &ExternalVlessXhttpDownload) -> Value {
    let mut settings = external_stream_settings("xhttp", &download.security);
    let object = settings.as_object_mut().unwrap();
    object.insert("address".to_owned(), json!(download.address));
    object.insert("port".to_owned(), json!(download.port));
    object.insert(
        "xhttpSettings".to_owned(),
        external_xhttp_base(
            &download.path,
            download.host.as_deref(),
            download.mux,
            download.mode.as_str(),
        ),
    );
    settings
}

fn external_xhttp_base(
    path: &str,
    host: Option<&str>,
    mux: Option<u16>,
    mode: Option<&str>,
) -> Value {
    let mut settings = Map::new();
    settings.insert("path".to_owned(), json!(path));
    if let Some(host) = host {
        settings.insert("host".to_owned(), json!(host));
    }
    if let Some(mode) = mode {
        settings.insert("mode".to_owned(), json!(mode));
    }
    if let Some(mux) = mux {
        settings.insert(
            "xmux".to_owned(),
            xhttp_xmux(&XhttpXmux::with_concurrency(mux)),
        );
    }
    Value::Object(settings)
}

fn xhttp_xmux(xmux: &XhttpXmux) -> Value {
    let mut value = Map::new();
    if let Some(concurrency) = xmux.max_concurrency {
        value.insert("maxConcurrency".to_owned(), json!(concurrency));
    }
    if let Some(connections) = xmux.max_connections {
        value.insert("maxConnections".to_owned(), json!(connections));
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
        value.insert("hKeepAlivePeriod".to_owned(), json!(period));
    }
    Value::Object(value)
}

fn xhttp_range(range: &XhttpXmuxRange) -> Value {
    if range.from == range.to {
        json!(range.from)
    } else {
        json!(format!("{}-{}", range.from, range.to))
    }
}

fn external_authenticated_proxy_settings(
    address: &str,
    port: u16,
    username: &Option<String>,
    credential: &str,
) -> Value {
    let mut settings = json!({ "address": address, "port": port });
    if let Some(username) = username {
        let object = settings.as_object_mut().unwrap();
        object.insert("user".to_owned(), json!(username));
        object.insert("pass".to_owned(), json!(credential));
    }
    settings
}

fn external_endpoint(address: &str, port: u16) -> String {
    if address.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv6()) {
        format!("[{address}]:{port}")
    } else {
        format!("{address}:{port}")
    }
}

/// The `policy` block.
///
/// Only `bufferSize` is conditional, and that is the point of it being an `Option` all the
/// way from the model: writing no key is a distinct instruction, not a missing value. Left
/// out, xray sizes the buffer from the CPU architecture — 512 KB on x86_64, 4 KB on arm64 —
/// and that difference is deliberate, since the machines given 4 KB are the ones that
/// cannot spare 512. Substituting a number would erase it.
///
/// The rest are always written, defaults included. An artifact that says what it means
/// costs nothing extra, and one reading `"connIdle": 300` answers the question that one
/// omitting the key leaves to whoever remembers xray's defaults.
fn policy_config(policy: &XrayPolicy) -> Value {
    let mut level = Map::new();
    level.insert("statsUserUplink".to_owned(), json!(true));
    level.insert("statsUserDownlink".to_owned(), json!(true));
    if policy.stats_user_online {
        level.insert("statsUserOnline".to_owned(), json!(true));
    }
    level.insert("handshake".to_owned(), json!(policy.handshake_secs));
    level.insert("connIdle".to_owned(), json!(policy.conn_idle_secs));
    level.insert("uplinkOnly".to_owned(), json!(policy.uplink_only_secs));
    level.insert("downlinkOnly".to_owned(), json!(policy.downlink_only_secs));
    if let Some(kb) = policy.buffer_size_kb {
        level.insert("bufferSize".to_owned(), json!(kb));
    }
    json!({
        "levels": { "0": Value::Object(level) },
        "system": {
            "statsInboundUplink": true,
            "statsInboundDownlink": true,
            "statsOutboundUplink": true,
            "statsOutboundDownlink": true,
        }
    })
}

fn routing_rule(rule: &XrayRoutingRule) -> Value {
    let mut object = Map::new();
    object.insert("type".to_owned(), json!("field"));
    // What lets this rule be removed from a running xray by name. Unconditional: a rule
    // without one is invisible to `xray api lsrules` (it reports `{}`) and unreachable by
    // `rmrules`, so one unnamed rule in the table is enough to make the table unswappable.
    object.insert("ruleTag".to_owned(), json!(rule.rule_tag));
    if !rule.inbound_tags.is_empty() {
        object.insert("inboundTag".to_owned(), json!(rule.inbound_tags));
    }
    if !rule.users.is_empty() {
        object.insert("user".to_owned(), json!(rule.users));
    }
    put_condition(&mut object, &rule.condition);
    // The two are exclusive: a rule pointing at a balancer writes no outboundTag.
    // The liveness rule takes the former, matching a domain that never appears,
    // purely so the balancer gets instantiated.
    match &rule.balancer_tag {
        Some(tag) => object.insert("balancerTag".to_owned(), json!(tag)),
        None => object.insert("outboundTag".to_owned(), json!(rule.outbound_tag)),
    };
    Value::Object(object)
}

fn put_condition(object: &mut Map<String, Value>, condition: &XrayMatchCondition) {
    if !condition.domain.is_empty() {
        object.insert("domain".to_owned(), json!(condition.domain));
    }
    if !condition.ip.is_empty() {
        object.insert("ip".to_owned(), json!(condition.ip));
    }
    if let Some(port) = &condition.port {
        object.insert("port".to_owned(), json!(port));
    }
    if let Some(network) = &condition.network {
        object.insert("network".to_owned(), json!(network));
    }
    if !condition.protocol.is_empty() {
        object.insert("protocol".to_owned(), json!(condition.protocol));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "xray outbound must be an object before inserting mux")]
    fn mux_insertion_fails_loudly_for_a_non_object() {
        let mut value = json!([]);
        insert_mux(&mut value, Some(&XrayMux { concurrency: 2 }));
    }
}
