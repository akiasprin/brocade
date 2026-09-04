use brocade_core::client_config::ClientProjectionDownloadEndpoint;
use brocade_core::hash::sha256_hex;
use brocade_core::model::{
    AnyTlsMasquerade, AppView, Dns, DomainStrategy, ExternalOutbound, HysteriaCongestion,
    HysteriaMasquerade, HysteriaObfs, Ingress, IngressWires, ModelSnapshot, Node, Projection,
    ProjectionDownloadEndpoint, ProjectionEndpoint, RealityFallbackLimits, RealityFallbackMode,
    RealitySettings, RealitySite, Transport, XhttpDownload,
};
use brocade_deployment::plan::{
    grants_match, narrow_to_kind, plan_deployment as plan_snapshot_deployment,
    plan_desired_deployment, plan_forced_deployment as plan_forced_snapshot_deployment,
    AppliedArtifactState, AppliedGrantsState, ConfigArtifact, DeploymentKind, DeploymentPlan,
    DesiredArtifact, DesiredGrants, NodeAppliedState, NodeDesiredState, ObservedInbound,
    PlanDiagnostic, PlannedTarget, PlannedTargetStatus,
};
use brocade_deployment::protocol::{
    CreateDeploymentRequest, CreateDeploymentResult, CreateRollbackRequest,
    DeploymentCommandResult, DeploymentDetail, DeploymentList, DeploymentListItem,
    DeploymentTargetDetail, DeploymentWaveConfirmationResult, IsolateDeploymentTargetRequest,
    NodeDesiredDeployment, NodeIsolationCommandResult, ReportTargetResult, ReportedNodeState,
    RestoreNodeServiceRequest, TargetApplyResult, TargetConvergenceReport,
};
use serde_json::{json, Value};
use sqlx::{Executor, PgConnection, PgPool, Postgres, Row, Transaction};
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    console, materialize, settings, AdminContext, NodeLifecyclePhase,
    NodeLifecycleTransitionResult, Result, StoreError,
};

const DISPATCH_LEASE_INTERVAL: &str = "15 minutes";

struct CanceledDeployment {
    status: String,
    active: Option<bool>,
    uncertain_nodes: Vec<String>,
}

struct RollbackTarget {
    deployment_id: i64,
    revision_id: u64,
}

type WarpBindingTokenMap = BTreeMap<(String, String, String, String), String>;

/// The automatic grants worker's atomic result. `deferred` contains real conflicts: a
/// configuration target already handed to an agent is changing xray, so a permission writer may
/// not race it. Pending targets are rebased and ordered after the permission writer instead.
pub(crate) struct AutomaticGrantsDeploymentResult {
    pub deployment_id: Option<i64>,
    pub deferred: Vec<String>,
}

pub async fn plan_deployment(
    pool: &PgPool,
    actor: &AdminContext,
    revision_id: u64,
) -> Result<DeploymentPlan> {
    // This is the plan exposed to the console.  Permission-only work is automatic and must not
    // make the manual "create change order" button light up; creation below defaults to Config
    // and applies this same narrowing.
    let mut plan = narrow_to_kind(
        plan_full_deployment(pool, actor, revision_id).await?,
        DeploymentKind::Config,
    );
    // The preview page lists machine by machine what this deployment turns the artifacts into:
    // its baseline is the one fixed when the deployment was created (the same query as at
    // creation), and everything the preview page creates is a configuration deployment.
    plan.base_revision_id = last_succeeded_revision(pool, DeploymentKind::Config).await?;
    let isolated = isolated_node_ids(pool).await?;
    mark_isolated_targets(&mut plan, &isolated);
    Ok(plan)
}

/// The automatic permission worker needs the unsplit plan first: whether SyncGrants may run now
/// depends on the accompanying xray actions.  It computes that prerequisite, then narrows to the
/// grants line itself.
pub(crate) async fn plan_full_deployment(
    pool: &PgPool,
    actor: &AdminContext,
    revision_id: u64,
) -> Result<DeploymentPlan> {
    scope_plan(
        pool,
        actor,
        plan_deployment_unscoped(pool, revision_id).await?,
    )
    .await
}

async fn plan_deployment_unscoped(pool: &PgPool, revision_id: u64) -> Result<DeploymentPlan> {
    let snapshot = load_snapshot_for_deployment(pool, revision_id).await?;
    let mut applied = load_applied_states(pool).await?;
    attach_running_xray(pool, &mut applied).await?;
    let plan = plan_snapshot_deployment(&snapshot, &applied).map_err(plan_error)?;
    let terminal = terminal_lifecycle_nodes(pool).await?;
    Ok(without_terminal_lifecycle_targets(plan, &terminal))
}

async fn terminal_lifecycle_nodes<'e, E>(executor: E) -> Result<BTreeSet<String>>
where
    E: Executor<'e, Database = Postgres>,
{
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT node_id
           FROM node_lifecycle_state
          WHERE phase IN ('retired', 'abandoned')",
    )
    .fetch_all(executor)
    .await?
    .into_iter()
    .collect())
}

fn without_terminal_lifecycle_targets(
    mut plan: DeploymentPlan,
    terminal: &BTreeSet<String>,
) -> DeploymentPlan {
    plan.targets
        .retain(|target| !terminal.contains(&target.node_id));
    plan.summary = brocade_deployment::plan::summarize_targets(&plan.targets);
    plan
}

async fn isolated_node_ids<'e, E>(executor: E) -> Result<BTreeSet<String>>
where
    E: Executor<'e, Database = Postgres>,
{
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT node_id FROM node_operational_isolations ORDER BY node_id FOR SHARE",
    )
    .fetch_all(executor)
    .await?
    .into_iter()
    .collect())
}

fn mark_isolated_targets(plan: &mut DeploymentPlan, isolated: &BTreeSet<String>) {
    for target in &mut plan.targets {
        if target.status == PlannedTargetStatus::Pending && isolated.contains(&target.node_id) {
            target.status = PlannedTargetStatus::Deferred;
        }
    }
    plan.summary = brocade_deployment::plan::summarize_targets(&plan.targets);
}

/// Works out, for each machine, the text of the xray config it is running.
///
/// Nowhere is it stored. `deployment_target_state` keeps a digest and a byte count, not the
/// artifact — deliberately, since the artifacts are large and reproducible. So the text is
/// reproduced: the last configuration release names the revision the fleet was brought to,
/// and compiling that revision yields what each machine was handed.
///
/// Compiling it is not the same as knowing it arrived. A machine may have been skipped, may
/// have failed, may have drifted since. The digest settles it — the recompiled text is
/// accepted only where it hashes to what the machine itself reported, and left out
/// otherwise. Left out means the planner assumes a restart, which is what it assumed before
/// any of this existed.
async fn attach_running_xray(pool: &PgPool, applied: &mut [NodeAppliedState]) -> Result<()> {
    let Some(baseline) = last_succeeded_revision(pool, DeploymentKind::Config).await? else {
        return Ok(());
    };
    let snapshot = load_snapshot_for_deployment(pool, baseline.max(0) as u64).await?;
    // Planning against no applied state at all: only the desired artifacts are wanted here,
    // and the actions the planner would derive are of no interest.
    let Ok(plan) = plan_snapshot_deployment(&snapshot, &[]) else {
        // The baseline no longer compiles — the model has moved on in ways that revision
        // cannot express. Nothing is known about what is running, which is a safe answer.
        return Ok(());
    };
    let compiled = plan
        .targets
        .into_iter()
        .filter_map(|target| match target.desired.xray {
            DesiredArtifact::Present { content, sha256 } => {
                Some((target.node_id, (content, sha256)))
            }
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();

    for state in applied.iter_mut() {
        let AppliedArtifactState::Present { sha256 } = &state.xray else {
            continue;
        };
        if let Some((content, compiled_sha)) = compiled.get(&state.node_id) {
            if compiled_sha == sha256 {
                state.running_xray = Some(content.clone());
            }
        }
    }
    Ok(())
}

pub async fn create_deployment(
    pool: &PgPool,
    actor: &AdminContext,
    request: CreateDeploymentRequest,
) -> Result<CreateDeploymentResult> {
    if request.idempotency_key.trim().is_empty() {
        return Err(StoreError::InvalidData(
            "deployment idempotency_key must not be empty".to_owned(),
        ));
    }

    // Scope by the operator's tenant subtree first, then narrow to this kind of deployment. The
    // order cannot invert: narrowing recomputes the summary, and narrowing before scoping leaves
    // machines outside the subtree in it.
    let mut plan = narrow_to_kind(
        scope_plan(
            pool,
            actor,
            plan_deployment_unscoped(pool, request.revision_id).await?,
        )
        .await?,
        request.kind,
    );
    // A deployment that moves no machine may not be created. The release history is where people
    // look up when the configuration last changed, and a string of successful deployments that
    // did nothing buries the few that really moved something. The automatic quota-enforcement
    // path already tests changed_targets == 0 itself, so tightening this does not affect it.
    if plan.summary.changed_targets == 0 {
        return Err(StoreError::InvalidData(
            if plan.summary.total_targets == 0 {
                "cannot create deployment for an empty target set".to_owned()
            } else {
                "cannot create deployment: every node is already converged".to_owned()
            },
        ));
    }

    let mut tx = pool.begin().await?;
    let isolated = isolated_node_ids(&mut *tx).await?;
    mark_isolated_targets(&mut plan, &isolated);

    if let Some(existing) = sqlx::query(
        "SELECT id, revision_id, status
         FROM deployments
         WHERE idempotency_key = $1",
    )
    .bind(&request.idempotency_key)
    .fetch_optional(&mut *tx)
    .await?
    {
        let existing_revision = revision_to_u64(existing.try_get("revision_id")?)?;
        if existing_revision != request.revision_id {
            return Err(StoreError::InvalidData(format!(
                "idempotency_key {} already belongs to revision {}, requested {}",
                request.idempotency_key, existing_revision, request.revision_id
            )));
        }
        let deployment_id = existing.try_get("id")?;
        let status = existing.try_get("status")?;
        tx.commit().await?;
        return Ok(CreateDeploymentResult {
            deployment_id,
            status,
            reused: true,
            plan,
        });
    }

    // Single-flight is per kind: while somebody is pushing xray in waves (confirming each,
    // possibly for minutes), grant changes should not queue behind it — the two kinds do not
    // interfere.
    if let Some(active) =
        sqlx::query("SELECT id FROM deployments WHERE active = TRUE AND kind = $1 LIMIT 1")
            .bind(request.kind.as_str())
            .fetch_optional(&mut *tx)
            .await?
    {
        return Err(StoreError::Unsupported(format!(
            "another {} deployment is active: {}",
            request.kind.as_str(),
            active.try_get::<i64, _>("id")?
        )));
    }

    // The gate above guarantees there is work here, so there is no "created already succeeded"
    // branch.
    let status = "planned";
    let active = Some(true);
    let warnings = serde_json::to_value(&plan.warnings)?;
    let revision_id = revision_to_i64(request.revision_id)?;
    let base_revision_id = last_succeeded_revision(&mut *tx, request.kind).await?;
    let note = match request.note.as_deref().map(str::trim) {
        Some(written) if !written.is_empty() => written.to_owned(),
        _ => default_note(&mut tx, request.revision_id, &plan).await?,
    };

    let row = sqlx::query(
        "INSERT INTO deployments (
            revision_id, status, actor, idempotency_key, active, warnings, note, kind,
            base_revision_id, finished_at
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9,
            CASE WHEN $5::boolean IS TRUE THEN NULL ELSE now() END
         )
         RETURNING id",
    )
    .bind(revision_id)
    .bind(status)
    .bind(&request.actor)
    .bind(&request.idempotency_key)
    .bind(active)
    .bind(warnings)
    .bind(&note)
    .bind(request.kind.as_str())
    .bind(base_revision_id)
    .fetch_one(&mut *tx)
    .await?;
    let deployment_id = row.try_get("id")?;

    for target in &plan.targets {
        insert_target(&mut tx, deployment_id, target).await?;
    }

    let result_status = if plan
        .targets
        .iter()
        .any(|target| target.status == PlannedTargetStatus::Pending)
    {
        status.to_owned()
    } else {
        refresh_deployment_status(&mut tx, deployment_id, "deferred").await?
    };

    tx.commit().await?;
    Ok(CreateDeploymentResult {
        deployment_id,
        status: result_status,
        reused: false,
        plan,
    })
}

/// Change a machine's lifecycle as one operational action: commit the model intent, fence every
/// older target, cancel active work orders, and create a replacement configuration deployment
/// from the newest revision. Retirement is therefore never a label which still waits for someone
/// to remember a separate publish step.
pub async fn transition_node_status(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    request: console::UpdateNodeStatusRequest,
) -> Result<NodeLifecycleTransitionResult> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can change node lifecycle".to_owned(),
        ));
    }
    let status = request.status.trim();
    let retiring = match status {
        "retired" => true,
        "active" => false,
        other => {
            return Err(StoreError::InvalidData(format!(
                "node status must be active or retired, got {other}"
            )))
        }
    };
    let note = console::note_or(request.note.as_deref(), || {
        if retiring {
            format!("retire node {node_id} and create teardown deployment")
        } else {
            format!("reactivate node {node_id} and create convergence deployment")
        }
    });

    let mut tx = pool.begin().await?;
    // Report, retry, cancel and rollback all lock the deployment row before any target or
    // lifecycle row. Preserve that global order here too: changing the epoch first and only then
    // waiting for an in-flight report's deployment row forms the inverse lock order and lets a
    // simultaneous retirement/report deadlock.
    let active_ids = sqlx::query_scalar::<_, i64>(
        "SELECT id
           FROM deployments
          WHERE active = TRUE AND status IN ('planned', 'running', 'halted')
          ORDER BY CASE kind WHEN 'config' THEN 0 ELSE 1 END, id
          FOR UPDATE",
    )
    .fetch_all(&mut *tx)
    .await?;
    let uncertain_obligations = sqlx::query(
        "SELECT source_deployment_id, kind
           FROM node_convergence_obligations
          WHERE node_id = $1
            AND status IN ('dispatched', 'converging')",
    )
    .bind(node_id)
    .fetch_all(&mut *tx)
    .await?;
    let previous = console::lock_control_state(&mut tx).await?;
    let proposed_revision = console::insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let changed =
        console::update_node_status_tx(&mut tx, actor, proposed_revision, node_id, request, &note)
            .await?;
    let revision_id =
        console::commit_revision(&mut tx, proposed_revision, previous, changed).await?;

    let lifecycle = sqlx::query(
        "SELECT lifecycle_epoch, phase, deployment_id
           FROM node_lifecycle_state
          WHERE node_id = $1
          FOR UPDATE",
    )
    .bind(node_id)
    .fetch_one(&mut *tx)
    .await?;
    let lifecycle_epoch: i64 = lifecycle.try_get("lifecycle_epoch")?;
    let lifecycle_phase: String = lifecycle.try_get("phase")?;
    let attached_deployment_id: Option<i64> = lifecycle.try_get("deployment_id")?;
    let attached_is_open = if let Some(deployment_id) = attached_deployment_id {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
                 SELECT 1
                   FROM deployments
                  WHERE id = $1
                    AND active = TRUE
                    AND status IN ('planned', 'running', 'halted')
             )",
        )
        .bind(deployment_id)
        .fetch_one(&mut *tx)
        .await?
    } else {
        false
    };
    // Repeating an ordinary status write is a no-op. Repeating retirement while its operational
    // work order is missing or terminal is deliberately different: it is the repair path for a
    // legacy retired_at backfill and for a canceled teardown, and reuses the same epoch rather
    // than pretending that model intent changed again.
    let repairs_missing_teardown = !changed
        && retiring
        && lifecycle_phase == NodeLifecyclePhase::Retiring.as_str()
        && !attached_is_open;
    if !changed && !repairs_missing_teardown {
        tx.commit().await?;
        let lifecycle = crate::lifecycle::load(pool, node_id).await?;
        return Ok(NodeLifecycleTransitionResult {
            revision_id,
            node_id: node_id.to_owned(),
            deployment_id: lifecycle.deployment_id,
            lifecycle,
            canceled_deployment_ids: Vec::new(),
        });
    }

    for obligation in uncertain_obligations {
        let source_deployment_id: i64 = obligation.try_get("source_deployment_id")?;
        let kind_value: String = obligation.try_get("kind")?;
        let kind = DeploymentKind::parse(&kind_value).ok_or_else(|| {
            StoreError::InvalidData(format!(
                "deployment {source_deployment_id} has unknown obligation kind {kind_value}"
            ))
        })?;
        mark_node_applied_dirty_tx(
            &mut tx,
            source_deployment_id,
            node_id,
            kind,
            "节点生命周期代次变化时隔离债务仍在执行，运行态不确定",
        )
        .await?;
    }

    // Both configuration and hot-grant targets carry the old epoch. Leaving either active can
    // block its wave forever even though the changed machine can no longer claim it, so cancel
    // both complete work orders and let the ordinary workers re-plan from the newest revision.
    let mut canceled_deployment_ids = Vec::new();
    for deployment_id in active_ids {
        cancel_deployment_tx(&mut tx, deployment_id).await?;
        canceled_deployment_ids.push(deployment_id);
    }

    let snapshot = materialize::load_current_snapshot_tx(&mut tx).await?;
    let applied = load_applied_states(&mut *tx).await?;
    let plan = narrow_to_kind(
        plan_snapshot_deployment(&snapshot, &applied).map_err(plan_error)?,
        DeploymentKind::Config,
    );
    let terminal = terminal_lifecycle_nodes(&mut *tx).await?;
    let mut plan = without_terminal_lifecycle_targets(plan, &terminal);
    if lifecycle_phase == NodeLifecyclePhase::Active.as_str() {
        let isolated = isolated_node_ids(&mut *tx).await?;
        mark_isolated_targets(&mut plan, &isolated);
    }
    let retirement_target_pending = plan
        .targets
        .iter()
        .any(|target| target.node_id == node_id && target.status == PlannedTargetStatus::Pending);
    let deployment_id = if plan.summary.changed_targets > 0 {
        let idempotency_base = format!("system:node-lifecycle:{node_id}:{lifecycle_epoch}");
        let prior_attempts = sqlx::query_scalar::<_, i64>(
            "SELECT count(*)
               FROM deployments
              WHERE idempotency_key = $1
                 OR idempotency_key LIKE $1 || ':retry:%'",
        )
        .bind(&idempotency_base)
        .fetch_one(&mut *tx)
        .await?;
        let idempotency_key = if prior_attempts == 0 {
            idempotency_base
        } else {
            format!("{idempotency_base}:retry:{prior_attempts}")
        };
        let deployment_id = insert_lifecycle_deployment_tx(
            &mut tx,
            actor.operator_id(),
            &idempotency_key,
            &note,
            &plan,
        )
        .await?;
        if lifecycle_phase == NodeLifecyclePhase::Retiring.as_str() {
            if !retirement_target_pending {
                return Err(StoreError::InvalidData(format!(
                    "retiring node {node_id} has no teardown target"
                )));
            }
            crate::lifecycle::attach_deployment_tx(
                &mut tx,
                node_id,
                lifecycle_epoch,
                deployment_id,
            )
            .await?;
        }
        Some(deployment_id)
    } else {
        if lifecycle_phase == NodeLifecyclePhase::Retiring.as_str() {
            crate::lifecycle::complete_retirement_tx(
                &mut tx,
                node_id,
                lifecycle_epoch,
                None,
                actor.operator_id(),
            )
            .await?;
        }
        None
    };

    tx.commit().await?;
    Ok(NodeLifecycleTransitionResult {
        revision_id,
        node_id: node_id.to_owned(),
        lifecycle: crate::lifecycle::load(pool, node_id).await?,
        deployment_id,
        canceled_deployment_ids,
    })
}

pub async fn abandon_node(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    request: crate::AbandonNodeRequest,
) -> Result<NodeLifecycleTransitionResult> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can force-retire nodes".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    // Same lock order as report/cancel: deployment, then lifecycle/target.
    let active_ids = sqlx::query_scalar::<_, i64>(
        "SELECT id
           FROM deployments
          WHERE active = TRUE AND status IN ('planned', 'running', 'halted')
          ORDER BY CASE kind WHEN 'config' THEN 0 ELSE 1 END, id
          FOR UPDATE",
    )
    .fetch_all(&mut *tx)
    .await?;
    let lifecycle_epoch =
        crate::lifecycle::abandon_tx(&mut tx, node_id, actor.operator_id(), &request.reason)
            .await?;
    let mut canceled_deployment_ids = Vec::new();
    for deployment_id in active_ids {
        cancel_deployment_tx(&mut tx, deployment_id).await?;
        canceled_deployment_ids.push(deployment_id);
    }

    let snapshot = materialize::load_current_snapshot_tx(&mut tx).await?;
    let applied = load_applied_states(&mut *tx).await?;
    let plan = narrow_to_kind(
        plan_snapshot_deployment(&snapshot, &applied).map_err(plan_error)?,
        DeploymentKind::Config,
    );
    let terminal = terminal_lifecycle_nodes(&mut *tx).await?;
    let plan = without_terminal_lifecycle_targets(plan, &terminal);
    let deployment_id = if plan.summary.changed_targets > 0 {
        Some(
            insert_lifecycle_deployment_tx(
                &mut tx,
                actor.operator_id(),
                &format!("system:node-abandon:{node_id}:{lifecycle_epoch}"),
                &format!("replace active deployment after force-retiring node {node_id}"),
                &plan,
            )
            .await?,
        )
    } else {
        None
    };
    let revision_id = snapshot.revision;
    tx.commit().await?;
    Ok(NodeLifecycleTransitionResult {
        revision_id,
        node_id: node_id.to_owned(),
        lifecycle: crate::lifecycle::load(pool, node_id).await?,
        deployment_id,
        canceled_deployment_ids,
    })
}

async fn insert_lifecycle_deployment_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor_id: &str,
    idempotency_key: &str,
    note: &str,
    plan: &DeploymentPlan,
) -> Result<i64> {
    let warnings = serde_json::to_value(&plan.warnings)?;
    let revision_id = revision_to_i64(plan.revision)?;
    let base_revision_id = last_succeeded_revision(&mut **tx, DeploymentKind::Config).await?;
    let deployment_id: i64 = sqlx::query_scalar(
        "INSERT INTO deployments (
             revision_id, status, actor, idempotency_key, active, warnings, note, kind,
             base_revision_id
         )
         VALUES ($1, 'planned', $2, $3, TRUE, $4, $5, 'config', $6)
         RETURNING id",
    )
    .bind(revision_id)
    .bind(actor_id)
    .bind(idempotency_key)
    .bind(warnings)
    .bind(note)
    .bind(base_revision_id)
    .fetch_one(&mut **tx)
    .await?;
    for target in &plan.targets {
        insert_target(tx, deployment_id, target).await?;
    }
    if !plan
        .targets
        .iter()
        .any(|target| target.status == PlannedTargetStatus::Pending)
    {
        refresh_deployment_status(tx, deployment_id, "deferred").await?;
    }
    Ok(deployment_id)
}

