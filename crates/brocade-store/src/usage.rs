use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use brocade_core::model::{
    grant_label, is_probe_label, parse_grant_label, Action, HopDial, ModelSnapshot,
};
use brocade_deployment::plan::{DesiredArtifact, DesiredGrants, NodeDesiredState, PlannedAction};

use brocade_deployment::protocol::{
    RealtimeForwardingTarget, UsageChainSample, UsageDailyRow, UsageForwardingMonthlyRow,
    UsageForwardingWindow, UsageMonthlySummary, UsageMonthlyViewRow, UsageNodeBucket,
    UsageNodeSeries, UsageNodeSeriesList, UsageReportRequest, UsageReportResult, UsageSample,
    UsageSampleList, UserTrafficHistory, UserTrafficHistoryNode, UserTrafficHistorySeries,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, Result, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRealtimeTarget {
    pub node_id: String,
    pub labels: Vec<String>,
    pub forwarding: Vec<RealtimeForwardingTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FleetRealtimeUser {
    pub tenant_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetRealtimeTarget {
    pub node_id: String,
    pub users: Vec<FleetRealtimeUser>,
    pub forwarding: Vec<RealtimeForwardingTarget>,
}

/// Resolve every currently entitled user by active entry machine for the fleet traffic view.
///
/// The Agent's `All` selector deliberately omits idle rows. Keeping this authorization model next
/// to the single-user resolver is what lets the UI distinguish an entitled user with zero traffic
/// from a user who is not routed through that machine at all. The matrix is a read-only public
/// surface; signed-in tenant-scoped viewers remain restricted to their tenant subtree by the
/// query below. Machine detail and per-user history require a signed-in operator, with the same tenant scope.
pub async fn fleet_realtime_targets(
    pool: &PgPool,
    actor: &AdminContext,
) -> Result<Vec<FleetRealtimeTarget>> {
    let rows = sqlx::query(
        "SELECT ingress.node_id, grant_row.tenant_id, grant_row.user_id
           FROM grants grant_row
           JOIN ingresses ingress ON ingress.id = grant_row.ingress_id
           JOIN nodes node ON node.id = ingress.node_id
          WHERE node.retired_at IS NULL
            AND ($1::text IS NULL OR grant_row.tenant_id = $1 OR grant_row.tenant_id LIKE $2 ESCAPE '\\')
          GROUP BY ingress.node_id, grant_row.tenant_id, grant_row.user_id
          ORDER BY ingress.node_id, grant_row.tenant_id, grant_row.user_id",
    )
    .bind(actor.tenant_scope())
    .bind(actor.tenant_scope_like_pattern())
    .fetch_all(pool)
    .await?;
    let mut grouped =
        BTreeMap::<String, (BTreeSet<FleetRealtimeUser>, Vec<RealtimeForwardingTarget>)>::new();
    for row in rows {
        grouped
            .entry(row.try_get("node_id")?)
            .or_default()
            .0
            .insert(FleetRealtimeUser {
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
            });
    }
    for (node_id, target) in read_forwarding_realtime_targets(pool, actor, None, None).await? {
        let entry = grouped.entry(node_id).or_default();
        entry.0.insert(FleetRealtimeUser {
            tenant_id: target.tenant_id.clone(),
            user_id: target.user_id.clone(),
        });
        entry.1.push(target);
    }
    Ok(grouped
        .into_iter()
        .map(|(node_id, (users, forwarding))| FleetRealtimeTarget {
            node_id,
            users: users.into_iter().collect(),
            forwarding,
        })
        .collect())
}

/// Resolve current grant labels and entitled forwarding rules, grouped by active entry machine.
/// This is live authorization, not revision history: revoked sources leave the next live lease.
pub async fn user_realtime_targets(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<Vec<UserRealtimeTarget>> {
    if !actor.can_view_user_realtime_traffic() {
        return Err(StoreError::Forbidden(
            "user realtime traffic requires a signed-in operator".to_owned(),
        ));
    }
    actor.require_tenant_access(tenant_id, "user realtime traffic")?;
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM users WHERE tenant_id = $1 AND id = $2)",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    if !exists {
        return Err(StoreError::NotFound(format!("user {tenant_id}/{user_id}")));
    }

    let rows = sqlx::query(
        "SELECT ingress.node_id, grant_row.ingress_id
           FROM grants grant_row
           JOIN ingresses ingress ON ingress.id = grant_row.ingress_id
           JOIN nodes node ON node.id = ingress.node_id
          WHERE grant_row.tenant_id = $1
            AND grant_row.user_id = $2
            AND node.retired_at IS NULL
          ORDER BY ingress.node_id, grant_row.ingress_id",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let mut grouped = BTreeMap::<String, Vec<String>>::new();
    for row in rows {
        let node_id: String = row.try_get("node_id")?;
        let ingress_id: String = row.try_get("ingress_id")?;
        grouped
            .entry(node_id)
            .or_default()
            .push(grant_label(user_id, tenant_id, &ingress_id));
    }
    let mut targets = grouped
        .into_iter()
        .map(|(node_id, labels)| {
            (
                node_id.clone(),
                UserRealtimeTarget {
                    node_id,
                    labels,
                    forwarding: Vec::new(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (node_id, target) in
        forwarding_realtime_targets(pool, actor, None, Some((tenant_id, user_id))).await?
    {
        targets
            .entry(node_id.clone())
            .or_insert_with(|| UserRealtimeTarget {
                node_id,
                labels: Vec::new(),
                forwarding: Vec::new(),
            })
            .forwarding
            .push(target);
    }
    Ok(targets.into_values().collect())
}

pub async fn forwarding_realtime_targets(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: Option<&str>,
    user: Option<(&str, &str)>,
) -> Result<Vec<(String, RealtimeForwardingTarget)>> {
    if !actor.can_view_user_realtime_traffic() {
        return Err(StoreError::Forbidden(
            "forwarding realtime requires a signed-in operator".to_owned(),
        ));
    }
    read_forwarding_realtime_targets(pool, actor, node_id, user).await
}

async fn read_forwarding_realtime_targets(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: Option<&str>,
    user: Option<(&str, &str)>,
) -> Result<Vec<(String, RealtimeForwardingTarget)>> {
    if let Some((tenant_id, _)) = user {
        actor.require_tenant_access(tenant_id, "forwarding realtime")?;
    }
    let rows = sqlx::query(
        "SELECT r.id,r.node_id,r.tenant_id,r.user_id FROM forwarding_rules r
         JOIN forwarding_entitlements e ON e.tenant_id=r.tenant_id AND e.user_id=r.user_id AND e.node_id=r.node_id
         JOIN users u ON u.tenant_id=r.tenant_id AND u.id=r.user_id
         JOIN nodes n ON n.id=r.node_id
         WHERE r.enabled AND r.deleted_at IS NULL AND e.enabled AND u.status='active' AND n.retired_at IS NULL
           AND ($1::text IS NULL OR r.node_id=$1)
           AND ($2::text IS NULL OR r.tenant_id=$2 OR r.tenant_id LIKE $3 ESCAPE '\\')
           AND ($4::text IS NULL OR r.tenant_id=$4 AND r.user_id=$5)
         ORDER BY r.node_id,r.id",
    ).bind(node_id).bind(actor.tenant_scope()).bind(actor.tenant_scope_like_pattern())
     .bind(user.map(|u| u.0)).bind(user.map(|u| u.1)).fetch_all(pool).await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("node_id")?,
                RealtimeForwardingTarget {
                    rule_id: row.try_get("id")?,
                    tenant_id: row.try_get("tenant_id")?,
                    user_id: row.try_get("user_id")?,
                },
            ))
        })
        .collect()
}

/// Which accounting history the realtime user-traffic card reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserTrafficHistoryScope<'a> {
    /// Every user in the actor's tenant scope on one ingress machine (machine detail).
    Node(&'a str),
    /// One user on every machine that carried or may carry its traffic (user detail).
    User {
        tenant_id: &'a str,
        user_id: &'a str,
    },
}

/// The user detail retains its recent per-machine history. Machine statistics use calendar bounds.
const USER_TRAFFIC_HISTORY_MAX_SECS: u32 = 3_600;

/// Recent per-user accounting windows for the realtime user-traffic card.
///
/// Authorization matches the live user rates: signed-in operators can read them, with
/// tenant-scoped operators limited to their subtree. Window existence comes from
/// `usage_node_windows`, which records every delivered report including idle ones; it carries
/// timestamps only and is limited to machines the caller already resolved, so it is read without
/// a tenant filter.
pub async fn list_user_traffic_history(
    pool: &PgPool,
    actor: &AdminContext,
    scope: UserTrafficHistoryScope<'_>,
    window_secs: u32,
) -> Result<UserTrafficHistory> {
    let range_end: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(pool)
        .await?;
    let range_start = range_end - i64::from(window_secs.clamp(60, USER_TRAFFIC_HISTORY_MAX_SECS));
    traffic_history_range(pool, actor, scope, range_start, range_end, false).await
}

/// Aggregate machine identities in SQL before loading long ranges. Complete detail preserves
/// individual measurement bounds; independently retained machine windows use receipt bounds.
/// The response is then reduced to bounded chart buckets instead of sending every user's month.
pub(crate) async fn traffic_history_range(
    pool: &PgPool,
    actor: &AdminContext,
    scope: UserTrafficHistoryScope<'_>,
    range_start: i64,
    range_end: i64,
    aggregate_node: bool,
) -> Result<UserTrafficHistory> {
    if !actor.can_view_user_realtime_traffic() {
        return Err(StoreError::Forbidden(
            "user traffic history requires a signed-in operator".to_owned(),
        ));
    }
    // The user scope reuses the live authorization: tenant access, user existence and the
    // machines that currently hold the user's grants.
    let granted_nodes = match scope {
        UserTrafficHistoryScope::Node(_) => Vec::new(),
        UserTrafficHistoryScope::User { tenant_id, user_id } => {
            user_realtime_targets(pool, actor, tenant_id, user_id)
                .await?
                .into_iter()
                .map(|target| target.node_id)
                .collect()
        }
    };
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout = '10s'")
        .execute(&mut *tx)
        .await?;
    let rows = match scope {
        UserTrafficHistoryScope::Node(node_id) => {
            sqlx::query(
                "WITH detail_totals AS MATERIALIZED (
                     SELECT node_id,tenant_id,window_end,sum(uplink_bytes) AS up,sum(downlink_bytes) AS down
                     FROM usage_samples WHERE node_id=$1
                       AND window_end>to_timestamp($2::double precision)
                       AND window_end<=to_timestamp($3::double precision)
                     GROUP BY node_id,tenant_id,window_end
                 )
                 SELECT node_id, CASE WHEN $6 THEN '' ELSE tenant_id END AS tenant,
                        CASE WHEN $6 THEN '' ELSE user_id END AS account,
                        extract(epoch FROM window_start)::bigint AS window_start,
                        extract(epoch FROM window_end)::bigint AS window_end,
                        sum(uplink_bytes)::bigint AS uplink_bytes,
                        sum(downlink_bytes)::bigint AS downlink_bytes
                   FROM (
                       SELECT node_id,tenant_id,user_id,window_start,window_end,uplink_bytes,downlink_bytes
                         FROM usage_samples WHERE NOT $6
                       UNION ALL
                       SELECT s.node_id,s.tenant_id,s.user_id,s.window_start,s.window_end,s.uplink_bytes,s.downlink_bytes
                         FROM usage_samples s
                         JOIN usage_node_windows w ON w.node_id=s.node_id AND w.tenant_id=s.tenant_id AND w.window_end=s.window_end
                         JOIN detail_totals t ON t.node_id=w.node_id AND t.tenant_id=w.tenant_id AND t.window_end=w.window_end
                        WHERE $6 AND t.up=w.user_uplink_bytes AND t.down=w.user_downlink_bytes
                       UNION ALL
                       SELECT w.node_id,w.tenant_id,'' AS user_id,
                              to_timestamp(r.window_start::double precision) AS window_start,
                              w.window_end,w.user_uplink_bytes,w.user_downlink_bytes
                         FROM usage_node_windows w
                         LEFT JOIN (
                             SELECT node_id,read_at,min((result->>'window_start_unix_secs')::bigint) AS window_start
                             FROM usage_report_receipts WHERE node_id=$1
                               AND read_at>to_timestamp($2::double precision)
                               AND read_at<=to_timestamp($3::double precision)
                             GROUP BY node_id,read_at
                         ) r ON r.node_id=w.node_id AND r.read_at=w.window_end
                         LEFT JOIN detail_totals t ON t.node_id=w.node_id AND t.tenant_id=w.tenant_id AND t.window_end=w.window_end
                        WHERE $6 AND (t.up IS DISTINCT FROM w.user_uplink_bytes OR t.down IS DISTINCT FROM w.user_downlink_bytes)
                   ) measured
                  WHERE node_id = $1
                    AND window_end > to_timestamp($2::double precision)
                    AND window_end <= to_timestamp($3::double precision)
                    AND ($4::text IS NULL OR tenant_id = $4 OR tenant_id LIKE $5 ESCAPE '\\')
                  GROUP BY node_id, tenant, account, window_start, window_end
                  ORDER BY node_id, tenant, account, window_end, window_start",
            )
            .bind(node_id)
            .bind(range_start as f64)
            .bind(range_end as f64)
            .bind(actor.tenant_scope())
            .bind(actor.tenant_scope_like_pattern())
            .bind(aggregate_node)
            .fetch_all(&mut *tx)
            .await?
        }
        UserTrafficHistoryScope::User { tenant_id, user_id } => {
            sqlx::query(
                "SELECT node_id, tenant_id, user_id,
                        extract(epoch FROM window_start)::bigint AS window_start,
                        extract(epoch FROM window_end)::bigint AS window_end,
                        sum(uplink_bytes)::bigint AS uplink_bytes,
                        sum(downlink_bytes)::bigint AS downlink_bytes
                   FROM usage_samples
                  WHERE tenant_id = $1
                    AND user_id = $2
                    AND window_end > to_timestamp($3::double precision)
                    AND window_end <= to_timestamp($4::double precision)
                  GROUP BY node_id, tenant_id, user_id, window_start, window_end
                  ORDER BY node_id, tenant_id, user_id, window_end, window_start",
            )
            .bind(tenant_id)
            .bind(user_id)
            .bind(range_start as f64)
            .bind(range_end as f64)
            .fetch_all(&mut *tx)
            .await?
        }
    };

    let mut series: Vec<UserTrafficHistorySeries> = Vec::new();
    for row in rows {
        let node_id: String = row.try_get("node_id")?;
        let tenant_id: String =
            row.try_get(if matches!(scope, UserTrafficHistoryScope::Node(_)) {
                "tenant"
            } else {
                "tenant_id"
            })?;
        let user_id: String =
            row.try_get(if matches!(scope, UserTrafficHistoryScope::Node(_)) {
                "account"
            } else {
                "user_id"
            })?;
        let point = (
            row.try_get::<i64, _>("window_end")?,
            i64_to_u64("uplink_bytes", row.try_get("uplink_bytes")?)?,
            i64_to_u64("downlink_bytes", row.try_get("downlink_bytes")?)?,
        );
        let start: Option<i64> = row.try_get("window_start")?;
        // Machine windows outlive user detail independently. Without either receipt or sample
        // bounds these bytes have a ledger timestamp, but no measured duration for a rate.
        let unplaced = start.is_none();
        let window = start.map(|start| (start, point.0, point.1, point.2));
        match series.last_mut() {
            Some(last)
                if last.node_id == node_id
                    && last.tenant_id == tenant_id
                    && last.user_id == user_id =>
            {
                if let Some(previous) = last
                    .points
                    .last_mut()
                    .filter(|previous| previous.0 == point.0)
                {
                    previous.1 = previous.1.checked_add(point.1).ok_or_else(|| {
                        StoreError::InvalidData("history uplink overflow".to_owned())
                    })?;
                    previous.2 = previous.2.checked_add(point.2).ok_or_else(|| {
                        StoreError::InvalidData("history downlink overflow".to_owned())
                    })?;
                } else {
                    last.points.push(point);
                }
                if let Some(window) = window {
                    last.windows.push(window);
                } else {
                    last.unplaced_uplink_bytes = last
                        .unplaced_uplink_bytes
                        .checked_add(point.1)
                        .ok_or_else(|| {
                            StoreError::InvalidData("history uplink overflow".to_owned())
                        })?;
                    last.unplaced_downlink_bytes = last
                        .unplaced_downlink_bytes
                        .checked_add(point.2)
                        .ok_or_else(|| {
                            StoreError::InvalidData("history downlink overflow".to_owned())
                        })?;
                }
            }
            _ => series.push(UserTrafficHistorySeries {
                node_id,
                tenant_id,
                user_id,
                points: vec![point],
                windows: window.into_iter().collect(),
                forwarding: false,
                unplaced_uplink_bytes: if unplaced { point.1 } else { 0 },
                unplaced_downlink_bytes: if unplaced { point.2 } else { 0 },
            }),
        }
    }

    // Preserve source boundaries and actual durations. Legacy bytes have no measured interval.
    let (node_filter, tenant_filter, user_filter) = match scope {
        UserTrafficHistoryScope::Node(node) => (Some(node), None, None),
        UserTrafficHistoryScope::User { tenant_id, user_id } => {
            (None, Some(tenant_id), Some(user_id))
        }
    };
    let forwarding_rows = sqlx::query(
        "SELECT node_id,CASE WHEN $8 THEN '' ELSE tenant_id END AS tenant,
                CASE WHEN $8 THEN '' ELSE user_id END AS account,extract(epoch FROM window_start)::bigint AS start,
                extract(epoch FROM read_at)::bigint AS end,sum(uplink_bytes)::bigint AS up,
                sum(downlink_bytes)::bigint AS down,bool_or(has_gap) AS has_gap
         FROM (
           SELECT node_id,tenant_id,user_id,window_start,read_at,uplink_bytes,downlink_bytes,has_gap
           FROM forwarding_usage_samples WHERE NOT $8
           UNION ALL
           SELECT node_id,tenant_id,'' AS user_id,window_start,window_end AS read_at,uplink_bytes,downlink_bytes,has_gap
           FROM forwarding_usage_windows WHERE $8
         ) measured
         WHERE read_at>to_timestamp($1) AND read_at<=to_timestamp($2)
           AND ($3::text IS NULL OR node_id=$3)
           AND ($4::text IS NULL OR tenant_id=$4 AND user_id=$5)
           AND ($6::text IS NULL OR tenant_id=$6 OR tenant_id LIKE $7 ESCAPE '\\')
         GROUP BY node_id,tenant,account,window_start,read_at ORDER BY node_id,tenant,account,read_at,window_start",
    ).bind(range_start as f64).bind(range_end as f64).bind(node_filter).bind(tenant_filter).bind(user_filter)
     .bind(actor.tenant_scope()).bind(actor.tenant_scope_like_pattern()).bind(aggregate_node).fetch_all(&mut *tx).await?;
    let mut forwarded = BTreeMap::<(String, String, String), UserTrafficHistorySeries>::new();
    for row in forwarding_rows {
        let node_id: String = row.try_get("node_id")?;
        let tenant_id: String = row.try_get("tenant")?;
        let user_id: String = row.try_get("account")?;
        let entry = forwarded
            .entry((node_id.clone(), tenant_id.clone(), user_id.clone()))
            .or_insert_with(|| UserTrafficHistorySeries {
                node_id,
                tenant_id,
                user_id,
                points: Vec::new(),
                windows: Vec::new(),
                forwarding: true,
                unplaced_uplink_bytes: 0,
                unplaced_downlink_bytes: 0,
            });
        let up = i64_to_u64("forwarding up", row.try_get("up")?)?;
        let down = i64_to_u64("forwarding down", row.try_get("down")?)?;
        if let Some(start) = row
            .try_get::<Option<i64>, _>("start")?
            .filter(|_| !row.get::<bool, _>("has_gap"))
        {
            let end = row.try_get("end")?;
            entry.points.push((end, up, down));
            entry.windows.push((start, end, up, down));
        } else {
            entry.unplaced_uplink_bytes = entry
                .unplaced_uplink_bytes
                .checked_add(up)
                .ok_or_else(|| StoreError::InvalidData("forwarding history overflow".to_owned()))?;
            entry.unplaced_downlink_bytes = entry
                .unplaced_downlink_bytes
                .checked_add(down)
                .ok_or_else(|| StoreError::InvalidData("forwarding history overflow".to_owned()))?;
        }
    }
    series.extend(forwarded.into_values());

    // A machine appears once its windows are wanted: the requested machine, every machine holding
    // the user's grants now, and any machine that still has accounted bytes inside the range.
    let mut node_ids = BTreeSet::new();
    if let UserTrafficHistoryScope::Node(node_id) = scope {
        node_ids.insert(node_id.to_owned());
    }
    node_ids.extend(granted_nodes);
    node_ids.extend(series.iter().map(|entry| entry.node_id.clone()));
    let node_ids = node_ids.into_iter().collect::<Vec<_>>();
    let window_rows = sqlx::query(
        "SELECT node_id, extract(epoch FROM window_end)::bigint AS window_end
           FROM usage_node_windows
          WHERE node_id = ANY($1)
            AND window_end > to_timestamp($2::double precision)
            AND window_end <= to_timestamp($3::double precision)
          GROUP BY node_id, window_end
          ORDER BY node_id, window_end",
    )
    .bind(&node_ids)
    .bind(range_start as f64)
    .bind(range_end as f64)
    .fetch_all(&mut *tx)
    .await?;
    let mut windows = node_ids
        .into_iter()
        .map(|node_id| (node_id, Vec::new()))
        .collect::<BTreeMap<String, Vec<i64>>>();
    for row in window_rows {
        let node_id: String = row.try_get("node_id")?;
        windows
            .entry(node_id)
            .or_default()
            .push(row.try_get("window_end")?);
    }

    let receipt_rows = sqlx::query(
        "SELECT node_id, extract(epoch FROM read_at)::bigint AS read_at, result
           FROM usage_report_receipts
          WHERE node_id = ANY($1)
            AND read_at > to_timestamp($2::double precision)
            AND read_at <= to_timestamp($3::double precision)
          ORDER BY node_id, read_at",
    )
    .bind(windows.keys().collect::<Vec<_>>())
    .bind(range_start as f64)
    .bind(range_end as f64)
    .fetch_all(&mut *tx)
    .await?;
    let mut coverage = BTreeMap::<String, Vec<(i64, i64)>>::new();
    let mut zeros = BTreeMap::<String, Vec<(i64, i64)>>::new();
    let ledger_ends: BTreeMap<_, BTreeSet<_>> = windows
        .iter()
        .map(|(node, ends)| (node.clone(), ends.iter().copied().collect()))
        .collect();
    for row in receipt_rows {
        let node_id: String = row.try_get("node_id")?;
        let end: i64 = row.try_get("read_at")?;
        // Receipts expire independently. A surviving receipt cannot certify that a missing
        // machine chart window had zero bytes.
        if aggregate_node
            && !ledger_ends
                .get(&node_id)
                .is_some_and(|ends| ends.contains(&end))
        {
            continue;
        }
        let result: UsageReportResult = serde_json::from_value(row.try_get("result")?)?;
        if let Some(start) = result.window_start_unix_secs {
            coverage
                .entry(node_id.clone())
                .or_default()
                .push((start, end));
            windows.entry(node_id.clone()).or_default().push(end);
        }
        zeros.entry(node_id).or_default().extend(
            result
                .zero_windows
                .into_iter()
                .map(|window| (window.start_unix_millis, window.end_unix_millis)),
        );
    }
    // Legacy receipts lack coverage metadata. Real nonzero windows still have precise bounds;
    // legacy idle points retain their 30-second interpretation, never bridge an unknown outage.
    for entry in series.iter().filter(|entry| !entry.forwarding) {
        coverage
            .entry(entry.node_id.clone())
            .or_default()
            .extend(entry.windows.iter().map(|w| (w.0, w.1)));
    }
    let forwarding_coverage_rows = sqlx::query(
        "SELECT node_id,extract(epoch FROM window_start)::bigint AS start,extract(epoch FROM window_end)::bigint AS end
         FROM forwarding_usage_windows WHERE node_id=ANY($1) AND NOT has_gap AND window_end>to_timestamp($2) AND window_end<=to_timestamp($3)
         AND ($4::text IS NULL OR tenant_id=$4 OR tenant_id LIKE $5 ESCAPE '\\')
         GROUP BY node_id,window_start,window_end ORDER BY node_id,window_end,window_start",
    ).bind(windows.keys().collect::<Vec<_>>()).bind(range_start as f64).bind(range_end as f64)
     .bind(actor.tenant_scope()).bind(actor.tenant_scope_like_pattern()).fetch_all(&mut *tx).await?;
    let mut forwarding_coverage = BTreeMap::<String, Vec<(i64, i64)>>::new();
    for row in forwarding_coverage_rows {
        forwarding_coverage
            .entry(row.try_get("node_id")?)
            .or_default()
            .push((row.try_get("start")?, row.try_get("end")?));
    }
    tx.commit().await?;
    Ok(UserTrafficHistory {
        range_start_unix_secs: range_start,
        range_end_unix_secs: range_end,
        nodes: windows
            .into_iter()
            .map(|(node_id, mut window_ends_unix_secs)| {
                window_ends_unix_secs.sort_unstable();
                window_ends_unix_secs.dedup();
                let mut node_windows = coverage.remove(&node_id).unwrap_or_default();
                let covered_ends: BTreeSet<_> =
                    node_windows.iter().map(|window| window.1).collect();
                for &end in &window_ends_unix_secs {
                    if !aggregate_node && !covered_ends.contains(&end) {
                        node_windows.push((end - 30, end));
                    }
                }
                node_windows.sort_unstable();
                node_windows.dedup();
                let mut zero_windows = zeros.remove(&node_id).unwrap_or_default();
                zero_windows.sort_unstable();
                zero_windows.dedup();
                UserTrafficHistoryNode {
                    forwarding_windows: forwarding_coverage.remove(&node_id).unwrap_or_default(),
                    node_id,
                    window_ends_unix_secs,
                    windows: node_windows,
                    zero_windows,
                }
            })
            .collect(),
        series,
    })
}

/// How far the agent's reported instant may diverge from the control plane's clock and still
/// count. Beyond it the whole round is refused — the window boundary decides which month these
/// bytes land in, and month boundaries are where bills divide.
const MAX_CLOCK_SKEW_SECS: i64 = 600;

/// Process-local observations for counters which are absent from the reporting generation.
///
/// Xray keeps removed user counters in process memory, so merely seeing an unknown label is not
/// actionable. What matters is whether its absolute byte count keeps moving inside the same exact
/// Xray epoch. This state is deliberately not durable: after a control-plane restart the first
/// report establishes a fresh baseline and the second can prove growth again.
#[derive(Debug, Default)]
pub(crate) struct UsageRuntimeState {
    unknown: Mutex<BTreeMap<String, UnknownNodeCounters>>,
}

#[derive(Debug, Default)]
struct UnknownNodeCounters {
    read_at_unix_secs: i64,
    heads: BTreeMap<[u8; 32], UnknownCounterHead>,
    growing: u64,
}

#[derive(Debug)]
struct UnknownCounterHead {
    xray_epoch: String,
    uplink_bytes: u64,
    downlink_bytes: u64,
}

#[derive(Debug)]
struct UnknownCounterObservation {
    fingerprint: [u8; 32],
    xray_epoch: String,
    uplink_bytes: u64,
    downlink_bytes: u64,
}

impl UsageRuntimeState {
    fn observe_unknown_counters(
        &self,
        node_id: &str,
        read_at_unix_secs: i64,
        observations: Vec<UnknownCounterObservation>,
    ) {
        let mut nodes = self
            .unknown
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = nodes.get(node_id);

        // A delayed spool report may commit after a newer observation. It remains valid billing
        // input, but must not rewind this explicitly latest-round operational indicator.
        if previous.is_some_and(|state| read_at_unix_secs <= state.read_at_unix_secs) {
            return;
        }

        let mut heads = BTreeMap::new();
        let mut growing = 0_u64;
        for observation in observations {
            if previous
                .and_then(|state| state.heads.get(&observation.fingerprint))
                .is_some_and(|head| {
                    head.xray_epoch == observation.xray_epoch
                        && observation.uplink_bytes >= head.uplink_bytes
                        && observation.downlink_bytes >= head.downlink_bytes
                        && (observation.uplink_bytes > head.uplink_bytes
                            || observation.downlink_bytes > head.downlink_bytes)
                })
            {
                growing += 1;
            }
            heads.insert(
                observation.fingerprint,
                UnknownCounterHead {
                    xray_epoch: observation.xray_epoch,
                    uplink_bytes: observation.uplink_bytes,
                    downlink_bytes: observation.downlink_bytes,
                },
            );
        }
        nodes.insert(
            node_id.to_owned(),
            UnknownNodeCounters {
                read_at_unix_secs,
                heads,
                growing,
            },
        );
    }

    pub(crate) fn growing_unknown_counters(&self, node_id: &str) -> Option<u64> {
        self.unknown
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(node_id)
            .map(|state| state.growing)
    }
}

fn unknown_counter_fingerprint(node_id: &str, label: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(node_id.as_bytes());
    digest.update([0]);
    digest.update(label.as_bytes());
    digest.finalize().into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum FirstReadingPolicy {
    /// The control plane did not witness this counter being created. Its first absolute value may
    /// contain historical traffic, so it can only establish the durable head.
    EstablishBaseline,
    /// This label became billable after a previously known generation did not own it. Xray's
    /// counter therefore starts at zero for this authorization, and the first absolute value is
    /// real traffic rather than an unknown historical balance.
    CountFromZero,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum FrozenBinding {
    Foreign,
    User {
        tenant_id: String,
        user_id: String,
        ingress_id: String,
        app_id: String,
        #[serde(default)]
        chain_id: Option<String>,
        /// Whether the first observed absolute counter is a baseline or billable from zero.
        first_reading: FirstReadingPolicy,
    },
    ChainHop {
        tenant_id: String,
        app_id: String,
        chain_id: String,
        node_id: String,
    },
}

impl FrozenBinding {
    fn to_counter_owner(&self, label: &str) -> Option<CounterOwner> {
        match self {
            Self::Foreign => None,
            Self::User {
                tenant_id,
                user_id,
                ingress_id,
                app_id,
                ..
            } => Some(CounterOwner::User(GrantMapping {
                tenant_id: tenant_id.clone(),
                user_id: user_id.clone(),
                ingress_id: ingress_id.clone(),
                app_id: app_id.clone(),
                label: label.to_owned(),
            })),
            Self::ChainHop {
                tenant_id,
                app_id,
                chain_id,
                node_id,
            } => Some(CounterOwner::ChainHop(ChainHopMapping {
                tenant_id: tenant_id.clone(),
                app_id: app_id.clone(),
                chain_id: chain_id.clone(),
                node_id: node_id.clone(),
                label: label.to_owned(),
            })),
        }
    }

    fn first_reading_policy(&self) -> FirstReadingPolicy {
        match self {
            Self::User { first_reading, .. } => *first_reading,
            Self::Foreign | Self::ChainHop { .. } => FirstReadingPolicy::EstablishBaseline,
        }
    }

    fn same_user_owner(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (
                Self::User {
                    tenant_id,
                    user_id,
                    ingress_id,
                    app_id,
                    ..
                },
                Self::User {
                    tenant_id: other_tenant,
                    user_id: other_user,
                    ingress_id: other_ingress,
                    app_id: other_app,
                    ..
                }
            ) if tenant_id == other_tenant
                && user_id == other_user
                && ingress_id == other_ingress
                && app_id == other_app
        )
    }

    fn set_first_reading_policy(&mut self, policy: FirstReadingPolicy) {
        if let Self::User { first_reading, .. } = self {
            *first_reading = policy;
        }
    }
}

#[derive(Debug)]
struct UsageGeneration {
    id: i64,
    deployment_id: Option<i64>,
    revision_id: Option<i64>,
    bindings: BTreeMap<String, FrozenBinding>,
    activated_at_unix_secs: Option<i64>,
}

fn report_identity(request: &UsageReportRequest) -> Result<(String, u64)> {
    let instance = &request.agent_instance_id;
    if request.sequence == 0
        || instance.len() != 32
        || !instance
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StoreError::InvalidData(
            "usage agent_instance_id must be 32 lowercase hex bytes and sequence must be positive"
                .to_owned(),
        ));
    }
    Ok((instance.clone(), request.sequence))
}

/// Freeze the namespace for a target before it can be dispatched. This function reads only the
/// supplied revision snapshot and the target's already-frozen grant list; it never consults live
/// ownership while replaying usage.
pub(crate) async fn create_usage_generation_for_target(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
    node_id: &str,
    snapshot: &ModelSnapshot,
    desired: &NodeDesiredState,
    actions: &[PlannedAction],
) -> Result<Option<i64>> {
    if !actions.iter().any(|action| {
        matches!(
            action,
            PlannedAction::ApplyXray | PlannedAction::DisableXray | PlannedAction::SyncGrants
        )
    }) {
        return Ok(None);
    }
    let disabled = matches!(desired.xray, DesiredArtifact::Disabled { .. });
    let mut bindings = if disabled {
        BTreeMap::new()
    } else {
        frozen_bindings(snapshot, node_id, Some(&desired.grants))
    };
    let previous = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT g.bindings
         FROM node_agent_state s
         JOIN usage_generations g ON g.id = s.usage_generation_id
         WHERE s.node_id = $1",
    )
    .bind(node_id)
    .fetch_optional(&mut **tx)
    .await?
    .map(serde_json::from_value::<BTreeMap<String, FrozenBinding>>)
    .transpose()?;
    if let DesiredArtifact::Present { content, .. } = &desired.xray {
        let actual = xray_client_labels(content)?;
        for (label, binding) in &mut bindings {
            if parse_grant_label(label).is_none()
                && matches!(binding, FrozenBinding::ChainHop { .. })
                && !actual.contains(label)
            {
                *binding = FrozenBinding::Foreign;
            }
        }
    }
    let grants_only = actions.contains(&PlannedAction::SyncGrants)
        && !actions.iter().any(|action| {
            matches!(
                action,
                PlannedAction::ApplyXray | PlannedAction::DisableXray
            )
        });
    if grants_only {
        if let Some(previous) = &previous {
            // A grants update cannot move an existing source to an unshipped chain topology.
            for (label, binding) in &mut bindings {
                if let Some(prior) = previous.get(label) {
                    if binding.same_user_owner(prior) {
                        if let (
                            FrozenBinding::User { chain_id, .. },
                            FrozenBinding::User { chain_id: old, .. },
                        ) = (binding, prior)
                        {
                            *chain_id = old.clone();
                        }
                    }
                }
            }
            // SyncGrants changes only runtime users; the Xray topology stays exactly as it was in
            // the previous generation, even when a newer unshipped model revision already exists.
            bindings.retain(|label, _| parse_grant_label(label).is_some());
            for (label, owner) in previous {
                if parse_grant_label(label).is_none() {
                    bindings.insert(label.clone(), owner.clone());
                }
            }
            if let DesiredGrants::Present { inbounds } = &desired.grants {
                for email in inbounds
                    .iter()
                    .flat_map(|inbound| inbound.clients.iter().map(|client| &client.email))
                {
                    if !bindings.contains_key(email) {
                        if let Some(owner) = previous.get(email) {
                            bindings.insert(email.clone(), owner.clone());
                        }
                    }
                }
            }
        }
    }
    let headed_labels =
        sqlx::query_scalar::<_, String>("SELECT label FROM usage_counter_heads WHERE node_id = $1")
            .bind(node_id)
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .collect::<BTreeSet<_>>();
    assign_first_reading_policies(&mut bindings, previous.as_ref(), &headed_labels);
    let revision_id: i64 = sqlx::query_scalar("SELECT revision_id FROM deployments WHERE id = $1")
        .bind(deployment_id)
        .fetch_one(&mut **tx)
        .await?;
    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO usage_generations (node_id, deployment_id, revision_id, bindings)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (deployment_id, node_id) DO UPDATE SET
             revision_id = EXCLUDED.revision_id,
             bindings = EXCLUDED.bindings
         WHERE NOT EXISTS (
             SELECT 1
               FROM usage_generation_activations activation
              WHERE activation.generation_id = usage_generations.id
         )
         RETURNING id",
    )
    .bind(node_id)
    .bind(deployment_id)
    .bind(revision_id)
    .bind(serde_json::to_value(bindings)?)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| {
        StoreError::Conflict(format!(
            "usage generation for deployment {deployment_id}/{node_id} is already active"
        ))
    })?;
    Ok(Some(id))
}

