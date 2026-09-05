//! Frozen work list for an operator-triggered user authorization probe.
//!
//! The browser never supplies an address, port, protocol parameter or credential. Every field is
//! projected from the same fully converged Serving snapshot used by subscriptions; otherwise this
//! endpoint would be both an SSRF primitive and a test of a path subscribers were never given.

use brocade_core::{
    compile::compile,
    model::{HysteriaBbrProfile, HysteriaObfs, IpFamily},
    physical::user::UserSecurityPlan,
};
use brocade_deployment::protocol::{
    E2eProbeAnyTls, E2eProbeHysteria2, E2eProbeReality, E2eProbeTarget, E2eProbeTls, E2eProbeXhttp,
    E2eProbeXhttpRange, E2eProbeXhttpXmux,
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

    let output = compile(&serving.snapshot);
    let user = output.project_user(&tenant_id, &user_id)?;
    if user.entries.is_empty() {
        return Err(StoreError::NotFound(format!(
            "user {tenant_id}/{user_id} has no effective serving grants"
        )));
    }

    let mut items = Vec::with_capacity(user.entries.len());
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
        let expected_exit_ips = output
            .project_probe(&ingress.node)?
            .targets
            .into_iter()
            .find(|target| target.ingress_id == ingress.id)
            .map(|target| target.expected_exit_ips)
            .unwrap_or_default();

        let family = match entry.family {
            Some(IpFamily::V4) => "ipv4",
            Some(IpFamily::V6) => "ipv6",
            None => "unknown",
        };
        let (protocol, reality, tls, hysteria2) = match &entry.security {
            UserSecurityPlan::Reality(value) => (
                "vless",
                E2eProbeReality {
                    public_key: value.public_key.clone(),
                    short_id: value.short_id.clone(),
                    server_name: value.server_name.clone(),
                    fingerprint: value.fingerprint.clone(),
                    flow: value.flow.clone(),
                },
                None,
                None,
            ),
            UserSecurityPlan::Tls(value) => (
                "vless",
                empty_reality(),
                Some(E2eProbeTls {
                    server_name: value.server_name.clone(),
                    flow: value.flow.clone(),
                }),
                None,
            ),
            UserSecurityPlan::AnyTls(value) => (
                "anytls",
                value
                    .reality
                    .as_ref()
                    .map_or_else(empty_reality, |reality| E2eProbeReality {
                        public_key: reality.public_key.clone(),
                        short_id: reality.short_id.clone(),
                        server_name: reality.server_name.clone(),
                        fingerprint: reality.fingerprint.clone(),
                        flow: None,
                    }),
                None,
                None,
            ),
            UserSecurityPlan::Hysteria2(value) => (
                "hysteria2",
                empty_reality(),
                None,
                Some(E2eProbeHysteria2 {
                    server_name: value.server_name.clone(),
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
                reality,
                hysteria2,
                anytls: match &entry.security {
                    UserSecurityPlan::AnyTls(value) => Some(E2eProbeAnyTls {
                        server_name: value.server_name.clone(),
                        idle_session_check_interval_secs: value
                            .settings
                            .idle_session_check_interval_secs,
                        idle_session_timeout_secs: value.settings.idle_session_timeout_secs,
                        min_idle_session: value.settings.min_idle_session,
                    }),
                    _ => None,
                },
                tls,
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
                expected_exit_ips,
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

pub async fn user_grant_probe_generation_matches(pool: &PgPool, expected: u64) -> Result<bool> {
    let serving = crate::serving::load_subscription_serving_projection(pool).await?;
    serving.ensure_available()?;
    Ok(serving.generation() == expected)
}

fn empty_reality() -> E2eProbeReality {
    E2eProbeReality {
        public_key: String::new(),
        short_id: String::new(),
        server_name: String::new(),
        fingerprint: String::new(),
        flow: None,
    }
}