/// Create the permission work that can safely run against what each machine is running now.
///
/// A normal plan is compiled from the newest model. That is the wrong topology for this job when
/// configuration work is waiting: it may contain an inbound that does not exist yet, or omit one
/// the machine still serves. Here the topology comes from the configuration whose xray digest the
/// machine actually reported, while users and grant rows come from `revision_id`.
///
/// Pending configuration targets are locked and rebased to the same newest permissions before the
/// grants deployment is inserted. Therefore an old configuration order cannot later put the old
/// list back. Once a target has been dispatched its snapshot is immutable; an xray-changing target
/// in that state is returned as a conflict and the durable job retries immediately after it lands.
pub(crate) async fn create_automatic_grants_deployment(
    pool: &PgPool,
    actor: &AdminContext,
    revision_id: u64,
    note: &str,
) -> Result<AutomaticGrantsDeploymentResult> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;

    let latest = materialize::load_snapshot_tx(&mut tx, Some(revision_id)).await?;
    let applied = load_applied_states(&mut *tx).await?;
    let applied_by_node = applied
        .iter()
        .map(|state| (state.node_id.clone(), state.clone()))
        .collect::<BTreeMap<_, _>>();

    // Match the reported xray digest to the configuration target that supplied those exact bytes.
    // `source_deployment_id` is insufficient: a later config order that moved only WireGuard also
    // becomes the source, even though it deliberately left xray Unmanaged.
    let running_rows = sqlx::query(
        "SELECT DISTINCT ON (s.node_id)
                s.node_id, d.revision_id
         FROM node_applied_state s
         JOIN deployment_target_state dts
           ON dts.node_id = s.node_id
          AND dts.desired_structure #>> '{xray,state}' = 'present'
          AND dts.desired_structure #>> '{xray,sha256}' = s.xray_sha256
         JOIN deployment_targets dt
           ON dt.deployment_id = dts.deployment_id
          AND dt.node_id = dts.node_id
          AND dt.status = 'succeeded'
         JOIN node_lifecycle_state lifecycle
           ON lifecycle.node_id = s.node_id
          AND lifecycle.lifecycle_epoch = dts.lifecycle_epoch
          AND lifecycle.phase IN ('active', 'retiring')
         JOIN deployments d
           ON d.id = dts.deployment_id
          AND d.kind = 'config'
         WHERE s.xray_state = 'present'
         ORDER BY s.node_id, d.id DESC",
    )
    .fetch_all(&mut *tx)
    .await?;
    let running_revision_by_node = running_rows
        .into_iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>("node_id")?,
                revision_to_u64(row.try_get("revision_id")?)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;

    // Lock every non-terminal configuration target before deciding whether it conflicts. Agent
    // claim takes the same target/state locks, so a row cannot cross from pending to dispatched
    // between the rebase and insertion of the grants deployment.
    let config_rows = sqlx::query(
        "SELECT d.id AS deployment_id,
                d.revision_id,
                dt.node_id,
                dt.status AS target_status,
                dts.wave,
                dts.desired_structure,
                dts.desired_grants,
                dts.dispatched_grants
         FROM deployments d
         JOIN deployment_targets dt ON dt.deployment_id = d.id
         JOIN deployment_target_state dts
           ON dts.deployment_id = dt.deployment_id
          AND dts.node_id = dt.node_id
         JOIN node_lifecycle_state lifecycle
           ON lifecycle.node_id = dt.node_id
          AND lifecycle.lifecycle_epoch = dts.lifecycle_epoch
          AND lifecycle.phase IN ('active', 'retiring')
         WHERE d.kind = 'config'
           AND d.active = TRUE
           AND d.status IN ('planned', 'running', 'halted')
           AND dt.status IN (
               'pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty'
           )
         ORDER BY dt.node_id
         FOR UPDATE OF dt, dts",
    )
    .fetch_all(&mut *tx)
    .await?;

    // An isolated configuration target is no longer active deployment work, but an unclaimed
    // obligation has the same rebase requirement. Once claimed, its bytes are immutable and the
    // permission change must instead follow as a grants obligation.
    let config_obligations = sqlx::query(
        "SELECT obligation.node_id,
                obligation.lifecycle_epoch,
                obligation.generation,
                obligation.source_deployment_id,
                obligation.source_revision_id,
                obligation.status,
                obligation.claim_generation,
                obligation.desired_structure,
                obligation.desired_grants
           FROM node_convergence_obligations obligation
           JOIN node_operational_isolations isolation
             ON isolation.node_id = obligation.node_id
           JOIN node_lifecycle_state lifecycle
             ON lifecycle.node_id = obligation.node_id
            AND lifecycle.lifecycle_epoch = obligation.lifecycle_epoch
            AND lifecycle.phase = 'active'
          WHERE obligation.kind = 'config'
            AND obligation.status IN (
                'pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty'
            )
          ORDER BY obligation.node_id
          FOR UPDATE OF obligation",
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut revisions = running_revision_by_node
        .values()
        .copied()
        .collect::<BTreeSet<_>>();
    for row in &config_rows {
        revisions.insert(revision_to_u64(row.try_get("revision_id")?)?);
    }
    for row in &config_obligations {
        revisions.insert(revision_to_u64(row.try_get("source_revision_id")?)?);
    }

    // Compile each distinct topology only once. The permission projection retains that revision's
    // nodes, listeners and Flow, replacing only users and grant relations with the latest ones.
    let mut projected_by_revision = BTreeMap::new();
    let mut snapshots_by_revision = BTreeMap::new();
    for base_revision in revisions {
        let base = if base_revision == revision_id {
            latest.clone()
        } else {
            materialize::load_snapshot_tx(&mut tx, Some(base_revision)).await?
        };
        let projected = crate::serving::permission_projection(base, &latest);
        projected_by_revision.insert(base_revision, projected_grants(projected.clone())?);
        snapshots_by_revision.insert(base_revision, projected);
    }

    let mut desired_now = BTreeMap::<String, DesiredGrants>::new();
    for (node_id, base_revision) in &running_revision_by_node {
        let Some(desired) = projected_by_revision
            .get(base_revision)
            .and_then(|by_node| by_node.get(node_id))
            .cloned()
        else {
            continue;
        };
        let Some(observed) = applied_by_node.get(node_id).map(|state| &state.grants) else {
            continue;
        };
        if !grants_match(&desired, observed) {
            desired_now.insert(node_id.clone(), desired);
        }
    }

    for row in config_obligations {
        let node_id: String = row.try_get("node_id")?;
        let config_revision = revision_to_u64(row.try_get("source_revision_id")?)?;
        let future_grants = projected_by_revision
            .get(&config_revision)
            .and_then(|by_node| by_node.get(&node_id))
            .cloned()
            .ok_or_else(|| {
                StoreError::InvalidData(format!(
                    "isolated config obligation for {node_id} has no permission projection at revision {config_revision}"
                ))
            })?;

        let current_value: Value = row.try_get("desired_grants")?;
        let future_value = serde_json::to_value(&future_grants)?;
        if current_value != future_value {
            // Keep a following grants generation even after rebasing. It is redundant when config
            // succeeds, but is required when config was already in flight or fails before
            // applying the newest runtime client list.
            desired_now.insert(node_id.clone(), future_grants.clone());
        }

        let status: String = row.try_get("status")?;
        let claim_generation: i64 = row.try_get("claim_generation")?;
        if status != "pending" || claim_generation != 0 {
            continue;
        }

        let mut structure: Value = row.try_get("desired_structure")?;
        structure
            .as_object_mut()
            .ok_or_else(|| {
                StoreError::InvalidData(format!(
                    "isolated config obligation for {node_id} has a non-object desired structure"
                ))
            })?
            .insert("grants_revision".to_owned(), json!(revision_id));
        let desired_fingerprint = sha256_hex(&serde_json::to_vec(&json!({
            "structure": &structure,
            "grants": &future_value,
        }))?);
        let source_deployment_id: i64 = row.try_get("source_deployment_id")?;
        let lifecycle_epoch: i64 = row.try_get("lifecycle_epoch")?;
        let generation: i64 = row.try_get("generation")?;
        let updated = sqlx::query(
            "UPDATE node_convergence_obligations
                SET desired_structure = $5,
                    desired_grants = $6,
                    desired_fingerprint = $7
              WHERE node_id = $1
                AND kind = 'config'
                AND lifecycle_epoch = $2
                AND generation = $3
                AND source_deployment_id = $4
                AND status = 'pending'
                AND claim_generation = 0",
        )
        .bind(&node_id)
        .bind(lifecycle_epoch)
        .bind(generation)
        .bind(source_deployment_id)
        .bind(&structure)
        .bind(&future_value)
        .bind(desired_fingerprint)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::Conflict(format!(
                "isolated config obligation for {node_id} was claimed while permissions were rebased"
            )));
        }
        sqlx::query(
            "UPDATE deployment_target_state
                SET desired_grants = $3,
                    desired_structure = jsonb_set(
                        desired_structure,
                        '{grants_revision}',
                        to_jsonb($4::bigint),
                        TRUE
                    )
              WHERE deployment_id = $1 AND node_id = $2",
        )
        .bind(source_deployment_id)
        .bind(&node_id)
        .bind(future_value)
        .bind(revision_to_i64(revision_id)?)
        .execute(&mut *tx)
        .await?;
        let usage_generation_id = rebase_pending_config_usage_generation_tx(
            &mut tx,
            source_deployment_id,
            &node_id,
            0,
            structure,
            future_grants,
            snapshots_by_revision
                .get(&config_revision)
                .expect("every projected revision retains its source snapshot"),
        )
        .await?;
        if let Some(usage_generation_id) = usage_generation_id {
            sqlx::query(
                "UPDATE node_convergence_obligations
                    SET usage_generation_id = $5
                  WHERE node_id = $1
                    AND kind = 'config'
                    AND lifecycle_epoch = $2
                    AND generation = $3
                    AND source_deployment_id = $4",
            )
            .bind(&node_id)
            .bind(lifecycle_epoch)
            .bind(generation)
            .bind(source_deployment_id)
            .bind(usage_generation_id)
            .execute(&mut *tx)
            .await?;
        }
    }

    let mut deferred = BTreeSet::new();
    for row in config_rows {
        let deployment_id: i64 = row.try_get("deployment_id")?;
        let node_id: String = row.try_get("node_id")?;
        let target_status: String = row.try_get("target_status")?;
        let structure: Value = row.try_get("desired_structure")?;
        if !config_manages_xray(&structure) {
            // A WireGuard/phantun-only target cannot overwrite the runtime xray list.
            continue;
        }

        let config_revision = revision_to_u64(row.try_get("revision_id")?)?;
        let Some(future_grants) = projected_by_revision
            .get(&config_revision)
            .and_then(|by_node| by_node.get(&node_id))
            .cloned()
        else {
            deferred.insert(node_id);
            continue;
        };

        let in_flight = matches!(target_status.as_str(), "dispatched" | "converging")
            || row
                .try_get::<Option<Value>, _>("dispatched_grants")?
                .is_some();
        if in_flight {
            // Nothing already handed to an agent may be rewritten. Even if it happens to carry
            // the newest list, the agent is changing xray now and a second list writer must not
            // race it; the queued job will observe its result on the next round.
            let frozen = row
                .try_get::<Option<Value>, _>("dispatched_grants")?
                .or(row.try_get::<Option<Value>, _>("desired_grants")?);
            if desired_now.contains_key(&node_id)
                || frozen.as_ref() != Some(&serde_json::to_value(&future_grants)?)
            {
                deferred.insert(node_id);
            }
            continue;
        }

        let future_value = serde_json::to_value(&future_grants)?;
        let mut rebased_structure = structure;
        rebased_structure
            .as_object_mut()
            .ok_or_else(|| {
                StoreError::InvalidData(format!(
                    "config target {deployment_id}/{node_id} has a non-object desired structure"
                ))
            })?
            .insert("grants_revision".to_owned(), json!(revision_id));
        sqlx::query(
            "UPDATE deployment_target_state
             SET desired_grants = $3,
                 desired_structure = $4
             WHERE deployment_id = $1
               AND node_id = $2
               AND dispatched_grants IS NULL",
        )
        .bind(deployment_id)
        .bind(&node_id)
        .bind(&future_value)
        .bind(&rebased_structure)
        .execute(&mut *tx)
        .await?;
        rebase_pending_config_usage_generation_tx(
            &mut tx,
            deployment_id,
            &node_id,
            row.try_get("wave")?,
            rebased_structure,
            future_grants,
            snapshots_by_revision
                .get(&config_revision)
                .expect("every projected revision retains its source snapshot"),
        )
        .await?;
    }

    let desired = desired_now
        .into_iter()
        .filter(|(node_id, _)| !deferred.contains(node_id))
        .map(|(node_id, grants)| {
            (
                node_id,
                NodeDesiredState {
                    phantun: unmanaged_for_grants("权限单不碰 phantun"),
                    hy2_port_hop: unmanaged_for_grants("权限单不碰端口跳转"),
                    wireguard: unmanaged_for_grants("权限单不碰 wireguard"),
                    xray: unmanaged_for_grants("权限单不重启 xray"),
                    grants,
                },
            )
        })
        .collect::<Vec<_>>();
    let mut plan = narrow_to_kind(
        plan_desired_deployment(revision_id, desired, &applied, Vec::new()),
        DeploymentKind::Grants,
    );
    let isolated = isolated_node_ids(&mut *tx).await?;
    mark_isolated_targets(&mut plan, &isolated);
    plan.base_revision_id = last_succeeded_revision(&mut *tx, DeploymentKind::Grants).await?;

    if plan.summary.changed_targets == 0 {
        tx.commit().await?;
        return Ok(AutomaticGrantsDeploymentResult {
            deployment_id: None,
            deferred: deferred.into_iter().collect(),
        });
    }

    let fingerprint = automatic_grants_fingerprint(&plan)?;
    let idempotency_key = format!("grants:auto:{revision_id}:{fingerprint}");
    if let Some(existing) = sqlx::query("SELECT id FROM deployments WHERE idempotency_key = $1")
        .bind(&idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
    {
        let deployment_id = existing.try_get("id")?;
        tx.commit().await?;
        return Ok(AutomaticGrantsDeploymentResult {
            deployment_id: Some(deployment_id),
            deferred: deferred.into_iter().collect(),
        });
    }

    if let Some(active) =
        sqlx::query("SELECT id FROM deployments WHERE active = TRUE AND kind = 'grants' LIMIT 1")
            .fetch_optional(&mut *tx)
            .await?
    {
        return Err(StoreError::Unsupported(format!(
            "another grants deployment is active: {}",
            active.try_get::<i64, _>("id")?
        )));
    }

    let row = sqlx::query(
        "INSERT INTO deployments (
            revision_id, status, actor, idempotency_key, active, warnings, note, kind,
            base_revision_id
         ) VALUES ($1, 'planned', $2, $3, TRUE, $4, $5, 'grants', $6)
         RETURNING id",
    )
    .bind(revision_to_i64(revision_id)?)
    .bind(actor.operator_id())
    .bind(&idempotency_key)
    .bind(serde_json::to_value(&plan.warnings)?)
    .bind(note)
    .bind(plan.base_revision_id)
    .fetch_one(&mut *tx)
    .await?;
    let deployment_id = row.try_get("id")?;
    for target in &plan.targets {
        insert_target(&mut tx, deployment_id, target).await?;
    }
    if !plan
        .targets
        .iter()
        .any(|target| target.status == PlannedTargetStatus::Pending)
    {
        refresh_deployment_status(&mut tx, deployment_id, "deferred").await?;
    }

    tx.commit().await?;
    Ok(AutomaticGrantsDeploymentResult {
        deployment_id: Some(deployment_id),
        deferred: deferred.into_iter().collect(),
    })
}

fn projected_grants(snapshot: ModelSnapshot) -> Result<BTreeMap<String, DesiredGrants>> {
    Ok(plan_snapshot_deployment(&snapshot, &[])
        .map_err(plan_error)?
        .targets
        .into_iter()
        .map(|target| (target.node_id, target.desired.grants))
        .collect())
}

fn config_manages_xray(structure: &Value) -> bool {
    structure
        .get("actions")
        .and_then(Value::as_array)
        .is_some_and(|actions| {
            actions
                .iter()
                .any(|action| matches!(action.as_str(), Some("apply-xray" | "disable-xray")))
        })
}

async fn rebase_pending_config_usage_generation_tx(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
    node_id: &str,
    wave: i32,
    desired_structure: Value,
    grants: DesiredGrants,
    topology: &ModelSnapshot,
) -> Result<Option<i64>> {
    let desired = desired_deployment_from_structure_tx(
        tx,
        deployment_id,
        node_id.to_owned(),
        wave,
        desired_structure,
        grants,
    )
    .await?;
    let usage_generation_id = crate::usage::create_usage_generation_for_target(
        tx,
        deployment_id,
        node_id,
        topology,
        &desired.desired,
        &desired.actions,
    )
    .await?;
    if let Some(usage_generation_id) = usage_generation_id {
        sqlx::query(
            "UPDATE deployment_target_state
                SET usage_generation_id = $3
              WHERE deployment_id = $1 AND node_id = $2",
        )
        .bind(deployment_id)
        .bind(node_id)
        .bind(usage_generation_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(usage_generation_id)
}

fn unmanaged_for_grants(reason: &str) -> DesiredArtifact {
    DesiredArtifact::Unmanaged {
        reason: reason.to_owned(),
    }
}

fn automatic_grants_fingerprint(plan: &DeploymentPlan) -> Result<String> {
    let bytes = serde_json::to_vec(&plan.targets)?;
    Ok(sha256_hex(&bytes))
}

/// Write a sentence for a deployment when nobody wrote a note.
///
/// This used to be the caller's `console · revision 71` — repeating a number already displayed
/// to its right, so that every row of the release list read identically while the list is
/// precisely what distinguishes one deployment from another by its note. The console then had to
/// write a regex to recognize its own sentence and treat it as no note at all, with both ends
/// working around each other.
///
/// It now says two things, both the deployment's own:
///
/// - What changed — taken from the revision's note. Assembled from the operations at draft
///   commit (`apply_ops` in draft.rs), it is already the sentence describing what this run
///   moved. The leading "committed:" is the draft layer's phrasing, and repeating it here only
///   takes up room, so it is stripped.
/// - Who it goes to — the number of machines with artifact changes. A release is in essence
///   delivering artifacts to machines, and the count is both this deployment's scale and what
///   somebody decides on when choosing whether to push now.
///
/// Where neither is available it falls back to the machine count — still more useful than a
/// revision number.
async fn default_note(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
    plan: &DeploymentPlan,
) -> Result<String> {
    let what = sqlx::query("SELECT note FROM revisions WHERE id = $1")
        .bind(revision_to_i64(revision_id)?)
        .fetch_optional(&mut **tx)
        .await?
        .and_then(|row| row.try_get::<Option<String>, _>("note").ok().flatten())
        .map(|note| strip_commit_prefix(&note).to_owned())
        .filter(|note| !note.is_empty());

    let scale = format!("{} 台", plan.summary.changed_targets);
    Ok(match what {
        Some(what) => format!("{what} · {scale}"),
        None => scale,
    })
}

/// Strip the draft layer's phrasing from the front of a revision note: `提交：` or
/// `提交 3 处改动：`. An unrecognized note is returned unchanged — people can write their own,
/// and guessing wrong is worse than leaving it alone.
fn strip_commit_prefix(note: &str) -> &str {
    let note = note.trim();
    let Some(rest) = note.strip_prefix("提交") else {
        return note;
    };
    match rest.split_once('：') {
        // Both `提交：` and `提交 N 处改动：` are taken; anything else (someone's own wording)
        // is left alone
        Some((mid, tail)) if mid.is_empty() || mid.ends_with("处改动") => tail.trim(),
        _ => note,
    }
}

pub async fn create_rollback_deployment(
    pool: &PgPool,
    actor: &AdminContext,
    request: CreateRollbackRequest,
) -> Result<CreateDeploymentResult> {
    if request.idempotency_key.trim().is_empty() {
        return Err(StoreError::InvalidData(
            "rollback idempotency_key must not be empty".to_owned(),
        ));
    }

    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "rollback must run as system-admin because it targets all nodes".to_owned(),
        ));
    }

    let mut tx = pool.begin().await?;

    if let Some(existing) = sqlx::query(
        "SELECT id, revision_id, status, rollback_of_deployment_id
         FROM deployments
         WHERE idempotency_key = $1",
    )
    .bind(&request.idempotency_key)
    .fetch_optional(&mut *tx)
    .await?
    {
        let rollback_of = existing.try_get::<Option<i64>, _>("rollback_of_deployment_id")?;
        if rollback_of != Some(request.target_deployment_id) {
            return Err(StoreError::InvalidData(format!(
                "idempotency_key {} already belongs to a different rollback target",
                request.idempotency_key
            )));
        }
        let deployment_id = existing.try_get("id")?;
        let status = existing.try_get("status")?;
        let revision_id = revision_to_u64(existing.try_get("revision_id")?)?;
        let snapshot = materialize::load_snapshot_tx(&mut tx, Some(revision_id)).await?;
        let applied = load_applied_states(&mut *tx).await?;
        let mut plan = plan_forced_snapshot_deployment(&snapshot, &applied).map_err(plan_error)?;
        let terminal = terminal_lifecycle_nodes(&mut *tx).await?;
        plan = without_terminal_lifecycle_targets(plan, &terminal);
        let isolated = isolated_node_ids(&mut *tx).await?;
        mark_isolated_targets(&mut plan, &isolated);
        tx.commit().await?;
        return Ok(CreateDeploymentResult {
            deployment_id,
            status,
            reused: true,
            plan,
        });
    }

    let target_snapshot = rollback_target_snapshot(&mut tx, request.target_deployment_id).await?;
    let target_revision = target_snapshot.revision;
    let previous_revision = console::lock_control_state(&mut tx).await?;
    let uncertain_nodes = cancel_active_deployment_for_rollback(&mut tx).await?;
    let actor_id = request
        .actor
        .as_deref()
        .unwrap_or_else(|| actor.operator_id())
        .to_owned();
    let note = request.note.unwrap_or_else(|| {
        format!(
            "rollback current model to deployment #{} (revision {})",
            request.target_deployment_id, target_revision
        )
    });
    let rollback_revision = console::insert_revision(&mut tx, &actor_id, &note).await?;
    restore_model_snapshot_tx(&mut tx, rollback_revision, &target_snapshot).await?;
    let rollback_revision =
        console::commit_revision(&mut tx, rollback_revision, previous_revision, true).await?;
    let restored_snapshot = materialize::load_current_snapshot_tx(&mut tx).await?;
    ensure_restored_snapshot(
        "rollback",
        &target_snapshot,
        &restored_snapshot,
        rollback_revision,
    )?;

    let mut applied = load_applied_states(&mut *tx).await?;
    if !uncertain_nodes.is_empty() {
        mark_applied_states_dirty_with_reason(
            &mut applied,
            &uncertain_nodes,
            format!(
                "rollback to deployment {} canceled a deployment target while it was in flight",
                request.target_deployment_id
            ),
        );
    }
    let mut plan =
        plan_forced_snapshot_deployment(&restored_snapshot, &applied).map_err(plan_error)?;
    let terminal = terminal_lifecycle_nodes(&mut *tx).await?;
    plan = without_terminal_lifecycle_targets(plan, &terminal);
    let isolated = isolated_node_ids(&mut *tx).await?;
    mark_isolated_targets(&mut plan, &isolated);
    if plan.summary.total_targets == 0 {
        return Err(StoreError::InvalidData(
            "cannot create rollback deployment for an empty target set".to_owned(),
        ));
    }
    let (deployment_id, status) = insert_rollback_deployment_tx(
        &mut tx,
        &actor_id,
        &request.idempotency_key,
        &note,
        request.target_deployment_id,
        &plan,
    )
    .await?;

    tx.commit().await?;
    Ok(CreateDeploymentResult {
        deployment_id,
        status,
        reused: false,
        plan,
    })
}

