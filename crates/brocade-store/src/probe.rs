//! Link probing: the agent reports path MTU, and the control plane records it and derives a
//! suggestion.
//!
//! The control plane does not measure it itself — it has no path to the underlay, and the line
//! between two machines is visible only to its two ends.

use std::collections::{BTreeMap, BTreeSet};

use brocade_core::{
    model::{ModelSnapshot, WgTransport},
    physical::probe::{ProbePlan, ProbeSecurity},
};
use brocade_deployment::protocol::{
    E2eExitVerdict, E2eProbe, E2eProbeAnyTls, E2eProbeHysteria2, E2eProbeReality, E2eProbeRequest,
    E2eProbeResult, E2eProbeStatus, E2eProbeTarget, E2eProbeTargetList, E2eProbeTls, E2eProbeXhttp,
    E2eProbeXhttpRange, E2eProbeXhttpXmux, LinkHealthRequest, LinkHealthResult, LinkProbeRequest,
    LinkProbeResult, LinkProbeStatus, ProbeTarget, ProbeTargetList, ProbeTransport,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{admin::tenant_filter, AdminContext, Result, StoreError};

const MAX_CLOCK_SKEW_SECS: i64 = 600;
const MAX_ITEMS_PER_REPORT: usize = 512;

async fn validate_observed_at(
    tx: &mut Transaction<'_, Postgres>,
    field: &str,
    observed_at: i64,
) -> Result<()> {
    let server_now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(&mut **tx)
        .await?;
    let skew = observed_at.saturating_sub(server_now).abs();
    if skew > MAX_CLOCK_SKEW_SECS {
        return Err(StoreError::InvalidData(format!(
            "{field} clock skew {skew}s exceeds {MAX_CLOCK_SKEW_SECS}s; check the node's clock"
        )));
    }
    Ok(())
}

// How these views are scoped: `None` from `tenant_filter` means unscoped (a system-admin, or a
// global operator with no tenant_scope); `Some((scope, pattern))` means this branch only.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkMtuView {
    /// Each pair's raw probe results. Path MTU really is a property of the path, so the
    /// measurements are stored per pair.
    pub links: Vec<LinkMtuItem>,
    /// Each machine's current and suggested values. The setting is node-level (one wg0, one
    /// MTU), which does not conflict with measuring per pair: a machine's suggestion is the
    /// smallest across all its paths.
    pub nodes: Vec<NodeMtuItem>,
    /// The default for machines with no `Node.mtu` (`settings.overlay.mtu`).
    pub default_mtu: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeMtuItem {
    pub node_id: String,
    /// The effective value: its own where set, otherwise the global default.
    pub current_mtu: u16,
    /// Whether the effective value is one this machine set itself.
    pub overridden: bool,
    pub suggested_mtu: Option<u16>,
    /// The peer that pushed the suggestion down to this number. Knowing which path is narrow is
    /// what makes fixing it possible at all.
    pub tightest_peer: Option<String>,
    /// How many of this machine's paths yielded no result (unreachable, or ICMP blocked). The
    /// suggestion rests only on those that answered, and this number is the warning that it may
    /// be too large.
    pub inconclusive: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkMtuItem {
    pub node_id: String,
    pub peer_node_id: String,
    pub endpoint_host: String,
    pub status: String,
    pub path_mtu: Option<u16>,
    pub suggested_wg_mtu: Option<u16>,
    pub probed_at: String,
}

/// Which endpoints this machine should probe.
///
/// Computed from the current model: every other live backbone member, taking their
/// `public_ipv4` and wg transport. Peers behind NAT do not enter the list — they cannot be
/// probed from this side, and that link is probed by them (`ProbeTarget`).
pub async fn probe_targets(pool: &PgPool, node_id: &str) -> Result<ProbeTargetList> {
    let snapshot = crate::materialize::load_current_immutable_snapshot(pool).await?;
    // Links exist only between backbone members, and a decommissioned one is not on the
    // network. A machine does not probe itself.
    let mut targets = snapshot
        .nodes
        .iter()
        .filter(|node| node.overlay && !node.retired && node.id != node_id)
        .filter(|node| !node.public_ipv4_nat)
        .filter_map(|node| {
            Some(ProbeTarget {
                peer_node_id: node.id.clone(),
                host: node.public_ipv4.clone()?,
                transport: match node.wireguard.transport {
                    WgTransport::Udp => ProbeTransport::Udp,
                    WgTransport::FakeTcp { .. } => ProbeTransport::FakeTcp,
                },
            })
        })
        .collect::<Vec<_>>();
    targets.sort_by(|a, b| a.peer_node_id.cmp(&b.peer_node_id));

    Ok(ProbeTargetList { targets })
}

pub async fn record_link_probe(
    pool: &PgPool,
    node_id: &str,
    request: LinkProbeRequest,
) -> Result<LinkProbeResult> {
    if request.probed_at_unix_secs <= 0 {
        return Err(StoreError::InvalidData(
            "link probe timestamp must be positive unix seconds".to_owned(),
        ));
    }
    if request.links.len() > MAX_ITEMS_PER_REPORT {
        return Err(StoreError::InvalidData(format!(
            "link probe report contains more than {MAX_ITEMS_PER_REPORT} links"
        )));
    }

    let mut tx = pool.begin().await?;
    validate_observed_at(&mut tx, "link probe timestamp", request.probed_at_unix_secs).await?;
    let peer_ids = request
        .links
        .iter()
        .map(|link| link.peer_node_id.clone())
        .collect::<Vec<_>>();
    let known_peers = sqlx::query_scalar::<_, String>("SELECT id FROM nodes WHERE id = ANY($1)")
        .bind(&peer_ids)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect::<BTreeSet<_>>();

    let mut accepted = 0_u64;
    let mut unknown = 0_u64;
    for link in &request.links {
        if link.peer_node_id == node_id {
            return Err(StoreError::InvalidData(
                "link probe cannot target the reporting node itself".to_owned(),
            ));
        }
        // A peer absent from the model is dropped: a freshly decommissioned machine lingers in
        // other people's wireguard.conf for a while, which is not an error but simply a row with
        // no owner. The foreign key would error outright, hence filtering against the batched
        // lookup first.
        if !known_peers.contains(&link.peer_node_id) {
            unknown += 1;
            continue;
        }

        let (path_mtu, suggested) = match link.status {
            LinkProbeStatus::Ok => match (link.path_mtu, link.suggested_wg_mtu) {
                (Some(path), Some(suggested)) => {
                    (Some(i32::from(path)), Some(i32::from(suggested)))
                }
                _ => {
                    return Err(StoreError::InvalidData(
                        "link probe with status ok must carry both mtu values".to_owned(),
                    ))
                }
            },
            // The two undeterminable outcomes carry no number.
            _ => (None, None),
        };

        sqlx::query(
            "INSERT INTO link_probes
                 (node_id, peer_node_id, endpoint_host, status, path_mtu, suggested_wg_mtu, probed_at)
             VALUES ($1, $2, $3, $4, $5, $6, to_timestamp($7))
             ON CONFLICT (node_id, peer_node_id) DO UPDATE SET
                 endpoint_host = EXCLUDED.endpoint_host,
                 status = EXCLUDED.status,
                 path_mtu = EXCLUDED.path_mtu,
                 suggested_wg_mtu = EXCLUDED.suggested_wg_mtu,
                 probed_at = EXCLUDED.probed_at,
                 updated_at = now()
             WHERE link_probes.probed_at <= EXCLUDED.probed_at",
        )
        .bind(node_id)
        .bind(&link.peer_node_id)
        .bind(&link.endpoint_host)
        .bind(status_text(link.status))
        .bind(path_mtu)
        .bind(suggested)
        .bind(request.probed_at_unix_secs as f64)
        .execute(&mut *tx)
        .await?;
        accepted += 1;
    }

    tx.commit().await?;

    Ok(LinkProbeResult {
        node_id: node_id.to_owned(),
        accepted_links: accepted,
        unknown_peers: unknown,
    })
}

pub async fn link_mtu_view(pool: &PgPool, actor: &AdminContext) -> Result<LinkMtuView> {
    let filter = tenant_filter(actor);
    let (scope, pattern) = match &filter {
        Some((scope, pattern)) => (Some(scope.as_str()), Some(pattern.as_str())),
        None => (None, None),
    };

    // A link is kept where at least one end is in view, not where both are.
    // A path is shared by its two ends and the suggestion is the narrowest across all of mine —
    // filtering out rows whose peer is out of view raises the remaining minimum, and too large
    // is the fatal side (large packets silently dropped). Better to let the peer's node_id show:
    // it has to appear in the "narrowest path" column anyway, and one needs to know which path
    // pushed the number down.
    let rows = sqlx::query(
        "SELECT node_id, peer_node_id, endpoint_host, status, path_mtu, suggested_wg_mtu,
                probed_at::text AS probed_at
         FROM link_probes
         WHERE $1::text IS NULL
            OR node_id IN (SELECT id FROM nodes
                           WHERE tenant_id = $1 OR tenant_id LIKE $2 ESCAPE '\\')
            OR peer_node_id IN (SELECT id FROM nodes
                                WHERE tenant_id = $1 OR tenant_id LIKE $2 ESCAPE '\\')
         ORDER BY node_id, peer_node_id",
    )
    .bind(scope)
    .bind(pattern)
    .fetch_all(pool)
    .await?;

    let mut links = Vec::with_capacity(rows.len());
    for row in &rows {
        links.push(LinkMtuItem {
            node_id: row.try_get("node_id")?,
            peer_node_id: row.try_get("peer_node_id")?,
            endpoint_host: row.try_get("endpoint_host")?,
            status: row.try_get("status")?,
            path_mtu: row
                .try_get::<Option<i32>, _>("path_mtu")?
                .map(|value| u16::try_from(value).unwrap_or(u16::MAX)),
            suggested_wg_mtu: row
                .try_get::<Option<i32>, _>("suggested_wg_mtu")?
                .map(|value| u16::try_from(value).unwrap_or(u16::MAX)),
            probed_at: row.try_get("probed_at")?,
        });
    }

    let default_mtu = sqlx::query("SELECT overlay_mtu FROM control_state WHERE id = TRUE")
        .fetch_one(pool)
        .await?
        .try_get::<i32, _>("overlay_mtu")?;
    let default_mtu = u16::try_from(default_mtu).unwrap_or(1420);

    // The node table is scoped strictly: this one answers whose MTU should change, the place to
    // change it is the node page, and the node page lists only machines in view anyway.
    let node_rows = sqlx::query(
        "SELECT id, mtu FROM nodes
         WHERE retired_at IS NULL
           AND ($1::text IS NULL OR tenant_id = $1 OR tenant_id LIKE $2 ESCAPE '\\')
         ORDER BY id",
    )
    .bind(scope)
    .bind(pattern)
    .fetch_all(pool)
    .await?;
    let mut nodes = Vec::with_capacity(node_rows.len());
    for row in &node_rows {
        let node_id: String = row.try_get("id")?;
        let own = row
            .try_get::<Option<i32>, _>("mtu")?
            .map(|value| u16::try_from(value).unwrap_or(default_mtu));

        // Both sides count. A path is shared by its two ends and only the side with an Endpoint
        // written can probe it — looking at `node_id = X` alone leaves the machine behind NAT
        // without a suggestion forever, and it is precisely the one most likely to need its MTU
        // adjusted.
        let mine = links
            .iter()
            .filter(|link| link.node_id == node_id || link.peer_node_id == node_id);
        let mut suggested: Option<(u16, String)> = None;
        let mut inconclusive = 0_u64;
        for link in mine {
            let peer = if link.node_id == node_id {
                &link.peer_node_id
            } else {
                &link.node_id
            };
            match (link.status.as_str(), link.suggested_wg_mtu) {
                ("ok", Some(value)) => {
                    if suggested.as_ref().is_none_or(|(best, _)| value < *best) {
                        suggested = Some((value, peer.clone()));
                    }
                }
                ("ok", None) => {}
                _ => inconclusive += 1,
            }
        }

        nodes.push(NodeMtuItem {
            node_id,
            current_mtu: own.unwrap_or(default_mtu),
            overridden: own.is_some(),
            suggested_mtu: suggested.as_ref().map(|(value, _)| *value),
            tightest_peer: suggested.map(|(_, peer)| peer),
            inconclusive,
        });
    }

    Ok(LinkMtuView {
        links,
        nodes,
        default_mtu,
    })
}

fn status_text(status: LinkProbeStatus) -> &'static str {
    match status {
        LinkProbeStatus::Ok => "ok",
        LinkProbeStatus::Unreachable => "unreachable",
        LinkProbeStatus::Blocked => "blocked",
        LinkProbeStatus::Unsupported => "unsupported",
    }
}

/// Whether one relay hop works right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkHealthItem {
    pub node_id: String,
    pub chain_id: String,
    pub peer_node_id: String,
    pub alive: bool,
    pub downlink_bytes: u64,
    pub window_secs: u64,
    pub checked_at: String,
}

