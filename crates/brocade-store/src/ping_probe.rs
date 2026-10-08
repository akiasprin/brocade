//! Active TCP-connect and ICMP-echo observation.
//!
//! One shared settings list may carry TCP and ICMP targets at the same time. Every target has an
//! IPv4 and an IPv6 endpoint, either optional, and each configured endpoint is its own series. A
//! round records only when it ran, which series it addressed, whether a wire measurement was
//! possible, an optional latency, and a coarse reason when no measurement was possible. DNS and
//! local diagnostics never become historical metrics.

use std::collections::{BTreeMap, BTreeSet};

use brocade_deployment::protocol::{
    LegacyPingProbeTarget, NodePingProbeLatestList, NodePingProbeLatestView, NodePingProbeList,
    NodePingProbeView, PingEndpointError, PingProbeEndpoint, PingProbeFamily,
    PingProbeFamilyLatest, PingProbeFamilySeries, PingProbeKind, PingProbePoint,
    PingProbeReportRequest, PingProbeReportResult, PingProbeSettings, PingProbeSkipReason,
    PingProbeTarget, PingProbeTargetLatest, PingProbeTargetSeries,
};
use sqlx::{PgPool, Row};

use crate::{admin::tenant_filter, AdminContext, Result, StoreError};

// The primary key is (series_id, probed_at). Ask for each requested series
// explicitly so each LATERAL arm is a bounded reverse index scan. DISTINCT ON would sort the
// entire retained table to produce the same handful of rows.
const LATEST_NODE_SAMPLES_SQL: &str =
    "SELECT requested_node.node_id, requested.target, requested.family,
            extract(epoch FROM latest.probed_at)::bigint AS probed_at,
            latest.attempted, latest.latency_us, latest.skip_reason
       FROM unnest($1::text[]) AS requested_node(node_id)
       CROSS JOIN unnest($2::text[], $3::text[]) AS requested(target, family)
       JOIN node_ping_probe_series series
         ON series.node_id = requested_node.node_id
        AND series.target = requested.target AND series.family = requested.family
       JOIN LATERAL (
            SELECT sample.probed_at, sample.attempted, sample.latency_us, sample.skip_reason
              FROM node_ping_probe_samples sample
             WHERE sample.series_id = series.id
             ORDER BY sample.probed_at DESC
             LIMIT 1
       ) latest ON TRUE
      ORDER BY requested_node.node_id, requested.target, requested.family";

const MAX_CLOCK_SKEW_SECS: i64 = 600;
const MAX_TARGETS: usize = 32;
/// One sample per configured family: at most two per target.
const MAX_SAMPLES: usize = MAX_TARGETS * 2;
const MIN_INTERVAL_SECS: u32 = 5;
const MAX_INTERVAL_SECS: u32 = 86_400;
const MIN_TIMEOUT_MS: u32 = 1;
const MAX_TIMEOUT_MS: u32 = 120_000;
const MAX_NAME_CHARS: usize = 64;
const MAX_READ_WINDOW_SECS: u32 = 7 * 86_400;