/// Attach provenance to a newly frozen namespace.
///
/// A missing durable head alone is not enough to count the first absolute reading: the counter
/// may already have accumulated traffic before the control plane observed it. A prior generation
/// is the witness.
/// If that known namespace did not own this label, the new authorization starts from zero. The
/// marker is carried across unrelated generations until a head exists, so a quiet new user does
/// not lose their first window merely because another release landed before they connected.
fn assign_first_reading_policies(
    bindings: &mut BTreeMap<String, FrozenBinding>,
    previous: Option<&BTreeMap<String, FrozenBinding>>,
    headed_labels: &BTreeSet<String>,
) {
    for (label, binding) in bindings {
        if !matches!(binding, FrozenBinding::User { .. }) {
            continue;
        }
        let policy = if headed_labels.contains(label) {
            FirstReadingPolicy::EstablishBaseline
        } else if let Some(previous) = previous {
            match previous.get(label) {
                Some(old) if binding.same_user_owner(old) => old.first_reading_policy(),
                Some(_) | None => FirstReadingPolicy::CountFromZero,
            }
        } else {
            FirstReadingPolicy::EstablishBaseline
        };
        binding.set_first_reading_policy(policy);
    }
}

fn xray_client_labels(content: &str) -> Result<BTreeSet<String>> {
    let value: serde_json::Value = serde_json::from_str(content)?;
    let mut labels = BTreeSet::new();
    for inbound in value
        .get("inbounds")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        for client in inbound
            .get("settings")
            .and_then(|settings| settings.get("clients"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(email) = client.get("email").and_then(serde_json::Value::as_str) {
                labels.insert(email.to_owned());
            }
        }
    }
    Ok(labels)
}

