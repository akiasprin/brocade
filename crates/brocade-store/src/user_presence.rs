//! Current and recently disconnected public source addresses for user credentials.
//!
//! Xray owns connection truth; the Store keeps each node's latest accepted snapshot plus a
//! bounded 30-day tail for incident review. Presence never enters revisions, artifacts, usage
//! accounting or the Agent's durable spool.

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
    pub first_observed_at: String,
    pub last_observed_at: String,
    pub xray_last_seen_at: String,
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
    /// NULL means the last accepted snapshot predates protocol-aware reporting.
    pub protocols: Option<Vec<OnlineSourceProtocol>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPresenceView {
    pub tenant_id: String,
    pub user_id: String,
    pub state: UserPresenceState,
    pub expected_nodes: u64,
    pub reporting_nodes: u64,
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
    let mut normalized =
        BTreeMap::<(String, String, String, String), (i64, BTreeSet<OnlineSourceProtocol>)>::new();
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
                .or_insert_with(|| (source.last_seen_unix_secs, BTreeSet::new()));
            entry.0 = entry.0.max(source.last_seen_unix_secs);
            entry.1.extend(protocols);
        }
    }

    Ok(normalized
        .into_iter()
        .map(
            |((tenant_id, user_id, ingress_id, ip), (xray_last_seen_unix_secs, protocols))| {
                NormalizedSource {
                    tenant_id,
                    user_id,
                    ingress_id,
                    ip,
                    xray_last_seen_unix_secs,
                    protocols: (protocols != BTreeSet::from([OnlineSourceProtocol::Unknown]))
                        .then(|| protocols.into_iter().collect()),
                }
            },
        )
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

    // First close the preceding snapshot. Rows present below are reactivated by the upsert in the
    // same transaction, so readers never observe the intermediate state.
    sqlx::query(
        "UPDATE user_online_sources
            SET active = FALSE,
                last_observed_at = GREATEST(last_observed_at, to_timestamp($2)),
                offline_at = to_timestamp($2)
          WHERE node_id = $1 AND active",
    )
    .bind(node_id)
    .bind(observed_at_unix_secs as f64)
    .execute(&mut **tx)
    .await?;

    if sources.is_empty() {
        return Ok(());
    }

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

    // Joining current grants and ingress ownership drops probe labels, stale credentials and a
    // compromised node attempting to attribute an address to another node's listener.
    sqlx::query(
        "WITH incoming AS (
            SELECT *
              FROM unnest(
                    $3::text[], $4::text[], $5::text[], $6::text[], $7::bigint[], $8::jsonb[]
              ) AS value(tenant_id, user_id, ingress_id, source_ip, xray_last_seen, protocols)
         )
         INSERT INTO user_online_sources (
            node_id, tenant_id, user_id, ingress_id, source_ip, active,
            first_observed_at, last_observed_at, xray_last_seen_at, offline_at, protocols
         )
         SELECT $1, value.tenant_id, value.user_id, value.ingress_id,
                value.source_ip::inet, TRUE,
                to_timestamp($2), to_timestamp($2),
                to_timestamp(value.xray_last_seen::double precision), NULL,
                NULLIF(value.protocols, 'null'::jsonb)
           FROM incoming value
           JOIN grants grant_row
             ON grant_row.tenant_id = value.tenant_id
            AND grant_row.user_id = value.user_id
            AND grant_row.ingress_id = value.ingress_id
           JOIN ingresses ingress
             ON ingress.id = value.ingress_id
            AND ingress.node_id = $1
         ON CONFLICT (node_id, tenant_id, user_id, ingress_id, source_ip)
         DO UPDATE SET
            active = TRUE,
            last_observed_at = EXCLUDED.last_observed_at,
            xray_last_seen_at = EXCLUDED.xray_last_seen_at,
            protocols = EXCLUDED.protocols,
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
            .push(" ESCAPE '\\\\')");
    }
    query.push(
        "), expected AS (
            SELECT visible.tenant_id, visible.user_id,
                   count(DISTINCT ingress.node_id)::bigint AS expected_nodes,
                   count(DISTINCT ingress.node_id) FILTER (
                       WHERE agent.online_sources_reported_at >=
                             now() - make_interval(secs => ",
    );
    query.push_bind(ONLINE_SOURCE_FRESH_SECS as i32);
    query.push(
        "))::bigint AS reporting_nodes
              FROM visible_users visible
              LEFT JOIN grants grant_row
                ON grant_row.tenant_id = visible.tenant_id
               AND grant_row.user_id = visible.user_id
              LEFT JOIN ingresses ingress ON ingress.id = grant_row.ingress_id
              LEFT JOIN node_agent_state agent ON agent.node_id = ingress.node_id
             GROUP BY visible.tenant_id, visible.user_id
         ), source_rows AS (
            SELECT source.tenant_id, source.user_id, host(source.source_ip) AS ip,
                   min(source.first_observed_at)::text AS first_observed_at,
                   max(source.last_observed_at)::text AS last_observed_at,
                   max(source.xray_last_seen_at)::text AS xray_last_seen_at,
                   array_agg(DISTINCT source.node_id ORDER BY source.node_id) AS node_ids,
                   array_agg(DISTINCT source.ingress_id ORDER BY source.ingress_id) AS ingress_ids,
                   jsonb_agg(jsonb_build_object(
                       'node_id', source.node_id, 'ingress_id', source.ingress_id,
                       'protocols', source.protocols
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
                expected.expected_nodes, expected.reporting_nodes,
                COALESCE(
                    jsonb_agg(
                        jsonb_build_object(
                            'ip', source.ip,
                            'first_observed_at', source.first_observed_at,
                            'last_observed_at', source.last_observed_at,
                            'xray_last_seen_at', source.xray_last_seen_at,
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
                   expected.expected_nodes, expected.reporting_nodes
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
                sources,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(UserPresenceList {
        freshness_secs: ONLINE_SOURCE_FRESH_SECS as u64,
        users,
    })
}

/// List recently disconnected public source addresses for one user.
///
/// History is intentionally separate from [`list`]: the roster polls current presence, while
/// incident-review addresses are fetched only after an administrator expands one user. An IP
/// that is current on any freshly reporting node is omitted even if another node already has an
/// offline row for the same address.
pub async fn history(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<UserOnlineSourceHistory> {
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
        "SELECT host(source.source_ip) AS ip,
                min(source.first_observed_at)::text AS first_observed_at,
                max(source.last_observed_at)::text AS last_observed_at,
                max(source.xray_last_seen_at)::text AS xray_last_seen_at,
                array_agg(DISTINCT source.node_id ORDER BY source.node_id) AS node_ids,
                array_agg(DISTINCT source.ingress_id ORDER BY source.ingress_id) AS ingress_ids,
                jsonb_agg(jsonb_build_object(
                    'node_id', source.node_id, 'ingress_id', source.ingress_id,
                    'protocols', source.protocols
                ) ORDER BY source.node_id, source.ingress_id) AS accesses
           FROM user_online_sources source
          WHERE source.tenant_id = $1
            AND source.user_id = $2
            AND source.last_observed_at >= now() - make_interval(days => $3)
            AND NOT EXISTS (
                SELECT 1
                  FROM user_online_sources current_source
                  JOIN node_agent_state agent ON agent.node_id = current_source.node_id
                 WHERE current_source.tenant_id = source.tenant_id
                   AND current_source.user_id = source.user_id
                   AND current_source.source_ip = source.source_ip
                   AND current_source.active
                   AND agent.online_sources_reported_at >=
                       now() - make_interval(secs => $4)
            )
          GROUP BY source.source_ip
          ORDER BY max(source.last_observed_at) DESC, host(source.source_ip)
          LIMIT $5",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(USER_ONLINE_SOURCE_RETENTION_DAYS as i32)
    .bind(ONLINE_SOURCE_FRESH_SECS as i32)
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
                first_observed_at: row.try_get("first_observed_at")?,
                last_observed_at: row.try_get("last_observed_at")?,
                xray_last_seen_at: row.try_get("xray_last_seen_at")?,
                node_ids: row.try_get("node_ids")?,
                ingress_ids: row.try_get("ingress_ids")?,
                accesses: serde_json::from_value(row.try_get("accesses")?)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(UserOnlineSourceHistory {
        tenant_id,
        user_id,
        retention_days: USER_ONLINE_SOURCE_RETENTION_DAYS,
        truncated,
        sources,
    })
}

pub async fn prune(pool: &PgPool, retain_days: u32) -> Result<u64> {
    let retain_days =
        i32::try_from(retain_days.clamp(1, 3650)).expect("online source retention fits i32");
    // Every accepted snapshot refreshes last_observed_at for active rows. An active row older
    // than the retention window therefore belongs to a node that stopped reporting; keeping it
    // forever would make the documented bounded history unbounded for offline nodes.
    let result = sqlx::query(
        "DELETE FROM user_online_sources
          WHERE last_observed_at < now() - make_interval(days => $1)",
    )
    .bind(retain_days)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
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
                },
                OnlineSource {
                    ip: "1.1.1.1".to_owned(),
                    last_seen_unix_secs: 95,
                    protocols: Some(vec![
                        OnlineSourceProtocol::AnyTls,
                        OnlineSourceProtocol::Vless,
                    ]),
                },
                OnlineSource {
                    ip: "192.168.1.2".to_owned(),
                    last_seen_unix_secs: 99,
                    protocols: None,
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
            }],
        }];
        assert_eq!(normalize_snapshot(100, &users).unwrap()[0].protocols, None);
        users[0].sources.push(OnlineSource {
            ip: "1.1.1.1".to_owned(),
            last_seen_unix_secs: 95,
            protocols: Some(vec![OnlineSourceProtocol::Hysteria2]),
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
    fn presence_state_distinguishes_complete_partial_and_unavailable_coverage() {
        assert_eq!(presence_state(0, 0), UserPresenceState::Complete);
        assert_eq!(presence_state(2, 2), UserPresenceState::Complete);
        assert_eq!(presence_state(2, 1), UserPresenceState::Partial);
        assert_eq!(presence_state(2, 0), UserPresenceState::Unavailable);
    }
}
