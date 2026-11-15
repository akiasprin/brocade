//! Versioned operational maintenance settings and idempotent, bounded Agent receipts.
mod assets;
mod compat;
use crate::{AdminContext, PgStore, Result, StoreError};
use brocade_deployment::ip_maintenance::*;
use sqlx::{Postgres, Row, Transaction};

pub(crate) const MAX_RETAINED_RUNS: i64 = 100;

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::InvalidData(message.into())
}
fn integer(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| invalid("养护版本或回执序号超出范围"))
}
fn administrator(actor: &AdminContext) -> Result<()> {
    if actor.is_system_admin() {
        Ok(())
    } else {
        Err(StoreError::Forbidden("仅系统管理员可管理机器养护".into()))
    }
}
async fn lock_node(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<()> {
    let exists = sqlx::query_scalar::<_, String>(
        "SELECT id FROM nodes WHERE id=$1 AND retired_at IS NULL FOR UPDATE",
    )
    .bind(node)
    .fetch_optional(&mut **tx)
    .await?;
    if exists.is_none() {
        return Err(StoreError::NotFound("机器不存在或已停用".into()));
    }
    Ok(())
}
async fn desired_tx(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
) -> Result<IpMaintenanceDesired> {
    let row = sqlx::query("SELECT generation,policy,control_sequence,manual_request_id,stop_sequence,target_round_id,manual_sequence,manual_expires_at,session_generation FROM ip_maintenance_policies WHERE node_id=$1")
        .bind(node).fetch_optional(&mut **tx).await?;
    let Some(row) = row else {
        return Ok(IpMaintenanceDesired::default());
    };
    let unsigned = |field: &str| -> Result<u64> {
        u64::try_from(row.try_get::<i64, _>(field)?).map_err(|_| invalid("养护策略包含负版本"))
    };
    Ok(IpMaintenanceDesired {
        generation: unsigned("generation")?,
        policy: serde_json::from_value(row.try_get("policy")?)?,
        control_sequence: unsigned("control_sequence")?,
        manual_request_id: row.try_get("manual_request_id")?,
        stop_sequence: unsigned("stop_sequence")?,
        target_round_id: row.try_get("target_round_id")?,
        manual_sequence: unsigned("manual_sequence")?,
        manual_expires_at: row.try_get("manual_expires_at")?,
        session_generation: unsigned("session_generation")?,
        report_sequence_floor: 0,
        data_digest: None,
    })
}
async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    actor: &AdminContext,
    desired: &IpMaintenanceDesired,
    kind: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO ip_maintenance_events(node_id,generation,actor_id,event) VALUES($1,$2,$3,$4)",
    )
    .bind(node)
    .bind(integer(desired.generation)?)
    .bind(actor.operator_id())
    .bind(serde_json::json!({"kind":kind,"desired":desired}))
    .execute(&mut **tx)
    .await?;
    Ok(())
}
fn snapshot_reports(json: &serde_json::Value) -> Result<Vec<IpMaintenanceReport>> {
    if let Some(reports) = json.get("family_reports") {
        let reports: Vec<IpMaintenanceReport> = serde_json::from_value(reports.clone())?;
        if reports.len() > 2 {
            return Err(invalid("养护地址族回执超出限制"));
        }
        return Ok(reports);
    }
    Ok(vec![serde_json::from_value(json.clone())?])
}
fn report_family(report: &IpMaintenanceReport) -> Option<MaintenanceFamily> {
    report.family.or_else(|| {
        report.actual_exit_ip.map(|ip| {
            if ip.is_ipv4() {
                MaintenanceFamily::V4
            } else {
                MaintenanceFamily::V6
            }
        })
    })
}