/// The chart reads every retained PING point, but repeated object keys account for most of the
/// row-oriented JSON. Parallel arrays preserve the exact samples while writing the target
/// metadata and field names once per series.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NodePingProbeColumnarView {
    pub node_id: String,
    pub targets: Vec<PingProbeTargetColumnarSeries>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PingProbeTargetColumnarSeries {
    pub name: String,
    pub kind: PingProbeKind,
    pub ipv4: Option<PingProbeFamilyColumns>,
    pub ipv6: Option<PingProbeFamilyColumns>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PingProbeFamilyColumns {
    pub address: String,
    pub probed_at_unix_secs: Vec<i64>,
    pub attempted: Vec<bool>,
    pub latency_us: Vec<Option<u32>>,
    pub skip_reason: Vec<Option<PingProbeSkipReason>>,
}

fn family_columns(series: PingProbeFamilySeries) -> PingProbeFamilyColumns {
    let mut columns = PingProbeFamilyColumns {
        address: series.address,
        probed_at_unix_secs: Vec::with_capacity(series.samples.len()),
        attempted: Vec::with_capacity(series.samples.len()),
        latency_us: Vec::with_capacity(series.samples.len()),
        skip_reason: Vec::with_capacity(series.samples.len()),
    };
    for sample in series.samples {
        columns.probed_at_unix_secs.push(sample.probed_at_unix_secs);
        columns.attempted.push(sample.attempted);
        columns.latency_us.push(sample.latency_us);
        columns.skip_reason.push(sample.skip_reason);
    }
    columns
}

pub fn columnar_view(view: NodePingProbeView) -> NodePingProbeColumnarView {
    NodePingProbeColumnarView {
        node_id: view.node_id,
        targets: view
            .targets
            .into_iter()
            .map(|target| PingProbeTargetColumnarSeries {
                name: target.name,
                kind: target.kind,
                ipv4: target.ipv4.map(family_columns),
                ipv6: target.ipv6.map(family_columns),
            })
            .collect(),
    }
}

pub async fn load_settings(pool: &PgPool) -> Result<PingProbeSettings> {
    let row = sqlx::query(
        "SELECT ping_probe_targets, ping_probe_interval_secs, ping_probe_timeout_ms
           FROM control_state WHERE id = TRUE",
    )
    .fetch_one(pool)
    .await?;
    let targets: serde_json::Value = row.try_get("ping_probe_targets")?;
    let settings = PingProbeSettings {
        targets: stored_targets(targets)?,
        interval_secs: u32::try_from(row.try_get::<i32, _>("ping_probe_interval_secs")?).map_err(
            |_| StoreError::InvalidData("negative PING probe interval in database".into()),
        )?,
        timeout_ms: u32::try_from(row.try_get::<i32, _>("ping_probe_timeout_ms")?).map_err(
            |_| StoreError::InvalidData("negative PING probe timeout in database".into()),
        )?,
    };
    validate_settings(&settings)?;
    Ok(settings)
}

/// Read the stored target list. Lists saved before dual-stack probing hold one URI per target;
/// they are converted on read (`PingProbeTarget::from_legacy`) and rewritten in the current shape
/// on the next save. A domain URI becomes a target with both families. Should two legacy URIs
/// canonicalize to the same series, the later copy of that family is dropped so the list stays
/// valid; a target left with no family is dropped with it.
fn stored_targets(value: serde_json::Value) -> Result<Vec<PingProbeTarget>> {
    if let Ok(targets) = serde_json::from_value::<Vec<PingProbeTarget>>(value.clone()) {
        return Ok(targets);
    }
    let legacy: Vec<LegacyPingProbeTarget> = serde_json::from_value(value)?;
    let mut seen = BTreeSet::new();
    let mut targets = Vec::with_capacity(legacy.len());
    for entry in &legacy {
        let mut target = PingProbeTarget::from_legacy(entry).ok_or_else(|| {
            StoreError::InvalidData(format!(
                "stored PING probe target is not a tcp:// or icmp:// address: {}",
                entry.address
            ))
        })?;
        for family in PingProbeFamily::ALL {
            let Some(address) = target.series_address(family) else {
                continue;
            };
            if !seen.insert((address, family)) {
                match family {
                    PingProbeFamily::Ipv4 => target.ipv4 = None,
                    PingProbeFamily::Ipv6 => target.ipv6 = None,
                }
            }
        }
        if target.ipv4.is_some() || target.ipv6.is_some() {
            targets.push(target);
        }
    }
    Ok(targets)
}

pub async fn update_settings(
    pool: &PgPool,
    actor: &AdminContext,
    settings: PingProbeSettings,
) -> Result<PingProbeSettings> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update PING probe settings".to_owned(),
        ));
    }
    let settings = normalize_settings(settings);
    validate_settings(&settings)?;
    sqlx::query(
        "UPDATE control_state
            SET ping_probe_targets = $1,
                ping_probe_interval_secs = $2,
                ping_probe_timeout_ms = $3
          WHERE id = TRUE",
    )
    .bind(serde_json::to_value(&settings.targets)?)
    .bind(i32::try_from(settings.interval_secs).expect("validated interval fits i32"))
    .bind(i32::try_from(settings.timeout_ms).expect("validated timeout fits i32"))
    .execute(pool)
    .await?;
    Ok(settings)
}

/// Every configured series of the current settings, keyed by (series address, family).
fn configured_series(settings: &PingProbeSettings) -> BTreeSet<(String, PingProbeFamily)> {
    settings
        .targets
        .iter()
        .flat_map(|target| {
            PingProbeFamily::ALL
                .into_iter()
                .filter_map(|family| Some((target.series_address(family)?, family)))
        })
        .collect()
}

