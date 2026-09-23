//! Active TCP-connect and ICMP-echo observation.
//!
//! One shared settings list may carry `tcp://` and `icmp://` targets at the same time. A round
//! records only when it ran, which target it addressed, whether a wire measurement was possible,
//! and an optional latency. DNS and local diagnostics never become historical metrics.

use std::collections::{BTreeMap, BTreeSet};

use brocade_deployment::protocol::{
    NodePingProbeLatestList, NodePingProbeLatestView, NodePingProbeList, NodePingProbeView,
    PingProbePoint, PingProbeReportRequest, PingProbeReportResult, PingProbeSettings,
    PingProbeTarget, PingProbeTargetLatest, PingProbeTargetSeries,
};
use sqlx::{PgPool, Row};

use crate::{admin::tenant_filter, AdminContext, Result, StoreError};

// The primary key is (node_id, target, probed_at). Ask for each requested pair explicitly so each
// LATERAL arm is a bounded reverse index scan. DISTINCT ON would sort the entire retained table to
// produce the same handful of rows.
const LATEST_NODE_SAMPLES_SQL: &str = "SELECT requested_node.node_id, requested_target.target,
            extract(epoch FROM latest.probed_at)::bigint AS probed_at,
            latest.attempted, latest.latency_us
       FROM unnest($1::text[]) AS requested_node(node_id)
       CROSS JOIN unnest($2::text[]) AS requested_target(target)
       JOIN LATERAL (
            SELECT sample.probed_at, sample.attempted, sample.latency_us
              FROM node_ping_probe_samples sample
             WHERE sample.node_id = requested_node.node_id
               AND sample.target = requested_target.target
             ORDER BY sample.probed_at DESC
             LIMIT 1
       ) latest ON TRUE
      ORDER BY requested_node.node_id, requested_target.target";

const MAX_CLOCK_SKEW_SECS: i64 = 600;
const MAX_TARGETS: usize = 32;
const MIN_INTERVAL_SECS: u32 = 5;
const MAX_INTERVAL_SECS: u32 = 86_400;
const MIN_TIMEOUT_MS: u32 = 1;
const MAX_TIMEOUT_MS: u32 = 120_000;
const MAX_NAME_CHARS: usize = 64;
const MAX_ADDRESS_CHARS: usize = 512;
const MAX_READ_WINDOW_SECS: u32 = 7 * 86_400;

/// The chart reads every retained PING point, but repeated object keys account for most of the
/// row-oriented JSON. Parallel arrays preserve the exact samples while writing the target
/// metadata and field names once.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NodePingProbeColumnarView {
    pub node_id: String,
    pub targets: Vec<PingProbeTargetColumnarSeries>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PingProbeTargetColumnarSeries {
    pub name: String,
    pub address: String,
    pub probed_at_unix_secs: Vec<i64>,
    pub attempted: Vec<bool>,
    pub latency_us: Vec<Option<u32>>,
}

