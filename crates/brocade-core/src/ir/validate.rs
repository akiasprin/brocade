use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, Ipv4Addr},
};

use base64::{engine::general_purpose, Engine as _};
use ipnet::IpNet;

use crate::{
    diagnostic::Diagnostic,
    model::{
        Action, Dns, DomainStrategy, ExternalOutboundProtocol, ExternalOutboundSecurity,
        ExternalVlessTransport, ExternalVlessXhttpDownload, HopDial, HopPool, HopWire, Hysteria2,
        HysteriaCongestion, HysteriaMasquerade, HysteriaObfs, HysteriaQuic, ModelSnapshot,
        RealityFallbackLimits, RealityFallbackRateLimit, Transport, Xhttp, XhttpMode,
    },
    text::parse_semver3,
};

use super::{
    hops::{HopDialWire, HopPath},
    routing::HopIn,
    routing::{egress_tag, AppIr, DestMatch, Ingress, Rule},
    system::SystemIr,
};

const MAX_REALITY_TIME_DIFF_MS: u64 = 24 * 60 * 60 * 1000;

pub fn validate_model_snapshot(snapshot: &ModelSnapshot, diagnostics: &mut Vec<Diagnostic>) {
    validate_settings(snapshot, diagnostics);

    unique_by(
        snapshot.nodes.iter().map(|node| node.id.as_str()),
        "id.dup",
        "ModelSnapshot.nodes",
        diagnostics,
    );
    unique_by(
        snapshot.apps.iter().map(|app| app.id.as_str()),
        "id.dup",
        "ModelSnapshot.apps",
        diagnostics,
    );
    for app in &snapshot.apps {
        validate_slug(&app.id, format!("app {}", app.id), diagnostics);
    }
    unique_by(
        snapshot
            .users
            .iter()
            .map(|user| format!("{}/{}", user.tenant, user.id)),
        "user.dup",
        "ModelSnapshot.users",
        diagnostics,
    );
    unique_by(
        snapshot
            .external_outbounds
            .iter()
            .map(|outbound| format!("{}/{}", outbound.app, outbound.id)),
        "external-outbound.dup",
        "ModelSnapshot.external_outbounds",
        diagnostics,
    );
    for outbound in &snapshot.external_outbounds {
        if !snapshot.apps.iter().any(|app| app.id == outbound.app) {
            diagnostics.push(Diagnostic::error(
                "external-outbound.unknown-app",
                format!("{}/{}", outbound.app, outbound.id),
                format!("外部出站 {} 指向不存在的项目 {}", outbound.id, outbound.app),
            ));
        }
    }

    let mut uuid_owner = BTreeMap::<&str, String>::new();
    for user in &snapshot.users {
        let owner = format!("{}/{}", user.tenant, user.id);
        if let Some(previous) = uuid_owner.insert(user.uuid.as_str(), owner.clone()) {
            diagnostics.push(Diagnostic::error(
                "user.uuid-dup",
                owner,
                format!("用户 uuid 与 {previous} 重复"),
            ));
        }
    }
}

fn validate_settings(snapshot: &ModelSnapshot, diagnostics: &mut Vec<Diagnostic>) {
    let policy = &snapshot.settings.reality_client;
    let min = validate_reality_client_version(
        policy.min_client_ver.as_deref(),
        "settings.reality_client.min_client_ver",
        diagnostics,
    );
    let max = validate_reality_client_version(
        policy.max_client_ver.as_deref(),
        "settings.reality_client.max_client_ver",
        diagnostics,
    );

    if let (Some(min), Some(max)) = (min, max) {
        if min > max {
            diagnostics.push(Diagnostic::error(
                "reality.client-ver-range",
                "settings.reality_client",
                "min_client_ver 不能大于 max_client_ver",
            ));
        }
    }

    if policy
        .max_time_diff_ms
        .is_some_and(|value| value > MAX_REALITY_TIME_DIFF_MS)
    {
        diagnostics.push(Diagnostic::error(
            "reality.max-time-diff",
            "settings.reality_client.max_time_diff_ms",
            format!("max_time_diff_ms 不能超过 {MAX_REALITY_TIME_DIFF_MS}"),
        ));
    }
}

