//! Durable Console-originated probes for external outbounds.
//!
//! Policies and runs are operational state: they are tenant-scoped, immediately effective, and
//! never create a model revision. A Serving run references its immutable topology revision; a
//! manual draft run AEAD-seals only the selected outbound at the click boundary. Both freeze the
//! current public endpoint. Credentials are materialized only after the worker has fenced
//! ownership, and a draft's short-lived ciphertext is erased at the terminal transition.

use std::collections::BTreeMap;

use brocade_core::model::{ExternalOutbound, ExternalOutboundProtocol, ModelSnapshot};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{admin::tenant_filter, input::required_text, AdminContext, Result, StoreError};

pub const TUNNEL_PROBE_RETENTION_DAYS: u32 = 7;
pub const TUNNEL_PROBE_GLOBAL_CONCURRENCY: i64 = 30;
pub const TUNNEL_PROBE_LEASE_SECS: i32 = 30;
const MAX_DUE_PER_TICK: i64 = 32;
const PRUNE_BATCH: i64 = 5_000;
const SCHEDULER_ADVISORY_LOCK: i64 = 0x4252_4f43_5052_4f42;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TunnelProbeHealth {
    Healthy,
    Degraded,
    Down,
    Paused,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TunnelProbePolicy {
    pub enabled: bool,
    pub interval_secs: u32,
    pub timeout_secs: u16,
    pub next_run_at_unix_secs: Option<i64>,
    pub updated_at_unix_secs: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TunnelProbeTrigger {
    Manual,
    Scheduled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TunnelProbeSource {
    #[default]
    Serving,
    Draft,
}

impl TunnelProbeSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Serving => "serving",
            Self::Draft => "draft",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TunnelProbeJobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Canceled,
    Unsupported,
}

impl TunnelProbeJobStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TunnelProbePhase {
    Queued,
    Preparing,
    StartingXray,
    Requesting,
    Finished,
}

impl TunnelProbePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Preparing => "preparing",
            Self::StartingXray => "starting-xray",
            Self::Requesting => "requesting",
            Self::Finished => "finished",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TunnelProbeResultStatus {
    Ok,
    Timeout,
    ConnectFailed,
    TargetFailed,
    Unsupported,
    Canceled,
    Interrupted,
}

impl TunnelProbeResultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Timeout => "timeout",
            Self::ConnectFailed => "connect-failed",
            Self::TargetFailed => "target-failed",
            Self::Unsupported => "unsupported",
            Self::Canceled => "canceled",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TunnelProbeRun {
    pub id: i64,
    pub tenant_id: String,
    pub outbound_id: String,
    pub outbound_name: String,
    pub protocol: String,
    pub trigger: TunnelProbeTrigger,
    pub source: TunnelProbeSource,
    pub topology_revision: u64,
    pub serving_generation: Option<u64>,
    pub draft_sha256: Option<String>,
    pub settings_revision: u64,
    pub timeout_secs: u16,
    pub status: TunnelProbeJobStatus,
    pub phase: TunnelProbePhase,
    pub result: Option<TunnelProbeResultStatus>,
    pub ttfb_ms: Option<u32>,
    pub http_status: Option<u16>,
    pub exit_ip: Option<String>,
    pub exit_loc: Option<String>,
    pub attempt_count: u8,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub queued_at_unix_secs: i64,
    pub started_at_unix_secs: Option<i64>,
    pub finished_at_unix_secs: Option<i64>,
    pub cancel_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TunnelProbeListItem {
    pub tenant_id: String,
    pub outbound_id: String,
    pub name: String,
    pub protocol: String,
    pub supported: bool,
    pub unsupported_reason: Option<String>,
    pub health: TunnelProbeHealth,
    pub policy: Option<TunnelProbePolicy>,
    pub latest_run: Option<TunnelProbeRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TunnelProbeList {
    pub origin: &'static str,
    pub endpoint_url: String,
    pub retention_days: u32,
    pub items: Vec<TunnelProbeListItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TunnelProbeSummary {
    pub window_secs: u32,
    pub total: u64,
    pub succeeded: u64,
    pub success_rate: Option<f64>,
    pub p50_ms: Option<u32>,
    pub p95_ms: Option<u32>,
    pub failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TunnelProbePoint {
    pub run_id: i64,
    pub finished_at_unix_secs: i64,
    pub result: TunnelProbeResultStatus,
    pub ttfb_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TunnelProbeView {
    pub item: TunnelProbeListItem,
    pub retention_days: u32,
    pub summary: TunnelProbeSummary,
    pub points: Vec<TunnelProbePoint>,
    pub recent_runs: Vec<TunnelProbeRun>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTunnelProbePolicy {
    pub enabled: bool,
    pub interval_secs: u32,
    pub timeout_secs: u16,
}

#[derive(Debug, Clone)]
pub struct ClaimedTunnelProbe {
    pub run: TunnelProbeRun,
    pub endpoint_url: String,
    pub lease_owner: String,
    pub lease_generation: u64,
}

#[derive(Debug, Clone)]
pub struct TunnelProbeCompletion {
    pub status: TunnelProbeJobStatus,
    pub result: TunnelProbeResultStatus,
    pub ttfb_ms: Option<u32>,
    pub http_status: Option<u16>,
    pub exit_ip: Option<String>,
    pub exit_loc: Option<String>,
    pub attempt_count: u8,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
}

fn protocol_name(protocol: &ExternalOutboundProtocol) -> &'static str {
    match protocol {
        ExternalOutboundProtocol::Anytls { .. } => "anytls",
        ExternalOutboundProtocol::Vless { .. } => "vless",
        ExternalOutboundProtocol::Shadowsocks2022 { .. } => "shadowsocks2022",
        ExternalOutboundProtocol::Socks5 { .. } => "socks5",
        ExternalOutboundProtocol::HttpConnect { .. } => "http_connect",
        ExternalOutboundProtocol::Wireguard { .. } => "wireguard",
        ExternalOutboundProtocol::Warp { .. } => "warp",
        ExternalOutboundProtocol::Vpngate { .. } => "vpngate",
    }
}

fn i64_to_u64(field: &str, value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{field} is negative: {value}")))
}

fn i32_to_u32(field: &str, value: i32) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{field} is negative: {value}")))
}

fn i32_to_u16(field: &str, value: i32) -> Result<u16> {
    u16::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{field} is outside u16: {value}")))
}

fn i32_to_u8(field: &str, value: i32) -> Result<u8> {
    u8::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{field} is outside u8: {value}")))
}

fn job_status(value: &str) -> Result<TunnelProbeJobStatus> {
    match value {
        "queued" => Ok(TunnelProbeJobStatus::Queued),
        "running" => Ok(TunnelProbeJobStatus::Running),
        "succeeded" => Ok(TunnelProbeJobStatus::Succeeded),
        "failed" => Ok(TunnelProbeJobStatus::Failed),
        "canceled" => Ok(TunnelProbeJobStatus::Canceled),
        "unsupported" => Ok(TunnelProbeJobStatus::Unsupported),
        other => Err(StoreError::InvalidData(format!(
            "unknown tunnel probe status {other}"
        ))),
    }
}

fn phase(value: &str) -> Result<TunnelProbePhase> {
    match value {
        "queued" => Ok(TunnelProbePhase::Queued),
        "preparing" => Ok(TunnelProbePhase::Preparing),
        "starting-xray" => Ok(TunnelProbePhase::StartingXray),
        "requesting" => Ok(TunnelProbePhase::Requesting),
        "finished" => Ok(TunnelProbePhase::Finished),
        other => Err(StoreError::InvalidData(format!(
            "unknown tunnel probe phase {other}"
        ))),
    }
}

fn result_status(value: &str) -> Result<TunnelProbeResultStatus> {
    match value {
        "ok" => Ok(TunnelProbeResultStatus::Ok),
        "timeout" => Ok(TunnelProbeResultStatus::Timeout),
        "connect-failed" => Ok(TunnelProbeResultStatus::ConnectFailed),
        "target-failed" => Ok(TunnelProbeResultStatus::TargetFailed),
        "unsupported" => Ok(TunnelProbeResultStatus::Unsupported),
        "canceled" => Ok(TunnelProbeResultStatus::Canceled),
        "interrupted" => Ok(TunnelProbeResultStatus::Interrupted),
        other => Err(StoreError::InvalidData(format!(
            "unknown tunnel probe result {other}"
        ))),
    }
}

fn trigger(value: &str) -> Result<TunnelProbeTrigger> {
    match value {
        "manual" => Ok(TunnelProbeTrigger::Manual),
        "scheduled" => Ok(TunnelProbeTrigger::Scheduled),
        other => Err(StoreError::InvalidData(format!(
            "unknown tunnel probe trigger {other}"
        ))),
    }
}

fn source(value: &str) -> Result<TunnelProbeSource> {
    match value {
        "serving" => Ok(TunnelProbeSource::Serving),
        "draft" => Ok(TunnelProbeSource::Draft),
        other => Err(StoreError::InvalidData(format!(
            "unknown tunnel probe source {other}"
        ))),
    }
}

fn run_from_row(row: &sqlx::postgres::PgRow) -> Result<TunnelProbeRun> {
    Ok(TunnelProbeRun {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        outbound_id: row.try_get("outbound_id")?,
        outbound_name: row.try_get("outbound_name")?,
        protocol: row.try_get("protocol")?,
        trigger: trigger(&row.try_get::<String, _>("trigger")?)?,
        source: source(&row.try_get::<String, _>("source")?)?,
        topology_revision: i64_to_u64(
            "external_outbound_probe_runs.topology_revision_id",
            row.try_get("topology_revision_id")?,
        )?,
        serving_generation: row
            .try_get::<Option<i64>, _>("serving_generation")?
            .map(|value| i64_to_u64("external_outbound_probe_runs.serving_generation", value))
            .transpose()?,
        draft_sha256: row.try_get("draft_sha256")?,
        settings_revision: i64_to_u64(
            "external_outbound_probe_runs.settings_revision_id",
            row.try_get("settings_revision_id")?,
        )?,
        timeout_secs: i32_to_u16(
            "external_outbound_probe_runs.timeout_secs",
            row.try_get("timeout_secs")?,
        )?,
        status: job_status(&row.try_get::<String, _>("status")?)?,
        phase: phase(&row.try_get::<String, _>("phase")?)?,
        result: row
            .try_get::<Option<String>, _>("result")?
            .as_deref()
            .map(result_status)
            .transpose()?,
        ttfb_ms: row
            .try_get::<Option<i32>, _>("ttfb_ms")?
            .map(|value| i32_to_u32("external_outbound_probe_runs.ttfb_ms", value))
            .transpose()?,
        http_status: row
            .try_get::<Option<i32>, _>("http_status")?
            .map(|value| i32_to_u16("external_outbound_probe_runs.http_status", value))
            .transpose()?,
        exit_ip: row.try_get("exit_ip")?,
        exit_loc: row.try_get("exit_loc")?,
        attempt_count: i32_to_u8(
            "external_outbound_probe_runs.attempt_count",
            row.try_get("attempt_count")?,
        )?,
        error_code: row.try_get("error_code")?,
        error_detail: row.try_get("error_detail")?,
        queued_at_unix_secs: row.try_get("queued_at_unix_secs")?,
        started_at_unix_secs: row.try_get("started_at_unix_secs")?,
        finished_at_unix_secs: row.try_get("finished_at_unix_secs")?,
        cancel_requested: row.try_get("cancel_requested")?,
    })
}

fn policy_from_row(row: &sqlx::postgres::PgRow) -> Result<Option<TunnelProbePolicy>> {
    let Some(enabled) = row.try_get::<Option<bool>, _>("policy_enabled")? else {
        return Ok(None);
    };
    Ok(Some(TunnelProbePolicy {
        enabled,
        interval_secs: i32_to_u32(
            "external_outbound_probe_policies.interval_secs",
            row.try_get("policy_interval_secs")?,
        )?,
        timeout_secs: i32_to_u16(
            "external_outbound_probe_policies.timeout_secs",
            row.try_get("policy_timeout_secs")?,
        )?,
        next_run_at_unix_secs: row.try_get("policy_next_run_at_unix_secs")?,
        updated_at_unix_secs: row.try_get("policy_updated_at_unix_secs")?,
    }))
}

fn jittered_interval(outbound_id: &str, interval_secs: u32) -> u32 {
    // Stable per outbound, so replicas calculate the same next value. A 21-bucket spread covers
    // -10% through +10% without needing a random source or another persisted field.
    let hash = outbound_id.bytes().fold(0_u32, |value, byte| {
        value.wrapping_mul(16_777_619) ^ u32::from(byte)
    });
    let percent = i64::from(hash % 21) - 10;
    let delta = i64::from(interval_secs) * percent / 100;
    u32::try_from(i64::from(interval_secs) + delta).unwrap_or(interval_secs)
}

async fn load_serving_topology(pool: &PgPool) -> Result<(ModelSnapshot, u64)> {
    // Tunnel probes need only the last successfully published topology. Loading the composed
    // subscription projection here would also make them depend on the permissions/client
    // checkpoints and on fleet runtime cleanliness, none of which changes an external outbound.
    load_serving_topology_optional(pool).await?.ok_or_else(|| {
        StoreError::Unavailable(
            "尚无已成功发布的 Serving 拓扑；完成首次配置发布后再拨测".to_owned(),
        )
    })
}

async fn load_serving_topology_optional(pool: &PgPool) -> Result<Option<(ModelSnapshot, u64)>> {
    let row = sqlx::query(
        "SELECT topology_revision_id, generation
           FROM subscription_serving_state
          WHERE id = TRUE",
    )
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let revision = i64_to_u64(
        "subscription_serving_state.topology_revision_id",
        row.try_get("topology_revision_id")?,
    )?;
    let generation = i64_to_u64(
        "subscription_serving_state.generation",
        row.try_get("generation")?,
    )?;
    Ok(Some((
        crate::materialize::load_immutable_snapshot(pool, revision).await?,
        generation,
    )))
}

fn apply_serving_target(item: &mut TunnelProbeListItem, outbound: Option<&ExternalOutbound>) {
    let Some(outbound) = outbound else {
        item.supported = false;
        item.unsupported_reason =
            Some("尚未进入 Serving；发布后才能启用定时监测或拨测 Serving".to_owned());
        item.health = TunnelProbeHealth::Unknown;
        return;
    };
    item.name.clone_from(&outbound.name);
    item.protocol = protocol_name(&outbound.protocol).to_owned();
    item.supported = !matches!(
        outbound.protocol,
        ExternalOutboundProtocol::Warp { .. } | ExternalOutboundProtocol::Vpngate { .. }
    );
    item.unsupported_reason = (!item.supported).then(|| match outbound.protocol {
        ExternalOutboundProtocol::Vpngate { .. } => {
            "VPN Gate 由引用它的节点 Agent 实拨；请在 VPN Gate 的观测页查看".to_owned()
        }
        _ => "Console 拨测暂不支持 WARP：需要独立的 Console 身份".to_owned(),
    });
    if !item.supported {
        item.health = TunnelProbeHealth::Unknown;
    }
}

fn health_of(
    policy: Option<&TunnelProbePolicy>,
    recent: &[TunnelProbeRun],
    now_unix_secs: i64,
) -> TunnelProbeHealth {
    let Some(policy) = policy else {
        return TunnelProbeHealth::Paused;
    };
    if !policy.enabled {
        return TunnelProbeHealth::Paused;
    }
    let Some(latest) = recent.first() else {
        return TunnelProbeHealth::Unknown;
    };
    let stale_after = i64::from(policy.interval_secs.saturating_mul(3));
    if latest
        .finished_at_unix_secs
        .is_none_or(|finished| now_unix_secs.saturating_sub(finished) > stale_after)
    {
        return TunnelProbeHealth::Unknown;
    }

    let succeeded = recent
        .iter()
        .filter(|run| run.result == Some(TunnelProbeResultStatus::Ok))
        .count();
    let latest_failed = latest.result != Some(TunnelProbeResultStatus::Ok);
    let consecutive_failures = recent
        .iter()
        .take_while(|run| run.result != Some(TunnelProbeResultStatus::Ok))
        .count();
    if consecutive_failures >= 2 {
        return TunnelProbeHealth::Down;
    }
    let success_rate = succeeded as f64 / recent.len() as f64;
    let slow = recent
        .iter()
        .filter_map(|run| run.ttfb_ms)
        .max()
        .is_some_and(|ttfb| ttfb > 300);
    if latest_failed || success_rate < 0.8 || slow {
        TunnelProbeHealth::Degraded
    } else {
        TunnelProbeHealth::Healthy
    }
}

fn list_item_from_row(
    row: &sqlx::postgres::PgRow,
    recent: &[TunnelProbeRun],
    now_unix_secs: i64,
) -> Result<TunnelProbeListItem> {
    let tenant_id: String = row.try_get("item_tenant_id")?;
    let outbound_id: String = row.try_get("item_outbound_id")?;
    let policy = policy_from_row(row)?;
    let latest_run = row
        .try_get::<Option<i64>, _>("id")?
        .map(|_| run_from_row(row))
        .transpose()?;
    let protocol: String = row.try_get("item_protocol")?;
    let supported = protocol != "warp";
    Ok(TunnelProbeListItem {
        tenant_id,
        outbound_id,
        name: row.try_get("item_name")?,
        protocol,
        supported,
        unsupported_reason: (!supported)
            .then(|| "Console 拨测暂不支持 WARP：需要独立的 Console 身份".to_owned()),
        health: if supported {
            health_of(policy.as_ref(), recent, now_unix_secs)
        } else {
            TunnelProbeHealth::Unknown
        },
        policy,
        latest_run,
    })
}

pub async fn list(pool: &PgPool, actor: &AdminContext) -> Result<TunnelProbeList> {
    let filter = tenant_filter(actor);
    let (scope, pattern) = match &filter {
        Some((scope, pattern)) => (Some(scope.as_str()), Some(pattern.as_str())),
        None => (None, None),
    };
    let rows = sqlx::query(
        "SELECT outbound.tenant_id AS item_tenant_id,
                outbound.id AS item_outbound_id, outbound.name AS item_name,
                outbound.protocol AS item_protocol,
                policy.enabled AS policy_enabled,
                policy.interval_secs AS policy_interval_secs,
                policy.timeout_secs AS policy_timeout_secs,
                extract(epoch FROM policy.next_run_at)::bigint AS policy_next_run_at_unix_secs,
                extract(epoch FROM policy.updated_at)::bigint AS policy_updated_at_unix_secs,
                latest.id, latest.tenant_id, latest.outbound_id, latest.outbound_name,
                latest.protocol, latest.trigger, latest.source,
                latest.topology_revision_id, latest.serving_generation, latest.draft_sha256,
                latest.settings_revision_id, latest.timeout_secs,
                latest.status, latest.phase, latest.result, latest.ttfb_ms,
                latest.http_status, latest.exit_ip, latest.exit_loc, latest.attempt_count,
                latest.error_code, latest.error_detail,
                extract(epoch FROM latest.queued_at)::bigint AS queued_at_unix_secs,
                extract(epoch FROM latest.started_at)::bigint AS started_at_unix_secs,
                extract(epoch FROM latest.finished_at)::bigint AS finished_at_unix_secs,
                (latest.cancel_requested_at IS NOT NULL) AS cancel_requested
           FROM external_outbounds outbound
           LEFT JOIN external_outbound_probe_policies policy ON policy.outbound_id = outbound.id
           LEFT JOIN LATERAL (
                SELECT run.* FROM external_outbound_probe_runs run
                 WHERE run.outbound_id = outbound.id
                   AND run.tenant_id = outbound.tenant_id
                   AND run.source = 'serving'
                 ORDER BY run.queued_at DESC, run.id DESC LIMIT 1
           ) latest ON TRUE
          WHERE $1::text IS NULL
             OR outbound.tenant_id = $1
             OR outbound.tenant_id LIKE $2 ESCAPE '\\'
          ORDER BY outbound.tenant_id, outbound.name, outbound.id",
    )
    .bind(scope)
    .bind(pattern)
    .fetch_all(pool)
    .await?;

    let sample_rows = sqlx::query(
        "SELECT * FROM (
             SELECT run.id, run.tenant_id, run.outbound_id, run.outbound_name, run.protocol,
                    run.trigger, run.source, run.topology_revision_id, run.serving_generation,
                    run.draft_sha256,
                    run.settings_revision_id, run.timeout_secs, run.status, run.phase, run.result,
                    run.ttfb_ms, run.http_status, run.exit_ip, run.exit_loc, run.attempt_count,
                    run.error_code, run.error_detail,
                    extract(epoch FROM run.queued_at)::bigint AS queued_at_unix_secs,
                    extract(epoch FROM run.started_at)::bigint AS started_at_unix_secs,
                    extract(epoch FROM run.finished_at)::bigint AS finished_at_unix_secs,
                    (run.cancel_requested_at IS NOT NULL) AS cancel_requested,
                    row_number() OVER (
                        PARTITION BY run.tenant_id, run.outbound_id
                        ORDER BY run.finished_at DESC, run.id DESC
                    ) AS sample_rank
               FROM external_outbound_probe_runs run
              WHERE run.finished_at IS NOT NULL
                AND run.source = 'serving'
                AND run.result IN ('ok', 'timeout', 'connect-failed', 'target-failed')
                AND run.finished_at >= now() - interval '7 days'
                AND ($1::text IS NULL OR run.tenant_id = $1 OR run.tenant_id LIKE $2 ESCAPE '\\')
         ) recent
         WHERE sample_rank <= 5
         ORDER BY tenant_id, outbound_id, finished_at_unix_secs DESC, id DESC",
    )
    .bind(scope)
    .bind(pattern)
    .fetch_all(pool)
    .await?;
    let mut recent = BTreeMap::<(String, String), Vec<TunnelProbeRun>>::new();
    for row in &sample_rows {
        let run = run_from_row(row)?;
        recent
            .entry((run.tenant_id.clone(), run.outbound_id.clone()))
            .or_default()
            .push(run);
    }
    let now_unix_secs: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(pool)
        .await?;
    let settings = crate::settings::load_settings(pool).await?;
    let serving = load_serving_topology_optional(pool).await?;

    let mut items = Vec::with_capacity(rows.len());
    for row in &rows {
        let tenant_id: String = row.try_get("item_tenant_id")?;
        let outbound_id: String = row.try_get("item_outbound_id")?;
        let samples = recent
            .get(&(tenant_id.clone(), outbound_id.clone()))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut item = list_item_from_row(row, samples, now_unix_secs)?;
        let serving_outbound = serving.as_ref().and_then(|(snapshot, _)| {
            snapshot
                .external_outbounds
                .iter()
                .find(|outbound| outbound.tenant == tenant_id && outbound.id == outbound_id)
        });
        apply_serving_target(&mut item, serving_outbound);
        if item.supported {
            items.push(item);
        }
    }

    Ok(TunnelProbeList {
        origin: "console",
        endpoint_url: settings.probe.endpoint_url,
        retention_days: TUNNEL_PROBE_RETENTION_DAYS,
        items,
    })
}

async fn detail_item(
    pool: &PgPool,
    tenant_id: &str,
    outbound_id: &str,
) -> Result<TunnelProbeListItem> {
    // Detail is polled while a run is active. Keep this lookup proportional to one tunnel rather
    // than routing it through `list`, whose health query intentionally covers every visible item.
    let row = sqlx::query(
        "SELECT outbound.tenant_id AS item_tenant_id,
                outbound.id AS item_outbound_id, outbound.name AS item_name,
                outbound.protocol AS item_protocol,
                policy.enabled AS policy_enabled,
                policy.interval_secs AS policy_interval_secs,
                policy.timeout_secs AS policy_timeout_secs,
                extract(epoch FROM policy.next_run_at)::bigint AS policy_next_run_at_unix_secs,
                extract(epoch FROM policy.updated_at)::bigint AS policy_updated_at_unix_secs,
                latest.id, latest.tenant_id, latest.outbound_id, latest.outbound_name,
                latest.protocol, latest.trigger, latest.source,
                latest.topology_revision_id, latest.serving_generation, latest.draft_sha256,
                latest.settings_revision_id, latest.timeout_secs,
                latest.status, latest.phase, latest.result, latest.ttfb_ms,
                latest.http_status, latest.exit_ip, latest.exit_loc, latest.attempt_count,
                latest.error_code, latest.error_detail,
                extract(epoch FROM latest.queued_at)::bigint AS queued_at_unix_secs,
                extract(epoch FROM latest.started_at)::bigint AS started_at_unix_secs,
                extract(epoch FROM latest.finished_at)::bigint AS finished_at_unix_secs,
                (latest.cancel_requested_at IS NOT NULL) AS cancel_requested,
                extract(epoch FROM now())::bigint AS now_unix_secs
           FROM external_outbounds outbound
           LEFT JOIN external_outbound_probe_policies policy ON policy.outbound_id = outbound.id
           LEFT JOIN LATERAL (
                SELECT run.* FROM external_outbound_probe_runs run
                 WHERE run.tenant_id = outbound.tenant_id
                   AND run.outbound_id = outbound.id
                 ORDER BY run.queued_at DESC, run.id DESC LIMIT 1
           ) latest ON TRUE
          WHERE outbound.tenant_id = $1 AND outbound.id = $2",
    )
    .bind(tenant_id)
    .bind(outbound_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        // A tunnel created only in the browser draft has no mutable-table row. Once its first
        // draft probe is queued, the copied display facts on that durable run keep the progress
        // and history page addressable without turning the draft itself into stored model state.
        let latest_id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM external_outbound_probe_runs
              WHERE tenant_id = $1 AND outbound_id = $2
              ORDER BY queued_at DESC, id DESC LIMIT 1",
        )
        .bind(tenant_id)
        .bind(outbound_id)
        .fetch_optional(pool)
        .await?;
        let latest = run_by_id(
            pool,
            latest_id
                .ok_or_else(|| StoreError::NotFound(format!("tunnel {tenant_id}/{outbound_id}")))?,
        )
        .await?;
        let supported = latest.protocol != "warp";
        return Ok(TunnelProbeListItem {
            tenant_id: tenant_id.to_owned(),
            outbound_id: outbound_id.to_owned(),
            name: latest.outbound_name.clone(),
            protocol: latest.protocol.clone(),
            supported,
            unsupported_reason: (!supported)
                .then(|| "Console 拨测暂不支持 WARP：需要独立的 Console 身份".to_owned()),
            health: TunnelProbeHealth::Paused,
            policy: None,
            latest_run: Some(latest),
        });
    };
    let sample_rows = sqlx::query(
        "SELECT id, tenant_id, outbound_id, outbound_name, protocol, trigger, source,
                topology_revision_id, serving_generation, draft_sha256,
                settings_revision_id, timeout_secs,
                status, phase, result, ttfb_ms, http_status, exit_ip, exit_loc, attempt_count,
                error_code, error_detail,
                extract(epoch FROM queued_at)::bigint AS queued_at_unix_secs,
                extract(epoch FROM started_at)::bigint AS started_at_unix_secs,
                extract(epoch FROM finished_at)::bigint AS finished_at_unix_secs,
                (cancel_requested_at IS NOT NULL) AS cancel_requested
           FROM external_outbound_probe_runs
          WHERE tenant_id = $1 AND outbound_id = $2 AND finished_at IS NOT NULL
            AND source = 'serving'
            AND result IN ('ok', 'timeout', 'connect-failed', 'target-failed')
            AND finished_at >= now() - interval '7 days'
          ORDER BY finished_at DESC, id DESC LIMIT 5",
    )
    .bind(tenant_id)
    .bind(outbound_id)
    .fetch_all(pool)
    .await?;
    let recent = sample_rows
        .iter()
        .map(run_from_row)
        .collect::<Result<Vec<_>>>()?;
    list_item_from_row(&row, &recent, row.try_get("now_unix_secs")?)
}

pub async fn detail(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    outbound_id: &str,
    window_secs: u32,
) -> Result<TunnelProbeView> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let outbound_id = required_text(outbound_id, "outbound_id")?;
    actor.require_tenant_access(&tenant_id, "tunnel probe")?;
    let mut item = detail_item(pool, &tenant_id, &outbound_id).await?;
    let serving = load_serving_topology_optional(pool).await?;
    let serving_outbound = serving.as_ref().and_then(|(snapshot, _)| {
        snapshot
            .external_outbounds
            .iter()
            .find(|outbound| outbound.tenant == tenant_id && outbound.id == outbound_id)
    });
    apply_serving_target(&mut item, serving_outbound);

    let window_secs = window_secs.clamp(3_600, 7 * 86_400);
    let summary = sqlx::query(
        "SELECT count(*)::bigint AS total,
                count(*) FILTER (WHERE result = 'ok')::bigint AS succeeded,
                percentile_disc(0.5) WITHIN GROUP (ORDER BY ttfb_ms)
                    FILTER (WHERE result = 'ok') AS p50_ms,
                percentile_disc(0.95) WITHIN GROUP (ORDER BY ttfb_ms)
                    FILTER (WHERE result = 'ok') AS p95_ms
           FROM external_outbound_probe_runs
          WHERE tenant_id = $1 AND outbound_id = $2 AND finished_at IS NOT NULL
            AND source = 'serving'
            AND result IN ('ok', 'timeout', 'connect-failed', 'target-failed')
            AND finished_at >= now() - make_interval(secs => $3)",
    )
    .bind(&tenant_id)
    .bind(&outbound_id)
    .bind(i32::try_from(window_secs).unwrap_or(7 * 86_400))
    .fetch_one(pool)
    .await?;
    let total = i64_to_u64("tunnel probe total", summary.try_get("total")?)?;
    let succeeded = i64_to_u64("tunnel probe succeeded", summary.try_get("succeeded")?)?;
    let bucket_secs = i32::try_from((window_secs / 240).max(60)).unwrap_or(60);
    let point_rows = sqlx::query(
        "SELECT run_id, finished_at_unix_secs, result, ttfb_ms FROM (
             SELECT id AS run_id, extract(epoch FROM finished_at)::bigint AS finished_at_unix_secs,
                    result, ttfb_ms,
                    row_number() OVER (
                        PARTITION BY floor(extract(epoch FROM finished_at) / $4)
                        ORDER BY (result <> 'ok') DESC, finished_at DESC, id DESC
                    ) AS bucket_rank
               FROM external_outbound_probe_runs
              WHERE tenant_id = $1 AND outbound_id = $2 AND finished_at IS NOT NULL
                AND source = 'serving'
                AND result IN ('ok', 'timeout', 'connect-failed', 'target-failed')
                AND finished_at >= now() - make_interval(secs => $3)
         ) points
         WHERE bucket_rank = 1
         ORDER BY finished_at_unix_secs
         LIMIT 300",
    )
    .bind(&tenant_id)
    .bind(&outbound_id)
    .bind(i32::try_from(window_secs).unwrap_or(7 * 86_400))
    .bind(bucket_secs)
    .fetch_all(pool)
    .await?;
    let points = point_rows
        .iter()
        .map(|row| {
            Ok(TunnelProbePoint {
                run_id: row.try_get("run_id")?,
                finished_at_unix_secs: row.try_get("finished_at_unix_secs")?,
                result: result_status(&row.try_get::<String, _>("result")?)?,
                ttfb_ms: row
                    .try_get::<Option<i32>, _>("ttfb_ms")?
                    .map(|value| i32_to_u32("external_outbound_probe_runs.ttfb_ms", value))
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let recent_rows = sqlx::query(
        "SELECT id, tenant_id, outbound_id, outbound_name, protocol, trigger, source,
                topology_revision_id, serving_generation, draft_sha256,
                settings_revision_id, timeout_secs,
                status, phase, result, ttfb_ms, http_status, exit_ip, exit_loc, attempt_count,
                error_code, error_detail,
                extract(epoch FROM queued_at)::bigint AS queued_at_unix_secs,
                extract(epoch FROM started_at)::bigint AS started_at_unix_secs,
                extract(epoch FROM finished_at)::bigint AS finished_at_unix_secs,
                (cancel_requested_at IS NOT NULL) AS cancel_requested
           FROM external_outbound_probe_runs
          WHERE tenant_id = $1 AND outbound_id = $2
          ORDER BY queued_at DESC, id DESC LIMIT 20",
    )
    .bind(&tenant_id)
    .bind(&outbound_id)
    .fetch_all(pool)
    .await?;
    let recent_runs = recent_rows
        .iter()
        .map(run_from_row)
        .collect::<Result<Vec<_>>>()?;

    Ok(TunnelProbeView {
        item,
        retention_days: TUNNEL_PROBE_RETENTION_DAYS,
        summary: TunnelProbeSummary {
            window_secs,
            total,
            succeeded,
            success_rate: (total > 0).then(|| succeeded as f64 / total as f64),
            p50_ms: summary
                .try_get::<Option<i32>, _>("p50_ms")?
                .map(|value| i32_to_u32("tunnel probe p50", value))
                .transpose()?,
            p95_ms: summary
                .try_get::<Option<i32>, _>("p95_ms")?
                .map(|value| i32_to_u32("tunnel probe p95", value))
                .transpose()?,
            failures: total.saturating_sub(succeeded),
        },
        points,
        recent_runs,
    })
}

pub async fn update_policy(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    outbound_id: &str,
    request: UpdateTunnelProbePolicy,
) -> Result<TunnelProbePolicy> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let outbound_id = required_text(outbound_id, "outbound_id")?;
    actor.require_tenant_access(&tenant_id, "tunnel probe policy")?;
    if !matches!(request.interval_secs, 60 | 300 | 900 | 1800 | 3600) {
        return Err(StoreError::InvalidData(
            "tunnel probe interval must be one of 60, 300, 900, 1800 or 3600 seconds".to_owned(),
        ));
    }
    if !(1..=120).contains(&request.timeout_secs) {
        return Err(StoreError::InvalidData(
            "tunnel probe timeout must be between 1 and 120 seconds".to_owned(),
        ));
    }
    let serving = load_serving_topology_optional(pool).await?;
    let serving_protocol = serving.as_ref().and_then(|(snapshot, _)| {
        snapshot
            .external_outbounds
            .iter()
            .find(|outbound| outbound.tenant == tenant_id && outbound.id == outbound_id)
            .map(|outbound| protocol_name(&outbound.protocol))
    });
    let protocol = match serving_protocol {
        Some(protocol) => protocol.to_owned(),
        None => sqlx::query_scalar(
            "SELECT protocol FROM external_outbounds WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&tenant_id)
        .bind(&outbound_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("tunnel {tenant_id}/{outbound_id}")))?,
    };
    if protocol == "warp" && request.enabled {
        return Err(StoreError::Unsupported(
            "Console 拨测暂不支持 WARP：需要独立的 Console 身份".to_owned(),
        ));
    }
    let next_secs = jittered_interval(&outbound_id, request.interval_secs);
    let row = sqlx::query(
        "INSERT INTO external_outbound_probe_policies
             (outbound_id, enabled, interval_secs, timeout_secs, next_run_at, updated_by)
         VALUES ($1, $2, $3, $4,
                 CASE WHEN $2 THEN now() + make_interval(secs => $5) ELSE NULL END, $6)
         ON CONFLICT (outbound_id) DO UPDATE SET
             enabled = EXCLUDED.enabled,
             interval_secs = EXCLUDED.interval_secs,
             timeout_secs = EXCLUDED.timeout_secs,
             next_run_at = CASE
                 WHEN EXCLUDED.enabled AND (
                     NOT external_outbound_probe_policies.enabled
                     OR external_outbound_probe_policies.interval_secs <> EXCLUDED.interval_secs
                 ) THEN EXCLUDED.next_run_at
                 WHEN EXCLUDED.enabled THEN external_outbound_probe_policies.next_run_at
                 ELSE NULL
             END,
             updated_by = EXCLUDED.updated_by,
             updated_at = now()
         RETURNING enabled AS policy_enabled,
                   interval_secs AS policy_interval_secs,
                   timeout_secs AS policy_timeout_secs,
                   extract(epoch FROM next_run_at)::bigint AS policy_next_run_at_unix_secs,
                   extract(epoch FROM updated_at)::bigint AS policy_updated_at_unix_secs",
    )
    .bind(&outbound_id)
    .bind(request.enabled)
    .bind(i32::try_from(request.interval_secs).unwrap_or(3600))
    .bind(i32::from(request.timeout_secs))
    .bind(i32::try_from(next_secs).unwrap_or(3600))
    .bind(actor.operator_id())
    .fetch_one(pool)
    .await?;
    policy_from_row(&row)?.ok_or_else(|| {
        StoreError::InvalidData("updated tunnel probe policy disappeared".to_owned())
    })
}

pub async fn start_manual(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    outbound_id: &str,
) -> Result<(TunnelProbeRun, bool)> {
    start_manual_from(
        pool,
        actor,
        tenant_id,
        outbound_id,
        TunnelProbeSource::Serving,
        Vec::new(),
    )
    .await
}

pub async fn start_manual_from(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    outbound_id: &str,
    source: TunnelProbeSource,
    draft_ops: Vec<crate::ModelOp>,
) -> Result<(TunnelProbeRun, bool)> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let outbound_id = required_text(outbound_id, "outbound_id")?;
    actor.require_tenant_access(&tenant_id, "tunnel probe")?;

    let (outbound, topology_revision, serving_generation) = match source {
        TunnelProbeSource::Serving => {
            if !draft_ops.is_empty() {
                return Err(StoreError::InvalidData(
                    "a Serving tunnel probe must not include draft operations".to_owned(),
                ));
            }
            let (serving, serving_generation) = load_serving_topology(pool).await?;
            let topology_revision = serving.revision;
            let outbound = serving
                .external_outbounds
                .into_iter()
                .find(|outbound| outbound.tenant == tenant_id && outbound.id == outbound_id)
                .ok_or_else(|| {
                    StoreError::NotFound(format!(
                        "serving tunnel {tenant_id}/{outbound_id}; publish it before probing"
                    ))
                })?;
            (outbound, topology_revision, Some(serving_generation))
        }
        TunnelProbeSource::Draft => {
            let snapshot = crate::draft::draft_snapshot(pool, actor, draft_ops).await?;
            let topology_revision = snapshot.revision;
            let outbound = snapshot
                .external_outbounds
                .into_iter()
                .find(|outbound| outbound.tenant == tenant_id && outbound.id == outbound_id)
                .ok_or_else(|| {
                    StoreError::NotFound(format!(
                        "draft tunnel {tenant_id}/{outbound_id}; restore it before probing"
                    ))
                })?;
            (outbound, topology_revision, None)
        }
    };
    if matches!(outbound.protocol, ExternalOutboundProtocol::Warp { .. }) {
        return Err(StoreError::Unsupported(
            "Console 拨测暂不支持 WARP：需要独立的 Console 身份".to_owned(),
        ));
    }
    if matches!(outbound.protocol, ExternalOutboundProtocol::Vpngate { .. }) {
        return Err(StoreError::Unsupported(
            "VPN Gate 由引用它的节点 Agent 实拨；请在 VPN Gate 的观测页查看".to_owned(),
        ));
    }
    let settings = crate::settings::load_settings_snapshot(pool).await?;
    let timeout: Option<i32> = sqlx::query_scalar(
        "SELECT timeout_secs FROM external_outbound_probe_policies WHERE outbound_id = $1",
    )
    .bind(&outbound_id)
    .fetch_optional(pool)
    .await?;
    let timeout = timeout.unwrap_or(10).clamp(1, 120);
    let id: i64 = sqlx::query_scalar(
        "SELECT nextval(pg_get_serial_sequence('external_outbound_probe_runs', 'id'))",
    )
    .fetch_one(pool)
    .await?;
    let (draft_sha256, outbound_sealed) = if source == TunnelProbeSource::Draft {
        let plaintext = serde_json::to_string(&outbound)?;
        let fingerprint =
            crate::secrets::tunnel_probe_draft_fingerprint(&tenant_id, &outbound_id, &plaintext)?;
        let sealed = crate::secrets::seal(
            &crate::secrets::tunnel_probe_draft_context(id, &tenant_id, &outbound_id),
            &plaintext,
        )?;
        (Some(fingerprint), Some(sealed))
    } else {
        (None, None)
    };
    let inserted: Option<i64> = sqlx::query_scalar(
        "INSERT INTO external_outbound_probe_runs
             (id, tenant_id, outbound_id, outbound_name, protocol, trigger, source,
              topology_revision_id, serving_generation, draft_sha256, outbound_sealed,
              settings_revision_id, endpoint_url, timeout_secs, requested_by)
         VALUES ($1, $2, $3, $4, $5, 'manual', $6, $7, $8, $9, $10, $11, $12, $13, $14)
         ON CONFLICT (tenant_id, outbound_id, source)
             WHERE status IN ('queued', 'running') DO NOTHING
         RETURNING id",
    )
    .bind(id)
    .bind(&tenant_id)
    .bind(&outbound_id)
    .bind(&outbound.name)
    .bind(protocol_name(&outbound.protocol))
    .bind(source.as_str())
    .bind(i64::try_from(topology_revision).map_err(|_| {
        StoreError::InvalidData("probe topology revision is outside i64".to_owned())
    })?)
    .bind(
        serving_generation
            .map(i64::try_from)
            .transpose()
            .map_err(|_| StoreError::InvalidData("serving generation is outside i64".to_owned()))?,
    )
    .bind(&draft_sha256)
    .bind(&outbound_sealed)
    .bind(
        i64::try_from(settings.revision_id)
            .map_err(|_| StoreError::InvalidData("settings revision is outside i64".to_owned()))?,
    )
    .bind(&settings.settings.probe.endpoint_url)
    .bind(timeout)
    .bind(actor.operator_id())
    .fetch_optional(pool)
    .await?;
    let (run_id, reused) = match inserted {
        Some(run_id) => (run_id, false),
        None => {
            let run_id = sqlx::query_scalar(
                "SELECT id FROM external_outbound_probe_runs
                  WHERE tenant_id = $1 AND outbound_id = $2
                    AND source = $3
                    AND status IN ('queued', 'running')
                  ORDER BY queued_at DESC, id DESC LIMIT 1",
            )
            .bind(&tenant_id)
            .bind(&outbound_id)
            .bind(source.as_str())
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| {
                StoreError::Conflict(
                    "another tunnel probe changed state; retry the request".to_owned(),
                )
            })?;
            (run_id, true)
        }
    };
    let run = run_by_id(pool, run_id).await?;
    if run.tenant_id != tenant_id || run.outbound_id != outbound_id || run.source != source {
        return Err(StoreError::Conflict(
            "active tunnel probe identity changed; retry the request".to_owned(),
        ));
    }
    if reused
        && (run.topology_revision != topology_revision
            || run.serving_generation != serving_generation
            || run.draft_sha256 != draft_sha256
            || run.settings_revision != settings.revision_id
            || i32::from(run.timeout_secs) != timeout)
    {
        return Err(StoreError::Conflict(
            "another probe for an older version of this target is still active; cancel it or wait for it to finish"
                .to_owned(),
        ));
    }
    Ok((run, reused))
}

async fn run_by_id(pool: &PgPool, id: i64) -> Result<TunnelProbeRun> {
    let row = sqlx::query(
        "SELECT id, tenant_id, outbound_id, outbound_name, protocol, trigger, source,
                topology_revision_id, serving_generation, draft_sha256,
                settings_revision_id, timeout_secs,
                status, phase, result, ttfb_ms, http_status, exit_ip, exit_loc, attempt_count,
                error_code, error_detail,
                extract(epoch FROM queued_at)::bigint AS queued_at_unix_secs,
                extract(epoch FROM started_at)::bigint AS started_at_unix_secs,
                extract(epoch FROM finished_at)::bigint AS finished_at_unix_secs,
                (cancel_requested_at IS NOT NULL) AS cancel_requested
           FROM external_outbound_probe_runs WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("tunnel probe {id}")))?;
    run_from_row(&row)
}

pub async fn get_run(pool: &PgPool, actor: &AdminContext, id: i64) -> Result<TunnelProbeRun> {
    let run = run_by_id(pool, id).await?;
    actor.require_tenant_access(&run.tenant_id, "tunnel probe")?;
    Ok(run)
}

pub async fn cancel_run(pool: &PgPool, actor: &AdminContext, id: i64) -> Result<TunnelProbeRun> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT tenant_id, status FROM external_outbound_probe_runs WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("tunnel probe {id}")))?;
    actor.require_tenant_access(&row.try_get::<String, _>("tenant_id")?, "tunnel probe")?;
    let status: String = row.try_get("status")?;
    if status == "queued" {
        sqlx::query(
            "UPDATE external_outbound_probe_runs
                SET status = 'canceled', phase = 'finished', result = 'canceled',
                    error_code = 'canceled', error_detail = '操作者取消了拨测',
                    cancel_requested_at = now(), finished_at = now(), outbound_sealed = NULL
              WHERE id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    } else if status == "running" {
        sqlx::query(
            "UPDATE external_outbound_probe_runs
                SET cancel_requested_at = COALESCE(cancel_requested_at, now()) WHERE id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    run_by_id(pool, id).await
}

pub async fn enqueue_due(pool: &PgPool) -> Result<u64> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SCHEDULER_ADVISORY_LOCK)
        .execute(&mut *tx)
        .await?;
    let rows = sqlx::query(
        "SELECT policy.outbound_id, policy.interval_secs, policy.timeout_secs,
                extract(epoch FROM policy.next_run_at)::bigint AS scheduled_for_unix_secs,
                outbound.tenant_id AS policy_tenant_id
           FROM external_outbound_probe_policies policy
           JOIN external_outbounds outbound ON outbound.id = policy.outbound_id
          WHERE policy.enabled AND policy.next_run_at <= now()
          ORDER BY policy.next_run_at, policy.outbound_id
          LIMIT $1 FOR UPDATE OF policy SKIP LOCKED",
    )
    .bind(MAX_DUE_PER_TICK)
    .fetch_all(&mut *tx)
    .await?;
    if rows.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }
    let serving = sqlx::query(
        "SELECT topology_revision_id, generation FROM subscription_serving_state WHERE id = TRUE",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let serving_snapshot = match serving.as_ref() {
        Some(row) => Some(
            crate::materialize::load_immutable_snapshot_tx(
                &mut tx,
                i64_to_u64(
                    "subscription_serving_state.topology_revision_id",
                    row.try_get("topology_revision_id")?,
                )?,
            )
            .await?,
        ),
        None => None,
    };
    let settings = sqlx::query(
        "SELECT current_revision, probe_endpoint_url FROM control_state WHERE id = TRUE",
    )
    .fetch_one(&mut *tx)
    .await?;
    let mut inserted = 0_u64;
    for row in rows {
        let outbound_id: String = row.try_get("outbound_id")?;
        let policy_tenant_id: String = row.try_get("policy_tenant_id")?;
        let interval_secs: i32 = row.try_get("interval_secs")?;
        let next_secs = jittered_interval(
            &outbound_id,
            i32_to_u32(
                "external_outbound_probe_policies.interval_secs",
                interval_secs,
            )?,
        );
        sqlx::query(
            "UPDATE external_outbound_probe_policies
                SET next_run_at = now() + make_interval(secs => $2)
              WHERE outbound_id = $1",
        )
        .bind(&outbound_id)
        .bind(i32::try_from(next_secs).unwrap_or(interval_secs))
        .execute(&mut *tx)
        .await?;
        let Some(serving) = serving.as_ref() else {
            continue;
        };
        let Some(outbound) = serving_snapshot.as_ref().and_then(|snapshot| {
            snapshot
                .external_outbounds
                .iter()
                .find(|outbound| outbound.id == outbound_id && outbound.tenant == policy_tenant_id)
        }) else {
            // A policy may be configured before its tunnel is published. Keep the
            // next slot moving, but never claim that the mutable head was tested as
            // though it were the active data plane.
            continue;
        };
        if matches!(
            outbound.protocol,
            ExternalOutboundProtocol::Warp { .. } | ExternalOutboundProtocol::Vpngate { .. }
        ) {
            continue;
        }
        let affected = sqlx::query(
            "INSERT INTO external_outbound_probe_runs
                 (tenant_id, outbound_id, outbound_name, protocol, trigger, source, scheduled_for,
                  topology_revision_id, serving_generation, settings_revision_id,
                  endpoint_url, timeout_secs)
             VALUES ($1, $2, $3, $4, 'scheduled', 'serving', to_timestamp($5), $6, $7, $8, $9, $10)
             ON CONFLICT DO NOTHING",
        )
        .bind(&outbound.tenant)
        .bind(&outbound_id)
        .bind(&outbound.name)
        .bind(protocol_name(&outbound.protocol))
        .bind(row.try_get::<i64, _>("scheduled_for_unix_secs")?)
        .bind(serving.try_get::<i64, _>("topology_revision_id")?)
        .bind(serving.try_get::<i64, _>("generation")?)
        .bind(settings.try_get::<i64, _>("current_revision")?)
        .bind(settings.try_get::<String, _>("probe_endpoint_url")?)
        .bind(row.try_get::<i32, _>("timeout_secs")?)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        inserted = inserted.saturating_add(affected);
    }
    tx.commit().await?;
    Ok(inserted)
}