pub(crate) async fn activate_usage_generation(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    generation_id: i64,
    deployment_id: i64,
    activated_at_unix_secs: Option<i64>,
) -> Result<()> {
    if activated_at_unix_secs.is_some_and(|value| value <= 0) {
        return Err(StoreError::InvalidData(
            "usage activation timestamp must be a positive unix second".to_owned(),
        ));
    }
    sqlx::query(
        "INSERT INTO usage_generation_activations
             (node_id, generation_id, deployment_id, activated_at)
         VALUES ($1, $2, $3, coalesce(to_timestamp($4::double precision), now()))
         ON CONFLICT (node_id, generation_id) DO NOTHING",
    )
    .bind(node_id)
    .bind(generation_id)
    .bind(deployment_id)
    .bind(activated_at_unix_secs)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO node_agent_state (node_id, usage_generation_id)
         VALUES ($1, $2)
         ON CONFLICT (node_id) DO UPDATE SET usage_generation_id = EXCLUDED.usage_generation_id",
    )
    .bind(node_id)
    .bind(generation_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn frozen_bindings(
    snapshot: &ModelSnapshot,
    reporter_node: &str,
    desired_grants: Option<&DesiredGrants>,
) -> BTreeMap<String, FrozenBinding> {
    let allowed_users: Option<BTreeSet<String>> = desired_grants.map(|grants| match grants {
        DesiredGrants::Present { inbounds } => inbounds
            .iter()
            .flat_map(|inbound| inbound.clients.iter().map(|client| client.email.clone()))
            .collect(),
        DesiredGrants::Disabled { .. } | DesiredGrants::Unmanaged { .. } => BTreeSet::new(),
    });
    let mut bindings = BTreeMap::new();
    for app in &snapshot.apps {
        for grant in &app.grants {
            let label = grant_label(&grant.user, &grant.tenant, &grant.ingress);
            if allowed_users
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&label))
            {
                continue;
            }
            let binding = if app
                .ingresses
                .iter()
                .any(|ingress| ingress.id == grant.ingress && ingress.node == reporter_node)
            {
                FrozenBinding::User {
                    tenant_id: grant.tenant.clone(),
                    user_id: grant.user.clone(),
                    ingress_id: grant.ingress.clone(),
                    app_id: app.id.clone(),
                    chain_id: app
                        .ingresses
                        .iter()
                        .find(|i| i.id == grant.ingress)
                        .map(|i| i.chain.clone()),
                    first_reading: FirstReadingPolicy::EstablishBaseline,
                }
            } else {
                FrozenBinding::Foreign
            };
            bindings.insert(label, binding);
        }
        for step in &app.steps {
            let Some(accept) = &step.accept else { continue };
            let carried_here = step.node == reporter_node
                || app.steps.iter().any(|portal| {
                    portal.node == reporter_node
                        && portal.chain == step.chain
                        && portal.rules.iter().any(|rule| {
                            matches!(
                                &rule.action,
                                Action::Forward { to, dial: HopDial::Reverse(_), .. }
                                    if to == &step.node
                            )
                        })
                });
            if let Some(chain) = app.chains.iter().find(|chain| chain.id == step.chain) {
                bindings.insert(
                    accept.label.clone(),
                    if carried_here {
                        FrozenBinding::ChainHop {
                            tenant_id: chain.tenant.clone(),
                            app_id: app.id.clone(),
                            chain_id: chain.id.clone(),
                            node_id: step.node.clone(),
                        }
                    } else {
                        FrozenBinding::Foreign
                    },
                );
            }
        }
    }
    bindings
}

pub(crate) async fn other_domain_owners(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    generation_id: i64,
) -> Result<BTreeMap<String, (String, String)>> {
    let generation = resolve_usage_generation(tx, node_id, generation_id).await?;
    Ok(generation
        .bindings
        .iter()
        .filter_map(|(label, binding)| match binding.to_counter_owner(label) {
            Some(CounterOwner::User(grant)) => {
                Some((label.clone(), (grant.tenant_id, grant.user_id)))
            }
            _ => None,
        })
        .collect())
}

