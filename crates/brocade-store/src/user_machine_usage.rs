//! User traffic attributed to the reporting ingress, independently of current grants.
//! This is a projection of accepted accounting windows, not a second billing path.
use std::collections::{BTreeMap, BTreeSet};

use brocade_deployment::site_usage::SiteUsagePeriod;
use serde::Serialize;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, Result, StoreError};

#[derive(Debug, Serialize)]
pub struct UserMachineUsage {
    pub period: SiteUsagePeriod,
    pub range_start_unix_secs: i64,
    pub range_end_unix_secs: i64,
    pub period_end_unix_secs: i64,
    pub bucket_secs: u32,
    pub reporting_nodes: usize,
    pub expected_nodes: usize,
    pub rows: Vec<UserMachineUsageRow>,
}

#[derive(Debug, Default, Serialize)]
pub struct UserMachineUsageRow {
    /// None is the ledger remainder for which retained data cannot establish a machine.
    pub node_id: Option<String>,
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
    pub forwarding_bytes: u64,
    pub has_gap: bool,
    pub expected: bool,
    pub latest_report_unix_secs: Option<i64>,
    /// Bucket, up, down, forwarding, explicit accounting gap, observation evidence.
    pub points: Vec<(i64, u64, u64, u64, bool, bool)>,
}

pub(crate) struct UserMachineUsageWindow {
    pub(crate) node_id: String,
    pub(crate) tenant_id: String,
    pub(crate) user_id: String,
    pub(crate) window_start_unix_secs: i64,
    pub(crate) uplink_bytes: i64,
    pub(crate) downlink_bytes: i64,
    pub(crate) has_gap: bool,
}

pub(crate) async fn record_batch(
    tx: &mut Transaction<'_, Postgres>,
    windows: &[UserMachineUsageWindow],
) -> Result<()> {
    if windows.is_empty() {
        return Ok(());
    }
    let mut tenants = Vec::with_capacity(windows.len());
    let mut users = Vec::with_capacity(windows.len());
    let mut nodes = Vec::with_capacity(windows.len());
    let mut starts = Vec::with_capacity(windows.len());
    let mut uplinks = Vec::with_capacity(windows.len());
    let mut downlinks = Vec::with_capacity(windows.len());
    let mut gaps = Vec::with_capacity(windows.len());
    for window in windows {
        tenants.push(window.tenant_id.as_str());
        users.push(window.user_id.as_str());
        nodes.push(window.node_id.as_str());
        starts.push(window.window_start_unix_secs as f64);
        uplinks.push(window.uplink_bytes);
        downlinks.push(window.downlink_bytes);
        gaps.push(window.has_gap);
    }
    sqlx::query(
        "INSERT INTO usage_user_node_rollups (
            tenant_id, user_id, node_id, period_start, uplink_bytes, downlink_bytes, has_gap
         )
         SELECT
            t.tenant_id,
            t.user_id,
            t.node_id,
            date_trunc('day', to_timestamp(t.window_start) AT TIME ZONE 'Asia/Hong_Kong')
                AT TIME ZONE 'Asia/Hong_Kong' AS period_start,
            sum(t.uplink_bytes)::bigint,
            sum(t.downlink_bytes)::bigint,
            bool_or(t.has_gap)
         FROM UNNEST(
            $1::text[],
            $2::text[],
            $3::text[],
            $4::double precision[],
            $5::bigint[],
            $6::bigint[],
            $7::boolean[]
         ) AS t(tenant_id, user_id, node_id, window_start, uplink_bytes, downlink_bytes, has_gap)
         GROUP BY 1, 2, 3, 4
         ORDER BY 1, 2, 4, 3
         ON CONFLICT (tenant_id, user_id, period_start, node_id) DO UPDATE SET
            uplink_bytes = usage_user_node_rollups.uplink_bytes + EXCLUDED.uplink_bytes,
            downlink_bytes = usage_user_node_rollups.downlink_bytes + EXCLUDED.downlink_bytes,
            has_gap = usage_user_node_rollups.has_gap OR EXCLUDED.has_gap
         WHERE EXCLUDED.uplink_bytes <> 0 OR EXCLUDED.downlink_bytes <> 0 OR EXCLUDED.has_gap",
    )
    .bind(&tenants)
    .bind(&users)
    .bind(&nodes)
    .bind(&starts)
    .bind(&uplinks)
    .bind(&downlinks)
    .bind(&gaps)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn bytes(value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| StoreError::InvalidData("negative machine usage projection".into()))
}