pub async fn record_report(
    pool: &PgPool,
    node_id: &str,
    request: PingProbeReportRequest,
) -> Result<PingProbeReportResult> {
    if request.probed_at_unix_secs <= 0 {
        return Err(StoreError::InvalidData(
            "PING probe timestamp must be positive unix seconds".to_owned(),
        ));
    }
    if request.samples.len() > MAX_SAMPLES {
        return Err(StoreError::InvalidData(format!(
            "PING probe carries {} samples, over the {MAX_SAMPLES} limit",
            request.samples.len()
        )));
    }
    let (skew_secs,): (i64,) =
        sqlx::query_as("SELECT $1::bigint - extract(epoch FROM now())::bigint")
            .bind(request.probed_at_unix_secs)
            .fetch_one(pool)
            .await?;
    if skew_secs.abs() > MAX_CLOCK_SKEW_SECS {
        return Err(StoreError::InvalidData(format!(
            "PING probe clock skew {}s exceeds {MAX_CLOCK_SKEW_SECS}s; check the node's clock",
            skew_secs.abs()
        )));
    }

    let settings = load_settings(pool).await?;
    let known = configured_series(&settings);
    let mut seen = BTreeSet::new();
    let mut skipped_samples = 0;
    let mut unknown_targets = 0;
    let mut validated = Vec::with_capacity(request.samples.len());

    for sample in request.samples {
        // Agents older than the dual-stack protocol send no family. Their sample is attributable
        // only when the address is an IP literal; a domain may have resolved to either family.
        let Some(family) = sample
            .family
            .or_else(|| PingProbeFamily::of_series_address(&sample.target))
        else {
            unknown_targets += 1;
            continue;
        };
        let series = (sample.target, family);
        if !known.contains(&series) {
            unknown_targets += 1;
            continue;
        }
        if !seen.insert(series.clone()) {
            skipped_samples += 1;
            continue;
        }
        let (target, family) = series;
        if !sample.attempted && sample.latency_us.is_some() {
            return Err(StoreError::InvalidData(format!(
                "未执行的 PING 探测不能携带延迟：{target}"
            )));
        }
        if sample.attempted && sample.skip_reason.is_some() {
            return Err(StoreError::InvalidData(format!(
                "已执行的 PING 探测不能携带未探测原因：{target}"
            )));
        }
        let attempted = sample.attempted;
        let latency_us = successful_latency_us(sample.latency_us, attempted, settings.timeout_ms)
            .map(|value| {
                i32::try_from(value).map_err(|_| {
                    StoreError::InvalidData("PING latency exceeds storage range".into())
                })
            })
            .transpose()?;
        validated.push((
            target,
            family.as_str(),
            attempted,
            latency_us,
            sample.skip_reason.map(PingProbeSkipReason::as_str),
        ));
    }

    // Canonical lock order avoids concurrent reports inserting the same new series in opposite
    // orders. Two statements deliberately avoid the ON CONFLICT/CTE snapshot-visibility trap.
    validated.sort_unstable_by(|left, right| (&left.0, left.1).cmp(&(&right.0, right.1)));
    let targets: Vec<_> = validated.iter().map(|sample| sample.0.as_str()).collect();
    let families: Vec<_> = validated.iter().map(|sample| sample.1).collect();
    let attempted: Vec<_> = validated.iter().map(|sample| sample.2).collect();
    let latencies: Vec<_> = validated.iter().map(|sample| sample.3).collect();
    let reasons: Vec<_> = validated.iter().map(|sample| sample.4).collect();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO node_ping_probe_series (node_id, target, family)
         SELECT $1, target, family FROM unnest($2::text[], $3::text[]) AS input(target, family)
         ORDER BY target, family
         ON CONFLICT (node_id, target, family) DO NOTHING",
    )
    .bind(node_id)
    .bind(&targets)
    .bind(&families)
    .execute(&mut *tx)
    .await?;
    let accepted_samples = sqlx::query(
        "INSERT INTO node_ping_probe_samples (series_id, probed_at, attempted, latency_us, skip_reason)
         SELECT series.id, to_timestamp($2), input.attempted, input.latency_us, input.skip_reason
           FROM unnest($3::text[], $4::text[], $5::bool[], $6::int[], $7::text[])
                AS input(target, family, attempted, latency_us, skip_reason)
           JOIN node_ping_probe_series series
             ON series.node_id = $1 AND series.target = input.target AND series.family = input.family
          ORDER BY series.id
         ON CONFLICT (series_id, probed_at) DO NOTHING",
    ).bind(node_id).bind(request.probed_at_unix_secs).bind(&targets).bind(&families)
        .bind(&attempted).bind(&latencies).bind(&reasons).execute(&mut *tx).await?.rows_affected();
    skipped_samples += validated.len() as u64 - accepted_samples;
    tx.commit().await?;

    Ok(PingProbeReportResult {
        node_id: node_id.to_owned(),
        accepted_samples,
        skipped_samples,
        unknown_targets,
    })
}

/// Drop expired PING history from the control-plane maintenance loop.
///
/// This used to run after every report. At a ten-second interval that meant one global retention
/// scan per Agent report even though almost every scan deleted nothing. An hourly pass keeps the
/// same retention boundary while removing that work from the ingestion transaction.
pub async fn prune_samples(pool: &PgPool, retain_days: u32) -> Result<u64> {
    let retain_days = i32::try_from(retain_days.clamp(1, 365)).expect("retention fits i32");
    let started = std::time::Instant::now();
    let mut deleted = 0;
    // Independent small transactions release row locks and pool capacity between batches. The
    // hourly pass can remove 200k points (above current ingestion); a backlog drains across passes.
    for _ in 0..20 {
        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL statement_timeout = '5s'")
            .execute(&mut *tx)
            .await?;
        let count = sqlx::query(
            "WITH expired AS (
                SELECT ctid FROM node_ping_probe_samples
                 WHERE probed_at < now() - make_interval(days => $1)
                 ORDER BY probed_at LIMIT 10000 FOR UPDATE SKIP LOCKED
             ) DELETE FROM node_ping_probe_samples sample USING expired
                WHERE sample.ctid = expired.ctid",
        )
        .bind(retain_days)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        deleted += count;
        if count < 10_000 || started.elapsed() >= std::time::Duration::from_secs(10) {
            break;
        }
    }
    Ok(deleted)
}