pub async fn claim_next(pool: &PgPool, owner: &str) -> Result<Option<ClaimedTunnelProbe>> {
    let owner = required_text(owner, "tunnel probe lease owner")?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SCHEDULER_ADVISORY_LOCK)
        .execute(&mut *tx)
        .await?;
    reap_expired_tx(&mut tx).await?;
    sqlx::query(
        "UPDATE external_outbound_probe_runs
            SET status = 'canceled', phase = 'finished', result = 'canceled',
                error_code = 'canceled', error_detail = '操作者取消了拨测', finished_at = now(),
                outbound_sealed = NULL
          WHERE status = 'queued' AND cancel_requested_at IS NOT NULL",
    )
    .execute(&mut *tx)
    .await?;
    let running: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM external_outbound_probe_runs
          WHERE status = 'running' AND lease_until >= now()",
    )
    .fetch_one(&mut *tx)
    .await?;
    if running >= TUNNEL_PROBE_GLOBAL_CONCURRENCY {
        tx.commit().await?;
        return Ok(None);
    }
    let candidate: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM external_outbound_probe_runs
          WHERE status = 'queued'
          ORDER BY (trigger = 'manual') DESC, queued_at, id
          LIMIT 1 FOR UPDATE SKIP LOCKED",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(id) = candidate else {
        tx.commit().await?;
        return Ok(None);
    };
    let row = sqlx::query(
        "UPDATE external_outbound_probe_runs
            SET status = 'running', phase = 'preparing', started_at = COALESCE(started_at, now()),
                lease_owner = $2, lease_generation = lease_generation + 1,
                lease_until = now() + make_interval(secs => $3)
          WHERE id = $1
          RETURNING id, tenant_id, outbound_id, outbound_name, protocol, trigger, source,
                    topology_revision_id, serving_generation, draft_sha256,
                    settings_revision_id, timeout_secs,
                    status, phase, result, ttfb_ms, http_status, exit_ip, exit_loc, attempt_count,
                    error_code, error_detail, endpoint_url, lease_generation,
                    extract(epoch FROM queued_at)::bigint AS queued_at_unix_secs,
                    extract(epoch FROM started_at)::bigint AS started_at_unix_secs,
                    extract(epoch FROM finished_at)::bigint AS finished_at_unix_secs,
                    (cancel_requested_at IS NOT NULL) AS cancel_requested",
    )
    .bind(id)
    .bind(&owner)
    .bind(TUNNEL_PROBE_LEASE_SECS)
    .fetch_one(&mut *tx)
    .await?;
    let run = run_from_row(&row)?;
    let endpoint_url = row.try_get("endpoint_url")?;
    let lease_generation = i64_to_u64(
        "external_outbound_probe_runs.lease_generation",
        row.try_get("lease_generation")?,
    )?;
    tx.commit().await?;
    Ok(Some(ClaimedTunnelProbe {
        run,
        endpoint_url,
        lease_owner: owner,
        lease_generation,
    }))
}

