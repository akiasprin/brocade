//! Automatic traffic-quota enforcement: accounting, revoking grants, restoring them, and
//! releasing when needed.
//!
//! Reading and writing quotas themselves lives in `console.rs` (it is part of the admin API,
//! alongside the CRUD for users and grants). What lives here is the background round — the loop
//! in the control-plane process calls it once every 60 seconds.
//!
//! Three steps to a round:
//!   1. Account: how much each (tenant, user, view) with a quota used this month, who went over,
//!      and who should be restored;
//!   2. Edit the model: revoke and add grants, composing the whole round into one revision;
//!   3. Ship a grants deployment: `narrow_to_kind(Grants)` admits only the machines that need
//!      nothing but a list sync this round, and marks all three artifacts Unmanaged. Such a
//!      deployment structurally cannot push configuration, so no runtime gate is needed to check
//!      whether other actions slipped in — and configuration changes others left unreleased are
//!      unaffected.
//!
//! Why this approach works (drop any one of these and it does not):
//!   - Revoking does not disconnect anyone: xray is Unmanaged in a grants deployment, the
//!     agent's `converge_linux_xray` returns on seeing that, and never reaches the `pkill` in
//!     `apply_xray`. This did not hold while the deployment kinds were undivided — back then
//!     changing one grant restarted xray and dropped everyone;
//!   - `SyncGrants`'s `is_disruptive` is always false and it does not wave, so
//!     `requires_confirmation` is always false too (the first row of that table), and one wave
//!     completes without anyone confirming;
//!   - Revoking does not lose usage: adding and removing grants does not affect xray's counters
//!     at all, the values survive an `rmu`, and adding one back resumes the count. Otherwise one
//!     revocation would zero the usage and hand the user a whole extra month of quota;
//!   - Repetition is harmless: `commit_revision` returns the number when nothing really changed,
//!     `adu`/`rmu` are idempotent themselves, and `create_deployment` honors the idempotency
//!     key. Several instances running at once cost a few extra log lines at most.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use brocade_deployment::{
    plan::{
        can_sync_grants_now, narrow_to_kind, DeploymentKind, PlannedAction, PlannedTargetStatus,
    },
    protocol::CreateDeploymentRequest,
};

use crate::{
    console::{
        commit_revision_without_grant_automation, insert_revision, lock_control_state,
        upsert_grant_tx,
    },
    deployment::{create_deployment, plan_full_deployment},
    materialize::current_revision,
    AdminContext, CreateGrantRequest, Result, StoreError,
};

/// Whose name enforcement records under. Written into `revisions.author` and
/// `deployments.actor`, so that the release history shows at a glance that nobody pressed
/// this.
pub const QUOTA_ACTOR: &str = "system:quota";

/// One grant to revoke, or one to restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaGrantChange {
    pub tenant_id: String,
    pub user_id: String,
    pub app_id: String,
    pub ingress_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaEnforcementPlan {
    /// Grants still in place whose quota is spent
    pub suspend: Vec<QuotaGrantChange>,
    /// Ones the system revoked that should no longer be (a month reset, a raised quota, a
    /// deleted quota)
    pub restore: Vec<QuotaGrantChange>,
}