pub(crate) async fn source_traffic_owners(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    generation_id: i64,
) -> Result<BTreeMap<String, crate::source_traffic::SourceOwner>> {
    let generation = resolve_usage_generation(tx, node_id, generation_id).await?;
    Ok(generation
        .bindings
        .into_iter()
        .filter_map(|(label, binding)| match binding {
            FrozenBinding::User {
                tenant_id,
                user_id,
                ingress_id,
                app_id,
                chain_id,
                ..
            } => Some((
                label,
                crate::source_traffic::SourceOwner {
                    tenant_id,
                    user_id,
                    ingress_id,
                    app_id,
                    chain_id: chain_id.unwrap_or_default(),
                },
            )),
            _ => None,
        })
        .collect())
}

async fn resolve_usage_generation(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    generation_id: i64,
) -> Result<UsageGeneration> {
    let row = sqlx::query(
        "SELECT id, deployment_id, revision_id, bindings
         FROM usage_generations WHERE id = $1 AND node_id = $2",
    )
    .bind(generation_id)
    .bind(node_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| {
        StoreError::InvalidData(format!(
            "usage generation {generation_id} does not belong to node {node_id}"
        ))
    })?;
    let id: i64 = row.try_get("id")?;
    Ok(UsageGeneration {
        id,
        deployment_id: row.try_get("deployment_id")?,
        revision_id: row.try_get("revision_id")?,
        bindings: serde_json::from_value(row.try_get("bindings")?)?,
        activated_at_unix_secs: sqlx::query_scalar(
            "SELECT CASE
                      WHEN activated_at > '-infinity'::timestamptz
                       AND activated_at < 'infinity'::timestamptz
                      THEN extract(epoch FROM activated_at)::bigint
                    END
             FROM usage_generation_activations
             WHERE node_id = $1 AND generation_id = $2",
        )
        .bind(node_id)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .flatten(),
    })
}

pub async fn record_usage_report(
    pool: &PgPool,
    runtime: &UsageRuntimeState,
    node_id: &str,
    mut request: UsageReportRequest,
) -> Result<UsageReportResult> {
    if request.read_at_unix_secs <= 0 || request.xray_started_at_unix_secs <= 0 {
        return Err(StoreError::InvalidData(
            "usage report timestamps must be positive unix seconds".to_owned(),
        ));
    }
    if request.read_at_unix_secs < request.xray_started_at_unix_secs {
        return Err(StoreError::InvalidData(
            "usage report read_at must not be before xray_started_at".to_owned(),
        ));
    }
    if request.counters.len() > 100_000 {
        return Err(StoreError::InvalidData(
            "usage report contains too many counters".to_owned(),
        ));
    }
    if request.zero_windows.len() > 64
        || request.zero_windows.iter().any(|window| {
            window.start_unix_millis <= 0
                || window.end_unix_millis <= window.start_unix_millis
                || window.end_unix_millis / 1000 > request.read_at_unix_secs
        })
    {
        return Err(StoreError::InvalidData(
            "invalid confirmed zero windows".to_owned(),
        ));
    }
    if request.xray_epoch.trim().is_empty() || request.xray_epoch.len() > 128 {
        return Err(StoreError::InvalidData(
            "usage xray_epoch must contain 1..128 bytes".to_owned(),
        ));
    }
    if request.usage_generation_id <= 0 {
        return Err(StoreError::InvalidData(
            "usage generation id must be positive".to_owned(),
        ));
    }

    // Delayed spool delivery is valid. Only a clock in the future is unsafe, because it can put
    // bytes in an accounting period which has not begun. Conflict is retryable by the agent;
    // once NTP repairs the clock, a newer report can move the queue without losing this one.
    let (server_now,): (i64,) = sqlx::query_as("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(pool)
        .await?;
    let future_secs = request.read_at_unix_secs - server_now;
    if future_secs > MAX_CLOCK_SKEW_SECS {
        return Err(StoreError::Conflict(format!(
            "usage report is {future_secs}s in the future; check the node's clock"
        )));
    }

    // Keep the receipt identity identical to pre-site-statistics Console builds.
    // Observations are accepted only with the first billing transaction; retries
    // never enrich them. Including this optional dimension in the identity would
    // block lost-ACK billing retries after rolling Console back to an older build.
    let site_usage = request.site_usage.take();
    let payload_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&request)?));
    // Optional observations must not block a billing receipt when an Agent has a newer
    // catalog or report version. Treat invalid observations as absent; the existing gap
    // path below clears their coverage head without changing billing or receipt identity.
    request.site_usage = site_usage.filter(|report| crate::site_usage::validate(report).is_ok());
    let (agent_instance_id, sequence) = report_identity(&request)?;
    let sequence_i64 = u64_to_i64("sequence", sequence)?;
    let xray_epoch = request.xray_epoch.clone();

    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO node_agent_state (node_id) VALUES ($1) ON CONFLICT (node_id) DO NOTHING",
    )
    .bind(node_id)
    .execute(&mut *tx)
    .await?;
    // Machine-level serialization makes lock order independent of counter order and turns the
    // whole report, its receipt and all counter heads into one atomic decision.
    sqlx::query("SELECT node_id FROM node_agent_state WHERE node_id = $1 FOR UPDATE")
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await?;

    if let Some(row) = sqlx::query(
        "SELECT payload_sha256, result FROM usage_report_receipts
         WHERE node_id = $1 AND agent_instance_id = $2 AND sequence = $3",
    )
    .bind(node_id)
    .bind(&agent_instance_id)
    .bind(sequence_i64)
    .fetch_optional(&mut *tx)
    .await?
    {
        let seen: String = row.try_get("payload_sha256")?;
        if seen != payload_sha256 {
            return Err(StoreError::InvalidData(format!(
                "usage report sequence {sequence} was reused with different content"
            )));
        }
        let mut result: UsageReportResult = serde_json::from_value(row.try_get("result")?)?;
        result.duplicate = true;
        tx.commit().await?;
        return Ok(result);
    }
    if let Some(last) = sqlx::query_scalar::<_, i64>(
        "SELECT last_sequence FROM usage_agent_cursors
         WHERE node_id = $1 AND agent_instance_id = $2",
    )
    .bind(node_id)
    .bind(&agent_instance_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        if sequence_i64 <= last {
            return Err(StoreError::InvalidData(format!(
                "usage report sequence {sequence} is behind committed sequence {last}"
            )));
        }
    }

    let generation =
        resolve_usage_generation(&mut tx, node_id, request.usage_generation_id).await?;
    let metadata = UsageMetadata {
        revision_id: generation.revision_id,
        deployment_id: generation.deployment_id,
    };
    let collect_sites = request.site_usage.is_some()
        && sqlx::query_scalar::<_, bool>(
            "SELECT site_usage_enabled FROM control_state WHERE id=TRUE",
        )
        .fetch_one(&mut *tx)
        .await?;
    if let Some(report) = request.site_usage.as_ref().filter(|_| collect_sites) {
        let owners = generation
            .bindings
            .iter()
            .filter_map(|(label, binding)| match binding.to_counter_owner(label) {
                Some(CounterOwner::User(grant)) => {
                    Some((label.clone(), (grant.tenant_id, grant.user_id)))
                }
                _ => None,
            })
            .collect();
        crate::site_usage::record(
            &mut tx,
            node_id,
            generation.id,
            request.read_at_unix_secs,
            report,
            &owners,
        )
        .await?;
    } else {
        // Missing observations (old Agent/Xray or an explicit collection failure)
        // break coverage, not billing. The next supported report establishes a
        // fresh site baseline instead of spreading unknown traffic across the gap.
        sqlx::query("DELETE FROM node_site_usage_state WHERE node_id=$1 AND read_at <= $2")
            .bind(node_id)
            .bind(request.read_at_unix_secs)
            .execute(&mut *tx)
            .await?;
    }
    let previous_read: Option<i64> = sqlx::query_scalar(
        "SELECT extract(epoch FROM read_at)::bigint FROM usage_report_receipts
         WHERE node_id = $1 AND agent_instance_id = $2 ORDER BY sequence DESC LIMIT 1",
    )
    .bind(node_id)
    .bind(&agent_instance_id)
    .fetch_optional(&mut *tx)
    .await?;
    // Even an empty QueryStats response is a successful local observation. Unlike an API
    // failure it proves no user counters exist; include that coverage without creating fake
    // user detail rows or resetting heads.
    let mut coverage_start = request.counters.is_empty().then(|| {
        previous_read
            .unwrap_or(request.xray_started_at_unix_secs)
            .max(request.xray_started_at_unix_secs)
    });
    let mut accepted_readings = 0_u64;
    let mut inserted_samples = 0_u64;
    let mut skipped_counters = 0_u64;
    let mut rejected_counters = 0_u64;
    let mut gap_samples = 0_u64;
    let mut unknown_counters = Vec::new();
    let mut node_windows = BTreeMap::<(String, String), UsageWindowTotals>::new();
    let route = request.route.clone();
    request.counters.sort_by(|a, b| a.label.cmp(&b.label));
    let mut labels = BTreeSet::new();
    for counter in request.counters {
        let label = counter.label.trim().to_owned();
        if label.is_empty() {
            skipped_counters += 1;
            continue;
        }
        let Ok(uplink_bytes) = u64_to_i64("uplink_bytes", counter.uplink_bytes) else {
            skipped_counters += 1;
            continue;
        };
        let Ok(downlink_bytes) = u64_to_i64("downlink_bytes", counter.downlink_bytes) else {
            skipped_counters += 1;
            continue;
        };
        if !labels.insert(label.clone()) {
            return Err(StoreError::InvalidData(format!(
                "usage report contains duplicate counter label {label}"
            )));
        }
        // The E2E probe deliberately authenticates through the real ingress and therefore owns
        // an Xray `user>>>` counter. It is not a user and is intentionally absent from frozen
        // billing generations; treating it as an unknown label makes every healthy probe look
        // like attribution drift. Keep it outside billing, skipped counts and the in-memory
        // unknown-growth detector alike.
        if is_probe_label(&label) {
            continue;
        }
        let Some(binding) = generation.bindings.get(&label) else {
            skipped_counters += 1;
            unknown_counters.push(UnknownCounterObservation {
                fingerprint: unknown_counter_fingerprint(node_id, &label),
                xray_epoch: xray_epoch.clone(),
                uplink_bytes: counter.uplink_bytes,
                downlink_bytes: counter.downlink_bytes,
            });
            continue;
        };
        let first_reading_policy = binding.first_reading_policy();
        let previous = usage_head_for_update(&mut tx, node_id, &label).await?;
        let Some(owner) = binding.to_counter_owner(&label) else {
            // Persistent Xray counters survive a topology change that removes their owner from
            // this machine. An unchanged value in the same namespace is already accounted for;
            // it neither advances the head nor indicates foreign traffic. Require the exact
            // durable baseline: new bytes, regressions and unproven namespaces still reject.
            if previous.as_ref().is_some_and(|head| {
                request.read_at_unix_secs >= head.read_at_unix_secs
                    && xray_epoch == head.xray_epoch
                    && uplink_bytes == head.uplink_bytes
                    && downlink_bytes == head.downlink_bytes
            }) {
                skipped_counters += 1;
            } else {
                rejected_counters += 1;
            }
            continue;
        };
        let canonical_label = match &owner {
            CounterOwner::User(grant) => &grant.label,
            CounterOwner::ChainHop(hop) => &hop.label,
        };

        let inserted = insert_usage_reading(
            &mut tx,
            UsageReadingInsert {
                node_id,
                label: canonical_label,
                agent_instance_id: &agent_instance_id,
                sequence: sequence_i64,
                generation_id: generation.id,
                xray_epoch: &xray_epoch,
                read_at_unix_secs: request.read_at_unix_secs,
                xray_started_at_unix_secs: request.xray_started_at_unix_secs,
                uplink_bytes,
                downlink_bytes,
            },
        )
        .await?;
        if !inserted {
            return Err(StoreError::InvalidData(format!(
                "duplicate usage reading for {canonical_label}"
            )));
        }
        let mut update_head = true;
        if let Some(previous) = &previous {
            if request.read_at_unix_secs < previous.read_at_unix_secs {
                rejected_counters += 1;
                update_head = false;
            } else if request.read_at_unix_secs == previous.read_at_unix_secs {
                // The agent timestamps reports in whole seconds. A release takes a protected
                // pre-change sample while the ordinary 30-second sampler runs independently;
                // the in-process meter orders both reads, but two consecutive reads can still
                // land in the same wall-clock second. There is no positive-duration window to
                // book, so leave the earlier head in place and let the next later report include
                // any same-second counter growth. A strictly older timestamp remains a rejection
                // above; treating equality as one made a healthy grants hot-swap surface as
                // "out of order" in the node health UI.
                update_head = false;
            } else if previous.xray_epoch.starts_with("journal:")
                && !xray_epoch.starts_with("journal:")
                && request.xray_started_at_unix_secs <= previous.read_at_unix_secs
            {
                // Older Agents can briefly lose journal-writer confirmation during handoff.
                // An already-running candidate is not a fresh legacy counter namespace. Keep
                // the durable journal baseline until identity recovers; its next cumulative
                // reading includes every byte collected in the meantime. A genuine legacy
                // restart after the previous reading still follows the normal reset path.
                rejected_counters += 1;
                update_head = false;
            } else if previous.xray_epoch == xray_epoch
                && (uplink_bytes < previous.uplink_bytes
                    || downlink_bytes < previous.downlink_bytes)
            {
                // A counter cannot decrease within one exact process epoch. Preserve the known
                // good head so a later reading can recover; treating this as a restart would bill
                // the entire absolute counter a second time.
                rejected_counters += 1;
                update_head = false;
            } else {
                let restarted = previous.xray_epoch != xray_epoch;
                let (window_start, uplink_delta, downlink_delta, has_gap) = if restarted {
                    (
                        previous
                            .read_at_unix_secs
                            .max(request.xray_started_at_unix_secs),
                        uplink_bytes,
                        downlink_bytes,
                        true,
                    )
                } else {
                    (
                        previous.read_at_unix_secs,
                        uplink_bytes - previous.uplink_bytes,
                        downlink_bytes - previous.downlink_bytes,
                        previous.generation_id != generation.id,
                    )
                };
                if window_start < request.read_at_unix_secs {
                    coverage_start =
                        Some(coverage_start.map_or(window_start, |start| start.min(window_start)));
                    let insert = UsageSampleInsert {
                        node_id,
                        owner: &owner,
                        window_start_unix_secs: window_start,
                        window_end_unix_secs: request.read_at_unix_secs,
                        uplink_bytes: uplink_delta,
                        downlink_bytes: downlink_delta,
                        has_gap,
                        revision_id: metadata.revision_id,
                        deployment_id: metadata.deployment_id,
                        generation_id: generation.id,
                    };
                    let sample_inserted = match &owner {
                        CounterOwner::User(_) => insert_usage_sample(&mut tx, insert).await?,
                        CounterOwner::ChainHop(_) => insert_chain_sample(&mut tx, insert).await?,
                    };
                    if sample_inserted {
                        add_node_window(
                            &mut node_windows,
                            node_id,
                            &owner,
                            uplink_delta,
                            downlink_delta,
                        )?;
                        inserted_samples += 1;
                        if has_gap {
                            gap_samples += 1;
                        }
                    }
                }
            }
        } else if first_reading_policy == FirstReadingPolicy::CountFromZero {
            // This is not the generic "first time the control plane saw a counter" case. The
            // frozen generation records that a known preceding namespace did not authorize this
            // label, so its first absolute value is the complete usage since authorization.
            let (candidate_start, has_gap) = match generation.activated_at_unix_secs {
                Some(activated_at) if activated_at <= request.read_at_unix_secs => {
                    (activated_at.max(request.xray_started_at_unix_secs), false)
                }
                // The reading arrived before the convergence observation. Keep every byte, use a
                // minimal non-empty window, and expose the imprecise boundary through has_gap.
                Some(_) | None => (request.read_at_unix_secs.saturating_sub(1), true),
            };
            // Unix seconds have one-second resolution. A user can transfer bytes in the same
            // second as authorization, while the schema correctly requires end > start.
            let window_start = candidate_start.min(request.read_at_unix_secs.saturating_sub(1));
            if window_start < request.read_at_unix_secs {
                coverage_start =
                    Some(coverage_start.map_or(window_start, |start| start.min(window_start)));
                let insert = UsageSampleInsert {
                    node_id,
                    owner: &owner,
                    window_start_unix_secs: window_start,
                    window_end_unix_secs: request.read_at_unix_secs,
                    uplink_bytes,
                    downlink_bytes,
                    has_gap,
                    revision_id: metadata.revision_id,
                    deployment_id: metadata.deployment_id,
                    generation_id: generation.id,
                };
                let sample_inserted = match &owner {
                    CounterOwner::User(_) => insert_usage_sample(&mut tx, insert).await?,
                    CounterOwner::ChainHop(_) => insert_chain_sample(&mut tx, insert).await?,
                };
                if sample_inserted {
                    add_node_window(
                        &mut node_windows,
                        node_id,
                        &owner,
                        uplink_bytes,
                        downlink_bytes,
                    )?;
                    inserted_samples += 1;
                    if has_gap {
                        gap_samples += 1;
                    }
                }
            }
        }
        if update_head {
            accepted_readings += 1;
            upsert_usage_head(
                &mut tx,
                node_id,
                canonical_label,
                &xray_epoch,
                request.read_at_unix_secs,
                request.xray_started_at_unix_secs,
                uplink_bytes,
                downlink_bytes,
                generation.id,
                &agent_instance_id,
                sequence_i64,
            )
            .await?;
        }
    }

    persist_node_windows(&mut tx, request.read_at_unix_secs, node_windows).await?;
    let result = UsageReportResult {
        node_id: node_id.to_owned(),
        agent_instance_id: request.agent_instance_id.clone(),
        sequence: request.sequence,
        usage_generation_id: generation.id,
        duplicate: false,
        accepted_readings,
        inserted_samples,
        skipped_counters,
        rejected_counters,
        gap_samples,
        window_start_unix_secs: coverage_start.filter(|start| *start < request.read_at_unix_secs),
        zero_windows: request.zero_windows,
    };
    let result_json = serde_json::to_value(&result)?;
    sqlx::query(
        "INSERT INTO usage_report_receipts
             (node_id, agent_instance_id, sequence, payload_sha256, generation_id, read_at, result)
         VALUES ($1, $2, $3, $4, $5, to_timestamp($6::double precision), $7)",
    )
    .bind(node_id)
    .bind(&agent_instance_id)
    .bind(sequence_i64)
    .bind(&payload_sha256)
    .bind(generation.id)
    .bind(request.read_at_unix_secs)
    .bind(&result_json)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO usage_agent_cursors (node_id, agent_instance_id, last_sequence)
         VALUES ($1, $2, $3)
         ON CONFLICT (node_id, agent_instance_id) DO UPDATE SET
            last_sequence = EXCLUDED.last_sequence, updated_at = now()",
    )
    .bind(node_id)
    .bind(&agent_instance_id)
    .bind(sequence_i64)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO node_agent_state (
            node_id, last_usage_report_at, xray_started_at, route_ipv4, route_ipv6,
            usage_generation_id, usage_last_result
         )
         VALUES (
            $1, now(), to_timestamp($2::double precision),
            CASE WHEN $3::boolean THEN $4 ELSE NULL END,
            CASE WHEN $3::boolean THEN $5 ELSE NULL END,
            $6, $7
         )
         ON CONFLICT (node_id) DO UPDATE SET
            last_usage_report_at = EXCLUDED.last_usage_report_at,
            xray_started_at = EXCLUDED.xray_started_at,
            route_ipv4 = CASE WHEN $3::boolean THEN EXCLUDED.route_ipv4 ELSE node_agent_state.route_ipv4 END,
            route_ipv6 = CASE WHEN $3::boolean THEN EXCLUDED.route_ipv6 ELSE node_agent_state.route_ipv6 END,
            usage_generation_id = EXCLUDED.usage_generation_id,
            usage_last_result = EXCLUDED.usage_last_result",
    )
    .bind(node_id)
    .bind(request.xray_started_at_unix_secs)
    .bind(route.is_some())
    .bind(route.as_ref().and_then(|route| route.ipv4.as_deref()))
    .bind(route.as_ref().and_then(|route| route.ipv6.as_deref()))
    .bind(generation.id)
    .bind(result_json)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    runtime.observe_unknown_counters(node_id, request.read_at_unix_secs, unknown_counters);

    Ok(result)
}