pub fn columnar_view(view: NodePingProbeView) -> NodePingProbeColumnarView {
    NodePingProbeColumnarView {
        node_id: view.node_id,
        targets: view
            .targets
            .into_iter()
            .map(|target| {
                let mut probed_at_unix_secs = Vec::with_capacity(target.samples.len());
                let mut attempted = Vec::with_capacity(target.samples.len());
                let mut latency_us = Vec::with_capacity(target.samples.len());
                for sample in target.samples {
                    probed_at_unix_secs.push(sample.probed_at_unix_secs);
                    attempted.push(sample.attempted);
                    latency_us.push(sample.latency_us);
                }
                PingProbeTargetColumnarSeries {
                    name: target.name,
                    address: target.address,
                    probed_at_unix_secs,
                    attempted,
                    latency_us,
                }
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
        targets: serde_json::from_value(targets)?,
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
    if request.samples.len() > MAX_TARGETS {
        return Err(StoreError::InvalidData(format!(
            "PING probe carries {} targets, over the {MAX_TARGETS} limit",
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
    let known = settings
        .targets
        .iter()
        .map(|target| target.address.as_str())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut accepted_samples = 0;
    let mut skipped_samples = 0;
    let mut unknown_targets = 0;
    let mut tx = pool.begin().await?;

    for sample in request.samples {
        if !known.contains(sample.target.as_str()) {
            unknown_targets += 1;
            continue;
        }
        if !seen.insert(sample.target.clone()) {
            skipped_samples += 1;
            continue;
        }
        if !sample.attempted && sample.latency_us.is_some() {
            return Err(StoreError::InvalidData(format!(
                "未执行的 PING 探测不能携带延迟：{}",
                sample.target
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
        let result = sqlx::query(
            "INSERT INTO node_ping_probe_samples
                 (node_id, target, probed_at, attempted, latency_us)
             VALUES ($1, $2, to_timestamp($3), $4, $5)
             ON CONFLICT (node_id, target, probed_at) DO NOTHING",
        )
        .bind(node_id)
        .bind(&sample.target)
        .bind(request.probed_at_unix_secs)
        .bind(attempted)
        .bind(latency_us)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 1 {
            accepted_samples += 1;
        } else {
            skipped_samples += 1;
        }
    }

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
    Ok(sqlx::query(
        "DELETE FROM node_ping_probe_samples
          WHERE probed_at < now() - make_interval(days => $1)",
    )
    .bind(retain_days)
    .execute(pool)
    .await?
    .rows_affected())
}

pub async fn node_view(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    window_secs: u32,
) -> Result<NodePingProbeView> {
    let settings = load_settings(pool).await?;
    if !node_in_scope(pool, actor, node_id).await? {
        return Ok(empty_view(node_id, &settings.targets));
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
        return Ok(empty_view(node_id, &settings.targets));
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

pub async fn list_nodes(
    pool: &PgPool,
    actor: &AdminContext,
    window_secs: u32,
) -> Result<NodePingProbeList> {
    let settings = load_settings(pool).await?;
    let ids = scoped_live_node_ids(pool, actor).await?;
    let mut points = BTreeMap::<(String, String), Vec<PingProbePoint>>::new();
    if !ids.is_empty() {
        let rows = sqlx::query(
            "SELECT node_id, target,
                    extract(epoch FROM probed_at)::bigint AS probed_at,
                    attempted, latency_us
               FROM node_ping_probe_samples
              WHERE node_id = ANY($1::text[])
                AND probed_at >= now() - make_interval(secs => $2::double precision)
              ORDER BY node_id, target, probed_at ASC",
        )
        .bind(&ids)
        .bind(i32::try_from(bounded_window(window_secs)).expect("bounded window fits i32"))
        .fetch_all(pool)
        .await?;
        for row in rows {
            let node_id: String = row.try_get("node_id")?;
            let target: String = row.try_get("target")?;
            points
                .entry((node_id, target))
                .or_default()
                .push(point_from_row(&row, settings.timeout_ms)?);
        }
    }
    let nodes = ids
        .into_iter()
        .map(|node_id| NodePingProbeView {
            targets: settings
                .targets
                .iter()
                .map(|target| PingProbeTargetSeries {
                    name: target.name.clone(),
                    address: target.address.clone(),
                    samples: points
                        .remove(&(node_id.clone(), target.address.clone()))
                        .unwrap_or_default(),
                })
                .collect(),
            node_id,
        })
        .collect();
    Ok(NodePingProbeList { nodes })
}

/// The machine list needs only the newest observation for each configured target. Fetching a
/// history window here used to perform one query per machine and ship every point to the browser,
/// where the whole window was reduced to one card statistic. Keep the history
/// endpoint for a machine detail chart; this path has a fixed query count and bounded response.
pub async fn list_latest_nodes(
    pool: &PgPool,
    actor: &AdminContext,
) -> Result<NodePingProbeLatestList> {
    let settings = load_settings(pool).await?;
    let ids = scoped_live_node_ids(pool, actor).await?;
    let addresses = settings
        .targets
        .iter()
        .map(|target| target.address.clone())
        .collect::<Vec<_>>();
    let mut latest = BTreeMap::<(String, String), PingProbePoint>::new();
    if !ids.is_empty() && !addresses.is_empty() {
        let rows = sqlx::query(LATEST_NODE_SAMPLES_SQL)
            .bind(&ids)
            .bind(&addresses)
            .fetch_all(pool)
            .await?;
        for row in rows {
            let node_id: String = row.try_get("node_id")?;
            let target: String = row.try_get("target")?;
            latest.insert(
                (node_id, target),
                point_from_row(&row, settings.timeout_ms)?,
            );
        }
    }
    let nodes = ids
        .into_iter()
        .map(|node_id| NodePingProbeLatestView {
            targets: settings
                .targets
                .iter()
                .map(|target| PingProbeTargetLatest {
                    name: target.name.clone(),
                    address: target.address.clone(),
                    latest: latest.remove(&(node_id.clone(), target.address.clone())),
                })
                .collect(),
            node_id,
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
        "SELECT target, extract(epoch FROM probed_at)::bigint AS probed_at, attempted, latency_us
           FROM node_ping_probe_samples
          WHERE node_id = $1
            AND (
                ($3::bigint IS NULL
                 AND probed_at >= now() - make_interval(secs => $2::double precision))
                OR
                ($3::bigint IS NOT NULL
                 AND probed_at >= to_timestamp($3)
                 AND probed_at <= to_timestamp($4))
            )
          ORDER BY probed_at ASC",
    )
    .bind(node_id)
    .bind(i32::try_from(window_secs).expect("bounded window fits i32"))
    .bind(absolute.map(|range| range.0))
    .bind(absolute.map(|range| range.1))
    .fetch_all(pool)
    .await?;
    let mut points = BTreeMap::<String, Vec<PingProbePoint>>::new();
    for row in rows {
        let target: String = row.try_get("target")?;
        // Apply the current policy to historical rows too. Lowering the timeout must not leave
        // old, now-invalid latency points visible until retention expires.
        points
            .entry(target)
            .or_default()
            .push(point_from_row(&row, timeout_ms)?);
    }
    Ok(NodePingProbeView {
        node_id: node_id.to_owned(),
        targets: targets
            .iter()
            .map(|target| PingProbeTargetSeries {
                name: target.name.clone(),
                address: target.address.clone(),
                samples: points.remove(&target.address).unwrap_or_default(),
            })
            .collect(),
    })
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
    Ok(PingProbePoint {
        probed_at_unix_secs: row.try_get("probed_at")?,
        attempted,
        latency_us,
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

fn empty_view(node_id: &str, targets: &[PingProbeTarget]) -> NodePingProbeView {
    NodePingProbeView {
        node_id: node_id.to_owned(),
        targets: targets
            .iter()
            .map(|target| PingProbeTargetSeries {
                name: target.name.clone(),
                address: target.address.clone(),
                samples: Vec::new(),
            })
            .collect(),
    }
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

fn normalize_settings(settings: PingProbeSettings) -> PingProbeSettings {
    PingProbeSettings {
        targets: settings
            .targets
            .into_iter()
            .map(|target| PingProbeTarget {
                name: target.name.trim().to_owned(),
                address: target.address.trim().to_owned(),
            })
            .collect(),
        interval_secs: settings.interval_secs,
        timeout_ms: settings.timeout_ms,
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
    let mut addresses = BTreeSet::new();
    for target in &settings.targets {
        let name_len = target.name.chars().count();
        if name_len == 0 || name_len > MAX_NAME_CHARS || target.name.chars().any(char::is_control) {
            return Err(StoreError::InvalidData(format!(
                "PING 探测目标名称必须为 1–{MAX_NAME_CHARS} 个可见字符"
            )));
        }
        validate_probe_address(&target.address)?;
        if !addresses.insert(target.address.as_str()) {
            return Err(StoreError::InvalidData(format!(
                "PING 探测地址不能重复：{}",
                target.address
            )));
        }
    }
    Ok(())
}

fn validate_probe_address(address: &str) -> Result<()> {
    if address.len() > MAX_ADDRESS_CHARS {
        return Err(StoreError::InvalidData("PING 探测地址过长".to_owned()));
    }
    if let Some(authority) = address.strip_prefix("tcp://") {
        return validate_tcp_authority(address, authority);
    }
    if let Some(authority) = address.strip_prefix("icmp://") {
        return validate_icmp_authority(address, authority);
    }
    Err(StoreError::InvalidData(
        "PING 探测地址必须使用 tcp://host:port 或 icmp://host".to_owned(),
    ))
}

fn invalid_authority(authority: &str) -> bool {
    authority.is_empty()
        || authority.chars().any(char::is_whitespace)
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
}

fn validate_tcp_authority(address: &str, authority: &str) -> Result<()> {
    if invalid_authority(authority) {
        return Err(StoreError::InvalidData(format!(
            "无效的 TCP 探测地址：{address}"
        )));
    };
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((host, port)) = bracketed.split_once("]:") else {
            return Err(StoreError::InvalidData(format!(
                "无效的 TCP 探测地址：{address}"
            )));
        };
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(StoreError::InvalidData(format!(
                "方括号内必须是 IPv6 地址：{address}"
            )));
        }
        (host, port)
    } else {
        let pair = authority
            .rsplit_once(':')
            .ok_or_else(|| StoreError::InvalidData(format!("TCP 探测地址缺少端口：{address}")))?;
        if pair.0.chars().any(|ch| matches!(ch, ':' | '[' | ']')) {
            return Err(StoreError::InvalidData(format!(
                "IPv6 地址必须放在方括号内：{address}"
            )));
        }
        pair
    };
    if host.is_empty() || port.parse::<u16>().ok().filter(|port| *port > 0).is_none() {
        return Err(StoreError::InvalidData(format!(
            "无效的 TCP 探测地址：{address}"
        )));
    }
    Ok(())
}

fn validate_icmp_authority(address: &str, authority: &str) -> Result<()> {
    if invalid_authority(authority) {
        return Err(StoreError::InvalidData(format!(
            "无效的 ICMP 探测地址：{address}"
        )));
    }
    if let Some(host) = authority
        .strip_prefix('[')
        .and_then(|bracketed| bracketed.strip_suffix(']'))
    {
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(StoreError::InvalidData(format!(
                "方括号内必须是 IPv6 地址：{address}"
            )));
        }
        return Ok(());
    }
    if authority.chars().any(|ch| matches!(ch, ':' | '[' | ']')) {
        return Err(StoreError::InvalidData(format!(
            "ICMP 不接受端口，IPv6 地址必须放在方括号内：{address}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_tcp_and_icmp_targets_together() {
        assert!(validate_probe_address("tcp://example.com:443").is_ok());
        assert!(validate_probe_address("tcp://[2001:db8::1]:443").is_ok());
        assert!(validate_probe_address("icmp://1.1.1.1").is_ok());
        assert!(validate_probe_address("icmp://example.com").is_ok());
        assert!(validate_probe_address("icmp://[2001:db8::1]").is_ok());
        assert!(validate_probe_address("icmp://1.1.1.1:80").is_err());
        assert!(validate_probe_address("icmp://2001:db8::1").is_err());
        assert!(validate_probe_address("tcp://example.com").is_err());
        assert!(validate_probe_address("tcp://example.com:0").is_err());
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
        assert!(!LATEST_NODE_SAMPLES_SQL.contains("DISTINCT ON"));
    }

    #[test]
    fn columnar_view_keeps_exact_ping_order_and_states() {
        let view = NodePingProbeView {
            node_id: "n1".to_owned(),
            targets: vec![PingProbeTargetSeries {
                name: "target".to_owned(),
                address: "icmp://example.test".to_owned(),
                samples: vec![
                    PingProbePoint {
                        probed_at_unix_secs: 10,
                        attempted: true,
                        latency_us: Some(12_345),
                    },
                    PingProbePoint {
                        probed_at_unix_secs: 20,
                        attempted: true,
                        latency_us: None,
                    },
                    PingProbePoint {
                        probed_at_unix_secs: 30,
                        attempted: false,
                        latency_us: None,
                    },
                ],
            }],
        };

        let compact = columnar_view(view);
        assert_eq!(compact.targets[0].probed_at_unix_secs, vec![10, 20, 30]);
        assert_eq!(compact.targets[0].attempted, vec![true, true, false]);
        assert_eq!(
            compact.targets[0].latency_us,
            vec![Some(12_345), None, None]
        );
    }
}