fn restore_evidence(
    report: &mut IpMaintenanceReport,
    desired: &IpMaintenanceDesired,
    observations: &[IpMaintenanceReport],
) {
    if report.generation != desired.generation
        || report.applied_session_generation != desired.session_generation
    {
        report.evidence.clear();
        report.evidence_observed_at_unix_secs = None;
        return;
    }
    if report.evidence_observed_at_unix_secs.is_some() {
        return;
    }
    let family = report_family(report).or_else(|| {
        (desired.policy.family != MaintenanceFamily::Dual).then_some(desired.policy.family)
    });
    if let Some(previous) = observations
        .iter()
        .find(|previous| report_family(previous) == family)
    {
        // Only the live view borrows the last complete observation. Round history
        // and its actual actions/evidence remain the Agent's original receipt.
        report.evidence.clone_from(&previous.evidence);
        report.evidence_observed_at_unix_secs = previous.evidence_observed_at_unix_secs;
    }
}
impl PgStore {
    async fn require_maintenance_reader(&self, actor: &AdminContext, node: &str) -> Result<()> {
        if actor.is_system_admin() {
            return Ok(());
        }
        // Maintenance follows the machine inventory's tenant visibility, including visitors.
        let visible = self.list_node_agent_states(actor).await?;
        if visible.nodes.iter().any(|item| item.node_id == node) {
            Ok(())
        } else {
            Err(StoreError::NotFound(format!("node {node}")))
        }
    }