fn validate_reality_client_version(
    value: Option<&str>,
    location: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(u64, u64, u64)> {
    let value = value?;
    match parse_semver3(value) {
        Some(version) => Some(version),
        None => {
            diagnostics.push(Diagnostic::error(
                "reality.client-ver",
                location,
                format!("版本「{value}」不符合 x.y.z 格式"),
            ));
            None
        }
    }
}

pub fn validate_system(sys: &SystemIr, diagnostics: &mut Vec<Diagnostic>) {
    unique_by(
        sys.nodes.iter().map(|node| node.id.as_str()),
        "id.dup",
        "SystemIr.nodes",
        diagnostics,
    );
    unique_by(
        sys.links.iter().map(|link| link.id.as_str()),
        "id.dup",
        "SystemIr.links",
        diagnostics,
    );

    let mut seen_overlay = BTreeMap::<Ipv4Addr, &str>::new();
    for node in &sys.nodes {
        // A relay off the backbone has no overlay address, so neither of these
        // applies to it.
        let Some(overlay_addr) = node.overlay_addr else {
            continue;
        };
        if let Some(previous) = seen_overlay.insert(overlay_addr, &node.id) {
            diagnostics.push(Diagnostic::error(
                "node.overlay-dup",
                &node.id,
                format!("overlay 地址 {overlay_addr} 与 {previous} 重复"),
            ));
        }
        if !sys.overlay_cidr.contains(&overlay_addr) {
            diagnostics.push(Diagnostic::error(
                "node.overlay-out",
                &node.id,
                format!("overlay 地址 {overlay_addr} 不在 {} 内", sys.overlay_cidr),
            ));
        }
    }
}

/// The two checks on relay hops.
///
/// Both are warnings rather than errors, for one shared reason: the compiler cannot
/// determine where the address written on a chain is exposed. Most likely the public
/// internet, but possibly a leased line or a private network in one datacenter — where
/// running unencrypted is a reasonable choice, and reporting an error would wall off a
/// legitimate configuration. So this only makes it visible.
///
/// The test comes from how the hop actually dials rather than from some field on the
/// machine: with relay ports split per chain, one machine can perfectly well take the
/// overlay on one chain and dial bare on another, and testing by machine would collapse
/// the two into one statement.
///
/// The test is whether it passes through WireGuard, not whether I initiate it. Reverse
/// access is on the public internet as well — `HopDial::Reverse` offers no "reverse
/// over the overlay" combination, so it is as bare as direct dialing and only the
/// initiator differs. Filtering on "is it Direct" misses the entire reverse family, and
/// what gets missed is a chain running in the clear over the public internet without a
/// word.
fn validate_hop_security(app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    for hop in &app.hops {
        // Only the overlay variant is excluded: WireGuard already wraps that hop, so
        // running unencrypted inside is correct and another layer would burn CPU for
        // nothing.
        if hop.path == HopPath::Overlay {
            continue;
        }
        let at = format!("{}/{}->{}", hop.chain, hop.from, hop.to);
        match &hop.security {
            // `Info` rather than `Warn`: the compiler cannot determine where this
            // address is exposed. Most likely the public internet, but possibly a leased
            // line or a private datacenter network, where running unencrypted is a
            // reasonable choice. Unable to determine it, this only makes it visible —
            // the same reasoning as `link.no-endpoint` (see `Level::Info`). Warnings are
            // reserved for what the compiler can establish as a problem, such as the
            // non-443 REALITY check below.
            HopDialWire::None => {
                // On the reverse variant `address` is this machine's and the peer is
                // the one connecting, so the wording has to change with it — phrased as
                // for direct dialing it reads "I dial myself in the clear", which has
                // the reader doubting the compiler first.
                let dial = if hop.path == HopPath::Reverse {
                    format!("{} 明文连入 {}:{}", hop.to, hop.address, hop.port)
                } else {
                    format!("明文直拨 {}:{}", hop.address, hop.port)
                };
                diagnostics.push(Diagnostic::info(
                    "hop.plaintext",
                    at,
                    format!(
                        "{dial}：该跳不经过 WireGuard，\
                         若 {} 位于公网，UUID 和目标地址以明文传输",
                        hop.address
                    ),
                ));
            }
            // xray says as much itself under -test. REALITY on a non-443 port is
            // conspicuous: a real site does not serve TLS on port 20000, one active
            // probe settles it, and the price is the machine's whole IP being blocked.
            HopDialWire::Reality { .. } if hop.port != 443 => {
                diagnostics.push(Diagnostic::warn(
                    "hop.reality-port",
                    at,
                    format!("REALITY 端口 {} 不是 443，伪装特征明显", hop.port),
                ));
            }
            _ => {}
        }
    }
}

pub fn validate_app(sys: &SystemIr, app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    validate_unique_ids(app, diagnostics);
    validate_slugs(app, diagnostics);
    validate_chain_ingresses(app, diagnostics);
    validate_ports(sys, app, diagnostics);
    validate_labels(app, diagnostics);
    validate_steps(sys, app, diagnostics);
    validate_external_outbounds(app, diagnostics);
    validate_topology(app, diagnostics);
    validate_fronts(app, diagnostics);
    validate_reality(app, diagnostics);
    validate_hop_security(app, diagnostics);
    validate_reverse_needs_vless(app, diagnostics);
    validate_tenants(app, diagnostics);
    validate_dns(app, diagnostics);
}

fn validate_external_outbounds(app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    let app_name = app.app_id.as_deref().unwrap_or("?");
    for outbound in &app.external_outbounds {
        let at = format!("{app_name}/{}", outbound.id);
        validate_slug(&outbound.id, at.clone(), diagnostics);
        if outbound.name.trim().is_empty() {
            diagnostics.push(Diagnostic::error(
                "external-outbound.blank-name",
                &at,
                "外部出站名称不能为空",
            ));
        }
        if outbound.address.trim().is_empty() || outbound.address.chars().any(char::is_whitespace) {
            diagnostics.push(Diagnostic::error(
                "external-outbound.address",
                &at,
                "外部出站服务器地址为空或含空白",
            ));
        }
        if !outbound.protocol.allows_empty_credential()
            && outbound.protocol.credential().trim().is_empty()
        {
            diagnostics.push(Diagnostic::error(
                "external-outbound.credential",
                &at,
                "外部出站凭据不能为空",
            ));
        }
        match &outbound.protocol {
            ExternalOutboundProtocol::Vless {
                encryption,
                flow,
                transport,
                ..
            } => {
                if encryption.trim().is_empty() {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.vless-encryption",
                        &at,
                        "VLESS encryption 不能为空；不用协议层加密时应写 none",
                    ));
                }
                if flow.as_deref().is_some_and(|value| {
                    !matches!(value, "xtls-rprx-vision" | "xtls-rprx-vision-udp443")
                }) {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.vless-flow",
                        &at,
                        "VLESS flow 只支持 xtls-rprx-vision 或 xtls-rprx-vision-udp443",
                    ));
                }
                if let ExternalVlessTransport::Xhttp(xhttp) = transport {
                    if flow.as_deref().is_some_and(|flow| !flow.is_empty()) {
                        diagnostics.push(Diagnostic::error(
                            "external-outbound.xhttp-flow-conflict",
                            &at,
                            "外部 VLESS 使用 XHTTP 时不能启用 Vision flow；请清空 Flow 或改回 RAW/TCP",
                        ));
                    }
                    validate_external_xhttp_fields(
                        diagnostics,
                        &at,
                        "external-outbound.xhttp-path",
                        "external-outbound.xhttp-host",
                        "external-outbound.xhttp-mux-range",
                        &xhttp.path,
                        xhttp.host.as_deref(),
                        xhttp.mux,
                    );
                    if let Some(download) = &xhttp.download {
                        if xhttp.mode == XhttpMode::StreamOne {
                            diagnostics.push(Diagnostic::error(
                                "external-outbound.xhttp-download-stream-one",
                                &at,
                                "外部 VLESS 配置了独立下载，但 stream-one 将上下行放在同一个请求中；请改用 auto 或 stream-up",
                            ));
                        }
                        validate_external_xhttp_download(diagnostics, &at, download);
                    }
                }
            }
            ExternalOutboundProtocol::Shadowsocks2022 { credential, method } => {
                let key_len = match method.as_str() {
                    "2022-blake3-aes-128-gcm" => Some(16),
                    "2022-blake3-aes-256-gcm" | "2022-blake3-chacha20-poly1305" => Some(32),
                    _ => None,
                };
                if key_len.is_none() {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.shadowsocks2022-method",
                        &at,
                        "外部 Shadowsocks 只支持 SS2022 的三种 2022-blake3-* 加密方式",
                    ));
                } else if !ss2022_credential_is_valid(credential, key_len.unwrap()) {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.shadowsocks2022-key",
                        &at,
                        format!(
                            "SS2022 密钥必须是 Base64 编码的 {} 字节 PSK；多用户服务端使用 server-key:user-key",
                            key_len.unwrap()
                        ),
                    ));
                }
            }
            ExternalOutboundProtocol::Socks5 {
                username,
                credential,
            }
            | ExternalOutboundProtocol::HttpConnect {
                username,
                credential,
            } => {
                let has_username = username
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
                let has_password = !credential.is_empty();
                if has_username != has_password
                    || username
                        .as_deref()
                        .is_some_and(|value| value.trim().is_empty())
                {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.proxy-auth",
                        &at,
                        "SOCKS5 / HTTP CONNECT 必须同时填写用户名和密码，或两项都不填",
                    ));
                }
            }
            ExternalOutboundProtocol::Wireguard {
                credential,
                peer_public_key,
                local_addresses,
                mtu,
                reserved,
                allowed_ips,
                domain_strategy,
                ..
            } => {
                if !wireguard_key_is_valid(credential) {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.wireguard-private-key",
                        &at,
                        "WireGuard 私钥必须是 Base64 编码的 32 字节密钥",
                    ));
                }
                if !wireguard_key_is_valid(peer_public_key) {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.wireguard-public-key",
                        &at,
                        "WireGuard peer 公钥必须是 Base64 编码的 32 字节密钥",
                    ));
                }
                if local_addresses.is_empty()
                    || local_addresses
                        .iter()
                        .any(|address| address.parse::<IpNet>().is_err())
                {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.wireguard-addresses",
                        &at,
                        "WireGuard 隧道地址至少填写一个合法 CIDR",
                    ));
                }
                if !(576..=9000).contains(mtu) {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.wireguard-mtu",
                        &at,
                        "WireGuard MTU 必须在 576–9000 之间",
                    ));
                }
                if !reserved.is_empty() && reserved.len() != 3 {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.wireguard-reserved",
                        &at,
                        "WireGuard reserved 必须留空或恰好包含 3 个字节",
                    ));
                }
                if allowed_ips.is_empty()
                    || allowed_ips
                        .iter()
                        .any(|network| network.parse::<IpNet>().is_err())
                {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.wireguard-allowed-ips",
                        &at,
                        "WireGuard allowed IPs 至少填写一个合法 CIDR",
                    ));
                }
                if !matches!(
                    domain_strategy.as_str(),
                    "ForceIP" | "ForceIPv4" | "ForceIPv6" | "ForceIPv4v6" | "ForceIPv6v4"
                ) {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.wireguard-domain-strategy",
                        &at,
                        "WireGuard 域名策略不是 Xray 支持的 ForceIP 系列取值",
                    ));
                }
            }
        }
        match &outbound.security {
            ExternalOutboundSecurity::None
                if matches!(&outbound.protocol, ExternalOutboundProtocol::Vless { .. }) =>
            {
                diagnostics.push(Diagnostic::error(
                    "external-outbound.security-required",
                    &at,
                    "公网 VLESS 外部出站必须选择 TLS 或 REALITY",
                ));
            }
            ExternalOutboundSecurity::Tls { .. } | ExternalOutboundSecurity::Reality { .. }
                if matches!(
                    &outbound.protocol,
                    ExternalOutboundProtocol::Shadowsocks2022 { .. }
                ) =>
            {
                diagnostics.push(Diagnostic::error(
                    "external-outbound.shadowsocks2022-transport",
                    &at,
                    "SS2022 外部出站只支持 RAW，不叠加 TLS 或 REALITY",
                ));
            }
            ExternalOutboundSecurity::Tls { .. } | ExternalOutboundSecurity::Reality { .. }
                if matches!(
                    &outbound.protocol,
                    ExternalOutboundProtocol::Socks5 { .. }
                        | ExternalOutboundProtocol::Wireguard { .. }
                ) =>
            {
                diagnostics.push(Diagnostic::error(
                    "external-outbound.raw-transport",
                    &at,
                    "SOCKS5 和 WireGuard 外部出站只支持自身的 RAW 传输",
                ));
            }
            ExternalOutboundSecurity::Reality { .. }
                if matches!(
                    &outbound.protocol,
                    ExternalOutboundProtocol::HttpConnect { .. }
                ) =>
            {
                diagnostics.push(Diagnostic::error(
                    "external-outbound.http-connect-security",
                    &at,
                    "HTTP CONNECT 只支持 RAW 或 TLS，不支持 REALITY",
                ));
            }
            ExternalOutboundSecurity::Tls {
                server_name,
                fingerprint,
            } => {
                if server_name.trim().is_empty() || fingerprint.trim().is_empty() {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.tls",
                        &at,
                        "TLS 的 SNI 和指纹不能为空",
                    ));
                }
            }
            ExternalOutboundSecurity::Reality {
                server_name,
                public_key,
                short_id,
                fingerprint,
            } => {
                if server_name.trim().is_empty()
                    || public_key.trim().is_empty()
                    || fingerprint.trim().is_empty()
                    || short_id.is_empty()
                    || short_id.len() > 16
                    || !short_id.chars().all(|c| c.is_ascii_hexdigit())
                {
                    diagnostics.push(Diagnostic::error(
                        "external-outbound.reality",
                        &at,
                        "REALITY 的 SNI、公钥、指纹不能为空，short id 必须为 1–16 位十六进制",
                    ));
                }
            }
            _ => {}
        }
    }

    for step in &app.steps {
        let chain_tenant = app
            .chains
            .iter()
            .find(|chain| chain.id == step.chain)
            .map(|chain| chain.tenant.as_str());
        for rule in &step.rules {
            let Action::Proxy { outbound } = &rule.action else {
                continue;
            };
            let Some(target) = app
                .external_outbounds
                .iter()
                .find(|target| target.id == *outbound)
            else {
                diagnostics.push(Diagnostic::error(
                    "rule.unknown-external-outbound",
                    format!("{}/{}", step.chain, step.node),
                    format!("规则指向不存在的外部出站 {outbound}"),
                ));
                continue;
            };
            if chain_tenant.is_some_and(|tenant| !under(tenant, &target.tenant)) {
                diagnostics.push(Diagnostic::error(
                    "tenant.scope",
                    format!("{}/{}", step.chain, target.id),
                    format!(
                        "链属于 {}，而外部出站 {} 归属于 {}，不在可见范围内",
                        chain_tenant.unwrap_or_default(),
                        target.id,
                        target.tenant
                    ),
                ));
            }
        }
    }
}

fn validate_external_xhttp_fields(
    diagnostics: &mut Vec<Diagnostic>,
    at: &str,
    path_code: &'static str,
    host_code: &'static str,
    mux_code: &'static str,
    path: &str,
    host: Option<&str>,
    mux: Option<u16>,
) {
    if !path.starts_with('/')
        || path
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '?' | '#'))
    {
        diagnostics.push(Diagnostic::error(
            path_code,
            at,
            "XHTTP 路径必须以 / 开头，且不能包含空白、? 或 #",
        ));
    }
    if host.is_some_and(|host| host.trim().is_empty()) {
        diagnostics.push(Diagnostic::error(
            host_code,
            at,
            "XHTTP Host 不能是空白字符；不需要设置时请留空",
        ));
    }
    if mux.is_some_and(|mux| !(Xhttp::MUX_MIN..=Xhttp::MUX_MAX).contains(&mux)) {
        diagnostics.push(Diagnostic::error(
            mux_code,
            at,
            format!(
                "XHTTP 并发数必须在 {}–{} 之间；留空使用客户端默认",
                Xhttp::MUX_MIN,
                Xhttp::MUX_MAX
            ),
        ));
    }
}

fn validate_external_xhttp_download(
    diagnostics: &mut Vec<Diagnostic>,
    at: &str,
    download: &ExternalVlessXhttpDownload,
) {
    if download.address.trim().is_empty()
        || download.address.chars().any(char::is_whitespace)
        || download.port == 0
    {
        diagnostics.push(Diagnostic::error(
            "external-outbound.xhttp-download-endpoint",
            at,
            "XHTTP 独立下载的服务器地址不能为空或含空白，端口不能为 0",
        ));
    }
    validate_external_xhttp_fields(
        diagnostics,
        at,
        "external-outbound.xhttp-download-path",
        "external-outbound.xhttp-download-host",
        "external-outbound.xhttp-download-mux-range",
        &download.path,
        download.host.as_deref(),
        download.mux,
    );
    match &download.security {
        ExternalOutboundSecurity::None => diagnostics.push(Diagnostic::error(
            "external-outbound.xhttp-download-security",
            at,
            "公网 XHTTP 独立下载必须使用 TLS 或 REALITY",
        )),
        ExternalOutboundSecurity::Tls {
            server_name,
            fingerprint,
        } => {
            if server_name.trim().is_empty() || fingerprint.trim().is_empty() {
                diagnostics.push(Diagnostic::error(
                    "external-outbound.xhttp-download-tls",
                    at,
                    "XHTTP 独立下载 TLS 的 SNI 和指纹不能为空",
                ));
            }
        }
        ExternalOutboundSecurity::Reality {
            server_name,
            public_key,
            short_id,
            fingerprint,
        } => {
            if server_name.trim().is_empty()
                || public_key.trim().is_empty()
                || fingerprint.trim().is_empty()
                || short_id.is_empty()
                || short_id.len() > 16
                || !short_id
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
            {
                diagnostics.push(Diagnostic::error(
                    "external-outbound.xhttp-download-reality",
                    at,
                    "XHTTP 独立下载 REALITY 的 SNI、公钥、指纹不能为空，short id 必须为 1–16 位十六进制",
                ));
            }
        }
    }
}

fn ss2022_credential_is_valid(credential: &str, key_len: usize) -> bool {
    !credential.is_empty()
        && credential.split(':').all(|part| {
            !part.is_empty()
                && general_purpose::STANDARD
                    .decode(part)
                    .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(part))
                    .is_ok_and(|key| key.len() == key_len)
        })
}

fn wireguard_key_is_valid(key: &str) -> bool {
    general_purpose::STANDARD
        .decode(key)
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(key))
        .is_ok_and(|decoded| decoded.len() == 32)
}