/// What the bar chart at the right of the machine list and its "this month" reading need: a
/// per-machine aggregated usage series plus the month's running total.
///
/// Why `/usage/samples` is not reused: that returns detail rows (one row = one window × one
/// grant), so sixteen machines over ten minutes is `16 × 20 × users × ingresses` rows against a
/// limit capped at 500. Aggregating here yields everything the page needs in one query.
///
/// The bounded machine-window projection includes both user and relay traffic. It keeps real
/// zero windows without requiring a zero-valued detail row for every inactive user.
///
/// `window_secs` sets how long the series is (not the bucket width — the buckets are the agent's
/// reporting windows themselves).
pub async fn list_usage_node_series(
    pool: &PgPool,
    actor: &AdminContext,
    window_secs: u32,
    node_id: Option<&str>,
) -> Result<UsageNodeSeriesList> {
    list_usage_node_series_selected(
        pool,
        actor,
        UsageSeriesSelection::Recent(window_secs),
        node_id,
    )
    .await
}

pub async fn list_usage_node_series_range(
    pool: &PgPool,
    actor: &AdminContext,
    start_unix_secs: i64,
    end_unix_secs: i64,
    node_id: Option<&str>,
) -> Result<UsageNodeSeriesList> {
    let span = end_unix_secs.checked_sub(start_unix_secs).ok_or_else(|| {
        StoreError::InvalidData("invalid usage series timestamp range".to_owned())
    })?;
    if start_unix_secs < 0 || !(1..=86_400).contains(&span) {
        return Err(StoreError::InvalidData(
            "usage series range must be positive and no longer than 24 hours".to_owned(),
        ));
    }
    list_usage_node_series_selected(
        pool,
        actor,
        UsageSeriesSelection::Absolute {
            start_unix_secs,
            end_unix_secs,
        },
        node_id,
    )
    .await
}

#[derive(Clone, Copy)]
enum UsageSeriesSelection {
    Recent(u32),
    Absolute {
        start_unix_secs: i64,
        end_unix_secs: i64,
    },
}

async fn list_usage_node_series_selected(
    pool: &PgPool,
    actor: &AdminContext,
    selection: UsageSeriesSelection,
    node_id: Option<&str>,
) -> Result<UsageNodeSeriesList> {
    let tenant_scope = actor.tenant_scope();
    let tenant_pattern = actor.tenant_scope_like_pattern();

    // Month boundaries follow list_monthly_usage_summary: calendar months at +08, decided
    // server-side and never sent by the UI. wall_start is the human-facing wall-clock string,
    // inst_start carries the offset and is used only for filtering.
    let (wall_month_start, inst_month_start, inst_month_end): (String, String, String) =
        sqlx::query_as(
            "SELECT to_char(date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong'),
                        'YYYY-MM-DD HH24:MI:SS'),
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                 AT TIME ZONE 'Asia/Hong_Kong')::text,
                ((date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                  + INTERVAL '1 month') AT TIME ZONE 'Asia/Hong_Kong')::text",
        )
        .fetch_one(pool)
        .await?;

    let (since, until): (String, String) = match selection {
        UsageSeriesSelection::Recent(window_secs) => {
            // Capped at 24 hours: anything longer belongs in a rollup table rather than scanning
            // detail rows window by window.
            let window_secs = i64::from(window_secs.clamp(60, 86_400));
            sqlx::query_as(
                "SELECT (now() - make_interval(secs => $1::double precision))::text, now()::text",
            )
            .bind(window_secs as f64)
            .fetch_one(pool)
            .await?
        }
        UsageSeriesSelection::Absolute {
            start_unix_secs,
            end_unix_secs,
        } => {
            sqlx::query_as("SELECT to_timestamp($1)::text, to_timestamp($2)::text")
                .bind(start_unix_secs as f64)
                .bind(end_unix_secs as f64)
                .fetch_one(pool)
                .await?
        }
    };

    // Tenant rows are summed only after applying the actor's scope; time boundaries are the
    // actual Agent windows, with no resampling or inferred zero points.
    let bucket_rows = sqlx::query(
        "SELECT node_id,
                window_end::text AS window_end,
                sum(user_uplink_bytes)::bigint AS user_uplink_bytes,
                sum(user_downlink_bytes)::bigint AS user_downlink_bytes,
                sum(relay_uplink_bytes)::bigint AS relay_uplink_bytes,
                sum(relay_downlink_bytes)::bigint AS relay_downlink_bytes
         FROM usage_node_windows
         WHERE window_end >= $1::timestamptz
           AND window_end <= $2::timestamptz
           AND ($3::text IS NULL OR tenant_id = $3 OR tenant_id LIKE $4 ESCAPE '\\')
           AND ($5::text IS NULL OR node_id = $5)
         GROUP BY node_id, window_end
         ORDER BY node_id, window_end",
    )
    .bind(&since)
    .bind(&until)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .bind(node_id)
    .fetch_all(pool)
    .await?;

    // Month totals have day-level resolution and therefore come from the durable rollup written
    // in the same transaction as each detail sample. Scanning both 30-second detail tables here
    // made the fleet endpoint grow linearly with every report ever received.
    let month_rows = sqlx::query(
        "SELECT node_id,
                coalesce(sum(user_uplink_bytes), 0)::bigint AS user_uplink_bytes,
                coalesce(sum(user_downlink_bytes), 0)::bigint AS user_downlink_bytes,
                coalesce(sum(relay_uplink_bytes), 0)::bigint AS relay_uplink_bytes,
                coalesce(sum(relay_downlink_bytes), 0)::bigint AS relay_downlink_bytes,
                bool_or(has_gap) AS has_gap
         FROM usage_node_rollups
         WHERE period_start >= $1::timestamptz
           AND period_start < $2::timestamptz
           AND ($3::text IS NULL OR tenant_id = $3 OR tenant_id LIKE $4 ESCAPE '\\')
           AND ($5::text IS NULL OR node_id = $5)
         GROUP BY node_id",
    )
    .bind(&inst_month_start)
    .bind(&inst_month_end)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .bind(node_id)
    .fetch_all(pool)
    .await?;

    // The two queries merge on node_id. A machine with a monthly total but no recent samples
    // (it ran this month and just stopped) must still appear in the result, or the UI treats it
    // as never enrolled.
    let blank = |node_id: String| UsageNodeSeries {
        node_id,
        buckets: Vec::new(),
        forwarding: Vec::new(),
        month_forwarding_uplink_bytes: 0,
        month_forwarding_downlink_bytes: 0,
        month_user_uplink_bytes: 0,
        month_user_downlink_bytes: 0,
        month_relay_uplink_bytes: 0,
        month_relay_downlink_bytes: 0,
        month_has_gap: false,
    };
    let mut by_node: BTreeMap<String, UsageNodeSeries> = BTreeMap::new();
    for row in &bucket_rows {
        let node_id: String = row.try_get("node_id")?;
        by_node
            .entry(node_id.clone())
            .or_insert_with(|| blank(node_id))
            .buckets
            .push(UsageNodeBucket {
                window_end: row.try_get("window_end")?,
                user_uplink_bytes: i64_to_u64(
                    "user_uplink_bytes",
                    row.try_get("user_uplink_bytes")?,
                )?,
                user_downlink_bytes: i64_to_u64(
                    "user_downlink_bytes",
                    row.try_get("user_downlink_bytes")?,
                )?,
                relay_uplink_bytes: i64_to_u64(
                    "relay_uplink_bytes",
                    row.try_get("relay_uplink_bytes")?,
                )?,
                relay_downlink_bytes: i64_to_u64(
                    "relay_downlink_bytes",
                    row.try_get("relay_downlink_bytes")?,
                )?,
            });
    }
    for row in &month_rows {
        let node_id: String = row.try_get("node_id")?;
        let entry = by_node
            .entry(node_id.clone())
            .or_insert_with(|| blank(node_id));
        entry.month_user_uplink_bytes =
            i64_to_u64("user_uplink_bytes", row.try_get("user_uplink_bytes")?)?;
        entry.month_user_downlink_bytes =
            i64_to_u64("user_downlink_bytes", row.try_get("user_downlink_bytes")?)?;
        entry.month_relay_uplink_bytes =
            i64_to_u64("relay_uplink_bytes", row.try_get("relay_uplink_bytes")?)?;
        entry.month_relay_downlink_bytes =
            i64_to_u64("relay_downlink_bytes", row.try_get("relay_downlink_bytes")?)?;
        entry.month_has_gap = row.try_get("has_gap")?;
    }

    let forwarding_windows = sqlx::query(
        "SELECT node_id,window_start::text,window_end::text,sum(uplink_bytes)::bigint AS up,
                sum(downlink_bytes)::bigint AS down,bool_or(has_gap) AS has_gap
         FROM forwarding_usage_windows WHERE window_end >= $1::timestamptz AND window_end <= $2::timestamptz
           AND ($3::text IS NULL OR tenant_id=$3 OR tenant_id LIKE $4 ESCAPE '\\')
           AND ($5::text IS NULL OR node_id=$5)
         GROUP BY node_id,window_start,window_end ORDER BY node_id,window_end,window_start",
    ).bind(&since).bind(&until).bind(tenant_scope).bind(&tenant_pattern).bind(node_id).fetch_all(pool).await?;
    for row in forwarding_windows {
        let node_id: String = row.try_get("node_id")?;
        by_node
            .entry(node_id.clone())
            .or_insert_with(|| blank(node_id))
            .forwarding
            .push(UsageForwardingWindow {
                window_start: row.try_get("window_start")?,
                window_end: row.try_get("window_end")?,
                uplink_bytes: i64_to_u64("up", row.try_get("up")?)?,
                downlink_bytes: i64_to_u64("down", row.try_get("down")?)?,
                has_gap: row.try_get("has_gap")?,
            });
    }
    let forwarding_month = sqlx::query(
        "SELECT node_id,sum(uplink_bytes)::bigint AS up,sum(downlink_bytes)::bigint AS down,bool_or(has_gap) AS has_gap
         FROM forwarding_usage_rollups WHERE period_start >= $1::timestamptz AND period_start < $2::timestamptz
           AND ($3::text IS NULL OR tenant_id=$3 OR tenant_id LIKE $4 ESCAPE '\\')
           AND ($5::text IS NULL OR node_id=$5) GROUP BY node_id",
    ).bind(&inst_month_start).bind(&inst_month_end).bind(tenant_scope).bind(&tenant_pattern).bind(node_id).fetch_all(pool).await?;
    for row in forwarding_month {
        let node_id: String = row.try_get("node_id")?;
        let entry = by_node
            .entry(node_id.clone())
            .or_insert_with(|| blank(node_id));
        entry.month_forwarding_uplink_bytes = i64_to_u64("up", row.try_get("up")?)?;
        entry.month_forwarding_downlink_bytes = i64_to_u64("down", row.try_get("down")?)?;
        entry.month_has_gap |= row.try_get::<bool, _>("has_gap")?;
    }

    Ok(UsageNodeSeriesList {
        since,
        month_start: wall_month_start,
        nodes: by_node.into_values().collect(),
    })
}

