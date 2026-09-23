//! Telemetry: host load and per-hop link quality.
//!
//! Reads and writes two time series plus two latest-only tables. The shape of what arrives, and
//! why it arrives already differenced, is argued at length on `LoadReportRequest` in protocol.rs;
//! what matters here is the consequence: **the control plane stores what it is told**. It cannot
//! re-derive a rate from counters the way usage does, so the only defences available are the ones
//! below — a clock check, a window-overlap check, and an ownership check on each hop.
//!
//! That is a weaker guarantee than usage gets, and deliberately so. Usage is money; this is
//! diagnostics. A node that lies about its CPU wastes an operator's afternoon, while a node that
//! lies about its counters takes revenue.

use std::collections::{BTreeMap, BTreeSet};

use brocade_deployment::protocol::{
    CpuDetailSample, DiskDetailSample, HopLinkList, HopLinkSample, HopLinkView, HostFacts,
    LoadReportRequest, LoadReportResult, LoadSample, MemoryDetailSample, NetworkDetailSample,
    NodeLoadList, NodeLoadView, ProcessSample,
};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{admin::tenant_filter, AdminContext, Result, StoreError};

/// The machine overview draws only its short NIC sparkline. Keeping that response separate from
/// [`NodeLoadView`] avoids serialising every CPU, memory, disk and socket detail for every card.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NodeNicSample {
    pub window_start_unix_secs: i64,
    pub window_end_unix_secs: i64,
    pub has_gap: bool,
    pub nic_rx_bps: u64,
    pub nic_tx_bps: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NodeNicView {
    pub node_id: String,
    pub series: Vec<NodeNicSample>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NodeNicList {
    pub nodes: Vec<NodeNicView>,
}

/// Exact, column-oriented samples for a bounded set of detail-chart metrics.
///
/// The ordinary overview deliberately omits the four deep JSON objects. A detail expansion asks
/// for the fields used by at most two charts, and this shape writes the shared time axis once
/// instead of repeating timestamps and JSON field names for every line.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct NodeLoadMetricView {
    pub node_id: String,
    pub range_start_unix_secs: i64,
    pub range_end_unix_secs: i64,
    pub window_end_unix_secs: Vec<i64>,
    pub has_gap: Vec<bool>,
    pub metrics: BTreeMap<String, Vec<Option<f64>>>,
}

/// Transport form of the initial machine observation. Every ECharts sample is retained; only the
/// JSON layout changes from repeated row objects to parallel columns. Deep objects stay solely in
/// `latest_sample` as capability/current-state facts and are fetched historically in chart pairs.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct NodeLoadOverviewView {
    pub node_id: String,
    pub range_start_unix_secs: i64,
    pub range_end_unix_secs: i64,
    pub reported_at_unix_secs: Option<i64>,
    pub clock_skew_secs: Option<i64>,
    pub host: Option<HostFacts>,
    pub latest_sample: Option<LoadSample>,
    pub series: NodeLoadOverviewSeries,
    pub processes: Vec<ProcessSample>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct NodeLoadOverviewSeries {
    pub window_start_unix_secs: Vec<i64>,
    pub window_end_unix_secs: Vec<i64>,
    pub has_gap: Vec<bool>,
    pub cpu_user_pct: Vec<f32>,
    pub cpu_sys_pct: Vec<f32>,
    pub cpu_softirq_pct: Vec<f32>,
    pub cpu_peak_pct: Vec<f32>,
    pub cpu_steal_pct: Vec<f32>,
    pub load1: Vec<f32>,
    pub mem_available_bytes: Vec<u64>,
    pub swap_used_bytes: Vec<u64>,
    pub oom_kills: Vec<u64>,
    pub disk_free_bytes: Vec<u64>,
    pub disk_inode_free_pct: Vec<f32>,
    pub nic_rx_bps: Vec<u64>,
    pub nic_tx_bps: Vec<u64>,
    pub nic_rx_drop: Vec<u64>,
    pub nic_tx_drop: Vec<u64>,
    pub nic_err: Vec<u64>,
    pub conntrack_count: Vec<Option<u64>>,
    pub uptime_secs: Vec<u64>,
}

pub fn columnar_overview(view: NodeLoadView) -> NodeLoadOverviewView {
    let mut series = NodeLoadOverviewSeries::default();
    for sample in view.series {
        series
            .window_start_unix_secs
            .push(sample.window_start_unix_secs);
        series
            .window_end_unix_secs
            .push(sample.window_end_unix_secs);
        series.has_gap.push(sample.has_gap);
        series.cpu_user_pct.push(sample.cpu_user_pct);
        series.cpu_sys_pct.push(sample.cpu_sys_pct);
        series.cpu_softirq_pct.push(sample.cpu_softirq_pct);
        series.cpu_peak_pct.push(sample.cpu_peak_pct);
        series.cpu_steal_pct.push(sample.cpu_steal_pct);
        series.load1.push(sample.load1);
        series.mem_available_bytes.push(sample.mem_available_bytes);
        series.swap_used_bytes.push(sample.swap_used_bytes);
        series.oom_kills.push(sample.oom_kills);
        series.disk_free_bytes.push(sample.disk_free_bytes);
        series.disk_inode_free_pct.push(sample.disk_inode_free_pct);
        series.nic_rx_bps.push(sample.nic_rx_bps);
        series.nic_tx_bps.push(sample.nic_tx_bps);
        series.nic_rx_drop.push(sample.nic_rx_drop);
        series.nic_tx_drop.push(sample.nic_tx_drop);
        series.nic_err.push(sample.nic_err);
        series.conntrack_count.push(sample.conntrack_count);
        series.uptime_secs.push(sample.uptime_secs);
    }
    NodeLoadOverviewView {
        node_id: view.node_id,
        range_start_unix_secs: view.range_start_unix_secs,
        range_end_unix_secs: view.range_end_unix_secs,
        reported_at_unix_secs: view.reported_at_unix_secs,
        clock_skew_secs: view.clock_skew_secs,
        host: view.host,
        latest_sample: view.latest_sample,
        series,
        processes: view.processes,
    }
}

// Scoping matters here even though telemetry is an operator's view: a tenant-admin is an operator
// of *their* branch, and without `tenant_filter` they would read the CPU, memory and link quality
// of every other tenant's machines. Node-side scoping alone is enough — a load reading belongs to
// the machine that produced it, the way link_health does and unlike path MTU, which two ends
// decide together.

/// Same threshold, and the same reasoning, as usage's: far above any normal jitter (a 30-second
/// round plus network) and small enough not to cross an accounting boundary.
///
/// The stakes are lower here — no month boundary divides a CPU reading — but a machine whose clock
/// has run away would write windows into next week, where they sit above every real reading in
/// every "latest N" query, permanently. That is worse than dropping the round.
const MAX_CLOCK_SKEW_SECS: i64 = 600;

/// How many windows one report may carry. A round produces one; more means the agent is catching
/// up after failing to send. Beyond this it is not catching up, it is confused.
const MAX_SAMPLES_PER_REPORT: usize = 60;

/// Guard against a single report claiming the whole fleet's hops.
const MAX_HOPS_PER_REPORT: usize = 512;

/// Which part of a node's load history a reader requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadSeriesQuery {
    /// Every sample overlapping this half-open wall-clock interval.
    Absolute {
        start_unix_secs: i64,
        end_unix_secs: i64,
    },
    /// The newest `windows` samples, regardless of their age.
    LatestWindows { windows: u32 },
}

impl LoadSeriesQuery {
    fn response_range(self, series: &[LoadSample]) -> (i64, i64) {
        match self {
            Self::Absolute {
                start_unix_secs,
                end_unix_secs,
            } => (start_unix_secs, end_unix_secs),
            Self::LatestWindows { .. } => series
                .first()
                .zip(series.last())
                .map(|(first, last)| (first.window_start_unix_secs, last.window_end_unix_secs))
                .unwrap_or((0, 0)),
        }
    }
}