pub fn validate_app_set(apps: &[AppIr], diagnostics: &mut Vec<Diagnostic>) {
    validate_app_set_ports(apps, diagnostics);
    validate_app_set_labels(apps, diagnostics);
    validate_app_set_dns(apps, diagnostics);
}

/// A reverse hop's relay port cannot speak Shadowsocks 2022.
///
/// The tunnel is not a connection the two ends agree about between themselves — xray hangs it
/// off an account on the accepting side, and only VLESS has accounts to hang it off. The
/// shadowsocks inbound has one key and no account list, so there is nowhere for the tunnel's
/// name to go.
///
/// Rejected rather than reported, because the shape of the failure is bad: both machines
/// would take their configuration, xray would start on both, and the downstream would sit
/// there having attached nothing while every artifact looked correct. That is the failure the
/// note on `XrayReversePortalPlan` describes from the era when the two ends were paired by an
/// agreed token, and it is not worth reintroducing.
///
/// It is the accepting end that is constrained. On a reverse hop the peer connects to me, so
/// the port carrying the tunnel is mine — the same end whose client list gains the downstream's
/// entry in `physical/node.rs`.
fn validate_reverse_needs_vless(app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    for hop in app.hops.iter().filter(|hop| hop.path == HopPath::Reverse) {
        let accepting = app
            .steps
            .iter()
            .find(|step| step.chain == hop.chain && step.node == hop.from);
        let Some(HopIn {
            security: HopWire::Shadowsocks2022 { .. },
            ..
        }) = accepting.and_then(|step| step.hop_in.as_ref())
        else {
            continue;
        };
        diagnostics.push(Diagnostic::error(
            "hop.reverse-needs-vless",
            format!("{}/{}", hop.chain, hop.from),
            format!(
                "{} 的中转端口使用了 Shadowsocks 2022，但 {} 需要从此处反向接入，\
                 反向隧道只支持 VLESS",
                hop.from, hop.to
            ),
        ));
    }
}

fn validate_unique_ids(app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    unique_by(
        app.nodes.iter().map(|node| node.id.as_str()),
        "id.dup",
        "AppIr.nodes",
        diagnostics,
    );
    unique_by(
        app.chains.iter().map(|chain| chain.id.as_str()),
        "id.dup",
        "AppIr.chains",
        diagnostics,
    );
    unique_by(
        app.ingresses.iter().map(|ingress| ingress.id.as_str()),
        "id.dup",
        "AppIr.ingresses",
        diagnostics,
    );
    unique_by(
        app.fronts.iter().map(|front| front.id.as_str()),
        "id.dup",
        "AppIr.fronts",
        diagnostics,
    );
    unique_by(
        app.steps.iter().map(|step| step.id.as_str()),
        "id.dup",
        "AppIr.steps",
        diagnostics,
    );
    unique_by(
        app.grants.iter().map(|grant| grant.id.as_str()),
        "id.dup",
        "AppIr.grants",
        diagnostics,
    );
    unique_by(
        app.users
            .iter()
            .map(|user| format!("{}/{}", user.tenant, user.id)),
        "user.dup",
        "AppIr.users",
        diagnostics,
    );
    unique_by(
        app.external_outbounds
            .iter()
            .map(|outbound| outbound.id.as_str()),
        "external-outbound.dup",
        "AppIr.external_outbounds",
        diagnostics,
    );
}

/// Port occupancy across apps.
///
/// Keyed by protocol as well as number, the same way the per-machine layer is (`Proto` in
/// `validate_ports`). It was not, back when every ingress and every relay inbound was TCP —
/// and the note left here then said what would happen if that ever stopped being true. It
/// stopped with Hysteria 2: a QUIC ingress on UDP 8443 and a relay inbound on TCP 8443 do not
/// contend for anything, and judging them by number alone reports a clash that does not exist
/// and blocks the release.
fn validate_app_set_ports(apps: &[AppIr], diagnostics: &mut Vec<Diagnostic>) {
    let mut used = BTreeMap::<String, BTreeMap<(Proto, u16), (usize, String)>>::new();

    for (app_index, app) in apps.iter().enumerate() {
        let app_name = app_name(app, app_index);
        for ingress in &app.ingresses {
            let ingress_id = format!("{app_name}/{}", ingress.id);
            for occupied in occupied_ingress_ports(ingress) {
                put_cross_app_port(
                    &mut used,
                    &ingress.node,
                    occupied.proto,
                    occupied.port,
                    app_index,
                    ingress_port_owner(&ingress_id, occupied.suffix),
                    diagnostics,
                );
            }
        }
        for step in &app.steps {
            let Some(hop_in) = &step.hop_in else {
                continue;
            };
            put_cross_app_port(
                &mut used,
                &step.node,
                Proto::Tcp,
                hop_in.port,
                app_index,
                format!("{} 链 {} 的中转 inbound", app_name, step.chain),
                diagnostics,
            );
        }
    }
}

/// Ports already claimed on each node: node id, then (proto, port), then who claimed it — the
/// app's index and a description, which is what lets a collision name both sides rather than only
/// the app that arrived second.
type CrossAppPorts = BTreeMap<String, BTreeMap<(Proto, u16), (usize, String)>>;

fn put_cross_app_port(
    used: &mut CrossAppPorts,
    node: &str,
    proto: Proto,
    port: u16,
    app_index: usize,
    what: String,
    diagnostics: &mut Vec<Diagnostic>,
) {
    use std::collections::btree_map::Entry;

    match used
        .entry(node.to_owned())
        .or_default()
        .entry((proto, port))
    {
        Entry::Vacant(entry) => {
            entry.insert((app_index, what));
        }
        Entry::Occupied(entry) => {
            let (previous_app, previous) = entry.get();
            if *previous_app != app_index {
                diagnostics.push(Diagnostic::error(
                    "node.port-clash",
                    node,
                    format!("{proto} 端口 {port} 同时被 {previous} 和 {what} 占用"),
                ));
            }
        }
    }
}

fn validate_app_set_labels(apps: &[AppIr], diagnostics: &mut Vec<Diagnostic>) {
    let mut used = BTreeMap::<String, BTreeMap<String, (usize, String)>>::new();

    for (app_index, app) in apps.iter().enumerate() {
        let app_name = app_name(app, app_index);
        for step in app
            .steps
            .iter()
            .filter_map(|step| step.accept.as_ref().map(|accept| (step, accept)))
        {
            put_cross_app_label(
                &mut used,
                &step.0.node,
                &step.1.label,
                app_index,
                format!("{} 链 {} 的接受凭据", app_name, step.0.chain),
                diagnostics,
            );
        }

        for grant in &app.grants {
            let Some(ingress) = app
                .ingresses
                .iter()
                .find(|ingress| ingress.id == grant.ingress)
            else {
                continue;
            };
            put_cross_app_label(
                &mut used,
                &ingress.node,
                &grant.label,
                app_index,
                format!("{} 用户 {}/{}", app_name, grant.tenant, grant.user),
                diagnostics,
            );
        }
    }
}

fn validate_app_set_dns(apps: &[AppIr], diagnostics: &mut Vec<Diagnostic>) {
    let mut dns_by_node = BTreeMap::<String, (Dns, DomainStrategy)>::new();
    let mut egress_by_node = BTreeMap::<String, BTreeSet<String>>::new();

    for app in apps {
        for node in &app.nodes {
            dns_by_node
                .entry(node.id.clone())
                .or_insert_with(|| (node.dns.clone(), node.domain_strategy));
        }
        for step in &app.steps {
            for rule in &step.rules {
                if let Action::Egress { send_through } = rule.action {
                    egress_by_node
                        .entry(step.node.clone())
                        .or_default()
                        .insert(egress_tag(send_through.as_ref()));
                }
            }
        }
    }

    for (node_id, (dns, strategy)) in dns_by_node {
        if !dns.needs_route() {
            continue;
        }

        // AsIs never asks xray's DNS at all — the domain goes to the dialer untouched and
        // the machine's own resolver settles it — so the servers configured here are dead
        // weight. Reported rather than rejected: the combination is legal, just useless,
        // and which of the two the operator meant to change is theirs to say.
        //
        // It returns before the route count below on purpose. That check exists to give the
        // internal DNS's own queries a definite way out, and under AsIs there are no such
        // queries; letting it run would reject a legal machine over an ambiguity that
        // cannot be reached.
        if strategy == DomainStrategy::AsIs {
            diagnostics.push(Diagnostic::warn(
                "node.dns-bypassed",
                &node_id,
                format!("{node_id} 配置了外部 DNS，但域名解析设为 AsIs，该配置不会生效"),
            ));
            continue;
        }

        let routes = egress_by_node.get(&node_id);
        match routes.map(BTreeSet::len).unwrap_or_default() {
            0 => diagnostics.push(Diagnostic::warn(
                "node.dns-unused",
                &node_id,
                format!("{node_id} 配置了外部 DNS，但没有出网动作"),
            )),
            1 => {}
            count => diagnostics.push(Diagnostic::error(
                "dns.route-ambiguous",
                &node_id,
                format!("{node_id} 内建 DNS 的出网口不唯一，共 {count} 个"),
            )),
        }
    }
}

fn put_cross_app_label(
    used: &mut BTreeMap<String, BTreeMap<String, (usize, String)>>,
    node: &str,
    label: &str,
    app_index: usize,
    what: String,
    diagnostics: &mut Vec<Diagnostic>,
) {
    use std::collections::btree_map::Entry;

    match used
        .entry(node.to_owned())
        .or_default()
        .entry(label.to_owned())
    {
        Entry::Vacant(entry) => {
            entry.insert((app_index, what));
        }
        Entry::Occupied(entry) => {
            let (previous_app, previous) = entry.get();
            if *previous_app != app_index {
                diagnostics.push(Diagnostic::error(
                    "label.duplicate",
                    node,
                    format!("label「{label}」被 {previous} 和 {what} 同时使用"),
                ));
            }
        }
    }
}

fn app_name(app: &AppIr, index: usize) -> String {
    app.app_id.clone().unwrap_or_else(|| format!("app#{index}"))
}

fn validate_chain_ingresses(app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    for chain in &app.chains {
        if chain.root.is_some() {
            continue;
        }
        // A chain's entrance is not declared; `ingress.node` is the head. No ingress on
        // a chain means the ingress was forgotten, the compilation layer compiles
        // nothing without a head, and this blocks it. A chain whose ingress node was
        // decommissioned never reaches here at all (`compile_app` disables the whole
        // chain).
        diagnostics.push(Diagnostic::error(
            "chain.no-ingress",
            &chain.id,
            format!("链 {} 上没有任何接入面", chain.id),
        ));
    }

    for ingress in &app.ingresses {
        validate_ingress_stream(diagnostics, ingress);
        if let Some(settings) = ingress.wires.hysteria2() {
            validate_hysteria2(diagnostics, ingress, settings);
        }
        validate_ingress_certificate(diagnostics, ingress);
        validate_ingress_guard(diagnostics, ingress);
        if !app.chains.iter().any(|chain| chain.id == ingress.chain) {
            diagnostics.push(Diagnostic::error(
                "ingress.no-chain",
                &ingress.id,
                format!("接入面指向的链 {} 不存在", ingress.chain),
            ));
            continue;
        };

        // The head is the machine hosting the ingress, and that machine must really
        // exist: a nonexistent head compiles to no steps at all (the missing target was
        // already reported as `chain.unknown-node` while expanding rule Forwards).
        if !app.nodes.iter().any(|node| node.id == ingress.node) {
            diagnostics.push(Diagnostic::error(
                "ingress.no-node",
                &ingress.id,
                format!("接入面所在节点 {} 不存在", ingress.node),
            ));
        }

        validate_projection(ingress, diagnostics);
    }
}

