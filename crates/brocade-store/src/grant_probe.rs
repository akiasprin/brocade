//! Frozen work list for an operator-triggered user authorization probe.
//!
//! The browser never supplies an address, port, protocol parameter or credential. Every field is
//! projected from the same fully converged Serving snapshot used by subscriptions; otherwise this
//! endpoint would be both an SSRF primitive and a test of a path subscribers were never given.

use std::collections::BTreeMap;

use brocade_core::{
    model::{
        ExternalOutbound, ExternalOutboundProtocol, HysteriaBbrProfile, HysteriaObfs, IpFamily,
    },
    physical::user::UserSecurityPlan,
};
use brocade_deployment::protocol::{
    E2eProbeAnyTls, E2eProbeHysteria2, E2eProbeReality, E2eProbeSecurity, E2eProbeTarget,
    E2eProbeTls, E2eProbeXhttp, E2eProbeXhttpRange, E2eProbeXhttpXmux,
};
use sqlx::PgPool;

use crate::{input::required_text, AdminContext, Result, StoreError};

#[derive(Debug, Clone)]
pub struct UserGrantProbePlan {
    pub serving_generation: u64,
    pub serving_revision: u64,
    pub endpoint_url: String,
    pub timeout_secs: u64,
    pub items: Vec<UserGrantProbeTarget>,
}

#[derive(Debug, Clone)]
pub struct UserGrantProbeTarget {
    /// Stable inside a Serving generation and free of credentials. It is returned to the browser
    /// and may come back only as a selection key; the target itself always stays server-side.
    pub id: String,
    pub name: String,
    pub app_id: String,
    pub app_name: String,
    pub chain_id: String,
    pub ingress_id: String,
    pub family: &'static str,
    pub protocol: &'static str,
    pub target: E2eProbeTarget,
}

/// One executable variant of a pinned Front matrix cell. A logical ingress can project more
/// than one family/protocol entry, so the complete cell is the Cartesian product of the user's
/// effective member and target entries rather than one guessed representative connection.
#[derive(Debug, Clone)]
pub struct FrontCombinationProbeTarget {
    pub id: String,
    pub name: String,
    pub app_id: String,
    pub app_name: String,
    pub front_id: String,
    pub front_name: String,
    pub member_id: String,
    pub member_name: String,
    pub member_family: &'static str,
    pub member_protocol: &'static str,
    pub target_id: String,
    pub target_name: String,
    pub target_family: &'static str,
    pub target_protocol: &'static str,
    pub member: FrontCombinationProbeMember,
    pub target: E2eProbeTarget,
}

/// Server-side material for the first hop. Internal members use the selected user's grant;
/// external members use the tunnel already embedded in the same Serving client snapshot.
#[derive(Debug, Clone)]
pub enum FrontCombinationProbeMember {
    Internal(E2eProbeTarget),
    External(ExternalOutbound),
}

#[derive(Debug, Clone)]
pub struct FrontCombinationProbePlan {
    pub serving_generation: u64,
    pub serving_revision: u64,
    pub client_snapshot_id: u64,
    pub endpoint_url: String,
    pub timeout_secs: u64,
    pub items: Vec<FrontCombinationProbeTarget>,
}