pub async fn record_link_health(
    pool: &PgPool,
    node_id: &str,
    request: LinkHealthRequest,
) -> Result<LinkHealthResult> {
    if request.checked_at_unix_secs <= 0 {
        return Err(StoreError::InvalidData(
            "link health timestamp must be positive unix seconds".to_owned(),
        ));
    }
    if request.hops.len() > MAX_ITEMS_PER_REPORT {
        return Err(StoreError::InvalidData(format!(
            "link health report contains more than {MAX_ITEMS_PER_REPORT} hops"
        )));
    }

    let mut tx = pool.begin().await?;
    validate_observed_at(
        &mut tx,
        "link health timestamp",
        request.checked_at_unix_secs,
    )
    .await?;
    let peer_ids = request
        .hops
        .iter()
        .map(|hop| hop.peer_node_id.clone())
        .collect::<Vec<_>>();
    let known_peers = sqlx::query_scalar::<_, String>("SELECT id FROM nodes WHERE id = ANY($1)")
        .bind(&peer_ids)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let chain_ids = request
        .hops
        .iter()
        .map(|hop| {
            hop.chain_id
                .rsplit('/')
                .next()
                .unwrap_or(&hop.chain_id)
                .to_owned()
        })
        .collect::<Vec<_>>();
    let mut known_chains = BTreeSet::new();
    for row in sqlx::query("SELECT app_id, id FROM chains WHERE id = ANY($1)")
        .bind(&chain_ids)
        .fetch_all(&mut *tx)
        .await?
    {
        known_chains.insert((
            row.try_get::<String, _>("app_id")?,
            row.try_get::<String, _>("id")?,
        ));
    }

    let mut accepted = 0_u64;
    let mut unknown = 0_u64;
    for hop in &request.hops {
        if hop.peer_node_id == node_id {
            return Err(StoreError::InvalidData(
                "link health cannot target the reporting node itself".to_owned(),
            ));
        }
        // A peer absent from the model is dropped: right after a chain is deleted or a machine
        // decommissioned, the agent's artifacts lag for a while. Not an error, simply a row with
        // no owner.
        if !known_peers.contains(&hop.peer_node_id) {
            unknown += 1;
            continue;
        }

        // Chain IDs are model identities, not free-form telemetry dimensions. After the
        // friendly-ID rollout there is deliberately no alias fallback: a stale tag is unknown and
        // must not recreate the retired namespace in an operational table.
        let known_chain = match hop.chain_id.split_once('/') {
            Some((app_id, chain_id)) => {
                known_chains.contains(&(app_id.to_owned(), chain_id.to_owned()))
            }
            None => known_chains
                .iter()
                .any(|(_, chain_id)| chain_id == &hop.chain_id),
        };
        if !known_chain {
            unknown += 1;
            continue;
        }

        sqlx::query(
            "INSERT INTO link_health
                 (node_id, chain_id, peer_node_id, alive, downlink_bytes, window_secs, checked_at)
             VALUES ($1, $2, $3, $4, $5, $6, to_timestamp($7))
             ON CONFLICT (node_id, chain_id, peer_node_id) DO UPDATE SET
                 alive = EXCLUDED.alive,
                 downlink_bytes = EXCLUDED.downlink_bytes,
                 window_secs = EXCLUDED.window_secs,
                 checked_at = EXCLUDED.checked_at,
                 updated_at = now()
             WHERE link_health.checked_at <= EXCLUDED.checked_at",
        )
        .bind(node_id)
        .bind(&hop.chain_id)
        .bind(&hop.peer_node_id)
        .bind(hop.alive)
        .bind(i64::try_from(hop.downlink_bytes).unwrap_or(i64::MAX))
        .bind(i64::try_from(request.window_secs).unwrap_or(i64::MAX))
        .bind(request.checked_at_unix_secs as f64)
        .execute(&mut *tx)
        .await?;
        accepted += 1;
    }

    tx.commit().await?;

    Ok(LinkHealthResult {
        node_id: node_id.to_owned(),
        accepted_hops: accepted,
        unknown_hops: unknown,
    })
}