/// Projection checks exactly one thing: whether an enabled family has an address.
///
/// "No projection" is `None`, not an empty string. An empty string means "enabled but
/// unfilled" — an error, not a configuration. The two must stay distinguishable: once
/// an empty string lands in the database, whether the operator meant to turn it off or
/// filled it in halfway can never be established again, while the artifacts end up with
/// an undialable address and the machine side looks entirely fine.
///
/// The UI blocks it too (the save button greys out), but the UI cannot be the only
/// guard: drafts can be pushed straight through the API.
///
/// It does not check whether the address really reaches this machine — that line's
/// relaying arrangement lies outside brocade, the compiler has no basis to judge, and
/// checking would only produce false alarms.
fn validate_projection(ingress: &Ingress, diagnostics: &mut Vec<Diagnostic>) {
    for (family, endpoint) in [
        ("IPv4", ingress.projection.v4.as_ref()),
        ("IPv6", ingress.projection.v6.as_ref()),
    ] {
        let Some(endpoint) = endpoint else {
            continue;
        };
        if endpoint.host.trim().is_empty() {
            diagnostics.push(Diagnostic::error(
                "ingress.projection-blank",
                &ingress.id,
                format!(
                    "接入面 {} 已启用 {family} 投影但未填写地址；不需要投影时请关闭开关，而非留空",
                    ingress.id
                ),
            ));
        }
        if endpoint.port == 0 {
            diagnostics.push(Diagnostic::error(
                "ingress.projection-port",
                &ingress.id,
                format!("接入面 {} 的 {family} 投影端口是 0", ingress.id),
            ));
        }
        if let Some(download) = &endpoint.download {
            if download.host.trim().is_empty() {
                diagnostics.push(Diagnostic::error(
                    "ingress.projection-download-blank",
                    &ingress.id,
                    format!("接入面 {} 的 {family} 下载投影没有填写地址", ingress.id),
                ));
            }
            if download.port == 0 {
                diagnostics.push(Diagnostic::error(
                    "ingress.projection-download-port",
                    &ingress.id,
                    format!("接入面 {} 的 {family} 下载投影端口是 0", ingress.id),
                ));
            }
            if !matches!(
                ingress.wires.vless(),
                Some(Transport::VlessTlsXhttp(_) | Transport::VlessRealityXhttp(_))
            ) {
                diagnostics.push(Diagnostic::error(
                    "ingress.projection-download-transport",
                    &ingress.id,
                    format!(
                        "接入面 {} 配置了 {family} 独立下载投影，但该配置只适用于 XHTTP",
                        ingress.id
                    ),
                ));
            }
            if matches!(ingress.wires.vless(), Some(Transport::VlessRealityXhttp(_)))
                && download.node_port() == ingress.port
            {
                diagnostics.push(Diagnostic::error(
                    "ingress.projection-download-port-clash",
                    &ingress.id,
                    format!(
                        "接入面 {} 的 REALITY 上行和 TLS 下载不能同时监听端口 {}",
                        ingress.id, ingress.port
                    ),
                ));
            }
        }
    }
}

fn validate_slugs(app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    if let Some(app_id) = &app.app_id {
        validate_slug(app_id, format!("app {}", app_id), diagnostics);
    }
    for node in &app.nodes {
        validate_slug(&node.id, format!("node {}", node.id), diagnostics);
    }
    for user in &app.users {
        validate_slug(
            &user.id,
            format!("user {}/{}", user.tenant, user.id),
            diagnostics,
        );
        for segment in user.tenant.split('.') {
            validate_slug(
                segment,
                format!("tenant segment {} in {}", segment, user.tenant),
                diagnostics,
            );
        }
    }
    for chain in &app.chains {
        validate_slug(&chain.id, format!("chain {}", chain.id), diagnostics);
        for segment in chain.tenant.split('.') {
            validate_slug(
                segment,
                format!("tenant segment {} in {}", segment, chain.tenant),
                diagnostics,
            );
        }
    }
    for ingress in &app.ingresses {
        validate_slug(&ingress.id, format!("ingress {}", ingress.id), diagnostics);
    }
    for front in &app.fronts {
        validate_slug(&front.id, format!("front {}", front.id), diagnostics);
        for segment in front.tenant.split('.') {
            validate_slug(
                segment,
                format!("tenant segment {} in {}", segment, front.tenant),
                diagnostics,
            );
        }
    }
}

fn validate_slug(value: &str, location: String, diagnostics: &mut Vec<Diagnostic>) {
    // The test itself lives in model::is_valid_slug, which the write path uses as
    // well; do not write a second copy here
    if !crate::model::is_valid_slug(value) {
        diagnostics.push(Diagnostic::error(
            "label.charset",
            location,
            format!("slug「{value}」不符合 [a-z0-9._-]{{1,32}}"),
        ));
    }
}

/// Port conflicts must be examined per protocol.
///
/// One number on TCP and on UDP are two different ports, and the kernel permits binding
/// both. Checking them together yields false alarms — and the most confusing kind: the
/// report says 51820 is already taken by WireGuard, while WireGuard is not on TCP at
/// all, leaving someone unable to make sense of a legitimate configuration.
///
/// Two things are UDP here: wg, and a Hysteria 2 ingress. Everything else — the VLESS
/// shapes, relay ports, phantun's fake-TCP ports — is TCP. An ingress can carry both wire
/// families, so this is a list rather than a protocol-valued property; both validation layers
/// consume the same list from `occupied_ingress_ports`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Proto {
    Tcp,
    Udp,
}

impl std::fmt::Display for Proto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Proto::Tcp => "TCP",
            Proto::Udp => "UDP",
        })
    }
}

#[derive(Clone, Copy)]
struct OccupiedIngressPort {
    proto: Proto,
    port: u16,
    suffix: &'static str,
}