pub async fn user_grant_probe_plan(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<UserGrantProbePlan> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let user_id = required_text(user_id, "user_id")?;
    actor.require_tenant_access(&tenant_id, "user grant probe")?;

    let serving = crate::serving::load_subscription_serving_projection(pool).await?;
    serving.ensure_available()?;
    if !serving
        .snapshot
        .users
        .iter()
        .any(|user| user.tenant == tenant_id && user.id == user_id)
    {
        return Err(StoreError::NotFound(format!("user {tenant_id}/{user_id}")));
    }

    let output = crate::compile_cache::compile_incremental(&serving.snapshot);
    let user = crate::compile_cache::project_user(&output, &tenant_id, &user_id)?
        .as_ref()
        .clone();
    if user.entries.is_empty() {
        return Err(StoreError::NotFound(format!(
            "user {tenant_id}/{user_id} has no effective serving grants"
        )));
    }

    let mut items = Vec::with_capacity(user.entries.len());
    let mut self_signed_pins = BTreeMap::<String, Option<String>>::new();
    for entry in user.entries {
        let (app, ingress) = serving
            .snapshot
            .apps
            .iter()
            .find_map(|app| {
                app.ingresses
                    .iter()
                    .find(|ingress| ingress.id == entry.ingress_id)
                    .map(|ingress| (app, ingress))
            })
            .ok_or_else(|| {
                StoreError::InvalidData(format!(
                    "serving grant {} points at missing ingress {}",
                    entry.grant_id, entry.ingress_id
                ))
            })?;
        let pinned_peer_cert_sha256 = match self_signed_pins.get(&ingress.node) {
            Some(value) => value.clone(),
            None => {
                let value = self_signed_certificate_pin(pool, &ingress.node).await?;
                self_signed_pins.insert(ingress.node.clone(), value.clone());
                value
            }
        };
        let chain = app
            .chains
            .iter()
            .find(|chain| chain.id == ingress.chain)
            .ok_or_else(|| {
                StoreError::InvalidData(format!(
                    "serving ingress {} points at missing chain {}",
                    ingress.id, ingress.chain
                ))
            })?;
        let family = match entry.family {
            Some(IpFamily::V4) => "ipv4",
            Some(IpFamily::V6) => "ipv6",
            None => "unknown",
        };
        let (protocol, security) = match &entry.security {
            UserSecurityPlan::VlessEncryption {
                public_key,
                options,
                ..
            } => (
                "vless-encryption",
                E2eProbeSecurity::VlessEncryption {
                    encryption: options.encryption(public_key),
                },
            ),
            UserSecurityPlan::Reality(value) => (
                "vless",
                E2eProbeSecurity::Reality(E2eProbeReality {
                    public_key: value.public_key.clone(),
                    short_id: value.short_id.clone(),
                    server_name: value.server_name.clone(),
                    fingerprint: value.fingerprint.clone(),
                    flow: value.flow.clone(),
                }),
            ),
            UserSecurityPlan::Tls(value) => (
                "vless",
                E2eProbeSecurity::Tls(E2eProbeTls {
                    server_name: value.server_name.clone(),
                    pinned_peer_cert_sha256: pinned_peer_cert_sha256.clone(),
                    flow: value.flow.clone(),
                }),
            ),
            UserSecurityPlan::AnyTls(value) => (
                "anytls",
                E2eProbeSecurity::AnyTls {
                    settings: E2eProbeAnyTls {
                        server_name: value.server_name.clone(),
                        pinned_peer_cert_sha256: pinned_peer_cert_sha256.clone(),
                        idle_session_check_interval_secs: value
                            .settings
                            .idle_session_check_interval_secs,
                        idle_session_timeout_secs: value.settings.idle_session_timeout_secs,
                        min_idle_session: value.settings.min_idle_session,
                    },
                    reality: value.reality.as_ref().map(|reality| E2eProbeReality {
                        public_key: reality.public_key.clone(),
                        short_id: reality.short_id.clone(),
                        server_name: reality.server_name.clone(),
                        fingerprint: reality.fingerprint.clone(),
                        flow: None,
                    }),
                },
            ),
            UserSecurityPlan::Hysteria2(value) => (
                "hysteria2",
                E2eProbeSecurity::Hysteria2(E2eProbeHysteria2 {
                    server_name: value.server_name.clone(),
                    pinned_peer_cert_sha256: pinned_peer_cert_sha256.clone(),
                    congestion: value.settings.congestion.as_str().to_owned(),
                    up: value.settings.bandwidth.up.clone(),
                    down: value.settings.bandwidth.down.clone(),
                    bbr_profile: (value.settings.bbr_profile != HysteriaBbrProfile::default())
                        .then(|| value.settings.bbr_profile.as_str().to_owned()),
                    init_stream_receive_window: value.settings.quic.init_stream_receive_window,
                    max_stream_receive_window: value.settings.quic.max_stream_receive_window,
                    init_connection_receive_window: value
                        .settings
                        .quic
                        .init_connection_receive_window,
                    max_connection_receive_window: value
                        .settings
                        .quic
                        .max_connection_receive_window,
                    max_idle_timeout_secs: value.settings.quic.max_idle_timeout_secs,
                    keep_alive_period_secs: value.settings.quic.keep_alive_period_secs,
                    disable_path_mtu_discovery: value.settings.quic.disable_path_mtu_discovery,
                    salamander_password: value.settings.obfs.as_ref().map(|obfs| match obfs {
                        HysteriaObfs::Salamander { password } => password.clone(),
                    }),
                }),
            ),
            // MTProxy tunnels Telegram's own DC transport rather than an arbitrary probe URL.
            // A TCP connect would only prove that a socket is open, not that the per-user secret
            // and MTProto handshake work, so keep it out of the end-to-end proxy probe set.
            UserSecurityPlan::MtProto { .. } => continue,
        };
        let item_id = format!("{}:{family}:{protocol}", entry.grant_id);
        items.push(UserGrantProbeTarget {
            id: item_id,
            name: entry.name,
            app_id: app.id.clone(),
            app_name: if app.label.is_empty() {
                app.id.clone()
            } else {
                app.label.clone()
            },
            chain_id: chain.id.clone(),
            ingress_id: ingress.id.clone(),
            family,
            protocol,
            target: E2eProbeTarget {
                app_id: Some(app.id.clone()),
                chain_id: chain.id.clone(),
                chain_name: chain.name.clone(),
                ingress_id: ingress.id.clone(),
                dial_host: entry.server,
                port: entry.port,
                uuid: entry.uuid,
                security,
                xhttp: entry.xhttp.as_ref().map(|xhttp| E2eProbeXhttp {
                    path: xhttp.path.clone(),
                    host: xhttp.host.clone(),
                    xmux: xhttp.xmux.as_ref().map(|xmux| E2eProbeXhttpXmux {
                        max_concurrency: xmux.max_concurrency,
                        max_connections: xmux.max_connections,
                        h_max_request_times: E2eProbeXhttpRange {
                            from: xmux.h_max_request_times.from,
                            to: xmux.h_max_request_times.to,
                        },
                        h_max_reusable_secs: E2eProbeXhttpRange {
                            from: xmux.h_max_reusable_secs.from,
                            to: xmux.h_max_reusable_secs.to,
                        },
                        h_keep_alive_period_secs: xmux.h_keep_alive_period_secs,
                    }),
                    x_padding_bytes: xhttp.tuning.as_ref().and_then(|tuning| {
                        tuning
                            .x_padding_bytes
                            .as_ref()
                            .map(|range| E2eProbeXhttpRange {
                                from: range.from,
                                to: range.to,
                            })
                    }),
                    mode: xhttp.mode.as_str().map(str::to_owned),
                }),
                // An operator-triggered authorization probe answers whether the advertised
                // credential and transport can complete a request. NAT, dynamic addresses and
                // external tunnels must not turn that successful authentication into a failure.
                expected_exit_ips: Vec::new(),
            },
        });
    }

    Ok(UserGrantProbePlan {
        serving_generation: serving.generation(),
        serving_revision: serving.snapshot.revision,
        endpoint_url: serving.snapshot.settings.probe.endpoint_url.clone(),
        timeout_secs: u64::from(serving.snapshot.settings.probe.timeout_secs),
        items,
    })
}