async fn reap_expired_tx(tx: &mut Transaction<'_, Postgres>) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE external_outbound_probe_runs
            SET status = 'failed', phase = 'finished', result = 'interrupted',
                error_code = 'worker-interrupted',
                error_detail = 'Console worker stopped before the probe reached a terminal state',
                finished_at = now(), lease_owner = NULL, lease_until = NULL,
                outbound_sealed = NULL
          WHERE status = 'running' AND lease_until < now()",
    )
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

pub async fn reap_expired(pool: &PgPool) -> Result<u64> {
    let mut tx = pool.begin().await?;
    let affected = reap_expired_tx(&mut tx).await?;
    tx.commit().await?;
    Ok(affected)
}

pub async fn claimed_outbound(
    pool: &PgPool,
    claim: &ClaimedTunnelProbe,
) -> Result<ExternalOutbound> {
    let outbound = match claim.run.source {
        TunnelProbeSource::Serving => {
            let snapshot =
                crate::materialize::load_immutable_snapshot(pool, claim.run.topology_revision)
                    .await?;
            snapshot
                .external_outbounds
                .into_iter()
                .find(|outbound| {
                    outbound.tenant == claim.run.tenant_id && outbound.id == claim.run.outbound_id
                })
                .ok_or_else(|| {
                    StoreError::InvalidData(format!(
                        "frozen revision {} does not contain tunnel {}/{}",
                        claim.run.topology_revision, claim.run.tenant_id, claim.run.outbound_id
                    ))
                })?
        }
        TunnelProbeSource::Draft => {
            let sealed: Option<String> = sqlx::query_scalar(
                "SELECT outbound_sealed FROM external_outbound_probe_runs
                  WHERE id = $1 AND status = 'running'
                    AND lease_owner = $2 AND lease_generation = $3",
            )
            .bind(claim.run.id)
            .bind(&claim.lease_owner)
            .bind(i64::try_from(claim.lease_generation).unwrap_or(i64::MAX))
            .fetch_optional(pool)
            .await?
            .flatten();
            let sealed = sealed.ok_or_else(|| {
                StoreError::InvalidData(format!(
                    "draft tunnel probe {} has no frozen outbound",
                    claim.run.id
                ))
            })?;
            let plaintext = crate::secrets::open(
                &crate::secrets::tunnel_probe_draft_context(
                    claim.run.id,
                    &claim.run.tenant_id,
                    &claim.run.outbound_id,
                ),
                &sealed,
            )?;
            let fingerprint = crate::secrets::tunnel_probe_draft_fingerprint(
                &claim.run.tenant_id,
                &claim.run.outbound_id,
                &plaintext,
            )?;
            if claim.run.draft_sha256.as_deref() != Some(fingerprint.as_str()) {
                return Err(StoreError::InvalidData(format!(
                    "draft tunnel probe {} fingerprint does not match its frozen outbound",
                    claim.run.id
                )));
            }
            serde_json::from_str::<ExternalOutbound>(&plaintext)?
        }
    };
    if outbound.tenant != claim.run.tenant_id
        || outbound.id != claim.run.outbound_id
        || protocol_name(&outbound.protocol) != claim.run.protocol
    {
        return Err(StoreError::InvalidData(format!(
            "frozen tunnel probe {} identity does not match its run row",
            claim.run.id
        )));
    }
    Ok(outbound)
}