/// Every socket or redirected port range claimed by one ingress.
///
/// This is the sole definition shared by the per-project and cross-project checks. In
/// particular, `Ingress::port` belongs only to the VLESS half; Hysteria 2 owns its own UDP
/// listener and optional hopping range, while a split REALITY/XHTTP download owns another TCP
/// listener.
fn occupied_ingress_ports(ingress: &Ingress) -> Vec<OccupiedIngressPort> {
    let mut occupied = Vec::new();

    if ingress.wires.vless().is_some() {
        occupied.push(OccupiedIngressPort {
            proto: Proto::Tcp,
            port: ingress.port,
            suffix: "",
        });
    }

    if let Some(hysteria2) = ingress.wires.hysteria2() {
        occupied.push(OccupiedIngressPort {
            proto: Proto::Udp,
            port: hysteria2.port,
            suffix: " 的 Hysteria 2",
        });
        if let Some(hop) = &hysteria2.hop {
            for port in hop.start..=hop.end {
                if port != hysteria2.port {
                    occupied.push(OccupiedIngressPort {
                        proto: Proto::Udp,
                        port,
                        suffix: " 的端口跳转区间",
                    });
                }
            }
        }
    }

    if matches!(ingress.wires.vless(), Some(Transport::VlessRealityXhttp(_))) {
        let mut download_ports = [
            ingress.projection.v4.as_ref(),
            ingress.projection.v6.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter_map(|endpoint| {
            endpoint
                .download
                .as_ref()
                .map(|download| download.node_port())
        })
        .collect::<Vec<_>>();
        download_ports.sort_unstable();
        download_ports.dedup();
        occupied.extend(download_ports.into_iter().map(|port| OccupiedIngressPort {
            proto: Proto::Tcp,
            port,
            suffix: " 的 TLS 下载前置",
        }));
    }

    occupied
}

fn ingress_port_owner(ingress_id: &str, suffix: &str) -> String {
    format!("接入面 {ingress_id}{suffix}")
}

fn validate_ports(sys: &SystemIr, app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    for node in &app.nodes {
        let mut used = BTreeMap::<(Proto, u16), String>::new();
        let mut put =
            |proto: Proto, port: Option<u16>, what: String, diagnostics: &mut Vec<Diagnostic>| {
                let Some(port) = port else {
                    return;
                };
                if let Some(previous) = used.insert((proto, port), what.clone()) {
                    diagnostics.push(Diagnostic::error(
                        "node.port-clash",
                        &node.id,
                        format!("{proto} 端口 {port} 同时被 {previous} 和 {what} 占用"),
                    ));
                }
            };

        if let Some(system_node) = sys.nodes.iter().find(|candidate| candidate.id == node.id) {
            // WireGuard is the only UDP at this layer. It coexists perfectly well with
            // a TCP port of the same number — which is exactly how the fake-TCP variant
            // works: phantun accepts TCP and forwards to the local wg's UDP.
            put(
                Proto::Udp,
                system_node
                    .wireguard
                    .as_ref()
                    .and_then(|wireguard| wireguard.listen_port),
                "WireGuard".to_owned(),
                diagnostics,
            );
            // Each chain's inbound on this machine occupies its own port. Two chains
            // colliding on one machine means two inbounds fighting over one bind,
            // reported here rather than left to surface as a failed xray start.
            for step in app.steps.iter().filter(|step| step.node == node.id) {
                if let Some(hop_in) = &step.hop_in {
                    put(
                        Proto::Tcp,
                        Some(hop_in.port),
                        format!("链 {} 的中转 inbound", step.chain),
                        diagnostics,
                    );
                }
            }
            // A fake-TCP port is a port really bound on this machine, and a collision
            // means it will not start — without this check the symptom is a phantun
            // server failing to start while the config looks entirely correct. Ask the
            // link rather than the node: the server is not necessarily hosted on the
            // declaring side, and with that side behind NAT it moves to the peer
            // (`ir::system::LinkWrap`), where the port is bound. Deduplicated by
            // port.
            let mut fake_tcp_ports = sys
                .links
                .iter()
                .filter_map(|link| match &link.wrap {
                    crate::ir::system::LinkWrap::FakeTcp { servers } => servers.get(&node.id),
                    crate::ir::system::LinkWrap::Udp => None,
                })
                .filter_map(|endpoint| endpoint.rsplit_once(':'))
                .filter_map(|(_, port)| port.parse::<u16>().ok())
                .collect::<Vec<_>>();
            fake_tcp_ports.sort_unstable();
            fake_tcp_ports.dedup();
            for port in fake_tcp_ports {
                put(
                    Proto::Tcp,
                    Some(port),
                    "phantun 伪 TCP 口".to_owned(),
                    diagnostics,
                );
            }
        }
        put(
            Proto::Tcp,
            node.api_port,
            "api 取数入口".to_owned(),
            diagnostics,
        );

        for ingress in app
            .ingresses
            .iter()
            .filter(|ingress| ingress.node == node.id)
        {
            for occupied in occupied_ingress_ports(ingress) {
                put(
                    occupied.proto,
                    Some(occupied.port),
                    ingress_port_owner(&ingress.id, occupied.suffix),
                    diagnostics,
                );
            }
        }
    }
}

fn validate_hysteria2(diagnostics: &mut Vec<Diagnostic>, ingress: &Ingress, settings: &Hysteria2) {
    if settings.port == 0 {
        diagnostics.push(Diagnostic::error(
            "ingress.hy2-port",
            &ingress.id,
            format!("接入面 {} 的 Hysteria 2 监听端口不能是 0", ingress.id),
        ));
    }
    // TCP and UDP are separate spaces, so sharing a number collides with nothing at the socket
    // layer — `validate_ports` will not say a word. It is refused anyway: a hop range is a rule
    // applied to a run of numbers, and the first time somebody writes one without `-p udp` the
    // TCP wire on the same number goes down with it. Separate numbers make that unrepresentable.
    if ingress.wires.vless().is_some() && settings.port == ingress.port {
        diagnostics.push(Diagnostic::error(
            "ingress.hy2-port-shared",
            &ingress.id,
            format!(
                "接入面 {} 的 Hysteria 2 端口 {} 与 VLESS 侧相同，两条线需要各自使用独立端口",
                ingress.id, settings.port
            ),
        ));
    }
    if let Some(hop) = &settings.hop {
        if hop.start == 0 || hop.start > hop.end {
            diagnostics.push(Diagnostic::error(
                "ingress.hy2-hop-range",
                &ingress.id,
                format!(
                    "接入面 {} 的端口跳转区间 {}-{} 无效",
                    ingress.id, hop.start, hop.end
                ),
            ));
        } else if !hop.contains(settings.port) {
            // The server binds one port; every other port in the range reaches it only through
            // the machine's redirect. A range excluding the listener therefore leaves the one
            // port that works outside the set clients are told to rotate through — and the
            // server starts perfectly cleanly either way.
            diagnostics.push(Diagnostic::error(
                "ingress.hy2-hop-listener",
                &ingress.id,
                format!(
                    "接入面 {} 的端口跳转区间 {}-{} 不包含监听口 {}",
                    ingress.id, hop.start, hop.end, settings.port
                ),
            ));
        }
    }
    let up = settings.bandwidth.up.as_deref().map(str::trim);
    let down = settings.bandwidth.down.as_deref().map(str::trim);
    let up = up.filter(|value| !value.is_empty());
    let down = down.filter(|value| !value.is_empty());
    if up.is_some() != down.is_some() {
        diagnostics.push(Diagnostic::error(
            "ingress.hy2-bandwidth",
            &ingress.id,
            format!(
                "接入面 {} 的 Hysteria 2 带宽需要上下行同时填写或同时留空",
                ingress.id
            ),
        ));
    }
    for (direction, value) in [("上行", up), ("下行", down)] {
        let Some(value) = value else { continue };
        match hysteria_bandwidth_bytes_per_sec(value) {
            Ok(rate) if rate >= 65_536 => {}
            Ok(rate) => diagnostics.push(Diagnostic::error(
                "ingress.hy2-bandwidth",
                &ingress.id,
                format!(
                    "接入面 {} 的 Hysteria 2 {direction}带宽为 {rate} B/s，最小值为 65536 B/s",
                    ingress.id
                ),
            )),
            Err(reason) => diagnostics.push(Diagnostic::error(
                "ingress.hy2-bandwidth",
                &ingress.id,
                format!(
                    "接入面 {} 的 Hysteria 2 {direction}带宽「{value}」无效：{reason}",
                    ingress.id
                ),
            )),
        }
    }
    /* `force-brutal` is the one setting that does not fall back to BBR: xray demands a value for
    `up` while building the config (`if up == 0 { return errors.New("force-brutal requires up") }`).
    Without this check the save reports success, and the failure surfaces when the agent applies it
    and xray does not start — at which point every ingress on that machine stops serving. */
    if settings.congestion == HysteriaCongestion::ForceBrutal && up.is_none() {
        diagnostics.push(Diagnostic::error(
            "ingress.hy2-force-brutal-needs-up",
            &ingress.id,
            format!(
                "接入面 {} 的拥塞控制选择了 force-brutal，必须填写上行带宽",
                ingress.id
            ),
        ));
    }
    validate_hysteria2_quic(diagnostics, ingress, &settings.quic);
    if let Some(HysteriaObfs::Salamander { password }) = &settings.obfs {
        if password.trim().is_empty() {
            diagnostics.push(Diagnostic::error(
                "ingress.hy2-obfs-blank",
                &ingress.id,
                format!("接入面 {} 启用了 Salamander，但密码为空", ingress.id),
            ));
        }
    }
    if let HysteriaMasquerade::Proxy { url } = &settings.masquerade {
        let rest = url.trim().strip_prefix("https://");
        let valid = rest.is_some_and(|rest| {
            let authority = rest.split('/').next().unwrap_or_default();
            !authority.is_empty() && !authority.chars().any(char::is_whitespace)
        });
        if !valid {
            diagnostics.push(Diagnostic::error(
                "ingress.hy2-masquerade",
                &ingress.id,
                format!(
                    "接入面 {} 的 Hysteria 2 反代伪装必须是完整的 https:// 地址",
                    ingress.id
                ),
            ));
        }
    }
}

/// The QUIC knobs Xray range-checks while building the config. Same bounds, same reason as
/// `force-brutal` above: a value outside them stops xray from starting, and the cheapest place to
/// learn that is here, not on the machine.
///
/// The columns carry the same CHECKs. Two copies is deliberate — the constraint is what keeps bad
/// values out of the database, this is what tells the operator which ingress and which field.
fn validate_hysteria2_quic(
    diagnostics: &mut Vec<Diagnostic>,
    ingress: &Ingress,
    quic: &HysteriaQuic,
) {
    let mut reject = |what: &str, detail: String| {
        diagnostics.push(Diagnostic::error(
            "ingress.hy2-quic-range",
            &ingress.id,
            format!("接入面 {} 的 Hysteria 2 {what}{detail}", ingress.id),
        ));
    };
    for (what, value) in [
        ("流初始接收窗口", quic.init_stream_receive_window),
        ("流最大接收窗口", quic.max_stream_receive_window),
        ("连接初始接收窗口", quic.init_connection_receive_window),
        ("连接最大接收窗口", quic.max_connection_receive_window),
    ] {
        if let Some(value) = value {
            if value < HysteriaQuic::MIN_RECEIVE_WINDOW {
                reject(
                    what,
                    format!(
                        "为 {value} 字节，最小值为 {} 字节",
                        HysteriaQuic::MIN_RECEIVE_WINDOW
                    ),
                );
            }
        }
    }
    if let Some(value) = quic.max_idle_timeout_secs {
        if !(HysteriaQuic::MIN_IDLE_TIMEOUT_SECS..=HysteriaQuic::MAX_IDLE_TIMEOUT_SECS)
            .contains(&value)
        {
            reject(
                "空闲超时",
                format!(
                    "为 {value} 秒，需要在 {}–{} 之间",
                    HysteriaQuic::MIN_IDLE_TIMEOUT_SECS,
                    HysteriaQuic::MAX_IDLE_TIMEOUT_SECS
                ),
            );
        }
    }
    if let Some(value) = quic.keep_alive_period_secs {
        if !(HysteriaQuic::MIN_KEEP_ALIVE_SECS..=HysteriaQuic::MAX_KEEP_ALIVE_SECS).contains(&value)
        {
            reject(
                "保活间隔",
                format!(
                    "为 {value} 秒，需要在 {}–{} 之间",
                    HysteriaQuic::MIN_KEEP_ALIVE_SECS,
                    HysteriaQuic::MAX_KEEP_ALIVE_SECS
                ),
            );
        }
    }
    if let Some(value) = quic.max_incoming_streams {
        if value < HysteriaQuic::MIN_INCOMING_STREAMS {
            reject(
                "最大并发流",
                format!(
                    "为 {value}，最小值为 {}",
                    HysteriaQuic::MIN_INCOMING_STREAMS
                ),
            );
        }
    }
}

/// Match Xray v26.4.25's `Bandwidth.Bps`: accepted suffixes are bit-rate units and the result is
/// converted to bytes per second before the 64 KiB/s floor is checked.
fn hysteria_bandwidth_bytes_per_sec(value: &str) -> Result<u64, &'static str> {
    let value = value.trim().to_ascii_lowercase();
    let split = value
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit() && *ch != '.')
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    let number = value[..split]
        .parse::<f64>()
        .map_err(|_| "数字部分无法解析")?;
    if !number.is_finite() || number < 0.0 {
        return Err("数字必须是有限的非负值");
    }
    let multiplier = match value[split..].trim() {
        "" | "b" | "bps" => 1_u64,
        "k" | "kb" | "kbps" => 1024,
        "m" | "mb" | "mbps" => 1024 * 1024,
        "g" | "gb" | "gbps" => 1024 * 1024 * 1024,
        "t" | "tb" | "tbps" => 1024_u64.pow(4),
        _ => return Err("单位只支持 bps/kbps/mbps/gbps/tbps"),
    };
    Ok((number * multiplier as f64) as u64 / 8)
}

fn validate_labels(app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    for node in &app.nodes {
        let mut labels = BTreeMap::<String, String>::new();
        let mut put = |label: &str, what: String, diagnostics: &mut Vec<Diagnostic>| {
            if let Some(previous) = labels.insert(label.to_owned(), what.clone()) {
                diagnostics.push(Diagnostic::error(
                    "label.duplicate",
                    &node.id,
                    format!("label「{label}」被 {previous} 和 {what} 同时使用"),
                ));
            }
        };

        for step in app
            .steps
            .iter()
            .filter(|step| step.node == node.id)
            .filter_map(|step| step.accept.as_ref().map(|accept| (step, accept)))
        {
            put(
                &step.1.label,
                format!("链 {} 的接受凭据", step.0.chain),
                diagnostics,
            );
        }

        for grant in &app.grants {
            let Some(ingress) = app
                .ingresses
                .iter()
                .find(|ingress| ingress.id == grant.ingress)
            else {
                continue;
            };
            if ingress.node == node.id {
                put(
                    &grant.label,
                    format!("用户 {}/{}", grant.tenant, grant.user),
                    diagnostics,
                );
            }
        }
    }
}

fn validate_steps(sys: &SystemIr, app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    for step in &app.steps {
        validate_forward_dials(step, diagnostics);

        let Some(last) = step.rules.last() else {
            diagnostics.push(Diagnostic::error(
                "rule.no-default",
                format!("{}/{}", step.chain, step.node),
                "规则表为空，末条不是 Any",
            ));
            continue;
        };

        if !matches!(last.dest_match, DestMatch::Any) {
            diagnostics.push(Diagnostic::error(
                "rule.no-default",
                format!("{}/{}", step.chain, step.node),
                "规则表末条不是 Any",
            ));
        }

        // Reverse access's downstream is not bound by this check. Its `accept` is its
        // own identity, not a key others dial it with (see the credential passage in
        // `ir/hops.rs`) — it does not listen at all and dials the upstream itself.
        // Requiring it to be reachable would require it to join the backbone or open a
        // public port, and "an exit need not join the backbone" is precisely why this
        // variant exists (argued on `HopDial::Reverse`).
        let reverse_downstream = app.hops.iter().any(|hop| {
            hop.chain == step.chain && hop.to == step.node && hop.path == HopPath::Reverse
        });

        // What is required is being able to receive relayed traffic, not being on the
        // overlay: a machine with a public relay port receives it without wg. A machine
        // that made it into `SystemIr` satisfies at least one of the two.
        if step.accept.is_some()
            && !reverse_downstream
            && !sys.nodes.iter().any(|node| node.id == step.node)
        {
            diagnostics.push(Diagnostic::error(
                "step.accept-unreachable",
                format!("{}/{}", step.chain, step.node),
                format!(
                    "{} 作为中继不可达：未加入 overlay 且没有公网中转端口",
                    step.node
                ),
            ));
        }

        let Some(node) = app.nodes.iter().find(|node| node.id == step.node) else {
            continue;
        };

        for rule in &step.rules {
            if has_empty_match(rule) {
                diagnostics.push(Diagnostic::error(
                    "rule.empty-match",
                    format!("{}/{}", step.chain, step.node),
                    "规则缺少匹配值",
                ));
            }
            if has_unrepresentable_all_match(&rule.dest_match) {
                diagnostics.push(Diagnostic::error(
                    "rule.all-unrepresentable",
                    format!("{}/{}", step.chain, step.node),
                    "All 条件落到同一 xray 字段，交集不可表达",
                ));
            }
            if matches!(rule.action, Action::Egress { .. } | Action::Proxy { .. })
                && !node.egress_allowed
            {
                diagnostics.push(Diagnostic::error(
                    "step.egress-denied",
                    format!("{}/{}", step.chain, step.node),
                    format!("{} 禁止出网：存在落地或外部代理动作", step.node),
                ));
            }
        }
    }
}

