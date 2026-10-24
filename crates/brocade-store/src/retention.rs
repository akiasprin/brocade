//! Runtime retention policy for bounded historical and temporary tables.
//!
//! The catalogue is deliberately compiled into the Store rather than discovered from PostgreSQL.
//! A schema name alone cannot say whether deleting old rows is safe: model snapshots, release
//! records and billing rollups are historical too, but are durable audit state. Keeping a small
//! allow-list makes the Settings page incapable of turning a core model table into TTL data.

pub(crate) mod cleanup;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{AdminContext, PgStore, Result, StoreError};

pub const MAX_RETENTION_HOURS: u32 = 10 * 365 * 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataRetentionCategory {
    History,
    Temporary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataRetentionPolicyView {
    pub table_name: String,
    pub title: String,
    pub description: String,
    pub category: DataRetentionCategory,
    pub ttl_hours: u32,
    pub default_ttl_hours: u32,
    pub min_ttl_hours: u32,
    pub max_ttl_hours: u32,
    pub customized: bool,
    /// PostgreSQL's statistics estimate; deliberately not presented as an exact count.
    pub estimated_rows: u64,
    pub total_bytes: u64,
    pub estimated_dead_rows: u64,
    pub last_vacuum_unix_secs: Option<i64>,
    pub cleanup: Option<RetentionCleanupView>,
}

/// Most recent cleanup observation. Backlog is unknown when its bounded probe could not finish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionCleanupView {
    pub ttl_hours: u32,
    pub finished_at_unix_secs: i64,
    pub last_success_unix_secs: Option<i64>,
    /// Rows removed by the last successful run, preserved if a later run fails.
    pub deleted_rows: u64,
    pub error_code: Option<String>,
    pub has_backlog: Option<bool>,
    pub next_run_unix_secs: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionCleanupResult {
    pub table_name: String,
    pub deleted_rows: u64,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataRetentionView {
    pub policies: Vec<DataRetentionPolicyView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateDataRetentionPolicy {
    pub ttl_hours: u32,
}

#[derive(Debug, Clone)]
pub struct RetentionSchedule {
    hours: HashMap<String, u32>,
}

impl RetentionSchedule {
    pub fn ttl_hours(&self, table_name: &str) -> u32 {
        self.hours
            .get(table_name)
            .copied()
            .or_else(|| definition(table_name).map(|item| item.default_ttl_hours))
            .expect("retention callers use a table from the static catalogue")
    }
}

#[derive(Debug, Clone, Copy)]
struct Definition {
    table_name: &'static str,
    title: &'static str,
    description: &'static str,
    category: DataRetentionCategory,
    default_ttl_hours: u32,
    min_ttl_hours: u32,
}

const fn history(
    table_name: &'static str,
    title: &'static str,
    description: &'static str,
    default_ttl_hours: u32,
) -> Definition {
    Definition {
        table_name,
        title,
        description,
        category: DataRetentionCategory::History,
        default_ttl_hours,
        min_ttl_hours: 1,
    }
}

const fn temporary(
    table_name: &'static str,
    title: &'static str,
    description: &'static str,
    default_ttl_hours: u32,
) -> Definition {
    Definition {
        table_name,
        title,
        description,
        category: DataRetentionCategory::Temporary,
        default_ttl_hours,
        min_ttl_hours: 1,
    }
}

const fn detail_heads(table_name: &'static str, title: &'static str) -> Definition {
    Definition {
        min_ttl_hours: 24,
        ..temporary(
            table_name,
            title,
            "明细计数段的幂等基线，至少覆盖 24 小时离线重放窗口。",
            24,
        )
    }
}

/// Only tables whose rows can be deleted by age without changing model, billing or release truth.
const DEFINITIONS: &[Definition] = &[
    history(
        "source_traffic_windows",
        "来源流量窗口",
        "用户、接入点与来源 IP 的 30 秒诊断观测；不参与计费。",
        24,
    ),
    history(
        "source_traffic_daily",
        "来源流量日汇总",
        "来源 IP 的每日诊断汇总；自选分钟区间依赖保留的来源窗口。",
        31 * 24,
    ),
    history(
        "node_ping_probe_samples",
        "Ping 探测样本",
        "机器到探测目标的逐次延迟、成功与跳过原因，用于链路历史图。",
        7 * 24,
    ),
    history(
        "node_load_samples",
        "机器负载样本",
        "CPU、内存、磁盘和网卡的观测窗口；超过一小时的查询优先使用分钟聚合。",
        7 * 24,
    ),
    history(
        "node_hop_link_samples",
        "中继链路样本",
        "中继连接的带宽、重传和受限比例窗口，用于定位链路瓶颈。",
        7 * 24,
    ),
    history(
        "e2e_probe_samples",
        "端到端探测样本",
        "每条链最近的可用性和首字节延迟趋势；当前状态另存，不依赖这些历史点。",
        6,
    ),
    history(
        "usage_readings",
        "原始用量读数",
        "Agent 上报的累计计数诊断记录；计费基线保存在独立的计数头中。",
        24,
    ),
    history(
        "usage_samples",
        "用户用量明细",
        "用户流量的细粒度窗口；月度与每日汇总不会随明细过期。",
        7 * 24,
    ),
    history(
        "usage_chain_samples",
        "链路用量明细",
        "中继链路流量的细粒度窗口；机器汇总另行长期保存。",
        7 * 24,
    ),
    history(
        "usage_node_windows",
        "机器用量窗口",
        "机器图表使用的逐上报窗口，保留空闲与缺报之间的区别。",
        7 * 24,
    ),
    history(
        "forwarding_usage_samples",
        "端口转发用量明细",
        "端口转发规则的原始累计读数；长期总量使用独立汇总。",
        7 * 24,
    ),
    history(
        "forwarding_usage_windows",
        "端口转发用量窗口",
        "由原始读数生成的短周期端口转发增量。",
        7 * 24,
    ),
    history(
        "user_site_usage_hourly",
        "站点分类小时明细",
        "用户访问内置站点分类的小时流量汇总。",
        31 * 24,
    ),
    history(
        "user_other_domain_hourly",
        "其他域名小时明细",
        "未进入内置分类的域名 Top-N 小时流量。",
        24,
    ),
    history(
        "user_sniffing_failed_hourly",
        "识别失败目标小时明细",
        "无法识别域名的目标 Top-N 小时流量。",
        24,
    ),
    history(
        "vpngate_candidate_probe_samples",
        "VPN Gate 候选探测样本",
        "候选出口的探测证据；当前选择使用独立的最新状态。",
        24,
    ),
    history(
        "vpngate_probe_samples",
        "VPN Gate 运行探测样本",
        "实际出口池的逐次拨测历史；最新结果和最后成功证据独立保存。",
        7 * 24,
    ),
    history(
        "vpngate_server_observations",
        "VPN Gate 目录观测",
        "目录同步时看到的服务器历史，用于短期追溯候选变化。",
        2 * 24,
    ),
    history(
        "external_outbound_probe_runs",
        "外部出口探测运行",
        "外部出口定时或手动探测的完成记录与结果。",
        7 * 24,
    ),
    history(
        "node_public_ip_events",
        "公网地址变更事件",
        "机器公网地址和归属地发生变化时留下的事件历史。",
        crate::public_ip::PUBLIC_IP_EVENT_RETENTION_DAYS * 24,
    ),
    history(
        "machine_events",
        "机器通知事件",
        "上下线、地址变化等机器事件的通知历史。",
        crate::notifications::MACHINE_EVENT_RETENTION_DAYS * 24,
    ),
    history(
        "user_online_sources",
        "用户在线来源历史",
        "账号来源地址的最近观测；过期离线节点留下的来源会被清理。",
        30 * 24,
    ),
    history(
        "node_traffic_forecasts",
        "机器流量预测快照",
        "每天第一次生成的容量预测，用于事后核对预测准确度。",
        800 * 24,
    ),
    history(
        "certificate_scan_runs",
        "证书扫描运行",
        "证书检查与签发扫描的成功、失败和阶段记录。",
        30 * 24,
    ),
    temporary(
        "usage_report_receipts",
        "用量上报回执",
        "重复上报时返回相同结果的幂等回执；永久游标不会随回执过期。",
        7 * 24,
    ),
    temporary(
        "forwarding_usage_report_receipts",
        "端口转发上报回执",
        "端口转发用量上报的短期幂等响应。",
        7 * 24,
    ),
    detail_heads("other_domain_counter_heads", "其他域名临时计数头"),
    detail_heads("sniffing_failed_counter_heads", "识别失败临时计数头"),
];

fn definition(table_name: &str) -> Option<&'static Definition> {
    DEFINITIONS
        .iter()
        .find(|item| item.table_name == table_name)
}

async fn load_overrides(pool: &sqlx::PgPool) -> Result<HashMap<String, u32>> {
    sqlx::query("SELECT table_name, ttl_hours FROM data_retention_policies")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|row| {
            let table_name: String = row.try_get("table_name")?;
            let ttl_hours: i32 = row.try_get("ttl_hours")?;
            let ttl_hours = u32::try_from(ttl_hours).map_err(|_| {
                StoreError::InvalidData(format!("negative retention for {table_name}"))
            })?;
            Ok((table_name, ttl_hours))
        })
        .collect()
}

pub(crate) async fn ttl_hours(pool: &sqlx::PgPool, table_name: &str) -> Result<u32> {
    let item = definition(table_name)
        .ok_or_else(|| StoreError::NotFound(format!("retention policy {table_name}")))?;
    let value = sqlx::query_scalar::<_, i32>(
        "SELECT ttl_hours FROM data_retention_policies WHERE table_name = $1",
    )
    .bind(table_name)
    .fetch_optional(pool)
    .await?;
    value
        .map(|value| {
            u32::try_from(value).map_err(|_| {
                StoreError::InvalidData(format!("negative retention for {table_name}"))
            })
        })
        .transpose()
        .map(|value| {
            value
                .unwrap_or(item.default_ttl_hours)
                .max(item.min_ttl_hours)
        })
}

impl PgStore {
    pub async fn retention_schedule(&self) -> Result<RetentionSchedule> {
        let mut hours = DEFINITIONS
            .iter()
            .map(|item| (item.table_name.to_owned(), item.default_ttl_hours))
            .collect::<HashMap<_, _>>();
        for (table_name, ttl_hours) in load_overrides(self.pool()).await? {
            if let Some(item) = definition(&table_name) {
                hours.insert(table_name, ttl_hours.max(item.min_ttl_hours));
            }
        }
        Ok(RetentionSchedule { hours })
    }

    pub async fn data_retention(&self) -> Result<DataRetentionView> {
        let overrides = load_overrides(self.pool()).await?;
        let table_names = DEFINITIONS
            .iter()
            .map(|item| item.table_name)
            .collect::<Vec<_>>();
        let stats = sqlx::query(
            "WITH names(table_name) AS (SELECT unnest($1::text[]))
             SELECT names.table_name,
                    COALESCE(stats.n_live_tup, 0)::bigint AS estimated_rows,
                    COALESCE(pg_total_relation_size(to_regclass(names.table_name)), 0)::bigint AS total_bytes,
                    COALESCE(stats.n_dead_tup, 0)::bigint AS estimated_dead_rows,
                    extract(epoch FROM greatest(stats.last_vacuum, stats.last_autovacuum))::bigint AS last_vacuum,
                    cleanup.ttl_hours AS cleanup_hours,
                    extract(epoch FROM cleanup.finished_at)::bigint AS cleanup_finished,
                    extract(epoch FROM cleanup.last_success_at)::bigint AS cleanup_success,
                    cleanup.deleted_rows, cleanup.error_code, cleanup.has_backlog,
                    extract(epoch FROM cleanup.next_run_at)::bigint AS cleanup_next
               FROM names
               LEFT JOIN pg_stat_user_tables stats ON stats.relid = to_regclass(names.table_name)
               LEFT JOIN data_retention_cleanup cleanup ON cleanup.table_name = names.table_name",
        )
        .bind(&table_names)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|row| {
            let name: String = row.try_get("table_name")?;
            let rows: i64 = row.try_get("estimated_rows")?;
            let bytes: i64 = row.try_get("total_bytes")?;
            let dead: i64 = row.try_get("estimated_dead_rows")?;
            let vacuum: Option<i64> = row.try_get("last_vacuum")?;
            let finished: Option<i64> = row.try_get("cleanup_finished")?;
            let cleanup = if let Some(finished_at_unix_secs) = finished {
                Some(RetentionCleanupView {
                    ttl_hours: u32::try_from(row.try_get::<i32, _>("cleanup_hours")?)
                        .map_err(|_| StoreError::InvalidData("negative cleanup TTL".to_owned()))?,
                    finished_at_unix_secs,
                    last_success_unix_secs: row.try_get("cleanup_success")?,
                    deleted_rows: u64::try_from(row.try_get::<i64, _>("deleted_rows")?)
                        .map_err(|_| StoreError::InvalidData("negative cleanup count".to_owned()))?,
                    error_code: row.try_get("error_code")?,
                    has_backlog: row.try_get("has_backlog")?,
                    next_run_unix_secs: row.try_get("cleanup_next")?,
                })
            } else { None };
            Ok((name, (rows.max(0) as u64, bytes.max(0) as u64, dead.max(0) as u64, vacuum, cleanup)))
        })
        .collect::<Result<HashMap<_, _>>>()?;

        Ok(DataRetentionView {
            policies: DEFINITIONS
                .iter()
                .map(|item| {
                    let override_hours = overrides.get(item.table_name).copied();
                    let (
                        estimated_rows,
                        total_bytes,
                        estimated_dead_rows,
                        last_vacuum_unix_secs,
                        cleanup,
                    ) = stats.get(item.table_name).cloned().unwrap_or_default();
                    DataRetentionPolicyView {
                        table_name: item.table_name.to_owned(),
                        title: item.title.to_owned(),
                        description: item.description.to_owned(),
                        category: item.category,
                        ttl_hours: override_hours
                            .unwrap_or(item.default_ttl_hours)
                            .max(item.min_ttl_hours),
                        default_ttl_hours: item.default_ttl_hours,
                        min_ttl_hours: item.min_ttl_hours,
                        max_ttl_hours: MAX_RETENTION_HOURS,
                        customized: override_hours.is_some(),
                        estimated_rows,
                        total_bytes,
                        estimated_dead_rows,
                        last_vacuum_unix_secs,
                        cleanup,
                    }
                })
                .collect(),
        })
    }

    pub async fn update_data_retention(
        &self,
        actor: &AdminContext,
        table_name: &str,
        request: UpdateDataRetentionPolicy,
    ) -> Result<DataRetentionView> {
        if !actor.is_system_admin() {
            return Err(StoreError::Forbidden(
                "only system-admin can update data retention".to_owned(),
            ));
        }
        let item = definition(table_name)
            .ok_or_else(|| StoreError::NotFound(format!("retention policy {table_name}")))?;
        if !(item.min_ttl_hours..=MAX_RETENTION_HOURS).contains(&request.ttl_hours) {
            return Err(StoreError::InvalidData(format!(
                "{table_name} TTL must be between {} and {MAX_RETENTION_HOURS} hours",
                item.min_ttl_hours
            )));
        }
        sqlx::query(
            "INSERT INTO data_retention_policies
                (table_name, ttl_hours, updated_at, updated_by)
             VALUES ($1, $2, now(), $3)
             ON CONFLICT (table_name) DO UPDATE SET
                ttl_hours = EXCLUDED.ttl_hours,
                updated_at = EXCLUDED.updated_at,
                updated_by = EXCLUDED.updated_by",
        )
        .bind(table_name)
        .bind(i32::try_from(request.ttl_hours).expect("bounded retention fits i32"))
        .bind(actor.operator_id())
        .execute(self.pool())
        .await?;
        self.data_retention().await
    }

    /// Run due tables independently. A per-table failure is reported without skipping others.
    pub async fn run_data_retention(&self) -> Result<Vec<RetentionCleanupResult>> {
        let schedule = self.retention_schedule().await?;
        let mut results = Vec::new();
        for item in DEFINITIONS {
            match cleanup::run_one(self.pool(), item, &schedule).await {
                Ok(Some(result)) => results.push(result),
                Ok(None) => {}
                Err(_) => results.push(RetentionCleanupResult {
                    table_name: item.table_name.to_owned(),
                    deleted_rows: 0,
                    // Database/connection failures can prevent recording status itself. Never
                    // expose a raw database message, which can contain values or credentials.
                    error_code: Some("status_unavailable".to_owned()),
                }),
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_event_defaults_are_seven_days_and_overrides_remain_effective() {
        let mut schedule = RetentionSchedule {
            hours: HashMap::new(),
        };
        for table in ["node_public_ip_events", "machine_events"] {
            assert_eq!(definition(table).unwrap().default_ttl_hours, 7 * 24);
            assert_eq!(schedule.ttl_hours(table), 7 * 24);
            schedule.hours.insert(table.to_owned(), 48);
            assert_eq!(schedule.ttl_hours(table), 48);
        }
    }

    #[test]
    fn catalogue_exposes_only_bounded_operational_data() {
        for core in [
            "tenants",
            "users",
            "nodes",
            "apps",
            "chains",
            "revisions",
            "model_snapshots",
            "deployments",
            "usage_rollups",
            "usage_counter_heads",
        ] {
            assert!(
                definition(core).is_none(),
                "core table {core} leaked into TTL catalogue"
            );
        }
        assert!(definition("node_ping_probe_samples").is_some());
        assert!(definition("usage_report_receipts").is_some());
        assert!(DEFINITIONS
            .iter()
            .all(|item| item.default_ttl_hours >= item.min_ttl_hours));
    }
}