pub async fn record_load_report(
    pool: &PgPool,
    node_id: &str,
    request: LoadReportRequest,
) -> Result<LoadReportResult> {
    if request.read_at_unix_secs <= 0 {
        return Err(StoreError::InvalidData(
            "load report read_at must be positive unix seconds".to_owned(),
        ));
    }
    if request.samples.len() > MAX_SAMPLES_PER_REPORT {
        return Err(StoreError::InvalidData(format!(
            "load report carries {} windows, over the {MAX_SAMPLES_PER_REPORT} limit",
            request.samples.len()
        )));
    }
    if request.hops.len() > MAX_HOPS_PER_REPORT {
        return Err(StoreError::InvalidData(format!(
            "load report carries {} hops, over the {MAX_HOPS_PER_REPORT} limit",
            request.hops.len()
        )));
    }

    // Signed rather than abs(): the sign says which side runs ahead, and the value is stored
    // (see `upsert_host_facts`) so the UI can show the drift without re-measuring.
    let (skew_secs,): (i64,) =
        sqlx::query_as("SELECT $1::bigint - extract(epoch FROM now())::bigint")
            .bind(request.read_at_unix_secs)
            .fetch_one(pool)
            .await?;
    if skew_secs.abs() > MAX_CLOCK_SKEW_SECS {
        return Err(StoreError::InvalidData(format!(
            "load report clock skew {}s exceeds {MAX_CLOCK_SKEW_SECS}s; check the node's clock",
            skew_secs.abs()
        )));
    }

    let mut tx = pool.begin().await?;
    let mut accepted_samples = 0_u64;
    let mut skipped_samples = 0_u64;
    let mut accepted_hops = 0_u64;
    let mut rejected_hops = 0_u64;

    for sample in &request.samples {
        if sample.window_end_unix_secs <= sample.window_start_unix_secs {
            skipped_samples += 1;
            continue;
        }
        if insert_load_sample(&mut tx, node_id, request.btime_unix_secs, sample).await? {
            accepted_samples += 1;
        } else {
            // ON CONFLICT DO NOTHING rather than an upsert: a window already stored is a window
            // already stored, and a retry re-sending it must not overwrite. Not an error either —
            // a resend is exactly what a flaky link produces.
            skipped_samples += 1;
        }
    }

    for hop in &request.hops {
        if hop.window_end_unix_secs <= hop.window_start_unix_secs {
            rejected_hops += 1;
            continue;
        }
        if !node_carries_chain(&mut tx, node_id, &hop.chain_id).await? {
            // The machine is reporting a leg on a chain it does not sit on. Not noise — an
            // anomaly, counted apart so that "this agent is confused" is visible as a number
            // rather than inferred from missing rows.
            rejected_hops += 1;
            continue;
        }
        if insert_hop_sample(&mut tx, node_id, &hop.chain_id, hop).await? {
            accepted_hops += 1;
        } else {
            rejected_hops += 1;
        }
    }

    let latest = upsert_host_facts(
        &mut tx,
        node_id,
        &request.host,
        request.read_at_unix_secs,
        skew_secs,
    )
    .await?;
    if latest {
        replace_process_state(
            &mut tx,
            node_id,
            &request.processes,
            request.read_at_unix_secs,
        )
        .await?;
    }

    tx.commit().await?;

    Ok(LoadReportResult {
        node_id: node_id.to_owned(),
        accepted_samples,
        accepted_hops,
        skipped_samples,
        rejected_hops,
    })
}

/// Whether this machine has any step on the named chain.
///
/// A looser test than usage's `counter_by_label`, on purpose. Usage has to pin a counter to one
/// grant because a bill hangs off it; here the question is only "could this machine plausibly have
/// an outbound on this chain", and being strict about which leg would reject the legitimate
/// variants (reverse dial in particular) for no gain.
async fn node_carries_chain(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    chain_id: &str,
) -> Result<bool> {
    // The agent's tag is `out:{app}/{chain}>{to}`, so chain_id arrives as `{app}/{chain}` while
    // the model stores the bare chain id — the same last-segment match link_health does.
    let bare = chain_id.rsplit('/').next().unwrap_or(chain_id);
    let (found,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM steps WHERE node_id = $1 AND chain_id = $2)")
            .bind(node_id)
            .bind(bare)
            .fetch_one(&mut **tx)
            .await?;
    Ok(found)
}

async fn insert_load_sample(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    btime: i64,
    s: &LoadSample,
) -> Result<bool> {
    let cpu = cpu_split(s)?;
    // Deep diagnostics are deliberately fail-soft. They are optional enrichment, so a malformed
    // per-core reading from a hotplug/iowait counter regression must not discard the stable scalar
    // sample, host facts, processes and hop observations travelling in the same transaction.
    let cpu_detail = cpu_detail_json(s.cpu_detail.as_ref())?;
    let memory_detail = memory_detail_json(s.memory_detail.as_ref())?;
    let disk_detail = disk_detail_json(s.disk_detail.as_ref())?;
    let network_detail = network_detail_json(s.network_detail.as_ref())?;
    let result = sqlx::query(
        "INSERT INTO node_load_samples (
             node_id, window_start, window_end, btime, has_gap,
             cpu_user_pct, cpu_sys_pct, cpu_softirq_pct, cpu_peak_pct, cpu_steal_pct, load1, cpu_detail,
             mem_available_bytes, swap_used_bytes, memory_detail, oom_kills,
             disk_free_bytes, disk_inode_free_pct, disk_detail,
             nic_rx_bps, nic_tx_bps, nic_rx_drop, nic_tx_drop, nic_err,
             conntrack_count, network_detail, uptime_secs
         ) VALUES (
             $1, to_timestamp($2), to_timestamp($3), $4, $5,
             $6, $7, $8, $9, $10, $11, $12,
             $13, $14, $15, $16,
             $17, $18, $19,
             $20, $21, $22, $23, $24,
             $25, $26, $27
         )
         ON CONFLICT (node_id, window_start) DO NOTHING",
    )
    .bind(node_id)
    .bind(s.window_start_unix_secs)
    .bind(s.window_end_unix_secs)
    .bind(btime)
    .bind(s.has_gap)
    .bind(cpu.0)
    .bind(cpu.1)
    .bind(cpu.2)
    .bind(pct("cpu_peak_pct", s.cpu_peak_pct)?)
    // Clamped on its own, never summed with the three shares: steal is not work this machine
    // does, and the table's sum CHECK covers only work.
    .bind(pct("cpu_steal_pct", s.cpu_steal_pct)?)
    .bind(finite("load1", s.load1)?)
    .bind(cpu_detail)
    .bind(u64_to_i64("mem_available_bytes", s.mem_available_bytes)?)
    .bind(u64_to_i64("swap_used_bytes", s.swap_used_bytes)?)
    .bind(memory_detail)
    .bind(u64_to_i64("oom_kills", s.oom_kills)?)
    .bind(u64_to_i64("disk_free_bytes", s.disk_free_bytes)?)
    .bind(pct("disk_inode_free_pct", s.disk_inode_free_pct)?)
    .bind(disk_detail)
    .bind(u64_to_i64("nic_rx_bps", s.nic_rx_bps)?)
    .bind(u64_to_i64("nic_tx_bps", s.nic_tx_bps)?)
    .bind(u64_to_i64("nic_rx_drop", s.nic_rx_drop)?)
    .bind(u64_to_i64("nic_tx_drop", s.nic_tx_drop)?)
    .bind(u64_to_i64("nic_err", s.nic_err)?)
    .bind(
        s.conntrack_count
            .map(|v| u64_to_i64("conntrack_count", v))
            .transpose()?,
    )
    .bind(network_detail)
    .bind(u64_to_i64("uptime_secs", s.uptime_secs)?)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

async fn insert_hop_sample(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    chain_id: &str,
    h: &HopLinkSample,
) -> Result<bool> {
    let result = sqlx::query(
        "INSERT INTO node_hop_link_samples (
             node_id, chain_id, peer_node_id, window_start, window_end,
             conns, conns_measured, btlbw_p50_bps, btlbw_p90_bps,
             min_rtt_us, rtt_p50_us, rtt_p90_us, retrans_pct,
             busy_pct, rwnd_limited_pct, sndbuf_limited_pct
         ) VALUES (
             $1, $2, $3, to_timestamp($4), to_timestamp($5),
             $6, $7, $8, $9,
             $10, $11, $12, $13,
             $14, $15, $16
         )
         ON CONFLICT (node_id, chain_id, peer_node_id, window_start) DO NOTHING",
    )
    .bind(node_id)
    .bind(chain_id)
    .bind(&h.peer_node_id)
    .bind(h.window_start_unix_secs)
    .bind(h.window_end_unix_secs)
    .bind(i32::try_from(h.conns).unwrap_or(i32::MAX))
    .bind(i32::try_from(h.conns_measured).unwrap_or(i32::MAX))
    .bind(
        h.btlbw_p50_bps
            .map(|v| u64_to_i64("btlbw_p50_bps", v))
            .transpose()?,
    )
    .bind(
        h.btlbw_p90_bps
            .map(|v| u64_to_i64("btlbw_p90_bps", v))
            .transpose()?,
    )
    .bind(i32::try_from(h.min_rtt_us).unwrap_or(i32::MAX))
    .bind(i32::try_from(h.rtt_p50_us).unwrap_or(i32::MAX))
    .bind(i32::try_from(h.rtt_p90_us).unwrap_or(i32::MAX))
    .bind(pct("retrans_pct", h.retrans_pct)?)
    .bind(pct("busy_pct", h.busy_pct)?)
    .bind(pct("rwnd_limited_pct", h.rwnd_limited_pct)?)
    .bind(pct("sndbuf_limited_pct", h.sndbuf_limited_pct)?)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