pub async fn node_view(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    window_secs: u32,
) -> Result<NodePingProbeView> {
    let settings = load_settings(pool).await?;
    if !node_in_scope(pool, actor, node_id).await? {
        return Ok(series_view(
            node_id,
            &settings.targets,
            &mut BTreeMap::new(),
        ));
    }
    read_node(
        pool,
        node_id,
        &settings.targets,
        settings.timeout_ms,
        bounded_window(window_secs),
        None,
    )
    .await
}

pub async fn node_view_range(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    start_unix_secs: i64,
    end_unix_secs: i64,
) -> Result<NodePingProbeView> {
    let span = end_unix_secs
        .checked_sub(start_unix_secs)
        .ok_or_else(|| StoreError::InvalidData("invalid PING timestamp range".to_owned()))?;
    if start_unix_secs < 0 || !(1..=7 * 86_400).contains(&span) {
        return Err(StoreError::InvalidData(
            "PING range must be positive and no longer than 7 days".to_owned(),
        ));
    }
    let settings = load_settings(pool).await?;
    if !node_in_scope(pool, actor, node_id).await? {
        return Ok(series_view(
            node_id,
            &settings.targets,
            &mut BTreeMap::new(),
        ));
    }
    read_node(
        pool,
        node_id,
        &settings.targets,
        settings.timeout_ms,
        60,
        Some((start_unix_secs, end_unix_secs)),
    )
    .await
}

type SeriesPoints = BTreeMap<(String, PingProbeFamily), Vec<PingProbePoint>>;

/// Lay the configured targets over the points that were read. A configured family always gets a
/// series, empty when it has no retained points; an unconfigured family is `None`.
fn series_view(
    node_id: &str,
    targets: &[PingProbeTarget],
    points: &mut SeriesPoints,
) -> NodePingProbeView {
    let mut family_series = |target: &PingProbeTarget, family| {
        target
            .series_address(family)
            .map(|address| PingProbeFamilySeries {
                samples: points
                    .remove(&(address.clone(), family))
                    .unwrap_or_default(),
                address,
            })
    };
    NodePingProbeView {
        node_id: node_id.to_owned(),
        targets: targets
            .iter()
            .map(|target| PingProbeTargetSeries {
                name: target.name.clone(),
                kind: target.kind,
                ipv4: family_series(target, PingProbeFamily::Ipv4),
                ipv6: family_series(target, PingProbeFamily::Ipv6),
            })
            .collect(),
    }
}

pub async fn list_nodes(
    pool: &PgPool,
    actor: &AdminContext,
    window_secs: u32,
) -> Result<NodePingProbeList> {
    let settings = load_settings(pool).await?;
    let ids = scoped_live_node_ids(pool, actor).await?;
    let mut points = BTreeMap::<String, SeriesPoints>::new();
    if !ids.is_empty() {
        let rows = sqlx::query(
            "SELECT series.node_id, series.target, series.family,
                    extract(epoch FROM probed_at)::bigint AS probed_at,
                    attempted, latency_us, skip_reason
               FROM node_ping_probe_series series
               JOIN node_ping_probe_samples sample ON sample.series_id = series.id
              WHERE series.node_id = ANY($1::text[])
                AND probed_at >= now() - make_interval(secs => $2::double precision)
              ORDER BY node_id, target, family, probed_at ASC",
        )
        .bind(&ids)
        .bind(i32::try_from(bounded_window(window_secs)).expect("bounded window fits i32"))
        .fetch_all(pool)
        .await?;
        for row in rows {
            let node_id: String = row.try_get("node_id")?;
            let series = series_key(&row)?;
            points
                .entry(node_id)
                .or_default()
                .entry(series)
                .or_default()
                .push(point_from_row(&row, settings.timeout_ms)?);
        }
    }
    let nodes = ids
        .into_iter()
        .map(|node_id| {
            let mut node_points = points.remove(&node_id).unwrap_or_default();
            series_view(&node_id, &settings.targets, &mut node_points)
        })
        .collect();
    Ok(NodePingProbeList { nodes })
}

