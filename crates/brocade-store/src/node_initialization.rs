//! Durable, single-node bootstrap releases. Business configuration is never approved here.

use super::*;

pub const ACTOR: &str = "system:node-initialization";
const KIND: &str = "node-initialization";
const MAX_ATTEMPTS: i32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeInitializationState {
    WaitingAgent,
    Queued,
    Running,
    Succeeded,
    Manual,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeInitialization {
    pub state: NodeInitializationState,
    pub revision_id: u64,
    pub deployment_id: Option<i64>,
    pub reason: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Intent {
    node_id: String,
    revision_id: u64,
    requested_by: String,
    desired_sha256: Option<String>,
    #[serde(default)]
    deployment_id: Option<i64>,
}

pub(crate) async fn lock_tx(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    lock_deployment_isolation_tx(tx).await
}

fn fingerprint(target: &PlannedTarget) -> Result<String> {
    Ok(sha256_hex(&serde_json::to_vec(&target.desired)?))
}

fn compile_plan(snapshot: &ModelSnapshot, applied: &[NodeAppliedState]) -> Result<DeploymentPlan> {
    plan_snapshot_deployment(snapshot, applied).map_err(plan_error)
}

/// Only unchanged, explicitly queued bootstrap intent may be discounted as a sibling task.
async fn pending_intents_tx(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<BTreeMap<String, String>> {
    let rows = sqlx::query(
        "SELECT j.payload FROM jobs j JOIN nodes n ON n.id=j.payload->>'node_id'
          WHERE j.kind = $1 AND j.status IN ('queued', 'running')
            AND n.created_revision::text=j.payload->>'revision_id'
            AND (j.payload->>'deployment_id' IS NULL OR EXISTS (
                SELECT 1 FROM deployments d WHERE d.id::text=j.payload->>'deployment_id'
                  AND d.idempotency_key='node-initialization:' || j.id::text
                  AND d.status IN ('planned','running')))
          ORDER BY j.id",
    )
    .bind(KIND)
    .fetch_all(&mut **tx)
    .await?;
    let mut intents = BTreeMap::new();
    for row in rows {
        let intent: Intent = serde_json::from_value(row.try_get("payload")?)?;
        if let Some(sha) = intent.desired_sha256 {
            intents.insert(intent.node_id, sha);
        }
    }
    Ok(intents)
}

async fn backlog_reason_tx(
    tx: &mut Transaction<'_, Postgres>,
    plan: &DeploymentPlan,
    candidate: Option<&str>,
) -> Result<Option<String>> {
    let pending = pending_intents_tx(tx).await?;
    for target in &plan.targets {
        if Some(target.node_id.as_str()) == candidate || target.actions.is_empty() {
            continue;
        }
        if pending.get(&target.node_id) == Some(&fingerprint(target)?) {
            continue;
        }
        // Config work alone is a blocker; grants have their independent automation path.
        let config = narrow_to_kind(
            DeploymentPlan {
                targets: vec![target.clone()],
                ..plan.clone()
            },
            DeploymentKind::Config,
        );
        if config.summary.changed_targets > 0 {
            return Ok(Some(
                "已有其他机器的配置待发布或待收敛，请人工查看发布计划".to_owned(),
            ));
        }
    }
    let active_manual: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM deployments d WHERE d.kind = 'config' AND d.active
               AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.kind = $1
                   AND j.payload->>'deployment_id' = d.id::text
                   AND d.idempotency_key='node-initialization:' || j.id::text)
         ) OR EXISTS (
             SELECT 1 FROM node_convergence_obligations
              WHERE kind = 'config' AND status IN
                ('pending','dispatched','converging','failed-recovered','failed-dirty')
         )",
    )
    .bind(KIND)
    .fetch_one(&mut **tx)
    .await?;
    Ok(active_manual.then(|| "已有未完成的配置发布或收敛任务，请人工处理".to_owned()))
}