/// A shape that presents its own certificate needs the machine to hold one.
///
/// An error rather than a warning, and refused rather than compiled: with no certificate the
/// inbound names files that are not there, xray refuses the configuration at startup, and the
/// machine keeps serving whatever it had until somebody reads a log. The subscription is worse —
/// it would carry an empty name, so the client fails its own certificate check before dialing.
///
/// The certificate arrives outside the model, so this can turn from passing to failing without
/// anybody editing anything: issuing is asynchronous, and a machine enrolled a minute ago has
/// none yet. That is the intended reading — the ingress is not ready, and saying so is more use
/// than a green compile of something that cannot carry traffic.
/// The one refusal that needs something the entrance may not have.
///
/// Blocking BitTorrent matches on what the sniffer decided the connection is speaking, and an
/// entrance behind a front does not sniff (`sniff` in `ir/routing.rs`) — the rule compiles, ships,
/// matches nothing, and the console goes on showing the switch as on. Silent failure in the safety
/// direction is the worst kind: the operator believes the entrance is guarded and it is not.
///
/// An error rather than a warning, because there is a correct move in both directions and the
/// operator has to pick one: drop the front, or accept that this entrance cannot refuse torrents.
/// Neither is something the compiler may choose on their behalf.
fn validate_ingress_guard(diagnostics: &mut Vec<Diagnostic>, ingress: &Ingress) {
    if ingress.guard.no_bittorrent && !ingress.sniff {
        diagnostics.push(Diagnostic::error(
            "ingress.guard-needs-sniffing",
            &ingress.id,
            format!(
                "接入面 {} 使用了前置代理、不做协议嗅探，「禁止 BT」在该场景下不会生效。\
                 请移除前置代理，或关闭该开关",
                ingress.id
            ),
        ));
    }
}

fn validate_ingress_certificate(diagnostics: &mut Vec<Diagnostic>, ingress: &Ingress) {
    let split_download = matches!(ingress.wires.vless(), Some(Transport::VlessRealityXhttp(_)))
        && [
            ingress.projection.v4.as_ref(),
            ingress.projection.v6.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|endpoint| endpoint.download.is_some());
    if !ingress.wires.needs_node_certificate() && !split_download {
        return;
    }
    if ingress
        .certificate_name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
    {
        return;
    }
    diagnostics.push(Diagnostic::error(
        "ingress.tls-no-certificate",
        &ingress.id,
        format!(
            "接入面 {} 使用自有证书，但机器 {} 没有可用的证书。\
             去机器页确认它选了证书组，再去证书页确认那个组已经签发出证书；\
             或者把这个接入面改成 REALITY 指向外部站点，那样不需要本机证书",
            ingress.id, ingress.node
        ),
    ));
}

/// The HTTP layer's own rules, none of which xray will catch for us.
///
/// # Why Vision and XHTTP still meet here
///
/// The shape itself no longer lets the two be *chosen* together — that is what naming whole
/// combinations in [`Transport`](crate::model::Transport) buys. But flow is inherited: an ingress
/// that leaves it unset takes the fleet's, and the fleet's default is Vision. So the pair can
/// still arrive here without anybody having picked it, and it still has to be caught: xray
/// refuses it at runtime with `XTLS only supports TLS and REALITY directly for now`, and refuses
/// it *only* at runtime — `xray -test` reports `Configuration OK` on the pair. Measured on
/// 26.4.25 by running it. An ingress like that deploys green, passes its own config check, and
/// drops every connection.
fn validate_ingress_stream(diagnostics: &mut Vec<Diagnostic>, ingress: &Ingress) {
    let Some(xhttp) = ingress.wires.xhttp() else {
        return;
    };
    if ingress.wires.flow().is_some_and(|flow| !flow.is_empty()) {
        diagnostics.push(Diagnostic::error(
            "ingress.xhttp-flow-conflict",
            &ingress.id,
            format!(
                "接入面 {} 使用 XHTTP，但继承了流控 {}，xray 在运行时会拒绝连接（XTLS 只支持直连的 TLS/REALITY）。\
                 请在该接入面上关闭流控，或改回 TCP",
                ingress.id,
                ingress.wires.flow().unwrap_or_default()
            ),
        ));
    }

    // The path is matched literally by the server and a mismatch is refused outright, so a
    // malformed one is not a cosmetic problem: it is an ingress nobody can reach.
    if !xhttp.path.starts_with('/') {
        diagnostics.push(Diagnostic::error(
            "ingress.xhttp-path",
            &ingress.id,
            format!("接入面 {} 的 XHTTP 路径要以 / 开头", ingress.id),
        ));
    }
    if xhttp
        .path
        .chars()
        .any(|c| c.is_whitespace() || c == '?' || c == '#')
    {
        diagnostics.push(Diagnostic::error(
            "ingress.xhttp-path",
            &ingress.id,
            format!(
                "接入面 {} 的 XHTTP 路径中包含空白或 ? #，这些字符会被解析为查询串和片段，导致两端路径不一致",
                ingress.id
            ),
        ));
    }

    // Refused rather than clamped, on the same reasoning as a relay hop's pool: a value silently
    // changed reads back as something the operator did not choose.
    if let Some(mux) = xhttp.mux {
        if !(Xhttp::MUX_MIN..=Xhttp::MUX_MAX).contains(&mux) {
            diagnostics.push(Diagnostic::error(
                "ingress.xhttp-mux-range",
                &ingress.id,
                format!(
                    "接入面 {} 的并发数 {mux} 超出 {}–{} 的范围。1 表示连接池（一条连接同时只承载一条流，空闲后复用），\
                     留空不表示关闭复用，而是所有流共用一条连接",
                    ingress.id,
                    Xhttp::MUX_MIN,
                    Xhttp::MUX_MAX
                ),
            ));
        }
    }

    if xhttp
        .host
        .as_deref()
        .is_some_and(|host| host.trim().is_empty())
    {
        diagnostics.push(Diagnostic::error(
            "ingress.xhttp-host",
            &ingress.id,
            format!(
                "接入面 {} 的 XHTTP Host 不能为空白字符；不需要设置时请留空",
                ingress.id
            ),
        ));
    }

    for endpoint in [
        ingress.projection.v4.as_ref(),
        ingress.projection.v6.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        let Some(download) = &endpoint.download else {
            continue;
        };
        if xhttp.mode == XhttpMode::StreamOne {
            diagnostics.push(Diagnostic::error(
                "ingress.xhttp-download-stream-one",
                &ingress.id,
                format!(
                    "接入面 {} 配置了独立下载，但 stream-one 将上下行放在同一个请求中；请改用 auto 或 stream-up",
                    ingress.id
                ),
            ));
        }
        if download
            .http_host
            .as_deref()
            .is_some_and(|host| host.trim().is_empty())
        {
            diagnostics.push(Diagnostic::error(
                "ingress.projection-download-http-host",
                &ingress.id,
                format!(
                    "接入面 {} 的下载 HTTP Host 不能为空白字符；不需要设置时请留空",
                    ingress.id
                ),
            ));
        }
        if let Some(mux) = download.mux {
            if !(Xhttp::MUX_MIN..=Xhttp::MUX_MAX).contains(&mux) {
                diagnostics.push(Diagnostic::error(
                    "ingress.projection-download-mux-range",
                    &ingress.id,
                    format!(
                        "接入面 {} 的下载并发数 {mux} 超出 {}–{} 的范围。1 表示连接池，留空表示所有流共用一条连接",
                        ingress.id,
                        Xhttp::MUX_MIN,
                        Xhttp::MUX_MAX
                    ),
                ));
            }
        }
    }

    // A warning rather than an error: it is a legitimate choice, and an operator who wants a
    // caching CDN in front has no other one. What makes it worth saying out loud is that a server
    // given any explicit mode stops accepting clients that name a different one, and what a
    // client left at `auto` names is not one value — measured on 26.4.25 it comes out
    // `stream-one` under REALITY and `packet-up` under TLS. So whether the subscriptions already
    // in people's hands still work after this is set cannot be answered from the setting alone,
    // which is precisely why it is worth a line rather than silence.
    if xhttp.mode == XhttpMode::PacketUp {
        diagnostics.push(Diagnostic::warn(
            "ingress.xhttp-mode-packet-up",
            &ingress.id,
            format!(
                "接入面 {} 的上行模式设为 packet-up，服务端此后只接受该模式。\
                 未设置该项的客户端会自行选择上行模式——在 REALITY 下会使用 stream-one 并被拒绝；\
                 修改前已下发的订阅需要重新获取",
                ingress.id
            ),
        ));
    }
}

fn validate_forward_dials(step: &super::routing::Step, diagnostics: &mut Vec<Diagnostic>) {
    // Both fields describe the one outbound this edge compiles to, so both have to agree
    // across every rule pointing at the same target — two rules asking for different
    // connection handling are asking for two outbounds, and there is only ever one.
    let mut by_target = BTreeMap::<&str, (&HopDial, &HopPool)>::new();

    for rule in &step.rules {
        let Action::Forward { to, dial, pool } = &rule.action else {
            continue;
        };
        let at = format!("{}/{}->{}", step.chain, step.node, to);

        // Reverse has the peer open the connection; this machine holds a virtual outbound
        // onto a tunnel that is already up. There is nothing to pool, so a pool set here is
        // not a harmless leftover — it is a setting the operator believes is in effect.
        if matches!(dial, HopDial::Reverse(_)) && *pool != HopPool::None {
            diagnostics.push(Diagnostic::error(
                "rule.pool-on-reverse",
                at.clone(),
                format!("{to} 是反向接入，本机不发起连接，不能配置出站连接"),
            ));
        }

        // Refused rather than clamped. xray caps `concurrency` at 128 and reads 0 as 8, so
        // reproducing either would leave the console showing one number while the machine
        // runs another.
        if let HopPool::Merge(n) = pool {
            if !(HopPool::MERGE_MIN..=HopPool::MERGE_MAX).contains(n) {
                diagnostics.push(Diagnostic::error(
                    "rule.pool-range",
                    at.clone(),
                    if *n < HopPool::MERGE_MIN {
                        format!(
                            "{to} 的合并流数为 {n}，最小值为 {}；一条流对应一条连接对应连接池档位，请选择该档",
                            HopPool::MERGE_MIN
                        )
                    } else {
                        format!("{to} 的合并流是 {n}，最多 {}", HopPool::MERGE_MAX)
                    },
                ));
            }
        }

        let Some((previous_dial, previous_pool)) = by_target.insert(to.as_str(), (dial, pool))
        else {
            continue;
        };
        if previous_dial != dial {
            diagnostics.push(Diagnostic::error(
                "rule.forward-dial-conflict",
                at.clone(),
                format!("{to} 存在多种 dial 配置；同一条链边只允许一种"),
            ));
        }
        if previous_pool != pool {
            diagnostics.push(Diagnostic::error(
                "rule.forward-pool-conflict",
                at,
                format!("{to} 存在多种出站连接配置；同一条链边只允许一种"),
            ));
        }
    }
}