// `kind` as None means both. Viewing them apart has a reason: quota enforcement can ship dozens
// of grants deployments a day, and mixed into one stream they drown out when the configuration
// last changed — the most common question people bring to a release history. The filtering is
// server-side rather than in the UI: filtered in the UI, the limit fills with the high-frequency
// grants deployments first and not one configuration deployment surfaces.
pub async fn list_deployments(
    pool: &PgPool,
    actor: &AdminContext,
    limit: u32,
    kind: Option<DeploymentKind>,
) -> Result<DeploymentList> {
    let limit = i64::from(limit.clamp(1, 200));
    let kind = kind.map(DeploymentKind::as_str);
    let rows = if actor.is_system_admin() {
        sqlx::query(
            "SELECT d.id,
                    d.revision_id,
                    d.status,
                    d.activation_status,
                    d.settlement_status,
                    d.activated_at::text AS activated_at,
                    d.active,
                    d.actor,
                    d.kind,
                    d.note,
                    d.base_revision_id,
                    d.rollback_of_deployment_id,
                    d.sync_of_deployment_id,
                    d.created_at::text AS created_at,
                    d.started_at::text AS started_at,
                    d.finished_at::text AS finished_at,
                    count(dt.node_id) AS total_targets,
                    count(dt.node_id) FILTER (WHERE dt.status <> 'skipped') AS changed_targets,
                    count(dt.node_id) FILTER (WHERE dt.status = 'skipped') AS skipped_targets,
                    count(dt.node_id) FILTER (WHERE dt.status IN ('failed-recovered', 'failed-dirty')) AS failed_targets,
                    (SELECT count(*)
                       FROM node_convergence_obligations o
                      WHERE o.source_deployment_id = d.id
                        AND o.status IN ('pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty')) AS debt_targets,
                    count(dt.node_id) FILTER (WHERE dts.disruptive) AS disruptive_targets,
                    COALESCE(max(dts.wave), 0) AS max_wave,
                    -- 这一单是不是「在等人确认」而不是「在等机器」。两者在列表上长得
                    -- 一模一样时，人只知道它 running，不知道自己是那个卡住它的原因。
                    -- 判据跟 dispatch 里那道波闸一模一样（wave_gate）：当前波里有破坏性
                    -- 目标，且这一波是第二波往后、或者动作里带着 disable-*，而这一波还
                    -- 没有确认记录。抄一份而不是共用，是因为那边是逐台取任务、这边是
                    -- 整单汇总——两个形状，同一条规矩。
                    COALESCE((
                      WITH cur AS (
                        SELECT min(dts2.wave) AS wave
                        FROM deployment_targets dt2
                        JOIN deployment_target_state dts2
                          ON dts2.deployment_id = dt2.deployment_id
                         AND dts2.node_id = dt2.node_id
                        WHERE dt2.deployment_id = d.id
                          AND dt2.status IN ('pending', 'dispatched', 'converging')
                      )
                      SELECT bool_or(
                               dts2.disruptive
                               AND (
                                 dts2.wave > 1
                                 OR dts2.desired_structure->'actions' ?| array['disable-xray', 'disable-wire-guard']
                               )
                             )
                             AND NOT EXISTS (
                               SELECT 1
                               FROM deployment_wave_confirmations c
                               WHERE c.deployment_id = d.id
                                 AND c.wave = (SELECT wave FROM cur)
                             )
                      FROM deployment_targets dt2
                      JOIN deployment_target_state dts2
                        ON dts2.deployment_id = dt2.deployment_id
                       AND dts2.node_id = dt2.node_id
                      WHERE dt2.deployment_id = d.id
                        AND dt2.status IN ('pending', 'dispatched', 'converging')
                        AND dts2.wave = (SELECT wave FROM cur)
                    ), FALSE) AS awaiting_confirmation
             FROM deployments d
             LEFT JOIN deployment_targets dt
               ON dt.deployment_id = d.id
             LEFT JOIN deployment_target_state dts
               ON dts.deployment_id = dt.deployment_id
              AND dts.node_id = dt.node_id
             WHERE $2::text IS NULL OR d.kind = $2
             GROUP BY d.id
             ORDER BY d.id DESC
             LIMIT $1",
        )
        .bind(limit)
        .bind(kind)
        .fetch_all(pool)
        .await?
    } else {
        let tenant_scope = require_actor_tenant_scope(actor)?;
        let tenant_pattern = actor
            .tenant_scope_like_pattern()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))?;
        sqlx::query(
            "SELECT d.id,
                    d.revision_id,
                    d.status,
                    d.activation_status,
                    d.settlement_status,
                    d.activated_at::text AS activated_at,
                    d.active,
                    d.actor,
                    d.kind,
                    d.note,
                    d.base_revision_id,
                    d.rollback_of_deployment_id,
                    d.sync_of_deployment_id,
                    d.created_at::text AS created_at,
                    d.started_at::text AS started_at,
                    d.finished_at::text AS finished_at,
                    count(dt.node_id) AS total_targets,
                    count(dt.node_id) FILTER (WHERE dt.status <> 'skipped') AS changed_targets,
                    count(dt.node_id) FILTER (WHERE dt.status = 'skipped') AS skipped_targets,
                    count(dt.node_id) FILTER (WHERE dt.status IN ('failed-recovered', 'failed-dirty')) AS failed_targets,
                    (SELECT count(*)
                       FROM node_convergence_obligations o
                      WHERE o.source_deployment_id = d.id
                        AND o.status IN ('pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty')) AS debt_targets,
                    count(dt.node_id) FILTER (WHERE dts.disruptive) AS disruptive_targets,
                    COALESCE(max(dts.wave), 0) AS max_wave,
                    -- 这一单是不是「在等人确认」而不是「在等机器」。两者在列表上长得
                    -- 一模一样时，人只知道它 running，不知道自己是那个卡住它的原因。
                    -- 判据跟 dispatch 里那道波闸一模一样（wave_gate）：当前波里有破坏性
                    -- 目标，且这一波是第二波往后、或者动作里带着 disable-*，而这一波还
                    -- 没有确认记录。抄一份而不是共用，是因为那边是逐台取任务、这边是
                    -- 整单汇总——两个形状，同一条规矩。
                    COALESCE((
                      WITH cur AS (
                        SELECT min(dts2.wave) AS wave
                        FROM deployment_targets dt2
                        JOIN deployment_target_state dts2
                          ON dts2.deployment_id = dt2.deployment_id
                         AND dts2.node_id = dt2.node_id
                        WHERE dt2.deployment_id = d.id
                          AND dt2.status IN ('pending', 'dispatched', 'converging')
                      )
                      SELECT bool_or(
                               dts2.disruptive
                               AND (
                                 dts2.wave > 1
                                 OR dts2.desired_structure->'actions' ?| array['disable-xray', 'disable-wire-guard']
                               )
                             )
                             AND NOT EXISTS (
                               SELECT 1
                               FROM deployment_wave_confirmations c
                               WHERE c.deployment_id = d.id
                                 AND c.wave = (SELECT wave FROM cur)
                             )
                      FROM deployment_targets dt2
                      JOIN deployment_target_state dts2
                        ON dts2.deployment_id = dt2.deployment_id
                       AND dts2.node_id = dt2.node_id
                      WHERE dt2.deployment_id = d.id
                        AND dt2.status IN ('pending', 'dispatched', 'converging')
                        AND dts2.wave = (SELECT wave FROM cur)
                    ), FALSE) AS awaiting_confirmation
             FROM deployments d
             JOIN deployment_targets dt
               ON dt.deployment_id = d.id
             JOIN nodes n
               ON n.id = dt.node_id
             LEFT JOIN deployment_target_state dts
               ON dts.deployment_id = dt.deployment_id
              AND dts.node_id = dt.node_id
             WHERE (n.tenant_id = $2 OR n.tenant_id LIKE $3 ESCAPE '\\')
               AND ($4::text IS NULL OR d.kind = $4)
             GROUP BY d.id
             ORDER BY d.id DESC
             LIMIT $1",
        )
        .bind(limit)
        .bind(tenant_scope)
        .bind(tenant_pattern)
        .bind(kind)
        .fetch_all(pool)
        .await?
    };

    let deployments = rows
        .iter()
        .map(|row| {
            Ok(DeploymentListItem {
                id: row.try_get("id")?,
                revision_id: revision_to_u64(row.try_get("revision_id")?)?,
                status: row.try_get("status")?,
                activation_status: row.try_get("activation_status")?,
                settlement_status: row.try_get("settlement_status")?,
                activated_at: row.try_get("activated_at")?,
                active: row.try_get("active")?,
                actor: row.try_get("actor")?,
                kind: DeploymentKind::parse(row.try_get("kind")?).unwrap_or_default(),
                note: row.try_get("note")?,
                base_revision_id: row
                    .try_get::<Option<i64>, _>("base_revision_id")?
                    .map(revision_to_u64)
                    .transpose()?,
                rollback_of_deployment_id: row.try_get("rollback_of_deployment_id")?,
                sync_of_deployment_id: row.try_get("sync_of_deployment_id")?,
                created_at: row.try_get("created_at")?,
                started_at: row.try_get("started_at")?,
                finished_at: row.try_get("finished_at")?,
                total_targets: i64_to_u64("total_targets", row.try_get("total_targets")?)?,
                changed_targets: i64_to_u64("changed_targets", row.try_get("changed_targets")?)?,
                skipped_targets: i64_to_u64("skipped_targets", row.try_get("skipped_targets")?)?,
                failed_targets: i64_to_u64("failed_targets", row.try_get("failed_targets")?)?,
                debt_targets: i64_to_u64("debt_targets", row.try_get("debt_targets")?)?,
                disruptive_targets: i64_to_u64(
                    "disruptive_targets",
                    row.try_get("disruptive_targets")?,
                )?,
                max_wave: u32::try_from(row.try_get::<i32, _>("max_wave")?).map_err(|_| {
                    StoreError::InvalidData("deployment max_wave is out of range".to_owned())
                })?,
                awaiting_confirmation: row.try_get("awaiting_confirmation")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(DeploymentList { deployments })
}

pub async fn deployment_detail(
    pool: &PgPool,
    actor: &AdminContext,
    deployment_id: i64,
    include_content: bool,
) -> Result<DeploymentDetail> {
    let deployment = sqlx::query(
        "SELECT id, revision_id, status, activation_status, settlement_status,
                activated_at::text AS activated_at, active, actor, note, warnings, base_revision_id,
                created_at::text AS created_at,
                started_at::text AS started_at,
                halted_at::text AS halted_at,
                finished_at::text AS finished_at,
                rollback_of_deployment_id,
                sync_of_deployment_id,
                divergence_cleared_at::text AS divergence_cleared_at,
                (SELECT count(*)
                   FROM node_convergence_obligations o
                  WHERE o.source_deployment_id = deployments.id
                    AND o.status IN ('pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty')) AS debt_targets
         FROM deployments
         WHERE id = $1",
    )
    .bind(deployment_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("deployment {deployment_id}")))?;

    let rows = if actor.is_system_admin() {
        sqlx::query(
            "SELECT dt.node_id,
                    dt.status,
                    dt.error,
                    dts.wave,
                    dts.disruptive,
                    dts.desired_structure,
                    dts.observed_before,
                    dts.observed_after,
                    dts.verdict,
                    dts.dispatched_at::text AS dispatched_at
             FROM deployment_targets dt
             JOIN deployment_target_state dts
               ON dts.deployment_id = dt.deployment_id
              AND dts.node_id = dt.node_id
             WHERE dt.deployment_id = $1
             ORDER BY dts.wave, dt.node_id",
        )
        .bind(deployment_id)
        .fetch_all(pool)
        .await?
    } else {
        let tenant_scope = require_actor_tenant_scope(actor)?;
        let tenant_pattern = actor
            .tenant_scope_like_pattern()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))?;
        sqlx::query(
            "SELECT dt.node_id,
                    dt.status,
                    dt.error,
                    dts.wave,
                    dts.disruptive,
                    dts.desired_structure,
                    dts.observed_before,
                    dts.observed_after,
                    dts.verdict,
                    dts.dispatched_at::text AS dispatched_at
             FROM deployment_targets dt
             JOIN deployment_target_state dts
               ON dts.deployment_id = dt.deployment_id
              AND dts.node_id = dt.node_id
             JOIN nodes n
               ON n.id = dt.node_id
             WHERE dt.deployment_id = $1
               AND (n.tenant_id = $2 OR n.tenant_id LIKE $3 ESCAPE '\\')
             ORDER BY dts.wave, dt.node_id",
        )
        .bind(deployment_id)
        .bind(tenant_scope)
        .bind(tenant_pattern)
        .fetch_all(pool)
        .await?
    };
    if rows.is_empty() && !actor.is_system_admin() {
        return Err(StoreError::NotFound(format!("deployment {deployment_id}")));
    }

    let mut targets = rows
        .iter()
        .map(|row| {
            Ok(DeploymentTargetDetail {
                node_id: row.try_get("node_id")?,
                status: row.try_get("status")?,
                error: row.try_get("error")?,
                wave: u32::try_from(row.try_get::<i32, _>("wave")?).map_err(|_| {
                    StoreError::InvalidData("deployment target wave is out of range".to_owned())
                })?,
                disruptive: row.try_get("disruptive")?,
                desired_structure: row.try_get("desired_structure")?,
                observed_before: row.try_get("observed_before")?,
                observed_after: row.try_get("observed_after")?,
                verdict: row.try_get("verdict")?,
                dispatched_at: row.try_get("dispatched_at")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    if include_content {
        attach_known_artifact_content(pool, &mut targets).await?;
    }

    Ok(DeploymentDetail {
        id: deployment.try_get("id")?,
        revision_id: revision_to_u64(deployment.try_get("revision_id")?)?,
        status: deployment.try_get("status")?,
        activation_status: deployment.try_get("activation_status")?,
        settlement_status: deployment.try_get("settlement_status")?,
        activated_at: deployment.try_get("activated_at")?,
        debt_targets: i64_to_u64("debt_targets", deployment.try_get("debt_targets")?)?,
        active: deployment.try_get("active")?,
        actor: deployment.try_get("actor")?,
        note: deployment.try_get("note")?,
        base_revision_id: deployment
            .try_get::<Option<i64>, _>("base_revision_id")?
            .map(revision_to_u64)
            .transpose()?,
        warnings: deployment.try_get("warnings")?,
        created_at: deployment.try_get("created_at")?,
        started_at: deployment.try_get("started_at")?,
        halted_at: deployment.try_get("halted_at")?,
        finished_at: deployment.try_get("finished_at")?,
        rollback_of_deployment_id: deployment.try_get("rollback_of_deployment_id")?,
        sync_of_deployment_id: deployment.try_get("sync_of_deployment_id")?,
        divergence_cleared_at: deployment.try_get("divergence_cleared_at")?,
        targets,
    })
}

pub async fn confirm_deployment_wave(
    pool: &PgPool,
    admin: &AdminContext,
    deployment_id: i64,
    wave: u32,
    actor: Option<String>,
) -> Result<DeploymentWaveConfirmationResult> {
    ensure_deployment_write_access(pool, admin, deployment_id).await?;
    let wave_i32 = i32::try_from(wave)
        .map_err(|_| StoreError::InvalidData(format!("deployment wave out of range: {wave}")))?;
    let mut tx = pool.begin().await?;

    let deployment = sqlx::query(
        "SELECT status, active
         FROM deployments
         WHERE id = $1
         FOR UPDATE",
    )
    .bind(deployment_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("deployment {deployment_id}")))?;
    let status: String = deployment.try_get("status")?;
    if !matches!(status.as_str(), "planned" | "running") {
        return Err(StoreError::Unsupported(format!(
            "deployment {deployment_id} cannot confirm waves from status {status}"
        )));
    }
    if deployment.try_get::<Option<bool>, _>("active")? != Some(true) {
        return Err(StoreError::Unsupported(format!(
            "deployment {deployment_id} is not active"
        )));
    }

    // The action names must match PlannedAction's serde spelling verbatim: DisableWireGuard
    // serializes to `disable-wire-guard` (WireGuard splits into two segments). Written as
    // `disable-wireguard` this entry never matches, and the consequence is that disabling
    // WireGuard — the most destructive action there is — skips confirmation in the first
    // wave.
    let row = sqlx::query(
        "WITH current_wave AS (
            SELECT MIN(dts.wave) AS wave
            FROM deployment_targets dt
            JOIN deployment_target_state dts
              ON dts.deployment_id = dt.deployment_id
             AND dts.node_id = dt.node_id
            WHERE dt.deployment_id = $1
              AND dt.status IN ('pending', 'dispatched', 'converging')
         )
         SELECT count(*) AS target_count,
                COALESCE(bool_or(
                  dts.disruptive
                  AND (
                    dts.wave > 1
                    OR dts.desired_structure->'actions' ?| array['disable-xray', 'disable-wire-guard']
                  )
                ), FALSE) AS requires_confirmation
         FROM current_wave cw
         JOIN deployment_targets dt
           ON dt.deployment_id = $1
         JOIN deployment_target_state dts
           ON dts.deployment_id = dt.deployment_id
          AND dts.node_id = dt.node_id
         WHERE cw.wave = $2
           AND dts.wave = cw.wave
           AND dt.status IN ('pending', 'dispatched', 'converging')",
    )
    .bind(deployment_id)
    .bind(wave_i32)
    .fetch_one(&mut *tx)
    .await?;
    let target_count: i64 = row.try_get("target_count")?;
    if target_count == 0 {
        return Err(StoreError::Unsupported(format!(
            "deployment {deployment_id} wave {wave} is not the current open wave"
        )));
    }
    let requires_confirmation: bool = row.try_get("requires_confirmation")?;
    if !requires_confirmation {
        return Err(StoreError::Unsupported(format!(
            "deployment {deployment_id} wave {wave} does not require confirmation"
        )));
    }

    let result = sqlx::query(
        "INSERT INTO deployment_wave_confirmations (deployment_id, wave, actor)
         VALUES ($1, $2, $3)
         ON CONFLICT (deployment_id, wave) DO NOTHING",
    )
    .bind(deployment_id)
    .bind(wave_i32)
    .bind(&actor)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(DeploymentWaveConfirmationResult {
        deployment_id,
        wave,
        confirmed: true,
        reused: result.rows_affected() == 0,
    })
}

pub async fn load_desired_for_node(
    pool: &PgPool,
    node_id: &str,
) -> Result<Option<NodeDesiredDeployment>> {
    if let Some(row) = sqlx::query(
        "SELECT obligation.source_deployment_id AS deployment_id,
                obligation.desired_structure,
                obligation.desired_grants,
                obligation.usage_generation_id,
                obligation.claim_generation
           FROM node_operational_isolations isolation
           JOIN node_lifecycle_state lifecycle
             ON lifecycle.node_id = isolation.node_id
            AND lifecycle.phase = 'active'
           JOIN node_convergence_obligations obligation
             ON obligation.node_id = isolation.node_id
            AND obligation.lifecycle_epoch = lifecycle.lifecycle_epoch
            AND obligation.status IN ('pending', 'dispatched', 'converging')
          WHERE isolation.node_id = $1
          ORDER BY CASE obligation.kind WHEN 'config' THEN 0 ELSE 1 END,
                   obligation.priority DESC,
                   obligation.generation DESC
          LIMIT 1",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await?
    {
        let deployment_id: i64 = row.try_get("deployment_id")?;
        let mut desired = desired_deployment_from_structure(
            pool,
            deployment_id,
            node_id.to_owned(),
            0,
            row.try_get("desired_structure")?,
            serde_json::from_value(row.try_get("desired_grants")?)?,
        )
        .await?;
        desired.claim_generation = i64_to_u64(
            "obligation claim_generation",
            row.try_get("claim_generation")?,
        )?;
        desired.usage_generation_id = row.try_get("usage_generation_id")?;
        return Ok(Some(desired));
    }
    if sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM node_operational_isolations isolation
               JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = isolation.node_id
              WHERE isolation.node_id = $1 AND lifecycle.phase = 'active'
         )",
    )
    .bind(node_id)
    .fetch_one(pool)
    .await?
    {
        return Ok(None);
    }
    let Some(row) = sqlx::query(
        // Both lines can be active at once. A configuration target already handed to the agent
        // keeps the machine until it reports; otherwise an automatic grants target goes first.
        // Its creator built it against the running inbound topology and rebased every pending
        // config target that could later overwrite it. Giving every merely-pending config
        // unconditional priority is what made permissions wait behind an arbitrarily long queue.
         "WITH active_deployment AS (
            SELECT d.id
            FROM deployments d
            JOIN deployment_targets dt ON dt.deployment_id = d.id
            JOIN deployment_target_state dts
              ON dts.deployment_id = dt.deployment_id
             AND dts.node_id = dt.node_id
            JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = dt.node_id
            WHERE d.active = TRUE
              AND d.status IN ('planned', 'running')
              AND dt.node_id = $1
              AND dt.status IN ('pending', 'dispatched', 'converging')
              AND dts.lifecycle_epoch = lifecycle.lifecycle_epoch
              AND lifecycle.phase IN ('active', 'retiring')
            ORDER BY CASE
                       WHEN d.kind = 'config'
                        AND dt.status IN ('dispatched', 'converging') THEN 0
                       WHEN d.kind = 'grants' THEN 1
                       ELSE 2
                     END,
                     d.id
            LIMIT 1
         ),
         current_wave AS (
            SELECT MIN(dts.wave) AS wave
            FROM active_deployment ad
            JOIN deployment_targets dt
              ON dt.deployment_id = ad.id
            JOIN deployment_target_state dts
              ON dts.deployment_id = dt.deployment_id
             AND dts.node_id = dt.node_id
            WHERE dt.status IN ('pending', 'dispatched', 'converging')
         ),
         wave_gate AS (
            SELECT cw.wave,
                   bool_or(
                     dts.disruptive
                     AND (
                       dts.wave > 1
                       OR dts.desired_structure->'actions' ?| array['disable-xray', 'disable-wire-guard']
                     )
                   ) AS requires_confirmation,
                   EXISTS (
                     SELECT 1
                     FROM deployment_wave_confirmations c
                     WHERE c.deployment_id = ad.id
                       AND c.wave = cw.wave
                   ) AS confirmed
            FROM active_deployment ad
            JOIN current_wave cw
              ON cw.wave IS NOT NULL
            JOIN deployment_targets dt
              ON dt.deployment_id = ad.id
            JOIN deployment_target_state dts
              ON dts.deployment_id = dt.deployment_id
             AND dts.node_id = dt.node_id
            WHERE dt.status IN ('pending', 'dispatched', 'converging')
              AND dts.wave = cw.wave
            GROUP BY ad.id, cw.wave
         )
         SELECT ad.id AS deployment_id,
                dt.node_id,
                dts.wave,
                dts.desired_structure,
                dts.desired_grants,
                d.revision_id
         FROM active_deployment ad
         JOIN deployments d
           ON d.id = ad.id
         JOIN current_wave cw
           ON TRUE
         JOIN wave_gate wg
           ON wg.wave = cw.wave
         JOIN deployment_targets dt
           ON dt.deployment_id = ad.id
         JOIN deployment_target_state dts
           ON dts.deployment_id = dt.deployment_id
          AND dts.node_id = dt.node_id
         JOIN node_lifecycle_state lifecycle
           ON lifecycle.node_id = dt.node_id
         WHERE dt.node_id = $1
           AND dt.status IN ('pending', 'dispatched', 'converging')
           AND dts.lifecycle_epoch = lifecycle.lifecycle_epoch
           AND lifecycle.phase IN ('active', 'retiring')
           AND dts.wave = cw.wave
           AND (NOT wg.requires_confirmation OR wg.confirmed)",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };

    let desired_structure = row.try_get("desired_structure")?;
    let grants = frozen_desired_grants(
        pool,
        row.try_get("desired_grants")?,
        revision_to_u64(row.try_get("revision_id")?)?,
        node_id,
    )
    .await?;
    desired_deployment_from_structure(
        pool,
        row.try_get("deployment_id")?,
        row.try_get("node_id")?,
        row.try_get("wave")?,
        desired_structure,
        grants,
    )
    .await
    .map(Some)
}

pub async fn claim_desired_for_node(
    pool: &PgPool,
    node_id: &str,
) -> Result<Option<NodeDesiredDeployment>> {
    let mut tx = pool.begin().await?;
    // `load_current_snapshot_tx` is several SELECTs. Repeatable Read makes them one model view;
    // using one connection prevents the old pool-deadlock where every claimant held a
    // transaction and waited for a second connection to materialize that view.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;
    let isolated = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM node_operational_isolations isolation
               JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = isolation.node_id
              WHERE isolation.node_id = $1 AND lifecycle.phase = 'active'
         )",
    )
    .bind(node_id)
    .fetch_one(&mut *tx)
    .await?;
    if isolated {
        let obligation = sqlx::query(
            "SELECT obligation.kind,
                    obligation.lifecycle_epoch,
                    obligation.generation,
                    obligation.source_deployment_id AS deployment_id,
                    obligation.desired_structure,
                    obligation.desired_grants,
                    obligation.usage_generation_id,
                    obligation.claim_generation
               FROM node_convergence_obligations obligation
               JOIN node_lifecycle_state lifecycle
                 ON lifecycle.node_id = obligation.node_id
                AND lifecycle.lifecycle_epoch = obligation.lifecycle_epoch
                AND lifecycle.phase = 'active'
              WHERE obligation.node_id = $1
                AND obligation.status IN ('pending', 'dispatched', 'converging')
                AND (
                    obligation.status = 'pending'
                    OR COALESCE(obligation.claimed_at, '-infinity'::timestamptz)
                         < now() - $2::interval
                )
                AND (
                    obligation.kind = 'config'
                    OR NOT EXISTS (
                        SELECT 1
                          FROM node_convergence_obligations config_obligation
                         WHERE config_obligation.node_id = obligation.node_id
                           AND config_obligation.lifecycle_epoch = obligation.lifecycle_epoch
                           AND config_obligation.kind = 'config'
                           AND config_obligation.status IN (
                               'pending', 'dispatched', 'converging',
                               'failed-recovered', 'failed-dirty'
                           )
                    )
                )
              ORDER BY CASE obligation.kind WHEN 'config' THEN 0 ELSE 1 END,
                       obligation.priority DESC,
                       obligation.generation DESC
              LIMIT 1
              FOR UPDATE OF obligation SKIP LOCKED",
        )
        .bind(node_id)
        .bind(DISPATCH_LEASE_INTERVAL)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(obligation) = obligation else {
            tx.commit().await?;
            return Ok(None);
        };
        let deployment_id: i64 = obligation.try_get("deployment_id")?;
        let lifecycle_epoch: i64 = obligation.try_get("lifecycle_epoch")?;
        let generation: i64 = obligation.try_get("generation")?;
        let kind: String = obligation.try_get("kind")?;
        let claim_generation = obligation
            .try_get::<i64, _>("claim_generation")?
            .checked_add(1)
            .ok_or_else(|| {
                StoreError::InvalidData(format!(
                    "obligation claim generation overflow for {node_id}/{kind}"
                ))
            })?;
        let mut desired = desired_deployment_from_structure_tx(
            &mut tx,
            deployment_id,
            node_id.to_owned(),
            0,
            obligation.try_get("desired_structure")?,
            serde_json::from_value(obligation.try_get("desired_grants")?)?,
        )
        .await?;
        desired.claim_generation = i64_to_u64("obligation claim_generation", claim_generation)?;
        desired.usage_generation_id = obligation.try_get("usage_generation_id")?;
        sqlx::query(
            "UPDATE node_convergence_obligations
                SET status = 'dispatched',
                    claim_generation = $5,
                    claimed_at = now(),
                    last_error = NULL
              WHERE node_id = $1
                AND kind = $2
                AND lifecycle_epoch = $3
                AND generation = $4",
        )
        .bind(node_id)
        .bind(kind)
        .bind(lifecycle_epoch)
        .bind(generation)
        .bind(claim_generation)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(Some(desired));
    }
    let Some(row) = sqlx::query(
        // Same ordering as the read-only path above. A dispatched config keeps its lease; a
        // pending config has already been rebased and yields to the permission hot update.
        "WITH active_deployment AS (
            SELECT d.id
            FROM deployments d
            JOIN deployment_targets dt ON dt.deployment_id = d.id
            JOIN deployment_target_state dts
              ON dts.deployment_id = dt.deployment_id
             AND dts.node_id = dt.node_id
            JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = dt.node_id
            WHERE d.active = TRUE
              AND d.status IN ('planned', 'running')
              AND dt.node_id = $1
              AND dt.status IN ('pending', 'dispatched', 'converging')
              AND dts.lifecycle_epoch = lifecycle.lifecycle_epoch
              AND lifecycle.phase IN ('active', 'retiring')
            ORDER BY CASE
                       WHEN d.kind = 'config'
                        AND dt.status IN ('dispatched', 'converging') THEN 0
                       WHEN d.kind = 'grants' THEN 1
                       ELSE 2
                     END,
                     d.id
            LIMIT 1
         ),
         current_wave AS (
            SELECT MIN(dts.wave) AS wave
            FROM active_deployment ad
            JOIN deployment_targets dt
              ON dt.deployment_id = ad.id
            JOIN deployment_target_state dts
              ON dts.deployment_id = dt.deployment_id
             AND dts.node_id = dt.node_id
            WHERE dt.status IN ('pending', 'dispatched', 'converging')
         ),
         wave_gate AS (
            SELECT cw.wave,
                   bool_or(
                     dts.disruptive
                     AND (
                       dts.wave > 1
                       OR dts.desired_structure->'actions' ?| array['disable-xray', 'disable-wire-guard']
                     )
                   ) AS requires_confirmation,
                   EXISTS (
                     SELECT 1
                     FROM deployment_wave_confirmations c
                     WHERE c.deployment_id = ad.id
                       AND c.wave = cw.wave
                   ) AS confirmed
            FROM active_deployment ad
            JOIN current_wave cw
              ON cw.wave IS NOT NULL
            JOIN deployment_targets dt
              ON dt.deployment_id = ad.id
            JOIN deployment_target_state dts
              ON dts.deployment_id = dt.deployment_id
             AND dts.node_id = dt.node_id
            WHERE dt.status IN ('pending', 'dispatched', 'converging')
              AND dts.wave = cw.wave
            GROUP BY ad.id, cw.wave
         )
         SELECT ad.id AS deployment_id,
                dt.node_id,
                dts.wave,
                dts.desired_structure,
                dts.desired_grants,
                d.revision_id
         FROM active_deployment ad
         JOIN deployments d
           ON d.id = ad.id
         JOIN current_wave cw
           ON TRUE
         JOIN wave_gate wg
           ON wg.wave = cw.wave
         JOIN deployment_targets dt
           ON dt.deployment_id = ad.id
         JOIN deployment_target_state dts
           ON dts.deployment_id = dt.deployment_id
          AND dts.node_id = dt.node_id
         JOIN node_lifecycle_state lifecycle
           ON lifecycle.node_id = dt.node_id
         WHERE dt.node_id = $1
           AND dt.status IN ('pending', 'dispatched', 'converging')
           AND dts.lifecycle_epoch = lifecycle.lifecycle_epoch
           AND lifecycle.phase IN ('active', 'retiring')
           AND dts.wave = cw.wave
           AND (NOT wg.requires_confirmation OR wg.confirmed)
           AND (
                dt.status = 'pending'
                OR COALESCE(dts.dispatched_at, '-infinity'::timestamptz)
                     < now() - $2::interval
           )
         ORDER BY ad.id
         LIMIT 1
         FOR UPDATE OF d, dt, dts, lifecycle SKIP LOCKED",
    )
    .bind(node_id)
    .bind(DISPATCH_LEASE_INTERVAL)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(None);
    };

    let deployment_id = row.try_get("deployment_id")?;
    let node_id: String = row.try_get("node_id")?;
    let wave = row.try_get("wave")?;
    let desired_structure = row.try_get("desired_structure")?;
    let grants = frozen_desired_grants_tx(
        &mut tx,
        row.try_get("desired_grants")?,
        revision_to_u64(row.try_get("revision_id")?)?,
        &node_id,
    )
    .await?;
    let desired = desired_deployment_from_structure_tx(
        &mut tx,
        deployment_id,
        node_id.clone(),
        wave,
        desired_structure,
        grants.clone(),
    )
    .await?;
    let dispatched_grants = serde_json::to_value(&grants)?;

    sqlx::query(
        "UPDATE deployment_targets
         SET status = 'dispatched'
         WHERE deployment_id = $1
           AND node_id = $2",
    )
    .bind(deployment_id)
    .bind(&node_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE deployment_target_state
         SET dispatched_at = now(),
             dispatched_grants = $3
         WHERE deployment_id = $1 AND node_id = $2",
    )
    .bind(deployment_id)
    .bind(&node_id)
    .bind(dispatched_grants)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE deployments
         SET status = 'running',
             active = TRUE,
             started_at = COALESCE(started_at, now())
         WHERE id = $1
           AND status IN ('planned', 'running')
           AND active = TRUE",
    )
    .bind(deployment_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Some(desired))
}

