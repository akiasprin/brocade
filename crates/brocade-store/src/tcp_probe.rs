//! Active TCP-connect observation.
//!
//! The schema and protocol are intentionally minimal: a round records only when it ran, which
//! configured target it addressed, and an optional connect duration. DNS is resolved by the
//! agent before timing starts. A failed resolution, timeout, or refused connection all become the
//! same `NULL`, because the UI asks only whether the link answered and draws all missing samples
//! the same way.

use std::collections::{BTreeMap, BTreeSet};

use brocade_deployment::protocol::{
    NodeTcpProbeList, NodeTcpProbeView, TcpProbePoint, TcpProbeReportRequest, TcpProbeReportResult,
    TcpProbeSettings, TcpProbeTarget, TcpProbeTargetSeries,
};
use sqlx::{PgPool, Row};

use crate::{admin::tenant_filter, AdminContext, Result, StoreError};

const MAX_CLOCK_SKEW_SECS: i64 = 600;
const MAX_TARGETS: usize = 32;
const MIN_INTERVAL_SECS: u32 = 15;
const MAX_INTERVAL_SECS: u32 = 86_400;
const MIN_TIMEOUT_MS: u32 = 1;
const MAX_TIMEOUT_MS: u32 = 120_000;
const MAX_NAME_CHARS: usize = 64;
const MAX_ADDRESS_CHARS: usize = 512;
const MAX_READ_WINDOW_SECS: u32 = 7 * 86_400;
const RETAIN_SECS: i64 = 7 * 86_400;

pub async fn load_settings(pool: &PgPool) -> Result<TcpProbeSettings> {
    let row = sqlx::query(
        "SELECT tcp_probe_targets, tcp_probe_interval_secs, tcp_probe_timeout_ms
           FROM control_state WHERE id = TRUE",
    )
    .fetch_one(pool)
    .await?;
    let targets: serde_json::Value = row.try_get("tcp_probe_targets")?;
    let settings = TcpProbeSettings {
        targets: serde_json::from_value(targets)?,
        interval_secs: u32::try_from(row.try_get::<i32, _>("tcp_probe_interval_secs")?).map_err(
            |_| StoreError::InvalidData("negative TCP probe interval in database".into()),
        )?,
        timeout_ms: u32::try_from(row.try_get::<i32, _>("tcp_probe_timeout_ms")?).map_err(
            |_| StoreError::InvalidData("negative TCP probe timeout in database".into()),
        )?,
    };
    validate_settings(&settings)?;
    Ok(settings)
}