pub async fn update_phase(
    pool: &PgPool,
    claim: &ClaimedTunnelProbe,
    next: TunnelProbePhase,
) -> Result<bool> {
    let affected = sqlx::query(
        "UPDATE external_outbound_probe_runs SET phase = $4
          WHERE id = $1 AND status = 'running' AND lease_owner = $2 AND lease_generation = $3",
    )
    .bind(claim.run.id)
    .bind(&claim.lease_owner)
    .bind(i64::try_from(claim.lease_generation).unwrap_or(i64::MAX))
    .bind(next.as_str())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected == 1)
}

pub async fn set_config_sha256(
    pool: &PgPool,
    claim: &ClaimedTunnelProbe,
    sha256: &str,
) -> Result<bool> {
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StoreError::InvalidData(
            "tunnel probe config fingerprint must be 64 hex characters".to_owned(),
        ));
    }
    let affected = sqlx::query(
        "UPDATE external_outbound_probe_runs SET config_sha256 = lower($4)
          WHERE id = $1 AND status = 'running' AND lease_owner = $2 AND lease_generation = $3",
    )
    .bind(claim.run.id)
    .bind(&claim.lease_owner)
    .bind(i64::try_from(claim.lease_generation).unwrap_or(i64::MAX))
    .bind(sha256)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected == 1)
}