/// The machine list needs only the newest observation for each configured series. Fetching a
/// history window here used to perform one query per machine and ship every point to the browser,
/// where the whole window was reduced to one card statistic. Keep the history endpoint for a
/// machine detail chart; this path has a fixed query count and bounded response.
pub async fn list_latest_nodes(
    pool: &PgPool,
    actor: &AdminContext,
) -> Result<NodePingProbeLatestList> {
    let settings = load_settings(pool).await?;
    let ids = scoped_live_node_ids(pool, actor).await?;
    let (addresses, families): (Vec<String>, Vec<&str>) = configured_series(&settings)
        .into_iter()
        .map(|(address, family)| (address, family.as_str()))
        .unzip();
    let mut latest = BTreeMap::<(String, String, PingProbeFamily), PingProbePoint>::new();
    if !ids.is_empty() && !addresses.is_empty() {
        let rows = sqlx::query(LATEST_NODE_SAMPLES_SQL)
            .bind(&ids)
            .bind(&addresses)
            .bind(&families)
            .fetch_all(pool)
            .await?;
        for row in rows {
            let node_id: String = row.try_get("node_id")?;
            let (target, family) = series_key(&row)?;
            latest.insert(
                (node_id, target, family),
                point_from_row(&row, settings.timeout_ms)?,
            );
        }
    }
    let nodes = ids
        .into_iter()
        .map(|node_id| {
            let mut family_latest = |target: &PingProbeTarget, family| {
                target
                    .series_address(family)
                    .map(|address| PingProbeFamilyLatest {
                        latest: latest.remove(&(node_id.clone(), address.clone(), family)),
                        address,
                    })
            };
            let targets = settings
                .targets
                .iter()
                .map(|target| PingProbeTargetLatest {
                    name: target.name.clone(),
                    kind: target.kind,
                    ipv4: family_latest(target, PingProbeFamily::Ipv4),
                    ipv6: family_latest(target, PingProbeFamily::Ipv6),
                })
                .collect();
            NodePingProbeLatestView { node_id, targets }
        })
        .collect();
    Ok(NodePingProbeLatestList {
        interval_secs: settings.interval_secs,
        nodes,
    })
}

async fn read_node(
    pool: &PgPool,
    node_id: &str,
    targets: &[PingProbeTarget],
    timeout_ms: u32,
    window_secs: u32,
    absolute: Option<(i64, i64)>,
) -> Result<NodePingProbeView> {
    let rows = sqlx::query(
        "SELECT target, family, extract(epoch FROM probed_at)::bigint AS probed_at,
                attempted, latency_us, skip_reason
           FROM node_ping_probe_series series
           JOIN node_ping_probe_samples sample ON sample.series_id = series.id
          WHERE series.node_id = $1
            AND probed_at >= COALESCE(to_timestamp($3::bigint),
                now() - make_interval(secs => $2::double precision))
            AND probed_at <= COALESCE(to_timestamp($4::bigint), 'infinity'::timestamptz)
          ORDER BY probed_at ASC",
    )
    .bind(node_id)
    .bind(i32::try_from(window_secs).expect("bounded window fits i32"))
    .bind(absolute.map(|range| range.0))
    .bind(absolute.map(|range| range.1))
    .fetch_all(pool)
    .await?;
    let mut points = SeriesPoints::new();
    for row in rows {
        // Apply the current policy to historical rows too. Lowering the timeout must not leave
        // old, now-invalid latency points visible until retention expires.
        points
            .entry(series_key(&row)?)
            .or_default()
            .push(point_from_row(&row, timeout_ms)?);
    }
    Ok(series_view(node_id, targets, &mut points))
}

fn series_key(row: &sqlx::postgres::PgRow) -> Result<(String, PingProbeFamily)> {
    let target: String = row.try_get("target")?;
    let family: String = row.try_get("family")?;
    let family = PingProbeFamily::parse(&family).ok_or_else(|| {
        StoreError::InvalidData(format!("unknown PING family in database: {family}"))
    })?;
    Ok((target, family))
}

fn point_from_row(row: &sqlx::postgres::PgRow, timeout_ms: u32) -> Result<PingProbePoint> {
    let attempted: bool = row.try_get("attempted")?;
    let latency_us = successful_latency_us(
        row.try_get::<Option<i32>, _>("latency_us")?
            .map(|value| {
                u32::try_from(value).map_err(|_| {
                    StoreError::InvalidData("negative PING latency in database".into())
                })
            })
            .transpose()?,
        attempted,
        timeout_ms,
    );
    let skip_reason = row
        .try_get::<Option<String>, _>("skip_reason")?
        .map(|reason| {
            PingProbeSkipReason::parse(&reason).ok_or_else(|| {
                StoreError::InvalidData(format!("unknown PING skip reason in database: {reason}"))
            })
        })
        .transpose()?;
    Ok(PingProbePoint {
        probed_at_unix_secs: row.try_get("probed_at")?,
        attempted,
        latency_us,
        skip_reason,
    })
}