pub async fn update_settings(
    pool: &PgPool,
    actor: &AdminContext,
    settings: TcpProbeSettings,
) -> Result<TcpProbeSettings> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update TCP probe settings".to_owned(),
        ));
    }
    let settings = normalize_settings(settings);
    validate_settings(&settings)?;
    sqlx::query(
        "UPDATE control_state
            SET tcp_probe_targets = $1,
                tcp_probe_interval_secs = $2,
                tcp_probe_timeout_ms = $3
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
    request: TcpProbeReportRequest,
) -> Result<TcpProbeReportResult> {
    if request.probed_at_unix_secs <= 0 {
        return Err(StoreError::InvalidData(
            "TCP probe timestamp must be positive unix seconds".to_owned(),
        ));
    }
    if request.samples.len() > MAX_TARGETS {
        return Err(StoreError::InvalidData(format!(
            "TCP probe carries {} targets, over the {MAX_TARGETS} limit",
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
            "TCP probe clock skew {}s exceeds {MAX_CLOCK_SKEW_SECS}s; check the node's clock",
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
        let connect_ms = successful_connect_ms(sample.connect_ms, settings.timeout_ms)
            .map(|value| {
                i32::try_from(value).map_err(|_| {
                    StoreError::InvalidData("TCP connect duration exceeds storage range".into())
                })
            })
            .transpose()?;
        let result = sqlx::query(
            "INSERT INTO node_tcp_probe_samples (node_id, target, probed_at, connect_ms)
             VALUES ($1, $2, to_timestamp($3), $4)
             ON CONFLICT (node_id, target, probed_at) DO NOTHING",
        )
        .bind(node_id)
        .bind(&sample.target)
        .bind(request.probed_at_unix_secs)
        .bind(connect_ms)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 1 {
            accepted_samples += 1;
        } else {
            skipped_samples += 1;
        }
    }

    // Observation history is diagnostic rather than accounting. Bound it opportunistically on
    // writes so a forgotten installation cannot grow this table forever.
    sqlx::query(
        "DELETE FROM node_tcp_probe_samples
          WHERE probed_at < now() - make_interval(secs => $1::double precision)",
    )
    .bind(RETAIN_SECS)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(TcpProbeReportResult {
        node_id: node_id.to_owned(),
        accepted_samples,
        skipped_samples,
        unknown_targets,
    })
}

pub async fn node_view(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    window_secs: u32,
) -> Result<NodeTcpProbeView> {
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
    )
    .await
}

pub async fn list_nodes(
    pool: &PgPool,
    actor: &AdminContext,
    window_secs: u32,
) -> Result<NodeTcpProbeList> {
    let settings = load_settings(pool).await?;
    let filter = tenant_filter(actor);
    let (scope, pattern) = split_filter(&filter);
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM nodes
         WHERE retired_at IS NULL
           AND ($1::text IS NULL OR tenant_id = $1 OR tenant_id LIKE $2 ESCAPE '\\')
         ORDER BY id",
    )
    .bind(scope)
    .bind(pattern)
    .fetch_all(pool)
    .await?;
    let mut nodes = Vec::with_capacity(ids.len());
    for id in ids {
        nodes.push(
            read_node(
                pool,
                &id,
                &settings.targets,
                settings.timeout_ms,
                bounded_window(window_secs),
            )
            .await?,
        );
    }
    Ok(NodeTcpProbeList { nodes })
}

async fn read_node(
    pool: &PgPool,
    node_id: &str,
    targets: &[TcpProbeTarget],
    timeout_ms: u32,
    window_secs: u32,
) -> Result<NodeTcpProbeView> {
    let rows = sqlx::query(
        "SELECT target, extract(epoch FROM probed_at)::bigint AS probed_at, connect_ms
           FROM node_tcp_probe_samples
          WHERE node_id = $1
            AND probed_at >= now() - make_interval(secs => $2::double precision)
          ORDER BY probed_at ASC",
    )
    .bind(node_id)
    .bind(i32::try_from(window_secs).expect("bounded window fits i32"))
    .fetch_all(pool)
    .await?;
    let mut points = BTreeMap::<String, Vec<TcpProbePoint>>::new();
    for row in rows {
        let target: String = row.try_get("target")?;
        let connect_ms = successful_connect_ms(
            row.try_get::<Option<i32>, _>("connect_ms")?
                .map(|value| {
                    u32::try_from(value).map_err(|_| {
                        StoreError::InvalidData("negative TCP connect duration in database".into())
                    })
                })
                .transpose()?,
            timeout_ms,
        );
        // Apply the current policy to historical rows too. Lowering the timeout must not leave
        // old, now-invalid latency points visible until retention expires.
        points.entry(target).or_default().push(TcpProbePoint {
            probed_at_unix_secs: row.try_get("probed_at")?,
            connect_ms,
        });
    }
    Ok(NodeTcpProbeView {
        node_id: node_id.to_owned(),
        targets: targets
            .iter()
            .map(|target| TcpProbeTargetSeries {
                name: target.name.clone(),
                address: target.address.clone(),
                samples: points.remove(&target.address).unwrap_or_default(),
            })
            .collect(),
    })
}

fn empty_view(node_id: &str, targets: &[TcpProbeTarget]) -> NodeTcpProbeView {
    NodeTcpProbeView {
        node_id: node_id.to_owned(),
        targets: targets
            .iter()
            .map(|target| TcpProbeTargetSeries {
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

fn successful_connect_ms(connect_ms: Option<u32>, timeout_ms: u32) -> Option<u32> {
    connect_ms.filter(|value| *value <= timeout_ms)
}

fn normalize_settings(settings: TcpProbeSettings) -> TcpProbeSettings {
    TcpProbeSettings {
        targets: settings
            .targets
            .into_iter()
            .map(|target| TcpProbeTarget {
                name: target.name.trim().to_owned(),
                address: target.address.trim().to_owned(),
            })
            .collect(),
        interval_secs: settings.interval_secs,
        timeout_ms: settings.timeout_ms,
    }
}

fn validate_settings(settings: &TcpProbeSettings) -> Result<()> {
    if !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&settings.interval_secs) {
        return Err(StoreError::InvalidData(format!(
            "TCP 探测间隔必须为 {MIN_INTERVAL_SECS}–{MAX_INTERVAL_SECS} 秒"
        )));
    }
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&settings.timeout_ms) {
        return Err(StoreError::InvalidData(format!(
            "TCP 连接超时必须为 {MIN_TIMEOUT_MS}–{MAX_TIMEOUT_MS} 毫秒"
        )));
    }
    if settings.targets.len() > MAX_TARGETS {
        return Err(StoreError::InvalidData(format!(
            "TCP 探测目标不能超过 {MAX_TARGETS} 个"
        )));
    }
    let mut addresses = BTreeSet::new();
    for target in &settings.targets {
        let name_len = target.name.chars().count();
        if name_len == 0 || name_len > MAX_NAME_CHARS || target.name.chars().any(char::is_control) {
            return Err(StoreError::InvalidData(format!(
                "TCP 探测目标名称必须为 1–{MAX_NAME_CHARS} 个可见字符"
            )));
        }
        validate_tcp_address(&target.address)?;
        if !addresses.insert(target.address.as_str()) {
            return Err(StoreError::InvalidData(format!(
                "TCP 探测地址不能重复：{}",
                target.address
            )));
        }
    }
    Ok(())
}

fn validate_tcp_address(address: &str) -> Result<()> {
    if address.len() > MAX_ADDRESS_CHARS {
        return Err(StoreError::InvalidData("TCP 探测地址过长".to_owned()));
    }
    let Some(authority) = address.strip_prefix("tcp://") else {
        return Err(StoreError::InvalidData(
            "TCP 探测地址必须使用 tcp://host:port".to_owned(),
        ));
    };
    if authority.is_empty()
        || authority.chars().any(char::is_whitespace)
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
    {
        return Err(StoreError::InvalidData(format!(
            "无效的 TCP 探测地址：{address}"
        )));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_tcp_targets_and_rejects_other_schemes() {
        assert!(validate_tcp_address("tcp://example.com:443").is_ok());
        assert!(validate_tcp_address("tcp://[2001:db8::1]:443").is_ok());
        assert!(validate_tcp_address("icmp://1.1.1.1").is_err());
        assert!(validate_tcp_address("tcp://example.com").is_err());
        assert!(validate_tcp_address("tcp://example.com:0").is_err());
    }

    #[test]
    fn timeout_boundary_is_inclusive_and_slower_samples_are_missing() {
        assert_eq!(successful_connect_ms(Some(419), 420), Some(419));
        assert_eq!(successful_connect_ms(Some(420), 420), Some(420));
        assert_eq!(successful_connect_ms(Some(421), 420), None);
        assert_eq!(successful_connect_ms(None, 420), None);
    }
}
