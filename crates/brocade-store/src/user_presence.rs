//! Current and recently disconnected public source addresses for user credentials.
//!
//! Xray owns connection truth; the Store keeps each node's latest accepted snapshot and a
//! bounded tail of completed epochs (30 days by default). Presence never enters revisions,
//! artifacts, usage accounting or the Agent's durable spool.

use std::collections::{BTreeMap, BTreeSet};

use brocade_core::model::parse_grant_label;
use brocade_deployment::protocol::{
    OnlineSourceProtocol, UserOnlineSources, MAX_ONLINE_SOURCE_ENTRIES,
    MAX_ONLINE_SOURCE_PROTOCOLS, MAX_ONLINE_SOURCE_USERS,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, QueryBuilder, Row, Transaction};

use crate::{
    admin::PUBLIC_OPERATOR_ID, agent::public_route_ip, input::required_text, AdminContext,
    AdminRole, Result, StoreError,
};

pub const USER_ONLINE_SOURCE_RETENTION_DAYS: u32 = 30;
pub const USER_ONLINE_SOURCE_HISTORY_LIMIT: usize = 256;
const ONLINE_SOURCE_FRESH_SECS: i64 = 120;
const ONLINE_SOURCE_CLOCK_SLOP_SECS: i64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserPresenceState {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserOnlineSourceView {
    pub ip: String,
    /// Stable identity of an ended ingress/IP epoch. Live presence remains grouped by IP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// When Console first confirmed the epoch had ended; absent for live or legacy snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_at: Option<String>,
    pub first_observed_at: String,
    pub last_observed_at: String,
    pub xray_last_seen_at: String,
    /// Start and byte counters for this live or completed continuous online epoch.
    /// Missing means the reporting Agent/Xray predates source-session accounting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online_since_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_uplink_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_downlink_bytes: Option<u64>,
    pub node_ids: Vec<String>,
    pub ingress_ids: Vec<String>,
    /// Associations must stay together: a protocol observed on one node is not evidence for another.
    #[serde(default)]
    pub accesses: Vec<UserOnlineSourceAccess>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserOnlineSourceAccess {
    pub node_id: String,
    pub ingress_id: String,
    pub last_observed_at: String,
    pub xray_last_seen_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online_since_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_uplink_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_downlink_bytes: Option<u64>,
    /// NULL means the last accepted snapshot predates protocol-aware reporting.
    pub protocols: Option<Vec<OnlineSourceProtocol>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingPresenceNode {
    pub node_id: String,
    /// NULL distinguishes never receiving a successful snapshot from an expired snapshot.
    pub last_reported_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPresenceView {
    pub tenant_id: String,
    pub user_id: String,
    pub state: UserPresenceState,
    pub expected_nodes: u64,
    pub reporting_nodes: u64,
    #[serde(default)]
    pub missing_nodes: Vec<MissingPresenceNode>,
    pub sources: Vec<UserOnlineSourceView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPresenceList {
    pub freshness_secs: u64,
    pub users: Vec<UserPresenceView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserOnlineSourceHistory {
    pub tenant_id: String,
    pub user_id: String,
    pub retention_days: u32,
    pub truncated: bool,
    pub sources: Vec<UserOnlineSourceView>,
}

#[derive(Debug, Clone)]
struct NormalizedSource {
    tenant_id: String,
    user_id: String,
    ingress_id: String,
    ip: String,
    xray_last_seen_unix_secs: i64,
    protocols: Option<Vec<OnlineSourceProtocol>>,
    online_since_unix_secs: Option<i64>,
    session_uplink_bytes: Option<i64>,
    session_downlink_bytes: Option<i64>,
}

#[derive(Debug, Clone)]
struct NormalizedObservation {
    xray_last_seen_unix_secs: i64,
    protocols: BTreeSet<OnlineSourceProtocol>,
    session: Option<(i64, i64, i64)>,
}

fn presence_state(expected_nodes: u64, reporting_nodes: u64) -> UserPresenceState {
    if expected_nodes == 0 || reporting_nodes == expected_nodes {
        UserPresenceState::Complete
    } else if reporting_nodes > 0 {
        UserPresenceState::Partial
    } else {
        UserPresenceState::Unavailable
    }
}

fn normalize_snapshot(
    observed_at_unix_secs: i64,
    users: &[UserOnlineSources],
) -> Result<Vec<NormalizedSource>> {
    if users.len() > MAX_ONLINE_SOURCE_USERS {
        return Err(StoreError::InvalidData(format!(
            "online source snapshot exceeds {MAX_ONLINE_SOURCE_USERS} users"
        )));
    }
    let supplied_sources = users.iter().try_fold(0_usize, |total, user| {
        total
            .checked_add(user.sources.len())
            .ok_or_else(|| StoreError::InvalidData("online source count overflow".to_owned()))
    })?;
    if supplied_sources > MAX_ONLINE_SOURCE_ENTRIES {
        return Err(StoreError::InvalidData(format!(
            "online source snapshot exceeds {MAX_ONLINE_SOURCE_ENTRIES} sources"
        )));
    }

    // Xray maps are already unique per label/IP, but normalizing here makes retries stable and
    // prevents a malformed Agent from producing duplicate upsert work.
    let mut normalized = BTreeMap::<(String, String, String, String), NormalizedObservation>::new();
    for user in users {
        let Some((user_id, tenant_id, ingress_id)) = parse_grant_label(&user.label) else {
            // Probe credentials and labels from extensions outside the grant namespace do not
            // belong to a Console user and are intentionally ignored.
            continue;
        };
        for source in &user.sources {
            let protocols = source.protocols.as_deref().unwrap_or_default();
            if protocols.len() > MAX_ONLINE_SOURCE_PROTOCOLS {
                return Err(StoreError::InvalidData(format!(
                    "online source exceeds {MAX_ONLINE_SOURCE_PROTOCOLS} protocols"
                )));
            }
            let Some(ip) = public_route_ip(&source.ip) else {
                // Only public sources define the product metric. Private/loopback addresses can
                // occur behind another local proxy and must not become misleading "devices".
                continue;
            };
            if source.last_seen_unix_secs <= 0
                || source.last_seen_unix_secs
                    > observed_at_unix_secs.saturating_add(ONLINE_SOURCE_CLOCK_SLOP_SECS)
            {
                return Err(StoreError::InvalidData(
                    "online source last_seen is outside the accepted clock window".to_owned(),
                ));
            }
            let session = match (
                source.online_since_unix_secs,
                source.session_uplink_bytes,
                source.session_downlink_bytes,
            ) {
                (None, None, None) => None,
                (Some(online_since), Some(uplink), Some(downlink))
                    if online_since > 0
                        && online_since <= source.last_seen_unix_secs
                        && online_since <= observed_at_unix_secs =>
                {
                    Some((
                        online_since,
                        i64::try_from(uplink).map_err(|_| {
                            StoreError::InvalidData(
                                "online source session uplink exceeds PostgreSQL BIGINT".to_owned(),
                            )
                        })?,
                        i64::try_from(downlink).map_err(|_| {
                            StoreError::InvalidData(
                                "online source session downlink exceeds PostgreSQL BIGINT"
                                    .to_owned(),
                            )
                        })?,
                    ))
                }
                _ => {
                    return Err(StoreError::InvalidData(
                        "online source session metrics are incomplete or outside the accepted clock window"
                            .to_owned(),
                    ));
                }
            };
            let protocols = if protocols.is_empty() {
                BTreeSet::from([OnlineSourceProtocol::Unknown])
            } else {
                protocols.iter().copied().collect()
            };
            let entry = normalized
                .entry((
                    tenant_id.to_owned(),
                    user_id.to_owned(),
                    ingress_id.to_owned(),
                    ip.to_string(),
                ))
                .or_insert_with(|| NormalizedObservation {
                    xray_last_seen_unix_secs: source.last_seen_unix_secs,
                    protocols: BTreeSet::new(),
                    session,
                });
            if entry.session != session {
                return Err(StoreError::InvalidData(
                    "duplicate online source entries disagree on session metrics".to_owned(),
                ));
            }
            entry.xray_last_seen_unix_secs = entry
                .xray_last_seen_unix_secs
                .max(source.last_seen_unix_secs);
            entry.protocols.extend(protocols);
        }
    }

    Ok(normalized
        .into_iter()
        .map(|((tenant_id, user_id, ingress_id, ip), observation)| {
            let (online_since_unix_secs, session_uplink_bytes, session_downlink_bytes) =
                observation.session.map_or(
                    (None, None, None),
                    |(online_since, uplink, downlink)| {
                        (Some(online_since), Some(uplink), Some(downlink))
                    },
                );
            NormalizedSource {
                tenant_id,
                user_id,
                ingress_id,
                ip,
                xray_last_seen_unix_secs: observation.xray_last_seen_unix_secs,
                protocols: (observation.protocols
                    != BTreeSet::from([OnlineSourceProtocol::Unknown]))
                .then(|| observation.protocols.into_iter().collect()),
                online_since_unix_secs,
                session_uplink_bytes,
                session_downlink_bytes,
            }
        })
        .collect())
}

/// Replace one node's active presence snapshot inside the runtime-report transaction.
pub(crate) async fn replace_node_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    observed_at_unix_secs: i64,
    users: &[UserOnlineSources],
) -> Result<()> {
    let sources = normalize_snapshot(observed_at_unix_secs, users)?;

    let tenant_ids = sources
        .iter()
        .map(|source| source.tenant_id.clone())
        .collect::<Vec<_>>();
    let user_ids = sources
        .iter()
        .map(|source| source.user_id.clone())
        .collect::<Vec<_>>();
    let ingress_ids = sources
        .iter()
        .map(|source| source.ingress_id.clone())
        .collect::<Vec<_>>();
    let ips = sources
        .iter()
        .map(|source| source.ip.clone())
        .collect::<Vec<_>>();
    let xray_last_seen = sources
        .iter()
        .map(|source| source.xray_last_seen_unix_secs)
        .collect::<Vec<_>>();
    let protocols = sources
        .iter()
        .map(|source| serde_json::to_value(&source.protocols))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let online_since = sources
        .iter()
        .map(|source| source.online_since_unix_secs)
        .collect::<Vec<_>>();
    let session_uplink = sources
        .iter()
        .map(|source| source.session_uplink_bytes)
        .collect::<Vec<_>>();
    let session_downlink = sources
        .iter()
        .map(|source| source.session_downlink_bytes)
        .collect::<Vec<_>>();

    // node_agent_state is already locked by the runtime-report transaction. End only missing
    // sources or epochs whose Xray start/counters changed, including reconnects between reports.
    // A failed/missing snapshot never reaches this function and cannot fabricate an offline event.
    sqlx::query(
        "WITH incoming AS (
            SELECT value.* FROM unnest(
                $3::text[], $4::text[], $5::text[], $6::text[],
                $7::bigint[], $8::bigint[], $9::bigint[]
            ) AS value(tenant_id, user_id, ingress_id, source_ip, online_since, uplink, downlink)
            JOIN grants g USING (tenant_id, user_id, ingress_id)
            JOIN ingresses i ON i.id = value.ingress_id AND i.node_id = $1
         )
         UPDATE user_online_sources source
            SET active = FALSE,
                last_observed_at = GREATEST(source.last_observed_at, to_timestamp($2)),
                offline_at = to_timestamp($2)
          WHERE source.node_id = $1 AND source.active
            AND NOT EXISTS (
                SELECT 1 FROM incoming value
                 WHERE value.tenant_id = source.tenant_id AND value.user_id = source.user_id
                   AND value.ingress_id = source.ingress_id AND value.source_ip::inet = source.source_ip
                   AND (
                       (source.online_since_at IS NOT NULL AND value.online_since IS NOT NULL
                        AND source.online_since_at <> to_timestamp(value.online_since::double precision))
                       OR value.uplink < source.session_uplink_bytes
                       OR value.downlink < source.session_downlink_bytes
                   ) IS NOT TRUE
            )",
    )
    .bind(node_id)
    .bind(observed_at_unix_secs as f64)
    .bind(&tenant_ids)
    .bind(&user_ids)
    .bind(&ingress_ids)
    .bind(&ips)
    .bind(&online_since)
    .bind(&session_uplink)
    .bind(&session_downlink)
    .execute(&mut **tx)
    .await?;

    if sources.is_empty() {
        return Ok(());
    }

    // Joining current grants and ingress ownership drops probe labels, stale credentials and a
    // compromised node attempting to attribute an address to another node's listener.
    sqlx::query(
        "WITH incoming AS (
            SELECT *
              FROM unnest(
                    $3::text[], $4::text[], $5::text[], $6::text[], $7::bigint[], $8::jsonb[],
                    $9::bigint[], $10::bigint[], $11::bigint[]
              ) AS value(tenant_id, user_id, ingress_id, source_ip, xray_last_seen, protocols,
                         online_since, session_uplink, session_downlink)
         )
         INSERT INTO user_online_sources (
            node_id, tenant_id, user_id, ingress_id, source_ip, active,
            first_observed_at, last_observed_at, xray_last_seen_at, offline_at, protocols,
            online_since_at, session_uplink_bytes, session_downlink_bytes
         )
         SELECT $1, value.tenant_id, value.user_id, value.ingress_id,
                value.source_ip::inet, TRUE,
                to_timestamp($2), to_timestamp($2),
                to_timestamp(value.xray_last_seen::double precision), NULL,
                NULLIF(value.protocols, 'null'::jsonb),
                to_timestamp(value.online_since::double precision),
                value.session_uplink, value.session_downlink
           FROM incoming value
           JOIN grants grant_row
             ON grant_row.tenant_id = value.tenant_id
            AND grant_row.user_id = value.user_id
            AND grant_row.ingress_id = value.ingress_id
           JOIN ingresses ingress
             ON ingress.id = value.ingress_id
            AND ingress.node_id = $1
         ON CONFLICT (node_id, tenant_id, user_id, ingress_id, source_ip) WHERE active
         DO UPDATE SET
            active = TRUE,
            last_observed_at = EXCLUDED.last_observed_at,
            xray_last_seen_at = EXCLUDED.xray_last_seen_at,
            protocols = EXCLUDED.protocols,
            online_since_at = EXCLUDED.online_since_at,
            session_uplink_bytes = EXCLUDED.session_uplink_bytes,
            session_downlink_bytes = EXCLUDED.session_downlink_bytes,
            offline_at = NULL",
    )
    .bind(node_id)
    .bind(observed_at_unix_secs as f64)
    .bind(tenant_ids)
    .bind(user_ids)
    .bind(ingress_ids)
    .bind(ips)
    .bind(xray_last_seen)
    .bind(protocols)
    .bind(online_since)
    .bind(session_uplink)
    .bind(session_downlink)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// List current online sources within the caller's user scope.
pub async fn list(pool: &PgPool, actor: &AdminContext) -> Result<UserPresenceList> {
    let mut query = QueryBuilder::<Postgres>::new(
        "WITH visible_users AS (
            SELECT tenant_id, id AS user_id FROM users WHERE ",
    );
    if let Some(user) = actor.self_user() {
        query
            .push("tenant_id = ")
            .push_bind(user.tenant_id.clone())
            .push(" AND id = ")
            .push_bind(user.user_id.clone());
    } else if actor.is_global_scope() {
        query.push("TRUE");
    } else {
        let scope = actor
            .tenant_scope()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))?;
        let pattern = actor
            .tenant_scope_like_pattern()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))?;
        query
            .push("(tenant_id = ")
            .push_bind(scope.to_owned())
            .push(" OR tenant_id LIKE ")
            .push_bind(pattern)
            .push(" ESCAPE E'\\\\')");
    }
    query.push(
        "), entry_nodes AS (
            SELECT DISTINCT visible.tenant_id, visible.user_id, ingress.node_id,
                   agent.online_sources_reported_at,
                   agent.online_sources_reported_at >=
                       now() - make_interval(secs => ",
    );
    query.push_bind(ONLINE_SOURCE_FRESH_SECS as i32);
    query.push(
        ") AS fresh
              FROM visible_users visible
              LEFT JOIN grants grant_row
                ON grant_row.tenant_id = visible.tenant_id
               AND grant_row.user_id = visible.user_id
              LEFT JOIN ingresses ingress ON ingress.id = grant_row.ingress_id
              LEFT JOIN node_agent_state agent ON agent.node_id = ingress.node_id
         ), expected AS (
            SELECT tenant_id, user_id,
                   count(node_id)::bigint AS expected_nodes,
                   count(node_id) FILTER (WHERE fresh)::bigint AS reporting_nodes,
                   COALESCE(jsonb_agg(jsonb_build_object(
                       'node_id', node_id,
                       'last_reported_at', online_sources_reported_at::text
                   ) ORDER BY node_id) FILTER (
                       WHERE node_id IS NOT NULL AND fresh IS NOT TRUE
                   ), '[]'::jsonb) AS missing_nodes
              FROM entry_nodes
             GROUP BY tenant_id, user_id
         ), source_rows AS (
            SELECT source.tenant_id, source.user_id, host(source.source_ip) AS ip,
                   min(source.first_observed_at)::text AS first_observed_at,
                   max(source.last_observed_at)::text AS last_observed_at,
                   max(source.xray_last_seen_at)::text AS xray_last_seen_at,
                   CASE WHEN count(*) FILTER (WHERE source.online_since_at IS NULL) = 0
                        THEN min(source.online_since_at)::text END AS online_since_at,
                   CASE WHEN count(*) FILTER (WHERE source.session_uplink_bytes IS NULL) = 0
                        THEN LEAST(sum(source.session_uplink_bytes),
                                   9223372036854775807::numeric)::bigint END
                        AS session_uplink_bytes,
                   CASE WHEN count(*) FILTER (WHERE source.session_downlink_bytes IS NULL) = 0
                        THEN LEAST(sum(source.session_downlink_bytes),
                                   9223372036854775807::numeric)::bigint END
                        AS session_downlink_bytes,
                   array_agg(DISTINCT source.node_id ORDER BY source.node_id) AS node_ids,
                   array_agg(DISTINCT source.ingress_id ORDER BY source.ingress_id) AS ingress_ids,
                   jsonb_agg(jsonb_build_object(
                       'node_id', source.node_id, 'ingress_id', source.ingress_id,
                       'protocols', source.protocols,
                       'last_observed_at', source.last_observed_at::text,
                       'xray_last_seen_at', source.xray_last_seen_at::text,
                       'online_since_at', source.online_since_at::text,
                       'session_uplink_bytes', source.session_uplink_bytes,
                       'session_downlink_bytes', source.session_downlink_bytes
                   ) ORDER BY source.node_id, source.ingress_id) AS accesses
              FROM user_online_sources source
              JOIN visible_users visible
                ON visible.tenant_id = source.tenant_id
               AND visible.user_id = source.user_id
              JOIN node_agent_state agent ON agent.node_id = source.node_id
              JOIN grants grant_row
                ON grant_row.tenant_id = source.tenant_id
               AND grant_row.user_id = source.user_id
               AND grant_row.ingress_id = source.ingress_id
              JOIN ingresses ingress
                ON ingress.id = source.ingress_id
               AND ingress.node_id = source.node_id
             WHERE source.active
               AND agent.online_sources_reported_at >= now() - make_interval(secs => ",
    );
    query.push_bind(ONLINE_SOURCE_FRESH_SECS as i32);
    query.push(
        ")
             GROUP BY source.tenant_id, source.user_id, source.source_ip
         )
         SELECT expected.tenant_id, expected.user_id,
                expected.expected_nodes, expected.reporting_nodes, expected.missing_nodes,
                COALESCE(
                    jsonb_agg(
                        jsonb_build_object(
                            'ip', source.ip,
                            'first_observed_at', source.first_observed_at,
                            'last_observed_at', source.last_observed_at,
                            'xray_last_seen_at', source.xray_last_seen_at,
                            'online_since_at', source.online_since_at,
                            'session_uplink_bytes', source.session_uplink_bytes,
                            'session_downlink_bytes', source.session_downlink_bytes,
                            'node_ids', source.node_ids,
                            'ingress_ids', source.ingress_ids,
                            'accesses', source.accesses
                        ) ORDER BY source.last_observed_at DESC, source.ip
                    ) FILTER (WHERE source.ip IS NOT NULL),
                    '[]'::jsonb
                ) AS sources
           FROM expected
           LEFT JOIN source_rows source
             ON source.tenant_id = expected.tenant_id
            AND source.user_id = expected.user_id
          GROUP BY expected.tenant_id, expected.user_id,
                   expected.expected_nodes, expected.reporting_nodes, expected.missing_nodes
          ORDER BY expected.tenant_id, expected.user_id",
    );

    let rows = query.build().fetch_all(pool).await?;
    let users = rows
        .into_iter()
        .map(|row| {
            let expected_nodes = u64::try_from(row.try_get::<i64, _>("expected_nodes")?)
                .map_err(|_| StoreError::InvalidData("negative expected node count".to_owned()))?;
            let reporting_nodes = u64::try_from(row.try_get::<i64, _>("reporting_nodes")?)
                .map_err(|_| StoreError::InvalidData("negative reporting node count".to_owned()))?;
            let state = presence_state(expected_nodes, reporting_nodes);
            let sources = serde_json::from_value(row.try_get("sources")?)?;
            Ok(UserPresenceView {
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
                state,
                expected_nodes,
                reporting_nodes,
                missing_nodes: serde_json::from_value(row.try_get("missing_nodes")?)?,
                sources,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(UserPresenceList {
        freshness_secs: ONLINE_SOURCE_FRESH_SECS as u64,
        users,
    })
}

/// List completed ingress/IP epochs for one user, including addresses currently online again.
///
/// History is intentionally separate from [`list`]: the roster polls current presence, while
/// incident-review epochs are fetched only after an administrator expands one user. Expired
/// active snapshots are unknown, not completed sessions: only a successful report can end one.
pub async fn history(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<UserOnlineSourceHistory> {
    let retention_hours = crate::retention::ttl_hours(pool, "user_online_sources").await?;
    let retention_days = retention_hours.div_ceil(24);
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let user_id = required_text(user_id, "user_id")?;
    if actor.role() == AdminRole::User || actor.operator_id() == PUBLIC_OPERATOR_ID {
        return Err(StoreError::Forbidden(
            "online source history is restricted to administrators".to_owned(),
        ));
    }
    if !actor.is_global_scope() {
        actor.require_tenant_access(&tenant_id, "online source history")?;
    }

    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE tenant_id = $1 AND id = $2)")
            .bind(&tenant_id)
            .bind(&user_id)
            .fetch_one(pool)
            .await?;
    if !exists {
        return Err(StoreError::NotFound(format!("user {tenant_id}/{user_id}")));
    }

    let fetch_limit = i64::try_from(USER_ONLINE_SOURCE_HISTORY_LIMIT + 1)
        .expect("online source history limit fits i64");
    let rows = sqlx::query(
        "SELECT host(source.source_ip) AS ip, source.session_id::text AS session_id,
                source.offline_at::text AS offline_at,
                source.first_observed_at::text AS first_observed_at,
                source.last_observed_at::text AS last_observed_at,
                source.xray_last_seen_at::text AS xray_last_seen_at,
                source.online_since_at::text AS online_since_at,
                source.session_uplink_bytes, source.session_downlink_bytes,
                ARRAY[source.node_id] AS node_ids, ARRAY[source.ingress_id] AS ingress_ids,
                jsonb_build_array(jsonb_build_object(
                    'node_id', source.node_id, 'ingress_id', source.ingress_id,
                    'protocols', source.protocols,
                    'last_observed_at', source.last_observed_at::text,
                    'xray_last_seen_at', source.xray_last_seen_at::text,
                    'online_since_at', source.online_since_at::text,
                    'session_uplink_bytes', source.session_uplink_bytes,
                    'session_downlink_bytes', source.session_downlink_bytes
                )) AS accesses
           FROM user_online_sources source
          WHERE source.tenant_id = $1
            AND source.user_id = $2
            AND NOT source.active
            AND source.last_observed_at >= now() - make_interval(secs => $3)
          ORDER BY source.last_observed_at DESC, host(source.source_ip), source.session_id DESC
          LIMIT $4",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(f64::from(retention_hours) * 3600.0)
    .bind(fetch_limit)
    .fetch_all(pool)
    .await?;

    let truncated = rows.len() > USER_ONLINE_SOURCE_HISTORY_LIMIT;
    let sources = rows
        .into_iter()
        .take(USER_ONLINE_SOURCE_HISTORY_LIMIT)
        .map(|row| {
            Ok(UserOnlineSourceView {
                ip: row.try_get("ip")?,
                session_id: row.try_get("session_id")?,
                offline_at: row.try_get("offline_at")?,
                first_observed_at: row.try_get("first_observed_at")?,
                last_observed_at: row.try_get("last_observed_at")?,
                xray_last_seen_at: row.try_get("xray_last_seen_at")?,
                online_since_at: row.try_get("online_since_at")?,
                session_uplink_bytes: row
                    .try_get::<Option<i64>, _>("session_uplink_bytes")?
                    .map(u64::try_from)
                    .transpose()
                    .map_err(|_| {
                        StoreError::InvalidData(
                            "negative online source session uplink bytes".to_owned(),
                        )
                    })?,
                session_downlink_bytes: row
                    .try_get::<Option<i64>, _>("session_downlink_bytes")?
                    .map(u64::try_from)
                    .transpose()
                    .map_err(|_| {
                        StoreError::InvalidData(
                            "negative online source session downlink bytes".to_owned(),
                        )
                    })?,
                node_ids: row.try_get("node_ids")?,
                ingress_ids: row.try_get("ingress_ids")?,
                accesses: serde_json::from_value(row.try_get("accesses")?)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(UserOnlineSourceHistory {
        tenant_id,
        user_id,
        retention_days,
        truncated,
        sources,
    })
}

pub async fn prune(pool: &PgPool, retain_days: u32) -> Result<u64> {
    prune_hours(pool, retain_days.saturating_mul(24)).await
}

pub(crate) async fn prune_hours(pool: &PgPool, retain_hours: u32) -> Result<u64> {
    crate::retention::cleanup::prune_batch(pool, "user_online_sources", retain_hours).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use brocade_deployment::protocol::OnlineSource;

    #[test]
    fn snapshot_normalization_deduplicates_and_keeps_only_public_addresses() {
        let users = vec![UserOnlineSources {
            label: "alice@platform#i-main".to_owned(),
            sources: vec![
                OnlineSource {
                    ip: "1.1.1.1".to_owned(),
                    last_seen_unix_secs: 90,
                    protocols: Some(vec![OnlineSourceProtocol::Vless]),
                    online_since_unix_secs: None,
                    session_uplink_bytes: None,
                    session_downlink_bytes: None,
                },
                OnlineSource {
                    ip: "1.1.1.1".to_owned(),
                    last_seen_unix_secs: 95,
                    protocols: Some(vec![
                        OnlineSourceProtocol::AnyTls,
                        OnlineSourceProtocol::Vless,
                    ]),
                    online_since_unix_secs: None,
                    session_uplink_bytes: None,
                    session_downlink_bytes: None,
                },
                OnlineSource {
                    ip: "192.168.1.2".to_owned(),
                    last_seen_unix_secs: 99,
                    protocols: None,
                    online_since_unix_secs: None,
                    session_uplink_bytes: None,
                    session_downlink_bytes: None,
                },
            ],
        }];
        let normalized = normalize_snapshot(100, &users).unwrap();
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].ip, "1.1.1.1");
        assert_eq!(normalized[0].xray_last_seen_unix_secs, 95);
        assert_eq!(
            normalized[0].protocols,
            Some(vec![
                OnlineSourceProtocol::Vless,
                OnlineSourceProtocol::AnyTls
            ])
        );
    }

    #[test]
    fn snapshot_normalization_ignores_non_user_labels() {
        let users = vec![UserOnlineSources {
            label: "probe#i-main".to_owned(),
            sources: vec![OnlineSource {
                ip: "1.1.1.1".to_owned(),
                last_seen_unix_secs: 90,
                protocols: None,
                online_since_unix_secs: None,
                session_uplink_bytes: None,
                session_downlink_bytes: None,
            }],
        }];
        assert!(normalize_snapshot(100, &users).unwrap().is_empty());
    }

    #[test]
    fn snapshot_protocols_preserve_unknown_references_and_enforce_bounds() {
        let mut users = vec![UserOnlineSources {
            label: "alice@platform#i-main".to_owned(),
            sources: vec![OnlineSource {
                ip: "1.1.1.1".to_owned(),
                last_seen_unix_secs: 90,
                protocols: None,
                online_since_unix_secs: None,
                session_uplink_bytes: None,
                session_downlink_bytes: None,
            }],
        }];
        assert_eq!(normalize_snapshot(100, &users).unwrap()[0].protocols, None);
        users[0].sources.push(OnlineSource {
            ip: "1.1.1.1".to_owned(),
            last_seen_unix_secs: 95,
            protocols: Some(vec![OnlineSourceProtocol::Hysteria2]),
            online_since_unix_secs: None,
            session_uplink_bytes: None,
            session_downlink_bytes: None,
        });
        assert_eq!(
            normalize_snapshot(100, &users).unwrap()[0].protocols,
            Some(vec![
                OnlineSourceProtocol::Hysteria2,
                OnlineSourceProtocol::Unknown
            ])
        );
        users[0].sources[0].protocols = Some(vec![
            OnlineSourceProtocol::Vless;
            MAX_ONLINE_SOURCE_PROTOCOLS + 1
        ]);
        assert!(normalize_snapshot(100, &users).is_err());
    }

    #[test]
    fn snapshot_session_metrics_stay_atomic_and_exact() {
        let mut users = vec![UserOnlineSources {
            label: "alice@platform#i-main".to_owned(),
            sources: vec![OnlineSource {
                ip: "1.1.1.1".to_owned(),
                last_seen_unix_secs: 95,
                protocols: Some(vec![OnlineSourceProtocol::Vless]),
                online_since_unix_secs: Some(80),
                session_uplink_bytes: Some(1_024),
                session_downlink_bytes: Some(2_048),
            }],
        }];
        let normalized = normalize_snapshot(100, &users).unwrap();
        assert_eq!(normalized[0].online_since_unix_secs, Some(80));
        assert_eq!(normalized[0].session_uplink_bytes, Some(1_024));
        assert_eq!(normalized[0].session_downlink_bytes, Some(2_048));

        users[0].sources[0].session_downlink_bytes = None;
        assert!(normalize_snapshot(100, &users).is_err());
    }

    #[test]
    fn presence_state_distinguishes_complete_partial_and_unavailable_coverage() {
        assert_eq!(presence_state(0, 0), UserPresenceState::Complete);
        assert_eq!(presence_state(2, 2), UserPresenceState::Complete);
        assert_eq!(presence_state(2, 1), UserPresenceState::Partial);
        assert_eq!(presence_state(2, 0), UserPresenceState::Unavailable);
    }
}