pub(crate) async fn query(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    period: SiteUsagePeriod,
) -> Result<UserMachineUsage> {
    // Includes ordinary-user/visitor denial, tenant scope and existence checks.
    let targets = crate::usage::user_realtime_targets(pool, actor, tenant, user).await?;
    let expected: BTreeSet<_> = targets
        .iter()
        .map(|target| target.node_id.clone())
        .collect();
    let bounds = crate::site_usage::period_bounds(pool, period).await?;
    let mut tx = pool.begin().await?;
    // Ledger totals, retained detail and projection must be read at one accounting snapshot.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let mut rows: BTreeMap<String, UserMachineUsageRow> = expected
        .iter()
        .map(|node| {
            (
                node.clone(),
                UserMachineUsageRow {
                    node_id: Some(node.clone()),
                    expected: true,
                    ..Default::default()
                },
            )
        })
        .collect();
    let daily = sqlx::query("WITH daily AS (
        SELECT node_id,period_start,uplink_bytes AS up,downlink_bytes AS down,0::bigint AS forwarding,has_gap FROM usage_user_node_rollups WHERE tenant_id=$1 AND user_id=$2
        UNION ALL SELECT node_id,period_start,uplink_bytes,downlink_bytes,uplink_bytes+downlink_bytes,has_gap FROM forwarding_usage_rollups WHERE tenant_id=$1 AND user_id=$2
        ) SELECT node_id,extract(epoch FROM period_start)::bigint AS bucket,sum(up)::bigint AS up,sum(down)::bigint AS down,sum(forwarding)::bigint AS forwarding,bool_or(has_gap) AS gap
        FROM daily WHERE period_start>=to_timestamp($3) AND period_start<=to_timestamp($4)
        GROUP BY node_id,period_start ORDER BY node_id,period_start")
        .bind(tenant).bind(user).bind(bounds.start as f64).bind(bounds.now as f64).fetch_all(&mut *tx).await?;
    for row in daily {
        let node: String = row.try_get("node_id")?;
        let entry = rows
            .entry(node.clone())
            .or_insert_with(|| UserMachineUsageRow {
                node_id: Some(node),
                ..Default::default()
            });
        let up = bytes(row.try_get("up")?)?;
        let down = bytes(row.try_get("down")?)?;
        let forwarding = bytes(row.try_get("forwarding")?)?;
        let gap: bool = row.try_get("gap")?;
        entry.uplink_bytes += up;
        entry.downlink_bytes += down;
        entry.forwarding_bytes += forwarding;
        entry.has_gap |= gap;
        if period == SiteUsagePeriod::Month {
            entry
                .points
                .push((row.try_get("bucket")?, up, down, forwarding, gap, true));
        }
    }
    if period != SiteUsagePeriod::Month {
        // Keep the existing ledger attribution: bytes belong to the window's start. A first
        // forwarding reading has no measured interval; mark that uncertainty explicitly.
        let points = sqlx::query("WITH samples AS (
            SELECT node_id,window_start AS at,uplink_bytes AS up,downlink_bytes AS down,0::bigint AS forwarding,has_gap AS gap
            FROM usage_samples WHERE tenant_id=$1 AND user_id=$2 AND app_id IS NOT NULL
            UNION ALL SELECT node_id,coalesce(window_start,read_at),uplink_bytes,downlink_bytes,uplink_bytes+downlink_bytes,has_gap OR window_start IS NULL
            FROM forwarding_usage_samples WHERE tenant_id=$1 AND user_id=$2
            ) SELECT node_id,(floor(extract(epoch FROM at)/3600)*3600)::bigint AS bucket,sum(up)::bigint AS up,sum(down)::bigint AS down,sum(forwarding)::bigint AS forwarding,bool_or(gap) AS gap
            FROM samples WHERE at>=to_timestamp($3) AND at<=to_timestamp($4) GROUP BY node_id,bucket ORDER BY node_id,bucket")
            .bind(tenant).bind(user).bind(bounds.start as f64).bind(bounds.now as f64).fetch_all(&mut *tx).await?;
        for point in points {
            let node: String = point.try_get("node_id")?;
            let entry = rows.get_mut(&node).ok_or_else(|| {
                StoreError::InvalidData(
                    "machine detail without daily projection; run compatibility backfill".into(),
                )
            })?;
            entry.points.push((
                point.try_get("bucket")?,
                bytes(point.try_get("up")?)?,
                bytes(point.try_get("down")?)?,
                bytes(point.try_get("forwarding")?)?,
                point.try_get("gap")?,
                true,
            ));
        }
    }
    // Missing user samples are often deliberately sparse zeroes. Machine-level observation
    // evidence distinguishes those from a whole missing bucket; it does not certify continuous
    // collection inside an hour. Do not infer coverage from today's grants or current heartbeat.
    let nodes: Vec<_> = rows.keys().cloned().collect();
    let observation_sql = if period == SiteUsagePeriod::Month {
        "SELECT node_id,extract(epoch FROM period_start)::bigint AS at,has_gap AS gap FROM usage_node_rollups WHERE node_id=ANY($1) AND tenant_id=$2 AND period_start>=to_timestamp($3) AND period_start<=to_timestamp($4)
         UNION ALL SELECT node_id,extract(epoch FROM period_start)::bigint,has_gap FROM forwarding_usage_rollups WHERE node_id=ANY($1) AND tenant_id=$2 AND user_id=$5 AND period_start>=to_timestamp($3) AND period_start<=to_timestamp($4)"
    } else {
        "SELECT node_id,extract(epoch FROM window_end)::bigint AS at,false AS gap FROM usage_node_windows WHERE node_id=ANY($1) AND tenant_id=$2 AND window_end>=to_timestamp($3) AND window_end<=to_timestamp($4)
         UNION ALL SELECT node_id,extract(epoch FROM window_end)::bigint,has_gap FROM forwarding_usage_windows WHERE node_id=ANY($1) AND tenant_id=$2 AND window_end>=to_timestamp($3) AND window_end<=to_timestamp($4) AND $5::text IS NOT NULL"
    };
    let observation_sql = format!("SELECT node_id,$3::double precision::bigint+(at-$3::double precision::bigint)/$6::bigint*$6::bigint AS at,bool_or(gap) AS gap FROM ({observation_sql}) observations GROUP BY node_id,2");
    let observed = sqlx::query(&observation_sql)
        .bind(&nodes)
        .bind(tenant)
        .bind(bounds.start as f64)
        .bind(bounds.now as f64)
        .bind(user)
        .bind(i64::from(period.bucket_secs()))
        .fetch_all(&mut *tx)
        .await?;
    let step = i64::from(period.bucket_secs());
    let mut coverage = BTreeMap::<(String, i64), bool>::new();
    for row in observed {
        let at: i64 = row.try_get("at")?;
        let slot = bounds.start + (at - bounds.start) / step * step;
        *coverage.entry((row.try_get("node_id")?, slot)).or_default() |=
            row.try_get::<bool, _>("gap")?;
    }
    let reports = sqlx::query("SELECT node_id,max(at)::bigint AS at FROM (
        SELECT node_id,extract(epoch FROM read_at)::bigint AS at FROM usage_report_receipts WHERE node_id=ANY($1)
        UNION ALL SELECT node_id,extract(epoch FROM read_at)::bigint FROM forwarding_usage_counter_heads WHERE node_id=ANY($1)
        ) r GROUP BY node_id").bind(&nodes).fetch_all(&mut *tx).await?;
    for row in reports {
        if let Some(entry) = rows.get_mut(&row.try_get::<String, _>("node_id")?) {
            entry.latest_report_unix_secs = row.try_get("at")?;
        }
    }
    for (node, entry) in &mut rows {
        let mut points: BTreeMap<_, _> = entry
            .points
            .drain(..)
            .map(|point| (point.0, point))
            .collect();
        for at in (bounds.start..bounds.now).step_by(step as usize) {
            let observation = coverage.get(&(node.clone(), at));
            let point = points.entry(at).or_insert((at, 0, 0, 0, false, false));
            point.4 |= observation.copied().unwrap_or(false);
            point.5 |= observation.is_some();
        }
        entry.points = points.into_values().collect();
    }
    // Compare daily facts, never spread a legacy monthly remainder across machines or hours.
    let remainder = sqlx::query("WITH ledger AS (
        SELECT period_start,sum(uplink_bytes)::bigint AS up,sum(downlink_bytes)::bigint AS down FROM usage_rollups WHERE tenant_id=$1 AND user_id=$2 AND period_start>=to_timestamp($3) AND period_start<=to_timestamp($4) GROUP BY period_start
        ), known AS (
        SELECT period_start,sum(uplink_bytes)::bigint AS up,sum(downlink_bytes)::bigint AS down FROM usage_user_node_rollups WHERE tenant_id=$1 AND user_id=$2 AND period_start>=to_timestamp($3) AND period_start<=to_timestamp($4) GROUP BY period_start
        ) SELECT extract(epoch FROM l.period_start)::bigint AS bucket,l.up-coalesce(k.up,0) AS up,l.down-coalesce(k.down,0) AS down FROM ledger l LEFT JOIN known k USING(period_start) ORDER BY l.period_start")
        .bind(tenant).bind(user).bind(bounds.start as f64).bind(bounds.now as f64).fetch_all(&mut *tx).await?;
    let mut unassigned = UserMachineUsageRow::default();
    for row in remainder {
        let up = bytes(row.try_get("up")?)?;
        let down = bytes(row.try_get("down")?)?;
        unassigned.uplink_bytes += up;
        unassigned.downlink_bytes += down;
        if period == SiteUsagePeriod::Month && up + down > 0 {
            unassigned
                .points
                .push((row.try_get("bucket")?, up, down, 0, false, true));
        }
    }
    let reporting_nodes = rows
        .values()
        .filter(|row| {
            row.expected
                && row
                    .latest_report_unix_secs
                    .is_some_and(|at| at >= bounds.now - 300)
        })
        .count();
    let mut rows: Vec<_> = rows.into_values().collect();
    rows.sort_by(|a, b| {
        (b.uplink_bytes + b.downlink_bytes)
            .cmp(&(a.uplink_bytes + a.downlink_bytes))
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    if unassigned.uplink_bytes + unassigned.downlink_bytes > 0 {
        rows.push(unassigned);
    }
    tx.commit().await?;
    Ok(UserMachineUsage {
        period,
        range_start_unix_secs: bounds.start,
        range_end_unix_secs: bounds.now,
        period_end_unix_secs: bounds.end,
        bucket_secs: period.bucket_secs(),
        reporting_nodes,
        expected_nodes: expected.len(),
        rows,
    })
}
