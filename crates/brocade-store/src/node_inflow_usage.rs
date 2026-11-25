//! A machine's inflow: user traffic entering through its access chains, its users' forwarding
//! rules, and the days before per-chain accounting. A projection of accepted accounting windows,
//! not a second billing path; relay and exit traffic carries no user identity and is not here.
use std::collections::{BTreeMap, BTreeSet};

use brocade_deployment::site_usage::SiteUsagePeriod;
use serde::Serialize;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, Result, StoreError};

#[derive(Debug, Serialize)]
pub struct NodeInflowUsage {
    pub period: SiteUsagePeriod,
    pub range_start_unix_secs: i64,
    pub range_end_unix_secs: i64,
    pub period_end_unix_secs: i64,
    /// Newest accepted accounting or forwarding reading from this machine; None if it never
    /// reported. It does not certify that the whole period was collected.
    pub latest_report_unix_secs: Option<i64>,
    pub rows: Vec<NodeInflowRow>,
    /// Users with a grant or forwarding entitlement on this machine and no inflow in the period.
    pub idle_users: Vec<NodeInflowUser>,
    pub range_limited: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeInflowSource {
    /// Authenticated proxy traffic entering through one of the machine's access chains.
    Chain,
    /// The user's own L4 forwarding rules on this machine.
    Forwarding,
    /// Known to the user × machine rollup but older than per-chain accounting. Never estimated.
    Unattributed,
}

#[derive(Debug, Serialize)]
pub struct NodeInflowRow {
    pub tenant_id: String,
    pub user_id: String,
    pub source: NodeInflowSource,
    pub app_id: Option<String>,
    pub chain_id: Option<String>,
    /// Only on a chain row whose ingress was already gone when its traffic was recorded.
    pub ingress_id: Option<String>,
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
    pub has_gap: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct NodeInflowUser {
    pub tenant_id: String,
    pub user_id: String,
}

/// One accepted accounting window of one user on one ingress.
pub(crate) struct IngressWindow<'a> {
    pub(crate) node_id: &'a str,
    pub(crate) tenant_id: &'a str,
    pub(crate) user_id: &'a str,
    pub(crate) ingress_id: &'a str,
    pub(crate) app_id: &'a str,
    pub(crate) window_start_unix_secs: i64,
    pub(crate) uplink_bytes: i64,
    pub(crate) downlink_bytes: i64,
    pub(crate) has_gap: bool,
}

/// Must run in the transaction that accepted the window, so a retried report cannot count twice.
pub(crate) async fn record(
    tx: &mut Transaction<'_, Postgres>,
    window: IngressWindow<'_>,
) -> Result<()> {
    // The day keeps the view and chain of its first window, like usage_samples.app_id.
    sqlx::query("INSERT INTO usage_user_ingress_rollups (node_id,period_start,tenant_id,user_id,ingress_id,app_id,chain_id,uplink_bytes,downlink_bytes,has_gap)
        VALUES ($1,date_trunc('day',to_timestamp($2) AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong',$3,$4,$5,$6,
            (SELECT chain_id FROM ingresses WHERE id=$5),$7,$8,$9)
        ON CONFLICT (node_id,period_start,tenant_id,user_id,ingress_id) DO UPDATE SET
        uplink_bytes=usage_user_ingress_rollups.uplink_bytes+EXCLUDED.uplink_bytes,
        downlink_bytes=usage_user_ingress_rollups.downlink_bytes+EXCLUDED.downlink_bytes,
        has_gap=usage_user_ingress_rollups.has_gap OR EXCLUDED.has_gap")
        .bind(window.node_id)
        .bind(window.window_start_unix_secs as f64)
        .bind(window.tenant_id)
        .bind(window.user_id)
        .bind(window.ingress_id)
        .bind(window.app_id)
        .bind(window.uplink_bytes)
        .bind(window.downlink_bytes)
        .bind(window.has_gap)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn bytes(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        StoreError::InvalidData(
            "machine inflow exceeds the user machine rollup; check the compatibility backfill"
                .into(),
        )
    })
}

type RowKey = (
    String,
    String,
    NodeInflowSource,
    Option<String>,
    Option<String>,
    Option<String>,
);

pub(crate) async fn query(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    period: SiteUsagePeriod,
) -> Result<NodeInflowUsage> {
    query_range(pool, actor, node_id, period, None).await
}

pub(crate) async fn query_range(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    period: SiteUsagePeriod,
    range: Option<brocade_deployment::source_traffic::TrafficTimeRange>,
) -> Result<NodeInflowUsage> {
    // Same boundary as the machine's live user rates: signed-in operators only, with tenant-scoped
    // operators limited to their subtree. The caller resolves machine visibility.
    if !actor.can_view_user_realtime_traffic() {
        return Err(StoreError::Forbidden(
            "machine inflow requires a signed-in operator".to_owned(),
        ));
    }
    let bounds = crate::site_usage::range_bounds(pool, period, range).await?;
    if range.is_some() {
        return query_windows(pool, actor, node_id, period, bounds).await;
    }
    let scope = actor.tenant_scope();
    let pattern = actor.tenant_scope_like_pattern();
    let mut tx = pool.begin().await?;
    // The chain rollup, forwarding and the machine rollup it is compared with share one snapshot.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let mut rows = BTreeMap::<RowKey, (u64, u64, bool)>::new();
    let chains = sqlx::query(
        "SELECT r.tenant_id,r.user_id,r.app_id,r.chain_id,
                CASE WHEN r.chain_id IS NULL THEN r.ingress_id END AS ingress_id,
                sum(r.uplink_bytes)::bigint AS up,sum(r.downlink_bytes)::bigint AS down,bool_or(r.has_gap) AS gap
           FROM usage_user_ingress_rollups r
          WHERE r.node_id=$1 AND r.period_start>=to_timestamp($2) AND r.period_start<=to_timestamp($3)
            AND ($4::text IS NULL OR r.tenant_id=$4 OR r.tenant_id LIKE $5 ESCAPE '\\')
          GROUP BY 1,2,3,4,5",
    )
    .bind(node_id)
    .bind(bounds.start as f64)
    .bind(bounds.now as f64)
    .bind(scope)
    .bind(pattern.as_deref())
    .fetch_all(&mut *tx)
    .await?;
    for row in chains {
        let key = (
            row.try_get("tenant_id")?,
            row.try_get("user_id")?,
            NodeInflowSource::Chain,
            Some(row.try_get::<String, _>("app_id")?),
            row.try_get("chain_id")?,
            row.try_get("ingress_id")?,
        );
        let entry = rows.entry(key).or_default();
        entry.0 += bytes(row.try_get("up")?)?;
        entry.1 += bytes(row.try_get("down")?)?;
        entry.2 |= row.try_get::<bool, _>("gap")?;
    }
    let forwarding = sqlx::query(
        "SELECT tenant_id,user_id,sum(uplink_bytes)::bigint AS up,sum(downlink_bytes)::bigint AS down,bool_or(has_gap) AS gap
           FROM forwarding_usage_rollups
          WHERE node_id=$1 AND period_start>=to_timestamp($2) AND period_start<=to_timestamp($3)
            AND ($4::text IS NULL OR tenant_id=$4 OR tenant_id LIKE $5 ESCAPE '\\')
          GROUP BY tenant_id,user_id",
    )
    .bind(node_id)
    .bind(bounds.start as f64)
    .bind(bounds.now as f64)
    .bind(scope)
    .bind(pattern.as_deref())
    .fetch_all(&mut *tx)
    .await?;
    for row in forwarding {
        rows.insert(
            (
                row.try_get("tenant_id")?,
                row.try_get("user_id")?,
                NodeInflowSource::Forwarding,
                None,
                None,
                None,
            ),
            (
                bytes(row.try_get("up")?)?,
                bytes(row.try_get("down")?)?,
                row.try_get("gap")?,
            ),
        );
    }
    // Compare day by day: a day the chain rollup already covers contributes nothing, a day before
    // it contributes the whole user × machine total. Never spread across chains or hours.
    let remainder = sqlx::query(
        "WITH machine AS (
            SELECT tenant_id,user_id,period_start,uplink_bytes AS up,downlink_bytes AS down,has_gap
              FROM usage_user_node_rollups
             WHERE node_id=$1 AND period_start>=to_timestamp($2) AND period_start<=to_timestamp($3)
               AND ($4::text IS NULL OR tenant_id=$4 OR tenant_id LIKE $5 ESCAPE '\\')
         ), known AS (
            SELECT tenant_id,user_id,period_start,sum(uplink_bytes)::bigint AS up,sum(downlink_bytes)::bigint AS down
              FROM usage_user_ingress_rollups
             WHERE node_id=$1 AND period_start>=to_timestamp($2) AND period_start<=to_timestamp($3)
               AND ($4::text IS NULL OR tenant_id=$4 OR tenant_id LIKE $5 ESCAPE '\\')
             GROUP BY tenant_id,user_id,period_start
         )
         SELECT m.tenant_id,m.user_id,
                sum(m.up-coalesce(k.up,0))::bigint AS up,sum(m.down-coalesce(k.down,0))::bigint AS down,
                min(least(m.up-coalesce(k.up,0),m.down-coalesce(k.down,0)))::bigint AS floor,bool_or(m.has_gap) AS gap
           FROM machine m LEFT JOIN known k USING (tenant_id,user_id,period_start)
          GROUP BY m.tenant_id,m.user_id",
    )
    .bind(node_id)
    .bind(bounds.start as f64)
    .bind(bounds.now as f64)
    .bind(scope)
    .bind(pattern.as_deref())
    .fetch_all(&mut *tx)
    .await?;
    for row in remainder {
        // A day where the chain rollup exceeds the machine rollup means the two projections
        // disagree; report it instead of hiding it behind a clamp.
        bytes(row.try_get::<i64, _>("floor")?.min(0))?;
        let up = bytes(row.try_get("up")?)?;
        let down = bytes(row.try_get("down")?)?;
        if up + down == 0 {
            continue;
        }
        rows.insert(
            (
                row.try_get("tenant_id")?,
                row.try_get("user_id")?,
                NodeInflowSource::Unattributed,
                None,
                None,
                None,
            ),
            (up, down, row.try_get("gap")?),
        );
    }
    let granted = sqlx::query(
        "SELECT DISTINCT tenant_id,user_id FROM (
            SELECT g.tenant_id,g.user_id FROM grants g JOIN ingresses i ON i.id=g.ingress_id WHERE i.node_id=$1
            UNION ALL SELECT tenant_id,user_id FROM forwarding_entitlements WHERE node_id=$1
         ) granted
         WHERE ($2::text IS NULL OR tenant_id=$2 OR tenant_id LIKE $3 ESCAPE '\\')",
    )
    .bind(node_id)
    .bind(scope)
    .bind(pattern.as_deref())
    .fetch_all(&mut *tx)
    .await?;
    let latest: Option<i64> = sqlx::query_scalar(
        "SELECT max(at)::bigint FROM (
            SELECT extract(epoch FROM read_at)::bigint AS at FROM usage_report_receipts WHERE node_id=$1
            UNION ALL SELECT extract(epoch FROM read_at)::bigint FROM forwarding_usage_counter_heads WHERE node_id=$1
         ) reports",
    )
    .bind(node_id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    // A gap-only window carries no bytes and draws no flow; such a user counts as idle.
    rows.retain(|_, totals| totals.0 + totals.1 > 0);
    let active: BTreeSet<(&str, &str)> = rows
        .keys()
        .map(|key| (key.0.as_str(), key.1.as_str()))
        .collect();
    let mut idle_users = granted
        .iter()
        .map(|row| {
            Ok(NodeInflowUser {
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    idle_users.retain(|user| !active.contains(&(user.tenant_id.as_str(), user.user_id.as_str())));
    idle_users.sort();
    Ok(NodeInflowUsage {
        period,
        range_start_unix_secs: bounds.start,
        range_end_unix_secs: bounds.now,
        period_end_unix_secs: bounds.end,
        latest_report_unix_secs: latest,
        rows: rows
            .into_iter()
            .map(
                |((tenant_id, user_id, source, app_id, chain_id, ingress_id), (up, down, gap))| {
                    NodeInflowRow {
                        tenant_id,
                        user_id,
                        source,
                        app_id,
                        chain_id,
                        ingress_id,
                        uplink_bytes: up,
                        downlink_bytes: down,
                        has_gap: gap,
                    }
                },
            )
            .collect(),
        idle_users,
        range_limited: false,
    })
}

async fn query_windows(
    pool: &PgPool,
    actor: &AdminContext,
    node: &str,
    period: SiteUsagePeriod,
    bounds: crate::site_usage::PeriodBounds,
) -> Result<NodeInflowUsage> {
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(pool)
        .await?;
    let ttl = crate::retention::ttl_hours(pool, "usage_samples")
        .await?
        .min(crate::retention::ttl_hours(pool, "forwarding_usage_samples").await?);
    let range_limited = bounds.start < now - i64::from(ttl) * 3600;
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout='10s'")
        .execute(&mut *tx)
        .await?;
    // A rule's first forwarding reading has no window start; the ledger and the machine outflow
    // charge it at observation time, so the inflow must not drop it from the same range.
    let records=sqlx::query("WITH entries AS (
        SELECT s.tenant_id,s.user_id,s.app_id,coalesce(g.bindings->s.grant_label->>'chain_id',r.chain_id) AS chain_id,s.ingress_id,s.uplink_bytes,s.downlink_bytes,s.has_gap
        FROM usage_samples s
        LEFT JOIN usage_generations g ON g.id=s.generation_id
        LEFT JOIN usage_user_ingress_rollups r ON r.node_id=s.node_id AND r.tenant_id=s.tenant_id AND r.user_id=s.user_id AND r.ingress_id=s.ingress_id
          AND r.period_start=date_trunc('day',s.window_start AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong'
        WHERE s.node_id=$1 AND s.window_start>=to_timestamp($2::bigint) AND s.window_start<to_timestamp($3::bigint)
          AND ($4::text IS NULL OR s.tenant_id=$4 OR s.tenant_id LIKE $5 ESCAPE '\\')
        ) SELECT tenant_id,user_id,'chain' AS source,app_id,chain_id,CASE WHEN chain_id IS NULL THEN ingress_id END AS ingress_id,
        sum(uplink_bytes)::bigint AS up,sum(downlink_bytes)::bigint AS down,bool_or(has_gap) AS gap FROM entries GROUP BY 1,2,4,5,6
        UNION ALL SELECT tenant_id,user_id,'forwarding',NULL,NULL,NULL,sum(uplink_bytes)::bigint,sum(downlink_bytes)::bigint,bool_or(has_gap)
        FROM forwarding_usage_samples WHERE node_id=$1 AND coalesce(window_start,read_at)>=to_timestamp($2::bigint) AND coalesce(window_start,read_at)<to_timestamp($3::bigint)
          AND ($4::text IS NULL OR tenant_id=$4 OR tenant_id LIKE $5 ESCAPE '\\') GROUP BY 1,2")
        .bind(node).bind(bounds.start).bind(bounds.now).bind(actor.tenant_scope()).bind(actor.tenant_scope_like_pattern()).fetch_all(&mut *tx).await?;
    let rows = records
        .into_iter()
        .map(|r| {
            Ok(NodeInflowRow {
                tenant_id: r.try_get("tenant_id")?,
                user_id: r.try_get("user_id")?,
                source: if r.get::<String, _>("source") == "chain" {
                    NodeInflowSource::Chain
                } else {
                    NodeInflowSource::Forwarding
                },
                app_id: r.try_get("app_id")?,
                chain_id: r.try_get("chain_id")?,
                ingress_id: r.try_get("ingress_id")?,
                uplink_bytes: bytes(r.try_get("up")?)?,
                downlink_bytes: bytes(r.try_get("down")?)?,
                has_gap: range_limited || r.try_get::<bool, _>("gap")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let latest=sqlx::query_scalar("SELECT greatest((SELECT extract(epoch FROM max(window_end))::bigint FROM usage_samples WHERE node_id=$1),(SELECT extract(epoch FROM max(read_at))::bigint FROM forwarding_usage_samples WHERE node_id=$1))")
        .bind(node).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(NodeInflowUsage {
        period,
        range_start_unix_secs: bounds.start,
        range_end_unix_secs: bounds.now,
        period_end_unix_secs: bounds.end,
        latest_report_unix_secs: latest,
        rows,
        idle_users: Vec::new(),
        range_limited,
    })
}