pub async fn report_target_result(
    pool: &PgPool,
    report: TargetConvergenceReport,
) -> Result<ReportTargetResult> {
    let mut tx = pool.begin().await?;

    // Not found is a 404, not a 500. The difference is not one of wording: the agent's retry
    // spool routes on 4xx versus 5xx, discarding 4xx as "the control plane says outright this is
    // useless" and keeping 5xx for retry. After a deployment is deleted that observation can
    // never land, and filed as 5xx it sits at the head of the queue blocking everything behind
    // it.
    let deployment = sqlx::query(
        "SELECT status, active
         FROM deployments
         WHERE id = $1
         FOR UPDATE",
    )
    .bind(report.deployment_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("deployment {}", report.deployment_id)))?;
    let deployment_status: String = deployment.try_get("status")?;
    let obligation = sqlx::query(
        "SELECT obligation.kind,
                obligation.lifecycle_epoch AS target_lifecycle_epoch,
                obligation.generation,
                obligation.claim_generation,
                obligation.desired_structure,
                obligation.desired_grants,
                obligation.usage_generation_id,
                lifecycle.lifecycle_epoch AS current_lifecycle_epoch,
                lifecycle.phase AS lifecycle_phase,
                dt.status AS target_status
           FROM node_convergence_obligations obligation
           JOIN deployment_targets dt
             ON dt.deployment_id = obligation.source_deployment_id
            AND dt.node_id = obligation.node_id
           JOIN deployment_target_state dts
             ON dts.deployment_id = dt.deployment_id
            AND dts.node_id = dt.node_id
           JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = obligation.node_id
           JOIN node_operational_isolations isolation ON isolation.node_id = obligation.node_id
          WHERE obligation.source_deployment_id = $1
            AND obligation.node_id = $2
            AND obligation.status IN ('dispatched', 'converging')
          FOR UPDATE OF obligation, dt, dts, lifecycle",
    )
    .bind(report.deployment_id)
    .bind(&report.node_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(obligation) = obligation {
        let expected_claim: i64 = obligation.try_get("claim_generation")?;
        let reported_claim = i64::try_from(report.claim_generation).map_err(|_| {
            StoreError::Conflict(format!(
                "report claim generation is out of range: {}",
                report.claim_generation
            ))
        })?;
        if expected_claim != reported_claim {
            return Err(StoreError::Conflict(format!(
                "obligation report for {}/{} has claim generation {}, current generation is {}",
                report.deployment_id, report.node_id, reported_claim, expected_claim
            )));
        }
        let target_epoch: i64 = obligation.try_get("target_lifecycle_epoch")?;
        let current_epoch: i64 = obligation.try_get("current_lifecycle_epoch")?;
        let lifecycle_phase: String = obligation.try_get("lifecycle_phase")?;
        if target_epoch != current_epoch || lifecycle_phase != "active" {
            return Err(StoreError::Conflict(format!(
                "obligation for {}/{} belongs to lifecycle epoch {}, current state is {}/{}",
                report.deployment_id, report.node_id, target_epoch, current_epoch, lifecycle_phase
            )));
        }

        let baseline_state = load_node_applied_state_for_update(&mut tx, &report.node_id).await?;
        let baseline_matched = baseline_state
            .as_ref()
            .map(|state| state == &report.observed_before);
        let desired_structure: Value = obligation.try_get("desired_structure")?;
        let desired_grants: DesiredGrants =
            serde_json::from_value(obligation.try_get("desired_grants")?)?;
        let final_status = target_status_from_report(&report, &desired_structure, &desired_grants)?;
        let observed_before = reported_node_state_json(&report.observed_before)?;
        let observed_after = reported_node_state_json(&report.observed_after)?;
        let verdict = json!({
            "reported_result": target_apply_result_name(report.result),
            "target_status": final_status,
            "error": report.error,
            "desired_matched": final_status == "succeeded",
            "baseline_matched": baseline_matched,
            "obligation_generation": obligation.try_get::<i64, _>("generation")?,
            "claim_generation": expected_claim,
        });
        sqlx::query(
            "UPDATE deployment_target_state
                SET observed_before = $3,
                    observed_after = $4,
                    verdict = $5,
                    dispatched_grants = NULL
              WHERE deployment_id = $1 AND node_id = $2",
        )
        .bind(report.deployment_id)
        .bind(&report.node_id)
        .bind(observed_before)
        .bind(observed_after)
        .bind(verdict)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE deployment_targets
                SET status = $3, error = $4
              WHERE deployment_id = $1 AND node_id = $2",
        )
        .bind(report.deployment_id)
        .bind(&report.node_id)
        .bind(final_status)
        .bind(&report.error)
        .execute(&mut *tx)
        .await?;
        let kind_value: String = obligation.try_get("kind")?;
        let kind = DeploymentKind::parse(&kind_value).ok_or_else(|| {
            StoreError::InvalidData(format!(
                "obligation has unknown deployment kind {kind_value}"
            ))
        })?;
        upsert_node_applied_state(
            &mut tx,
            report.deployment_id,
            &report.node_id,
            &report.observed_after,
            kind,
        )
        .await?;
        if final_status == "succeeded" {
            if let Some(generation_id) =
                obligation.try_get::<Option<i64>, _>("usage_generation_id")?
            {
                crate::usage::activate_usage_generation(
                    &mut tx,
                    &report.node_id,
                    generation_id,
                    report.deployment_id,
                    report.usage_activated_at_unix_secs,
                )
                .await?;
            }
        }
        sqlx::query(
            "UPDATE node_convergence_obligations
                SET status = $5,
                    settled_at = CASE WHEN $5 = 'succeeded' THEN now() ELSE NULL END,
                    last_error = $6
              WHERE node_id = $1
                AND kind = $2
                AND lifecycle_epoch = $3
                AND generation = $4",
        )
        .bind(&report.node_id)
        .bind(kind.as_str())
        .bind(target_epoch)
        .bind(obligation.try_get::<i64, _>("generation")?)
        .bind(final_status)
        .bind(&report.error)
        .execute(&mut *tx)
        .await?;
        store_deployment_settlement_tx(&mut tx, report.deployment_id).await?;
        tx.commit().await?;
        return Ok(ReportTargetResult {
            deployment_id: report.deployment_id,
            node_id: report.node_id,
            target_status: final_status.to_owned(),
            deployment_status,
        });
    }
    if !matches!(deployment_status.as_str(), "planned" | "running") {
        return Err(StoreError::Unsupported(format!(
            "deployment {} is not accepting reports in status {}",
            report.deployment_id, deployment_status
        )));
    }
    if deployment.try_get::<Option<bool>, _>("active")? != Some(true) {
        return Err(StoreError::Unsupported(format!(
            "deployment {} is not active",
            report.deployment_id
        )));
    }
    if report.claim_generation != 0 {
        return Err(StoreError::Conflict(format!(
            "active deployment target {}/{} does not accept claim generation {}",
            report.deployment_id, report.node_id, report.claim_generation
        )));
    }

    let target = sqlx::query(
        "SELECT dt.status, dts.desired_structure, dts.dispatched_grants,
                dts.usage_generation_id,
                dts.lifecycle_epoch AS target_lifecycle_epoch,
                lifecycle.lifecycle_epoch AS current_lifecycle_epoch,
                lifecycle.phase AS lifecycle_phase
         FROM deployment_targets dt
         JOIN deployment_target_state dts
           ON dts.deployment_id = dt.deployment_id
          AND dts.node_id = dt.node_id
         JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = dt.node_id
         WHERE dt.deployment_id = $1 AND dt.node_id = $2
         FOR UPDATE OF dt, dts, lifecycle",
    )
    .bind(report.deployment_id)
    .bind(&report.node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        StoreError::NotFound(format!(
            "target {}/{}",
            report.deployment_id, report.node_id
        ))
    })?;
    let target_status: String = target.try_get("status")?;
    if !matches!(
        target_status.as_str(),
        "pending" | "dispatched" | "converging"
    ) {
        return Err(StoreError::Unsupported(format!(
            "target {}/{} is not accepting reports in status {}",
            report.deployment_id, report.node_id, target_status
        )));
    }
    let target_lifecycle_epoch: i64 = target.try_get("target_lifecycle_epoch")?;
    let current_lifecycle_epoch: i64 = target.try_get("current_lifecycle_epoch")?;
    let lifecycle_phase: String = target.try_get("lifecycle_phase")?;
    if target_lifecycle_epoch != current_lifecycle_epoch {
        return Err(StoreError::Conflict(format!(
            "target {}/{} belongs to lifecycle epoch {}, current epoch is {}",
            report.deployment_id, report.node_id, target_lifecycle_epoch, current_lifecycle_epoch
        )));
    }
    if !matches!(lifecycle_phase.as_str(), "active" | "retiring") {
        return Err(StoreError::Unsupported(format!(
            "node {} is {}; it no longer accepts deployment reports",
            report.node_id, lifecycle_phase
        )));
    }

    let baseline_state = load_node_applied_state_for_update(&mut tx, &report.node_id).await?;
    let baseline_matched = baseline_state
        .as_ref()
        .map(|state| state == &report.observed_before);
    let desired_structure: Value = target.try_get("desired_structure")?;
    let dispatched_grants: Option<Value> = target.try_get("dispatched_grants")?;
    let desired_grants = dispatched_grants
        .ok_or_else(|| {
            StoreError::InvalidData(format!(
                "target {}/{} has no dispatched grants; desired state must be claimed before reporting",
                report.deployment_id, report.node_id
            ))
        })
        .and_then(|value| serde_json::from_value(value).map_err(StoreError::from))?;
    let final_status = target_status_from_report(&report, &desired_structure, &desired_grants)?;
    let observed_before = reported_node_state_json(&report.observed_before)?;
    let observed_after = reported_node_state_json(&report.observed_after)?;
    let verdict = json!({
        "reported_result": target_apply_result_name(report.result),
        "target_status": final_status,
        "error": report.error,
        "desired_matched": final_status == "succeeded",
        "baseline_matched": baseline_matched,
    });

    sqlx::query(
        "UPDATE deployment_target_state
         SET observed_before = $3,
             observed_after = $4,
             verdict = $5,
             dispatched_grants = NULL
         WHERE deployment_id = $1 AND node_id = $2",
    )
    .bind(report.deployment_id)
    .bind(&report.node_id)
    .bind(observed_before)
    .bind(observed_after)
    .bind(verdict)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE deployment_targets
         SET status = $3,
             error = $4
         WHERE deployment_id = $1 AND node_id = $2",
    )
    .bind(report.deployment_id)
    .bind(&report.node_id)
    .bind(final_status)
    .bind(&report.error)
    .execute(&mut *tx)
    .await?;

    // In a grants deployment phantun, wireguard, and xray are all Unmanaged, and the agent
    // faithfully reports Unmanaged (observe_linux_* returns it unchanged). Overwriting the applied
    // state with that would have the control plane believe this machine's configuration is
    // unmanaged — while it runs perfectly well on the machine. So a grants deployment updates only
    // the two list columns, and the three configuration artifacts keep the observation the last
    // configuration deployment left.
    let kind: String = sqlx::query_scalar("SELECT kind FROM deployments WHERE id = $1")
        .bind(report.deployment_id)
        .fetch_one(&mut *tx)
        .await?;
    let kind = DeploymentKind::parse(&kind).unwrap_or_default();
    upsert_node_applied_state(
        &mut tx,
        report.deployment_id,
        &report.node_id,
        &report.observed_after,
        kind,
    )
    .await?;

    if final_status == "succeeded" {
        if let Some(generation_id) = target.try_get::<Option<i64>, _>("usage_generation_id")? {
            crate::usage::activate_usage_generation(
                &mut tx,
                &report.node_id,
                generation_id,
                report.deployment_id,
                report.usage_activated_at_unix_secs,
            )
            .await?;
        }
    }

    if kind == DeploymentKind::Config
        && final_status == "succeeded"
        && lifecycle_phase == "retiring"
        && desired_structure_is_full_teardown(&desired_structure)
        && crate::lifecycle::fully_disabled(&report.observed_after)
    {
        crate::lifecycle::complete_retirement_tx(
            &mut tx,
            &report.node_id,
            current_lifecycle_epoch,
            Some(report.deployment_id),
            "agent",
        )
        .await?;
    } else if lifecycle_phase == "retiring" && final_status != "succeeded" {
        sqlx::query(
            "UPDATE node_lifecycle_state
                SET last_error = $3, updated_at = now()
              WHERE node_id = $1 AND lifecycle_epoch = $2 AND phase = 'retiring'",
        )
        .bind(&report.node_id)
        .bind(current_lifecycle_epoch)
        .bind(
            report
                .error
                .as_deref()
                .unwrap_or("retirement deployment did not converge"),
        )
        .execute(&mut *tx)
        .await?;
    }

    let deployment_status =
        refresh_deployment_status(&mut tx, report.deployment_id, final_status).await?;
    tx.commit().await?;

    Ok(ReportTargetResult {
        deployment_id: report.deployment_id,
        node_id: report.node_id,
        target_status: final_status.to_owned(),
        deployment_status,
    })
}

pub async fn isolate_deployment_target(
    pool: &PgPool,
    actor: &AdminContext,
    deployment_id: i64,
    node_id: &str,
    request: IsolateDeploymentTargetRequest,
) -> Result<NodeIsolationCommandResult> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can isolate deployment targets".to_owned(),
        ));
    }
    let reason = request.reason.trim();
    if reason.is_empty() {
        return Err(StoreError::InvalidData(
            "isolation reason must not be empty".to_owned(),
        ));
    }

    let mut tx = pool.begin().await?;
    let rows = sqlx::query(
        "SELECT d.id AS deployment_id,
                d.revision_id,
                d.kind,
                dt.status AS target_status,
                dts.wave,
                dts.desired_structure,
                dts.desired_grants,
                dts.dispatched_grants,
                dts.lifecycle_epoch
           FROM deployments d
           JOIN deployment_targets dt ON dt.deployment_id = d.id
           JOIN deployment_target_state dts
             ON dts.deployment_id = dt.deployment_id
            AND dts.node_id = dt.node_id
          WHERE d.active = TRUE
            AND d.status IN ('planned', 'running', 'halted')
            AND dt.node_id = $1
            AND dt.status IN (
                'pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty'
            )
          ORDER BY d.id
          FOR UPDATE OF d, dt, dts",
    )
    .bind(node_id)
    .fetch_all(&mut *tx)
    .await?;
    let selected = rows
        .iter()
        .find(|row| row.try_get::<i64, _>("deployment_id").ok() == Some(deployment_id))
        .ok_or_else(|| {
            StoreError::NotFound(format!(
                "active deployment target {deployment_id}/{node_id}"
            ))
        })?;
    let selected_status: String = selected.try_get("target_status")?;
    if selected_status != request.expected_target_status {
        return Err(StoreError::Conflict(format!(
            "target {deployment_id}/{node_id} changed from {} to {selected_status}",
            request.expected_target_status
        )));
    }
    let uncertain = rows.iter().any(|row| {
        row.try_get::<String, _>("target_status")
            .is_ok_and(|status| {
                matches!(
                    status.as_str(),
                    "dispatched" | "converging" | "failed-recovered" | "failed-dirty"
                )
            })
    });
    if uncertain && !request.acknowledge_uncertain {
        return Err(StoreError::Conflict(
            "target state is uncertain; acknowledge_uncertain is required".to_owned(),
        ));
    }

    let lifecycle = sqlx::query(
        "SELECT lifecycle_epoch, phase
           FROM node_lifecycle_state
          WHERE node_id = $1
          FOR UPDATE",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("node lifecycle {node_id}")))?;
    let lifecycle_epoch: i64 = lifecycle.try_get("lifecycle_epoch")?;
    let lifecycle_phase: String = lifecycle.try_get("phase")?;
    if lifecycle_phase != "active" {
        return Err(StoreError::Conflict(format!(
            "node {node_id} is {lifecycle_phase}; operational isolation only applies to active nodes"
        )));
    }

    let already_isolated = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1 FROM node_operational_isolations WHERE node_id = $1
         )",
    )
    .bind(node_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO node_operational_isolations (
             node_id, actor, reason, source_deployment_id
         ) VALUES ($1, $2, $3, $4)
         ON CONFLICT (node_id) DO UPDATE SET
             actor = EXCLUDED.actor,
             reason = EXCLUDED.reason,
             source_deployment_id = EXCLUDED.source_deployment_id,
             updated_at = now()",
    )
    .bind(node_id)
    .bind(actor.operator_id())
    .bind(reason)
    .bind(deployment_id)
    .execute(&mut *tx)
    .await?;
    if !already_isolated {
        sqlx::query(
            "INSERT INTO node_operational_isolation_events (
                 node_id, event, actor, reason, deployment_id, details
             ) VALUES ($1, 'isolated', $2, $3, $4, $5)",
        )
        .bind(node_id)
        .bind(actor.operator_id())
        .bind(reason)
        .bind(deployment_id)
        .bind(json!({
            "selected_target_status": selected_status,
            "uncertain": uncertain,
        }))
        .execute(&mut *tx)
        .await?;
    }

    let mut affected = BTreeSet::new();
    for row in rows {
        let source_deployment_id: i64 = row.try_get("deployment_id")?;
        let revision_id: i64 = row.try_get("revision_id")?;
        let kind_value: String = row.try_get("kind")?;
        let kind = DeploymentKind::parse(&kind_value).ok_or_else(|| {
            StoreError::InvalidData(format!(
                "deployment {source_deployment_id} has unknown kind {kind_value}"
            ))
        })?;
        let target_status: String = row.try_get("target_status")?;
        let target_epoch: i64 = row.try_get("lifecycle_epoch")?;
        if target_epoch != lifecycle_epoch {
            return Err(StoreError::Conflict(format!(
                "target {source_deployment_id}/{node_id} belongs to lifecycle epoch {target_epoch}, current epoch is {lifecycle_epoch}"
            )));
        }
        if matches!(
            target_status.as_str(),
            "dispatched" | "converging" | "failed-recovered" | "failed-dirty"
        ) {
            mark_node_applied_dirty_tx(
                &mut tx,
                source_deployment_id,
                node_id,
                kind,
                "节点隔离时目标已下发或失败，运行态不确定",
            )
            .await?;
            sqlx::query(
                "UPDATE deployment_target_state
                    SET claim_generation = claim_generation + 1,
                        dispatched_grants = NULL
                  WHERE deployment_id = $1 AND node_id = $2",
            )
            .bind(source_deployment_id)
            .bind(node_id)
            .execute(&mut *tx)
            .await?;
        }

        let snapshot =
            crate::materialize::load_snapshot_tx(&mut tx, Some(revision_to_u64(revision_id)?))
                .await?;
        let obligation_target = if kind == DeploymentKind::Config {
            plan_snapshot_deployment(&snapshot, &[])
                .map_err(plan_error)?
                .targets
                .into_iter()
                .find(|target| target.node_id == node_id)
                .ok_or_else(|| {
                    StoreError::InvalidData(format!(
                        "node {node_id} is absent from deployment {source_deployment_id} full desired state"
                    ))
                })?
        } else {
            let grants_value = row
                .try_get::<Option<Value>, _>("dispatched_grants")?
                .or(row.try_get::<Option<Value>, _>("desired_grants")?)
                .ok_or_else(|| {
                    StoreError::InvalidData(format!(
                        "target {source_deployment_id}/{node_id} has no frozen grants"
                    ))
                })?;
            let desired = desired_deployment_from_structure_tx(
                &mut tx,
                source_deployment_id,
                node_id.to_owned(),
                row.try_get("wave")?,
                row.try_get("desired_structure")?,
                serde_json::from_value(grants_value)?,
            )
            .await?;
            PlannedTarget {
                node_id: node_id.to_owned(),
                status: PlannedTargetStatus::Deferred,
                wave: desired.wave,
                disruptive: false,
                actions: desired.actions,
                desired: desired.desired,
            }
        };
        for (_, artifact) in obligation_target.desired.artifacts() {
            insert_artifact_blob(&mut tx, artifact).await?;
        }
        let usage_generation_id = crate::usage::create_usage_generation_for_target(
            &mut tx,
            source_deployment_id,
            node_id,
            &snapshot,
            &obligation_target.desired,
            &obligation_target.actions,
        )
        .await?;

        sqlx::query(
            "UPDATE deployment_targets
                SET status = 'deferred',
                    error = $3
              WHERE deployment_id = $1 AND node_id = $2",
        )
        .bind(source_deployment_id)
        .bind(node_id)
        .bind(if uncertain {
            "节点已隔离；旧执行状态不确定，恢复后补齐最新完整期望"
        } else {
            "节点已隔离；恢复后补齐最新完整期望"
        })
        .execute(&mut *tx)
        .await?;
        insert_or_supersede_obligation_tx(
            &mut tx,
            source_deployment_id,
            revision_id,
            lifecycle_epoch,
            kind,
            &obligation_target,
            usage_generation_id,
        )
        .await?;
        affected.insert(source_deployment_id);
    }

    for affected_deployment in &affected {
        refresh_deployment_status(&mut tx, *affected_deployment, "deferred").await?;
    }
    let serving_generation = crate::serving::refresh_isolation_snapshot_tx(&mut tx).await?;
    let debt_count: i64 = sqlx::query_scalar(
        "SELECT count(*)
           FROM node_convergence_obligations
          WHERE node_id = $1
            AND lifecycle_epoch = $2
            AND status IN (
                'pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty'
            )",
    )
    .bind(node_id)
    .bind(lifecycle_epoch)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(NodeIsolationCommandResult {
        node_id: node_id.to_owned(),
        isolated: true,
        affected_deployments: affected.into_iter().collect(),
        debt_count: i64_to_u64("isolation debt_count", debt_count)?,
        serving_generation,
    })
}

pub async fn restore_node_service(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    request: RestoreNodeServiceRequest,
) -> Result<NodeIsolationCommandResult> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can restore an isolated node to service".to_owned(),
        ));
    }
    let reason = request.reason.trim();
    if reason.is_empty() {
        return Err(StoreError::InvalidData(
            "restore reason must not be empty".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT lifecycle.phase,
                lifecycle.lifecycle_epoch,
                node.overlay,
                agent.last_poll_at >= now() - interval '90 seconds' AS poll_fresh,
                agent.runtime_reported_at >= now() - interval '2 minutes' AS runtime_fresh,
                COALESCE((agent.wireguard_health->>'enabled')::boolean, FALSE) AS wg_enabled,
                NULLIF(agent.wireguard_health->>'error', '') AS wg_error,
                applied.phantun_state,
                applied.wireguard_state,
                applied.xray_state,
                applied.hy2_port_hop_state,
                applied.grants_state
           FROM node_lifecycle_state lifecycle
           JOIN nodes node ON node.id = lifecycle.node_id
           LEFT JOIN node_agent_state agent ON agent.node_id = lifecycle.node_id
           LEFT JOIN node_applied_state applied ON applied.node_id = lifecycle.node_id
          WHERE lifecycle.node_id = $1
          FOR UPDATE OF lifecycle",
    )
    .bind(node_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "SELECT node_id
           FROM node_operational_isolations
          WHERE node_id = $1
          FOR UPDATE",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("node isolation {node_id}")))?;
    let phase: String = row.try_get("phase")?;
    if phase != "active" {
        return Err(StoreError::Conflict(format!(
            "node {node_id} is {phase}, not active"
        )));
    }
    let lifecycle_epoch: i64 = row.try_get("lifecycle_epoch")?;
    let debt_count: i64 = sqlx::query_scalar(
        "SELECT count(*)
           FROM node_convergence_obligations
          WHERE node_id = $1
            AND lifecycle_epoch = $2
            AND status IN (
                'pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty'
            )",
    )
    .bind(node_id)
    .bind(lifecycle_epoch)
    .fetch_one(&mut *tx)
    .await?;
    if debt_count > 0 {
        return Err(StoreError::Conflict(format!(
            "node {node_id} still has {debt_count} convergence obligation(s)"
        )));
    }
    if !row
        .try_get::<Option<bool>, _>("poll_fresh")?
        .unwrap_or(false)
    {
        return Err(StoreError::Conflict(format!(
            "node {node_id} has no desired poll in the last 90 seconds"
        )));
    }
    if !row
        .try_get::<Option<bool>, _>("runtime_fresh")?
        .unwrap_or(false)
    {
        return Err(StoreError::Conflict(format!(
            "node {node_id} has no runtime report in the last 2 minutes"
        )));
    }
    for field in [
        "phantun_state",
        "wireguard_state",
        "xray_state",
        "hy2_port_hop_state",
        "grants_state",
    ] {
        let state = row.try_get::<Option<String>, _>(field)?;
        if state
            .as_deref()
            .is_none_or(|state| matches!(state, "unknown" | "dirty"))
        {
            return Err(StoreError::Conflict(format!(
                "node {node_id} has unconfirmed applied state in {field}"
            )));
        }
    }
    let overlay: bool = row.try_get("overlay")?;
    if overlay
        && (!row
            .try_get::<Option<bool>, _>("wg_enabled")?
            .unwrap_or(false)
            || row.try_get::<Option<String>, _>("wg_error")?.is_some())
    {
        return Err(StoreError::Conflict(format!(
            "node {node_id} WireGuard runtime health is not ready"
        )));
    }

    let affected_deployments = sqlx::query_scalar::<_, i64>(
        "SELECT DISTINCT source_deployment_id
           FROM node_convergence_obligations
          WHERE node_id = $1
          ORDER BY source_deployment_id",
    )
    .bind(node_id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM node_operational_isolations WHERE node_id = $1")
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO node_operational_isolation_events (
             node_id, event, actor, reason, details
         ) VALUES ($1, 'restored', $2, $3, $4)",
    )
    .bind(node_id)
    .bind(actor.operator_id())
    .bind(reason)
    .bind(json!({ "lifecycle_epoch": lifecycle_epoch }))
    .execute(&mut *tx)
    .await?;
    let serving_generation = crate::serving::refresh_isolation_snapshot_tx(&mut tx).await?;
    tx.commit().await?;
    Ok(NodeIsolationCommandResult {
        node_id: node_id.to_owned(),
        isolated: false,
        affected_deployments,
        debt_count: 0,
        serving_generation,
    })
}

pub async fn halt_deployment(
    pool: &PgPool,
    admin: &AdminContext,
    deployment_id: i64,
) -> Result<DeploymentCommandResult> {
    ensure_deployment_write_access(pool, admin, deployment_id).await?;
    let row = sqlx::query(
        "UPDATE deployments
         SET status = 'halted',
             active = TRUE,
             halted_at = COALESCE(halted_at, now())
         WHERE id = $1
           AND status IN ('planned', 'running')
           AND active = TRUE
         RETURNING status, active",
    )
    .bind(deployment_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| {
        StoreError::Unsupported(format!(
            "deployment {deployment_id} cannot be halted from its current state"
        ))
    })?;

    Ok(DeploymentCommandResult {
        deployment_id,
        status: row.try_get("status")?,
        active: row.try_get("active")?,
        sync_deployment_id: None,
        rollback_deployment_id: None,
    })
}

pub async fn cancel_deployment(
    pool: &PgPool,
    actor: &AdminContext,
    deployment_id: i64,
) -> Result<DeploymentCommandResult> {
    ensure_deployment_write_access(pool, actor, deployment_id).await?;
    let mut tx = pool.begin().await?;
    let canceled = cancel_deployment_tx(&mut tx, deployment_id).await?;
    tx.commit().await?;

    Ok(DeploymentCommandResult {
        deployment_id,
        status: canceled.status,
        active: canceled.active,
        sync_deployment_id: None,
        rollback_deployment_id: None,
    })
}

pub async fn cancel_deployment_and_rollback(
    pool: &PgPool,
    actor: &AdminContext,
    deployment_id: i64,
) -> Result<DeploymentCommandResult> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "cancel-and-rollback must run as system-admin because it restores global model state"
                .to_owned(),
        ));
    }

    let idempotency_key = format!("system:cancel-and-rollback:{deployment_id}");
    let mut tx = pool.begin().await?;

    if let Some(existing) = sqlx::query(
        "SELECT id, rollback_of_deployment_id
         FROM deployments
         WHERE idempotency_key = $1",
    )
    .bind(&idempotency_key)
    .fetch_optional(&mut *tx)
    .await?
    {
        if existing
            .try_get::<Option<i64>, _>("rollback_of_deployment_id")?
            .is_none()
        {
            return Err(StoreError::InvalidData(format!(
                "idempotency_key {idempotency_key} already belongs to a non-rollback deployment"
            )));
        }
        let source = sqlx::query(
            "SELECT status, active
             FROM deployments
             WHERE id = $1",
        )
        .bind(deployment_id)
        .fetch_one(&mut *tx)
        .await?;
        let rollback_deployment_id = existing.try_get("id")?;
        tx.commit().await?;
        return Ok(DeploymentCommandResult {
            deployment_id,
            status: source.try_get("status")?,
            active: source.try_get("active")?,
            sync_deployment_id: None,
            rollback_deployment_id: Some(rollback_deployment_id),
        });
    }

    let target = latest_succeeded_deployment_before_tx(&mut tx, deployment_id).await?;
    let target_snapshot = materialize::load_snapshot_tx(&mut tx, Some(target.revision_id)).await?;
    let canceled = cancel_deployment_tx(&mut tx, deployment_id).await?;
    let previous_revision = console::lock_control_state(&mut tx).await?;
    let actor_id = actor.operator_id().to_owned();
    let note = format!(
        "cancel deployment #{} and rollback current model to deployment #{} (revision {})",
        deployment_id, target.deployment_id, target.revision_id
    );
    let rollback_revision = console::insert_revision(&mut tx, &actor_id, &note).await?;
    restore_model_snapshot_tx(&mut tx, rollback_revision, &target_snapshot).await?;
    let rollback_revision =
        console::commit_revision(&mut tx, rollback_revision, previous_revision, true).await?;
    let restored_snapshot = materialize::load_current_snapshot_tx(&mut tx).await?;
    ensure_restored_snapshot(
        "cancel-and-rollback",
        &target_snapshot,
        &restored_snapshot,
        rollback_revision,
    )?;

    let mut applied = load_applied_states(&mut *tx).await?;
    if !canceled.uncertain_nodes.is_empty() {
        mark_applied_states_dirty_with_reason(
            &mut applied,
            &canceled.uncertain_nodes,
            format!(
                "cancel-and-rollback deployment {} canceled a deployment target while it was in flight",
                deployment_id
            ),
        );
    }
    let mut plan =
        plan_forced_snapshot_deployment(&restored_snapshot, &applied).map_err(plan_error)?;
    let terminal = terminal_lifecycle_nodes(&mut *tx).await?;
    plan = without_terminal_lifecycle_targets(plan, &terminal);
    let isolated = isolated_node_ids(&mut *tx).await?;
    mark_isolated_targets(&mut plan, &isolated);
    if plan.summary.total_targets == 0 {
        return Err(StoreError::InvalidData(
            "cannot create rollback deployment for an empty target set".to_owned(),
        ));
    }
    let (rollback_deployment_id, _) = insert_rollback_deployment_tx(
        &mut tx,
        &actor_id,
        &idempotency_key,
        &note,
        target.deployment_id,
        &plan,
    )
    .await?;
    tx.commit().await?;

    Ok(DeploymentCommandResult {
        deployment_id,
        status: canceled.status,
        active: canceled.active,
        sync_deployment_id: None,
        rollback_deployment_id: Some(rollback_deployment_id),
    })
}