fn validate_topology(app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    for chain in &app.chains {
        // The head is the machine hosting the ingress. A chain with no ingress was
        // already reported as `chain.no-ingress`, and the topology check has no starting
        // point, so it is skipped.
        let Some(root) = chain.root.as_deref() else {
            continue;
        };
        let steps = app
            .steps
            .iter()
            .filter(|step| step.chain == chain.id)
            .collect::<Vec<_>>();
        let mut incoming = BTreeMap::<&str, BTreeSet<&str>>::new();

        for step in &steps {
            incoming.entry(step.node.as_str()).or_default();
            for rule in &step.rules {
                if let Action::Forward { to, .. } = &rule.action {
                    incoming
                        .entry(to.as_str())
                        .or_default()
                        .insert(step.node.as_str());
                }
            }
        }

        // This guards `compile_chain_steps`'s invariant: it builds steps only for nodes
        // reachable from the head by BFS, so every step but the head's should have an
        // upstream. Reporting nothing today is correct; should compilation ever change to
        // copy `app.steps` directly, this catches the orphan nodes that leak through.
        for (node, sources) in &incoming {
            if *node == root {
                continue;
            }
            if sources.is_empty() {
                diagnostics.push(Diagnostic::error(
                    "chain.unreachable",
                    format!("{}/{}", chain.id, node),
                    format!("{node} 没有上游转发规则指向它"),
                ));
            }
        }

        let mut stack = BTreeSet::new();
        let mut visited = BTreeSet::new();
        detect_cycle(
            &chain.id,
            root,
            &steps,
            &mut stack,
            &mut visited,
            diagnostics,
        );

        for step in &steps {
            let forwards = step
                .rules
                .iter()
                .any(|rule| matches!(rule.action, Action::Forward { .. }));
            if !forwards {
                let terminal = step.rules.iter().any(|rule| {
                    matches!(
                        rule.action,
                        Action::Egress { .. } | Action::Proxy { .. } | Action::Block
                    )
                });
                if !terminal {
                    diagnostics.push(Diagnostic::error(
                        "step.dead-leaf",
                        format!("{}/{}", chain.id, step.node),
                        format!("{} 既不转发也不出网", step.node),
                    ));
                }
            }
        }
    }
}

fn detect_cycle(
    chain: &str,
    node: &str,
    steps: &[&super::routing::Step],
    stack: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if stack.contains(node) {
        diagnostics.push(Diagnostic::error(
            "chain.cycle",
            format!("{chain}/{node}"),
            format!("{node} 在链 {chain} 上形成了环"),
        ));
        return;
    }
    if !visited.insert(node.to_owned()) {
        return;
    }

    stack.insert(node.to_owned());
    if let Some(step) = steps.iter().find(|step| step.node == node) {
        for rule in &step.rules {
            if let Action::Forward { to, .. } = &rule.action {
                detect_cycle(chain, to, steps, stack, visited, diagnostics);
            }
        }
    }
    stack.remove(node);
}

fn validate_fronts(app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    for front in &app.fronts {
        // The group is empty while exit ingresses still point at it: the subscription
        // would present a policy group with no members, the client can select no node,
        // and not one ingress under that group connects. A warning rather than an error —
        // the most common cause is a member machine being decommissioned, where the
        // decommissioning should still ship, just not silently.
        if front.via.is_empty() && has_downstream(app, &front.id) {
            diagnostics.push(Diagnostic::warn(
                "front.no-via",
                &front.id,
                format!("前置组 {} 没有可用成员，该组下的接入面无法连接", front.id),
            ));
        }

        for via in &front.via {
            let Some(via_ingress) = app.ingresses.iter().find(|ingress| ingress.id == *via) else {
                diagnostics.push(Diagnostic::error(
                    "front.unknown-via",
                    &front.id,
                    format!("前置组指向的接入面 {via} 不存在"),
                ));
                continue;
            };

            if let Some(node) = app.nodes.iter().find(|node| node.id == via_ingress.node) {
                if !node.egress_allowed {
                    diagnostics.push(Diagnostic::error(
                        "front.via-no-egress",
                        format!("{}/{}", front.id, via_ingress.id),
                        format!(
                            "前置组成员 {} 所在节点 {} 不允许出网",
                            via_ingress.id, node.id
                        ),
                    ));
                }
            }

            for step in app
                .steps
                .iter()
                .filter(|step| step.chain == via_ingress.chain)
            {
                let Some(last) = step.rules.last() else {
                    continue;
                };
                if matches!(last.dest_match, DestMatch::Any)
                    && matches!(last.action, Action::Egress { .. } | Action::Proxy { .. })
                {
                    diagnostics.push(Diagnostic::error(
                        "front.via-open",
                        format!("{}/{}", via_ingress.id, step.node),
                        format!(
                            "{} 位于前置组「{}」成员 {} 的链上，但规则表以任意放行作为兜底",
                            step.node, front.name, via_ingress.id
                        ),
                    ));
                }
            }
        }
    }

    for grant in &app.grants {
        let Some(ingress) = app
            .ingresses
            .iter()
            .find(|ingress| ingress.id == grant.ingress)
        else {
            continue;
        };
        let Some(front_id) = ingress.front.as_deref() else {
            continue;
        };
        let Some(front) = app.fronts.iter().find(|front| front.id == front_id) else {
            continue;
        };
        // An empty group prompts no questions about grants. An empty via has only two
        // causes: the group has no members configured yet, or its members' machines were
        // decommissioned and `compile_app` dropped those vias. The latter is a
        // consequence of the decommissioning, and blocking on it would mean manually
        // deleting a swathe of grants before a machine hosting front ingresses could be
        // decommissioned — the same trap `front.unknown-via` fell into. An empty group
        // itself is reported by `front.no-via`.
        if front.via.is_empty() {
            continue;
        }

        let has_via_grant = app.grants.iter().any(|candidate| {
            candidate.tenant == grant.tenant
                && candidate.user == grant.user
                && front.via.contains(&candidate.ingress)
        });
        if !has_via_grant {
            diagnostics.push(Diagnostic::error(
                "front.grant-via",
                &grant.id,
                format!(
                    "用户 {}/{} 已获得 {} 的授权，但没有前置组 {} 中任何入口的授权",
                    grant.tenant, grant.user, grant.ingress, front.id
                ),
            ));
        }
    }

    // Ingresses fronted by a group: those with a non-empty front. These are the exits
    // under a group, what a client looks for after being relayed through it; a mistyped
    // group name reports front.missing, and a via member chain blocking them reports
    // front.blocked.
    for ingress in app
        .ingresses
        .iter()
        .filter(|ingress| ingress.front.is_some())
    {
        let Some(front) = ingress
            .front
            .as_ref()
            .and_then(|front_id| app.fronts.iter().find(|front| front.id == *front_id))
        else {
            diagnostics.push(Diagnostic::error(
                "front.missing",
                &ingress.id,
                format!(
                    "接入面引用的前置组 {} 不存在",
                    ingress.front.as_deref().unwrap_or("<none>")
                ),
            ));
            continue;
        };
        let Some(node) = app.nodes.iter().find(|node| node.id == ingress.node) else {
            continue;
        };
        let hosts = [node.public_ipv4.as_deref(), node.public_ipv6.as_deref()];

        for via in &front.via {
            let Some(via_ingress) = app.ingresses.iter().find(|candidate| candidate.id == *via)
            else {
                continue;
            };
            for host in hosts.into_iter().flatten() {
                let verdict = front_verdict(app, &via_ingress.chain, &via_ingress.node, host);
                match verdict.kind {
                    FrontVerdictKind::Ok => {}
                    FrontVerdictKind::Blocked => diagnostics.push(Diagnostic::error(
                        "front.blocked",
                        verdict.at,
                        format!(
                            "规则表拦截了 {host}，用户无法通过 {} 访问该地址",
                            ingress.id
                        ),
                    )),
                    // `Info` again, and the wording says why: it cannot be determined.
                    // Where the rules hold only dynamic matches such as geosite, static
                    // inspection cannot see whether it is admitted — the half that can be
                    // concluded is `front.blocked`, which is an error.
                    FrontVerdictKind::Maybe | FrontVerdictKind::Unknown => {
                        diagnostics.push(Diagnostic::info(
                            "front.unproven",
                            verdict.at,
                            format!("判定不了是否放行 {host}"),
                        ));
                    }
                }
            }
        }
    }
}

/// Whether any ingress hangs under this front group. Only those are the exits under a
/// group, and only they lose connectivity when the group empties.
fn has_downstream(app: &AppIr, front_id: &str) -> bool {
    app.ingresses
        .iter()
        .any(|ingress| ingress.front.as_deref() == Some(front_id))
}

fn validate_reality(app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    for ingress in &app.ingresses {
        // A shape presenting its own certificate has none of these to get wrong. Its own check —
        // that the machine actually holds a certificate — is `validate_ingress_certificate`.
        let Some(reality) = ingress.wires.reality() else {
            continue;
        };

        if !reality.uses_node_certificate_fallback() {
            if reality.server_names.is_empty() {
                diagnostics.push(Diagnostic::error(
                    "reality.no-sni",
                    &ingress.id,
                    "server_names 不能为空",
                ));
            }
            if !is_host_port(&reality.dest) {
                diagnostics.push(Diagnostic::error(
                    "reality.dest",
                    &ingress.id,
                    format!("dest「{}」不符合 host:port 格式", reality.dest),
                ));
            }
        }
        if let RealityFallbackLimits::Custom { upload, download } = &reality.fallback_limits {
            validate_fallback_rate(ingress, "upload", upload, diagnostics);
            validate_fallback_rate(ingress, "download", download, diagnostics);
        }
        if ingress.identity.short_ids.is_empty() {
            diagnostics.push(Diagnostic::error(
                "reality.short-id",
                &ingress.id,
                "short_ids 不能为空",
            ));
        }
        for short_id in &ingress.identity.short_ids {
            let ok = !short_id.is_empty()
                && short_id.len() <= 16
                && short_id.bytes().all(|byte| byte.is_ascii_hexdigit());
            if !ok {
                diagnostics.push(Diagnostic::error(
                    "reality.short-id",
                    &ingress.id,
                    format!("short_id「{short_id}」不是长度不超过 16 的十六进制字符串"),
                ));
            }
        }
    }
}

fn validate_fallback_rate(
    ingress: &Ingress,
    direction: &str,
    rate: &RealityFallbackRateLimit,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if rate.bytes_per_sec == 0
        || rate.burst_bytes_per_sec == 0
        || rate.burst_bytes_per_sec < rate.bytes_per_sec
    {
        diagnostics.push(Diagnostic::error(
            "reality.fallback-limit",
            &ingress.id,
            format!(
                "REALITY fallback {direction} 限速必须大于 0，且 burst_bytes_per_sec 不能小于 bytes_per_sec"
            ),
        ));
    }
}