async fn scoped_live_node_ids(pool: &PgPool, actor: &AdminContext) -> Result<Vec<String>> {
    let filter = tenant_filter(actor);
    let (scope, pattern) = split_filter(&filter);
    Ok(sqlx::query_scalar(
        "SELECT id FROM nodes
         WHERE retired_at IS NULL
           AND ($1::text IS NULL OR tenant_id = $1 OR tenant_id LIKE $2 ESCAPE '\\')
         ORDER BY id",
    )
    .bind(scope)
    .bind(pattern)
    .fetch_all(pool)
    .await?)
}

async fn node_in_scope(pool: &PgPool, actor: &AdminContext, node_id: &str) -> Result<bool> {
    let filter = tenant_filter(actor);
    let (scope, pattern) = split_filter(&filter);
    let (found,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM nodes
                        WHERE id = $3 AND retired_at IS NULL
                          AND ($1::text IS NULL OR tenant_id = $1 OR tenant_id LIKE $2 ESCAPE '\\'))",
    )
    .bind(scope)
    .bind(pattern)
    .bind(node_id)
    .fetch_one(pool)
    .await?;
    Ok(found)
}

fn split_filter(filter: &Option<(String, String)>) -> (Option<&str>, Option<&str>) {
    match filter {
        Some((scope, pattern)) => (Some(scope.as_str()), Some(pattern.as_str())),
        None => (None, None),
    }
}

fn bounded_window(window_secs: u32) -> u32 {
    window_secs.clamp(60, MAX_READ_WINDOW_SECS)
}

fn successful_latency_us(latency_us: Option<u32>, attempted: bool, timeout_ms: u32) -> Option<u32> {
    let timeout_us = timeout_ms.saturating_mul(1_000);
    attempted
        .then_some(latency_us)
        .flatten()
        .filter(|value| *value <= timeout_us)
}

/// Trim names and endpoints, store an empty endpoint as an unconfigured family, and rewrite
/// parseable endpoints in canonical form so equal endpoints share one series identity.
fn normalize_settings(settings: PingProbeSettings) -> PingProbeSettings {
    let endpoint = |kind: PingProbeKind, text: Option<String>| {
        let text = text?.trim().to_owned();
        if text.is_empty() {
            return None;
        }
        Some(
            PingProbeEndpoint::parse(kind, &text)
                .map(|endpoint| endpoint.text())
                .unwrap_or(text),
        )
    };
    PingProbeSettings {
        targets: settings
            .targets
            .into_iter()
            .map(|target| PingProbeTarget {
                name: target.name.trim().to_owned(),
                ipv4: endpoint(target.kind, target.ipv4),
                ipv6: endpoint(target.kind, target.ipv6),
                kind: target.kind,
            })
            .collect(),
        interval_secs: settings.interval_secs,
        timeout_ms: settings.timeout_ms,
    }
}

fn family_label(family: PingProbeFamily) -> &'static str {
    match family {
        PingProbeFamily::Ipv4 => "IPv4",
        PingProbeFamily::Ipv6 => "IPv6",
    }
}

fn endpoint_error_text(kind: PingProbeKind, error: PingEndpointError) -> &'static str {
    match (error, kind) {
        (PingEndpointError::Empty, _) => "不能为空",
        (PingEndpointError::TooLong, _) => "过长",
        (PingEndpointError::Malformed, PingProbeKind::Tcp) => {
            "只写主机和端口，不带协议、路径或空格"
        }
        (PingEndpointError::Malformed, PingProbeKind::Icmp) => "只写主机，不带协议、路径或空格",
        (PingEndpointError::MissingPort, _) => "缺少端口，TCP 地址写作 主机:端口",
        (PingEndpointError::InvalidPort, _) => "的端口必须为 1–65535",
        (PingEndpointError::PortNotAllowed, _) => "不接受端口",
        (PingEndpointError::BracketsNotIpv6, _) => "的方括号内必须是 IPv6 地址",
        (PingEndpointError::Ipv6NeedsBrackets, _) => "的 IPv6 地址需写作 [地址]:端口",
    }
}