pub async fn link_health_view(pool: &PgPool, actor: &AdminContext) -> Result<Vec<LinkHealthItem>> {
    let filter = tenant_filter(actor);
    let (scope, pattern) = match &filter {
        Some((scope, pattern)) => (Some(scope.as_str()), Some(pattern.as_str())),
        None => (None, None),
    };

    // This family records whether my counters toward the next hop are still growing, which
    // belongs to the node_id machine, so scoping on that one side suffices — unlike MTU, whose
    // numbers are decided by both ends together.
    let rows = sqlx::query(
        "SELECT node_id, chain_id, peer_node_id, alive, downlink_bytes, window_secs,
                checked_at::text AS checked_at
         FROM link_health
         WHERE $1::text IS NULL
            OR node_id IN (SELECT id FROM nodes
                           WHERE tenant_id = $1 OR tenant_id LIKE $2 ESCAPE '\\')
         ORDER BY node_id, chain_id, peer_node_id",
    )
    .bind(scope)
    .bind(pattern)
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(|row| {
            Ok(LinkHealthItem {
                node_id: row.try_get("node_id")?,
                chain_id: row.try_get("chain_id")?,
                peer_node_id: row.try_get("peer_node_id")?,
                alive: row.try_get("alive")?,
                downlink_bytes: u64::try_from(row.try_get::<i64, _>("downlink_bytes")?)
                    .unwrap_or_default(),
                window_secs: u64::try_from(row.try_get::<i64, _>("window_secs")?)
                    .unwrap_or_default(),
                checked_at: row.try_get("checked_at")?,
            })
        })
        .collect()
}