fn validate_tenants(app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    for chain in &app.chains {
        for step in app.steps.iter().filter(|step| step.chain == chain.id) {
            let Some(node) = app.nodes.iter().find(|node| node.id == step.node) else {
                continue;
            };
            if !under(&chain.tenant, &node.tenant) {
                diagnostics.push(Diagnostic::error(
                    "tenant.scope",
                    format!("{}/{}", chain.id, node.id),
                    format!(
                        "链属于 {}，而 {} 归属于 {}，不在可见范围内",
                        chain.tenant, node.id, node.tenant
                    ),
                ));
            }
        }
    }

    for front in &app.fronts {
        for via in &front.via {
            let Some(ingress) = app.ingresses.iter().find(|ingress| ingress.id == *via) else {
                continue;
            };
            if !under(&front.tenant, &ingress.tenant) {
                diagnostics.push(Diagnostic::error(
                    "tenant.scope",
                    format!("{}/{}", front.id, ingress.id),
                    format!(
                        "前置组属于 {}，而成员 {} 归属于 {}，不在可见范围内",
                        front.tenant, ingress.id, ingress.tenant
                    ),
                ));
            }
        }
    }

    for grant in &app.grants {
        let Some(user) = app
            .users
            .iter()
            .find(|user| user.tenant == grant.tenant && user.id == grant.user)
        else {
            continue;
        };
        let Some(ingress) = app
            .ingresses
            .iter()
            .find(|ingress| ingress.id == grant.ingress)
        else {
            continue;
        };

        if grant.tenant != user.tenant || !under(&grant.tenant, &ingress.tenant) {
            diagnostics.push(Diagnostic::error(
                "tenant.scope",
                &grant.id,
                format!(
                    "授权属于 {}，用户属于 {}，接入面归属于 {}",
                    grant.tenant, user.tenant, ingress.tenant
                ),
            ));
        }
    }
}

fn validate_dns(app: &AppIr, diagnostics: &mut Vec<Diagnostic>) {
    for node in &app.nodes {
        let Dns::Servers(servers) = &node.dns else {
            continue;
        };

        for server in servers {
            if !is_dns_server_form(server) {
                diagnostics.push(Diagnostic::error(
                    "node.dns-form",
                    &node.id,
                    format!("解析器「{server}」格式无效"),
                ));
            }
        }
    }
}

fn unique_by<I, K>(values: I, code: &'static str, location: &str, diagnostics: &mut Vec<Diagnostic>)
where
    I: IntoIterator<Item = K>,
    K: Ord + ToString,
{
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value.to_string()) {
            diagnostics.push(Diagnostic::error(
                code,
                location,
                format!("{} 重复", value.to_string()),
            ));
        }
    }
}

fn has_empty_match(rule: &Rule) -> bool {
    match &rule.dest_match {
        DestMatch::Any | DestMatch::FrontDownstream => false,
        DestMatch::DomainSuffix(values)
        | DestMatch::DomainKeyword(values)
        | DestMatch::Geosite(values)
        | DestMatch::IpCidr(values)
        | DestMatch::Geoip(values)
        | DestMatch::Protocol(values)
        | DestMatch::Port(values) => {
            values.iter().any(|value| value.trim().is_empty()) || values.is_empty()
        }
        DestMatch::DomainRegex(value) => value.trim().is_empty(),
        DestMatch::PortExcept(values) => values.is_empty(),
        DestMatch::Network(_) => false,
        DestMatch::All(values) => {
            values.is_empty()
                || values.iter().any(|item| {
                    has_empty_match(&Rule {
                        dest_match: item.clone(),
                        action: Action::Block,
                    })
                })
        }
    }
}

fn has_unrepresentable_all_match(dest_match: &DestMatch) -> bool {
    let DestMatch::All(values) = dest_match else {
        return false;
    };

    let mut slots = MatchSlots::default();
    values
        .iter()
        .any(|value| !collect_match_slots(value, &mut slots))
}

fn collect_match_slots(dest_match: &DestMatch, slots: &mut MatchSlots) -> bool {
    match dest_match {
        DestMatch::Any => true,
        DestMatch::DomainSuffix(_)
        | DestMatch::DomainKeyword(_)
        | DestMatch::DomainRegex(_)
        | DestMatch::Geosite(_)
        | DestMatch::FrontDownstream => slots.put(MatchSlot::Domain),
        DestMatch::IpCidr(_) | DestMatch::Geoip(_) => slots.put(MatchSlot::Ip),
        DestMatch::Port(_) | DestMatch::PortExcept(_) => slots.put(MatchSlot::Port),
        DestMatch::Network(_) => slots.put(MatchSlot::Network),
        DestMatch::Protocol(_) => slots.put(MatchSlot::Protocol),
        DestMatch::All(values) => {
            !has_unrepresentable_all_match(dest_match)
                && values.iter().all(|value| collect_match_slots(value, slots))
        }
    }
}

#[derive(Debug, Default)]
struct MatchSlots {
    domain: bool,
    ip: bool,
    port: bool,
    network: bool,
    protocol: bool,
}

impl MatchSlots {
    fn put(&mut self, slot: MatchSlot) -> bool {
        let occupied = match slot {
            MatchSlot::Domain => &mut self.domain,
            MatchSlot::Ip => &mut self.ip,
            MatchSlot::Port => &mut self.port,
            MatchSlot::Network => &mut self.network,
            MatchSlot::Protocol => &mut self.protocol,
        };
        if *occupied {
            return false;
        }
        *occupied = true;
        true
    }
}

#[derive(Debug, Clone, Copy)]
enum MatchSlot {
    Domain,
    Ip,
    Port,
    Network,
    Protocol,
}

fn under(child: &str, parent: &str) -> bool {
    child == parent || child.starts_with(&format!("{parent}."))
}

fn is_dns_server_form(value: &str) -> bool {
    let value = value.trim();
    value == "localhost"
        || value == "fakedns"
        || value.parse::<Ipv4Addr>().is_ok()
        || value.contains(':')
        || starts_with_dns_scheme(value)
}

fn starts_with_dns_scheme(value: &str) -> bool {
    [
        "tcp://",
        "udp://",
        "https://",
        "quic://",
        "h2c://",
        "tcp+local://",
        "udp+local://",
    ]
    .iter()
    .any(|prefix| value.starts_with(prefix))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrontVerdictKind {
    Ok,
    Blocked,
    Maybe,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrontVerdict {
    kind: FrontVerdictKind,
    at: String,
}

fn front_verdict(app: &AppIr, chain: &str, node: &str, host: &str) -> FrontVerdict {
    front_verdict_inner(app, chain, node, host, &mut BTreeSet::new())
}

fn front_verdict_inner(
    app: &AppIr,
    chain: &str,
    node: &str,
    host: &str,
    seen: &mut BTreeSet<String>,
) -> FrontVerdict {
    let key = format!("{chain}|{node}");
    if !seen.insert(key) {
        return FrontVerdict {
            kind: FrontVerdictKind::Maybe,
            at: node.to_owned(),
        };
    }

    let Some(step) = app
        .steps
        .iter()
        .find(|step| step.chain == chain && step.node == node)
    else {
        return FrontVerdict {
            kind: FrontVerdictKind::Unknown,
            at: node.to_owned(),
        };
    };

    let mut dynamic_before_decision = false;
    for rule in &step.rules {
        match match_host(&rule.dest_match, host) {
            MatchVerdict::Hit => match &rule.action {
                Action::Egress { .. } => {
                    return FrontVerdict {
                        kind: if dynamic_before_decision {
                            FrontVerdictKind::Maybe
                        } else {
                            FrontVerdictKind::Ok
                        },
                        at: node.to_owned(),
                    };
                }
                Action::Proxy { .. } => {
                    return FrontVerdict {
                        kind: if dynamic_before_decision {
                            FrontVerdictKind::Maybe
                        } else {
                            FrontVerdictKind::Ok
                        },
                        at: node.to_owned(),
                    };
                }
                Action::Block => {
                    return FrontVerdict {
                        kind: if dynamic_before_decision {
                            FrontVerdictKind::Maybe
                        } else {
                            FrontVerdictKind::Blocked
                        },
                        at: node.to_owned(),
                    };
                }
                Action::Forward { to, .. } => {
                    let verdict = front_verdict_inner(app, chain, to, host, seen);
                    return if dynamic_before_decision {
                        FrontVerdict {
                            kind: FrontVerdictKind::Maybe,
                            at: verdict.at,
                        }
                    } else {
                        verdict
                    };
                }
            },
            MatchVerdict::Miss => {}
            MatchVerdict::Maybe => dynamic_before_decision = true,
        }
    }

    FrontVerdict {
        kind: if dynamic_before_decision {
            FrontVerdictKind::Maybe
        } else {
            FrontVerdictKind::Unknown
        },
        at: node.to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchVerdict {
    Hit,
    Miss,
    Maybe,
}

fn match_host(dest_match: &DestMatch, host: &str) -> MatchVerdict {
    match dest_match {
        DestMatch::Any => MatchVerdict::Hit,
        DestMatch::DomainSuffix(values) => {
            if values.iter().any(|value| domain_suffix_match(host, value)) {
                MatchVerdict::Hit
            } else {
                MatchVerdict::Miss
            }
        }
        DestMatch::DomainKeyword(values) => {
            if values.iter().any(|value| host.contains(value)) {
                MatchVerdict::Hit
            } else {
                MatchVerdict::Miss
            }
        }
        DestMatch::IpCidr(values) => ip_match(host, values),
        DestMatch::All(values) => {
            let mut maybe = false;
            for value in values {
                match match_host(value, host) {
                    MatchVerdict::Hit => {}
                    MatchVerdict::Miss => return MatchVerdict::Miss,
                    MatchVerdict::Maybe => maybe = true,
                }
            }
            if maybe {
                MatchVerdict::Maybe
            } else {
                MatchVerdict::Hit
            }
        }
        DestMatch::FrontDownstream => MatchVerdict::Miss,
        // Maybe rather than Miss: this walk knows a hostname and nothing else, and every one of
        // these reads something it cannot see from here — a geo list, the port, the transport, what
        // the sniffer found. Answering Miss would let a rule be pruned that fires in production.
        DestMatch::DomainRegex(_)
        | DestMatch::Geosite(_)
        | DestMatch::Geoip(_)
        | DestMatch::Port(_)
        | DestMatch::PortExcept(_)
        | DestMatch::Protocol(_)
        | DestMatch::Network(_) => MatchVerdict::Maybe,
    }
}

fn domain_suffix_match(host: &str, suffix: &str) -> bool {
    host == suffix || host.ends_with(&format!(".{suffix}"))
}

fn ip_match(host: &str, values: &[String]) -> MatchVerdict {
    let Ok(ip) = host.parse::<IpAddr>() else {
        return MatchVerdict::Miss;
    };

    for value in values {
        if let Ok(exact) = value.parse::<IpAddr>() {
            if exact == ip {
                return MatchVerdict::Hit;
            }
        }
        if let Ok(net) = value.parse::<ipnet::IpNet>() {
            if net.contains(&ip) {
                return MatchVerdict::Hit;
            }
        }
    }

    MatchVerdict::Miss
}

fn is_host_port(value: &str) -> bool {
    let Some((host, port)) = value.rsplit_once(':') else {
        return false;
    };
    !host.is_empty() && port.parse::<u16>().is_ok()
}