/// Freeze a real, subscriber-visible probe plan for one member × target Front cell.
///
/// The request supplies only resource ids. Addresses, credentials and transport settings are
/// selected from the immutable Serving projection and never accepted from the browser. Loading
/// the user plan and Front ownership separately is safe only when both reads name the same
/// Serving generation; a concurrent activation otherwise returns a conflict instead of mixing
/// credentials from one generation with membership from another.
#[allow(clippy::too_many_arguments)]
pub async fn front_combination_probe_plan(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    app_id: &str,
    front_id: &str,
    member_id: &str,
    target_id: &str,
) -> Result<FrontCombinationProbePlan> {
    let app_id = required_text(app_id, "app_id")?;
    let front_id = required_text(front_id, "front_id")?;
    let member_id = required_text(member_id, "member_id")?;
    let target_id = required_text(target_id, "target_id")?;
    if member_id == target_id {
        return Err(StoreError::InvalidData(
            "前置成员和目标入口不能是同一个入口".to_owned(),
        ));
    }

    let user_plan = user_grant_probe_plan(pool, actor, tenant_id, user_id).await?;
    let serving = crate::serving::load_subscription_serving_projection(pool).await?;
    serving.ensure_available()?;
    if serving.generation() != user_plan.serving_generation {
        return Err(StoreError::Conflict(
            "Serving 在生成组合拨测计划时发生变化，请重试".to_owned(),
        ));
    }
    let app = serving
        .snapshot
        .apps
        .iter()
        .find(|app| app.id == app_id)
        .ok_or_else(|| StoreError::NotFound(format!("app {app_id}")))?;
    let front = app
        .fronts
        .iter()
        .find(|front| front.id == front_id)
        .ok_or_else(|| StoreError::NotFound(format!("front {front_id}")))?;
    actor.require_tenant_access(&front.tenant, "front combination probe")?;
    let is_internal_member = front.via.iter().any(|id| id == &member_id);
    let is_external_member = front.external_via.iter().any(|id| id == &member_id);
    if is_internal_member && is_external_member {
        return Err(StoreError::InvalidData(format!(
            "前置组 {front_id} 的内部入口与外部隧道使用了相同 id {member_id}"
        )));
    }
    if !is_internal_member && !is_external_member {
        return Err(StoreError::Conflict(format!(
            "资源 {member_id} 已不是前置组 {front_id} 的成员，请刷新页面"
        )));
    }
    let target_ingress = app
        .ingresses
        .iter()
        .find(|ingress| ingress.id == target_id)
        .ok_or_else(|| StoreError::NotFound(format!("ingress {target_id}")))?;
    if target_ingress.front.as_deref() != Some(front_id.as_str()) {
        return Err(StoreError::Conflict(format!(
            "入口 {target_id} 已不是前置组 {front_id} 的目标，请刷新页面"
        )));
    }

    let targets = user_plan
        .items
        .iter()
        .filter(|item| item.app_id == app_id && item.ingress_id == target_id)
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Err(StoreError::InvalidData(format!(
            "所选用户当前没有目标入口 {target_id} 的生效授权，无法复现其订阅路径"
        )));
    }

    const MAX_VARIANTS: usize = 64;
    let app_name = if app.label.is_empty() {
        app.id.clone()
    } else {
        app.label.clone()
    };
    let mut items = Vec::new();
    if is_internal_member {
        let members = user_plan
            .items
            .iter()
            .filter(|item| item.app_id == app_id && item.ingress_id == member_id)
            .collect::<Vec<_>>();
        if members.is_empty() {
            return Err(StoreError::InvalidData(format!(
                "所选用户当前没有成员入口 {member_id} 的生效授权，无法复现其订阅路径"
            )));
        }
        let variant_count = members.len().saturating_mul(targets.len());
        if variant_count > MAX_VARIANTS {
            return Err(StoreError::InvalidData(format!(
                "组合拨测会产生 {variant_count} 个协议变体，超过上限 {MAX_VARIANTS}"
            )));
        }
        items.reserve(variant_count);
        for member in members {
            for target in &targets {
                items.push(FrontCombinationProbeTarget {
                    id: format!("{}=>{}", member.id, target.id),
                    name: format!("{} → {}", member.name, target.name),
                    app_id: app.id.clone(),
                    app_name: app_name.clone(),
                    front_id: front.id.clone(),
                    front_name: front.name.clone(),
                    member_id: member.ingress_id.clone(),
                    member_name: member.name.clone(),
                    member_family: member.family,
                    member_protocol: member.protocol,
                    target_id: target.ingress_id.clone(),
                    target_name: target.name.clone(),
                    target_family: target.family,
                    target_protocol: target.protocol,
                    member: FrontCombinationProbeMember::Internal(member.target.clone()),
                    target: target.target.clone(),
                });
            }
        }
    } else {
        let member = serving
            .snapshot
            .external_outbounds
            .iter()
            .find(|outbound| outbound.id == member_id)
            .ok_or_else(|| {
                StoreError::Conflict(format!(
                    "外部隧道 {member_id} 尚未进入当前 Serving，请刷新页面"
                ))
            })?;
        if targets.len() > MAX_VARIANTS {
            return Err(StoreError::InvalidData(format!(
                "组合拨测会产生 {} 个协议变体，超过上限 {MAX_VARIANTS}",
                targets.len()
            )));
        }
        let member_name = if member.name.is_empty() {
            member.id.clone()
        } else {
            member.name.clone()
        };
        let member_protocol = external_protocol_name(&member.protocol);
        items.reserve(targets.len());
        for target in &targets {
            items.push(FrontCombinationProbeTarget {
                id: format!("external:{}=>{}", member.id, target.id),
                name: format!("{} → {}", member_name, target.name),
                app_id: app.id.clone(),
                app_name: app_name.clone(),
                front_id: front.id.clone(),
                front_name: front.name.clone(),
                member_id: member.id.clone(),
                member_name: member_name.clone(),
                member_family: "tunnel",
                member_protocol,
                target_id: target.ingress_id.clone(),
                target_name: target.name.clone(),
                target_family: target.family,
                target_protocol: target.protocol,
                member: FrontCombinationProbeMember::External(member.clone()),
                target: target.target.clone(),
            });
        }
    }
    Ok(FrontCombinationProbePlan {
        serving_generation: user_plan.serving_generation,
        serving_revision: user_plan.serving_revision,
        client_snapshot_id: serving.client_snapshot_id(),
        endpoint_url: user_plan.endpoint_url,
        timeout_secs: user_plan.timeout_secs,
        items,
    })
}