// ══════════════════════════════════════════════════════════════════════
// End-to-end probing
//
// Its division of labor with the two families above: `link_probes` is the MTU of one underlay
// segment, `link_health` is whether my counters toward the next hop are still growing, and this
// is whether a user dialing in at the ingress can traverse the whole chain.
// ══════════════════════════════════════════════════════════════════════

/// The chain card compares the same six-hour window as the machine card. Retention is time based,
/// not count based: the probe interval is configurable, so "20 samples" can mean five minutes or
/// more than a day and cannot honestly label a time axis.
const SAMPLE_WINDOW_SECS: i64 = 6 * 60 * 60;

/// One chain's latest end-to-end probe result, plus the recent series of timings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeItem {
    pub app_id: String,
    pub chain_id: String,
    pub chain_name: String,
    /// Who probed. Always the chain's head.
    pub node_id: String,
    pub status: String,
    pub ttfb_ms: Option<u32>,
    pub exit_ip: Option<String>,
    pub exit_loc: Option<String>,
    pub exit_verdict: String,
    pub detail: Option<String>,
    pub probed_at: String,
    /// The last six hours in chronological order (oldest to newest). The UI places them on a real
    /// time axis rather than distributing an irregular series at equal distances.
    pub samples: Vec<E2eProbeSample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeSample {
    pub probed_at: String,
    pub status: String,
    pub ttfb_ms: Option<u32>,
}