pub async fn retry_target(
    pool: &PgPool,
    actor: &AdminContext,
    deployment_id: i64,
    node_id: &str,
) -> Result<ReportTargetResult> {
    ensure_node_access(pool, actor, node_id).await?;
    ensure_deployment_write_access(pool, actor, deployment_id).await?;
    let mut tx = pool.begin().await?;
    let deployment = sqlx::query(
        "SELECT status, active
         FROM deployments
         WHERE id = $1
         FOR UPDATE",
    )
    .bind(deployment_id)
    .fetch_one(&mut *tx)
    .await?;
    let deployment_status: String = deployment.try_get("status")?;
    let obligation = sqlx::query(
        "SELECT obligation.kind, obligation.lifecycle_epoch, obligation.generation
           FROM node_convergence_obligations obligation
           JOIN node_operational_isolations isolation ON isolation.node_id = obligation.node_id
           JOIN node_lifecycle_state lifecycle
             ON lifecycle.node_id = obligation.node_id
            AND lifecycle.lifecycle_epoch = obligation.lifecycle_epoch
            AND lifecycle.phase = 'active'
          WHERE obligation.source_deployment_id = $1
            AND obligation.node_id = $2
            AND obligation.status IN ('failed-recovered', 'failed-dirty')
          FOR UPDATE OF obligation",
    )
    .bind(deployment_id)
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(obligation) = obligation {
        if !actor.is_system_admin() {
            return Err(StoreError::Forbidden(
                "only system-admin can retry isolated convergence debt".to_owned(),
            ));
        }
        sqlx::query(
            "UPDATE node_convergence_obligations
                SET status = 'pending',
                    claimed_at = NULL,
                    last_error = NULL
              WHERE node_id = $1
                AND kind = $2
                AND lifecycle_epoch = $3
                AND generation = $4",
        )
        .bind(node_id)
        .bind(obligation.try_get::<String, _>("kind")?)
        .bind(obligation.try_get::<i64, _>("lifecycle_epoch")?)
        .bind(obligation.try_get::<i64, _>("generation")?)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE deployment_targets
                SET status = 'deferred',
                    error = '隔离节点债务等待重新领取'
              WHERE deployment_id = $1 AND node_id = $2",
        )
        .bind(deployment_id)
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
        store_deployment_settlement_tx(&mut tx, deployment_id).await?;
        tx.commit().await?;
        return Ok(ReportTargetResult {
            deployment_id,
            node_id: node_id.to_owned(),
            target_status: "deferred".to_owned(),
            deployment_status,
        });
    }
    if deployment_status != "halted" {
        return Err(StoreError::Unsupported(format!(
            "deployment {deployment_id} cannot retry targets from status {deployment_status}"
        )));
    }

    let target = sqlx::query(
        "SELECT dt.status,
                dts.lifecycle_epoch AS target_lifecycle_epoch,
                lifecycle.lifecycle_epoch AS current_lifecycle_epoch,
                lifecycle.phase AS lifecycle_phase
           FROM deployment_targets dt
           JOIN deployment_target_state dts
             ON dts.deployment_id = dt.deployment_id AND dts.node_id = dt.node_id
           JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = dt.node_id
          WHERE dt.deployment_id = $1 AND dt.node_id = $2
          FOR UPDATE OF dt, dts, lifecycle",
    )
    .bind(deployment_id)
    .bind(node_id)
    .fetch_one(&mut *tx)
    .await?;
    let target_status: String = target.try_get("status")?;
    if !matches!(target_status.as_str(), "failed-recovered" | "failed-dirty") {
        return Err(StoreError::Unsupported(format!(
            "target {deployment_id}/{node_id} cannot be retried from status {target_status}"
        )));
    }
    let target_epoch: i64 = target.try_get("target_lifecycle_epoch")?;
    let current_epoch: i64 = target.try_get("current_lifecycle_epoch")?;
    let lifecycle_phase: String = target.try_get("lifecycle_phase")?;
    if target_epoch != current_epoch || !matches!(lifecycle_phase.as_str(), "active" | "retiring") {
        return Err(StoreError::Conflict(format!(
            "target {deployment_id}/{node_id} belongs to an obsolete node lifecycle"
        )));
    }

    sqlx::query(
        "UPDATE deployment_targets
         SET status = 'pending',
             error = NULL
         WHERE deployment_id = $1 AND node_id = $2",
    )
    .bind(deployment_id)
    .bind(node_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE deployment_target_state
         SET dispatched_at = NULL,
             dispatched_grants = NULL
         WHERE deployment_id = $1 AND node_id = $2",
    )
    .bind(deployment_id)
    .bind(node_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE deployments
         SET status = 'running',
             active = TRUE,
             halted_at = NULL,
             finished_at = NULL,
             started_at = COALESCE(started_at, now())
         WHERE id = $1",
    )
    .bind(deployment_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(ReportTargetResult {
        deployment_id,
        node_id: node_id.to_owned(),
        target_status: "pending".to_owned(),
        deployment_status: "running".to_owned(),
    })
}

async fn scope_plan(
    pool: &PgPool,
    actor: &AdminContext,
    mut plan: DeploymentPlan,
) -> Result<DeploymentPlan> {
    if actor.is_system_admin() {
        return Ok(plan);
    }
    let accessible_nodes = accessible_node_ids(pool, actor).await?;
    plan.targets
        .retain(|target| accessible_nodes.contains(&target.node_id));
    retain_visible_warnings(&mut plan.warnings, &accessible_nodes);
    plan.summary = brocade_deployment::plan::summarize_targets(&plan.targets);
    Ok(plan)
}

fn retain_visible_warnings(warnings: &mut Vec<PlanDiagnostic>, visible_nodes: &BTreeSet<String>) {
    warnings.retain(|warning| warning_is_visible_to_nodes(warning, visible_nodes));
}

fn warning_is_visible_to_nodes(warning: &PlanDiagnostic, visible_nodes: &BTreeSet<String>) -> bool {
    visible_nodes.contains(warning.location.as_str())
        || warning
            .location
            .split('/')
            .any(|part| visible_nodes.contains(part))
}

async fn accessible_node_ids(pool: &PgPool, actor: &AdminContext) -> Result<BTreeSet<String>> {
    let tenant_scope = require_actor_tenant_scope(actor)?;
    let tenant_pattern = actor
        .tenant_scope_like_pattern()
        .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))?;
    let rows = sqlx::query(
        "SELECT id
         FROM nodes
         WHERE tenant_id = $1 OR tenant_id LIKE $2 ESCAPE '\\'",
    )
    .bind(tenant_scope)
    .bind(tenant_pattern)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| row.try_get("id").map_err(StoreError::from))
        .collect()
}

async fn cancel_deployment_tx(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
) -> Result<CanceledDeployment> {
    let deployment = sqlx::query(
        "SELECT status, active
         FROM deployments
         WHERE id = $1
         FOR UPDATE",
    )
    .bind(deployment_id)
    .fetch_one(&mut **tx)
    .await?;
    let status: String = deployment.try_get("status")?;
    if !matches!(status.as_str(), "planned" | "running" | "halted") {
        return Err(StoreError::Unsupported(format!(
            "deployment {deployment_id} cannot be canceled from status {status}"
        )));
    }

    let rows = sqlx::query(
        "SELECT node_id, status
         FROM deployment_targets
         WHERE deployment_id = $1
           AND status IN ('pending', 'dispatched', 'converging')
         ORDER BY node_id
         FOR UPDATE",
    )
    .bind(deployment_id)
    .fetch_all(&mut **tx)
    .await?;
    let uncertain_nodes = rows
        .iter()
        .filter_map(|row| {
            let status: String = row.try_get("status").ok()?;
            matches!(status.as_str(), "dispatched" | "converging").then(|| row.try_get("node_id"))
        })
        .collect::<std::result::Result<Vec<String>, sqlx::Error>>()?;

    mark_uncertain_cancel_targets_dirty(tx, deployment_id, &uncertain_nodes).await?;

    let obligation_rows = sqlx::query(
        "SELECT node_id, kind, status
           FROM node_convergence_obligations
          WHERE source_deployment_id = $1
            AND status IN (
                'pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty'
            )
          FOR UPDATE",
    )
    .bind(deployment_id)
    .fetch_all(&mut **tx)
    .await?;
    for obligation in &obligation_rows {
        let obligation_status: String = obligation.try_get("status")?;
        if matches!(obligation_status.as_str(), "dispatched" | "converging") {
            let kind_value: String = obligation.try_get("kind")?;
            let kind = DeploymentKind::parse(&kind_value).ok_or_else(|| {
                StoreError::InvalidData(format!("unknown obligation kind {kind_value}"))
            })?;
            let obligation_node: String = obligation.try_get("node_id")?;
            mark_node_applied_dirty_tx(
                tx,
                deployment_id,
                &obligation_node,
                kind,
                "隔离债务随未激活发布取消，旧执行结果不再可信",
            )
            .await?;
        }
    }
    sqlx::query(
        "UPDATE node_convergence_obligations
            SET status = 'canceled', last_error = '来源发布已取消'
          WHERE source_deployment_id = $1
            AND status IN (
                'pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty'
            )",
    )
    .bind(deployment_id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "UPDATE deployment_targets
         SET status = 'canceled'
         WHERE deployment_id = $1
           AND status IN ('pending', 'dispatched', 'converging', 'deferred')",
    )
    .bind(deployment_id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "UPDATE deployment_target_state
         SET dispatched_grants = NULL
         WHERE deployment_id = $1",
    )
    .bind(deployment_id)
    .execute(&mut **tx)
    .await?;

    let row = sqlx::query(
        "UPDATE deployments
         SET status = 'canceled',
             active = NULL,
             activation_status = 'rejected',
             settlement_status = 'converged',
             finished_at = COALESCE(finished_at, now())
         WHERE id = $1
         RETURNING status, active",
    )
    .bind(deployment_id)
    .fetch_one(&mut **tx)
    .await?;

    Ok(CanceledDeployment {
        status: row.try_get("status")?,
        active: row.try_get("active")?,
        uncertain_nodes,
    })
}

async fn latest_succeeded_deployment_before_tx(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
) -> Result<RollbackTarget> {
    let row = sqlx::query(
        "SELECT id, revision_id
         FROM deployments
         WHERE id < $1
           AND status = 'succeeded'
           AND activation_status = 'activated'
         ORDER BY id DESC
         LIMIT 1",
    )
    .bind(deployment_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| {
        StoreError::Unsupported(format!(
            "deployment {deployment_id} has no earlier succeeded deployment to roll back to"
        ))
    })?;

    Ok(RollbackTarget {
        deployment_id: row.try_get("id")?,
        revision_id: revision_to_u64(row.try_get("revision_id")?)?,
    })
}

async fn cancel_active_deployment_for_rollback(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<String>> {
    let Some(active) = sqlx::query(
        "SELECT id, status
         FROM deployments
         WHERE active = TRUE
         ORDER BY id
         LIMIT 1
         FOR UPDATE",
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(Vec::new());
    };
    let deployment_id = active.try_get("id")?;
    let status: String = active.try_get("status")?;
    if !matches!(status.as_str(), "planned" | "running" | "halted") {
        return Err(StoreError::Unsupported(format!(
            "active deployment {deployment_id} cannot be canceled from status {status}"
        )));
    }

    let canceled = cancel_deployment_tx(tx, deployment_id).await?;
    Ok(canceled.uncertain_nodes)
}

async fn rollback_target_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    target_deployment_id: i64,
) -> Result<ModelSnapshot> {
    let target = sqlx::query(
        "SELECT revision_id, status, activation_status
         FROM deployments
         WHERE id = $1",
    )
    .bind(target_deployment_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("deployment {target_deployment_id}")))?;
    let target_status: String = target.try_get("status")?;
    let activation_status: String = target.try_get("activation_status")?;
    if target_status != "succeeded" || activation_status != "activated" {
        return Err(StoreError::Unsupported(format!(
            "deployment {target_deployment_id} cannot be used as rollback target from status {target_status}/{activation_status}"
        )));
    }
    let target_revision = revision_to_u64(target.try_get("revision_id")?)?;
    materialize::load_snapshot_tx(tx, Some(target_revision)).await
}

async fn restore_model_snapshot_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
    snapshot: &ModelSnapshot,
) -> Result<()> {
    // Provider control tokens are operational state and intentionally absent from model
    // snapshots. Preserve one when the target restores the exact same Cloudflare device.
    let warp_binding_tokens = load_warp_binding_tokens_tx(tx).await?;
    restore_settings_tx(tx, snapshot).await?;
    restore_tenants_tx(tx, revision_id, snapshot).await?;
    restore_users_tx(tx, revision_id, snapshot).await?;
    restore_nodes_tx(tx, revision_id, snapshot).await?;
    clear_app_model_tx(tx).await?;
    restore_apps_tx(tx, revision_id, snapshot, &warp_binding_tokens).await?;
    restore_node_egress_dns_tx(tx, revision_id, snapshot).await?;
    Ok(())
}