pub async fn renew_lease(pool: &PgPool, claim: &ClaimedTunnelProbe) -> Result<bool> {
    let affected = sqlx::query(
        "UPDATE external_outbound_probe_runs
            SET lease_until = now() + make_interval(secs => $4)
          WHERE id = $1 AND status = 'running' AND lease_owner = $2 AND lease_generation = $3",
    )
    .bind(claim.run.id)
    .bind(&claim.lease_owner)
    .bind(i64::try_from(claim.lease_generation).unwrap_or(i64::MAX))
    .bind(TUNNEL_PROBE_LEASE_SECS)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected == 1)
}

pub async fn cancel_requested(pool: &PgPool, claim: &ClaimedTunnelProbe) -> Result<bool> {
    let requested: Option<bool> = sqlx::query_scalar(
        "SELECT cancel_requested_at IS NOT NULL
           FROM external_outbound_probe_runs
          WHERE id = $1 AND status = 'running' AND lease_owner = $2 AND lease_generation = $3",
    )
    .bind(claim.run.id)
    .bind(&claim.lease_owner)
    .bind(i64::try_from(claim.lease_generation).unwrap_or(i64::MAX))
    .fetch_optional(pool)
    .await?;
    Ok(requested.unwrap_or(true))
}

pub async fn complete(
    pool: &PgPool,
    claim: &ClaimedTunnelProbe,
    completion: TunnelProbeCompletion,
) -> Result<bool> {
    if completion.status.is_active() {
        return Err(StoreError::InvalidData(
            "tunnel probe completion must be terminal".to_owned(),
        ));
    }
    let affected = sqlx::query(
        "UPDATE external_outbound_probe_runs
            SET status = $4, phase = 'finished', result = $5, ttfb_ms = $6,
                http_status = $7, exit_ip = $8, exit_loc = $9, attempt_count = $10,
                error_code = $11, error_detail = $12, finished_at = now(),
                lease_owner = NULL, lease_until = NULL, outbound_sealed = NULL
          WHERE id = $1 AND status = 'running' AND lease_owner = $2 AND lease_generation = $3",
    )
    .bind(claim.run.id)
    .bind(&claim.lease_owner)
    .bind(i64::try_from(claim.lease_generation).unwrap_or(i64::MAX))
    .bind(match completion.status {
        TunnelProbeJobStatus::Succeeded => "succeeded",
        TunnelProbeJobStatus::Failed => "failed",
        TunnelProbeJobStatus::Canceled => "canceled",
        TunnelProbeJobStatus::Unsupported => "unsupported",
        TunnelProbeJobStatus::Queued | TunnelProbeJobStatus::Running => unreachable!(),
    })
    .bind(completion.result.as_str())
    .bind(
        completion
            .ttfb_ms
            .map(|value| i32::try_from(value).unwrap_or(i32::MAX)),
    )
    .bind(completion.http_status.map(i32::from))
    .bind(completion.exit_ip)
    .bind(completion.exit_loc)
    .bind(i32::from(completion.attempt_count))
    .bind(completion.error_code)
    .bind(completion.error_detail)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected == 1)
}