async fn upsert_host_facts(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    host: &HostFacts,
    read_at: i64,
    clock_skew_secs: i64,
) -> Result<bool> {
    let json = serde_json::to_value(host).map_err(|error| {
        StoreError::InvalidData(format!("host facts not serializable: {error}"))
    })?;
    // Token enrollment normally creates the row first. Keeping this write atomic as an UPSERT
    // also makes ingestion robust if initialization and the first report race.
    let result = sqlx::query(
        "INSERT INTO node_agent_state (node_id, load_host_facts, load_reported_at, load_clock_skew_secs)
         VALUES ($1, $2, to_timestamp($3), $4)
         ON CONFLICT (node_id) DO UPDATE
            SET load_host_facts = EXCLUDED.load_host_facts,
                load_reported_at = EXCLUDED.load_reported_at,
                load_clock_skew_secs = EXCLUDED.load_clock_skew_secs
          WHERE node_agent_state.load_reported_at IS NULL
             OR node_agent_state.load_reported_at <= EXCLUDED.load_reported_at",
    )
    .bind(node_id)
    .bind(json)
    .bind(read_at)
    .bind(clock_skew_secs)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Replace this machine's process rows wholesale.
///
/// Delete-then-insert rather than per-row upsert, because disappearance is information: xray
/// removed from a machine that becomes a pure relay must stop being listed, and an upsert leaves
/// the stale row there forever, showing a process that no longer exists.
async fn replace_process_state(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    processes: &[ProcessSample],
    read_at: i64,
) -> Result<()> {
    sqlx::query("DELETE FROM node_process_state WHERE node_id = $1")
        .bind(node_id)
        .execute(&mut **tx)
        .await?;
    for p in processes {
        // The protocol version pins this vocabulary, so an unknown process is malformed input
        // rather than an extension that can be silently discarded.
        if !matches!(p.proc.as_str(), "xray" | "wg" | "phantun" | "agent") {
            return Err(StoreError::InvalidData(format!(
                "unknown managed process {:?}",
                p.proc
            )));
        }
        sqlx::query(
            "INSERT INTO node_process_state
                 (node_id, proc, rss_bytes, cpu_pct, started_at, fds, fd_limit, observed_at)
             VALUES ($1, $2, $3, $4, CASE WHEN $5::bigint IS NULL THEN NULL ELSE to_timestamp($5) END, $6, $7, to_timestamp($8))",
        )
        .bind(node_id)
        .bind(&p.proc)
        .bind(p.rss_bytes.map(|v| u64_to_i64("rss_bytes", v)).transpose()?)
        .bind(p.cpu_pct.map(|v| finite("cpu_pct", v)).transpose()?)
        .bind(p.started_at_unix_secs)
        .bind(p.fds.map(|v| u64_to_i64("fds", v)).transpose()?)
        .bind(p.fd_limit.map(|v| u64_to_i64("fd_limit", v)).transpose()?)
        .bind(read_at)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// One machine's selected load windows, oldest first, plus its newest stored sample.
pub async fn node_load_view(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    selection: LoadSeriesQuery,
    max_samples: u32,
) -> Result<NodeLoadView> {
    node_load_view_inner(
        pool,
        actor,
        node_id,
        selection,
        max_samples,
        LoadDetailColumns::ALL,
    )
    .await
}

/// Overview data keeps every base sample needed by the visible KPI and NIC ECharts, but strips
/// deep CPU/memory/disk/network objects from the history. `latest_sample` remains complete so the
/// current reading and capability flags do not require a second request.
pub async fn node_load_overview(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    selection: LoadSeriesQuery,
    max_samples: u32,
) -> Result<NodeLoadView> {
    node_load_view_inner(
        pool,
        actor,
        node_id,
        selection,
        max_samples,
        LoadDetailColumns::NONE,
    )
    .await
}

/// Return exact retained points for the metric selectors requested by a pair of charts.
///
/// The selector allow-list is intentionally closed. Besides preventing SQL-shaped input from
/// becoming part of a future dynamic query, it keeps the frontend/backend contract auditable.
/// Values are column-oriented; `has_gap` is shared by every column and remains authoritative.
pub async fn node_load_metrics(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    selection: LoadSeriesQuery,
    max_samples: u32,
    metric_ids: &[String],
) -> Result<NodeLoadMetricView> {
    const MAX_METRICS_PER_BATCH: usize = 16;
    if metric_ids.is_empty() || metric_ids.len() > MAX_METRICS_PER_BATCH {
        return Err(StoreError::InvalidData(format!(
            "load metric batch must contain 1–{MAX_METRICS_PER_BATCH} selectors"
        )));
    }

    let metric_ids = metric_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for metric_id in &metric_ids {
        if *metric_id != "cpu.cores.busy_pct" && load_metric_series(&[], metric_id).is_none() {
            return Err(StoreError::InvalidData(format!(
                "unknown load metric selector: {metric_id}"
            )));
        }
    }
    let detail_columns = LoadDetailColumns::for_metrics(&metric_ids);
    let view =
        node_load_view_inner(pool, actor, node_id, selection, max_samples, detail_columns).await?;
    let mut metrics = BTreeMap::new();
    for metric_id in metric_ids {
        if metric_id == "cpu.cores.busy_pct" {
            let core_ids = view
                .series
                .iter()
                .flat_map(|sample| sample.cpu_detail.iter())
                .flat_map(|detail| detail.cores.iter().map(|core| core.cpu))
                .collect::<BTreeSet<_>>();
            for cpu in core_ids {
                metrics.insert(
                    format!("cpu.core.{cpu}.busy_pct"),
                    metric_series(&view.series, |sample| {
                        sample
                            .cpu_detail
                            .as_ref()?
                            .cores
                            .iter()
                            .find(|core| core.cpu == cpu)
                            .map(|core| {
                                f64::from(core.user_pct + core.system_pct + core.softirq_pct)
                            })
                    }),
                );
            }
            continue;
        }
        let values = load_metric_series(&view.series, metric_id)
            .expect("metric selector validated before querying");
        metrics.insert(metric_id.to_owned(), values);
    }
    Ok(NodeLoadMetricView {
        node_id: view.node_id,
        range_start_unix_secs: view.range_start_unix_secs,
        range_end_unix_secs: view.range_end_unix_secs,
        window_end_unix_secs: view
            .series
            .iter()
            .map(|sample| sample.window_end_unix_secs)
            .collect(),
        has_gap: view.series.iter().map(|sample| sample.has_gap).collect(),
        metrics,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LoadDetailColumns {
    cpu: bool,
    memory: bool,
    disk: bool,
    network: bool,
}

impl LoadDetailColumns {
    const ALL: Self = Self {
        cpu: true,
        memory: true,
        disk: true,
        network: true,
    };
    const NONE: Self = Self {
        cpu: false,
        memory: false,
        disk: false,
        network: false,
    };

    fn for_metrics(metric_ids: &BTreeSet<&str>) -> Self {
        Self {
            cpu: metric_ids.iter().any(|metric| metric.starts_with("cpu.")),
            memory: metric_ids
                .iter()
                .any(|metric| metric.starts_with("memory.")),
            disk: metric_ids.iter().any(|metric| metric.starts_with("disk.")),
            network: metric_ids
                .iter()
                .any(|metric| metric.starts_with("network.")),
        }
    }

    fn select_sql(self) -> String {
        fn column(included: bool, name: &str) -> String {
            if included {
                name.to_owned()
            } else {
                format!("NULL::jsonb AS {name}")
            }
        }
        [
            column(self.cpu, "cpu_detail"),
            column(self.memory, "memory_detail"),
            column(self.disk, "disk_detail"),
            column(self.network, "network_detail"),
        ]
        .join(", ")
    }
}

fn metric_series(
    samples: &[LoadSample],
    read: impl Fn(&LoadSample) -> Option<f64>,
) -> Vec<Option<f64>> {
    samples.iter().map(read).collect()
}

#[allow(clippy::too_many_lines)]
fn load_metric_series(samples: &[LoadSample], metric_id: &str) -> Option<Vec<Option<f64>>> {
    let values = match metric_id {
        "cpu.user_pct" => metric_series(samples, |sample| Some(f64::from(sample.cpu_user_pct))),
        "cpu.sys_pct" => metric_series(samples, |sample| Some(f64::from(sample.cpu_sys_pct))),
        "cpu.softirq_pct" => {
            metric_series(samples, |sample| Some(f64::from(sample.cpu_softirq_pct)))
        }
        "cpu.peak_pct" => metric_series(samples, |sample| Some(f64::from(sample.cpu_peak_pct))),
        "cpu.steal_pct" => metric_series(samples, |sample| Some(f64::from(sample.cpu_steal_pct))),
        "cpu.iowait_pct" => metric_series(samples, |sample| {
            sample
                .cpu_detail
                .as_ref()
                .map(|detail| f64::from(detail.iowait_pct))
        }),
        "cpu.pressure_some_pct" => metric_series(samples, |sample| {
            sample.cpu_detail.as_ref()?.pressure_some_pct.map(f64::from)
        }),
        "cpu.io_pressure_some_pct" => metric_series(samples, |sample| {
            sample
                .cpu_detail
                .as_ref()?
                .io_pressure_some_pct
                .map(f64::from)
        }),
        "cpu.io_pressure_full_pct" => metric_series(samples, |sample| {
            sample
                .cpu_detail
                .as_ref()?
                .io_pressure_full_pct
                .map(f64::from)
        }),
        "load.1" => metric_series(samples, |sample| Some(f64::from(sample.load1))),
        "cpu.load5" => metric_series(samples, |sample| {
            sample
                .cpu_detail
                .as_ref()
                .map(|detail| f64::from(detail.load5))
        }),
        "cpu.load15" => metric_series(samples, |sample| {
            sample
                .cpu_detail
                .as_ref()
                .map(|detail| f64::from(detail.load15))
        }),
        "cpu.procs_running" => metric_series(samples, |sample| {
            sample
                .cpu_detail
                .as_ref()?
                .procs_running
                .map(|value| value as f64)
        }),
        "cpu.context_switches_per_sec" => metric_series(samples, |sample| {
            sample
                .cpu_detail
                .as_ref()?
                .context_switches_per_sec
                .map(|value| value as f64)
        }),
        "cpu.net_rx_softirqs_per_sec" => metric_series(samples, |sample| {
            sample
                .cpu_detail
                .as_ref()?
                .net_rx_softirqs_per_sec
                .map(|value| value as f64)
        }),
        "cpu.net_tx_softirqs_per_sec" => metric_series(samples, |sample| {
            sample
                .cpu_detail
                .as_ref()?
                .net_tx_softirqs_per_sec
                .map(|value| value as f64)
        }),
        "cpu.throttled_usec" => metric_series(samples, |sample| {
            sample
                .cpu_detail
                .as_ref()?
                .throttled_usec
                .map(|value| value as f64)
        }),
        "memory.anon_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.anon_bytes as f64)
        }),
        "memory.shmem_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.shmem_bytes as f64)
        }),
        "memory.kernel_other_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.kernel_other_bytes as f64)
        }),
        "memory.file_cache_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.file_cache_bytes as f64)
        }),
        "memory.free_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.free_bytes as f64)
        }),
        "memory.available_min_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.available_min_bytes as f64)
        }),
        "memory.pressure_some_pct" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()?
                .pressure_some_pct
                .map(f64::from)
        }),
        "memory.pressure_full_pct" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()?
                .pressure_full_pct
                .map(f64::from)
        }),
        "memory.buffers_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.buffers_bytes as f64)
        }),
        "memory.kernel_reclaimable_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.kernel_reclaimable_bytes as f64)
        }),
        "memory.slab_unreclaimable_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.slab_unreclaimable_bytes as f64)
        }),
        "memory.unevictable_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.unevictable_bytes as f64)
        }),
        "memory.mlocked_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.mlocked_bytes as f64)
        }),
        "memory.gup_pinned_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()?
                .gup_pinned_bytes
                .map(|value| value as f64)
        }),
        "memory.swap_used_bytes" => {
            metric_series(samples, |sample| Some(sample.swap_used_bytes as f64))
        }
        "memory.swap_cached_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.swap_cached_bytes as f64)
        }),
        "memory.dirty_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.dirty_bytes as f64)
        }),
        "memory.writeback_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.writeback_bytes as f64)
        }),
        "memory.swap_in_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.swap_in_bytes as f64)
        }),
        "memory.swap_out_bytes" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.swap_out_bytes as f64)
        }),
        "memory.major_faults" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.major_faults as f64)
        }),
        "memory.direct_reclaim_pages" => metric_series(samples, |sample| {
            sample
                .memory_detail
                .as_ref()
                .map(|detail| detail.direct_reclaim_pages as f64)
        }),
        "disk.free_bytes" => metric_series(samples, |sample| Some(sample.disk_free_bytes as f64)),
        "disk.inode_free_pct" => metric_series(samples, |sample| {
            Some(f64::from(sample.disk_inode_free_pct))
        }),
        "disk.total_bytes" => metric_series(samples, |sample| {
            sample
                .disk_detail
                .as_ref()?
                .total_bytes
                .map(|value| value as f64)
        }),
        "disk.busy_pct" => metric_series(samples, |sample| {
            sample.disk_detail.as_ref()?.busy_pct.map(f64::from)
        }),
        "disk.pressure_some_pct" => metric_series(samples, |sample| {
            sample
                .disk_detail
                .as_ref()?
                .pressure_some_pct
                .map(f64::from)
        }),
        "disk.pressure_full_pct" => metric_series(samples, |sample| {
            sample
                .disk_detail
                .as_ref()?
                .pressure_full_pct
                .map(f64::from)
        }),
        "disk.read_bps" => metric_series(samples, |sample| {
            sample
                .disk_detail
                .as_ref()?
                .read_bps
                .map(|value| value as f64)
        }),
        "disk.write_bps" => metric_series(samples, |sample| {
            sample
                .disk_detail
                .as_ref()?
                .write_bps
                .map(|value| value as f64)
        }),
        "disk.read_iops" => metric_series(samples, |sample| {
            sample.disk_detail.as_ref()?.read_iops.map(f64::from)
        }),
        "disk.write_iops" => metric_series(samples, |sample| {
            sample.disk_detail.as_ref()?.write_iops.map(f64::from)
        }),
        "disk.read_await_ms" => metric_series(samples, |sample| {
            sample.disk_detail.as_ref()?.read_await_ms.map(f64::from)
        }),
        "disk.write_await_ms" => metric_series(samples, |sample| {
            sample.disk_detail.as_ref()?.write_await_ms.map(f64::from)
        }),
        "disk.queue_depth" => metric_series(samples, |sample| {
            sample.disk_detail.as_ref()?.queue_depth.map(f64::from)
        }),
        "disk.in_flight" => metric_series(samples, |sample| {
            sample
                .disk_detail
                .as_ref()?
                .in_flight
                .map(|value| value as f64)
        }),
        "network.conntrack_count" => metric_series(samples, |sample| {
            sample.conntrack_count.map(|value| value as f64)
        }),
        "network.tcp_curr_estab" => metric_series(samples, |sample| {
            sample
                .network_detail
                .as_ref()?
                .tcp_curr_estab
                .map(|value| value as f64)
        }),
        "network.tcp_inuse" => metric_series(samples, |sample| {
            sample
                .network_detail
                .as_ref()?
                .tcp_inuse
                .map(|value| value as f64)
        }),
        "network.tcp_time_wait" => metric_series(samples, |sample| {
            sample
                .network_detail
                .as_ref()?
                .tcp_time_wait
                .map(|value| value as f64)
        }),
        "network.tcp_orphan" => metric_series(samples, |sample| {
            sample
                .network_detail
                .as_ref()?
                .tcp_orphan
                .map(|value| value as f64)
        }),
        "network.udp_inuse" => metric_series(samples, |sample| {
            sample
                .network_detail
                .as_ref()?
                .udp_inuse
                .map(|value| value as f64)
        }),
        "network.ephemeral_port_capacity" => metric_series(samples, |sample| {
            sample
                .network_detail
                .as_ref()?
                .ephemeral_port_capacity
                .map(|value| value as f64)
        }),
        "network.tcp_ephemeral_inuse_v4" => metric_series(samples, |sample| {
            sample
                .network_detail
                .as_ref()?
                .tcp_ephemeral_inuse_v4
                .map(|value| value as f64)
        }),
        "network.tcp_ephemeral_inuse_v6" => metric_series(samples, |sample| {
            sample
                .network_detail
                .as_ref()?
                .tcp_ephemeral_inuse_v6
                .map(|value| value as f64)
        }),
        "network.tcp_ephemeral_time_wait_v4" => metric_series(samples, |sample| {
            sample
                .network_detail
                .as_ref()?
                .tcp_ephemeral_time_wait_v4
                .map(|value| value as f64)
        }),
        "network.tcp_ephemeral_time_wait_v6" => metric_series(samples, |sample| {
            sample
                .network_detail
                .as_ref()?
                .tcp_ephemeral_time_wait_v6
                .map(|value| value as f64)
        }),
        "network.tcp_ephemeral_top_target_v4" => metric_series(samples, |sample| {
            sample
                .network_detail
                .as_ref()?
                .tcp_ephemeral_top_target_v4
                .map(|value| value as f64)
        }),
        "network.tcp_ephemeral_top_target_v6" => metric_series(samples, |sample| {
            sample
                .network_detail
                .as_ref()?
                .tcp_ephemeral_top_target_v6
                .map(|value| value as f64)
        }),
        "network.tcp_active_opens" => network_metric(samples, |detail| detail.tcp_active_opens),
        "network.tcp_passive_opens" => network_metric(samples, |detail| detail.tcp_passive_opens),
        "network.tcp_attempt_fails" => network_metric(samples, |detail| detail.tcp_attempt_fails),
        "network.tcp_estab_resets" => network_metric(samples, |detail| detail.tcp_estab_resets),
        "network.tcp_retrans_segs" => network_metric(samples, |detail| detail.tcp_retrans_segs),
        "network.tcp_syn_retrans" => network_metric(samples, |detail| detail.tcp_syn_retrans),
        "network.tcp_timeouts" => network_metric(samples, |detail| detail.tcp_timeouts),
        "network.tcp_in_errors" => network_metric(samples, |detail| detail.tcp_in_errors),
        "network.tcp_out_resets" => network_metric(samples, |detail| detail.tcp_out_resets),
        "network.tcp_listen_overflows" => {
            network_metric(samples, |detail| detail.tcp_listen_overflows)
        }
        "network.tcp_listen_drops" => network_metric(samples, |detail| detail.tcp_listen_drops),
        "network.udp_in_errors" => network_metric(samples, |detail| detail.udp_in_errors),
        "network.udp_no_ports" => network_metric(samples, |detail| detail.udp_no_ports),
        "network.udp_rcvbuf_errors" => network_metric(samples, |detail| detail.udp_rcvbuf_errors),
        "network.udp_sndbuf_errors" => network_metric(samples, |detail| detail.udp_sndbuf_errors),
        "network.tcp_mem_bytes" => network_metric(samples, |detail| detail.tcp_mem_bytes),
        "network.udp_mem_bytes" => network_metric(samples, |detail| detail.udp_mem_bytes),
        _ => return None,
    };
    Some(values)
}