pub(crate) async fn enqueue_tx(
    tx: &mut Transaction<'_, Postgres>,
    before: &ModelSnapshot,
    revision_id: u64,
    node_id: &str,
    requested_by: &str,
) -> Result<()> {
    let applied = load_applied_states(&mut **tx).await?;
    let current = materialize::load_current_snapshot_tx(tx).await?;
    let mut reason;
    let frozen = match compile_plan(&current, &applied) {
        Ok(plan) => {
            let target = plan.targets.iter().find(|target| target.node_id == node_id);
            let node = current.nodes.iter().find(|node| node.id == node_id);
            if node.is_some_and(|node| node.overlay) {
                reason = Some("已启用 WireGuard，请人工确认发布范围".to_owned());
            } else if let Ok(previous) = compile_plan(before, &applied) {
                reason = backlog_reason_tx(tx, &previous, None).await?;
                if reason.is_none() {
                    reason = backlog_reason_tx(tx, &plan, Some(node_id)).await?;
                }
            } else {
                reason = Some("纳管前的模型无法编译，请人工检查配置".to_owned());
            }
            target.map(fingerprint).transpose()?
        }
        Err(_) => {
            reason = Some("当前模型无法编译，请人工检查配置".to_owned());
            None
        }
    };
    let intent = Intent {
        node_id: node_id.to_owned(),
        revision_id,
        requested_by: requested_by.to_owned(),
        desired_sha256: frozen,
        deployment_id: None,
    };
    sqlx::query(
        "INSERT INTO jobs (kind,status,payload,last_error)
         VALUES ($1,$2,$3,$4)",
    )
    .bind(KIND)
    .bind(if reason.is_some() {
        "canceled"
    } else {
        "queued"
    })
    .bind(serde_json::to_value(intent)?)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// One bounded attempt per tick; offline nodes consume no retry attempts.
pub(crate) async fn process(pool: &PgPool) -> Result<()> {
    sqlx::query(
        "UPDATE jobs j SET status=CASE WHEN d.status='succeeded' THEN 'succeeded' ELSE 'failed' END,
                updated_at=now()
           FROM deployments d WHERE j.kind=$1 AND j.status='running'
             AND d.id::text=j.payload->>'deployment_id'
             AND d.status IN ('succeeded','failed','halted','canceled')",
    ).bind(KIND).execute(pool).await?;
    let id = sqlx::query_scalar::<_, i64>(
        "SELECT j.id FROM jobs j
           JOIN nodes n ON n.id = j.payload->>'node_id' AND n.retired_at IS NULL
           JOIN node_agent_state a ON a.node_id = n.id
          WHERE j.kind = $1 AND j.status = 'queued' AND j.run_after <= now()
            AND n.created_revision::text=j.payload->>'revision_id'
            AND a.last_poll_at >= now() - interval '90 seconds'
            AND a.agent_protocol_version >= $2
          ORDER BY j.id LIMIT 1",
    )
    .bind(KIND)
    .bind(
        i32::try_from(CONFIG_XRAY_FORCE_RETIREMENT_PROTOCOL_VERSION).map_err(|_| {
            StoreError::InvalidData("initialization protocol version is out of range".to_owned())
        })?,
    )
    .fetch_optional(pool)
    .await?;
    let Some(id) = id else {
        return Ok(());
    };
    if let Err(error) = process_job(pool, id).await {
        sqlx::query(
            "UPDATE jobs SET attempts=attempts+1,
                status=CASE WHEN attempts+1 >= $2 THEN 'failed' ELSE 'queued' END,
                run_after=now()+interval '10 seconds',last_error=$3,updated_at=now()
              WHERE id=$1 AND status='queued'",
        )
        .bind(id)
        .bind(MAX_ATTEMPTS)
        .bind(format!("初始化计划创建失败：{error}"))
        .execute(pool)
        .await?;
        return Err(error);
    }
    Ok(())
}

async fn manual_tx(tx: &mut Transaction<'_, Postgres>, id: i64, reason: &str) -> Result<()> {
    sqlx::query("UPDATE jobs SET status='canceled',last_error=$2,updated_at=now() WHERE id=$1")
        .bind(id)
        .bind(reason)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn process_job(pool: &PgPool, id: i64) -> Result<()> {
    let mut tx = pool.begin().await?;
    lock_tx(&mut tx).await?;
    // Model writers take this same singleton lock; no business edit can enter between the
    // eligibility check and persisting the immutable single-node target.
    console::lock_control_state(&mut tx).await?;
    let row = sqlx::query("SELECT payload FROM jobs WHERE id=$1 AND status='queued' FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(row) = row else {
        return Ok(());
    };
    let mut intent: Intent = serde_json::from_value(row.try_get("payload")?)?;

    let registration_active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM nodes n JOIN node_lifecycle_state l ON l.node_id=n.id
          WHERE n.id=$1 AND n.created_revision=$2 AND n.retired_at IS NULL
            AND l.phase='active' AND l.lifecycle_epoch=0)",
    )
    .bind(&intent.node_id)
    .bind(revision_to_i64(intent.revision_id)?)
    .fetch_one(&mut *tx)
    .await?;
    if !registration_active {
        manual_tx(&mut tx, id, "机器生命周期已变化，请人工处理初始化").await?;
        tx.commit().await?;
        return Ok(());
    }
    // A manual order already owns this machine. Follow its receipt instead of creating another.
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT d.id FROM deployments d JOIN deployment_targets t ON t.deployment_id=d.id
          WHERE d.kind='config' AND t.node_id=$1 AND t.status <> 'skipped'
          ORDER BY d.id DESC LIMIT 1",
    )
    .bind(&intent.node_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(existing) = existing {
        intent.deployment_id = Some(existing);
        follow_tx(&mut tx, id, &intent).await?;
        tx.commit().await?;
        return Ok(());
    }
    let current = materialize::load_current_snapshot_tx(&mut tx).await?;
    let node = current.nodes.iter().find(|node| node.id == intent.node_id);
    let isolated = isolated_node_ids(&mut *tx).await?;
    if node.is_none_or(|node| node.retired || node.overlay) || isolated.contains(&intent.node_id) {
        manual_tx(
            &mut tx,
            id,
            "机器状态或 WireGuard 配置已变化，请人工检查发布计划",
        )
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    let applied = load_applied_states(&mut *tx).await?;
    let full = match compile_plan(&current, &applied) {
        Ok(plan) => plan,
        Err(_) => {
            manual_tx(&mut tx, id, "当前模型无法编译，请人工检查配置").await?;
            tx.commit().await?;
            return Ok(());
        }
    };
    let target = full
        .targets
        .iter()
        .find(|target| target.node_id == intent.node_id)
        .ok_or_else(|| {
            StoreError::InvalidData("initialization node is missing from plan".to_owned())
        })?;
    if Some(fingerprint(target)?) != intent.desired_sha256 || !target.prerequisites.is_empty() {
        manual_tx(
            &mut tx,
            id,
            "纳管后配置已变化，自动初始化不会夹带业务修改，请人工发布",
        )
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    if let Some(reason) = backlog_reason_tx(&mut tx, &full, Some(&intent.node_id)).await? {
        manual_tx(&mut tx, id, &reason).await?;
        tx.commit().await?;
        return Ok(());
    }
    let binary_active: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM binary_release_targets t
           JOIN binary_releases r ON r.id=t.release_id
          WHERE t.node_id=$1 AND r.active)",
    )
    .bind(&intent.node_id)
    .fetch_one(&mut *tx)
    .await?;
    let config_active: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM deployments WHERE kind='config' AND active)",
    )
    .fetch_one(&mut *tx)
    .await?;
    if config_active || binary_active {
        // Another bootstrap or this node's binary upgrade owns the execution slot. Waiting does
        // not consume a failure attempt and may never expand the selected target set.
        sqlx::query(
            "UPDATE jobs SET run_after=now()+interval '2 seconds',last_error=$2 WHERE id=$1",
        )
        .bind(id)
        .bind("等待正在执行的发布完成后初始化")
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    let fresh: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM node_agent_state WHERE node_id=$1
          AND last_poll_at >= now()-interval '90 seconds' AND agent_protocol_version >= $2)",
    )
    .bind(&intent.node_id)
    .bind(CONFIG_XRAY_FORCE_RETIREMENT_PROTOCOL_VERSION as i32)
    .fetch_one(&mut *tx)
    .await?;
    if !fresh {
        return Ok(());
    }
    let plan = narrow_to_kind(
        plan_desired_deployment(
            intent.revision_id,
            vec![(intent.node_id.clone(), target.desired.clone())],
            &applied,
            Vec::new(),
        ),
        DeploymentKind::Config,
    );
    if plan.summary.changed_targets == 0 {
        sqlx::query(
            "UPDATE jobs SET status='succeeded',last_error=NULL,updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    let request = CreateDeploymentRequest {
        revision_id: intent.revision_id,
        idempotency_key: format!("node-initialization:{id}"),
        actor: Some(ACTOR.to_owned()),
        note: Some(format!(
            "新机器初始化 {}（由 {} 纳管）",
            intent.node_id, intent.requested_by
        )),
        kind: DeploymentKind::Config,
    };
    let created = create_from_plan_tx(&mut tx, request, plan).await?;
    // Unknown local state still uses ordinary disable/read-back semantics. Approval of those
    // initial actions is recorded explicitly rather than falsifying the planner's risk flag.
    for target in &created.plan.targets {
        if target.disruptive
            && (target.wave > 1
                || target.actions.iter().any(|action| {
                    matches!(
                        action,
                        PlannedAction::DisableXray | PlannedAction::DisableWireGuard
                    )
                }))
        {
            sqlx::query("INSERT INTO deployment_wave_confirmations(deployment_id,wave,actor) VALUES($1,$2,$3)
                         ON CONFLICT DO NOTHING")
                .bind(created.deployment_id).bind(target.wave as i32).bind(ACTOR)
                .execute(&mut *tx).await?;
        }
    }
    intent.deployment_id = Some(created.deployment_id);
    follow_tx(&mut tx, id, &intent).await?;
    tx.commit().await?;
    Ok(())
}

async fn follow_tx(tx: &mut Transaction<'_, Postgres>, id: i64, intent: &Intent) -> Result<()> {
    sqlx::query(
        "UPDATE jobs SET status='running',payload=$2,last_error=NULL,updated_at=now() WHERE id=$1",
    )
    .bind(id)
    .bind(serde_json::to_value(intent)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn statuses(
    pool: &PgPool,
    node_ids: &[String],
) -> Result<BTreeMap<String, NodeInitialization>> {
    let rows = sqlx::query(
        "SELECT DISTINCT ON (j.payload->>'node_id') j.payload,j.status,j.last_error,
                manual.id AS deployment_id,
                manual.status AS deployment_status,
                a.agent_protocol_version,
                COALESCE(a.last_poll_at >= now()-interval '90 seconds',FALSE) AS online
           FROM jobs j
           LEFT JOIN node_agent_state a ON a.node_id=j.payload->>'node_id'
           LEFT JOIN LATERAL (
               SELECT candidate.id,
                      CASE WHEN t.status='succeeded' THEN 'succeeded' ELSE candidate.status END AS status
                 FROM deployments candidate
               JOIN deployment_targets t ON t.deployment_id=candidate.id
               WHERE candidate.kind='config' AND t.node_id=j.payload->>'node_id'
                 AND t.status<>'skipped'
               ORDER BY CASE WHEN t.status='succeeded' THEN 0 ELSE 1 END,candidate.id DESC LIMIT 1
           ) manual ON TRUE
          WHERE j.kind=$1 AND j.payload->>'node_id'=ANY($2)
          ORDER BY j.payload->>'node_id',j.id DESC",
    )
    .bind(KIND)
    .bind(node_ids)
    .fetch_all(pool)
    .await?;
    let mut result = BTreeMap::new();
    for row in rows {
        let intent: Intent = serde_json::from_value(row.try_get("payload")?)?;
        let status: String = row.try_get("status")?;
        let deployment_status: Option<String> = row.try_get("deployment_status")?;
        let unsupported = row.try_get::<bool, _>("online")?
            && row
                .try_get::<Option<i32>, _>("agent_protocol_version")?
                .is_none_or(|version| {
                    version < CONFIG_XRAY_FORCE_RETIREMENT_PROTOCOL_VERSION as i32
                });
        let state = match deployment_status.as_deref() {
            Some("succeeded") => NodeInitializationState::Succeeded,
            Some("planned" | "running") => NodeInitializationState::Running,
            Some(_) => NodeInitializationState::Failed,
            None => match status.as_str() {
                "succeeded" => NodeInitializationState::Succeeded,
                "canceled" => NodeInitializationState::Manual,
                "failed" => NodeInitializationState::Failed,
                _ if unsupported => NodeInitializationState::Manual,
                _ if row.try_get::<bool, _>("online")? => NodeInitializationState::Queued,
                _ => NodeInitializationState::WaitingAgent,
            },
        };
        result.insert(
            intent.node_id,
            NodeInitialization {
                state,
                revision_id: intent.revision_id,
                deployment_id: row.try_get("deployment_id")?,
                reason: if state == NodeInitializationState::Succeeded {
                    None
                } else if unsupported && state == NodeInitializationState::Manual {
                    Some("Agent 协议尚不支持初始化发布，请先升级 Agent".to_owned())
                } else if state == NodeInitializationState::Failed && deployment_status.is_some() {
                    Some("初始化发布未完成，请查看发布单并重试失败目标".to_owned())
                } else {
                    row.try_get("last_error")?
                },
            },
        );
    }
    Ok(result)
}