async fn restore_node_egress_dns_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
    snapshot: &ModelSnapshot,
) -> Result<()> {
    sqlx::query("DELETE FROM node_egress_dns")
        .execute(&mut **tx)
        .await?;
    let revision_id = revision_to_i64(revision_id)?;
    for (node_id, position, selector, resolution) in
        crate::egress_dns::policies_from_snapshot(snapshot)
    {
        sqlx::query(
            "INSERT INTO node_egress_dns (
                node_id, position, selector, resolution, created_revision
             ) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(node_id)
        .bind(
            i32::try_from(position).map_err(|_| {
                StoreError::InvalidData("机器 DNS 策略优先级超出数据库范围".to_owned())
            })?,
        )
        .bind(serde_json::to_value(selector)?)
        .bind(serde_json::to_value(resolution)?)
        .bind(revision_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Refuse to commit a restore that merely carries the expected revision number.
///
/// Certificate names are observed operational state: renewal is deliberately not a model
/// revision, and rollback must keep the certificate a node currently holds. Everything else in
/// the snapshot is revisioned model state and must equal the historical target exactly.
fn ensure_restored_snapshot(
    operation: &str,
    target: &ModelSnapshot,
    restored: &ModelSnapshot,
    revision: u64,
) -> Result<()> {
    let certificate_names = restored
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.certificate_name.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut expected = target.clone();
    expected.revision = revision;
    for node in &mut expected.nodes {
        node.certificate_name = certificate_names.get(node.id.as_str()).cloned().flatten();
    }
    if expected == *restored {
        return Ok(());
    }

    let mut sections = Vec::new();
    if expected.revision != restored.revision {
        sections.push("revision");
    }
    if expected.overlay_cidr != restored.overlay_cidr {
        sections.push("overlay_cidr");
    }
    if expected.settings != restored.settings {
        sections.push("settings");
    }
    if expected.nodes != restored.nodes {
        sections.push("nodes");
    }
    if expected.node_egress_dns != restored.node_egress_dns {
        sections.push("node_egress_dns");
    }
    if expected.users != restored.users {
        sections.push("users");
    }
    if expected.external_outbounds != restored.external_outbounds {
        sections.push("external_outbounds");
    }
    if expected.apps != restored.apps {
        sections.push("apps");
    }
    Err(StoreError::InvalidData(format!(
        "{operation} restored revision {revision} with content differing from its target in {}",
        sections.join(", ")
    )))
}

async fn restore_settings_tx(
    tx: &mut Transaction<'_, Postgres>,
    snapshot: &ModelSnapshot,
) -> Result<()> {
    // Settings have one canonical complete writer. Calling it here instead of repeating its
    // column list is what keeps a newly added setting from being silently omitted by rollback.
    // The snapshot was produced by this same normalized path; if an old snapshot is no longer
    // valid, failing the rollback is safer than restoring a shape which never existed.
    settings::update_settings_tx(
        tx,
        &AdminContext::system_admin("system:rollback"),
        snapshot.settings.clone(),
    )
    .await?;

    // The overlay network predates ModelSettings and remains a top-level snapshot field.
    sqlx::query(
        "UPDATE control_state
         SET overlay_cidr = $1::cidr
         WHERE id = TRUE",
    )
    .bind(snapshot.overlay_cidr.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn restore_tenants_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
    snapshot: &ModelSnapshot,
) -> Result<()> {
    let revision_id = revision_to_i64(revision_id)?;
    for tenant_id in snapshot_tenant_ids(snapshot) {
        sqlx::query(
            "INSERT INTO tenants (id, name, created_revision)
             VALUES ($1, $1, $2)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&tenant_id)
        .bind(revision_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn restore_users_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
    snapshot: &ModelSnapshot,
) -> Result<()> {
    sqlx::query("UPDATE users SET status = 'disabled' WHERE status <> 'disabled'")
        .execute(&mut **tx)
        .await?;

    let revision_id = revision_to_i64(revision_id)?;
    for user in &snapshot.users {
        sqlx::query(
            "INSERT INTO users (tenant_id, id, uuid, created_revision, status)
             VALUES ($1, $2, $3::uuid, $4, 'active')
             ON CONFLICT (tenant_id, id) DO UPDATE SET
                uuid = EXCLUDED.uuid,
                status = 'active',
                created_revision = COALESCE(users.created_revision, EXCLUDED.created_revision)",
        )
        .bind(&user.tenant)
        .bind(&user.id)
        .bind(&user.uuid)
        .bind(revision_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn restore_nodes_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
    snapshot: &ModelSnapshot,
) -> Result<()> {
    let lifecycle_revision = revision_id;
    let node_ids = snapshot
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    sqlx::query(
        "UPDATE nodes
         SET retired_at = COALESCE(retired_at, now())
         WHERE NOT (id = ANY($1::text[]))
           AND retired_at IS NULL",
    )
    .bind(&node_ids)
    .execute(&mut **tx)
    .await?;

    let revision_id = revision_to_i64(revision_id)?;
    for node in &snapshot.nodes {
        restore_node_tx(tx, revision_id, node).await?;
    }
    let rows = sqlx::query(
        "SELECT n.id, n.retired_at IS NOT NULL AS retired, lifecycle.phase
           FROM nodes n
           JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = n.id
          ORDER BY n.id
          FOR UPDATE OF n, lifecycle",
    )
    .fetch_all(&mut **tx)
    .await?;
    for row in rows {
        let node_id: String = row.try_get("id")?;
        let retired: bool = row.try_get("retired")?;
        let phase: String = row.try_get("phase")?;
        let intent_changed = if retired {
            phase == "active"
        } else {
            phase != "active"
        };
        if intent_changed {
            crate::lifecycle::advance_intent_tx(
                tx,
                &node_id,
                retired,
                lifecycle_revision,
                "system:rollback",
                "restore node lifecycle from rollback snapshot",
            )
            .await?;
        }
    }
    Ok(())
}

async fn restore_node_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: i64,
    node: &Node,
) -> Result<()> {
    let (dns_kind, dns_servers) = dns_columns(&node.dns)?;
    let domain_strategy = domain_strategy_column(node.domain_strategy)?;
    let conn_secs = |value: Option<u32>| value.map(|v| i32::try_from(v).unwrap_or(i32::MAX));
    sqlx::query(
        "INSERT INTO nodes (
            id, tenant_id, name, public_ipv4, public_ipv6, overlay_addr,
            wg_private_key, wg_public_key, wg_listen_port,
            api_port, overlay, egress_allowed,
            dns_kind, dns_servers, created_revision, mtu, wg_transport,
            public_ipv4_nat, public_ipv6_nat, domain_strategy, retired_at,
            conn_idle_secs, conn_uplink_only_secs, conn_downlink_only_secs, conn_buffer_size_kb
         )
         VALUES (
            $1, $2, $3, $4, $5, $6::inet,
            $7, $8, $9,
            $10, $11, $12,
            $13, $14, $15, $16, $17,
            $18, $19, $21,
            CASE WHEN $20::boolean THEN now() ELSE NULL END,
            $22, $23, $24, $25
         )
         ON CONFLICT (id) DO UPDATE SET
            tenant_id = EXCLUDED.tenant_id,
            name = EXCLUDED.name,
            public_ipv4 = EXCLUDED.public_ipv4,
            public_ipv6 = EXCLUDED.public_ipv6,
            overlay_addr = EXCLUDED.overlay_addr,
            wg_private_key = EXCLUDED.wg_private_key,
            wg_public_key = EXCLUDED.wg_public_key,
            wg_listen_port = EXCLUDED.wg_listen_port,
            api_port = EXCLUDED.api_port,
            overlay = EXCLUDED.overlay,
            egress_allowed = EXCLUDED.egress_allowed,
            dns_kind = EXCLUDED.dns_kind,
            dns_servers = EXCLUDED.dns_servers,
            created_revision = COALESCE(nodes.created_revision, EXCLUDED.created_revision),
            mtu = EXCLUDED.mtu,
            wg_transport = EXCLUDED.wg_transport,
            public_ipv4_nat = EXCLUDED.public_ipv4_nat,
            public_ipv6_nat = EXCLUDED.public_ipv6_nat,
            domain_strategy = EXCLUDED.domain_strategy,
            conn_idle_secs = EXCLUDED.conn_idle_secs,
            conn_uplink_only_secs = EXCLUDED.conn_uplink_only_secs,
            conn_downlink_only_secs = EXCLUDED.conn_downlink_only_secs,
            conn_buffer_size_kb = EXCLUDED.conn_buffer_size_kb,
            retired_at = CASE WHEN $20::boolean THEN COALESCE(nodes.retired_at, now()) ELSE NULL END",
    )
    .bind(&node.id)
    .bind(&node.tenant)
    .bind(&node.name)
    .bind(&node.public_ipv4)
    .bind(&node.public_ipv6)
    .bind(node.overlay_addr.to_string())
    .bind(&node.wireguard.private_key)
    .bind(&node.wireguard.public_key)
    .bind(i32::from(node.wireguard.listen_port))
    .bind(node.api_port.map(i32::from))
    .bind(node.overlay)
    .bind(node.egress_allowed)
    .bind(dns_kind)
    .bind(dns_servers)
    .bind(revision_id)
    .bind(node.mtu.map(i32::from))
    .bind(serde_json::to_value(&node.wireguard.transport)?)
    .bind(node.public_ipv4_nat)
    .bind(node.public_ipv6_nat)
    .bind(node.retired)
    .bind(domain_strategy)
    .bind(conn_secs(node.connection.conn_idle_secs))
    .bind(conn_secs(node.connection.uplink_only_secs))
    .bind(conn_secs(node.connection.downlink_only_secs))
    .bind(conn_secs(node.connection.buffer_size_kb))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn load_warp_binding_tokens_tx(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<WarpBindingTokenMap> {
    let rows = sqlx::query(
        "SELECT external_outbounds.tenant_id, external_outbound_bindings.outbound_id,
                node_id, device_id, access_token_sealed
         FROM external_outbound_bindings
         JOIN external_outbounds ON external_outbounds.id = external_outbound_bindings.outbound_id",
    )
    .fetch_all(&mut **tx)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                (
                    row.try_get("tenant_id")?,
                    row.try_get("outbound_id")?,
                    row.try_get("node_id")?,
                    row.try_get("device_id")?,
                ),
                row.try_get("access_token_sealed")?,
            ))
        })
        .collect()
}

async fn restore_warp_bindings_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: i64,
    outbound: &ExternalOutbound,
    tokens: &WarpBindingTokenMap,
) -> Result<()> {
    for binding in &outbound.bindings {
        let token_key = (
            outbound.tenant.clone(),
            outbound.id.clone(),
            binding.node.clone(),
            binding.device_id.clone(),
        );
        let access_token_sealed = match tokens.get(&token_key) {
            Some(token) => token.clone(),
            None => crate::secrets::seal(
                &crate::secrets::external_outbound_binding_token_context(
                    &outbound.tenant,
                    &outbound.id,
                    &binding.node,
                ),
                &format!(
                    "rollback-control-token-unavailable:undefined:device={}",
                    binding.device_id
                ),
            )?,
        };
        let private_key_sealed = crate::secrets::seal(
            &crate::secrets::external_outbound_binding_key_context(
                &outbound.tenant,
                &outbound.id,
                &binding.node,
            ),
            &binding.private_key,
        )?;
        sqlx::query(
            "INSERT INTO external_outbound_bindings
                (outbound_id, node_id, device_id, account_id, access_token_sealed,
                 private_key_sealed, peer_public_key, local_addresses, reserved,
                 endpoint_address, endpoint_port, mtu, keep_alive, allowed_ips,
                 no_kernel_tun, domain_strategy, workers, registered_at, created_revision)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                     $13, $14, $15, $16, $17, $18::timestamptz, $19)",
        )
        .bind(&outbound.id)
        .bind(&binding.node)
        .bind(&binding.device_id)
        .bind(&binding.account_id)
        .bind(access_token_sealed)
        .bind(private_key_sealed)
        .bind(&binding.peer_public_key)
        .bind(serde_json::to_value(&binding.local_addresses)?)
        .bind(serde_json::to_value(&binding.reserved)?)
        .bind(&binding.endpoint_address)
        .bind(binding.endpoint_port.map(i32::from))
        .bind(binding.mtu.map(i32::from))
        .bind(binding.keep_alive.map(i32::from))
        .bind(
            binding
                .allowed_ips
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?,
        )
        .bind(binding.no_kernel_tun)
        .bind(&binding.domain_strategy)
        .bind(binding.workers.map(i32::from))
        .bind(&binding.registered_at)
        .bind(revision_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn clear_app_model_tx(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    for statement in [
        "DELETE FROM front_external_vias",
        "DELETE FROM front_vias",
        "DELETE FROM grants",
        "DELETE FROM steps",
        "DELETE FROM ingresses",
        "DELETE FROM fronts",
        "DELETE FROM chains",
        "DELETE FROM apps",
        "DELETE FROM external_outbounds",
    ] {
        sqlx::query(statement).execute(&mut **tx).await?;
    }
    Ok(())
}

async fn restore_apps_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
    snapshot: &ModelSnapshot,
    warp_binding_tokens: &WarpBindingTokenMap,
) -> Result<()> {
    // Tunnels are tenant resources, independent from projects. Restore them once before any app
    // front can recreate its references.
    for outbound in &snapshot.external_outbounds {
        console::upsert_external_outbound_tx(
            tx,
            &AdminContext::system_admin("system:rollback"),
            revision_id,
            console::UpsertExternalOutboundRequest {
                id: outbound.id.clone(),
                tenant_id: outbound.tenant.clone(),
                name: outbound.name.clone(),
                address: outbound.address.clone(),
                port: outbound.port,
                protocol: outbound.protocol.clone(),
                security: outbound.security.clone(),
                note: None,
            },
        )
        .await?;
        restore_warp_bindings_tx(
            tx,
            revision_to_i64(revision_id)?,
            outbound,
            warp_binding_tokens,
        )
        .await?;
    }
    for (position, app) in snapshot.apps.iter().enumerate() {
        restore_app_tx(
            tx,
            revision_id,
            u32::try_from(position)
                .map_err(|_| StoreError::InvalidData("app position out of range".to_owned()))?,
            app,
            &snapshot.settings.reality_site,
        )
        .await?;
    }
    Ok(())
}

async fn restore_app_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
    position: u32,
    app: &AppView,
    site: &RealitySite,
) -> Result<()> {
    let revision_id = revision_to_i64(revision_id)?;
    sqlx::query(
        "INSERT INTO apps (id, label, position, created_revision)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(&app.id)
    .bind(&app.label)
    .bind(
        i32::try_from(position).map_err(|_| {
            StoreError::InvalidData(format!("app {} position out of range", app.id))
        })?,
    )
    .bind(revision_id)
    .execute(&mut **tx)
    .await?;

    for (chain_position, chain) in app.chains.iter().enumerate() {
        sqlx::query(
            "INSERT INTO chains
                (id, app_id, tenant_id, name, subscription_country, position, created_revision)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&chain.id)
        .bind(&app.id)
        .bind(&chain.tenant)
        .bind(&chain.name)
        .bind(&chain.subscription_country)
        .bind(i32::try_from(chain_position).map_err(|_| {
            StoreError::InvalidData(format!("chain {} position out of range", chain.id))
        })?)
        .bind(revision_id)
        .execute(&mut **tx)
        .await?;
    }

    for front in &app.fronts {
        sqlx::query(
            "INSERT INTO fronts (id, app_id, tenant_id, name, strategy, created_revision)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&front.id)
        .bind(&app.id)
        .bind(&front.tenant)
        .bind(&front.name)
        .bind(front.strategy.as_str())
        .bind(revision_id)
        .execute(&mut **tx)
        .await?;
    }

    for ingress in &app.ingresses {
        // Every column the compiler later reads has to be written here, because a rollback
        // deletes the rows first. Anything left out silently reverts to its default — which is
        // how a rollback used to turn an XHTTP ingress back into a TCP one and drop projections
        // on the floor.
        //
        // Identity belongs to the ingress, not to its current transport. Restoring it verbatim
        // keeps the public and private halves paired across REALITY/TLS switches and rollbacks.
        let identity = &ingress.identity;
        let RealityOverrides {
            dest,
            server_names,
            flow,
            fallback_mode,
            fallback_limits,
            fallback_guard,
        } = ingress_reality_override_columns(ingress.wires.reality(), site, &ingress.wires);
        let xhttp = ingress.wires.xhttp();
        let xhttp_tuning = xhttp
            .and_then(|xhttp| xhttp.tuning.as_ref())
            .map(serde_json::to_value)
            .transpose()?;
        let xhttp_download = restored_xhttp_download(ingress);
        let reality_split = matches!(ingress.wires.vless(), Some(Transport::VlessRealityXhttp(_)));
        let xhttp_download_v4 = xhttp_download
            .as_ref()
            .and_then(|download| download.v4.as_ref())
            .map(client_download_json)
            .transpose()?;
        let xhttp_download_v6 = xhttp_download
            .as_ref()
            .and_then(|download| download.v6.as_ref())
            .map(client_download_json)
            .transpose()?;
        let xhttp_download_v4_origin_port = reality_split
            .then(|| {
                xhttp_download
                    .as_ref()
                    .and_then(|download| download.v4.as_ref())
                    .and_then(|download| download.origin_port)
                    .map(i32::from)
            })
            .flatten();
        let xhttp_download_v6_origin_port = reality_split
            .then(|| {
                xhttp_download
                    .as_ref()
                    .and_then(|download| download.v6.as_ref())
                    .and_then(|download| download.origin_port)
                    .map(i32::from)
            })
            .flatten();
        let projection = projection_without_download(&ingress.projection);
        let hysteria2 = ingress.wires.hysteria2();
        let quic = hysteria2.map(|h| h.quic).unwrap_or_default();
        let (hy2_masquerade_kind, hy2_masquerade_url) = match hysteria2.map(|h| &h.masquerade) {
            Some(HysteriaMasquerade::Proxy { url }) => ("proxy", Some(url.clone())),
            _ => ("not-found", None),
        };
        let anytls = ingress.wires.anytls();
        let anytls_padding_scheme = anytls
            .map(|settings| serde_json::to_value(&settings.padding_scheme))
            .transpose()?;
        let (
            anytls_masquerade_kind,
            anytls_masquerade_content,
            anytls_masquerade_headers,
            anytls_masquerade_status_code,
        ) = match anytls.map(|settings| &settings.masquerade) {
            Some(AnyTlsMasquerade::String {
                content,
                headers,
                status_code,
            }) => (
                "string",
                Some(content.clone()),
                Some(serde_json::to_value(headers)?),
                Some(i32::from(*status_code)),
            ),
            Some(AnyTlsMasquerade::NotFound { headers }) if !headers.is_empty() => {
                ("404", None, Some(serde_json::to_value(headers)?), None)
            }
            _ => ("404", None, None, None),
        };
        sqlx::query(
            "INSERT INTO ingresses (
                id, app_id, chain_id, node_id, bind, port, front_id,
                reality_private_key, reality_public_key, reality_short_ids,
                reality_dest, reality_server_names, reality_flow,
                reality_fallback_mode, reality_fallback_limits,
                reality_fallback_guard,
                transport_kind, hy2_enabled, xhttp_path, xhttp_mode,
                hy2_port, hy2_hop_start, hy2_hop_end,
                hy2_up, hy2_down, hy2_congestion, hy2_obfs_password,
                hy2_masquerade_kind, hy2_masquerade_url,
                projection_v4_host, projection_v4_port,
                projection_v4_download_host, projection_v4_download_port,
                projection_v4_download_origin_port,
                projection_v4_download_http_host, projection_v4_download_mux,
                projection_v6_host, projection_v6_port,
                projection_v6_download_host, projection_v6_download_port,
                projection_v6_download_origin_port,
                projection_v6_download_http_host, projection_v6_download_mux,
                guard_no_private, guard_no_bittorrent, guard_no_mail,
                guard_no_udp_amplification, guard_tcp_and_quic_only,
                created_revision,
                hy2_bbr_profile,
                hy2_quic_init_stream_window, hy2_quic_max_stream_window,
                hy2_quic_init_conn_window, hy2_quic_max_conn_window,
                hy2_quic_max_idle_secs, hy2_quic_keepalive_secs,
                hy2_quic_max_incoming_streams, hy2_quic_disable_pmtud,
                xhttp_tuning,
                xhttp_download_v4_origin_port, xhttp_download_v6_origin_port,
                anytls_enabled, anytls_port, anytls_padding_scheme,
                anytls_masquerade_kind, anytls_masquerade_content,
                anytls_masquerade_headers, anytls_masquerade_status_code
             )
             VALUES (
                $1, $2, $3, $4, $5::inet, $6, $7,
                $8, $9, $10,
                $11, $12, $13,
                $14, $15,
                $16,
                $17, $18, $19, $20,
                $21, $22, $23,
                $24, $25, $26, $27, $28, $29,
                $30, $31, $32, $33, $34, $35, $36,
                $37, $38, $39, $40, $41, $42, $43,
                $44, $45, $46, $47, $48,
                $49,
                $50,
                $51, $52, $53, $54,
                $55, $56, $57, $58,
                $59, $60, $61,
                $62, $63, $64, $65, $66, $67, $68
             )",
        )
        .bind(&ingress.id)
        .bind(&app.id)
        .bind(&ingress.chain)
        .bind(&ingress.node)
        .bind(ingress.bind.to_string())
        .bind(i32::from(ingress.port))
        .bind(&ingress.front)
        .bind(&identity.private_key)
        .bind(&identity.public_key)
        .bind(serde_json::to_value(&identity.short_ids)?)
        .bind(dest)
        .bind(serde_json::to_value(&server_names)?)
        .bind(flow)
        .bind(fallback_mode)
        .bind(serde_json::to_value(fallback_limits)?)
        .bind(fallback_guard)
        .bind(ingress.wires.vless_kind())
        .bind(ingress.wires.has_udp())
        .bind(xhttp.map(|xhttp| xhttp.path.clone()))
        .bind(xhttp.and_then(|xhttp| xhttp.mode.as_str()))
        .bind(hysteria2.map(|h| i32::from(h.port)))
        .bind(hysteria2.and_then(|h| h.hop.map(|hop| i32::from(hop.start))))
        .bind(hysteria2.and_then(|h| h.hop.map(|hop| i32::from(hop.end))))
        .bind(hysteria2.and_then(|h| h.bandwidth.up.clone()))
        .bind(hysteria2.and_then(|h| h.bandwidth.down.clone()))
        .bind(
            hysteria2
                .map(|h| h.congestion)
                .unwrap_or(HysteriaCongestion::Brutal)
                .as_str(),
        )
        .bind(hysteria2.and_then(|h| {
            h.obfs.as_ref().map(|obfs| match obfs {
                HysteriaObfs::Salamander { password } => password.clone(),
            })
        }))
        .bind(hy2_masquerade_kind)
        .bind(hy2_masquerade_url)
        .bind(projection.v4.as_ref().map(|to| to.host.clone()))
        .bind(projection.v4.as_ref().map(|to| i32::from(to.port)))
        .bind(None::<String>)
        .bind(None::<i32>)
        .bind(None::<i32>)
        .bind(None::<String>)
        .bind(None::<i32>)
        .bind(projection.v6.as_ref().map(|to| to.host.clone()))
        .bind(projection.v6.as_ref().map(|to| i32::from(to.port)))
        .bind(None::<String>)
        .bind(None::<i32>)
        .bind(None::<i32>)
        .bind(None::<String>)
        .bind(None::<i32>)
        .bind(ingress.guard.no_private)
        .bind(ingress.guard.no_bittorrent)
        .bind(ingress.guard.no_mail)
        .bind(ingress.guard.no_udp_amplification)
        .bind(ingress.guard.tcp_and_quic_only)
        .bind(revision_id)
        /* 跟 console.rs 的那条 INSERT 同样的排法：新列接在 created_revision 后面，
        前面 53 个占位符的编号一个都不动。回滚写的是快照里已经过校验的值，
        窗口能装进 BIGINT——它当初就是从 BIGINT 读出来的。 */
        .bind(
            hysteria2
                .map(|h| h.bbr_profile)
                .unwrap_or_default()
                .as_str(),
        )
        .bind(quic.init_stream_receive_window.map(|v| v as i64))
        .bind(quic.max_stream_receive_window.map(|v| v as i64))
        .bind(quic.init_connection_receive_window.map(|v| v as i64))
        .bind(quic.max_connection_receive_window.map(|v| v as i64))
        .bind(quic.max_idle_timeout_secs.map(i64::from))
        .bind(quic.keep_alive_period_secs.map(i64::from))
        .bind(quic.max_incoming_streams.map(i64::from))
        .bind(quic.disable_path_mtu_discovery)
        .bind(xhttp_tuning)
        .bind(xhttp_download_v4_origin_port)
        .bind(xhttp_download_v6_origin_port)
        .bind(anytls.is_some())
        .bind(anytls.map(|settings| i32::from(settings.port)))
        .bind(anytls_padding_scheme.unwrap_or_else(|| json!([])))
        .bind(anytls_masquerade_kind)
        .bind(anytls_masquerade_content.unwrap_or_default())
        .bind(anytls_masquerade_headers.unwrap_or_else(|| json!({})))
        .bind(anytls_masquerade_status_code.unwrap_or(200))
        .execute(&mut **tx)
        .await?;

        let reality_fingerprint = ingress.wires.reality().and_then(|reality| {
            let global = site.fingerprint.as_deref().unwrap_or("chrome");
            (reality.fingerprint != global).then(|| reality.fingerprint.clone())
        });
        sqlx::query(
            "INSERT INTO ingress_client_settings (
                 ingress_id, reality_fingerprint, xhttp_host, xhttp_xmux,
                 xhttp_download_v4, xhttp_download_v6,
                 anytls_idle_session_check_interval, anytls_idle_session_timeout,
                 anytls_min_idle_session
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (ingress_id) DO UPDATE SET
                 reality_fingerprint = EXCLUDED.reality_fingerprint,
                 xhttp_host = EXCLUDED.xhttp_host,
                 xhttp_xmux = EXCLUDED.xhttp_xmux,
                 xhttp_download_v4 = EXCLUDED.xhttp_download_v4,
                 xhttp_download_v6 = EXCLUDED.xhttp_download_v6,
                 anytls_idle_session_check_interval = EXCLUDED.anytls_idle_session_check_interval,
                 anytls_idle_session_timeout = EXCLUDED.anytls_idle_session_timeout,
                 anytls_min_idle_session = EXCLUDED.anytls_min_idle_session,
                 updated_at = now()",
        )
        .bind(&ingress.id)
        .bind(reality_fingerprint)
        .bind(xhttp.and_then(|xhttp| xhttp.host.clone()))
        .bind(
            xhttp
                .and_then(|xhttp| xhttp.xmux.as_ref())
                .map(serde_json::to_value)
                .transpose()?,
        )
        .bind(xhttp_download_v4)
        .bind(xhttp_download_v6)
        .bind(
            anytls
                .and_then(|settings| settings.idle_session_check_interval_secs)
                .map(i64::from),
        )
        .bind(
            anytls
                .and_then(|settings| settings.idle_session_timeout_secs)
                .map(i64::from),
        )
        .bind(
            anytls
                .and_then(|settings| settings.min_idle_session)
                .map(i64::from),
        )
        .execute(&mut **tx)
        .await?;
    }

    for front in &app.fronts {
        for (ordinal, ingress_id) in front.via.iter().enumerate() {
            sqlx::query(
                "INSERT INTO front_vias (front_id, ingress_id, ordinal)
                 VALUES ($1, $2, $3)",
            )
            .bind(&front.id)
            .bind(ingress_id)
            .bind(i32::try_from(ordinal).map_err(|_| {
                StoreError::InvalidData(format!("front {} via list is too long", front.id))
            })?)
            .execute(&mut **tx)
            .await?;
        }
        for (ordinal, outbound_id) in front.external_via.iter().enumerate() {
            sqlx::query(
                "INSERT INTO front_external_vias (front_id, outbound_id, ordinal)
                 VALUES ($1, $2, $3)",
            )
            .bind(&front.id)
            .bind(outbound_id)
            .bind(i32::try_from(ordinal).map_err(|_| {
                StoreError::InvalidData(format!(
                    "front {} external tunnel list is too long",
                    front.id
                ))
            })?)
            .execute(&mut **tx)
            .await?;
        }
    }

    for step in &app.steps {
        let hop_security = step
            .hop_in
            .as_ref()
            .map(|hop_in| serde_json::to_value(&hop_in.security))
            .transpose()?;
        sqlx::query(
            "INSERT INTO steps (
                chain_id, node_id, accept_uuid, accept_label, rules,
                hop_in_port, hop_in_wire, created_revision
             )
             VALUES ($1, $2, $3::uuid, $4, $5, $6, $7, $8)",
        )
        .bind(&step.chain)
        .bind(&step.node)
        .bind(step.accept.as_ref().map(|accept| accept.uuid.as_str()))
        .bind(step.accept.as_ref().map(|accept| accept.label.as_str()))
        .bind(serde_json::to_value(&step.rules)?)
        .bind(step.hop_in.as_ref().map(|hop_in| i32::from(hop_in.port)))
        .bind(hop_security)
        .bind(revision_id)
        .execute(&mut **tx)
        .await?;
    }

    for grant in &app.grants {
        sqlx::query(
            "INSERT INTO grants (app_id, tenant_id, user_id, ingress_id, created_revision)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&app.id)
        .bind(&grant.tenant)
        .bind(&grant.user)
        .bind(&grant.ingress)
        .bind(revision_id)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

fn snapshot_tenant_ids(snapshot: &ModelSnapshot) -> BTreeSet<String> {
    let mut tenants = BTreeSet::new();
    tenants.extend(snapshot.nodes.iter().map(|node| node.tenant.clone()));
    tenants.extend(snapshot.users.iter().map(|user| user.tenant.clone()));
    tenants.extend(
        snapshot
            .external_outbounds
            .iter()
            .map(|outbound| outbound.tenant.clone()),
    );
    for app in &snapshot.apps {
        tenants.extend(app.chains.iter().map(|chain| chain.tenant.clone()));
        tenants.extend(app.fronts.iter().map(|front| front.tenant.clone()));
        tenants.extend(app.grants.iter().map(|grant| grant.tenant.clone()));
    }
    tenants
}

fn dns_columns(dns: &Dns) -> Result<(&'static str, Value)> {
    match dns {
        Dns::System => Ok(("system", json!([]))),
        Dns::Servers(servers) => Ok(("servers", serde_json::to_value(servers)?)),
    }
}

/// See the note on the column in `migrations/0001_init.sql`: the stored text is
/// `DomainStrategy`'s serde spelling, so serde does the encoding rather than a match that
/// would have to be remembered alongside the CHECK constraint.
fn domain_strategy_column(strategy: DomainStrategy) -> Result<String> {
    match serde_json::to_value(strategy)? {
        Value::String(value) => Ok(value),
        other => Err(crate::StoreError::InvalidData(format!(
            "domain strategy did not serialize to a string: {other}"
        ))),
    }
}

/// Which of the borrowed-site columns this ingress actually overrides.
///
/// A shape holding its own certificate borrows no site, so all four fold back to NULL except the
/// two that were never REALITY's to begin with — the ClientHello to imitate, and whether to run
/// flow control. Both are read back for every shape, so both have to be written for every shape.
struct RealityOverrides {
    dest: Option<String>,
    server_names: Vec<String>,
    flow: Option<String>,
    fallback_mode: &'static str,
    fallback_limits: RealityFallbackLimits,
    fallback_guard: bool,
}

/// Encode a resolved model flow into the `reality_flow` column's three states.
///
/// `ingress_from_row_with_site` (materialize.rs) reads that column as three states: NULL follows
/// the fleet, the empty string is Vision explicitly off, a value is that value. Inheritance is
/// already resolved in the model, so a `Some(x)` equal to the site and a plain "inherit" are the
/// same thing and both write NULL. The one that must not collapse to NULL is `None`: it means
/// Vision is off on this ingress, and NULL would read back as "follow the fleet" and turn it on —
/// which is every XHTTP ingress, since XHTTP cannot carry Vision. That state is the empty string,
/// not NULL. Writing NULL here is what made a rollback silently re-enable Vision on those
/// ingresses, and what `ensure_restored_snapshot` refuses to commit.
fn flow_column(flow: Option<String>, site_flow: &Option<String>) -> Option<String> {
    if *site_flow == flow {
        None
    } else if flow.is_none() {
        Some(String::new())
    } else {
        flow
    }
}

fn restored_xhttp_download(ingress: &Ingress) -> Option<XhttpDownload> {
    let xhttp = ingress.wires.xhttp()?;
    if let Some(download) = &xhttp.download {
        return Some(download.clone());
    }
    let v4 = ingress
        .projection
        .v4
        .as_ref()
        .and_then(|endpoint| endpoint.download.clone());
    let v6 = ingress
        .projection
        .v6
        .as_ref()
        .and_then(|endpoint| endpoint.download.clone());
    (v4.is_some() || v6.is_some()).then_some(XhttpDownload { v4, v6 })
}

fn projection_without_download(projection: &Projection) -> Projection {
    Projection {
        v4: projection.v4.as_ref().map(|endpoint| ProjectionEndpoint {
            host: endpoint.host.clone(),
            port: endpoint.port,
            download: None,
        }),
        v6: projection.v6.as_ref().map(|endpoint| ProjectionEndpoint {
            host: endpoint.host.clone(),
            port: endpoint.port,
            download: None,
        }),
    }
}

fn client_download_json(download: &ProjectionDownloadEndpoint) -> Result<Value> {
    Ok(serde_json::to_value(ClientProjectionDownloadEndpoint {
        host: download.host.clone(),
        port: download.port,
        http_host: download.http_host.clone(),
        mux: download.mux,
    })?)
}

fn ingress_reality_override_columns(
    reality: Option<&RealitySettings>,
    site: &RealitySite,
    wires: &IngressWires,
) -> RealityOverrides {
    // The guard flag on the shapes with no REALITY of their own is the column default rather than
    // a decision: nothing reads it back until a REALITY wire is put on the ingress, and that write
    // carries its own value.
    if !wires.has_tcp() {
        return RealityOverrides {
            dest: None,
            server_names: Vec::new(),
            flow: None,
            fallback_mode: "global-site",
            fallback_limits: RealityFallbackLimits::Off,
            fallback_guard: true,
        };
    }
    let Some(reality) = reality else {
        let flow = flow_column(wires.flow().map(str::to_owned), &site.flow);
        return RealityOverrides {
            dest: None,
            server_names: Vec::new(),
            flow,
            fallback_mode: "global-site",
            fallback_limits: RealityFallbackLimits::Off,
            fallback_guard: true,
        };
    };
    let (dest, server_names) = match reality.fallback_mode {
        RealityFallbackMode::GlobalSite | RealityFallbackMode::NodeCertificate => {
            (None, Vec::new())
        }
        RealityFallbackMode::CustomSite => {
            (Some(reality.dest.clone()), reality.server_names.clone())
        }
    };
    // Three states, not two: equal to the site folds back to following the global setting (NULL),
    // an explicit value stays itself, and `None` while the site has a flow is Vision off on this
    // ingress and has to be the empty string — see `flow_column`.
    let flow = flow_column(reality.flow.clone(), &site.flow);
    RealityOverrides {
        dest,
        server_names,
        flow,
        fallback_mode: match reality.fallback_mode {
            RealityFallbackMode::GlobalSite => "global-site",
            RealityFallbackMode::NodeCertificate => "node-certificate",
            RealityFallbackMode::CustomSite => "custom-site",
        },
        fallback_limits: reality.fallback_limits.clone(),
        fallback_guard: reality.fallback_guard,
    }
}

async fn insert_rollback_deployment_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor_id: &str,
    idempotency_key: &str,
    note: &str,
    target_deployment_id: i64,
    plan: &DeploymentPlan,
) -> Result<(i64, String)> {
    let has_pending = plan
        .targets
        .iter()
        .any(|target| target.status == PlannedTargetStatus::Pending);
    let status = if has_pending { "planned" } else { "succeeded" };
    let active = has_pending.then_some(true);
    let warnings = serde_json::to_value(&plan.warnings)?;
    let revision_id = revision_to_i64(plan.revision)?;
    // A rollback deployment writes no kind, taking the default config, and its baseline is
    // computed on the configuration line: both express which version currently runs on the
    // machines, and that version is exactly what a rollback changes.
    let base_revision_id = last_succeeded_revision(&mut **tx, DeploymentKind::Config).await?;

    let row = sqlx::query(
        "INSERT INTO deployments (
            revision_id, status, actor, idempotency_key, active, warnings, note,
            rollback_of_deployment_id, base_revision_id, finished_at
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9,
            CASE WHEN $5::boolean IS TRUE THEN NULL ELSE now() END
         )
         RETURNING id",
    )
    .bind(revision_id)
    .bind(status)
    .bind(actor_id)
    .bind(idempotency_key)
    .bind(active)
    .bind(warnings)
    .bind(note)
    .bind(target_deployment_id)
    .bind(base_revision_id)
    .fetch_one(&mut **tx)
    .await?;
    let deployment_id = row.try_get("id")?;

    for target in &plan.targets {
        insert_target(tx, deployment_id, target).await?;
    }

    if status == "succeeded" {
        refresh_deployment_status(tx, deployment_id, "deferred").await?;
    }

    Ok((deployment_id, status.to_owned()))
}

fn mark_applied_states_dirty_with_reason(
    applied: &mut Vec<NodeAppliedState>,
    node_ids: &[String],
    reason: String,
) {
    let mut missing = node_ids.iter().cloned().collect::<BTreeSet<_>>();
    for state in applied.iter_mut() {
        if missing.remove(&state.node_id) {
            let node_id = state.node_id.clone();
            *state = dirty_applied_state(node_id, reason.clone());
        }
    }
    for node_id in missing {
        applied.push(dirty_applied_state(node_id, reason.clone()));
    }
}

async fn mark_uncertain_cancel_targets_dirty(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
    node_ids: &[String],
) -> Result<()> {
    // Cancelling a deployment already dispatched leaves those machines in a state where how far
    // they got is unknown, and they must be marked dirty. But the marking must match what the
    // deployment governs: a grants deployment can at most have half-changed a list, and it never
    // touched the three configuration artifacts at all — marking those dirty too conjures a
    // configuration drift that never happened.
    let kind: String = sqlx::query_scalar("SELECT kind FROM deployments WHERE id = $1")
        .bind(deployment_id)
        .fetch_one(&mut **tx)
        .await?;
    let kind = DeploymentKind::parse(&kind).unwrap_or_default();
    for node_id in node_ids {
        let reason = cancel_uncertain_target_dirty_reason(deployment_id);
        let state = match kind {
            DeploymentKind::Config => dirty_reported_state(reason),
            DeploymentKind::Grants => ReportedNodeState {
                phantun: AppliedArtifactState::Unmanaged,
                wireguard: AppliedArtifactState::Unmanaged,
                xray: AppliedArtifactState::Unmanaged,
                hy2_port_hop: AppliedArtifactState::Unmanaged,
                grants: AppliedGrantsState::Dirty { reason },
            },
        };
        upsert_node_applied_state(tx, deployment_id, node_id, &state, kind).await?;
    }
    Ok(())
}

fn dirty_applied_state(node_id: String, reason: String) -> NodeAppliedState {
    NodeAppliedState {
        node_id,
        // A machine reporting dirt is one whose config the control plane cannot vouch for.
        running_xray: None,
        phantun: AppliedArtifactState::Dirty {
            reason: reason.clone(),
        },
        wireguard: AppliedArtifactState::Dirty {
            reason: reason.clone(),
        },
        xray: AppliedArtifactState::Dirty {
            reason: reason.clone(),
        },
        hy2_port_hop: AppliedArtifactState::Dirty {
            reason: reason.clone(),
        },
        grants: AppliedGrantsState::Dirty { reason },
    }
}

fn dirty_reported_state(reason: String) -> ReportedNodeState {
    ReportedNodeState {
        phantun: AppliedArtifactState::Dirty {
            reason: reason.clone(),
        },
        wireguard: AppliedArtifactState::Dirty {
            reason: reason.clone(),
        },
        xray: AppliedArtifactState::Dirty {
            reason: reason.clone(),
        },
        hy2_port_hop: AppliedArtifactState::Dirty {
            reason: reason.clone(),
        },
        grants: AppliedGrantsState::Dirty { reason },
    }
}

fn cancel_uncertain_target_dirty_reason(deployment_id: i64) -> String {
    format!("deployment {deployment_id} was canceled after the target was dispatched")
}

async fn ensure_deployment_write_access(
    pool: &PgPool,
    actor: &AdminContext,
    deployment_id: i64,
) -> Result<()> {
    if actor.is_system_admin() {
        return Ok(());
    }
    let tenant_scope = require_actor_tenant_scope(actor)?;
    let tenant_pattern = actor
        .tenant_scope_like_pattern()
        .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))?;
    let row = sqlx::query(
        "SELECT count(dt.node_id) AS total_targets,
                count(dt.node_id) FILTER (
                    WHERE n.tenant_id = $2 OR n.tenant_id LIKE $3 ESCAPE '\\'
                ) AS visible_targets,
                count(dt.node_id) FILTER (
                    WHERE dt.status <> 'skipped'
                      AND NOT (n.tenant_id = $2 OR n.tenant_id LIKE $3 ESCAPE '\\')
                ) AS outside_changed_targets
         FROM deployments d
         LEFT JOIN deployment_targets dt
           ON dt.deployment_id = d.id
         LEFT JOIN nodes n
           ON n.id = dt.node_id
         WHERE d.id = $1
         GROUP BY d.id",
    )
    .bind(deployment_id)
    .bind(tenant_scope)
    .bind(tenant_pattern)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("deployment {deployment_id}")))?;

    let visible_targets: i64 = row.try_get("visible_targets")?;
    if visible_targets == 0 {
        return Err(StoreError::NotFound(format!("deployment {deployment_id}")));
    }
    let outside_changed_targets: i64 = row.try_get("outside_changed_targets")?;
    if outside_changed_targets > 0 {
        return Err(StoreError::Forbidden(format!(
            "deployment {deployment_id} has targets outside tenant scope"
        )));
    }
    Ok(())
}

