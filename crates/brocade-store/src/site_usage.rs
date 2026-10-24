//! Fixed-category observations and the independent forwarding ledger in user/machine reports.
use crate::{AdminContext, Result, StoreError};
use brocade_deployment::site_usage::{
    catalog, SiteUsagePeriod, SiteUsageReport, UserSiteUsage, UserSiteUsageRow,
    UserSiteUsageSeries, MAX_SITE_USAGE_COUNTERS, SITE_USAGE_VERSION,
};
use serde::Serialize;
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn validate(report: &SiteUsageReport) -> Result<()> {
    let mut seen = BTreeSet::new();
    if report.version != SITE_USAGE_VERSION
        || report.epoch.len() != 32
        || !report.epoch.bytes().all(|byte| byte.is_ascii_hexdigit())
        || report.counters.len() > MAX_SITE_USAGE_COUNTERS
        || report.activation_id.is_some_and(|id| id <= 0)
    {
        return Err(StoreError::InvalidData(
            "unsupported or invalid site usage report".into(),
        ));
    }
    for counter in &report.counters {
        if counter.label.is_empty()
            || counter.label.len() > 496
            || !catalog().iter().any(|site| site.id == counter.site_id)
            || counter.uplink_bytes > i64::MAX as u64
            || counter.downlink_bytes > i64::MAX as u64
            || !seen.insert((&counter.label, &counter.site_id))
        {
            return Err(StoreError::InvalidData(
                "invalid or duplicate site usage counter".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) async fn record(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    generation_id: i64,
    read_at: i64,
    report: &SiteUsageReport,
    owners: &BTreeMap<String, (String, String)>,
) -> Result<()> {
    let previous = sqlx::query("SELECT epoch,activation_id,generation_id,read_at,baseline_at FROM node_site_usage_state WHERE node_id=$1")
        .bind(node_id).fetch_optional(&mut **tx).await?;
    if previous
        .as_ref()
        .is_some_and(|row| row.get::<i64, _>("read_at") >= read_at)
    {
        return Ok(()); // Delayed generations cannot move a node's observation head backwards.
    }
    let same_epoch = previous.as_ref().is_some_and(|row| {
        row.get::<String, _>("epoch") == report.epoch
            && row.get::<i64, _>("activation_id") == report.activation_id.unwrap_or(0)
    });
    let same_generation = previous
        .as_ref()
        .is_some_and(|row| row.get::<i64, _>("generation_id") == generation_id);
    let baseline_at = if same_epoch {
        previous
            .as_ref()
            .map(|row| row.get("baseline_at"))
            .unwrap_or(read_at)
    } else {
        read_at
    };
    if !same_epoch {
        sqlx::query("DELETE FROM site_usage_counter_heads WHERE node_id=$1")
            .bind(node_id)
            .execute(&mut **tx)
            .await?;
    }
    let heads = sqlx::query("SELECT label,site_id,read_at,uplink_bytes,downlink_bytes FROM site_usage_counter_heads WHERE node_id=$1")
        .bind(node_id).fetch_all(&mut **tx).await?.into_iter().map(|row| {
            ((row.get::<String,_>("label"),row.get::<String,_>("site_id")),
             (row.get::<i64,_>("read_at"),row.get::<i64,_>("uplink_bytes"),row.get::<i64,_>("downlink_bytes")))
        }).collect::<BTreeMap<_,_>>();
    let mut hourly = HourlyTotals::new();
    let (mut labels, mut sites, mut ups, mut downs) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut limited = report.limited;
    for counter in &report.counters {
        let Some((tenant_id, user_id)) = owners.get(&counter.label) else {
            continue;
        };
        let head = heads.get(&(counter.label.clone(), counter.site_id.clone()));
        let up = counter.uplink_bytes as i64;
        let down = counter.downlink_bytes as i64;
        let (start, old_up, old_down) = if let Some(head) = head {
            *head
        } else if same_epoch && same_generation {
            (
                previous
                    .as_ref()
                    .map(|row| row.get("read_at"))
                    .unwrap_or(read_at),
                0,
                0,
            )
        } else {
            (read_at, up, down)
        };
        if up < old_up || down < old_down {
            limited = true;
            continue;
        }
        // Split long offline intervals proportionally at UTC hour boundaries. Integer
        // cumulative allocation preserves every byte, including the rounding remainder.
        for (bucket, delta_up, delta_down) in
            hourly_parts(start, read_at, up - old_up, down - old_down)
        {
            if delta_up == 0 && delta_down == 0 {
                continue;
            }
            let totals = hourly
                .entry((
                    bucket,
                    tenant_id.clone(),
                    user_id.clone(),
                    counter.site_id.clone(),
                ))
                .or_default();
            totals.0 = totals.0.checked_add(delta_up).ok_or_else(|| {
                StoreError::InvalidData("site uplink aggregation overflow".into())
            })?;
            totals.1 = totals.1.checked_add(delta_down).ok_or_else(|| {
                StoreError::InvalidData("site downlink aggregation overflow".into())
            })?;
        }
        if hourly.len() >= 1024 {
            flush_hourly(tx, node_id, &mut hourly).await?;
        }
        labels.push(counter.label.as_str());
        sites.push(counter.site_id.as_str());
        ups.push(up);
        downs.push(down);
    }
    flush_hourly(tx, node_id, &mut hourly).await?;
    if !labels.is_empty() {
        sqlx::query("INSERT INTO site_usage_counter_heads (node_id,label,site_id,read_at,uplink_bytes,downlink_bytes)
            SELECT $1,x.label,x.site_id,$4,x.up,x.down FROM unnest($2::text[],$3::text[],$5::bigint[],$6::bigint[]) AS x(label,site_id,up,down)
            ON CONFLICT (node_id,label,site_id) DO UPDATE SET read_at=EXCLUDED.read_at,uplink_bytes=EXCLUDED.uplink_bytes,downlink_bytes=EXCLUDED.downlink_bytes")
            .bind(node_id).bind(&labels).bind(&sites).bind(read_at).bind(&ups).bind(&downs).execute(&mut **tx).await?;
    }
    sqlx::query("INSERT INTO node_site_usage_state (node_id,epoch,generation_id,read_at,baseline_at,limited,activation_id) VALUES ($1,$2,$3,$4,$5,$6,$7)
        ON CONFLICT (node_id) DO UPDATE SET epoch=EXCLUDED.epoch,generation_id=EXCLUDED.generation_id,
        read_at=EXCLUDED.read_at,baseline_at=EXCLUDED.baseline_at,limited=EXCLUDED.limited,activation_id=EXCLUDED.activation_id")
        .bind(node_id).bind(&report.epoch).bind(generation_id).bind(read_at).bind(baseline_at).bind(limited).bind(report.activation_id.unwrap_or(0)).execute(&mut **tx).await?;
    Ok(())
}

type HourlyTotals = BTreeMap<(i64, String, String, String), (i64, i64)>;

async fn flush_hourly(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    rows: &mut HourlyTotals,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut buckets = Vec::new();
    let mut tenants = Vec::new();
    let mut users = Vec::new();
    let mut sites = Vec::new();
    let mut ups = Vec::new();
    let mut downs = Vec::new();
    for ((bucket, tenant, user, site), (up, down)) in rows.iter() {
        buckets.push(*bucket);
        tenants.push(tenant.as_str());
        users.push(user.as_str());
        sites.push(site.as_str());
        ups.push(*up);
        downs.push(*down);
    }
    sqlx::query("INSERT INTO user_site_usage_hourly (bucket,node_id,tenant_id,user_id,site_id,uplink_bytes,downlink_bytes)
        SELECT x.bucket,$1,x.tenant,x.user_id,x.site,x.up,x.down FROM unnest($2::bigint[],$3::text[],$4::text[],$5::text[],$6::bigint[],$7::bigint[]) AS x(bucket,tenant,user_id,site,up,down)
        ON CONFLICT (bucket,node_id,tenant_id,user_id,site_id) DO UPDATE SET uplink_bytes=user_site_usage_hourly.uplink_bytes+EXCLUDED.uplink_bytes,downlink_bytes=user_site_usage_hourly.downlink_bytes+EXCLUDED.downlink_bytes")
        .bind(node_id).bind(&buckets).bind(&tenants).bind(&users).bind(&sites).bind(&ups).bind(&downs).execute(&mut **tx).await?;
    rows.clear();
    Ok(())
}

fn hourly_parts(start: i64, end: i64, up: i64, down: i64) -> Vec<(i64, i64, i64)> {
    if end <= start || (up == 0 && down == 0) {
        return Vec::new();
    }
    // The longest view is a calendar month (at most 31 days). Older traffic is not misdated into a
    // current bucket.
    let mut cursor = start.max((end / 3600 - 31 * 24) * 3600);
    let mut result = Vec::new();
    let allocated = |bytes: i64, at: i64| {
        ((i128::from(bytes) * i128::from(at - start)) / i128::from(end - start)) as i64
    };
    while cursor < end {
        let next = ((cursor / 3600 + 1) * 3600).min(end);
        result.push((
            cursor / 3600 * 3600,
            allocated(up, next) - allocated(up, cursor),
            allocated(down, next) - allocated(down, cursor),
        ));
        cursor = next;
    }
    result
}

/// Start, end and query time of a calendar period, all read from the database clock.
pub(crate) struct PeriodBounds {
    pub(crate) clock: i64,
    pub(crate) now: i64,
    pub(crate) start: i64,
    pub(crate) end: i64,
}

pub(crate) async fn range_bounds(
    pool: &PgPool,
    period: SiteUsagePeriod,
    range: Option<brocade_deployment::source_traffic::TrafficTimeRange>,
) -> Result<PeriodBounds> {
    let mut bounds = period_bounds(pool, period).await?;
    if let Some(range) = range {
        let range = range
            .validate(bounds.now)
            .map_err(|e| StoreError::InvalidData(e.into()))?;
        bounds.start = range.start_unix_secs;
        bounds.now = range.end_unix_secs;
        bounds.end = range.end_unix_secs;
    }
    Ok(bounds)
}

/// Calendar boundaries follow monthly usage and quotas: `date_trunc` at Asia/Hong_Kong (+08, no
/// daylight saving). Weeks are ISO weeks and start on Monday.
pub(crate) async fn period_bounds(pool: &PgPool, period: SiteUsagePeriod) -> Result<PeriodBounds> {
    let row = sqlx::query(
        "SELECT extract(epoch FROM now())::bigint AS now,
            extract(epoch FROM (date_trunc($1, now() AT TIME ZONE 'Asia/Hong_Kong')
                AT TIME ZONE 'Asia/Hong_Kong'))::bigint AS start_at,
            extract(epoch FROM ((date_trunc($1, now() AT TIME ZONE 'Asia/Hong_Kong')
                + ('1 ' || $1)::interval) AT TIME ZONE 'Asia/Hong_Kong'))::bigint AS end_at",
    )
    .bind(period.calendar_unit())
    .fetch_one(pool)
    .await?;
    Ok(PeriodBounds {
        clock: row.try_get("now")?,
        now: row.try_get("now")?,
        start: row.try_get("start_at")?,
        end: row.try_get("end_at")?,
    })
}

/// Destination observations are persisted hourly; do not prorate an hour into minute estimates.
pub(crate) async fn destination_bounds(
    pool: &PgPool,
    period: SiteUsagePeriod,
    range: Option<brocade_deployment::source_traffic::TrafficTimeRange>,
) -> Result<PeriodBounds> {
    let mut bounds = range_bounds(pool, period, range).await?;
    if range.is_some() {
        bounds.start = bounds.start / 3600 * 3600;
        bounds.end = (bounds.now + 3599) / 3600 * 3600;
        bounds.now = bounds.end.min(bounds.clock);
    }
    Ok(bounds)
}

fn site_name(id: &str) -> Result<String> {
    Ok(catalog()
        .iter()
        .find(|site| site.id == id)
        .ok_or_else(|| StoreError::InvalidData("unknown persisted site category".into()))?
        .name
        .clone())
}

pub(crate) async fn query(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    period: SiteUsagePeriod,
) -> Result<UserSiteUsage> {
    // This also checks tenant scope and user existence. Visitor and ordinary user roles
    // must not obtain even the fixed-category browsing profile.
    let targets = crate::usage::user_realtime_targets(pool, actor, tenant_id, user_id).await?;
    let nodes: Vec<_> = targets
        .into_iter()
        .filter(|target| !target.labels.is_empty())
        .map(|target| target.node_id)
        .collect();
    let bounds = period_bounds(pool, period).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    // Stored hours regrouped into chart buckets that start at the period start: whole hours for a
    // day or a week, +08 calendar days for a month. Window totals share the same SQL snapshot so
    // concurrent usage reports cannot make the ranking disagree with the chart.
    let points = sqlx::query("SELECT site_id, $3 + (bucket - $3) / $5 * $5 AS slot, sum(uplink_bytes)::bigint AS up, sum(downlink_bytes)::bigint AS down,
        sum(sum(uplink_bytes)) OVER (PARTITION BY site_id)::bigint AS total_up,
        sum(sum(downlink_bytes)) OVER (PARTITION BY site_id)::bigint AS total_down
        FROM user_site_usage_hourly WHERE tenant_id=$1 AND user_id=$2 AND bucket >= $3 AND bucket <= $4
        GROUP BY site_id, slot ORDER BY site_id, slot")
        .bind(tenant_id).bind(user_id).bind(bounds.start).bind(bounds.now).bind(i64::from(period.bucket_secs())).fetch_all(&mut *tx).await?;
    // Historical ownership survives rule deletion, revocation and moves between machines.
    let (forwarding_up, forwarding_down): (i64, i64) = sqlx::query_as(
        "SELECT coalesce(sum(uplink_bytes),0)::bigint,coalesce(sum(downlink_bytes),0)::bigint
           FROM forwarding_usage_rollups WHERE tenant_id=$1 AND user_id=$2
            AND period_start>=to_timestamp($3::bigint) AND period_start<to_timestamp($4::bigint)",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(bounds.start)
    .bind(bounds.now)
    .fetch_one(&mut *tx)
    .await?;
    let forwarding_points = forwarding_series_tx(
        &mut tx,
        ForwardingScope::User { tenant_id, user_id },
        &bounds,
        period,
    )
    .await?;
    tx.commit().await?;
    let mut usage = assemble(pool, &nodes, &bounds, period, points).await?;
    prepend_forwarding(
        &mut usage,
        forwarding_up as u64,
        forwarding_down as u64,
        forwarding_points,
    );
    Ok(usage)
}

enum ForwardingScope<'a> {
    User {
        tenant_id: &'a str,
        user_id: &'a str,
    },
    Node {
        node_id: &'a str,
        actor: &'a AdminContext,
    },
}

/// Retained daily totals must never be spread into invented hourly measurements.
async fn forwarding_series_tx(
    tx: &mut Transaction<'_, Postgres>,
    scope: ForwardingScope<'_>,
    bounds: &PeriodBounds,
    period: SiteUsagePeriod,
) -> Result<Vec<(i64, u64, u64)>> {
    // All identifiers/expressions are private constants; scope and time values remain binds.
    let (table, time, slot) = if period == SiteUsagePeriod::Month {
        (
            "forwarding_usage_rollups",
            "period_start",
            "extract(epoch FROM period_start)::bigint",
        )
    } else {
        (
            "forwarding_usage_samples",
            "coalesce(window_start,read_at)",
            "(floor(extract(epoch FROM coalesce(window_start,read_at))/3600)*3600)::bigint",
        )
    };
    // Keep node and user keys as direct predicates so their accounting indexes remain usable.
    let filter = match &scope {
        ForwardingScope::User { .. } => "tenant_id=$1 AND user_id=$2",
        ForwardingScope::Node { .. } => {
            "node_id=$1 AND ($2::text IS NULL OR tenant_id=$2 OR tenant_id LIKE $5 ESCAPE '\\')"
        }
    };
    let sql = format!(
        "SELECT {slot} AS slot,sum(uplink_bytes)::bigint AS up,sum(downlink_bytes)::bigint AS down
           FROM {table} WHERE {filter}
            AND {time}>=to_timestamp($3::bigint) AND {time}<to_timestamp($4::bigint)
          GROUP BY slot ORDER BY slot"
    );
    let query = sqlx::query(&sql);
    let rows = match scope {
        ForwardingScope::User { tenant_id, user_id } => {
            query
                .bind(tenant_id)
                .bind(user_id)
                .bind(bounds.start)
                .bind(bounds.now)
                .fetch_all(&mut **tx)
                .await?
        }
        ForwardingScope::Node { node_id, actor } => {
            query
                .bind(node_id)
                .bind(actor.tenant_scope())
                .bind(bounds.start)
                .bind(bounds.now)
                .bind(actor.tenant_scope_like_pattern())
                .fetch_all(&mut **tx)
                .await?
        }
    };
    rows.into_iter()
        .map(|row| {
            Ok((
                row.try_get("slot")?,
                row.try_get::<i64, _>("up")? as u64,
                row.try_get::<i64, _>("down")? as u64,
            ))
        })
        .collect()
}

fn prepend_forwarding(usage: &mut UserSiteUsage, up: u64, down: u64, points: Vec<(i64, u64, u64)>) {
    if up == 0 && down == 0 {
        return;
    }
    usage.rows.insert(
        0,
        UserSiteUsageRow {
            site_id: "forwarding".into(),
            name: "端口转发".into(),
            uplink_bytes: up,
            downlink_bytes: down,
        },
    );
    usage.series.insert(
        0,
        UserSiteUsageSeries {
            site_id: "forwarding".into(),
            points,
        },
    );
}

/// Per-site users on one machine, most traffic first; the remainder is folded into totals.
#[derive(Debug, Serialize)]
pub struct NodeSiteUsers {
    pub site_id: String,
    pub users: Vec<NodeSiteUser>,
    pub other_users: u64,
    pub other_uplink_bytes: u64,
    pub other_downlink_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct NodeSiteUser {
    pub tenant_id: String,
    pub user_id: String,
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
}

/// A machine's outflow by destination category: every user in the actor's scope on one ingress
/// machine, plus who carries each category. Port forwarding is a separate ledger-backed category.
#[derive(Debug, Serialize)]
pub struct NodeSiteUsage {
    #[serde(flatten)]
    pub usage: UserSiteUsage,
    pub top_users: Vec<NodeSiteUsers>,
}

/// How many users each category names before folding the rest.
const NODE_SITE_TOP_USERS: usize = 4;

/// Authorizes a machine-scoped observation read and returns the machine as the expected reporter,
/// or nothing when it has no authenticated ingress (forwarding is never classified). Same boundary
/// as the machine's live user rates; the caller resolves machine visibility.
pub(crate) async fn classified_node(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
) -> Result<Vec<String>> {
    if !actor.can_view_user_realtime_traffic() {
        return Err(StoreError::Forbidden(
            "machine traffic observations require a signed-in operator".to_owned(),
        ));
    }
    let classified: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ingresses WHERE node_id=$1)")
            .bind(node_id)
            .fetch_one(pool)
            .await?;
    Ok(if classified {
        vec![node_id.to_owned()]
    } else {
        Vec::new()
    })
}

pub(crate) async fn query_node(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    period: SiteUsagePeriod,
) -> Result<NodeSiteUsage> {
    query_node_range(pool, actor, node_id, period, None).await
}

pub(crate) async fn query_node_range(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    period: SiteUsagePeriod,
    range: Option<brocade_deployment::source_traffic::TrafficTimeRange>,
) -> Result<NodeSiteUsage> {
    let period = if range.is_some() {
        SiteUsagePeriod::Day
    } else {
        period
    };
    let nodes = classified_node(pool, actor, node_id).await?;
    let bounds = destination_bounds(pool, period, range).await?;
    let scope = actor.tenant_scope();
    let pattern = actor.tenant_scope_like_pattern();
    let mut tx = pool.begin().await?;
    // The chart, the ranking and the per-site users must describe the same accounting snapshot.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let points = sqlx::query("SELECT site_id, $2 + (bucket - $2) / $4 * $4 AS slot, sum(uplink_bytes)::bigint AS up, sum(downlink_bytes)::bigint AS down,
        sum(sum(uplink_bytes)) OVER (PARTITION BY site_id)::bigint AS total_up,
        sum(sum(downlink_bytes)) OVER (PARTITION BY site_id)::bigint AS total_down
        FROM user_site_usage_hourly WHERE node_id=$1 AND bucket >= $2 AND bucket < $3
        AND ($5::text IS NULL OR tenant_id=$5 OR tenant_id LIKE $6 ESCAPE '\\')
        GROUP BY site_id, slot ORDER BY site_id, slot")
        .bind(node_id).bind(bounds.start).bind(bounds.now).bind(i64::from(period.bucket_secs()))
        .bind(scope).bind(pattern.as_deref()).fetch_all(&mut *tx).await?;
    // These identifiers are private constants. Every scope/time value remains a SQL bind.
    let (forwarding_table, forwarding_time) = if range.is_some() {
        ("forwarding_usage_samples", "coalesce(window_start,read_at)")
    } else {
        ("forwarding_usage_rollups", "period_start")
    };
    let carriers = sqlx::query(&format!("SELECT site_id, tenant_id, user_id, sum(uplink_bytes)::bigint AS up, sum(downlink_bytes)::bigint AS down
        FROM user_site_usage_hourly WHERE node_id=$1 AND bucket >= $2 AND bucket < $3
        AND ($4::text IS NULL OR tenant_id=$4 OR tenant_id LIKE $5 ESCAPE '\\')
        GROUP BY site_id, tenant_id, user_id
        UNION ALL SELECT 'forwarding', tenant_id, user_id, sum(uplink_bytes)::bigint, sum(downlink_bytes)::bigint
        FROM {forwarding_table} WHERE node_id=$1 AND {forwarding_time}>=to_timestamp($2::bigint) AND {forwarding_time}<to_timestamp($3::bigint)
        AND ($4::text IS NULL OR tenant_id=$4 OR tenant_id LIKE $5 ESCAPE '\\')
        GROUP BY tenant_id, user_id HAVING sum(uplink_bytes)>0 OR sum(downlink_bytes)>0"))
        .bind(node_id).bind(bounds.start).bind(bounds.now)
        .bind(scope).bind(pattern.as_deref()).fetch_all(&mut *tx).await?;
    let forwarding_points = forwarding_series_tx(
        &mut tx,
        ForwardingScope::Node { node_id, actor },
        &bounds,
        period,
    )
    .await?;
    tx.commit().await?;
    let mut by_site = BTreeMap::<String, Vec<NodeSiteUser>>::new();
    for row in carriers {
        by_site
            .entry(row.try_get("site_id")?)
            .or_default()
            .push(NodeSiteUser {
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
                uplink_bytes: row.try_get::<i64, _>("up")? as u64,
                downlink_bytes: row.try_get::<i64, _>("down")? as u64,
            });
    }
    let mut usage = assemble(pool, &nodes, &bounds, period, points).await?;
    if let Some(users) = by_site.get("forwarding") {
        prepend_forwarding(
            &mut usage,
            users.iter().map(|user| user.uplink_bytes).sum(),
            users.iter().map(|user| user.downlink_bytes).sum(),
            forwarding_points,
        );
    }
    let top_users = by_site
        .into_iter()
        .map(|(site_id, mut users)| {
            users.sort_by(|a, b| {
                (b.uplink_bytes + b.downlink_bytes)
                    .cmp(&(a.uplink_bytes + a.downlink_bytes))
                    .then_with(|| (&a.tenant_id, &a.user_id).cmp(&(&b.tenant_id, &b.user_id)))
            });
            let other = users.split_off(users.len().min(NODE_SITE_TOP_USERS));
            NodeSiteUsers {
                site_id,
                users,
                other_users: other.len() as u64,
                other_uplink_bytes: other.iter().map(|user| user.uplink_bytes).sum(),
                other_downlink_bytes: other.iter().map(|user| user.downlink_bytes).sum(),
            }
        })
        .collect();
    Ok(NodeSiteUsage { usage, top_users })
}

/// Rows and per-category series from regrouped points, plus the reporting state of `nodes`.
async fn assemble(
    pool: &PgPool,
    nodes: &[String],
    bounds: &PeriodBounds,
    period: SiteUsagePeriod,
    points: Vec<sqlx::postgres::PgRow>,
) -> Result<UserSiteUsage> {
    let state = sqlx::query("SELECT (count(*) FILTER (WHERE read_at >= $2))::bigint AS reporting, bool_or(limited) AS limited, max(read_at) AS latest, max(baseline_at) AS baseline
        FROM node_site_usage_state WHERE node_id=ANY($1)")
        .bind(nodes).bind(bounds.clock-300).fetch_one(pool).await?;
    let mut result = Vec::new();
    let mut series: Vec<UserSiteUsageSeries> = Vec::new();
    for row in points {
        let id: String = row.try_get("site_id")?;
        let point = (
            row.try_get::<i64, _>("slot")?,
            row.try_get::<i64, _>("up")? as u64,
            row.try_get::<i64, _>("down")? as u64,
        );
        match series.last_mut() {
            Some(last) if last.site_id == id => last.points.push(point),
            _ => {
                result.push(UserSiteUsageRow {
                    name: site_name(&id)?,
                    site_id: id.clone(),
                    uplink_bytes: row.try_get::<i64, _>("total_up")? as u64,
                    downlink_bytes: row.try_get::<i64, _>("total_down")? as u64,
                });
                series.push(UserSiteUsageSeries {
                    site_id: id,
                    points: vec![point],
                });
            }
        }
    }
    Ok(UserSiteUsage {
        collection_enabled: load_policy(pool).await?.enabled,
        catalog_version: SITE_USAGE_VERSION,
        period,
        range_start_unix_secs: bounds.start,
        range_end_unix_secs: bounds.now,
        period_end_unix_secs: bounds.end,
        bucket_secs: period.bucket_secs(),
        reporting_nodes: state.try_get::<i64, _>("reporting")? as u64,
        expected_nodes: nodes.len() as u64,
        limited: state
            .try_get::<Option<bool>, _>("limited")?
            .unwrap_or(false),
        latest_report_unix_secs: state.try_get("latest")?,
        observed_since_unix_secs: state.try_get("baseline")?,
        rows: result,
        series,
    })
}

pub(crate) async fn load_policy(
    pool: &PgPool,
) -> Result<brocade_deployment::site_usage::SiteUsagePolicy> {
    Ok(brocade_deployment::site_usage::SiteUsagePolicy {
        enabled: sqlx::query_scalar("SELECT site_usage_enabled FROM control_state WHERE id=TRUE")
            .fetch_one(pool)
            .await?,
    })
}

pub(crate) async fn update_policy(
    pool: &PgPool,
    actor: &AdminContext,
    policy: brocade_deployment::site_usage::SiteUsagePolicy,
) -> Result<brocade_deployment::site_usage::SiteUsagePolicy> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update site usage collection".into(),
        ));
    }
    sqlx::query("UPDATE control_state SET site_usage_enabled=$1 WHERE id=TRUE")
        .bind(policy.enabled)
        .execute(pool)
        .await?;
    Ok(policy)
}

pub(crate) async fn prune(pool: &PgPool) -> Result<u64> {
    prune_hours(pool, 31 * 24).await
}

pub(crate) async fn prune_hours(pool: &PgPool, retain_hours: u32) -> Result<u64> {
    crate::retention::cleanup::prune_batch(pool, "user_site_usage_hourly", retain_hours).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hour_split_preserves_bytes_and_boundaries() {
        let parts = hourly_parts(3599, 7201, 7, 11);
        assert_eq!(parts.iter().map(|p| p.1).sum::<i64>(), 7);
        assert_eq!(parts.iter().map(|p| p.2).sum::<i64>(), 11);
        assert_eq!(
            parts.iter().map(|p| p.0).collect::<Vec<_>>(),
            vec![0, 3600, 7200]
        );
        assert!(hourly_parts(100, 100, 1, 1).is_empty());
        assert!(hourly_parts(1, 40 * 86400, 1, 1).len() <= 31 * 24 + 1);
    }
}