pub async fn fail_claim(
    pool: &PgPool,
    claim: &ClaimedTunnelProbe,
    code: &str,
    detail: &str,
) -> Result<bool> {
    complete(
        pool,
        claim,
        TunnelProbeCompletion {
            status: TunnelProbeJobStatus::Unsupported,
            result: TunnelProbeResultStatus::Unsupported,
            ttfb_ms: None,
            http_status: None,
            exit_ip: None,
            exit_loc: None,
            attempt_count: 0,
            error_code: Some(code.to_owned()),
            error_detail: Some(detail.to_owned()),
        },
    )
    .await
}

pub async fn prune(pool: &PgPool, retain_days: u32) -> Result<u64> {
    let retain_days = retain_days.clamp(1, 365);
    Ok(sqlx::query(
        "DELETE FROM external_outbound_probe_runs WHERE id IN (
             SELECT id FROM external_outbound_probe_runs
              WHERE finished_at < now() - make_interval(days => $1)
              ORDER BY finished_at LIMIT $2
         )",
    )
    .bind(i32::try_from(retain_days).unwrap_or(365))
    .bind(PRUNE_BATCH)
    .execute(pool)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(result: TunnelProbeResultStatus, ttfb_ms: Option<u32>, finished: i64) -> TunnelProbeRun {
        TunnelProbeRun {
            id: finished,
            tenant_id: "platform".to_owned(),
            outbound_id: "edge".to_owned(),
            outbound_name: "Edge".to_owned(),
            protocol: "vless".to_owned(),
            trigger: TunnelProbeTrigger::Scheduled,
            source: TunnelProbeSource::Serving,
            topology_revision: 1,
            serving_generation: Some(1),
            draft_sha256: None,
            settings_revision: 1,
            timeout_secs: 10,
            status: if result == TunnelProbeResultStatus::Ok {
                TunnelProbeJobStatus::Succeeded
            } else {
                TunnelProbeJobStatus::Failed
            },
            phase: TunnelProbePhase::Finished,
            result: Some(result),
            ttfb_ms,
            http_status: None,
            exit_ip: None,
            exit_loc: None,
            attempt_count: 1,
            error_code: None,
            error_detail: None,
            queued_at_unix_secs: finished,
            started_at_unix_secs: Some(finished),
            finished_at_unix_secs: Some(finished),
            cancel_requested: false,
        }
    }

    fn policy(enabled: bool) -> TunnelProbePolicy {
        TunnelProbePolicy {
            enabled,
            interval_secs: 300,
            timeout_secs: 10,
            next_run_at_unix_secs: enabled.then_some(2_000),
            updated_at_unix_secs: 1_000,
        }
    }

    #[test]
    fn health_requires_two_consecutive_failures_before_down() {
        let now = 1_500;
        assert_eq!(
            health_of(
                Some(&policy(true)),
                &[run(TunnelProbeResultStatus::Timeout, None, 1_490)],
                now,
            ),
            TunnelProbeHealth::Degraded
        );
        assert_eq!(
            health_of(
                Some(&policy(true)),
                &[
                    run(TunnelProbeResultStatus::Timeout, None, 1_490),
                    run(TunnelProbeResultStatus::Ok, Some(80), 1_200)
                ],
                now,
            ),
            TunnelProbeHealth::Degraded
        );
        assert_eq!(
            health_of(
                Some(&policy(true)),
                &[
                    run(TunnelProbeResultStatus::Timeout, None, 1_490),
                    run(TunnelProbeResultStatus::ConnectFailed, None, 1_480),
                    run(TunnelProbeResultStatus::Ok, Some(80), 1_200),
                ],
                now,
            ),
            TunnelProbeHealth::Down
        );
    }

    #[test]
    fn health_distinguishes_paused_stale_and_slow() {
        assert_eq!(health_of(None, &[], 1_500), TunnelProbeHealth::Paused);
        assert_eq!(
            health_of(Some(&policy(false)), &[], 1_500),
            TunnelProbeHealth::Paused
        );
        assert_eq!(
            health_of(
                Some(&policy(true)),
                &[run(TunnelProbeResultStatus::Ok, Some(80), 1)],
                2_000,
            ),
            TunnelProbeHealth::Unknown
        );
        assert_eq!(
            health_of(
                Some(&policy(true)),
                &[run(TunnelProbeResultStatus::Ok, Some(301), 1_490)],
                1_500,
            ),
            TunnelProbeHealth::Degraded
        );
    }

    #[test]
    fn jitter_is_stable_and_bounded() {
        let value = jittered_interval("edge", 300);
        assert_eq!(value, jittered_interval("edge", 300));
        assert!((270..=330).contains(&value));
    }
}