/// Delete one bounded batch of raw readings past their retention (each day is exactly 24 hours).
///
/// `usage_readings` is audit history only; the non-expiring baseline lives in
/// `usage_counter_heads`. This split is what makes retention safe even when a label is quiet for
/// longer than the retention window. Receipts expire independently, so removing diagnostic rows
/// must not change a retry's response. Concurrent cleaners skip locked rows; the batch and SQL
/// time limits prevent first-run catch-up from deleting days of history in one transaction.
pub async fn prune_usage_readings(pool: &PgPool, retain_days: u32) -> Result<u64> {
    prune_usage_readings_hours(pool, retain_days.saturating_mul(24)).await
}

pub(crate) async fn prune_usage_readings_hours(pool: &PgPool, retain_hours: u32) -> Result<u64> {
    crate::retention::cleanup::prune_batch(pool, "usage_readings", retain_hours).await
}

/// Expire replay responses separately from raw readings. The permanent cursor still rejects an
/// older sequence after its response expires; the durable counter baseline is never pruned here.
pub async fn prune_usage_report_receipts(pool: &PgPool, retain_days: u32) -> Result<u64> {
    prune_usage_report_receipts_hours(pool, retain_days.saturating_mul(24)).await
}

pub(crate) async fn prune_usage_report_receipts_hours(
    pool: &PgPool,
    retain_hours: u32,
) -> Result<u64> {
    crate::retention::cleanup::prune_batch(pool, "usage_report_receipts", retain_hours).await
}

/// Delete accounting detail only after its daily machine and user projections are durable.
///
/// The UI asks detail tables for at most one 24-hour interval. Seven days leaves enough evidence
/// for incident reconstruction while keeping details and exact machine windows bounded. One chunk per table
/// and hour lets an installation catch up without a long transaction or a sudden WAL spike.
pub async fn prune_usage_samples(pool: &PgPool, retain_days: u32) -> Result<u64> {
    let retain_hours = retain_days.saturating_mul(24);
    prune_usage_samples_hours(pool, retain_hours, retain_hours, retain_hours).await
}

pub(crate) async fn prune_usage_samples_hours(
    pool: &PgPool,
    user_retain_hours: u32,
    chain_retain_hours: u32,
    node_window_retain_hours: u32,
) -> Result<u64> {
    let mut total = 0;
    for (table, hours) in [
        ("usage_samples", user_retain_hours),
        ("usage_chain_samples", chain_retain_hours),
        ("usage_node_windows", node_window_retain_hours),
    ] {
        total += crate::retention::cleanup::prune_batch(pool, table, hours).await?;
    }
    Ok(total)
}