async fn ensure_node_access(pool: &PgPool, actor: &AdminContext, node_id: &str) -> Result<()> {
    if actor.is_system_admin() {
        return Ok(());
    }
    let row = sqlx::query(
        "SELECT tenant_id
         FROM nodes
         WHERE id = $1",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
    let tenant_id: String = row.try_get("tenant_id")?;
    actor.require_tenant_access(&tenant_id, "node")
}

fn require_actor_tenant_scope(actor: &AdminContext) -> Result<&str> {
    actor
        .tenant_scope()
        .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))
}

async fn attach_known_artifact_content(
    pool: &PgPool,
    targets: &mut [DeploymentTargetDetail],
) -> Result<()> {
    let mut shas = Vec::new();
    for target in targets.iter() {
        collect_content_shas(&target.desired_structure, &mut shas);
        if let Some(observed) = &target.observed_before {
            collect_content_shas(observed, &mut shas);
        }
        if let Some(observed) = &target.observed_after {
            collect_content_shas(observed, &mut shas);
        }
    }
    shas.sort();
    shas.dedup();
    if shas.is_empty() {
        return Ok(());
    }

    let rows = sqlx::query(
        "SELECT sha256, content, byte_len
         FROM artifact_blobs
         WHERE sha256 = ANY($1)",
    )
    .bind(&shas)
    .fetch_all(pool)
    .await?;
    let mut content_by_sha = BTreeMap::new();
    for row in rows {
        let sha256: String = row.try_get("sha256")?;
        content_by_sha.insert(
            sha256,
            (
                row.try_get::<String, _>("content")?,
                row.try_get::<i32, _>("byte_len")?,
            ),
        );
    }

    for target in targets {
        attach_content_to_value(&mut target.desired_structure, &content_by_sha);
        if let Some(observed) = &mut target.observed_before {
            attach_content_to_value(observed, &content_by_sha);
        }
        if let Some(observed) = &mut target.observed_after {
            attach_content_to_value(observed, &content_by_sha);
        }
    }

    Ok(())
}

fn collect_content_shas(value: &Value, shas: &mut Vec<String>) {
    collect_artifact_sha(value.get("phantun"), shas);
    collect_artifact_sha(value.get("wireguard"), shas);
    collect_artifact_sha(value.get("xray"), shas);
}

fn collect_artifact_sha(value: Option<&Value>, shas: &mut Vec<String>) {
    let Some(value) = value else {
        return;
    };
    if value.get("state").and_then(Value::as_str) != Some("present") {
        return;
    }
    if let Some(sha256) = value.get("sha256").and_then(Value::as_str) {
        shas.push(sha256.to_owned());
    }
}

fn attach_content_to_value(value: &mut Value, content_by_sha: &BTreeMap<String, (String, i32)>) {
    attach_artifact_content(value.get_mut("phantun"), content_by_sha);
    attach_artifact_content(value.get_mut("wireguard"), content_by_sha);
    attach_artifact_content(value.get_mut("xray"), content_by_sha);
}

fn attach_artifact_content(
    value: Option<&mut Value>,
    content_by_sha: &BTreeMap<String, (String, i32)>,
) {
    let Some(Value::Object(object)) = value else {
        return;
    };
    if object.get("state").and_then(Value::as_str) != Some("present") {
        return;
    }
    let Some(sha256) = object
        .get("sha256")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    let Some((content, byte_len)) = content_by_sha.get(&sha256) else {
        return;
    };
    object.insert("content".to_owned(), json!(content));
    object.insert("content_byte_len".to_owned(), json!(byte_len));
}

async fn load_snapshot_for_deployment(
    pool: &PgPool,
    revision_id: u64,
) -> Result<brocade_core::model::ModelSnapshot> {
    materialize::load_snapshot(pool, Some(revision_id)).await
}

async fn load_applied_states<'e, E>(executor: E) -> Result<Vec<NodeAppliedState>>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query(
        "SELECT node_id,
            phantun_state, phantun_sha256, phantun_observed,
            hy2_port_hop_state, hy2_port_hop_sha256, hy2_port_hop_observed,
            wireguard_state, wireguard_sha256, wireguard_observed,
            xray_state, xray_sha256, xray_observed,
            grants_state, grants_observed
         FROM node_applied_state
         ORDER BY node_id",
    )
    .fetch_all(executor)
    .await?;

    rows.iter()
        .map(|row| {
            Ok(NodeAppliedState {
                node_id: row.try_get("node_id")?,
                // Filled in by `attach_running_xray`: the table stores digests, and what is
                // wanted here is the text behind one of them.
                running_xray: None,
                phantun: parse_artifact_state(
                    "phantun",
                    row.try_get("phantun_state")?,
                    row.try_get("phantun_sha256")?,
                    // This column was added later and existing rows hold NULL — only the dirty
                    // variant really reads it, and the others can do without. An Option absorbs
                    // it, or the whole table fails to read.
                    row.try_get::<Option<Value>, _>("phantun_observed")?
                        .unwrap_or(Value::Null),
                )?,
                wireguard: parse_artifact_state(
                    "wireguard",
                    row.try_get("wireguard_state")?,
                    row.try_get("wireguard_sha256")?,
                    row.try_get("wireguard_observed")?,
                )?,
                xray: parse_artifact_state(
                    "xray",
                    row.try_get("xray_state")?,
                    row.try_get("xray_sha256")?,
                    row.try_get("xray_observed")?,
                )?,
                hy2_port_hop: parse_artifact_state(
                    "hy2_port_hop",
                    row.try_get("hy2_port_hop_state")?,
                    row.try_get("hy2_port_hop_sha256")?,
                    row.try_get::<Option<Value>, _>("hy2_port_hop_observed")?
                        .unwrap_or(Value::Null),
                )?,
                grants: parse_grants_state(
                    row.try_get("grants_state")?,
                    row.try_get("grants_observed")?,
                )?,
            })
        })
        .collect()
}

async fn load_node_applied_state_for_update(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
) -> Result<Option<ReportedNodeState>> {
    let row = sqlx::query(
        "SELECT hy2_port_hop_state, hy2_port_hop_sha256, hy2_port_hop_observed,
                phantun_state, phantun_sha256, phantun_observed,
                wireguard_state, wireguard_sha256, wireguard_observed,
            xray_state, xray_sha256, xray_observed,
            grants_state, grants_observed
         FROM node_applied_state
         WHERE node_id = $1
         FOR UPDATE",
    )
    .bind(node_id)
    .fetch_optional(&mut **tx)
    .await?;

    row.map(|row| {
        Ok(ReportedNodeState {
            phantun: parse_artifact_state(
                "phantun",
                row.try_get("phantun_state")?,
                row.try_get("phantun_sha256")?,
                // This column was added later and existing rows hold NULL — only the dirty
                // variant really reads it, and the others can do without. An Option absorbs it,
                // or the whole table fails to read.
                row.try_get::<Option<Value>, _>("phantun_observed")?
                    .unwrap_or(Value::Null),
            )?,
            wireguard: parse_artifact_state(
                "wireguard",
                row.try_get("wireguard_state")?,
                row.try_get("wireguard_sha256")?,
                row.try_get("wireguard_observed")?,
            )?,
            xray: parse_artifact_state(
                "xray",
                row.try_get("xray_state")?,
                row.try_get("xray_sha256")?,
                row.try_get("xray_observed")?,
            )?,
            hy2_port_hop: parse_artifact_state(
                "hy2_port_hop",
                row.try_get("hy2_port_hop_state")?,
                row.try_get("hy2_port_hop_sha256")?,
                row.try_get::<Option<Value>, _>("hy2_port_hop_observed")?
                    .unwrap_or(Value::Null),
            )?,
            grants: parse_grants_state(
                row.try_get("grants_state")?,
                row.try_get("grants_observed")?,
            )?,
        })
    })
    .transpose()
}

async fn desired_deployment_from_structure(
    pool: &PgPool,
    deployment_id: i64,
    node_id: String,
    wave: i32,
    desired_structure: Value,
    grants: DesiredGrants,
) -> Result<NodeDesiredDeployment> {
    let mut connection = pool.acquire().await?;
    desired_deployment_from_structure_connection(
        &mut connection,
        deployment_id,
        node_id,
        wave,
        desired_structure,
        grants,
    )
    .await
}

async fn desired_deployment_from_structure_tx(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
    node_id: String,
    wave: i32,
    desired_structure: Value,
    grants: DesiredGrants,
) -> Result<NodeDesiredDeployment> {
    desired_deployment_from_structure_connection(
        tx,
        deployment_id,
        node_id,
        wave,
        desired_structure,
        grants,
    )
    .await
}

async fn desired_deployment_from_structure_connection(
    connection: &mut PgConnection,
    deployment_id: i64,
    node_id: String,
    wave: i32,
    desired_structure: Value,
    grants: DesiredGrants,
) -> Result<NodeDesiredDeployment> {
    let mut artifacts = BTreeMap::new();
    for artifact in ConfigArtifact::ALL {
        let desired =
            if artifact.predates_records() && desired_structure.get(artifact.field()).is_none() {
                DesiredArtifact::Unmanaged {
                    reason: format!("这条发布记录早于 {}", artifact.field()),
                }
            } else {
                load_artifact_from_metadata(
                    &mut *connection,
                    desired_structure_field(&desired_structure, artifact.field())?,
                )
                .await?
            };
        artifacts.insert(artifact.field(), desired);
    }
    let mut take = |artifact: ConfigArtifact| {
        artifacts
            .remove(artifact.field())
            .expect("every artifact was just inserted")
    };
    let phantun = take(ConfigArtifact::Phantun);
    let hy2_port_hop = take(ConfigArtifact::Hy2PortHop);
    let wireguard = take(ConfigArtifact::WireGuard);
    let xray = take(ConfigArtifact::Xray);
    let actions =
        serde_json::from_value(desired_structure.get("actions").cloned().ok_or_else(|| {
            StoreError::InvalidData("desired_structure.actions is missing".to_owned())
        })?)?;
    let usage_generation_id: Option<i64> = sqlx::query_scalar(
        "SELECT usage_generation_id FROM deployment_target_state
         WHERE deployment_id = $1 AND node_id = $2",
    )
    .bind(deployment_id)
    .bind(&node_id)
    .fetch_one(&mut *connection)
    .await?;

    Ok(NodeDesiredDeployment {
        deployment_id,
        node_id,
        claim_generation: 0,
        wave: u32::try_from(wave)
            .map_err(|_| StoreError::InvalidData("deployment wave is out of range".to_owned()))?,
        actions,
        usage_generation_id,
        // The distribution source is filled in by the HTTP layer: it is configuration of the
        // runtime environment (env), which store should not know about.
        phantun_binary: None,
        desired: NodeDesiredState {
            phantun,
            wireguard,
            xray,
            hy2_port_hop,
            grants,
        },
    })
}

/// The revision this kind of deployment last shipped successfully, which the console uses as the
/// baseline for artifact diffs.
///
/// It looks only at its own kind: what lands on disk is decided by configuration deployments and
/// the runtime list by grants deployments, and the two lines do not interfere. It is queried once
/// at creation and written into a column rather than derived at query time — a rollback inverts
/// this timeline, and a baseline reconstructed afterwards is not the one that was used.
async fn last_succeeded_revision<'e, E>(executor: E, kind: DeploymentKind) -> Result<Option<i64>>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    Ok(sqlx::query(
        "SELECT revision_id
         FROM deployments
         WHERE kind = $1
           AND status = 'succeeded'
           AND activation_status = 'activated'
         ORDER BY id DESC
         LIMIT 1",
    )
    .bind(kind.as_str())
    .fetch_optional(executor)
    .await?
    .map(|row| row.try_get::<i64, _>("revision_id"))
    .transpose()?)
}

async fn insert_target(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
    target: &PlannedTarget,
) -> Result<()> {
    // Every artifact this target carries, not a hand-copied three. What the record references and
    // what is stored have to be the same set: `desired_structure` keeps a digest, and the machine's
    // own poll looks that digest up. Miss one and the release cannot be handed out at all — the
    // agent's request errors, the target never leaves `pending`, and nothing says why.
    for (_, artifact) in target.desired.artifacts() {
        insert_artifact_blob(tx, artifact).await?;
    }

    let target_status = match target.status {
        PlannedTargetStatus::Pending => "pending",
        PlannedTargetStatus::Deferred => "deferred",
        PlannedTargetStatus::Skipped => "skipped",
    };
    let lifecycle_epoch = crate::lifecycle::current_epoch_tx(tx, &target.node_id).await?;
    sqlx::query(
        "INSERT INTO deployment_targets (deployment_id, node_id, status)
         VALUES ($1, $2, $3)",
    )
    .bind(deployment_id)
    .bind(&target.node_id)
    .bind(target_status)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "INSERT INTO deployment_target_state (
            deployment_id, node_id, wave, disruptive, desired_structure, desired_grants,
            lifecycle_epoch
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(deployment_id)
    .bind(&target.node_id)
    .bind(i32::try_from(target.wave).map_err(|_| {
        StoreError::InvalidData(format!("deployment wave out of range: {}", target.wave))
    })?)
    .bind(target.disruptive)
    .bind(desired_structure_json(target)?)
    .bind(serde_json::to_value(&target.desired.grants)?)
    .bind(lifecycle_epoch)
    .execute(&mut **tx)
    .await?;

    let revision_id: i64 = sqlx::query_scalar("SELECT revision_id FROM deployments WHERE id = $1")
        .bind(deployment_id)
        .fetch_one(&mut **tx)
        .await?;
    let snapshot =
        crate::materialize::load_snapshot_tx(tx, Some(revision_to_u64(revision_id)?)).await?;
    let usage_generation_id = crate::usage::create_usage_generation_for_target(
        tx,
        deployment_id,
        &target.node_id,
        &snapshot,
        &target.desired,
        &target.actions,
    )
    .await?;
    if let Some(usage_generation_id) = usage_generation_id {
        sqlx::query(
            "UPDATE deployment_target_state SET usage_generation_id = $3
             WHERE deployment_id = $1 AND node_id = $2",
        )
        .bind(deployment_id)
        .bind(&target.node_id)
        .bind(usage_generation_id)
        .execute(&mut **tx)
        .await?;
    }

    if target.status == PlannedTargetStatus::Deferred {
        let kind: String = sqlx::query_scalar("SELECT kind FROM deployments WHERE id = $1")
            .bind(deployment_id)
            .fetch_one(&mut **tx)
            .await?;
        let kind = DeploymentKind::parse(&kind).ok_or_else(|| {
            StoreError::InvalidData(format!("deployment {deployment_id} has invalid kind"))
        })?;
        let obligation_target = if kind == DeploymentKind::Config {
            plan_snapshot_deployment(&snapshot, &[])
                .map_err(plan_error)?
                .targets
                .into_iter()
                .find(|candidate| candidate.node_id == target.node_id)
                .ok_or_else(|| {
                    StoreError::InvalidData(format!(
                        "node {} is absent from deployment {} full desired state",
                        target.node_id, deployment_id
                    ))
                })?
        } else {
            target.clone()
        };
        for (_, artifact) in obligation_target.desired.artifacts() {
            insert_artifact_blob(tx, artifact).await?;
        }
        let obligation_usage_generation = crate::usage::create_usage_generation_for_target(
            tx,
            deployment_id,
            &target.node_id,
            &snapshot,
            &obligation_target.desired,
            &obligation_target.actions,
        )
        .await?;
        insert_or_supersede_obligation_tx(
            tx,
            deployment_id,
            revision_id,
            lifecycle_epoch,
            kind,
            &obligation_target,
            obligation_usage_generation,
        )
        .await?;
    }

    Ok(())
}

async fn insert_or_supersede_obligation_tx(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
    revision_id: i64,
    lifecycle_epoch: i64,
    kind: DeploymentKind,
    target: &PlannedTarget,
    usage_generation_id: Option<i64>,
) -> Result<i64> {
    let previous = sqlx::query(
        "SELECT source_deployment_id, status
           FROM node_convergence_obligations
          WHERE node_id = $1
            AND kind = $2
            AND lifecycle_epoch = $3
            AND status IN (
                'pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty'
            )
          ORDER BY generation
          FOR UPDATE",
    )
    .bind(&target.node_id)
    .bind(kind.as_str())
    .bind(lifecycle_epoch)
    .fetch_all(&mut **tx)
    .await?;

    if previous.iter().any(|row| {
        row.try_get::<String, _>("status")
            .is_ok_and(|status| matches!(status.as_str(), "dispatched" | "converging"))
    }) {
        mark_node_applied_dirty_tx(
            tx,
            deployment_id,
            &target.node_id,
            kind,
            "隔离债务被更新期望取代，旧执行结果不再可信",
        )
        .await?;
    }

    sqlx::query(
        "UPDATE node_convergence_obligations
            SET status = 'superseded', superseded_at = now()
          WHERE node_id = $1
            AND kind = $2
            AND lifecycle_epoch = $3
            AND status IN (
                'pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty'
            )",
    )
    .bind(&target.node_id)
    .bind(kind.as_str())
    .bind(lifecycle_epoch)
    .execute(&mut **tx)
    .await?;

    let mut old_deployments = BTreeSet::new();
    for row in previous {
        let old_deployment_id: i64 = row.try_get("source_deployment_id")?;
        old_deployments.insert(old_deployment_id);
        sqlx::query(
            "UPDATE deployment_targets
                SET status = 'superseded',
                    error = '已被隔离节点的更新期望取代'
              WHERE deployment_id = $1
                AND node_id = $2
                AND status IN ('deferred', 'failed-recovered', 'failed-dirty')",
        )
        .bind(old_deployment_id)
        .bind(&target.node_id)
        .execute(&mut **tx)
        .await?;
    }

    let desired_structure = desired_structure_json(target)?;
    let desired_grants = serde_json::to_value(&target.desired.grants)?;
    let desired_fingerprint = sha256_hex(&serde_json::to_vec(&json!({
        "structure": desired_structure,
        "grants": desired_grants,
    }))?);
    let generation: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max(generation), 0) + 1
           FROM node_convergence_obligations
          WHERE node_id = $1 AND kind = $2 AND lifecycle_epoch = $3",
    )
    .bind(&target.node_id)
    .bind(kind.as_str())
    .bind(lifecycle_epoch)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO node_convergence_obligations (
             node_id, kind, lifecycle_epoch, generation,
             source_deployment_id, source_revision_id,
             desired_structure, desired_grants, desired_fingerprint,
             usage_generation_id, priority, status
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 0, 'pending')",
    )
    .bind(&target.node_id)
    .bind(kind.as_str())
    .bind(lifecycle_epoch)
    .bind(generation)
    .bind(deployment_id)
    .bind(revision_id)
    .bind(desired_structure)
    .bind(desired_grants)
    .bind(desired_fingerprint)
    .bind(usage_generation_id)
    .execute(&mut **tx)
    .await?;

    for old_deployment_id in old_deployments {
        if old_deployment_id != deployment_id {
            store_deployment_settlement_tx(tx, old_deployment_id).await?;
        }
    }
    Ok(generation)
}

async fn mark_node_applied_dirty_tx(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
    node_id: &str,
    kind: DeploymentKind,
    reason: &str,
) -> Result<()> {
    let state = if kind == DeploymentKind::Config {
        dirty_reported_state(reason.to_owned())
    } else {
        ReportedNodeState {
            phantun: AppliedArtifactState::Unmanaged,
            wireguard: AppliedArtifactState::Unmanaged,
            xray: AppliedArtifactState::Unmanaged,
            hy2_port_hop: AppliedArtifactState::Unmanaged,
            grants: AppliedGrantsState::Dirty {
                reason: reason.to_owned(),
            },
        }
    };
    upsert_node_applied_state(tx, deployment_id, node_id, &state, kind).await
}

async fn insert_artifact_blob(
    tx: &mut Transaction<'_, Postgres>,
    artifact: &DesiredArtifact,
) -> Result<()> {
    let DesiredArtifact::Present { content, sha256 } = artifact else {
        return Ok(());
    };
    let byte_len = i32::try_from(content.len())
        .map_err(|_| StoreError::InvalidData(format!("artifact too large to store: {sha256}")))?;

    sqlx::query(
        "INSERT INTO artifact_blobs (sha256, content, byte_len)
         VALUES ($1, $2, $3)
         ON CONFLICT (sha256) DO NOTHING",
    )
    .bind(sha256)
    .bind(content)
    .bind(byte_len)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

fn target_status_from_report(
    report: &TargetConvergenceReport,
    desired_structure: &Value,
    desired_grants: &DesiredGrants,
) -> Result<&'static str> {
    match report.result {
        TargetApplyResult::Applied => {
            if reported_state_matches_desired(
                &report.observed_after,
                desired_structure,
                desired_grants,
            )? {
                Ok("succeeded")
            } else {
                Ok("failed-dirty")
            }
        }
        TargetApplyResult::FailedRecovered => Ok("failed-recovered"),
        TargetApplyResult::FailedDirty => Ok("failed-dirty"),
    }
}

fn reported_state_matches_desired(
    observed: &ReportedNodeState,
    desired_structure: &Value,
    desired_grants: &DesiredGrants,
) -> Result<bool> {
    // Every artifact the release asked for, not the two somebody listed. Judging a subset is how a
    // deployment that moved nothing still reports success: the machine echoes back whatever it did
    // with the artifact nobody checks, the target goes green, and the plan asks for the same thing
    // again next round.
    //
    // A record predating an artifact has no field for it (see `predates_records`); there is nothing
    // to hold the machine to, so it does not count against convergence.
    for artifact in ConfigArtifact::ALL {
        if artifact.predates_records() && desired_structure.get(artifact.field()).is_none() {
            continue;
        }
        let metadata = desired_structure_field(desired_structure, artifact.field())?;
        if !artifact_state_matches_metadata(observed.artifact(artifact), metadata)? {
            return Ok(false);
        }
    }
    Ok(grants_match(desired_grants, &observed.grants))
}

fn artifact_state_matches_metadata(
    observed: &AppliedArtifactState,
    metadata: &Value,
) -> Result<bool> {
    let state = metadata
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| StoreError::InvalidData("artifact metadata state is missing".to_owned()))?;
    match state {
        "present" => {
            let expected_sha = metadata
                .get("sha256")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    StoreError::InvalidData(
                        "present artifact metadata sha256 is missing".to_owned(),
                    )
                })?;
            Ok(matches!(
                observed,
                AppliedArtifactState::Present { sha256 } if sha256 == expected_sha
            ))
        }
        "disabled" => Ok(matches!(observed, AppliedArtifactState::Disabled)),
        "unmanaged" => Ok(matches!(observed, AppliedArtifactState::Unmanaged)),
        value => Err(StoreError::InvalidData(format!(
            "unknown artifact metadata state {value}"
        ))),
    }
}

fn reported_node_state_json(state: &ReportedNodeState) -> Result<Value> {
    Ok(json!({
        "phantun": serde_json::to_value(&state.phantun)?,
        "wireguard": serde_json::to_value(&state.wireguard)?,
        "xray": serde_json::to_value(&state.xray)?,
        "hy2_port_hop": serde_json::to_value(&state.hy2_port_hop)?,
        "grants": serde_json::to_value(&state.grants)?,
    }))
}