impl QuotaEnforcementPlan {
    pub fn is_empty(&self) -> bool {
        self.suspend.is_empty() && self.restore.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaEnforcementOutcome {
    pub suspended: usize,
    pub restored: usize,
    /// Which revision it landed on. With no real change this is the current one
    /// (`commit_revision` returned the number).
    pub revision_id: u64,
    pub deployment_id: Option<i64>,
    // Machines whose lists changed but that this round cannot push: they also owe configuration
    // changes (somebody stamped a revision without releasing it), while a grants deployment
    // admits only machines needing nothing but a list sync. The round after the configuration
    // deployment lands picks them up naturally. Not an error, but something that must be said —
    // otherwise a red light shows in the UI with nobody knowing what it waits for.
    pub deferred: Vec<String>,
}

// This month's consumption does not go through `list_monthly_usage_summary` here: that query
// carries `coalesce(s.app_id, i.app_id)` and a LEFT JOIN on ingresses, serving someone opening a
// page and wanting historical completeness; this one runs every 60 seconds and has to hit the
// `usage_samples_by_user_app_window` index, so it uses the frozen column directly. 0002's
// backfill already attributed every historical sample it could, and those it could not (whose
// ingress was long deleted) belong to no view anyway.
//
// Month boundaries follow the same convention as everywhere else: date_trunc at +08, decided
// server-side.
const MONTH_USED_SQL: &str = "
    SELECT q.tenant_id, q.user_id, q.app_id, q.limit_bytes,
           coalesce((
               SELECT sum(s.uplink_bytes + s.downlink_bytes)
               FROM usage_samples s
               WHERE s.tenant_id = q.tenant_id
                 AND s.user_id = q.user_id
                 AND s.app_id = q.app_id
                 AND s.window_start >= (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                                        AT TIME ZONE 'Asia/Hong_Kong')
           ), 0)::bigint AS used_bytes
    FROM user_app_quotas q
";

/// Account. Writes nothing.
pub async fn plan_quota_enforcement(pool: &PgPool) -> Result<QuotaEnforcementPlan> {
    // To revoke: the quota is spent and they still hold unrevoked grants under this view. Grant
    // presence is the source of truth. A surviving suspension record must not exempt a grant an
    // operator or an old client put back; DELETE's rows_affected already gives idempotent counts.
    let suspend = sqlx::query(&format!(
        "WITH used AS ({MONTH_USED_SQL})
         SELECT g.tenant_id, g.user_id, g.app_id, g.ingress_id
         FROM used u
         JOIN grants g
           ON g.tenant_id = u.tenant_id
          AND g.user_id = u.user_id
          AND g.app_id = u.app_id
         WHERE u.used_bytes >= u.limit_bytes
         ORDER BY g.tenant_id, g.user_id, g.app_id, g.ingress_id"
    ))
    .fetch_all(pool)
    .await?;

    // To restore: a suspension record remains with no spent quota holding it down. One LEFT
    // JOIN covers all three cases — the quota was deleted, raised, or reset at the start of the
    // month. Suspensions whose ingress is long gone also fall into this set
    // (`upsert_grant_tx` reports NotFound from `ensure_ingress_in_app_tx`), so they are filtered
    // against the surviving ingresses first and cleared in passing during execution: what they
    // point at is gone, and keeping them only produces one error per round.
    let restore = sqlx::query(&format!(
        "WITH used AS ({MONTH_USED_SQL})
         SELECT s.tenant_id, s.user_id, s.app_id, s.ingress_id
         FROM quota_suspensions s
         LEFT JOIN used u
           ON u.tenant_id = s.tenant_id
          AND u.user_id = s.user_id
          AND u.app_id = s.app_id
         WHERE u.limit_bytes IS NULL OR u.used_bytes < u.limit_bytes
         ORDER BY s.tenant_id, s.user_id, s.app_id, s.ingress_id"
    ))
    .fetch_all(pool)
    .await?;

    Ok(QuotaEnforcementPlan {
        suspend: suspend.iter().map(change_from_row).collect::<Result<_>>()?,
        restore: restore.iter().map(change_from_row).collect::<Result<_>>()?,
    })
}

/// Apply the operator side of the quota state machine before changing a grant.
///
/// Quotas are hard limits. Re-enabling while still over quota is rejected immediately rather
/// than creating a short window until the next worker round. An explicit manual disable clears a
/// system suspension even where the grant is already absent: that records that the operator, not
/// the quota worker, now owns the disabled state, so a later month reset must not restore it.
pub(crate) async fn prepare_operator_grant_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    app_id: &str,
    ingress_id: &str,
    enabled: bool,
) -> Result<()> {
    if actor.operator_id() == QUOTA_ACTOR {
        return Ok(());
    }

    if enabled {
        let exceeded: bool = sqlx::query_scalar(&format!(
            "WITH used AS ({MONTH_USED_SQL})
             SELECT EXISTS (
                 SELECT 1 FROM used
                 WHERE tenant_id = $1 AND user_id = $2 AND app_id = $3
                   AND used_bytes >= limit_bytes
             )"
        ))
        .bind(tenant_id)
        .bind(user_id)
        .bind(app_id)
        .fetch_one(&mut **tx)
        .await?;
        if exceeded {
            return Err(StoreError::Unsupported(format!(
                "grant {tenant_id}/{user_id}/{app_id}/{ingress_id} cannot be enabled while its monthly quota is exhausted; raise or remove the quota first"
            )));
        }
    }

    sqlx::query(
        "DELETE FROM quota_suspensions
         WHERE tenant_id = $1 AND user_id = $2 AND app_id = $3 AND ingress_id = $4",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(app_id)
    .bind(ingress_id)
    .execute(&mut **tx)
    .await?;
    // Suspension ownership is operational bookkeeping, not model content. Clearing it alone must
    // not advance the model revision; the surrounding grant writer counts only an actual grant
    // row change.
    Ok(())
}

async fn has_suspensions(pool: &PgPool) -> Result<bool> {
    Ok(sqlx::query("SELECT 1 FROM quota_suspensions LIMIT 1")
        .fetch_optional(pool)
        .await?
        .is_some())
}

fn change_from_row(row: &sqlx::postgres::PgRow) -> Result<QuotaGrantChange> {
    Ok(QuotaGrantChange {
        tenant_id: row.try_get("tenant_id")?,
        user_id: row.try_get("user_id")?,
        app_id: row.try_get("app_id")?,
        ingress_id: row.try_get("ingress_id")?,
    })
}

/// Run one round. Account → edit the model → gate → release.
pub async fn enforce_quotas(pool: &PgPool) -> Result<QuotaEnforcementOutcome> {
    let actor = AdminContext::system_admin(QUOTA_ACTOR);
    let plan = plan_quota_enforcement(pool).await?;

    // A round that changed no model and holds no suspension records goes no further. Not for
    // economy but of necessity: `create_deployment` ships the whole current revision, so going
    // on would push out changes others left unreleased — while quota enforcement did nothing.
    // Conversely, any surviving suspension record means continuing: the previous round may have
    // been stopped by the gate, or the release itself may have failed, and this round retries;
    // where there really is nothing to push, changed_targets reaches 0 and it exits.
    if plan.is_empty() && !has_suspensions(pool).await? {
        return Ok(QuotaEnforcementOutcome {
            suspended: 0,
            restored: 0,
            revision_id: current_revision(pool).await?,
            deployment_id: None,
            deferred: Vec::new(),
        });
    }

    let (revision_id, applied) = if plan.is_empty() {
        (
            current_revision(pool).await?,
            QuotaEnforcementPlan::default(),
        )
    } else {
        apply_quota_changes(pool, &actor, &plan).await?
    };
    // Planning happens before the model lock. If a newer operator action made every planned
    // change stale and no older suspension still needs shipping, stop here rather than publishing
    // somebody else's pending model work under the quota worker's name.
    if applied.is_empty() && !has_suspensions(pool).await? {
        return Ok(QuotaEnforcementOutcome {
            suspended: 0,
            restored: 0,
            revision_id,
            deployment_id: None,
            deferred: Vec::new(),
        });
    }

    // What ships is a grants deployment: `narrow_to_kind(Grants)` keeps only the machines
    // needing nothing but a list sync this round, and marks all three artifacts Unmanaged. So it
    // structurally cannot push configuration and needs no runtime gate checking whether other
    // actions slipped in. Configuration changes others left in the current revision are
    // therefore unaffected — their machines' actions carry ApplyXray and the like, and never
    // enter this deployment at all.
    let full_plan = plan_full_deployment(pool, &actor, revision_id).await?;
    // A machine whose list must change while it also owes configuration changes cannot enter a
    // grants deployment. The round after the configuration deployment lands picks it up
    // naturally — this only records it, so that the log can say what is being waited on.
    let deferred = full_plan
        .targets
        .iter()
        .filter(|target| {
            target.status == PlannedTargetStatus::Pending
                && target.actions.contains(&PlannedAction::SyncGrants)
                && !can_sync_grants_now(target)
        })
        .map(|target| target.node_id.clone())
        .collect::<Vec<_>>();

    let deployment_plan = narrow_to_kind(full_plan, DeploymentKind::Grants);
    if deployment_plan.summary.changed_targets == 0 {
        return Ok(QuotaEnforcementOutcome {
            suspended: applied.suspend.len(),
            restored: applied.restore.len(),
            revision_id,
            deployment_id: None,
            deferred,
        });
    }

    let note = enforcement_note(&applied);
    let created = create_deployment(
        pool,
        &actor,
        CreateDeploymentRequest {
            revision_id,
            // One revision ships once. Colliding with another grants deployment still in flight
            // fails this round outright, and the next retries with the same key rather than
            // piling up a string of half-finished releases. Configuration deployments are not on
            // this lock.
            idempotency_key: format!("quota:{revision_id}"),
            actor: Some(QUOTA_ACTOR.to_owned()),
            note: Some(note),
            kind: DeploymentKind::Grants,
        },
    )
    .await?;

    Ok(QuotaEnforcementOutcome {
        suspended: applied.suspend.len(),
        restored: applied.restore.len(),
        revision_id,
        deployment_id: Some(created.deployment_id),
        deferred,
    })
}

/// The note for one round of quota enforcement. The revision history and the deployment share
/// this one sentence — they describe the same thing, and writing it twice eventually drifts.
///
/// It names who, not just how many: this is a release the system initiated itself, and a count
/// alone leaves nobody able to say afterwards from the UI whom it touched. The details are in
/// the grants artifact's diff, but reaching them means opening the detail view, expanding the
/// artifact, and reading a JSON-RPC message, whereas this sentence should answer it in the
/// list.
fn enforcement_note(plan: &QuotaEnforcementPlan) -> String {
    let mut parts = Vec::new();
    if !plan.suspend.is_empty() {
        parts.push(format!(
            "撤 {} 条 · {}",
            plan.suspend.len(),
            describe_grant_changes(&plan.suspend)
        ));
    }
    if !plan.restore.is_empty() {
        parts.push(format!(
            "恢复 {} 条 · {}",
            plan.restore.len(),
            describe_grant_changes(&plan.restore)
        ));
    }
    if parts.is_empty() {
        // Unreachable: every call site is past `plan.is_empty()`. A fallback sentence remains,
        // so that a new path arriving one day does not leave an unexplained revision in the
        // history.
        return "配额执行：没有要改的授权".to_owned();
    }
    format!("配额执行：{}", parts.join("；"))
}

/// Fold a run of grant changes into one sentence: grouped by person and project, with ingresses
/// merged into parentheses.
///
/// Not listed one by one because a person usually holds several ingresses within a project, and
/// listing them yields `alice@platform → app-x(i1), alice@platform → app-x(i2)` — the same thing
/// said twice. Beyond three groups it truncates: the note is for scanning in a list, and the
/// complete details remain in the artifact diff.
fn describe_grant_changes(changes: &[QuotaGrantChange]) -> String {
    const GROUPS_IN_NOTE: usize = 3;

    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for change in changes {
        let key = format!(
            "{}@{} → {}",
            change.user_id, change.tenant_id, change.app_id
        );
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, ingresses)) => ingresses.push(change.ingress_id.clone()),
            None => groups.push((key, vec![change.ingress_id.clone()])),
        }
    }
    // Sort once: the order of `plan`'s two Vecs comes from SQL, one batch of changes can come
    // back in a different order under a different query plan, and "the same thing reading
    // differently each time" is the hardest kind of inconsistency to chase.
    for (_, ingresses) in groups.iter_mut() {
        ingresses.sort();
    }
    groups.sort_by(|a, b| a.0.cmp(&b.0));

    let total = groups.len();
    let shown = groups
        .iter()
        .take(GROUPS_IN_NOTE)
        .map(|(key, ingresses)| format!("{key}({})", ingresses.join("、")))
        .collect::<Vec<_>>()
        .join("、");
    if total > GROUPS_IN_NOTE {
        format!("{shown} 等 {total} 项")
    } else {
        shown
    }
}