fn validate_settings(settings: &PingProbeSettings) -> Result<()> {
    if !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&settings.interval_secs) {
        return Err(StoreError::InvalidData(format!(
            "PING 探测间隔必须为 {MIN_INTERVAL_SECS}–{MAX_INTERVAL_SECS} 秒"
        )));
    }
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&settings.timeout_ms) {
        return Err(StoreError::InvalidData(format!(
            "PING 超时必须为 {MIN_TIMEOUT_MS}–{MAX_TIMEOUT_MS} 毫秒"
        )));
    }
    if settings.targets.len() > MAX_TARGETS {
        return Err(StoreError::InvalidData(format!(
            "PING 探测目标不能超过 {MAX_TARGETS} 个"
        )));
    }
    let mut series = BTreeSet::new();
    for target in &settings.targets {
        let name_len = target.name.chars().count();
        if name_len == 0 || name_len > MAX_NAME_CHARS || target.name.chars().any(char::is_control) {
            return Err(StoreError::InvalidData(format!(
                "PING 探测目标名称必须为 1–{MAX_NAME_CHARS} 个可见字符"
            )));
        }
        if target.ipv4.is_none() && target.ipv6.is_none() {
            return Err(StoreError::InvalidData(format!(
                "PING 探测目标「{}」至少要填写一个地址",
                target.name
            )));
        }
        for family in PingProbeFamily::ALL {
            let Some(text) = target.endpoint_text(family) else {
                continue;
            };
            let label = family_label(family);
            let endpoint = PingProbeEndpoint::parse(target.kind, text).map_err(|error| {
                StoreError::InvalidData(format!(
                    "PING 探测目标「{}」的 {label} 地址{}：{text}",
                    target.name,
                    endpoint_error_text(target.kind, error)
                ))
            })?;
            if let Some(literal) = endpoint
                .literal_family()
                .filter(|literal| *literal != family)
            {
                return Err(StoreError::InvalidData(format!(
                    "PING 探测目标「{}」的 {label} 地址填的是 {} 地址：{text}",
                    target.name,
                    family_label(literal)
                )));
            }
            let address = endpoint.series_address(target.kind);
            if !series.insert((address.clone(), family)) {
                return Err(StoreError::InvalidData(format!(
                    "PING 探测地址不能重复：{label} {address}"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(kind: PingProbeKind, ipv4: Option<&str>, ipv6: Option<&str>) -> PingProbeTarget {
        PingProbeTarget {
            name: "target".to_owned(),
            kind,
            ipv4: ipv4.map(str::to_owned),
            ipv6: ipv6.map(str::to_owned),
        }
    }

    fn settings(targets: Vec<PingProbeTarget>) -> PingProbeSettings {
        PingProbeSettings {
            targets,
            interval_secs: 60,
            timeout_ms: 420,
        }
    }

    #[test]
    fn each_family_is_validated_against_its_own_column() {
        let valid = |targets| validate_settings(&settings(targets)).is_ok();
        assert!(valid(vec![target(
            PingProbeKind::Icmp,
            Some("1.1.1.1"),
            Some("2606:4700:4700::1111")
        )]));
        assert!(valid(vec![target(
            PingProbeKind::Tcp,
            Some("www.google.com:443"),
            Some("www.google.com:443")
        )]));
        // Either family may be left empty, but not both.
        assert!(valid(vec![target(
            PingProbeKind::Icmp,
            Some("1.1.1.1"),
            None
        )]));
        assert!(valid(vec![target(
            PingProbeKind::Icmp,
            None,
            Some("ipv6.google.com")
        )]));
        assert!(!valid(vec![target(PingProbeKind::Icmp, None, None)]));
        // An IP literal must sit in its own family's column.
        assert!(!valid(vec![target(
            PingProbeKind::Icmp,
            Some("2001:db8::1"),
            None
        )]));
        assert!(!valid(vec![target(
            PingProbeKind::Tcp,
            None,
            Some("192.0.2.1:443")
        )]));
        assert!(!valid(vec![target(
            PingProbeKind::Icmp,
            Some("1.1.1.1:80"),
            None
        )]));
        assert!(!valid(vec![target(
            PingProbeKind::Tcp,
            Some("example.com"),
            None
        )]));
        // The same endpoint in the same family twice is one series.
        assert!(!valid(vec![
            target(PingProbeKind::Icmp, Some("1.1.1.1"), None),
            target(PingProbeKind::Icmp, Some("1.1.1.1"), None),
        ]));
        // A domain in both columns is two series.
        assert!(valid(vec![target(
            PingProbeKind::Icmp,
            Some("example.com"),
            Some("example.com")
        )]));
    }

    #[test]
    fn saving_trims_and_canonicalizes_endpoints() {
        let normalized = normalize_settings(settings(vec![PingProbeTarget {
            name: "  Cloudflare ".to_owned(),
            kind: PingProbeKind::Icmp,
            ipv4: Some(" ".to_owned()),
            ipv6: Some(" [2606:4700:4700:0:0:0:0:1111] ".to_owned()),
        }]));
        let target = &normalized.targets[0];
        assert_eq!(target.name, "Cloudflare");
        assert_eq!(target.ipv4, None);
        assert_eq!(target.ipv6.as_deref(), Some("2606:4700:4700::1111"));
    }

    #[test]
    fn legacy_stored_targets_are_read_as_dual_stack_targets() {
        let targets = stored_targets(serde_json::json!([
            { "name": "dns", "address": "icmp://dns.alidns.com" },
            { "name": "v4", "address": "icmp://223.5.5.5" },
            { "name": "v6", "address": "tcp://[2400:3200::1]:443" },
            { "name": "dup", "address": "icmp://DNS.alidns.com" },
        ]))
        .unwrap();
        assert_eq!(
            targets.len(),
            3,
            "a duplicate series is dropped, not invented"
        );
        assert_eq!(targets[0].ipv4.as_deref(), Some("dns.alidns.com"));
        assert_eq!(targets[0].ipv6.as_deref(), Some("dns.alidns.com"));
        assert_eq!(targets[1].ipv6, None);
        assert_eq!(targets[2].kind, PingProbeKind::Tcp);
        assert_eq!(targets[2].ipv4, None);
        assert!(validate_settings(&settings(targets)).is_ok());

        let current = stored_targets(serde_json::json!([
            { "name": "c", "kind": "icmp", "ipv4": "1.1.1.1", "ipv6": null }
        ]))
        .unwrap();
        assert_eq!(current[0].ipv4.as_deref(), Some("1.1.1.1"));
    }

    #[test]
    fn configured_series_are_keyed_by_address_and_family() {
        let series = configured_series(&settings(vec![target(
            PingProbeKind::Tcp,
            Some("example.com:443"),
            Some("example.com:443"),
        )]));
        assert_eq!(
            series.into_iter().collect::<Vec<_>>(),
            [
                ("tcp://example.com:443".to_owned(), PingProbeFamily::Ipv4),
                ("tcp://example.com:443".to_owned(), PingProbeFamily::Ipv6),
            ]
        );
    }

    #[test]
    fn timeout_boundary_is_inclusive_and_slower_samples_are_missing() {
        assert_eq!(
            successful_latency_us(Some(419_999), true, 420),
            Some(419_999)
        );
        assert_eq!(
            successful_latency_us(Some(420_000), true, 420),
            Some(420_000)
        );
        assert_eq!(successful_latency_us(Some(420_001), true, 420), None);
        assert_eq!(successful_latency_us(None, true, 420), None);
        assert_eq!(successful_latency_us(Some(1), false, 420), None);
    }

    #[test]
    fn five_seconds_is_the_minimum_probe_interval() {
        let settings = |interval_secs| PingProbeSettings {
            targets: Vec::new(),
            interval_secs,
            timeout_ms: 420,
        };

        assert!(validate_settings(&settings(5)).is_ok());
        assert!(validate_settings(&settings(4)).is_err());
    }

    #[test]
    fn latest_fleet_query_bounds_each_primary_key_probe() {
        assert!(LATEST_NODE_SAMPLES_SQL.contains("JOIN LATERAL"));
        assert!(LATEST_NODE_SAMPLES_SQL.contains("LIMIT 1"));
        assert!(LATEST_NODE_SAMPLES_SQL.contains("series.family = requested.family"));
        assert!(LATEST_NODE_SAMPLES_SQL.contains("sample.series_id = series.id"));
        assert!(!LATEST_NODE_SAMPLES_SQL.contains("DISTINCT ON"));
    }

    #[test]
    fn columnar_view_keeps_exact_ping_order_and_states_per_family() {
        let view = NodePingProbeView {
            node_id: "n1".to_owned(),
            targets: vec![PingProbeTargetSeries {
                name: "target".to_owned(),
                kind: PingProbeKind::Icmp,
                ipv4: Some(PingProbeFamilySeries {
                    address: "icmp://example.test".to_owned(),
                    samples: vec![
                        PingProbePoint {
                            probed_at_unix_secs: 10,
                            attempted: true,
                            latency_us: Some(12_345),
                            skip_reason: None,
                        },
                        PingProbePoint {
                            probed_at_unix_secs: 20,
                            attempted: true,
                            latency_us: None,
                            skip_reason: None,
                        },
                    ],
                }),
                ipv6: Some(PingProbeFamilySeries {
                    address: "icmp://example.test".to_owned(),
                    samples: vec![PingProbePoint {
                        probed_at_unix_secs: 30,
                        attempted: false,
                        latency_us: None,
                        skip_reason: Some(PingProbeSkipReason::NoRoute),
                    }],
                }),
            }],
        };

        let compact = columnar_view(view);
        let ipv4 = compact.targets[0].ipv4.as_ref().unwrap();
        assert_eq!(ipv4.probed_at_unix_secs, vec![10, 20]);
        assert_eq!(ipv4.attempted, vec![true, true]);
        assert_eq!(ipv4.latency_us, vec![Some(12_345), None]);
        let ipv6 = compact.targets[0].ipv6.as_ref().unwrap();
        assert_eq!(ipv6.skip_reason, vec![Some(PingProbeSkipReason::NoRoute)]);
    }
}