fn external_protocol_name(protocol: &ExternalOutboundProtocol) -> &'static str {
    match protocol {
        ExternalOutboundProtocol::Anytls { .. } => "anytls",
        ExternalOutboundProtocol::Vless { .. } => "vless",
        ExternalOutboundProtocol::Shadowsocks2022 { .. } => "shadowsocks",
        ExternalOutboundProtocol::Socks5 { .. } => "socks5",
        ExternalOutboundProtocol::HttpConnect { .. } => "http-connect",
        ExternalOutboundProtocol::Wireguard { .. } => "wireguard",
        ExternalOutboundProtocol::Warp { .. } => "warp",
        ExternalOutboundProtocol::Vpngate { .. } => "vpngate",
    }
}

async fn self_signed_certificate_pin(pool: &PgPool, node_id: &str) -> Result<Option<String>> {
    let Some(profile) = crate::cert::serving_certificate_profile_for_node(pool, node_id).await?
    else {
        return Ok(None);
    };
    if !profile.requires_pinning {
        return Ok(None);
    }
    if profile.trusted_peer_sha256.is_empty() {
        Err(StoreError::InvalidData(format!(
            "self-signed certificate trust set {} on node {node_id} has no peer fingerprint",
            profile.name
        )))
    } else {
        Ok(Some(profile.trusted_peer_sha256.join(",")))
    }
}

pub async fn user_grant_probe_generation_matches(pool: &PgPool, expected: u64) -> Result<bool> {
    let serving = crate::serving::load_subscription_serving_projection(pool).await?;
    serving.ensure_available()?;
    Ok(serving.generation() == expected)
}