    pub async fn ip_maintenance_desired(&self, node: &str) -> Result<IpMaintenanceDesired> {
        self.maintenance_desired(node, true).await
    }
    async fn maintenance_desired(
        &self,
        node: &str,
        enforce_lifecycle: bool,
    ) -> Result<IpMaintenanceDesired> {
        let mut tx = self.pool.begin().await?;
        let mut desired = desired_tx(&mut tx, node).await?;
        desired.report_sequence_floor = u64::try_from(
            sqlx::query_scalar::<_, i64>(
                "SELECT coalesce((SELECT sequence FROM ip_maintenance_state WHERE node_id=$1),0)",
            )
            .bind(node)
            .fetch_one(&mut *tx)
            .await?,
        )
        .map_err(|_| invalid("养护回执序号无效"))?;
        let key = format!("region:{}", desired.policy.region);
        let digests = sqlx::query("SELECT key,sha256 FROM ip_maintenance_assets WHERE key=ANY($1)")
            .bind(vec![key.clone(), "agents".to_owned()])
            .fetch_all(&mut *tx)
            .await?;
        let seed =
            data::region(&desired.policy.region).ok_or_else(|| invalid("所选养护地区不存在"))?;
        let mut region_digest = data::value_digest(seed).map_err(invalid)?;
        let mut agents_digest = data::seed_agents_digest().to_owned();
        for row in digests {
            if row.try_get::<String, _>("key")? == key {
                region_digest = row.try_get("sha256")?;
            } else {
                agents_digest = row.try_get("sha256")?;
            }
        }
        desired.data_digest = Some(data::manifest_digest(&region_digest, &agents_digest));
        let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT FROM node_lifecycle_state l JOIN nodes n ON n.id=l.node_id WHERE n.id=$1 AND n.retired_at IS NULL AND l.phase='active')")
            .bind(node).fetch_one(&mut *tx).await?;
        if enforce_lifecycle && !active {
            desired.policy.enabled = false;
            desired.manual_expires_at = None;
        }
        tx.commit().await?;
        Ok(desired)
    }
    pub async fn ip_maintenance_view(
        &self,
        actor: &AdminContext,
        node: &str,
    ) -> Result<IpMaintenanceView> {
        self.require_maintenance_reader(actor, node).await?;
        let supported: Option<bool> = sqlx::query_scalar("SELECT coalesce(a.agent_protocol_version>= $2,false) FROM nodes n LEFT JOIN node_agent_state a ON a.node_id=n.id WHERE n.id=$1")
            .bind(node).bind(IP_MAINTENANCE_PROTOCOL_VERSION as i32).fetch_optional(&self.pool).await?;
        let supported = supported.ok_or_else(|| StoreError::NotFound("机器".into()))?;
        let desired = self.maintenance_desired(node, false).await?;
        let dual_stack_supported: bool = sqlx::query_scalar("SELECT EXISTS(SELECT FROM node_agent_state WHERE node_id=$1 AND agent_protocol_version >= $2)")
            .bind(node).bind(IP_MAINTENANCE_DUAL_PROTOCOL_VERSION as i32).fetch_one(&self.pool).await?;
        let supported =
            supported && (desired.policy.family != MaintenanceFamily::Dual || dual_stack_supported);
        let row = sqlx::query("SELECT report,extract(epoch FROM received_at)::bigint AS received_at FROM ip_maintenance_state WHERE node_id=$1")
            .bind(node).fetch_optional(&self.pool).await?;
        let (mut report, mut family_reports, received_at_unix_secs) = match row {
            Some(row) => {
                let json: serde_json::Value = row.try_get("report")?;
                (
                    Some(serde_json::from_value(json.clone())?),
                    snapshot_reports(&json)?,
                    Some(row.try_get("received_at")?),
                )
            }
            None => (None, Vec::new(), None),
        };
        let needs_evidence = report.iter().chain(family_reports.iter()).any(|report| {
            report.generation == desired.generation
                && report.applied_session_generation == desired.session_generation
                && report.evidence_observed_at_unix_secs.is_none()
        });
        let observations = if needs_evidence {
            // Retention bounds each node to 2,000 rounds; at most one observation
            // per address family leaves this query. Arrival order is not probe time.
            sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT DISTINCT ON (family) report FROM (
                    SELECT id,report,coalesce(report->>'family',
                        CASE WHEN report->>'actual_exit_ip' LIKE '%:%' THEN 'v6'
                             WHEN report->>'actual_exit_ip' IS NOT NULL THEN 'v4' END) AS family
                    FROM ip_maintenance_runs WHERE node_id=$1
                      AND (report->>'generation')::bigint=$2
                      AND coalesce((report->>'applied_session_generation')::bigint,0)=$3
                      AND report->>'evidence_observed_at_unix_secs' IS NOT NULL
                ) observations WHERE family IN ('v4','v6')
                ORDER BY family,(report->>'evidence_observed_at_unix_secs')::bigint DESC,id DESC",
            )
            .bind(node)
            .bind(integer(desired.generation)?)
            .bind(integer(desired.session_generation)?)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<Vec<IpMaintenanceReport>, _>>()?
        } else {
            Vec::new()
        };
        for report in report.iter_mut().chain(family_reports.iter_mut()) {
            restore_evidence(report, &desired, &observations);
        }
        let data_state=sqlx::query("SELECT (SELECT fetched_at FROM ip_maintenance_assets WHERE key=$1) AS region_fetched,(SELECT fetched_at FROM ip_maintenance_assets WHERE key='agents') AS agents_fetched,(SELECT last_error FROM ip_maintenance_refresh WHERE id=TRUE) AS error")
            .bind(format!("region:{}",desired.policy.region)).fetch_one(&self.pool).await?;
        Ok(IpMaintenanceView {
            default_policy: IpMaintenancePolicy::default(),
            desired,
            supported,
            dual_stack_supported,
            family_reports,
            session_reset_supported: sqlx::query_scalar("SELECT EXISTS(SELECT FROM node_agent_state WHERE node_id=$1 AND agent_protocol_version >= $2)")
                .bind(node).bind(IP_MAINTENANCE_RESET_PROTOCOL_VERSION as i32).fetch_one(&self.pool).await?,
            lifecycle_active:sqlx::query_scalar("SELECT EXISTS(SELECT FROM node_lifecycle_state l JOIN nodes n ON n.id=l.node_id WHERE n.id=$1 AND n.retired_at IS NULL AND l.phase='active')").bind(node).fetch_one(&self.pool).await?,
            report,
            received_at_unix_secs,
            data_updated_at_unix_secs: data_state.try_get("region_fetched")?,
            agents_updated_at_unix_secs: data_state.try_get("agents_fetched")?,
            data_error: data_state.try_get("error")?,
        })
    }
    pub async fn update_ip_maintenance(
        &self,
        actor: &AdminContext,
        node: &str,
        request: UpdateIpMaintenance,
    ) -> Result<()> {
        administrator(actor)?;
        request.policy.validate().map_err(invalid)?;
        if data::region(&request.policy.region).is_none() {
            return Err(invalid("所选养护地区不存在"));
        }
        let mut tx = self.pool.begin().await?;
        lock_node(&mut tx, node).await?;
        if request.policy.enabled || request.policy.family == MaintenanceFamily::Dual {
            let supported: bool = sqlx::query_scalar("SELECT coalesce((SELECT agent_protocol_version >= $2 FROM node_agent_state WHERE node_id=$1),false)")
                .bind(node).bind(request.policy.protocol_version() as i32).fetch_one(&mut *tx).await?;
            if !supported {
                return Err(StoreError::Unsupported(
                    "请先升级支持所选养护地址族的 Agent".into(),
                ));
            }
        }
        let previous = desired_tx(&mut tx, node).await?;
        if previous.generation != request.expected_generation {
            return Err(StoreError::Conflict(
                "养护设置已被更新，请刷新后重试".into(),
            ));
        }
        let next = previous
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("养护版本已达上限"))?;
        sqlx::query("INSERT INTO ip_maintenance_policies(node_id,generation,policy,actor_id) VALUES($1,$2,$3,$4) ON CONFLICT(node_id) DO UPDATE SET generation=EXCLUDED.generation,policy=EXCLUDED.policy,manual_expires_at=NULL,control_sequence=ip_maintenance_policies.control_sequence+1,actor_id=EXCLUDED.actor_id,updated_at=now()")
            .bind(node).bind(integer(next)?).bind(serde_json::to_value(&request.policy)?).bind(actor.operator_id()).execute(&mut *tx).await?;
        let desired = desired_tx(&mut tx, node).await?;
        audit(&mut tx, node, actor, &desired, "policy").await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn command_ip_maintenance(
        &self,
        actor: &AdminContext,
        node: &str,
        request: IpMaintenanceCommand,
        reset_session: bool,
    ) -> Result<()> {
        self.maintenance_control(
            actor,
            node,
            request,
            if reset_session {
                "session_reset"
            } else {
                "run"
            },
            None,
        )
        .await
    }
    pub async fn stop_ip_maintenance(
        &self,
        actor: &AdminContext,
        node: &str,
        request: IpMaintenanceStop,
    ) -> Result<()> {
        if request.round_id.is_empty()
            || request.round_id.len() > 80
            || request.round_id.chars().any(char::is_control)
        {
            return Err(invalid("停止目标无效"));
        }
        self.maintenance_control(
            actor,
            node,
            IpMaintenanceCommand {
                expected_generation: request.expected_generation,
                request_id: request.request_id,
            },
            "stop",
            Some(request.round_id),
        )
        .await
    }
    async fn maintenance_control(
        &self,
        actor: &AdminContext,
        node: &str,
        request: IpMaintenanceCommand,
        kind: &str,
        target: Option<String>,
    ) -> Result<()> {
        administrator(actor)?;
        request.validate().map_err(invalid)?;
        let mut tx = self.pool.begin().await?;
        lock_node(&mut tx, node).await?;
        let duplicate=sqlx::query("SELECT kind,expected_generation,target_round_id FROM ip_maintenance_commands WHERE node_id=$1 AND request_id=$2")
            .bind(node).bind(&request.request_id).fetch_optional(&mut *tx).await?;
        if let Some(row) = duplicate {
            if row.try_get::<String, _>("kind")? != kind
                || row.try_get::<i64, _>("expected_generation")?
                    != integer(request.expected_generation)?
                || row.try_get::<Option<String>, _>("target_round_id")? != target
            {
                return Err(StoreError::Conflict("请求标识已用于另一个操作".into()));
            }
            return Ok(());
        }
        let desired = desired_tx(&mut tx, node).await?;
        if desired.generation != request.expected_generation {
            return Err(StoreError::Conflict("养护设置已更新，请刷新后重试".into()));
        }
        if desired.generation == 0 {
            return Err(StoreError::Conflict("请先保存养护策略".into()));
        }
        if kind == "stop" {
            // Reports and controls share the node lock. An old page must not replace
            // a pending stop for the current round with a different target.
            let json = sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT report FROM ip_maintenance_state WHERE node_id=$1",
            )
            .bind(node)
            .fetch_optional(&mut *tx)
            .await?;
            let matches = json
                .as_ref()
                .map(snapshot_reports)
                .transpose()?
                .unwrap_or_default()
                .iter()
                .any(|r| r.round_id.as_ref() == target.as_ref());
            if !matches {
                return Err(StoreError::Conflict(
                    "停止目标已不是最近回执中的轮次，请刷新后重试".into(),
                ));
            }
        }
        if kind != "stop" {
            let protocol = if kind == "session_reset" {
                IP_MAINTENANCE_RESET_PROTOCOL_VERSION.max(desired.policy.protocol_version())
            } else {
                desired.policy.protocol_version()
            };
            let supported:bool=sqlx::query_scalar("SELECT EXISTS(SELECT FROM node_agent_state a JOIN node_lifecycle_state l ON l.node_id=a.node_id WHERE a.node_id=$1 AND a.agent_protocol_version >= $2 AND l.phase='active')")
                .bind(node).bind(protocol as i32).fetch_one(&mut *tx).await?;
            if !supported {
                return Err(StoreError::Unsupported(
                    "请先升级并启用支持养护的 Agent".into(),
                ));
            }
            let row=sqlx::query("SELECT report,extract(epoch FROM now())::bigint AS stamp FROM ip_maintenance_state WHERE node_id=$1").bind(node).fetch_optional(&mut *tx).await?;
            let stamp: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
                .fetch_one(&mut *tx)
                .await?;
            let reported = row
                .as_ref()
                .map(|row| row.try_get::<serde_json::Value, _>("report"))
                .transpose()?;
            if kind == "session_reset" && desired.policy.family == MaintenanceFamily::Dual {
                let reports = reported
                    .as_ref()
                    .map(snapshot_reports)
                    .transpose()?
                    .unwrap_or_default();
                if [MaintenanceFamily::V4, MaintenanceFamily::V6]
                    .into_iter()
                    .any(|family| {
                        !reports.iter().any(|r| {
                            report_family(r) == Some(family)
                                && r.generation == desired.generation
                                && r.applied_control_sequence >= desired.control_sequence
                        })
                    })
                {
                    return Err(StoreError::Conflict("请等待两个养护出口确认空闲".into()));
                }
            }
            let reset_acknowledged = reported
                .as_ref()
                .and_then(|r| r.get("applied_session_generation").and_then(|v| v.as_u64()))
                .is_some_and(|session| session == desired.session_generation);
            if desired.session_generation > 0 && !reset_acknowledged {
                return Err(StoreError::Conflict("请等待养护会话重置完成".into()));
            }
            let acknowledged = row
                .as_ref()
                .and_then(|row| row.try_get::<serde_json::Value, _>("report").ok())
                .and_then(|r| r.get("applied_control_sequence").and_then(|v| v.as_u64()))
                .is_some_and(|seq| seq >= desired.control_sequence);
            if !acknowledged && desired.manual_expires_at.is_some_and(|v| v >= stamp) {
                return Err(StoreError::Conflict("已有待执行的手动请求".into()));
            }
            if let Some(row) = row {
                let reports = snapshot_reports(&row.try_get::<serde_json::Value, _>("report")?)?;
                if reports.iter().any(|report| report.phase.active()) {
                    return Err(StoreError::Conflict("请先停止当前轮次并等待确认".into()));
                }
                if kind == "run"
                    && reports
                        .iter()
                        .all(|report| report.cooldown_until_unix_secs.is_some_and(|v| v > stamp))
                {
                    return Err(StoreError::Conflict("养护仍在冷却中".into()));
                }
            }
            if kind == "run" && !desired.policy.google && !desired.policy.trust {
                return Err(StoreError::Conflict("请先开启一个养护模块".into()));
            }
        }
        let control = desired
            .control_sequence
            .checked_add(1)
            .ok_or_else(|| invalid("控制序号已达上限"))?;
        match kind {
            "run" => {
                sqlx::query("UPDATE ip_maintenance_policies SET control_sequence=$2,manual_sequence=manual_sequence+1,manual_request_id=$3,manual_expires_at=extract(epoch FROM now())::bigint+120,actor_id=$4,updated_at=now() WHERE node_id=$1")
                .bind(node).bind(integer(control)?).bind(&request.request_id).bind(actor.operator_id()).execute(&mut *tx).await?;
            }
            "session_reset" => {
                sqlx::query("UPDATE ip_maintenance_policies SET generation=generation+1,policy=jsonb_set(policy,'{enabled}','false'::jsonb),control_sequence=$2,session_generation=session_generation+1,manual_request_id=NULL,manual_expires_at=NULL,target_round_id=NULL,actor_id=$3,updated_at=now() WHERE node_id=$1")
                .bind(node).bind(integer(control)?).bind(actor.operator_id()).execute(&mut *tx).await?;
                sqlx::query("DELETE FROM ip_maintenance_state WHERE node_id=$1")
                    .bind(node)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM ip_maintenance_runs WHERE node_id=$1")
                    .bind(node)
                    .execute(&mut *tx)
                    .await?;
            }
            "stop" => {
                sqlx::query("UPDATE ip_maintenance_policies SET control_sequence=$2,stop_sequence=$2,target_round_id=$3,actor_id=$4,updated_at=now() WHERE node_id=$1")
                .bind(node).bind(integer(control)?).bind(&target).bind(actor.operator_id()).execute(&mut *tx).await?;
            }
            _ => unreachable!(),
        }
        sqlx::query("INSERT INTO ip_maintenance_commands(node_id,request_id,kind,expected_generation,target_round_id,control_sequence) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(node).bind(&request.request_id).bind(kind).bind(integer(request.expected_generation)?).bind(target).bind(integer(control)?).execute(&mut *tx).await?;
        let desired = desired_tx(&mut tx, node).await?;
        audit(&mut tx, node, actor, &desired, kind).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn ip_maintenance_runs(
        &self,
        actor: &AdminContext,
        node: &str,
        cursor: Option<u64>,
    ) -> Result<IpMaintenanceRuns> {
        self.require_maintenance_reader(actor, node).await?;
        let rows=sqlx::query("SELECT id,report FROM ip_maintenance_runs WHERE node_id=$1 AND ($2::bigint IS NULL OR id<$2) ORDER BY id DESC LIMIT 21")
            .bind(node).bind(cursor.map(integer).transpose()?).fetch_all(&self.pool).await?;
        let more = rows.len() > 20;
        let mut runs = Vec::new();
        let mut next_cursor = None;
        for row in rows.into_iter().take(20) {
            // The record sheet derives its chart and region flags from these pages.
            let report: IpMaintenanceReport = serde_json::from_value(row.try_get("report")?)?;
            if more {
                next_cursor = Some(
                    u64::try_from(row.try_get::<i64, _>("id")?)
                        .map_err(|_| invalid("历史游标无效"))?,
                );
            }
            runs.push(report);
        }
        Ok(IpMaintenanceRuns { runs, next_cursor })
    }
    pub async fn ip_maintenance_run(
        &self,
        actor: &AdminContext,
        node: &str,
        run: &str,
    ) -> Result<IpMaintenanceReport> {
        self.require_maintenance_reader(actor, node).await?;
        let json = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT report FROM ip_maintenance_runs WHERE node_id=$1 AND round_id=$2",
        )
        .bind(node)
        .bind(run)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound("养护轮次".into()))?;
        Ok(serde_json::from_value(json)?)
    }
    pub async fn report_ip_maintenance(
        &self,
        node: &str,
        report: IpMaintenanceReport,
    ) -> Result<()> {
        report.validate().map_err(invalid)?;
        let mut tx = self.pool.begin().await?;
        lock_node(&mut tx, node).await?;
        let desired = desired_tx(&mut tx, node).await?;
        if report.generation > desired.generation
            || report.applied_control_sequence > desired.control_sequence
            || report.applied_stop_sequence > desired.stop_sequence
            || report.applied_session_generation > desired.session_generation
        {
            return Err(invalid("养护回执来自未知策略或控制版本"));
        }
        // A reset is a new session. Acknowledge buffered old receipts without allowing them to
        // recreate the deleted current state or history after the transaction commits.
        if report.applied_session_generation < desired.session_generation {
            tx.commit().await?;
            return Ok(());
        }
        let json = serde_json::to_value(&report)?;
        let previous = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT report FROM ip_maintenance_state WHERE node_id=$1",
        )
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?;
        let old_reports = previous
            .as_ref()
            .map(snapshot_reports)
            .transpose()?
            .unwrap_or_default();
        let terminal_seen = report.round_id.as_ref().is_some_and(|round| {
            old_reports
                .iter()
                .any(|r| r.round_id.as_ref() == Some(round) && r.phase.terminal())
        });
        let mut state_json = json.clone();
        if let Some(family) = report_family(&report) {
            let mut families = std::collections::BTreeMap::new();
            for prior in old_reports {
                if prior.applied_session_generation == desired.session_generation {
                    if let Some(family) = report_family(&prior) {
                        families.insert(family, prior);
                    }
                }
            }
            families.insert(family, report.clone());
            state_json["family_reports"] =
                serde_json::to_value(families.into_values().collect::<Vec<_>>())?;
        }
        sqlx::query("INSERT INTO ip_maintenance_state(node_id,sequence,report) VALUES($1,$2,$3) ON CONFLICT(node_id) DO UPDATE SET sequence=EXCLUDED.sequence,report=EXCLUDED.report,received_at=now() WHERE ip_maintenance_state.sequence<EXCLUDED.sequence AND (ip_maintenance_state.report->>'generation')::bigint<=($3->>'generation')::bigint AND (ip_maintenance_state.report->>'applied_control_sequence')::bigint<=($3->>'applied_control_sequence')::bigint")
            .bind(node).bind(integer(report.sequence)?).bind(&state_json).execute(&mut *tx).await?;
        if let Some(round) = &report.round_id {
            let fresh:bool=sqlx::query_scalar("SELECT NOT EXISTS(SELECT FROM ip_maintenance_runs WHERE node_id=$1 AND round_id=$2)").bind(node).bind(round).fetch_one(&mut *tx).await?;
            // A health refresh of the last completed round must not resurrect expired history.
            if !fresh || !terminal_seen {
                sqlx::query("INSERT INTO ip_maintenance_runs(node_id,round_id,report) VALUES($1,$2,$3) ON CONFLICT(node_id,round_id) DO UPDATE SET report=EXCLUDED.report,received_at=now() WHERE (ip_maintenance_runs.report->>'sequence')::bigint < ($3->>'sequence')::bigint AND (ip_maintenance_runs.report->>'generation')::bigint<=($3->>'generation')::bigint AND (ip_maintenance_runs.report->>'applied_control_sequence')::bigint<=($3->>'applied_control_sequence')::bigint AND ip_maintenance_runs.report->>'phase' IN ('jitter','preflight','requesting','dwelling','postcheck')")
                    .bind(node).bind(round).bind(&json).execute(&mut *tx).await?;
                if fresh {
                    sqlx::query("DELETE FROM ip_maintenance_runs WHERE node_id=$1 AND id IN(SELECT id FROM ip_maintenance_runs WHERE node_id=$1 ORDER BY id DESC OFFSET $2)")
                        .bind(node).bind(MAX_RETAINED_RUNS).execute(&mut *tx).await?;
                }
            }
        }
        tx.commit().await?;
        Ok(())
    }
}