fn network_metric(
    samples: &[LoadSample],
    read: impl Fn(&NetworkDetailSample) -> Option<u64>,
) -> Vec<Option<f64>> {
    metric_series(samples, |sample| {
        read(sample.network_detail.as_ref()?).map(|value| value as f64)
    })
}

async fn node_load_view_inner(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    selection: LoadSeriesQuery,
    max_samples: u32,
    detail_columns: LoadDetailColumns,
) -> Result<NodeLoadView> {
    // Scope check first, and as an early return rather than a filter woven through the three
    // queries below: out of scope means this machine does not exist as far as this operator is
    // concerned, and an empty view says exactly that without leaking whether the id is real.
    if !node_in_scope(pool, actor, node_id).await? {
        let (range_start_unix_secs, range_end_unix_secs) = selection.response_range(&[]);
        return Ok(NodeLoadView {
            node_id: node_id.to_owned(),
            range_start_unix_secs,
            range_end_unix_secs,
            reported_at_unix_secs: None,
            clock_skew_secs: None,
            host: None,
            latest_sample: None,
            series: Vec::new(),
            processes: Vec::new(),
        });
    }
    let facts = sqlx::query(
        "SELECT load_host_facts, extract(epoch FROM load_reported_at)::bigint AS reported_at,
                load_clock_skew_secs
         FROM node_agent_state WHERE node_id = $1",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await?;

    let (host, reported_at, clock_skew) = match facts {
        Some(row) => {
            let raw: Option<serde_json::Value> = row.try_get("load_host_facts")?;
            let host = stored_host_facts(node_id, raw)?;
            (
                host,
                row.try_get::<Option<i64>, _>("reported_at")?,
                row.try_get::<Option<i64>, _>("load_clock_skew_secs")?,
            )
        }
        None => (None, None, None),
    };

    // Absolute selection uses window overlap and deliberately includes partial boundary windows.
    // Latest-window selection orders descending before LIMIT, then both are reversed below so the
    // wire representation is always oldest first. The row cap is an independent cardinality guard.
    // Epochs extracted in SQL rather than read as timestamps and converted here: the alternative
    // is a chrono dependency on this crate for one field, and the neighbouring queries
    // (`load_reported_at`, `started_at`) already do it this way.
    let detail_columns = detail_columns.select_sql();
    let samples_select = format!(
        "SELECT extract(epoch FROM window_start)::bigint AS window_start_secs,
                extract(epoch FROM window_end)::bigint AS window_end_secs,
                has_gap, cpu_user_pct, cpu_sys_pct, cpu_softirq_pct, cpu_peak_pct, cpu_steal_pct, load1,
                mem_available_bytes, swap_used_bytes, oom_kills,
                disk_free_bytes, disk_inode_free_pct,
                nic_rx_bps, nic_tx_bps, nic_rx_drop, nic_tx_drop, nic_err,
                conntrack_count, uptime_secs, {detail_columns}
         FROM node_load_samples
         WHERE node_id = "
    );
    let mut samples_query = sqlx::QueryBuilder::<Postgres>::new(&samples_select);
    samples_query.push_bind(node_id);
    match selection {
        LoadSeriesQuery::Absolute {
            start_unix_secs,
            end_unix_secs,
        } => {
            samples_query
                .push(" AND window_end > to_timestamp(")
                .push_bind(start_unix_secs)
                .push(") AND window_start < to_timestamp(")
                .push_bind(end_unix_secs)
                .push(')');
        }
        LoadSeriesQuery::LatestWindows { .. } => {}
    }
    let limit = match selection {
        LoadSeriesQuery::Absolute { .. } => max_samples,
        LoadSeriesQuery::LatestWindows { windows } => windows.min(max_samples),
    };
    samples_query
        .push(" ORDER BY window_start DESC LIMIT ")
        .push_bind(i64::from(limit));
    let rows = samples_query.build().fetch_all(pool).await?;
    let mut series = rows
        .iter()
        .map(load_sample_from_row)
        .collect::<Result<Vec<_>>>()?;
    series.reverse();

    let latest_sample = match selection {
        LoadSeriesQuery::LatestWindows { .. } => series.last().cloned(),
        LoadSeriesQuery::Absolute { .. } => {
            let row = sqlx::query(
                "SELECT extract(epoch FROM window_start)::bigint AS window_start_secs,
                        extract(epoch FROM window_end)::bigint AS window_end_secs,
                        has_gap, cpu_user_pct, cpu_sys_pct, cpu_softirq_pct, cpu_peak_pct, cpu_steal_pct, load1, cpu_detail,
                        mem_available_bytes, swap_used_bytes, memory_detail, oom_kills,
                        disk_free_bytes, disk_inode_free_pct, disk_detail,
                        nic_rx_bps, nic_tx_bps, nic_rx_drop, nic_tx_drop, nic_err,
                        conntrack_count, network_detail, uptime_secs
                 FROM node_load_samples
                 WHERE node_id = $1
                 ORDER BY window_start DESC
                 LIMIT 1",
            )
            .bind(node_id)
            .fetch_optional(pool)
            .await?;
            row.as_ref().map(load_sample_from_row).transpose()?
        }
    };

    let (range_start_unix_secs, range_end_unix_secs) = selection.response_range(&series);

    let processes = sqlx::query(
        "SELECT proc, rss_bytes, cpu_pct,
                extract(epoch FROM started_at)::bigint AS started_at, fds, fd_limit
         FROM node_process_state WHERE node_id = $1
         ORDER BY proc",
    )
    .bind(node_id)
    .fetch_all(pool)
    .await?
    .iter()
    .map(process_from_row)
    .collect::<Result<Vec<_>>>()?;

    Ok(NodeLoadView {
        node_id: node_id.to_owned(),
        range_start_unix_secs,
        range_end_unix_secs,
        reported_at_unix_secs: reported_at,
        clock_skew_secs: clock_skew,
        host,
        latest_sample,
        series,
        processes,
    })
}

/// Every live machine's selected load windows, for list and fleet views.
pub async fn list_node_load(
    pool: &PgPool,
    actor: &AdminContext,
    selection: LoadSeriesQuery,
    max_samples_per_node: u32,
) -> Result<NodeLoadList> {
    let filter = tenant_filter(actor);
    let (scope, pattern) = split_filter(&filter);
    let fact_rows = sqlx::query(
        "SELECT n.id AS node_id, s.load_host_facts,
                extract(epoch FROM s.load_reported_at)::bigint AS reported_at,
                s.load_clock_skew_secs
           FROM nodes n
           LEFT JOIN node_agent_state s ON s.node_id = n.id
          WHERE n.retired_at IS NULL
            AND ($1::text IS NULL OR n.tenant_id = $1 OR n.tenant_id LIKE $2 ESCAPE '\\')
          ORDER BY n.id",
    )
    .bind(scope)
    .bind(pattern)
    .fetch_all(pool)
    .await?;
    let mut nodes = BTreeMap::<String, NodeLoadView>::new();
    for row in fact_rows {
        let node_id: String = row.try_get("node_id")?;
        let raw: Option<serde_json::Value> = row.try_get("load_host_facts")?;
        let (range_start_unix_secs, range_end_unix_secs) = selection.response_range(&[]);
        nodes.insert(
            node_id.clone(),
            NodeLoadView {
                node_id: node_id.clone(),
                range_start_unix_secs,
                range_end_unix_secs,
                reported_at_unix_secs: row.try_get("reported_at")?,
                clock_skew_secs: row.try_get("load_clock_skew_secs")?,
                host: stored_host_facts(&node_id, raw)?,
                latest_sample: None,
                series: Vec::new(),
                processes: Vec::new(),
            },
        );
    }
    if nodes.is_empty() {
        return Ok(NodeLoadList { nodes: Vec::new() });
    }
    let ids = nodes.keys().cloned().collect::<Vec<_>>();
    let limit = match selection {
        LoadSeriesQuery::Absolute { .. } => max_samples_per_node,
        LoadSeriesQuery::LatestWindows { windows } => windows.min(max_samples_per_node),
    };

    // One bounded index probe per machine preserves the per-machine cap without ranking the whole
    // telemetry table. The primary key starts with (node_id, window_start), so PostgreSQL can read
    // each machine backwards and stop after `limit` rows.
    let mut samples_query = sqlx::QueryBuilder::<Postgres>::new(
        "SELECT requested.node_id,
                extract(epoch FROM sample.window_start)::bigint AS window_start_secs,
                extract(epoch FROM sample.window_end)::bigint AS window_end_secs,
                sample.has_gap, sample.cpu_user_pct, sample.cpu_sys_pct,
                sample.cpu_softirq_pct, sample.cpu_peak_pct, sample.cpu_steal_pct,
                sample.load1, sample.cpu_detail,
                sample.mem_available_bytes, sample.swap_used_bytes, sample.memory_detail,
                sample.oom_kills, sample.disk_free_bytes, sample.disk_inode_free_pct,
                sample.disk_detail, sample.nic_rx_bps, sample.nic_tx_bps,
                sample.nic_rx_drop, sample.nic_tx_drop, sample.nic_err,
                sample.conntrack_count, sample.network_detail, sample.uptime_secs
           FROM unnest(",
    );
    samples_query.push_bind(ids.clone()).push(
        "::text[]) AS requested(node_id)
           JOIN LATERAL (
                SELECT window_start, window_end,
                       has_gap, cpu_user_pct, cpu_sys_pct, cpu_softirq_pct, cpu_peak_pct,
                       cpu_steal_pct, load1, cpu_detail,
                       mem_available_bytes, swap_used_bytes, memory_detail, oom_kills,
                       disk_free_bytes, disk_inode_free_pct, disk_detail,
                       nic_rx_bps, nic_tx_bps, nic_rx_drop, nic_tx_drop, nic_err,
                       conntrack_count, network_detail, uptime_secs
                  FROM node_load_samples
                 WHERE node_id = requested.node_id",
    );
    if let LoadSeriesQuery::Absolute {
        start_unix_secs,
        end_unix_secs,
    } = selection
    {
        samples_query
            .push(" AND window_end > to_timestamp(")
            .push_bind(start_unix_secs)
            .push(") AND window_start < to_timestamp(")
            .push_bind(end_unix_secs)
            .push(')');
    }
    samples_query
        .push(" ORDER BY window_start DESC LIMIT ")
        .push_bind(i64::from(limit))
        .push(") sample ON TRUE ORDER BY requested.node_id, sample.window_start");
    for row in samples_query.build().fetch_all(pool).await? {
        let node_id: String = row.try_get("node_id")?;
        if let Some(view) = nodes.get_mut(&node_id) {
            view.series.push(load_sample_from_row(&row)?);
        }
    }

    for view in nodes.values_mut() {
        (view.range_start_unix_secs, view.range_end_unix_secs) =
            selection.response_range(&view.series);
        if matches!(selection, LoadSeriesQuery::LatestWindows { .. }) {
            view.latest_sample = view.series.last().cloned();
        }
    }

    if matches!(selection, LoadSeriesQuery::Absolute { .. }) {
        let latest_rows = sqlx::query(
            "SELECT requested.node_id,
                    extract(epoch FROM sample.window_start)::bigint AS window_start_secs,
                    extract(epoch FROM sample.window_end)::bigint AS window_end_secs,
                    sample.has_gap, sample.cpu_user_pct, sample.cpu_sys_pct,
                    sample.cpu_softirq_pct, sample.cpu_peak_pct, sample.cpu_steal_pct,
                    sample.load1, sample.cpu_detail,
                    sample.mem_available_bytes, sample.swap_used_bytes, sample.memory_detail,
                    sample.oom_kills, sample.disk_free_bytes, sample.disk_inode_free_pct,
                    sample.disk_detail, sample.nic_rx_bps, sample.nic_tx_bps,
                    sample.nic_rx_drop, sample.nic_tx_drop, sample.nic_err,
                    sample.conntrack_count, sample.network_detail, sample.uptime_secs
               FROM unnest($1::text[]) AS requested(node_id)
               JOIN LATERAL (
                    SELECT window_start, window_end,
                           has_gap, cpu_user_pct, cpu_sys_pct, cpu_softirq_pct, cpu_peak_pct,
                           cpu_steal_pct, load1, cpu_detail,
                           mem_available_bytes, swap_used_bytes, memory_detail, oom_kills,
                           disk_free_bytes, disk_inode_free_pct, disk_detail,
                           nic_rx_bps, nic_tx_bps, nic_rx_drop, nic_tx_drop, nic_err,
                           conntrack_count, network_detail, uptime_secs
                      FROM node_load_samples
                     WHERE node_id = requested.node_id
                     ORDER BY window_start DESC
                     LIMIT 1
               ) sample ON TRUE
              ORDER BY requested.node_id",
        )
        .bind(&ids)
        .fetch_all(pool)
        .await?;
        for row in latest_rows {
            let node_id: String = row.try_get("node_id")?;
            if let Some(view) = nodes.get_mut(&node_id) {
                view.latest_sample = Some(load_sample_from_row(&row)?);
            }
        }
    }

    let process_rows = sqlx::query(
        "SELECT node_id, proc, rss_bytes, cpu_pct,
                extract(epoch FROM started_at)::bigint AS started_at, fds, fd_limit
           FROM node_process_state
          WHERE node_id = ANY($1::text[])
          ORDER BY node_id, proc",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    for row in process_rows {
        let node_id: String = row.try_get("node_id")?;
        if let Some(view) = nodes.get_mut(&node_id) {
            view.processes.push(process_from_row(&row)?);
        }
    }
    Ok(NodeLoadList {
        nodes: nodes.into_values().collect(),
    })
}

/// Every live machine's newest NIC windows for the overview cards.
pub async fn list_node_nic(
    pool: &PgPool,
    actor: &AdminContext,
    windows: u32,
    max_samples_per_node: u32,
) -> Result<NodeNicList> {
    let filter = tenant_filter(actor);
    let (scope, pattern) = split_filter(&filter);
    let limit = windows.min(max_samples_per_node);
    let rows = sqlx::query(
        "SELECT n.id AS node_id,
                extract(epoch FROM sample.window_start)::bigint AS window_start_secs,
                extract(epoch FROM sample.window_end)::bigint AS window_end_secs,
                sample.has_gap, sample.nic_rx_bps, sample.nic_tx_bps
           FROM nodes n
           LEFT JOIN LATERAL (
                SELECT window_start, window_end, has_gap, nic_rx_bps, nic_tx_bps
                  FROM node_load_samples
                 WHERE node_id = n.id
                 ORDER BY window_start DESC
                 LIMIT $3
           ) sample ON TRUE
          WHERE n.retired_at IS NULL
            AND ($1::text IS NULL OR n.tenant_id = $1 OR n.tenant_id LIKE $2 ESCAPE '\\')
          ORDER BY n.id, sample.window_start",
    )
    .bind(scope)
    .bind(pattern)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await?;

    let mut nodes = BTreeMap::<String, NodeNicView>::new();
    for row in rows {
        let node_id: String = row.try_get("node_id")?;
        let view = nodes.entry(node_id.clone()).or_insert_with(|| NodeNicView {
            node_id,
            series: Vec::new(),
        });
        let Some(window_start_unix_secs) = row.try_get::<Option<i64>, _>("window_start_secs")?
        else {
            continue;
        };
        let window_end_unix_secs = row
            .try_get::<Option<i64>, _>("window_end_secs")?
            .ok_or_else(|| StoreError::InvalidData("NIC sample has no window end".to_owned()))?;
        let has_gap = row
            .try_get::<Option<bool>, _>("has_gap")?
            .ok_or_else(|| StoreError::InvalidData("NIC sample has no gap marker".to_owned()))?;
        let nic_rx_bps = row
            .try_get::<Option<i64>, _>("nic_rx_bps")?
            .ok_or_else(|| StoreError::InvalidData("NIC sample has no receive rate".to_owned()))?;
        let nic_tx_bps = row
            .try_get::<Option<i64>, _>("nic_tx_bps")?
            .ok_or_else(|| StoreError::InvalidData("NIC sample has no transmit rate".to_owned()))?;
        view.series.push(NodeNicSample {
            window_start_unix_secs,
            window_end_unix_secs,
            has_gap,
            nic_rx_bps: i64_to_u64("nic_rx_bps", nic_rx_bps)?,
            nic_tx_bps: i64_to_u64("nic_tx_bps", nic_tx_bps)?,
        });
    }
    Ok(NodeNicList {
        nodes: nodes.into_values().collect(),
    })
}

fn stored_host_facts(node_id: &str, raw: Option<serde_json::Value>) -> Result<Option<HostFacts>> {
    raw.map(|value| {
        serde_json::from_value::<HostFacts>(value).map_err(|error| {
            StoreError::InvalidData(format!(
                "node {node_id} has invalid stored host facts: {error}"
            ))
        })
    })
    .transpose()
}

async fn node_in_scope(pool: &PgPool, actor: &AdminContext, node_id: &str) -> Result<bool> {
    let filter = tenant_filter(actor);
    let (scope, pattern) = split_filter(&filter);
    let (found,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM nodes
                        WHERE id = $3
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

/// The latest window of every hop, optionally narrowed to one chain.
pub async fn hop_link_list(
    pool: &PgPool,
    actor: &AdminContext,
    chain_id: Option<&str>,
) -> Result<HopLinkList> {
    let filter = tenant_filter(actor);
    let (scope, pattern) = split_filter(&filter);
    // DISTINCT ON gives the newest row per (node, chain, peer) in one pass. The alternative —
    // fetching a window's worth and grouping in Rust — has to define "a window" first, and hops
    // do not share window boundaries across machines whose clocks differ by a second.
    let rows = sqlx::query(
        "SELECT DISTINCT ON (h.node_id, h.chain_id, h.peer_node_id)
                h.node_id, h.chain_id, h.peer_node_id,
                extract(epoch FROM h.window_start)::bigint AS window_start_secs,
                extract(epoch FROM h.window_end)::bigint AS window_end_secs,
                h.conns, h.conns_measured, h.btlbw_p50_bps, h.btlbw_p90_bps,
                h.min_rtt_us, h.rtt_p50_us, h.rtt_p90_us, h.retrans_pct,
                h.busy_pct, h.rwnd_limited_pct, h.sndbuf_limited_pct,
                s.load_host_facts ->> 'cc_algo' AS cc_algo
         FROM node_hop_link_samples h
         LEFT JOIN node_agent_state s ON s.node_id = h.node_id
         WHERE ($3::text IS NULL OR h.chain_id = $3)
           AND ($1::text IS NULL OR h.node_id IN (SELECT id FROM nodes
                                                  WHERE tenant_id = $1 OR tenant_id LIKE $2 ESCAPE '\\'))
         ORDER BY h.node_id, h.chain_id, h.peer_node_id, h.window_start DESC",
    )
    .bind(scope)
    .bind(pattern)
    .bind(chain_id)
    .fetch_all(pool)
    .await?;

    let hops = rows
        .iter()
        .map(|row| {
            Ok(HopLinkView {
                node_id: row.try_get("node_id")?,
                // Absent facts mean the agent reported hops before any host facts landed, which
                // one interrupted round can produce. Empty string, not "cubic": the UI's rule is
                // that an unmeasured hop names the algorithm that cannot measure, and naming the
                // wrong one sends somebody to change a setting that is already correct.
                cc_algo: row
                    .try_get::<Option<String>, _>("cc_algo")?
                    .unwrap_or_default(),
                sample: hop_sample_from_row(row)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(HopLinkList { hops })
}

/// Drop telemetry past its retention.
///
/// Both series in one call because they are pruned on the same schedule and for the same reason.
/// Unlike `prune_usage_readings`, nothing here is derived from these rows before they go — they
/// are the finished article, and once old, worthless.
pub async fn prune_load_samples(pool: &PgPool, retain_days: u32) -> Result<u64> {
    let cutoff = format!("{retain_days} days");
    let load = sqlx::query("DELETE FROM node_load_samples WHERE window_end < now() - $1::interval")
        .bind(&cutoff)
        .execute(pool)
        .await?
        .rows_affected();
    let hops =
        sqlx::query("DELETE FROM node_hop_link_samples WHERE window_end < now() - $1::interval")
            .bind(&cutoff)
            .execute(pool)
            .await?
            .rows_affected();
    Ok(load + hops)
}

fn load_sample_from_row(row: &sqlx::postgres::PgRow) -> Result<LoadSample> {
    Ok(LoadSample {
        window_start_unix_secs: row.try_get("window_start_secs")?,
        window_end_unix_secs: row.try_get("window_end_secs")?,
        has_gap: row.try_get("has_gap")?,
        cpu_user_pct: row.try_get("cpu_user_pct")?,
        cpu_sys_pct: row.try_get("cpu_sys_pct")?,
        cpu_softirq_pct: row.try_get("cpu_softirq_pct")?,
        cpu_peak_pct: row.try_get("cpu_peak_pct")?,
        cpu_steal_pct: row.try_get("cpu_steal_pct")?,
        load1: row.try_get("load1")?,
        cpu_detail: optional_detail_from_row(row, "cpu_detail")?,
        mem_available_bytes: i64_to_u64(
            "mem_available_bytes",
            row.try_get("mem_available_bytes")?,
        )?,
        swap_used_bytes: i64_to_u64("swap_used_bytes", row.try_get("swap_used_bytes")?)?,
        memory_detail: optional_detail_from_row(row, "memory_detail")?,
        oom_kills: i64_to_u64("oom_kills", row.try_get("oom_kills")?)?,
        disk_free_bytes: i64_to_u64("disk_free_bytes", row.try_get("disk_free_bytes")?)?,
        disk_inode_free_pct: row.try_get("disk_inode_free_pct")?,
        disk_detail: optional_detail_from_row(row, "disk_detail")?,
        nic_rx_bps: i64_to_u64("nic_rx_bps", row.try_get("nic_rx_bps")?)?,
        nic_tx_bps: i64_to_u64("nic_tx_bps", row.try_get("nic_tx_bps")?)?,
        nic_rx_drop: i64_to_u64("nic_rx_drop", row.try_get("nic_rx_drop")?)?,
        nic_tx_drop: i64_to_u64("nic_tx_drop", row.try_get("nic_tx_drop")?)?,
        nic_err: i64_to_u64("nic_err", row.try_get("nic_err")?)?,
        conntrack_count: row
            .try_get::<Option<i64>, _>("conntrack_count")?
            .map(|v| i64_to_u64("conntrack_count", v))
            .transpose()?,
        network_detail: optional_detail_from_row(row, "network_detail")?,
        uptime_secs: i64_to_u64("uptime_secs", row.try_get("uptime_secs")?)?,
    })
}

fn hop_sample_from_row(row: &sqlx::postgres::PgRow) -> Result<HopLinkSample> {
    Ok(HopLinkSample {
        chain_id: row.try_get("chain_id")?,
        peer_node_id: row.try_get("peer_node_id")?,
        window_start_unix_secs: row.try_get("window_start_secs")?,
        window_end_unix_secs: row.try_get("window_end_secs")?,
        conns: row.try_get::<i32, _>("conns")?.max(0) as u32,
        conns_measured: row.try_get::<i32, _>("conns_measured")?.max(0) as u32,
        btlbw_p50_bps: row
            .try_get::<Option<i64>, _>("btlbw_p50_bps")?
            .map(|v| i64_to_u64("btlbw_p50_bps", v))
            .transpose()?,
        btlbw_p90_bps: row
            .try_get::<Option<i64>, _>("btlbw_p90_bps")?
            .map(|v| i64_to_u64("btlbw_p90_bps", v))
            .transpose()?,
        min_rtt_us: row.try_get::<i32, _>("min_rtt_us")?.max(0) as u32,
        rtt_p50_us: row.try_get::<i32, _>("rtt_p50_us")?.max(0) as u32,
        rtt_p90_us: row.try_get::<i32, _>("rtt_p90_us")?.max(0) as u32,
        retrans_pct: row.try_get("retrans_pct")?,
        busy_pct: row.try_get("busy_pct")?,
        rwnd_limited_pct: row.try_get("rwnd_limited_pct")?,
        sndbuf_limited_pct: row.try_get("sndbuf_limited_pct")?,
    })
}

fn process_from_row(row: &sqlx::postgres::PgRow) -> Result<ProcessSample> {
    Ok(ProcessSample {
        proc: row.try_get("proc")?,
        rss_bytes: row
            .try_get::<Option<i64>, _>("rss_bytes")?
            .map(|v| i64_to_u64("rss_bytes", v))
            .transpose()?,
        cpu_pct: row.try_get("cpu_pct")?,
        started_at_unix_secs: row.try_get("started_at")?,
        fds: row
            .try_get::<Option<i64>, _>("fds")?
            .map(|v| i64_to_u64("fds", v))
            .transpose()?,
        fd_limit: row
            .try_get::<Option<i64>, _>("fd_limit")?
            .map(|v| i64_to_u64("fd_limit", v))
            .transpose()?,
    })
}

fn optional_json<T: serde::Serialize>(
    field: &str,
    value: Option<&T>,
) -> Result<Option<serde_json::Value>> {
    value
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| StoreError::InvalidData(format!("{field} not serializable: {error}")))
}

fn cpu_detail_json(value: Option<&CpuDetailSample>) -> Result<Option<serde_json::Value>> {
    match value {
        Some(detail) if validate_cpu_detail(detail).is_ok() => {
            optional_json("cpu_detail", Some(detail))
        }
        _ => Ok(None),
    }
}

fn memory_detail_json(value: Option<&MemoryDetailSample>) -> Result<Option<serde_json::Value>> {
    match value {
        Some(detail) if validate_memory_detail(detail).is_ok() => {
            optional_json("memory_detail", Some(detail))
        }
        _ => Ok(None),
    }
}

fn disk_detail_json(value: Option<&DiskDetailSample>) -> Result<Option<serde_json::Value>> {
    match value {
        Some(detail) if validate_disk_detail(detail).is_ok() => {
            optional_json("disk_detail", Some(detail))
        }
        _ => Ok(None),
    }
}

fn network_detail_json(value: Option<&NetworkDetailSample>) -> Result<Option<serde_json::Value>> {
    optional_json("network_detail", value)
}

fn optional_detail_from_row<T: serde::de::DeserializeOwned>(
    row: &sqlx::postgres::PgRow,
    field: &str,
) -> Result<Option<T>> {
    let raw: Option<serde_json::Value> = row.try_get(field)?;
    // A malformed/newer detail must not blank the entire load card. The stable scalar sample is
    // still useful, and absence already has an explicit "agent does not report this" UI state.
    Ok(raw.and_then(|value| serde_json::from_value(value).ok()))
}

#[cfg(test)]
fn validate_details(sample: &LoadSample) -> Result<()> {
    if let Some(detail) = &sample.cpu_detail {
        validate_cpu_detail(detail)?;
    }
    if let Some(detail) = &sample.memory_detail {
        validate_memory_detail(detail)?;
    }
    if let Some(detail) = &sample.disk_detail {
        validate_disk_detail(detail)?;
    }
    Ok(())
}

fn validate_cpu_detail(detail: &CpuDetailSample) -> Result<()> {
    strict_pct("cpu_detail.iowait_pct", detail.iowait_pct)?;
    finite("cpu_detail.load5", detail.load5)?;
    finite("cpu_detail.load15", detail.load15)?;
    for (field, value) in [
        ("cpu_detail.pressure_some_pct", detail.pressure_some_pct),
        (
            "cpu_detail.io_pressure_some_pct",
            detail.io_pressure_some_pct,
        ),
        (
            "cpu_detail.io_pressure_full_pct",
            detail.io_pressure_full_pct,
        ),
    ] {
        if let Some(value) = value {
            strict_pct(field, value)?;
        }
    }
    if detail.cores.len() > 1024 {
        return Err(StoreError::InvalidData(
            "cpu_detail carries more than 1024 logical CPUs".to_owned(),
        ));
    }
    let mut seen = BTreeSet::new();
    for core in &detail.cores {
        if !seen.insert(core.cpu) {
            return Err(StoreError::InvalidData(format!(
                "cpu_detail repeats logical CPU {}",
                core.cpu
            )));
        }
        let shares = [
            ("user", core.user_pct),
            ("system", core.system_pct),
            ("softirq", core.softirq_pct),
            ("iowait", core.iowait_pct),
            ("steal", core.steal_pct),
        ];
        let mut sum = 0.0;
        for (name, value) in shares {
            strict_pct(&format!("cpu_detail.cores[{}].{name}", core.cpu), value)?;
            sum += value;
        }
        if sum > 100.5 {
            return Err(StoreError::InvalidData(format!(
                "cpu_detail logical CPU {} shares add to {sum:.2}%",
                core.cpu
            )));
        }
    }
    Ok(())
}

fn validate_memory_detail(detail: &MemoryDetailSample) -> Result<()> {
    if let Some(value) = detail.pressure_some_pct {
        strict_pct("memory_detail.pressure_some_pct", value)?;
    }
    if let Some(value) = detail.pressure_full_pct {
        strict_pct("memory_detail.pressure_full_pct", value)?;
    }
    Ok(())
}

fn validate_disk_detail(detail: &DiskDetailSample) -> Result<()> {
    for (field, value) in [
        ("disk_detail.read_iops", detail.read_iops),
        ("disk_detail.write_iops", detail.write_iops),
        ("disk_detail.read_await_ms", detail.read_await_ms),
        ("disk_detail.write_await_ms", detail.write_await_ms),
        ("disk_detail.queue_depth", detail.queue_depth),
    ] {
        if let Some(value) = value {
            finite(field, value)?;
        }
    }
    for (field, value) in [
        ("disk_detail.busy_pct", detail.busy_pct),
        ("disk_detail.pressure_some_pct", detail.pressure_some_pct),
        ("disk_detail.pressure_full_pct", detail.pressure_full_pct),
    ] {
        if let Some(value) = value {
            strict_pct(field, value)?;
        }
    }
    Ok(())
}

fn strict_pct(field: &str, value: f32) -> Result<()> {
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        return Err(StoreError::InvalidData(format!(
            "{field} must be a percentage between 0 and 100"
        )));
    }
    Ok(())
}

/// Percentages carry two hazards the CHECK constraints would otherwise turn into a failed
/// transaction — taking every good sample in the round with them.
///
/// NaN is the first: serde_json renders it as `null`, so it does not survive the wire as NaN, but
/// an agent-side division that produced one still arrives as something. The second is a value over
/// 100, which a mis-scaled aggregation produces easily. Both are clamped rather than rejected: one
/// bad percentage should cost that field, not the round.
/// The three CPU shares, clamped individually **and** as a sum.
///
/// Clamping each one alone is not enough: three 99s add to 297, the table's CHECK refuses the row,
/// and because the whole report is one transaction that refusal takes every good sample in the
/// round with it. The same failure would also reach the console as a full-width bar — the three
/// are drawn stacked and normalised to 100.
///
/// Scaled proportionally rather than truncated, so the *ratio* between user, system and softirq
/// survives. That ratio is the entire reason the three are kept apart (see the column comments):
/// truncating softirq to fit would erase exactly the signal this split exists to carry.
fn cpu_split(s: &LoadSample) -> Result<(f32, f32, f32)> {
    let user = pct("cpu_user_pct", s.cpu_user_pct)?;
    let sys = pct("cpu_sys_pct", s.cpu_sys_pct)?;
    let softirq = pct("cpu_softirq_pct", s.cpu_softirq_pct)?;
    let sum = user + sys + softirq;
    if sum <= 100.0 {
        return Ok((user, sys, softirq));
    }
    let k = 100.0 / sum;
    Ok((user * k, sys * k, softirq * k))
}

fn pct(field: &str, value: f32) -> Result<f32> {
    if !value.is_finite() {
        return Err(StoreError::InvalidData(format!("{field} is not a number")));
    }
    Ok(value.clamp(0.0, 100.0))
}

fn finite(field: &str, value: f32) -> Result<f32> {
    if !value.is_finite() || value < 0.0 {
        return Err(StoreError::InvalidData(format!(
            "{field} must be a non-negative number"
        )));
    }
    Ok(value)
}

fn u64_to_i64(field: &str, value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::InvalidData(format!("{field} is out of range")))
}

fn i64_to_u64(field: &str, value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| StoreError::InvalidData(format!("{field} is out of range")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use brocade_deployment::protocol::{CpuCoreSample, CpuDetailSample};

    fn sample() -> LoadSample {
        LoadSample {
            window_start_unix_secs: 100,
            window_end_unix_secs: 130,
            has_gap: false,
            cpu_user_pct: 10.0,
            cpu_sys_pct: 5.0,
            cpu_softirq_pct: 3.0,
            cpu_peak_pct: 20.0,
            cpu_steal_pct: 0.0,
            load1: 0.2,
            cpu_detail: Some(CpuDetailSample {
                iowait_pct: 0.1,
                load5: 0.1,
                load15: 0.1,
                pressure_some_pct: Some(0.0),
                io_pressure_some_pct: Some(0.0),
                io_pressure_full_pct: Some(0.0),
                procs_running: Some(1),
                procs_total: Some(100),
                context_switches_per_sec: Some(1000),
                net_rx_softirqs_per_sec: Some(2000),
                net_tx_softirqs_per_sec: Some(100),
                throttled_usec: Some(0),
                frequency_mhz: None,
                cores: vec![CpuCoreSample {
                    cpu: 0,
                    user_pct: 10.0,
                    system_pct: 5.0,
                    softirq_pct: 3.0,
                    iowait_pct: 0.1,
                    steal_pct: 0.0,
                }],
            }),
            mem_available_bytes: 1024,
            swap_used_bytes: 0,
            memory_detail: None,
            oom_kills: 0,
            disk_free_bytes: 2048,
            disk_inode_free_pct: 99.0,
            disk_detail: None,
            nic_rx_bps: 0,
            nic_tx_bps: 0,
            nic_rx_drop: 0,
            nic_tx_drop: 0,
            nic_err: 0,
            conntrack_count: None,
            network_detail: None,
            uptime_secs: 10,
        }
    }

    #[test]
    fn deep_cpu_details_accept_unique_bounded_cores() {
        assert!(validate_details(&sample()).is_ok());
    }

    #[test]
    fn deep_cpu_details_reject_duplicate_core_ids() {
        let mut sample = sample();
        let detail = sample.cpu_detail.as_mut().unwrap();
        detail.cores.push(detail.cores[0].clone());
        let error = validate_details(&sample).unwrap_err().to_string();
        assert!(error.contains("repeats logical CPU 0"), "{error}");
    }

    #[test]
    fn deep_cpu_details_reject_impossible_core_shares() {
        let mut sample = sample();
        sample.cpu_detail.as_mut().unwrap().cores[0].softirq_pct = 90.0;
        let error = validate_details(&sample).unwrap_err().to_string();
        assert!(error.contains("shares add"), "{error}");
    }

    #[test]
    fn impossible_optional_cpu_detail_is_dropped_without_rejecting_the_scalar_sample() {
        let mut sample = sample();
        sample.cpu_detail.as_mut().unwrap().cores[0].softirq_pct = 90.0;
        let json = cpu_detail_json(sample.cpu_detail.as_ref()).unwrap();
        assert!(json.is_none());
        assert!(
            cpu_split(&sample).is_ok(),
            "the stable CPU split remains usable"
        );
    }

    #[test]
    fn impossible_optional_disk_detail_is_dropped_without_rejecting_capacity() {
        let mut sample = sample();
        sample.disk_detail = Some(DiskDetailSample {
            busy_pct: Some(f32::NAN),
            ..Default::default()
        });
        assert!(disk_detail_json(sample.disk_detail.as_ref())
            .unwrap()
            .is_none());
        assert_eq!(sample.disk_free_bytes, 2048);
    }

    #[test]
    fn metric_projection_preserves_every_retained_point() {
        let first = sample();
        let mut second = sample();
        second.window_end_unix_secs = 160;
        second.cpu_detail.as_mut().unwrap().iowait_pct = 2.5;
        let samples = vec![first, second];

        assert_eq!(
            load_metric_series(&samples, "cpu.iowait_pct"),
            Some(vec![Some(f64::from(0.1_f32)), Some(2.5)])
        );
        assert_eq!(
            load_metric_series(&samples, "cpu.user_pct"),
            Some(vec![Some(10.0), Some(10.0)])
        );
    }

    #[test]
    fn metric_projection_rejects_unknown_selectors() {
        assert_eq!(load_metric_series(&[sample()], "cpu.not-a-field"), None);
    }

    #[test]
    fn metric_batches_read_only_their_deep_json_families() {
        let metrics = BTreeSet::from([
            "cpu.iowait_pct",
            "cpu.pressure_some_pct",
            "network.tcp_curr_estab",
        ]);

        assert_eq!(
            LoadDetailColumns::for_metrics(&metrics),
            LoadDetailColumns {
                cpu: true,
                memory: false,
                disk: false,
                network: true,
            }
        );
    }

    #[test]
    fn overview_columns_preserve_every_sample_in_order() {
        let first = sample();
        let mut second = sample();
        second.window_start_unix_secs = 130;
        second.window_end_unix_secs = 160;
        second.has_gap = true;
        second.cpu_user_pct = 22.0;
        second.nic_rx_bps = 4_096;
        let view = NodeLoadView {
            node_id: "n1".to_owned(),
            range_start_unix_secs: 100,
            range_end_unix_secs: 160,
            reported_at_unix_secs: Some(160),
            clock_skew_secs: Some(0),
            host: None,
            latest_sample: Some(second.clone()),
            series: vec![first, second],
            processes: Vec::new(),
        };

        let overview = columnar_overview(view);

        assert_eq!(overview.series.window_end_unix_secs, vec![130, 160]);
        assert_eq!(overview.series.has_gap, vec![false, true]);
        assert_eq!(overview.series.cpu_user_pct, vec![10.0, 22.0]);
        assert_eq!(overview.series.nic_rx_bps, vec![0, 4_096]);
        assert_eq!(
            overview
                .latest_sample
                .as_ref()
                .and_then(|sample| sample.cpu_detail.as_ref())
                .map(|detail| detail.cores.len()),
            Some(1)
        );
    }
}