/// Which chains this machine probes as their head.
///
/// Compiled on demand from the current model rather than looked up: the work list is a function
/// of the model, and storing a copy introduces a state where the stored and the compiled
/// disagree — whose symptom is the probe knocking with a credential xray does not know, which
/// looks exactly like a genuinely broken chain.
pub async fn e2e_probe_targets(pool: &PgPool, node_id: &str) -> Result<E2eProbeTargetList> {
    let snapshot = crate::materialize::load_current_immutable_snapshot(pool).await?;
    let probe_settings = snapshot.settings.probe.clone();
    let plan = e2e_probe_plan(&snapshot, node_id)?;

    Ok(E2eProbeTargetList {
        targets: plan
            .targets
            .into_iter()
            .map(|target| E2eProbeTarget {
                app_id: target.app_id,
                chain_id: target.chain_id,
                chain_name: target.chain_name,
                ingress_id: target.ingress_id,
                dial_host: target.dial_host,
                port: target.port,
                uuid: target.uuid,
                reality: match &target.security {
                    ProbeSecurity::Reality(reality) => E2eProbeReality {
                        public_key: reality.public_key.clone(),
                        short_id: reality.short_id.clone(),
                        server_name: reality.server_name.clone(),
                        fingerprint: reality.fingerprint.clone(),
                        flow: reality.flow.clone(),
                    },
                    ProbeSecurity::AnyTls(anytls) if anytls.reality.is_some() => {
                        let reality = anytls.reality.as_ref().unwrap();
                        E2eProbeReality {
                            public_key: reality.public_key.clone(),
                            short_id: reality.short_id.clone(),
                            server_name: reality.server_name.clone(),
                            fingerprint: reality.fingerprint.clone(),
                            flow: None,
                        }
                    }
                    // Filler beside a `tls` block that supersedes it. Empty rather than absent
                    // because the field is what an older agent parses, and one that cannot be
                    // parsed costs that agent every other probe on the machine.
                    ProbeSecurity::Tls(_)
                    | ProbeSecurity::AnyTls(_)
                    | ProbeSecurity::Hysteria2(_) => E2eProbeReality {
                        public_key: String::new(),
                        short_id: String::new(),
                        server_name: String::new(),
                        fingerprint: String::new(),
                        flow: None,
                    },
                },
                hysteria2: match &target.security {
                    ProbeSecurity::Hysteria2(hysteria) => Some(E2eProbeHysteria2 {
                        server_name: hysteria.server_name.clone(),
                        congestion: hysteria.settings.congestion.as_str().to_owned(),
                        up: hysteria.settings.bandwidth.up.clone(),
                        down: hysteria.settings.bandwidth.down.clone(),
                        bbr_profile: (hysteria.settings.bbr_profile
                            != brocade_core::model::HysteriaBbrProfile::default())
                        .then(|| hysteria.settings.bbr_profile.as_str().to_owned()),
                        init_stream_receive_window: hysteria
                            .settings
                            .quic
                            .init_stream_receive_window,
                        max_stream_receive_window: hysteria.settings.quic.max_stream_receive_window,
                        init_connection_receive_window: hysteria
                            .settings
                            .quic
                            .init_connection_receive_window,
                        max_connection_receive_window: hysteria
                            .settings
                            .quic
                            .max_connection_receive_window,
                        max_idle_timeout_secs: hysteria.settings.quic.max_idle_timeout_secs,
                        keep_alive_period_secs: hysteria.settings.quic.keep_alive_period_secs,
                        disable_path_mtu_discovery: hysteria
                            .settings
                            .quic
                            .disable_path_mtu_discovery,
                        salamander_password: hysteria.settings.obfs.as_ref().map(
                            |obfs| match obfs {
                                brocade_core::model::HysteriaObfs::Salamander { password } => {
                                    password.clone()
                                }
                            },
                        ),
                    }),
                    _ => None,
                },
                anytls: match &target.security {
                    ProbeSecurity::AnyTls(anytls) => Some(E2eProbeAnyTls {
                        server_name: anytls.server_name.clone(),
                        idle_session_check_interval_secs: anytls
                            .settings
                            .idle_session_check_interval_secs,
                        idle_session_timeout_secs: anytls.settings.idle_session_timeout_secs,
                        min_idle_session: anytls.settings.min_idle_session,
                    }),
                    _ => None,
                },
                tls: match &target.security {
                    ProbeSecurity::Reality(_)
                    | ProbeSecurity::AnyTls(_)
                    | ProbeSecurity::Hysteria2(_) => None,
                    ProbeSecurity::Tls(tls) => Some(E2eProbeTls {
                        server_name: tls.server_name.clone(),
                        flow: tls.flow.clone(),
                    }),
                },
                xhttp: target.xhttp.as_ref().map(|xhttp| E2eProbeXhttp {
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
                expected_exit_ips: target.expected_exit_ips,
            })
            .collect(),
        endpoint_url: probe_settings.endpoint_url,
        timeout_secs: u64::from(probe_settings.timeout_secs),
        interval_secs: u64::from(probe_settings.interval_secs),
    })
}

/// Keep the publish gate on the store boundary. A compile failure is not an empty work list:
/// empty means this node genuinely heads no chains, while failure means the control plane cannot
/// safely state what it should probe.
fn e2e_probe_plan(snapshot: &ModelSnapshot, node_id: &str) -> Result<ProbePlan> {
    brocade_core::compile::compile(snapshot)
        .project_probe(node_id)
        .map_err(StoreError::from)
}

pub async fn record_e2e_probe(
    pool: &PgPool,
    node_id: &str,
    request: E2eProbeRequest,
) -> Result<E2eProbeResult> {
    if request.probed_at_unix_secs <= 0 {
        return Err(StoreError::InvalidData(
            "e2e probe timestamp must be positive unix seconds".to_owned(),
        ));
    }
    if request.chains.len() > MAX_ITEMS_PER_REPORT {
        return Err(StoreError::InvalidData(format!(
            "e2e probe report contains more than {MAX_ITEMS_PER_REPORT} chains"
        )));
    }

    let mut tx = pool.begin().await?;
    validate_observed_at(&mut tx, "e2e probe timestamp", request.probed_at_unix_secs).await?;
    let chain_ids = request
        .chains
        .iter()
        .map(|chain| chain.chain_id.clone())
        .collect::<Vec<_>>();
    let mut owners = BTreeMap::new();
    for row in sqlx::query("SELECT id, app_id FROM chains WHERE id = ANY($1)")
        .bind(&chain_ids)
        .fetch_all(&mut *tx)
        .await?
    {
        owners.insert(
            row.try_get::<String, _>("id")?,
            row.try_get::<String, _>("app_id")?,
        );
    }

    let mut accepted = 0_u64;
    let mut unknown = 0_u64;
    for chain in &request.chains {
        // A chain absent from the model is dropped: right after a chain is deleted or
        // reassigned, the agent's work list lags for a while. Not an error, simply a row with no
        // owner. The foreign key would error outright, hence filtering against the batched
        // lookup first.
        let Some(app_id) = owners.get(&chain.chain_id) else {
            unknown += 1;
            continue;
        };

        let status = status_text_e2e(chain.status);
        let ttfb = ttfb_for(chain);
        let verdict = verdict_text(chain.exit_verdict, chain.status);

        sqlx::query(
            "INSERT INTO e2e_probes
                 (chain_id, app_id, node_id, status, ttfb_ms, exit_ip, exit_loc,
                  exit_verdict, detail, probed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, to_timestamp($10))
             ON CONFLICT (chain_id) DO UPDATE SET
                 app_id = EXCLUDED.app_id,
                 node_id = EXCLUDED.node_id,
                 status = EXCLUDED.status,
                 ttfb_ms = EXCLUDED.ttfb_ms,
                 exit_ip = EXCLUDED.exit_ip,
                 exit_loc = EXCLUDED.exit_loc,
                 exit_verdict = EXCLUDED.exit_verdict,
                 detail = EXCLUDED.detail,
                 probed_at = EXCLUDED.probed_at,
                 updated_at = now()
             WHERE e2e_probes.probed_at <= EXCLUDED.probed_at",
        )
        .bind(&chain.chain_id)
        .bind(app_id)
        .bind(node_id)
        .bind(status)
        .bind(ttfb)
        .bind(chain.exit_ip.as_deref())
        .bind(chain.exit_loc.as_deref())
        .bind(verdict)
        .bind(chain.detail.as_deref())
        .bind(request.probed_at_unix_secs as f64)
        .execute(&mut *tx)
        .await?;

        // The sample table takes its own row. A repeat report within the same second collides
        // on the primary key — DO NOTHING rather than overwrite: with two probes landing in one
        // second, the first to arrive is already a fact.
        sqlx::query(
            "INSERT INTO e2e_probe_samples (chain_id, probed_at, status, ttfb_ms)
             VALUES ($1, to_timestamp($2), $3, $4)
             ON CONFLICT (chain_id, probed_at) DO NOTHING",
        )
        .bind(&chain.chain_id)
        .bind(request.probed_at_unix_secs as f64)
        .bind(status)
        .bind(ttfb)
        .execute(&mut *tx)
        .await?;

        // Trim by age rather than row count. ProbeSettings::interval_secs is configurable, so a
        // count cannot describe six hours. Trimming on the write side avoids a scheduled cleanup
        // job while bounding the fastest allowed cadence to about 1,440 rows per active chain.
        sqlx::query(
            "DELETE FROM e2e_probe_samples
             WHERE chain_id = $1
               AND probed_at < CURRENT_TIMESTAMP - make_interval(secs => $2::double precision)",
        )
        .bind(&chain.chain_id)
        .bind(SAMPLE_WINDOW_SECS)
        .execute(&mut *tx)
        .await?;

        accepted += 1;
    }

    tx.commit().await?;

    Ok(E2eProbeResult {
        node_id: node_id.to_owned(),
        accepted_chains: accepted,
        unknown_chains: unknown,
    })
}

// This family is partitioned by tenant, unlike MTU and link liveness: those two are properties
// of the backbone (one global network), whereas a chain belongs to a tenant. A tenant
// administrator sees whether their own chains work and not anyone else's.
pub async fn e2e_probe_view(
    pool: &PgPool,
    actor: &crate::AdminContext,
) -> Result<Vec<E2eProbeItem>> {
    let tenant_scope = actor.tenant_scope();
    let tenant_pattern = actor.tenant_scope_like_pattern();

    let rows = sqlx::query(
        "SELECT p.chain_id, p.app_id, c.name AS chain_name, p.node_id, p.status, p.ttfb_ms,
                p.exit_ip, p.exit_loc, p.exit_verdict, p.detail, p.probed_at::text AS probed_at
         FROM e2e_probes p
         JOIN chains c ON c.id = p.chain_id
         JOIN apps a ON a.id = c.app_id
         WHERE (
                $1::text IS NULL
                OR c.tenant_id = $1
                OR c.tenant_id LIKE $2 ESCAPE '\\'
         )
         ORDER BY a.position, a.id, c.position, c.id",
    )
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(pool)
    .await?;

    // Fetch the samples once and group them by chain rather than querying per chain in a loop:
    // with many chains that is N+1.
    let sample_rows = sqlx::query(
        "SELECT s.chain_id, s.probed_at::text AS probed_at, s.status, s.ttfb_ms
         FROM e2e_probe_samples s
         JOIN chains c ON c.id = s.chain_id
         WHERE (
                $1::text IS NULL
                OR c.tenant_id = $1
                OR c.tenant_id LIKE $2 ESCAPE '\\'
             )
           AND s.probed_at >= CURRENT_TIMESTAMP - make_interval(secs => $3::double precision)
         ORDER BY s.chain_id, s.probed_at",
    )
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .bind(SAMPLE_WINDOW_SECS)
    .fetch_all(pool)
    .await?;
    let mut samples: std::collections::BTreeMap<String, Vec<E2eProbeSample>> = Default::default();
    for row in &sample_rows {
        samples
            .entry(row.try_get("chain_id")?)
            .or_default()
            .push(E2eProbeSample {
                probed_at: row.try_get("probed_at")?,
                status: row.try_get("status")?,
                ttfb_ms: optional_u32(row.try_get("ttfb_ms")?),
            });
    }

    rows.iter()
        .map(|row| {
            let chain_id: String = row.try_get("chain_id")?;
            Ok(E2eProbeItem {
                app_id: row.try_get("app_id")?,
                chain_name: row.try_get("chain_name")?,
                node_id: row.try_get("node_id")?,
                status: row.try_get("status")?,
                ttfb_ms: optional_u32(row.try_get("ttfb_ms")?),
                exit_ip: row.try_get("exit_ip")?,
                exit_loc: row.try_get("exit_loc")?,
                exit_verdict: row.try_get("exit_verdict")?,
                detail: row.try_get("detail")?,
                probed_at: row.try_get("probed_at")?,
                samples: samples.remove(&chain_id).unwrap_or_default(),
                chain_id,
            })
        })
        .collect()
}

fn optional_u32(value: Option<i32>) -> Option<u32> {
    value.and_then(|value| u32::try_from(value).ok())
}

/// A failed attempt's timing is the timeout value and says nothing about the chain's speed —
/// discarded before it lands, so that some consumer forgetting to filter does not draw 10000ms
/// into a trend line. A CHECK in the database watches this.
fn ttfb_for(chain: &E2eProbe) -> Option<i32> {
    match chain.status {
        E2eProbeStatus::Ok => chain.ttfb_ms.and_then(|value| i32::try_from(value).ok()),
        _ => None,
    }
}

fn status_text_e2e(status: E2eProbeStatus) -> &'static str {
    match status {
        E2eProbeStatus::Ok => "ok",
        E2eProbeStatus::HandshakeFailed => "handshake-failed",
        E2eProbeStatus::ChainBroken => "chain-broken",
        E2eProbeStatus::Timeout => "timeout",
        E2eProbeStatus::Unsupported => "unsupported",
    }
}

/// Without a connection there is no exit to check. An agent reporting a match with a status
/// other than ok is an agent bug — but blocking it here beats letting the database's CHECK throw
/// a 500: the right treatment for this row is recording it as uncheckable, not failing the whole
/// batch.
fn verdict_text(verdict: E2eExitVerdict, status: E2eProbeStatus) -> &'static str {
    if !matches!(status, E2eProbeStatus::Ok) {
        return "unknown";
    }
    match verdict {
        E2eExitVerdict::Match => "match",
        E2eExitVerdict::Mismatch => "mismatch",
        E2eExitVerdict::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use brocade_core::model::{ModelSnapshot, User};

    use super::e2e_probe_plan;
    use crate::StoreError;

    #[test]
    fn unpublishable_model_is_an_error_not_an_empty_probe_plan() {
        let snapshot = ModelSnapshot {
            revision: 1,
            overlay_cidr: "10.66.0.0/16".parse().unwrap(),
            settings: Default::default(),
            nodes: Vec::new(),
            node_egress_dns: Vec::new(),
            users: vec![
                User {
                    id: "alice".to_owned(),
                    tenant: "platform.acme".to_owned(),
                    uuid: "duplicate".to_owned(),
                },
                User {
                    id: "bob".to_owned(),
                    tenant: "platform.acme".to_owned(),
                    uuid: "duplicate".to_owned(),
                },
            ],
            external_outbounds: Vec::new(),
            apps: Vec::new(),
        };

        let error = e2e_probe_plan(&snapshot, "hk").unwrap_err();
        let blocked = match error {
            StoreError::PublishBlocked(blocked) => blocked,
            other => panic!("编译失败必须保留为发布门禁错误：{other:?}"),
        };
        assert_eq!(blocked.summary.errors, 1, "{:#?}", blocked.diagnostics);
        assert!(
            blocked
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "user.uuid-dup"),
            "{:#?}",
            blocked.diagnostics
        );
    }
}