pub async fn list_usage_samples(
    pool: &PgPool,
    actor: &AdminContext,
    limit: u32,
    tenant_id: Option<&str>,
    user_id: Option<&str>,
    node_id: Option<&str>,
) -> Result<UsageSampleList> {
    let limit = i64::from(limit.clamp(1, 500));
    let tenant_scope = actor.tenant_scope();
    let tenant_pattern = actor.tenant_scope_like_pattern();
    let rows = sqlx::query(
        "SELECT id,
                sampled_at::text AS sampled_at,
                window_start::text AS window_start,
                window_end::text AS window_end,
                node_id,
                tenant_id,
                user_id,
                ingress_id,
                grant_label,
                uplink_bytes,
                downlink_bytes,
                has_gap,
                revision_id,
                deployment_id
         FROM usage_samples
         WHERE ($2::text IS NULL OR tenant_id = $2)
           AND ($3::text IS NULL OR user_id = $3)
           AND ($4::text IS NULL OR node_id = $4)
           AND (
                $5::text IS NULL
                OR tenant_id = $5
                OR tenant_id LIKE $6 ESCAPE '\\'
           )
         ORDER BY window_end DESC, id DESC
         LIMIT $1",
    )
    .bind(limit)
    .bind(tenant_id)
    .bind(user_id)
    .bind(node_id)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(pool)
    .await?;

    let samples = rows
        .iter()
        .map(|row| {
            let revision_id = row
                .try_get::<Option<i64>, _>("revision_id")?
                .map(revision_to_u64)
                .transpose()?;
            Ok(UsageSample {
                id: row.try_get("id")?,
                sampled_at: row.try_get("sampled_at")?,
                window_start: row.try_get("window_start")?,
                window_end: row.try_get("window_end")?,
                node_id: row.try_get("node_id")?,
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
                ingress_id: row.try_get("ingress_id")?,
                grant_label: row.try_get("grant_label")?,
                uplink_bytes: i64_to_u64("uplink_bytes", row.try_get("uplink_bytes")?)?,
                downlink_bytes: i64_to_u64("downlink_bytes", row.try_get("downlink_bytes")?)?,
                has_gap: row.try_get("has_gap")?,
                revision_id,
                deployment_id: row.try_get("deployment_id")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    // Link hops are returned alongside user samples but in separate fields. Filtering by user_id
    // returns none of them: that filter means "how much did this person use", and link overhead
    // belongs to nobody.
    let chain_samples = if user_id.is_some() {
        Vec::new()
    } else {
        list_chain_samples(
            pool,
            limit,
            tenant_id,
            node_id,
            tenant_scope,
            &tenant_pattern,
        )
        .await?
    };

    Ok(UsageSampleList {
        samples,
        chain_samples,
    })
}

/// The calendar-month rollup: one row per user across all their access points (usage_samples
/// writes only granted rows, so a per-user sum is naturally the traffic of their view's access
/// points). Months are calendar months in the control plane's local zone (+08). The UI selects
/// only a bounded offset and never supplies timestamp boundaries. Tenant-subtree scoping matches
/// list_usage_samples.
pub async fn list_monthly_usage_summary(
    pool: &PgPool,
    actor: &AdminContext,
) -> Result<UsageMonthlySummary> {
    list_monthly_usage_summary_for_offset(pool, actor, 0).await
}

/// The selected calendar-month rollup. The public console deliberately exposes only the current
/// and immediately preceding month: those are the two browsing states in the UI, and bounding the
/// offset prevents this detail-table query from becoming an unrestricted historical export.
pub async fn list_monthly_usage_summary_for_offset(
    pool: &PgPool,
    actor: &AdminContext,
    month_offset: i16,
) -> Result<UsageMonthlySummary> {
    if !(-1..=0).contains(&month_offset) {
        return Err(StoreError::InvalidData(
            "usage month_offset must be -1 or 0".to_owned(),
        ));
    }
    // Reports keep arriving while this read runs. Both aggregations must observe one snapshot or
    // the headline total can briefly disagree with the sum of the daily bars.
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let tenant_scope = actor.tenant_scope();
    let tenant_pattern = actor.tenant_scope_like_pattern();
    // Month boundaries need two representations:
    //  - wall_*: +08 wall-clock strings, human-facing (the month_start/month_end response). A
    //    timestamptz's ::text cannot be handed over directly — that rendering follows the
    //    database session's timezone, and under a UTC session midnight on 1 August renders as
    //    "2026-07-31 16:00:00+00", from which the UI slices 2026-07.
    //  - inst_*: timestamptz text (carrying an offset, unambiguous to parse), used only to
    //    filter samples.
    let (wall_start, wall_end, inst_start, inst_end): (String, String, String, String) =
        sqlx::query_as(
            "WITH selected AS (
                 SELECT date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                        + make_interval(months => $1) AS month_start
             )
             SELECT to_char(month_start,
                        'YYYY-MM-DD HH24:MI:SS'),
                    to_char(month_start + INTERVAL '1 month', 'YYYY-MM-DD HH24:MI:SS'),
                    (month_start AT TIME ZONE 'Asia/Hong_Kong')::text,
                    ((month_start + INTERVAL '1 month') AT TIME ZONE 'Asia/Hong_Kong')::text
             FROM selected",
        )
        .bind(i32::from(month_offset))
        .fetch_one(&mut *tx)
        .await?;
    // Daily rollups carry the same frozen app attribution as the detail samples. The ingestion
    // transaction updates both atomically, so the page scans at most one row per user/view/day
    // instead of every 30-second window twice.
    let rows = sqlx::query(
        "WITH monthly AS (
             SELECT s.tenant_id,
                    s.user_id,
                    s.app_id,
                    sum(s.uplink_bytes)::bigint AS uplink_bytes,
                    sum(s.downlink_bytes)::bigint AS downlink_bytes,
                    bool_or(s.has_gap) AS has_gap
             FROM usage_rollups s
             WHERE s.period_start >= $1::timestamptz
               AND s.period_start < $2::timestamptz
               AND (
                    $3::text IS NULL
                    OR s.tenant_id = $3
                    OR s.tenant_id LIKE $4 ESCAPE '\\'
               )
             GROUP BY s.tenant_id, s.user_id, s.app_id
         )
         SELECT monthly.tenant_id, monthly.user_id, monthly.app_id,
                monthly.uplink_bytes, monthly.downlink_bytes, monthly.has_gap
         FROM monthly
         LEFT JOIN apps a ON a.id = monthly.app_id
         ORDER BY monthly.tenant_id, monthly.user_id,
                  a.position NULLS LAST, monthly.app_id",
    )
    .bind(&inst_start)
    .bind(&inst_end)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(&mut *tx)
    .await?;

    let views = rows
        .iter()
        .map(|row| {
            Ok(UsageMonthlyViewRow {
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
                app_id: row.try_get("app_id")?,
                uplink_bytes: i64_to_u64("uplink_bytes", row.try_get("uplink_bytes")?)?,
                downlink_bytes: i64_to_u64("downlink_bytes", row.try_get("downlink_bytes")?)?,
                has_gap: row.try_get("has_gap")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let forwarding_rows = sqlx::query(
        "SELECT s.tenant_id,s.user_id,s.node_id,s.rule_id,r.name AS rule_name,
                sum(s.uplink_bytes)::bigint AS uplink_bytes,sum(s.downlink_bytes)::bigint AS downlink_bytes,
                bool_or(s.has_gap) AS has_gap
         FROM forwarding_usage_rollups s JOIN forwarding_rules r ON r.id=s.rule_id
         WHERE s.period_start >= $1::timestamptz AND s.period_start < $2::timestamptz
           AND ($3::text IS NULL OR s.tenant_id=$3 OR s.tenant_id LIKE $4 ESCAPE '\\')
         GROUP BY s.tenant_id,s.user_id,s.node_id,s.rule_id,r.name ORDER BY s.tenant_id,s.user_id,s.node_id,s.rule_id",
    ).bind(&inst_start).bind(&inst_end).bind(tenant_scope).bind(&tenant_pattern).fetch_all(&mut *tx).await?;
    let forwarding = forwarding_rows
        .iter()
        .map(|row| {
            Ok(UsageForwardingMonthlyRow {
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
                node_id: row.try_get("node_id")?,
                rule_id: row.try_get("rule_id")?,
                rule_name: row.try_get("rule_name")?,
                uplink_bytes: i64_to_u64("uplink_bytes", row.try_get("uplink_bytes")?)?,
                downlink_bytes: i64_to_u64("downlink_bytes", row.try_get("downlink_bytes")?)?,
                has_gap: row.try_get("has_gap")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    // Daily composition uses the exact same attribution and filtering boundary as the view
    // totals above. `window_start` decides both the month and local day; a 30-second sample is
    // never split across two labels. Empty dates are omitted and filled by the UI from the
    // returned calendar bounds.
    let day_rows = sqlx::query(
        "SELECT to_char(date_trunc('day', s.period_start AT TIME ZONE 'Asia/Hong_Kong'),
                        'YYYY-MM-DD') AS day,
                sum(s.uplink_bytes)::bigint AS uplink_bytes,
                sum(s.downlink_bytes)::bigint AS downlink_bytes,
                bool_or(s.has_gap) AS has_gap
         FROM (
             SELECT period_start,tenant_id,uplink_bytes,downlink_bytes,has_gap FROM usage_rollups
             UNION ALL
             SELECT period_start,tenant_id,uplink_bytes,downlink_bytes,has_gap FROM forwarding_usage_rollups
         ) s
         WHERE s.period_start >= $1::timestamptz
           AND s.period_start < $2::timestamptz
           AND (
                $3::text IS NULL
                OR s.tenant_id = $3
                OR s.tenant_id LIKE $4 ESCAPE '\\'
           )
         GROUP BY date_trunc('day', s.period_start AT TIME ZONE 'Asia/Hong_Kong')
         ORDER BY date_trunc('day', s.period_start AT TIME ZONE 'Asia/Hong_Kong')",
    )
    .bind(&inst_start)
    .bind(&inst_end)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(&mut *tx)
    .await?;
    let days = day_rows
        .iter()
        .map(|row| {
            Ok(UsageDailyRow {
                day: row.try_get("day")?,
                uplink_bytes: i64_to_u64("uplink_bytes", row.try_get("uplink_bytes")?)?,
                downlink_bytes: i64_to_u64("downlink_bytes", row.try_get("downlink_bytes")?)?,
                has_gap: row.try_get("has_gap")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    tx.commit().await?;

    Ok(UsageMonthlySummary {
        month_start: wall_start,
        month_end: wall_end,
        views,
        forwarding,
        days,
    })
}

async fn list_chain_samples(
    pool: &PgPool,
    limit: i64,
    tenant_id: Option<&str>,
    node_id: Option<&str>,
    tenant_scope: Option<&str>,
    tenant_pattern: &Option<String>,
) -> Result<Vec<UsageChainSample>> {
    let rows = sqlx::query(
        "SELECT id,
                sampled_at::text AS sampled_at,
                window_start::text AS window_start,
                window_end::text AS window_end,
                node_id,
                tenant_id,
                app_id,
                chain_id,
                hop_label,
                uplink_bytes,
                downlink_bytes,
                has_gap,
                revision_id,
                deployment_id
         FROM usage_chain_samples
         WHERE ($2::text IS NULL OR tenant_id = $2)
           AND ($3::text IS NULL OR node_id = $3)
           AND (
                $4::text IS NULL
                OR tenant_id = $4
                OR tenant_id LIKE $5 ESCAPE '\\'
           )
         ORDER BY window_end DESC, id DESC
         LIMIT $1",
    )
    .bind(limit)
    .bind(tenant_id)
    .bind(node_id)
    .bind(tenant_scope)
    .bind(tenant_pattern)
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(|row| {
            let revision_id = row
                .try_get::<Option<i64>, _>("revision_id")?
                .map(revision_to_u64)
                .transpose()?;
            Ok(UsageChainSample {
                id: row.try_get("id")?,
                sampled_at: row.try_get("sampled_at")?,
                window_start: row.try_get("window_start")?,
                window_end: row.try_get("window_end")?,
                node_id: row.try_get("node_id")?,
                tenant_id: row.try_get("tenant_id")?,
                app_id: row.try_get("app_id")?,
                chain_id: row.try_get("chain_id")?,
                hop_label: row.try_get("hop_label")?,
                uplink_bytes: i64_to_u64("uplink_bytes", row.try_get("uplink_bytes")?)?,
                downlink_bytes: i64_to_u64("downlink_bytes", row.try_get("downlink_bytes")?)?,
                has_gap: row.try_get("has_gap")?,
                revision_id,
                deployment_id: row.try_get("deployment_id")?,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct UsageMetadata {
    revision_id: Option<i64>,
    deployment_id: Option<i64>,
}

#[derive(Debug)]
struct GrantMapping {
    tenant_id: String,
    user_id: String,
    ingress_id: String,
    app_id: String,
    label: String,
}

/// A relay hop's attribution. There is no user — this hop's credential is `{chain}@{node}`, and
/// that family of labels has no per-user dimension to begin with.
#[derive(Debug)]
struct ChainHopMapping {
    tenant_id: String,
    app_id: String,
    chain_id: String,
    label: String,
    /// Which machine this hop counts against. Usually the reporting one, though not under
    /// `HopDial::Reverse`: there the bytes were read by the portal machine while the hop belongs
    /// to the bridge.
    node_id: String,
}

#[derive(Debug)]
enum CounterOwner {
    User(GrantMapping),
    ChainHop(ChainHopMapping),
}

/// The baseline the next reading is differenced against. No start time among the fields: the
/// counters alone decide whether xray restarted (see `record_usage_report`), and keeping a
/// timestamp here would invite that decision to be made on it again.
#[derive(Debug)]
struct UsageHead {
    read_at_unix_secs: i64,
    uplink_bytes: i64,
    downlink_bytes: i64,
    xray_epoch: String,
    generation_id: i64,
}

async fn usage_head_for_update(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    label: &str,
) -> Result<Option<UsageHead>> {
    let row = sqlx::query(
        "SELECT EXTRACT(EPOCH FROM read_at)::bigint AS read_at_unix_secs,
                uplink_bytes, downlink_bytes, xray_epoch, generation_id
         FROM usage_counter_heads
         WHERE node_id = $1 AND label = $2
         FOR UPDATE",
    )
    .bind(node_id)
    .bind(label)
    .fetch_optional(&mut **tx)
    .await?;

    row.map(|row| {
        Ok(UsageHead {
            read_at_unix_secs: row.try_get("read_at_unix_secs")?,
            uplink_bytes: row.try_get("uplink_bytes")?,
            downlink_bytes: row.try_get("downlink_bytes")?,
            xray_epoch: row.try_get("xray_epoch")?,
            generation_id: row.try_get("generation_id")?,
        })
    })
    .transpose()
}

#[allow(clippy::too_many_arguments)]
async fn upsert_usage_head(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    label: &str,
    xray_epoch: &str,
    read_at_unix_secs: i64,
    xray_started_at_unix_secs: i64,
    uplink_bytes: i64,
    downlink_bytes: i64,
    generation_id: i64,
    agent_instance_id: &str,
    sequence: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO usage_counter_heads (
            node_id, label, xray_epoch, read_at, xray_started_at,
            uplink_bytes, downlink_bytes, generation_id, agent_instance_id, sequence
         ) VALUES (
            $1, $2, $3, to_timestamp($4::double precision),
            to_timestamp($5::double precision), $6, $7, $8, $9, $10
         )
         ON CONFLICT (node_id, label) DO UPDATE SET
            xray_epoch = EXCLUDED.xray_epoch,
            read_at = EXCLUDED.read_at,
            xray_started_at = EXCLUDED.xray_started_at,
            uplink_bytes = EXCLUDED.uplink_bytes,
            downlink_bytes = EXCLUDED.downlink_bytes,
            generation_id = EXCLUDED.generation_id,
            agent_instance_id = EXCLUDED.agent_instance_id,
            sequence = EXCLUDED.sequence",
    )
    .bind(node_id)
    .bind(label)
    .bind(xray_epoch)
    .bind(read_at_unix_secs)
    .bind(xray_started_at_unix_secs)
    .bind(uplink_bytes)
    .bind(downlink_bytes)
    .bind(generation_id)
    .bind(agent_instance_id)
    .bind(sequence)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

struct UsageReadingInsert<'a> {
    node_id: &'a str,
    label: &'a str,
    agent_instance_id: &'a str,
    sequence: i64,
    generation_id: i64,
    xray_epoch: &'a str,
    read_at_unix_secs: i64,
    xray_started_at_unix_secs: i64,
    uplink_bytes: i64,
    downlink_bytes: i64,
}

async fn insert_usage_reading(
    tx: &mut Transaction<'_, Postgres>,
    reading: UsageReadingInsert<'_>,
) -> Result<bool> {
    let result = sqlx::query(
        "INSERT INTO usage_readings (
            node_id, label, agent_instance_id, sequence, generation_id, xray_epoch,
            read_at, xray_started_at, uplink_bytes, downlink_bytes
         )
         VALUES (
            $1, $2, $3, $4, $5, $6,
            to_timestamp($7::double precision),
            to_timestamp($8::double precision),
            $9, $10
         )
         ON CONFLICT (node_id, agent_instance_id, sequence, label) DO NOTHING",
    )
    .bind(reading.node_id)
    .bind(reading.label)
    .bind(reading.agent_instance_id)
    .bind(reading.sequence)
    .bind(reading.generation_id)
    .bind(reading.xray_epoch)
    .bind(reading.read_at_unix_secs)
    .bind(reading.xray_started_at_unix_secs)
    .bind(reading.uplink_bytes)
    .bind(reading.downlink_bytes)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

struct UsageSampleInsert<'a> {
    node_id: &'a str,
    owner: &'a CounterOwner,
    window_start_unix_secs: i64,
    window_end_unix_secs: i64,
    uplink_bytes: i64,
    downlink_bytes: i64,
    has_gap: bool,
    revision_id: Option<i64>,
    deployment_id: Option<i64>,
    generation_id: i64,
}

#[derive(Default)]
struct UsageWindowTotals {
    user_up: i64,
    user_down: i64,
    relay_up: i64,
    relay_down: i64,
}

fn add_node_window(
    windows: &mut BTreeMap<(String, String), UsageWindowTotals>,
    reporter: &str,
    owner: &CounterOwner,
    up: i64,
    down: i64,
) -> Result<()> {
    let (node, tenant, relay) = match owner {
        CounterOwner::User(grant) => (reporter, grant.tenant_id.as_str(), false),
        CounterOwner::ChainHop(hop) => (hop.node_id.as_str(), hop.tenant_id.as_str(), true),
    };
    let totals = windows
        .entry((node.to_owned(), tenant.to_owned()))
        .or_default();
    let (total_up, total_down) = if relay {
        (&mut totals.relay_up, &mut totals.relay_down)
    } else {
        (&mut totals.user_up, &mut totals.user_down)
    };
    *total_up = total_up
        .checked_add(up)
        .ok_or_else(|| StoreError::InvalidData("usage window uplink overflow".to_owned()))?;
    *total_down = total_down
        .checked_add(down)
        .ok_or_else(|| StoreError::InvalidData("usage window downlink overflow".to_owned()))?;
    Ok(())
}

async fn persist_node_windows(
    tx: &mut Transaction<'_, Postgres>,
    window_end: i64,
    windows: BTreeMap<(String, String), UsageWindowTotals>,
) -> Result<()> {
    // Sorted ownership keys give competing reports one lock order, including reverse-hop
    // attribution to a machine other than the reporter. Receipt and heads guard exact replay.
    for ((node, tenant), totals) in windows {
        sqlx::query(
            "INSERT INTO usage_node_windows
                (node_id, tenant_id, window_end, user_uplink_bytes, user_downlink_bytes,
                 relay_uplink_bytes, relay_downlink_bytes)
             VALUES ($1, $2, to_timestamp($3::double precision), $4, $5, $6, $7)
             ON CONFLICT (node_id, window_end, tenant_id) DO UPDATE SET
                user_uplink_bytes = usage_node_windows.user_uplink_bytes + EXCLUDED.user_uplink_bytes,
                user_downlink_bytes = usage_node_windows.user_downlink_bytes + EXCLUDED.user_downlink_bytes,
                relay_uplink_bytes = usage_node_windows.relay_uplink_bytes + EXCLUDED.relay_uplink_bytes,
                relay_downlink_bytes = usage_node_windows.relay_downlink_bytes + EXCLUDED.relay_downlink_bytes",
        )
        .bind(node).bind(tenant).bind(window_end as f64)
        .bind(totals.user_up).bind(totals.user_down).bind(totals.relay_up).bind(totals.relay_down)
        .execute(&mut **tx).await?;
    }
    Ok(())
}

async fn insert_usage_sample(
    tx: &mut Transaction<'_, Postgres>,
    sample: UsageSampleInsert<'_>,
) -> Result<bool> {
    let CounterOwner::User(grant) = sample.owner else {
        return Ok(false);
    };
    if sample.uplink_bytes == 0 && sample.downlink_bytes == 0 && !sample.has_gap {
        // Zero is a valid collected window, not an audit event. The machine projection preserves
        // that point, while the durable head/receipt already provide ordering and retry safety.
        // Keep day-level presence identical without rewriting the same zero totals every 30s.
        sqlx::query(
            "WITH period AS (
                SELECT date_trunc('day', to_timestamp($4::double precision) AT TIME ZONE 'Asia/Hong_Kong')
                           AT TIME ZONE 'Asia/Hong_Kong' AS start
             ), user_day AS (
                INSERT INTO usage_rollups
                    (tenant_id, user_id, app_id, period_start, period_end, uplink_bytes, downlink_bytes, has_gap)
                SELECT $1, $2, $3, start, start + interval '24 hours', 0, 0, FALSE FROM period
                ON CONFLICT DO NOTHING
             )
             INSERT INTO usage_node_rollups (tenant_id, node_id, period_start, period_end)
             SELECT $1, $5, start, start + interval '24 hours' FROM period
             ON CONFLICT DO NOTHING",
        )
        .bind(&grant.tenant_id).bind(&grant.user_id).bind(&grant.app_id)
        .bind(sample.window_start_unix_secs as f64).bind(sample.node_id)
        .execute(&mut **tx).await?;
        crate::user_machine_usage::record(
            tx,
            sample.node_id,
            &grant.tenant_id,
            &grant.user_id,
            sample.window_start_unix_secs,
            (0, 0),
            false,
        )
        .await?;
        return Ok(true);
    }
    // app_id is looked up from ingresses once at insert and frozen, no longer following the
    // model: when an ingress is moved to another view, this row still says which view it counted
    // against at the time. Derived on demand, a quota's numerator would jump wholesale to the new
    // view the instant the model changed.
    // An ingress that cannot be found leaves NULL, and the query side falls back to a JOIN.
    let inserted = sqlx::query_scalar::<_, i32>(
        "WITH inserted AS (
             INSERT INTO usage_samples (
                window_start, window_end, node_id,
                tenant_id, user_id, ingress_id, app_id, grant_label,
                uplink_bytes, downlink_bytes, has_gap,
                revision_id, deployment_id, generation_id
             )
             VALUES (
                to_timestamp($1::double precision),
                to_timestamp($2::double precision),
                $3, $4, $5, $6,
                $7, $8, $9, $10, $11, $12, $13, $14
             )
             ON CONFLICT (node_id, grant_label, window_start, window_end) DO NOTHING
             RETURNING tenant_id, node_id, user_id, app_id, window_start,
                       uplink_bytes, downlink_bytes, has_gap
         ), rolled AS (
             INSERT INTO usage_rollups (
                tenant_id, user_id, app_id, period_start, period_end,
                uplink_bytes, downlink_bytes, has_gap, updated_at
             )
             SELECT tenant_id, user_id, app_id,
                    date_trunc('day', window_start AT TIME ZONE 'Asia/Hong_Kong')
                        AT TIME ZONE 'Asia/Hong_Kong',
                    (date_trunc('day', window_start AT TIME ZONE 'Asia/Hong_Kong')
                        + INTERVAL '1 day') AT TIME ZONE 'Asia/Hong_Kong',
                    uplink_bytes, downlink_bytes, has_gap, now()
               FROM inserted
              WHERE app_id IS NOT NULL
             ON CONFLICT (tenant_id, user_id, app_id, period_start, period_end)
             DO UPDATE SET
                uplink_bytes = usage_rollups.uplink_bytes + EXCLUDED.uplink_bytes,
                downlink_bytes = usage_rollups.downlink_bytes + EXCLUDED.downlink_bytes,
                has_gap = usage_rollups.has_gap OR EXCLUDED.has_gap,
                updated_at = now()
             RETURNING 1
         ), node_rolled AS (
             INSERT INTO usage_node_rollups (
                tenant_id, node_id, period_start, period_end,
                user_uplink_bytes, user_downlink_bytes,
                relay_uplink_bytes, relay_downlink_bytes,
                has_gap, updated_at
             )
             SELECT tenant_id, node_id,
                    date_trunc('day', window_start AT TIME ZONE 'Asia/Hong_Kong')
                        AT TIME ZONE 'Asia/Hong_Kong',
                    (date_trunc('day', window_start AT TIME ZONE 'Asia/Hong_Kong')
                        + INTERVAL '1 day') AT TIME ZONE 'Asia/Hong_Kong',
                    uplink_bytes, downlink_bytes, 0, 0, has_gap, now()
               FROM inserted
             ON CONFLICT (tenant_id, node_id, period_start, period_end)
             DO UPDATE SET
                user_uplink_bytes = usage_node_rollups.user_uplink_bytes
                    + EXCLUDED.user_uplink_bytes,
                user_downlink_bytes = usage_node_rollups.user_downlink_bytes
                    + EXCLUDED.user_downlink_bytes,
                has_gap = usage_node_rollups.has_gap OR EXCLUDED.has_gap,
                updated_at = now()
             RETURNING 1
         )
         SELECT 1 FROM inserted",
    )
    .bind(sample.window_start_unix_secs)
    .bind(sample.window_end_unix_secs)
    .bind(sample.node_id)
    .bind(&grant.tenant_id)
    .bind(&grant.user_id)
    .bind(&grant.ingress_id)
    .bind(&grant.app_id)
    .bind(&grant.label)
    .bind(sample.uplink_bytes)
    .bind(sample.downlink_bytes)
    .bind(sample.has_gap)
    .bind(sample.revision_id)
    .bind(sample.deployment_id)
    .bind(sample.generation_id)
    .fetch_optional(&mut **tx)
    .await?;
    if inserted.is_some() {
        crate::user_machine_usage::record(
            tx,
            sample.node_id,
            &grant.tenant_id,
            &grant.user_id,
            sample.window_start_unix_secs,
            (sample.uplink_bytes, sample.downlink_bytes),
            sample.has_gap,
        )
        .await?;
        crate::node_inflow_usage::record(
            tx,
            crate::node_inflow_usage::IngressWindow {
                node_id: sample.node_id,
                tenant_id: &grant.tenant_id,
                user_id: &grant.user_id,
                ingress_id: &grant.ingress_id,
                app_id: &grant.app_id,
                window_start_unix_secs: sample.window_start_unix_secs,
                uplink_bytes: sample.uplink_bytes,
                downlink_bytes: sample.downlink_bytes,
                has_gap: sample.has_gap,
            },
        )
        .await?;
    }
    Ok(inserted.is_some())
}

async fn insert_chain_sample(
    tx: &mut Transaction<'_, Postgres>,
    sample: UsageSampleInsert<'_>,
) -> Result<bool> {
    let CounterOwner::ChainHop(hop) = sample.owner else {
        return Ok(false);
    };
    // `hop.node_id` rather than `sample.node_id`: under reverse access the counter was read by
    // the portal machine while the hop is forwarded by the bridge. The raw reading is still
    // recorded against whoever read it (differences must be taken against one source), and only
    // this step, turning it into accounting, attributes it to the hop's owner.
    let inserted = sqlx::query_scalar::<_, i32>(
        "WITH inserted AS (
             INSERT INTO usage_chain_samples (
                window_start, window_end, node_id,
                tenant_id, app_id, chain_id, hop_label,
                uplink_bytes, downlink_bytes, has_gap,
                revision_id, deployment_id, generation_id
             )
             VALUES (
                to_timestamp($1::double precision),
                to_timestamp($2::double precision),
                $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13
             )
             ON CONFLICT (node_id, hop_label, window_start, window_end) DO NOTHING
             RETURNING tenant_id, node_id, window_start,
                       uplink_bytes, downlink_bytes, has_gap
         ), rolled AS (
             INSERT INTO usage_node_rollups (
                tenant_id, node_id, period_start, period_end,
                user_uplink_bytes, user_downlink_bytes,
                relay_uplink_bytes, relay_downlink_bytes,
                has_gap, updated_at
             )
             SELECT tenant_id, node_id,
                    date_trunc('day', window_start AT TIME ZONE 'Asia/Hong_Kong')
                        AT TIME ZONE 'Asia/Hong_Kong',
                    (date_trunc('day', window_start AT TIME ZONE 'Asia/Hong_Kong')
                        + INTERVAL '1 day') AT TIME ZONE 'Asia/Hong_Kong',
                    0, 0, uplink_bytes, downlink_bytes, has_gap, now()
               FROM inserted
             ON CONFLICT (tenant_id, node_id, period_start, period_end)
             DO UPDATE SET
                relay_uplink_bytes = usage_node_rollups.relay_uplink_bytes
                    + EXCLUDED.relay_uplink_bytes,
                relay_downlink_bytes = usage_node_rollups.relay_downlink_bytes
                    + EXCLUDED.relay_downlink_bytes,
                has_gap = usage_node_rollups.has_gap OR EXCLUDED.has_gap,
                updated_at = now()
             RETURNING 1
         )
         SELECT 1 FROM inserted",
    )
    .bind(sample.window_start_unix_secs)
    .bind(sample.window_end_unix_secs)
    .bind(&hop.node_id)
    .bind(&hop.tenant_id)
    .bind(&hop.app_id)
    .bind(&hop.chain_id)
    .bind(&hop.label)
    .bind(sample.uplink_bytes)
    .bind(sample.downlink_bytes)
    .bind(sample.has_gap)
    .bind(sample.revision_id)
    .bind(sample.deployment_id)
    .bind(sample.generation_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(inserted.is_some())
}

fn u64_to_i64(field: &str, value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::InvalidData(format!("{field} is out of range")))
}

fn i64_to_u64(field: &str, value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| StoreError::InvalidData(format!("{field} is out of range")))
}

fn revision_to_u64(revision: i64) -> Result<u64> {
    u64::try_from(revision)
        .map_err(|_| StoreError::InvalidData(format!("revision_id out of range: {revision}")))
}

#[cfg(test)]
mod runtime_tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        assign_first_reading_policies, unknown_counter_fingerprint, FirstReadingPolicy,
        FrozenBinding, UnknownCounterObservation, UsageRuntimeState,
    };

    fn user_binding(user_id: &str, first_reading: FirstReadingPolicy) -> FrozenBinding {
        FrozenBinding::User {
            tenant_id: "platform.acme".to_owned(),
            user_id: user_id.to_owned(),
            ingress_id: "i-main".to_owned(),
            app_id: "app-main".to_owned(),
            chain_id: Some("chain-main".to_owned()),
            first_reading,
        }
    }

    #[test]
    fn first_generation_uses_a_conservative_baseline() {
        let mut bindings = BTreeMap::from([(
            "alice@platform.acme#i-main".to_owned(),
            user_binding("alice", FirstReadingPolicy::EstablishBaseline),
        )]);
        assign_first_reading_policies(&mut bindings, None, &BTreeSet::new());
        assert_eq!(
            bindings.values().next().unwrap().first_reading_policy(),
            FirstReadingPolicy::EstablishBaseline
        );
    }

    #[test]
    fn a_new_authorization_counts_from_zero_until_a_durable_head_exists() {
        let alice_label = "alice@platform.acme#i-main".to_owned();
        let bob_label = "bob@platform.acme#i-main".to_owned();
        let previous = BTreeMap::from([(
            alice_label.clone(),
            user_binding("alice", FirstReadingPolicy::EstablishBaseline),
        )]);
        let mut introduced = BTreeMap::from([
            (
                alice_label.clone(),
                user_binding("alice", FirstReadingPolicy::EstablishBaseline),
            ),
            (
                bob_label.clone(),
                user_binding("bob", FirstReadingPolicy::EstablishBaseline),
            ),
        ]);
        assign_first_reading_policies(&mut introduced, Some(&previous), &BTreeSet::new());
        assert_eq!(
            introduced[&alice_label].first_reading_policy(),
            FirstReadingPolicy::EstablishBaseline
        );
        assert_eq!(
            introduced[&bob_label].first_reading_policy(),
            FirstReadingPolicy::CountFromZero
        );

        // An unrelated generation can land before Bob transfers anything. Carry the proof that
        // this is a newly created counter instead of turning his eventual first report into a
        // baseline again.
        let mut next = BTreeMap::from([(
            bob_label.clone(),
            user_binding("bob", FirstReadingPolicy::EstablishBaseline),
        )]);
        assign_first_reading_policies(&mut next, Some(&introduced), &BTreeSet::new());
        assert_eq!(
            next[&bob_label].first_reading_policy(),
            FirstReadingPolicy::CountFromZero
        );

        let heads = BTreeSet::from([bob_label.clone()]);
        assign_first_reading_policies(&mut next, Some(&introduced), &heads);
        assert_eq!(
            next[&bob_label].first_reading_policy(),
            FirstReadingPolicy::EstablishBaseline
        );
    }

    fn observation(
        label: &str,
        epoch: &str,
        uplink: u64,
        downlink: u64,
    ) -> UnknownCounterObservation {
        UnknownCounterObservation {
            fingerprint: unknown_counter_fingerprint("n-test", label),
            xray_epoch: epoch.to_owned(),
            uplink_bytes: uplink,
            downlink_bytes: downlink,
        }
    }

    #[test]
    fn unknown_counter_warns_only_when_the_same_epoch_moves() {
        let runtime = UsageRuntimeState::default();
        assert_eq!(runtime.growing_unknown_counters("n-test"), None);

        runtime.observe_unknown_counters(
            "n-test",
            10,
            vec![observation("removed-user", "boot-a:100", 20, 30)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(0));

        runtime.observe_unknown_counters(
            "n-test",
            20,
            vec![observation("removed-user", "boot-a:100", 20, 30)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(0));

        runtime.observe_unknown_counters(
            "n-test",
            30,
            vec![observation("removed-user", "boot-a:100", 21, 30)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(1));

        // The finding describes the latest interval, rather than becoming a sticky alert.
        runtime.observe_unknown_counters(
            "n-test",
            40,
            vec![observation("removed-user", "boot-a:100", 21, 30)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(0));

        // An Xray restart establishes a new baseline; its smaller absolute value is not growth.
        runtime.observe_unknown_counters(
            "n-test",
            50,
            vec![observation("removed-user", "boot-a:900", 1, 2)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(0));
    }

    #[test]
    fn latest_unknown_round_prunes_disappeared_labels_and_ignores_old_replay() {
        let runtime = UsageRuntimeState::default();
        runtime.observe_unknown_counters(
            "n-test",
            20,
            vec![observation("removed-user", "boot-a:100", 10, 10)],
        );
        runtime.observe_unknown_counters(
            "n-test",
            30,
            vec![observation("removed-user", "boot-a:100", 20, 10)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(1));

        // An older queued report is billable history, not the latest operational state.
        runtime.observe_unknown_counters("n-test", 25, Vec::new());
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(1));

        runtime.observe_unknown_counters("n-test", 40, Vec::new());
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(0));

        // Reappearing after absence starts over instead of comparing against a stale head.
        runtime.observe_unknown_counters(
            "n-test",
            50,
            vec![observation("removed-user", "boot-a:100", 30, 10)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(0));
    }
}