/// The round's model changes compose into one revision. Going through `upsert_grant` one at a
/// time would make one person with three ingresses three revisions, and quota adjustments would
/// flood the history.
async fn apply_quota_changes(
    pool: &PgPool,
    actor: &AdminContext,
    plan: &QuotaEnforcementPlan,
) -> Result<(u64, QuotaEnforcementPlan)> {
    let note = enforcement_note(plan);
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let mut changed = false;
    let mut applied = QuotaEnforcementPlan::default();

    for item in &plan.suspend {
        let (_, c) =
            upsert_grant_tx(&mut tx, actor, revision_id, grant_request(item, false)).await?;
        changed |= c;
        // The plan was read before taking the model lock. If an operator revoked this grant in
        // between, recording a suspension now would make a later quota reset undo that manual
        // decision. Only the transaction which actually removes the grant owns a suspension.
        if c {
            record_suspension(&mut tx, item, revision_id).await?;
            applied.suspend.push(item.clone());
        }
    }

    for item in &plan.restore {
        // The ingress is gone: the grant went with it and there is nothing to restore. Clear
        // the suspension record and be done — kept, it produces a NotFound from
        // `ensure_ingress_in_app_tx` every round.
        if !ingress_in_app(&mut tx, &item.app_id, &item.ingress_id).await? {
            clear_suspension(&mut tx, item).await?;
            continue;
        }
        // Likewise, an operator may have manually disabled this grant and cleared the suspension
        // after planning. Claim the suspension under the model lock before restoring; no row now
        // means the operator's newer decision wins.
        if !clear_suspension(&mut tx, item).await? {
            continue;
        }
        let (_, c) =
            upsert_grant_tx(&mut tx, actor, revision_id, grant_request(item, true)).await?;
        changed |= c;
        if c {
            applied.restore.push(item.clone());
        }
    }

    if changed {
        sqlx::query("UPDATE revisions SET note = $2 WHERE id = $1")
            .bind(i64::try_from(revision_id).unwrap_or(i64::MAX))
            .bind(enforcement_note(&applied))
            .execute(&mut *tx)
            .await?;
    }
    let revision_id =
        commit_revision_without_grant_automation(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;
    Ok((revision_id, applied))
}

fn grant_request(item: &QuotaGrantChange, enabled: bool) -> CreateGrantRequest {
    CreateGrantRequest {
        app_id: item.app_id.clone(),
        tenant_id: item.tenant_id.clone(),
        user_id: item.user_id.clone(),
        ingress_id: item.ingress_id.clone(),
        enabled,
        note: None,
    }
}

async fn record_suspension(
    tx: &mut Transaction<'_, Postgres>,
    item: &QuotaGrantChange,
    revision_id: u64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO quota_suspensions (tenant_id, user_id, app_id, ingress_id, suspended_revision)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (tenant_id, user_id, app_id, ingress_id) DO NOTHING",
    )
    .bind(&item.tenant_id)
    .bind(&item.user_id)
    .bind(&item.app_id)
    .bind(&item.ingress_id)
    .bind(i64::try_from(revision_id).unwrap_or(i64::MAX))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn clear_suspension(
    tx: &mut Transaction<'_, Postgres>,
    item: &QuotaGrantChange,
) -> Result<bool> {
    Ok(sqlx::query(
        "DELETE FROM quota_suspensions
         WHERE tenant_id = $1 AND user_id = $2 AND app_id = $3 AND ingress_id = $4",
    )
    .bind(&item.tenant_id)
    .bind(&item.user_id)
    .bind(&item.app_id)
    .bind(&item.ingress_id)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0)
}

async fn ingress_in_app(
    tx: &mut Transaction<'_, Postgres>,
    app_id: &str,
    ingress_id: &str,
) -> Result<bool> {
    Ok(
        sqlx::query("SELECT 1 FROM ingresses WHERE id = $1 AND app_id = $2")
            .bind(ingress_id)
            .bind(app_id)
            .fetch_optional(&mut **tx)
            .await?
            .is_some(),
    )
}

#[cfg(test)]
mod tests {
    use super::{enforcement_note, QuotaEnforcementPlan, QuotaGrantChange};

    fn change(user: &str, app: &str, ingress: &str) -> QuotaGrantChange {
        QuotaGrantChange {
            tenant_id: "platform".to_owned(),
            user_id: user.to_owned(),
            app_id: app.to_owned(),
            ingress_id: ingress.to_owned(),
        }
    }

    #[test]
    fn note_names_who_and_where() {
        let plan = QuotaEnforcementPlan {
            suspend: vec![
                change("alice", "app-jp", "i2"),
                change("alice", "app-jp", "i1"),
            ],
            restore: Vec::new(),
        };
        assert_eq!(
            enforcement_note(&plan),
            "配额执行：撤 2 条 · alice@platform → app-jp(i1、i2)"
        );
    }

    #[test]
    fn note_keeps_both_halves() {
        let plan = QuotaEnforcementPlan {
            suspend: vec![change("alice", "app-jp", "i1")],
            restore: vec![change("bob", "app-tw", "i1")],
        };
        assert_eq!(
            enforcement_note(&plan),
            "配额执行：撤 1 条 · alice@platform → app-jp(i1)；恢复 1 条 · bob@platform → app-tw(i1)"
        );
    }

    // Revoking dozens of people in one round must not swell a list row into a paragraph. The
    // count stays accurate.
    #[test]
    fn note_truncates_beyond_three_groups() {
        let plan = QuotaEnforcementPlan {
            suspend: (0..5)
                .map(|i| change(&format!("u{i}"), "app-jp", "i1"))
                .collect(),
            restore: Vec::new(),
        };
        assert_eq!(
            enforcement_note(&plan),
            "配额执行：撤 5 条 · u0@platform → app-jp(i1)、u1@platform → app-jp(i1)、u2@platform → app-jp(i1) 等 5 项"
        );
    }

    // The order comes from SQL, and one batch of changes can come back differently under a
    // different query plan
    #[test]
    fn note_is_order_independent() {
        let a = QuotaEnforcementPlan {
            suspend: vec![
                change("bob", "app-tw", "i1"),
                change("alice", "app-jp", "i2"),
            ],
            restore: Vec::new(),
        };
        let b = QuotaEnforcementPlan {
            suspend: vec![
                change("alice", "app-jp", "i2"),
                change("bob", "app-tw", "i1"),
            ],
            restore: Vec::new(),
        };
        assert_eq!(enforcement_note(&a), enforcement_note(&b));
    }
}