async fn upsert_node_applied_state(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
    node_id: &str,
    state: &ReportedNodeState,
    kind: DeploymentKind,
) -> Result<()> {
    // For whatever this deployment did not manage the observation is `Unmanaged` — on seeing a
    // desired of Unmanaged the agent returns it unchanged without looking at the machine at all.
    // Overwriting the applied state with that would have the control plane believe those are
    // unmanaged while they run perfectly well on the machine. The test reads the artifact itself
    // rather than the deployment kind: configuration deployments narrow too (a machine that only
    // changed wg carries no xray), and the two are one thing. Before narrowing, Unmanaged cannot
    // appear in desired (`desired_*` produces only Present or Disabled), so encountering it means
    // this deployment did not touch it.
    let touched =
        |artifact: &AppliedArtifactState| !matches!(artifact, AppliedArtifactState::Unmanaged);
    let touched_phantun = touched(&state.phantun);
    let touched_wireguard = touched(&state.wireguard);
    let touched_xray = touched(&state.xray);
    let touched_hy2_port_hop = touched(&state.hy2_port_hop);
    let touched_grants = !matches!(state.grants, AppliedGrantsState::Unmanaged);
    // source_deployment_id says which release this configuration came from, and since a grants
    // deployment changed no configuration, that field should keep the last configuration
    // deployment's number.
    let touched_source = kind == DeploymentKind::Config;

    let (phantun_state, phantun_sha256, phantun_observed) = artifact_db_values(&state.phantun)?;
    let (wireguard_state, wireguard_sha256, wireguard_observed) =
        artifact_db_values(&state.wireguard)?;
    let (xray_state, xray_sha256, xray_observed) = artifact_db_values(&state.xray)?;
    let (hy2_port_hop_state, hy2_port_hop_sha256, hy2_port_hop_observed) =
        artifact_db_values(&state.hy2_port_hop)?;
    let (grants_state, grants_observed) = grants_db_values(&state.grants)?;

    sqlx::query(
        "INSERT INTO node_applied_state (
            node_id,
            phantun_state, phantun_sha256, phantun_observed,
            wireguard_state, wireguard_sha256, wireguard_observed,
            xray_state, xray_sha256, xray_observed,
            hy2_port_hop_state, hy2_port_hop_sha256, hy2_port_hop_observed,
            grants_state, grants_observed,
            source_deployment_id, observed_at
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, now())
         ON CONFLICT (node_id) DO UPDATE SET
            phantun_state = CASE WHEN $17::boolean
                THEN EXCLUDED.phantun_state ELSE node_applied_state.phantun_state END,
            phantun_sha256 = CASE WHEN $17::boolean
                THEN EXCLUDED.phantun_sha256 ELSE node_applied_state.phantun_sha256 END,
            phantun_observed = CASE WHEN $17::boolean
                THEN EXCLUDED.phantun_observed ELSE node_applied_state.phantun_observed END,
            wireguard_state = CASE WHEN $18::boolean
                THEN EXCLUDED.wireguard_state ELSE node_applied_state.wireguard_state END,
            wireguard_sha256 = CASE WHEN $18::boolean
                THEN EXCLUDED.wireguard_sha256 ELSE node_applied_state.wireguard_sha256 END,
            wireguard_observed = CASE WHEN $18::boolean
                THEN EXCLUDED.wireguard_observed ELSE node_applied_state.wireguard_observed END,
            xray_state = CASE WHEN $19::boolean
                THEN EXCLUDED.xray_state ELSE node_applied_state.xray_state END,
            xray_sha256 = CASE WHEN $19::boolean
                THEN EXCLUDED.xray_sha256 ELSE node_applied_state.xray_sha256 END,
            xray_observed = CASE WHEN $19::boolean
                THEN EXCLUDED.xray_observed ELSE node_applied_state.xray_observed END,
            hy2_port_hop_state = CASE WHEN $20::boolean
                THEN EXCLUDED.hy2_port_hop_state ELSE node_applied_state.hy2_port_hop_state END,
            hy2_port_hop_sha256 = CASE WHEN $20::boolean
                THEN EXCLUDED.hy2_port_hop_sha256 ELSE node_applied_state.hy2_port_hop_sha256 END,
            hy2_port_hop_observed = CASE WHEN $20::boolean
                THEN EXCLUDED.hy2_port_hop_observed ELSE node_applied_state.hy2_port_hop_observed END,
            grants_state = CASE WHEN $21::boolean
                THEN EXCLUDED.grants_state ELSE node_applied_state.grants_state END,
            grants_observed = CASE WHEN $21::boolean
                THEN EXCLUDED.grants_observed ELSE node_applied_state.grants_observed END,
            source_deployment_id = CASE WHEN $22::boolean
                THEN EXCLUDED.source_deployment_id ELSE node_applied_state.source_deployment_id END,
            observed_at = EXCLUDED.observed_at",
    )
    .bind(node_id)
    .bind(phantun_state)
    .bind(phantun_sha256)
    .bind(phantun_observed)
    .bind(wireguard_state)
    .bind(wireguard_sha256)
    .bind(wireguard_observed)
    .bind(xray_state)
    .bind(xray_sha256)
    .bind(xray_observed)
    .bind(hy2_port_hop_state)
    .bind(hy2_port_hop_sha256)
    .bind(hy2_port_hop_observed)
    .bind(grants_state)
    .bind(grants_observed)
    .bind(deployment_id)
    .bind(touched_phantun)
    .bind(touched_wireguard)
    .bind(touched_xray)
    .bind(touched_hy2_port_hop)
    .bind(touched_grants)
    .bind(touched_source)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

fn artifact_db_values(
    state: &AppliedArtifactState,
) -> Result<(&'static str, Option<String>, Value)> {
    let observed = serde_json::to_value(state)?;
    let values = match state {
        AppliedArtifactState::Present { sha256 } => ("present", Some(sha256.clone()), observed),
        AppliedArtifactState::Disabled => ("disabled", None, observed),
        AppliedArtifactState::Unmanaged => ("unmanaged", None, observed),
        AppliedArtifactState::Unknown => ("unknown", None, observed),
        AppliedArtifactState::Dirty { .. } => ("dirty", None, observed),
    };
    Ok(values)
}

fn grants_db_values(state: &AppliedGrantsState) -> Result<(&'static str, Value)> {
    let values = match state {
        AppliedGrantsState::Present { inbounds } => ("present", json!({ "inbounds": inbounds })),
        AppliedGrantsState::Disabled => ("disabled", serde_json::to_value(state)?),
        AppliedGrantsState::Unmanaged => ("unmanaged", serde_json::to_value(state)?),
        AppliedGrantsState::Unknown => ("unknown", serde_json::to_value(state)?),
        AppliedGrantsState::Dirty { .. } => ("dirty", serde_json::to_value(state)?),
    };
    Ok(values)
}

async fn refresh_deployment_status(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
    target_status: &str,
) -> Result<String> {
    if matches!(target_status, "failed-recovered" | "failed-dirty") {
        sqlx::query(
            "UPDATE deployments
             SET status = 'halted',
                 active = TRUE,
                 settlement_status = 'uncertain',
                 started_at = COALESCE(started_at, now()),
                 halted_at = COALESCE(halted_at, now())
             WHERE id = $1",
        )
        .bind(deployment_id)
        .execute(&mut **tx)
        .await?;
        return Ok("halted".to_owned());
    }

    let row = sqlx::query(
        "SELECT
            count(*) FILTER (WHERE status IN ('pending', 'dispatched', 'converging')) AS open_targets,
            count(*) FILTER (WHERE status IN ('failed-recovered', 'failed-dirty')) AS failed_targets
         FROM deployment_targets
         WHERE deployment_id = $1",
    )
    .bind(deployment_id)
    .fetch_one(&mut **tx)
    .await?;
    let open_targets: i64 = row.try_get("open_targets")?;
    let failed_targets: i64 = row.try_get("failed_targets")?;

    if failed_targets > 0 {
        sqlx::query(
            "UPDATE deployments
             SET status = 'halted',
                 active = TRUE,
                 settlement_status = 'uncertain',
                 started_at = COALESCE(started_at, now()),
                 halted_at = COALESCE(halted_at, now())
             WHERE id = $1",
        )
        .bind(deployment_id)
        .execute(&mut **tx)
        .await?;
        Ok("halted".to_owned())
    } else if open_targets == 0 {
        let settlement_status = refresh_deployment_settlement_tx(tx, deployment_id).await?;
        sqlx::query(
            "UPDATE deployments
             SET status = 'succeeded',
                 active = NULL,
                 settlement_status = $2,
                 started_at = COALESCE(started_at, now()),
                 finished_at = COALESCE(finished_at, now())
             WHERE id = $1",
        )
        .bind(deployment_id)
        .bind(settlement_status)
        .execute(&mut **tx)
        .await?;
        crate::serving::activate_deployment_tx(tx, deployment_id).await?;
        Ok("succeeded".to_owned())
    } else {
        sqlx::query(
            "UPDATE deployments
             SET status = 'running',
                 active = TRUE,
                 settlement_status = CASE
                     WHEN EXISTS (
                         SELECT 1 FROM node_convergence_obligations obligation
                          WHERE obligation.source_deployment_id = $1
                            AND obligation.status IN (
                                'pending', 'dispatched', 'converging',
                                'failed-recovered', 'failed-dirty'
                            )
                     ) THEN 'debt'
                     ELSE settlement_status
                 END,
                 started_at = COALESCE(started_at, now())
             WHERE id = $1",
        )
        .bind(deployment_id)
        .execute(&mut **tx)
        .await?;
        Ok("running".to_owned())
    }
}

async fn refresh_deployment_settlement_tx(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
) -> Result<&'static str> {
    let row = sqlx::query(
        "SELECT EXISTS (
                    SELECT 1 FROM node_convergence_obligations
                     WHERE source_deployment_id = $1
                       AND status IN ('failed-recovered', 'failed-dirty')
                ) AS uncertain,
                EXISTS (
                    SELECT 1 FROM node_convergence_obligations
                     WHERE source_deployment_id = $1
                       AND status IN ('pending', 'dispatched', 'converging')
                ) AS debt",
    )
    .bind(deployment_id)
    .fetch_one(&mut **tx)
    .await?;
    if row.try_get("uncertain")? {
        Ok("uncertain")
    } else if row.try_get("debt")? {
        Ok("debt")
    } else {
        Ok("converged")
    }
}

async fn store_deployment_settlement_tx(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
) -> Result<String> {
    let status = refresh_deployment_settlement_tx(tx, deployment_id).await?;
    sqlx::query("UPDATE deployments SET settlement_status = $2 WHERE id = $1")
        .bind(deployment_id)
        .bind(status)
        .execute(&mut **tx)
        .await?;
    Ok(status.to_owned())
}

fn target_apply_result_name(result: TargetApplyResult) -> &'static str {
    match result {
        TargetApplyResult::Applied => "applied",
        TargetApplyResult::FailedRecovered => "failed-recovered",
        TargetApplyResult::FailedDirty => "failed-dirty",
    }
}

async fn load_artifact_from_metadata(
    connection: &mut PgConnection,
    metadata: &Value,
) -> Result<DesiredArtifact> {
    let state = metadata
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| StoreError::InvalidData("artifact metadata state is missing".to_owned()))?;
    match state {
        "present" => {
            let sha256 = metadata
                .get("sha256")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    StoreError::InvalidData(
                        "present artifact metadata sha256 is missing".to_owned(),
                    )
                })?;
            let row = sqlx::query("SELECT content, byte_len FROM artifact_blobs WHERE sha256 = $1")
                .bind(sha256)
                .fetch_one(&mut *connection)
                .await?;
            let content: String = row.try_get("content")?;
            let byte_len: i32 = row.try_get("byte_len")?;
            validate_artifact_len(sha256, &content, byte_len, metadata)?;
            Ok(DesiredArtifact::Present {
                content,
                sha256: sha256.to_owned(),
            })
        }
        "disabled" => Ok(DesiredArtifact::Disabled {
            reason: metadata_reason(metadata),
        }),
        "unmanaged" => Ok(DesiredArtifact::Unmanaged {
            reason: metadata_reason(metadata),
        }),
        value => Err(StoreError::InvalidData(format!(
            "unknown artifact metadata state {value}"
        ))),
    }
}

fn desired_structure_json(target: &PlannedTarget) -> Result<Value> {
    let mut structure = serde_json::Map::new();
    structure.insert("actions".to_owned(), serde_json::to_value(&target.actions)?);
    for (artifact, desired) in target.desired.artifacts() {
        structure.insert(artifact.field().to_owned(), artifact_metadata(desired));
    }
    Ok(Value::Object(structure))
}

fn desired_structure_is_full_teardown(structure: &Value) -> bool {
    ConfigArtifact::ALL.into_iter().all(|artifact| {
        structure
            .get(artifact.field())
            .and_then(|metadata| metadata.get("state"))
            .and_then(Value::as_str)
            == Some("disabled")
    })
}

fn artifact_metadata(artifact: &DesiredArtifact) -> Value {
    match artifact {
        DesiredArtifact::Present { content, sha256 } => json!({
            "state": "present",
            "sha256": sha256,
            "byte_len": content.len(),
        }),
        DesiredArtifact::Disabled { reason } => json!({
            "state": "disabled",
            "reason": reason,
        }),
        DesiredArtifact::Unmanaged { reason } => json!({
            "state": "unmanaged",
            "reason": reason,
        }),
    }
}

async fn frozen_desired_grants(
    pool: &PgPool,
    stored: Option<Value>,
    revision_id: u64,
    node_id: &str,
) -> Result<DesiredGrants> {
    if let Some(stored) = stored {
        return serde_json::from_value(stored).map_err(StoreError::from);
    }
    // Compatibility for a work order created before desired_grants was recorded.  Its own
    // revision is still immutable and materialized, so reconstruct from that rather than silently
    // folding today's permissions into yesterday's release.
    let snapshot = materialize::load_snapshot(pool, Some(revision_id)).await?;
    desired_grants_for_node(snapshot, node_id)
}

async fn frozen_desired_grants_tx(
    tx: &mut Transaction<'_, Postgres>,
    stored: Option<Value>,
    revision_id: u64,
    node_id: &str,
) -> Result<DesiredGrants> {
    if let Some(stored) = stored {
        return serde_json::from_value(stored).map_err(StoreError::from);
    }
    let snapshot = materialize::load_snapshot_tx(tx, Some(revision_id)).await?;
    desired_grants_for_node(snapshot, node_id)
}

fn desired_grants_for_node(snapshot: ModelSnapshot, node_id: &str) -> Result<DesiredGrants> {
    let plan = plan_snapshot_deployment(&snapshot, &[]).map_err(plan_error)?;
    let target = plan
        .targets
        .into_iter()
        .find(|target| target.node_id == node_id)
        .ok_or_else(|| {
            StoreError::InvalidData(format!(
                "node {node_id} is not present in the deployment revision"
            ))
        })?;
    Ok(target.desired.grants)
}

fn desired_structure_field<'a>(value: &'a Value, field: &str) -> Result<&'a Value> {
    value
        .get(field)
        .ok_or_else(|| StoreError::InvalidData(format!("desired_structure.{field} is missing")))
}

fn validate_artifact_len(
    sha256: &str,
    content: &str,
    stored_byte_len: i32,
    metadata: &Value,
) -> Result<()> {
    let actual_len = i32::try_from(content.len()).map_err(|_| {
        StoreError::InvalidData(format!("artifact content length overflows i32: {sha256}"))
    })?;
    if actual_len != stored_byte_len {
        return Err(StoreError::InvalidData(format!(
            "artifact_blobs byte_len mismatch for {sha256}: stored {stored_byte_len}, actual {actual_len}"
        )));
    }

    if let Some(expected) = metadata.get("byte_len").and_then(Value::as_i64) {
        let expected = i32::try_from(expected).map_err(|_| {
            StoreError::InvalidData(format!(
                "artifact metadata byte_len out of range for {sha256}: {expected}"
            ))
        })?;
        if expected != stored_byte_len {
            return Err(StoreError::InvalidData(format!(
                "artifact metadata byte_len mismatch for {sha256}: metadata {expected}, blob {stored_byte_len}"
            )));
        }
    }

    Ok(())
}

fn metadata_reason(metadata: &Value) -> String {
    metadata
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn parse_artifact_state(
    kind: &str,
    state: String,
    sha256: Option<String>,
    observed: Value,
) -> Result<AppliedArtifactState> {
    match state.as_str() {
        "present" => Ok(AppliedArtifactState::Present {
            sha256: sha256.ok_or_else(|| {
                StoreError::InvalidData(format!(
                    "node_applied_state {kind}_state is present but {kind}_sha256 is NULL"
                ))
            })?,
        }),
        "disabled" => Ok(AppliedArtifactState::Disabled),
        "unmanaged" => Ok(AppliedArtifactState::Unmanaged),
        "unknown" => Ok(AppliedArtifactState::Unknown),
        "dirty" => Ok(AppliedArtifactState::Dirty {
            reason: observed_reason(&observed),
        }),
        value => Err(StoreError::InvalidData(format!(
            "unknown {kind}_state {value}"
        ))),
    }
}

fn parse_grants_state(state: String, observed: Value) -> Result<AppliedGrantsState> {
    match state.as_str() {
        "present" => Ok(AppliedGrantsState::Present {
            inbounds: parse_grant_inbounds(observed)?,
        }),
        "disabled" => Ok(AppliedGrantsState::Disabled),
        "unmanaged" => Ok(AppliedGrantsState::Unmanaged),
        "unknown" => Ok(AppliedGrantsState::Unknown),
        "dirty" => Ok(AppliedGrantsState::Dirty {
            reason: observed_reason(&observed),
        }),
        value => Err(StoreError::InvalidData(format!(
            "unknown grants_state {value}"
        ))),
    }
}

fn parse_grant_inbounds(observed: Value) -> Result<Vec<ObservedInbound>> {
    let inbounds = if let Some(inbounds) = observed.get("inbounds") {
        inbounds.clone()
    } else {
        observed
    };
    Ok(serde_json::from_value(inbounds)?)
}

fn observed_reason(observed: &Value) -> String {
    observed
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("observed state is dirty")
        .to_owned()
}

fn plan_error(error: brocade_deployment::plan::PlanError) -> StoreError {
    match error {
        brocade_deployment::plan::PlanError::PublishBlocked(blocked) => {
            StoreError::InvalidData(format!(
                "deployment blocked by compiler: {} error(s), {} warning(s)",
                blocked.summary.errors, blocked.summary.warnings
            ))
        }
    }
}

fn revision_to_i64(revision: u64) -> Result<i64> {
    i64::try_from(revision)
        .map_err(|_| StoreError::InvalidData(format!("revision out of range: {revision}")))
}

fn revision_to_u64(revision: i64) -> Result<u64> {
    u64::try_from(revision)
        .map_err(|_| StoreError::InvalidData(format!("revision out of range: {revision}")))
}

fn i64_to_u64(field: &str, value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{field} is out of range: {value}")))
}

// ── Discarding pending changes ──────────────────────────────────────
// Withdraw as a batch the changes that were committed but never shipped: the model reverts to the
// version the machines are running, and not one machine has to move.
//
// It reverts to the last successfully released revision, not by one step. The diff the plan
// preview page shows is the whole difference between the released version and the current
// revision; reverting one step leaves the intermediate versions — equally unshipped — in place,
// so the diff after withdrawing is still a wall of changes, and what one sees is "I pressed
// undo and the changes are still there".
//
// It is separate from a rollback (`cancel_deployment_and_rollback`) because the cost differs by a
// whole convergence round: a rollback happens after the changes were pushed and needs a forced
// sync deployment to reconverge the machines onto the old snapshot; the changes here never left
// the control plane and the machines still run that version's artifacts — revert the model, and
// the next verify is already converged, producing no deployment at all.
//
// current_revision is not wound back. It takes the same path as a rollback: create a new revision
// and write the baseline version's snapshot back. Monotonically increasing revision numbers are an
// implicit premise in several places (the agent's comparison, deployments.base_revision_id, that
// setval in `commit_revision`), and winding back invalidates them all at once. The discarded ones
// are marked `aborted` — `revisions.status`'s CHECK has long reserved that value, and the history
// then shows that these versions existed and never took effect.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiscardPendingResult {
    /// The revision numbers voided, oldest first.
    pub discarded_revisions: Vec<u64>,
    /// The current revision after the discard: a new number whose contents equal the
    /// baseline's.
    pub current_revision: u64,
    /// Whose contents the model reverted to — the last successfully released revision.
    pub restored_from_revision: u64,
}

pub async fn discard_pending_changes(
    pool: &PgPool,
    actor: &AdminContext,
    revision_id: u64,
) -> Result<DiscardPendingResult> {
    // The baseline is the revision of the last successful deployment, not the successful
    // deployment with the highest revision number: a rollback deployment's revision number
    // exceeds those it rolled back, and ordering by number would pick an earlier model as the
    // baseline. Ordering by deployment id is ordering by time, and that is what "which version
    // the machines run now" means.
    let base = sqlx::query(
        "SELECT revision_id
         FROM deployments
         WHERE status = 'succeeded'
           AND activation_status = 'activated'
         ORDER BY id DESC
         LIMIT 1",
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| {
        StoreError::Unsupported(
            "no successful deployment yet: there is no published revision to fall back to"
                .to_owned(),
        )
    })?;
    let base_revision = revision_to_u64(base.try_get("revision_id")?)?;
    if base_revision >= revision_id {
        return Err(StoreError::Unsupported(format!(
            "revision {revision_id} is not newer than the last published revision {base_revision}; nothing to discard"
        )));
    }
    // The snapshot is loaded outside the transaction, as a rollback does. The current revision
    // number is verified again inside it, so a commit that got in first fails here rather than
    // writing back from a stale snapshot.
    let restore_snapshot = materialize::load_snapshot(pool, Some(base_revision)).await?;

    let mut tx = pool.begin().await?;
    let current = console::lock_control_state(&mut tx).await?;
    if current != revision_id {
        return Err(StoreError::Unsupported(format!(
            "revision {revision_id} is not the current revision ({current}); reload and try again"
        )));
    }

    // A permission revision has a durable worker row even before it has a deployment. Serialize
    // with that worker so the check below and canceling the queued rows are one decision: it may
    // create the order first (which then blocks this discard), or this transaction cancels the
    // queue first, but the revision can never be restored while its old permission order appears
    // afterwards.
    crate::grant_automation::lock_tx(&mut tx).await?;

    // What has shipped cannot be discarded. The test is whether this span holds a deployment
    // that is not canceled — a canceled one means that release never took effect and should not
    // block the discard; whereas a planned one, though undispatched, already holds the
    // single-flight lock and a machine may receive it at any moment, where withdrawing the model
    // would leave the in-flight deployment pointing at a desired state that does not exist.
    // Cancel that deployment first, then come back and discard.
    let blocking = sqlx::query(
        "SELECT id, status, revision_id
         FROM deployments
         WHERE revision_id > $1 AND revision_id <= $2 AND status <> 'canceled'
         ORDER BY id
         LIMIT 1",
    )
    .bind(revision_to_i64(base_revision)?)
    .bind(revision_to_i64(revision_id)?)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(row) = blocking {
        let id: i64 = row.try_get("id")?;
        let status: String = row.try_get("status")?;
        let rev = revision_to_u64(row.try_get("revision_id")?)?;
        return Err(StoreError::Unsupported(format!(
            "revision {rev} already has deployment #{id} ({status}); cancel it first"
        )));
    }

    let discarded: Vec<u64> = sqlx::query(
        "SELECT id
         FROM revisions
         WHERE id > $1 AND id <= $2 AND status = 'committed'
         ORDER BY id",
    )
    .bind(revision_to_i64(base_revision)?)
    .bind(revision_to_i64(revision_id)?)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|row| revision_to_u64(row.try_get("id")?))
    .collect::<Result<Vec<_>>>()?;

    let actor_id = actor.operator_id().to_owned();
    let note = format!(
        "discard {} pending revision(s), restore model to published revision {base_revision}",
        discarded.len()
    );
    let new_revision = console::insert_revision(&mut tx, &actor_id, &note).await?;
    restore_model_snapshot_tx(&mut tx, new_revision, &restore_snapshot).await?;
    // Restoring the last published snapshot does not need a new permission release: those are
    // exactly the grants already represented by the published baseline.  The discarded
    // revisions' queued jobs are canceled below under the same automation lock.
    let new_revision =
        console::commit_revision_without_grant_automation(&mut tx, new_revision, current, true)
            .await?;
    let restored_snapshot = materialize::load_current_snapshot_tx(&mut tx).await?;
    ensure_restored_snapshot(
        "discard-pending",
        &restore_snapshot,
        &restored_snapshot,
        new_revision,
    )?;

    sqlx::query(
        "UPDATE revisions
         SET status = 'aborted'
         WHERE id > $1 AND id <= $2 AND status = 'committed'",
    )
    .bind(revision_to_i64(base_revision)?)
    .bind(revision_to_i64(revision_id)?)
    .execute(&mut *tx)
    .await?;
    crate::grant_automation::cancel_revision_jobs_tx(&mut tx, base_revision, revision_id).await?;
    tx.commit().await?;

    Ok(DiscardPendingResult {
        discarded_revisions: discarded,
        current_revision: new_revision,
        restored_from_revision: base_revision,
    })
}

#[cfg(test)]
mod tests {
    use super::{flow_column, strip_commit_prefix};

    /// A rollback restores each ingress's `reality_flow` column, and the column carries three
    /// states, not two. The one that used to be lost is Vision-off: written as NULL it reads back
    /// as "follow the fleet" and turns Vision on, which every XHTTP ingress is (XHTTP cannot carry
    /// Vision), so every rollback tripped `ensure_restored_snapshot` on the apps section — or, with
    /// the guard absent, silently broke those clients.
    #[test]
    fn flow_column_keeps_vision_off_out_of_null() {
        let fleet = Some("xtls-rprx-vision".to_owned());
        // Off on this ingress while the fleet runs Vision: the empty string, never NULL.
        assert_eq!(flow_column(None, &fleet), Some(String::new()));
        // Follows the fleet (or equals it): NULL.
        assert_eq!(flow_column(fleet.clone(), &fleet), None);
        // A different explicit value stays itself.
        assert_eq!(
            flow_column(Some("xtls-rprx-direct".to_owned()), &fleet),
            Some("xtls-rprx-direct".to_owned())
        );
        // Fleet also off: both None, and NULL reads back as off — no empty string needed.
        assert_eq!(flow_column(None, &None), None);
    }

    /// A deployment note strips the draft layer's phrasing from the front of a revision note,
    /// but only the two forms it recognizes. People can write their own notes, and guessing wrong
    /// cuts a sentence of theirs in half.
    #[test]
    fn strips_only_the_draft_layer_prefix() {
        // The two forms a draft commit assembles (`apply_ops` in draft.rs)
        assert_eq!(
            strip_commit_prefix("提交：规则 c-hk/sg-01"),
            "规则 c-hk/sg-01"
        );
        assert_eq!(
            strip_commit_prefix("提交 3 处改动：机器 aa、规则 c-hk/sg-01"),
            "机器 aa、规则 c-hk/sg-01"
        );

        // Notes people wrote are left alone — even one that also starts with that word
        assert_eq!(strip_commit_prefix("提交前先看一眼"), "提交前先看一眼");
        assert_eq!(
            strip_commit_prefix("提交给运维：明早再推"),
            "提交给运维：明早再推"
        );
        assert_eq!(strip_commit_prefix("港新线路换落地"), "港新线路换落地");

        // Whitespace at both ends is always trimmed: it travels into the list and skews the
        // row
        assert_eq!(strip_commit_prefix("  提交：机器 aa  "), "机器 aa");
        assert_eq!(strip_commit_prefix("   "), "");
    }
}
