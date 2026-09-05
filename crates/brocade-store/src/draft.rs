//! Draft commit: a run of edits, one write, one revision.
//!
//! What this replaced was "editing is committing" — one changed field stamping one revision,
//! with no draft state. That decision proved too costly in practice: the model's write
//! interfaces are pure upserts (`steps` has no delete at all), so "undo = edit back the other
//! way" is simply impossible for a large share of changes; and one UI action often means more
//! than one write (changing a forwarding rule also writes the peer's relay port), leaving
//! revision numbers out of step with people's mental actions.
//!
//! The layering is now three stages:
//!
//! ```text
//! front-end draft (a run of ModelOps) ──commit──▶ revision ──release──▶ deployment
//! ```
//!
//! The permission boundary is unmoved and still falls on the deployment: committing a draft
//! still requires editor, creating a deployment still requires publisher.
//!
//! # Why the preview runs on the server
//!
//! The easiest thing to get wrong about a draft state is who computes what it will look like
//! afterwards. Reimplementing the upsert semantics in the front end is the most direct
//! approach and the worst — that is a mirror model that drifts, and it cannot compute what the
//! server generates (`accept.uuid`, the REALITY key pair, and `hop_in`'s material inheritance
//! rules all live in store).
//!
//! So `preview` takes the real write path: open a transaction, execute the run as usual, read
//! back the snapshot and the compilation diagnostics, then `ROLLBACK`. There is one copy of
//! the semantics, and it is the `*_tx` functions in `console.rs`. The cost is a write
//! transaction per preview — no problem at the console's concurrency, and not worth trading
//! correctness for.

use brocade_core::model::{DestMatch, EgressDnsResolution, ModelSettings};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};

use crate::console::{
    ArtifactContent, ArtifactIndex, CompileView, ConsoleSnapshot, CreateAppRequest,
    CreateChainRequest, CreateFrontRequest, CreateGrantRequest, CreateIngressRequest,
    CreateTenantRequest, CreateUserRequest, PutStepRequest, UpdateNodeRequest,
    UpdateNodeStatusRequest, UpdateUserStatusRequest, UpsertExternalOutboundRequest,
};
use crate::{AdminContext, Result, StoreError};

/// One edit within a draft. Each corresponds to an existing write interface — adding no
/// semantics, merely turning "which interface with which arguments" into data that can be
/// accumulated and replayed as a batch.
// Not boxed, though the arms differ by several hundred bytes. This type is the wire form of a
// draft: it is serialized to the console and back, and a draft holds a handful of edits, not a
// stream of them. Boxing one arm to even out the sizes would put an allocation inside a type whose
// whole purpose is being plain data.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ModelOp {
    CreateApp {
        app: CreateAppRequest,
    },
    UpsertApp {
        app: CreateAppRequest,
    },
    /// Replace the complete top-level line order. The UI emits one final ordering document after
    /// a drag, so a draft never has to replay every intermediate pointer crossing.
    ReorderApps {
        ids: Vec<String>,
    },
    /// Replace the complete chain order within one app. IDs and every usage/probe reference stay
    /// stable; only app-local positions are rewritten.
    ReorderChains {
        app_id: String,
        ids: Vec<String>,
    },
    UpsertExternalOutbound {
        outbound: UpsertExternalOutboundRequest,
    },
    CreateChain {
        app_id: String,
        chain: CreateChainRequest,
    },
    UpsertChain {
        app_id: String,
        chain: CreateChainRequest,
    },
    UpsertFront {
        app_id: String,
        front: CreateFrontRequest,
    },
    CreateIngress {
        app_id: String,
        ingress: CreateIngressRequest,
    },
    UpsertIngress {
        app_id: String,
        ingress: CreateIngressRequest,
    },
    PutStep {
        app_id: String,
        chain_id: String,
        node_id: String,
        step: PutStepRequest,
    },
    /// Set or remove one machine-owned DNS policy. `None` removes the policy; route tables only
    /// activate its selector and never own this operation.
    SetNodeEgressDns {
        node_id: String,
        selector: DestMatch,
        resolution: Option<EgressDnsResolution>,
    },
    /// Replace the complete machine-wide resolver priority. This changes Xray DNS server order,
    /// not the top-to-bottom route order of any chain.
    ReorderNodeEgressDns {
        node_id: String,
        selectors: Vec<DestMatch>,
    },
    DeleteStep {
        app_id: String,
        chain_id: String,
        node_id: String,
    },
    DeleteChain {
        app_id: String,
        chain_id: String,
    },
    /// Drop the steps on this chain unreachable from its head. It comes last when the console
    /// saves a whole rule tree — the test needs the chain's complete rule table, while a draft
    /// replays operation by operation and every intermediate state is incomplete (see
    /// `prune_chain` in console.rs).
    PruneChain {
        app_id: String,
        chain_id: String,
    },
    UpsertGrant {
        grant: CreateGrantRequest,
    },
    CreateTenant {
        tenant: CreateTenantRequest,
    },
    CreateUser {
        user: CreateUserRequest,
    },
    RotateUserUuid {
        tenant_id: String,
        user_id: String,
    },
    UpdateUserStatus {
        tenant_id: String,
        user_id: String,
        status: UpdateUserStatusRequest,
    },
    UpdateNode {
        node_id: String,
        node: UpdateNodeRequest,
    },
    UpdateNodeStatus {
        node_id: String,
        status: UpdateNodeStatusRequest,
    },
    #[serde(rename = "set_wireguard_link_disabled")]
    SetWireGuardLinkDisabled {
        a: String,
        b: String,
        disabled: bool,
    },
    UpdateSettings {
        settings: ModelSettings,
    },
}

impl ModelOp {
    /// A human-facing sentence. Assembled into the revision note on commit, and used to list
    /// drafts in the UI.
    pub fn describe(&self) -> String {
        match self {
            ModelOp::CreateApp { app } | ModelOp::UpsertApp { app } => {
                format!("视图 {}", app.id)
            }
            ModelOp::ReorderApps { .. } => "调整线路顺序".to_owned(),
            ModelOp::ReorderChains { app_id, .. } => format!("调整链顺序 {app_id}"),
            ModelOp::UpsertExternalOutbound { outbound } => {
                format!("隧道 {}/{}", outbound.tenant_id, outbound.id)
            }
            ModelOp::CreateChain { app_id, chain } | ModelOp::UpsertChain { app_id, chain } => {
                format!("链 {app_id}/{}", chain.id)
            }
            ModelOp::UpsertFront { app_id, front } => format!("前置组 {app_id}/{}", front.id),
            ModelOp::CreateIngress { app_id, ingress }
            | ModelOp::UpsertIngress { app_id, ingress } => {
                format!("接入面 {app_id}/{}", ingress.id)
            }
            ModelOp::PutStep {
                chain_id, node_id, ..
            } => format!("规则 {chain_id}/{node_id}"),
            ModelOp::SetNodeEgressDns {
                node_id, selector, ..
            } => format!("机器 DNS 策略 {node_id}/{}", selector_summary(selector)),
            ModelOp::ReorderNodeEgressDns { node_id, .. } => {
                format!("机器 DNS 策略顺序 {node_id}")
            }
            ModelOp::DeleteStep {
                chain_id, node_id, ..
            } => {
                format!("删除规则 {chain_id}/{node_id}")
            }
            ModelOp::DeleteChain { app_id, chain_id } => format!("删除链 {app_id}/{chain_id}"),
            ModelOp::PruneChain { app_id, chain_id } => {
                format!("清理落单节点 {app_id}/{chain_id}")
            }
            ModelOp::UpsertGrant { grant } => format!(
                "{} 授权 {}/{} → {}",
                if grant.enabled { "开" } else { "撤" },
                grant.tenant_id,
                grant.user_id,
                grant.ingress_id
            ),
            ModelOp::CreateTenant { tenant } => format!("租户 {}", tenant.id),
            ModelOp::CreateUser { user } => format!("用户 {}/{}", user.tenant_id, user.id),
            ModelOp::RotateUserUuid { tenant_id, user_id } => {
                format!("轮换 uuid {tenant_id}/{user_id}")
            }
            ModelOp::UpdateUserStatus {
                tenant_id,
                user_id,
                status,
            } => format!("用户 {tenant_id}/{user_id} 置为 {}", status.status),
            ModelOp::UpdateNode { node_id, .. } => format!("机器 {node_id}"),
            ModelOp::UpdateNodeStatus { node_id, status } => {
                format!("机器 {node_id} 置为 {}", status.status)
            }
            ModelOp::SetWireGuardLinkDisabled { a, b, disabled } => format!(
                "{} WireGuard 链路 {a} ↔ {b}",
                if *disabled { "禁用" } else { "恢复" }
            ),
            ModelOp::UpdateSettings { .. } => "全局设置".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyDraftResult {
    pub revision_id: u64,
    /// How many operations actually changed the model. Where all are no-ops, `revision_id`
    /// falls back to the previous number (the rollback logic in `commit_revision`) and this is
    /// 0.
    pub changed: usize,
    pub client_config: crate::ClientConfigCommitResult,
}

// Outbound only: `CompileView` is a projection of a compilation result, has no `Deserialize`,
// and should not have one — it is something the server computed, and deserializing a copy from
// outside has no legitimate use.
#[derive(Debug, Clone, Serialize)]
pub struct DraftPreview {
    /// The model snapshot with the whole draft applied (already redacted). The UI renders
    /// straight from it.
    pub snapshot: ConsoleSnapshot,
    /// The same snapshot's compilation result: diagnostics, summary, `can_publish`.
    pub compile: CompileView,
    /// The artifact index computed from the same snapshot (the listing and sha256 only, no
    /// contents).
    ///
    /// Artifacts are a pure function of the snapshot, so a draft has them too — which
    /// configurations this run of edits would change, and into what, is exactly what one most
    /// wants to see before committing. Where compilation fails this is empty
    /// (`artifact_index` only produces output when `can_publish`), which is the truth of it:
    /// nothing can be produced.
    pub artifacts: ArtifactIndex,
}

/// Execute one operation inside the current transaction. Returns whether it actually changed
/// the model.
async fn apply_op(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    op: ModelOp,
) -> Result<bool> {
    use crate::console as c;
    Ok(match op {
        ModelOp::CreateApp { app } => c::create_app_tx(tx, actor, revision_id, app).await?,
        ModelOp::UpsertApp { app } => c::upsert_app_tx(tx, actor, revision_id, app).await?,
        ModelOp::ReorderApps { ids } => c::reorder_apps_tx(tx, actor, ids).await?,
        ModelOp::ReorderChains { app_id, ids } => {
            c::reorder_chains_tx(tx, actor, app_id, ids).await?
        }
        ModelOp::UpsertExternalOutbound { outbound } => {
            c::upsert_external_outbound_tx(tx, actor, revision_id, outbound).await?
        }
        ModelOp::CreateChain { app_id, chain } => {
            c::create_chain_tx(tx, actor, revision_id, &app_id, chain)
                .await?
                .1
        }
        ModelOp::UpsertChain { app_id, chain } => {
            c::upsert_chain_tx(tx, actor, revision_id, &app_id, chain)
                .await?
                .1
        }
        ModelOp::UpsertFront { app_id, front } => {
            c::upsert_front_tx(tx, actor, revision_id, &app_id, front)
                .await?
                .1
        }
        ModelOp::CreateIngress { app_id, ingress } => {
            c::create_ingress_tx(tx, actor, revision_id, &app_id, ingress)
                .await?
                .1
        }
        ModelOp::UpsertIngress { app_id, ingress } => {
            c::upsert_ingress_tx(tx, actor, revision_id, &app_id, ingress)
                .await?
                .1
        }
        ModelOp::PutStep {
            app_id,
            chain_id,
            node_id,
            step,
        } => c::put_step_tx(tx, actor, revision_id, &app_id, &chain_id, &node_id, step).await?,
        ModelOp::SetNodeEgressDns {
            node_id,
            selector,
            resolution,
        } => {
            crate::egress_dns::set_policy_tx(tx, actor, revision_id, &node_id, selector, resolution)
                .await?
        }
        ModelOp::ReorderNodeEgressDns { node_id, selectors } => {
            crate::egress_dns::reorder_policies_tx(tx, actor, &node_id, selectors).await?
        }
        ModelOp::DeleteStep {
            app_id,
            chain_id,
            node_id,
        } => {
            c::delete_step_tx(tx, actor, revision_id, &app_id, &chain_id, &node_id)
                .await?
                .changed
        }
        ModelOp::DeleteChain { app_id, chain_id } => {
            c::delete_whole_chain_tx(tx, actor, &app_id, &chain_id)
                .await?
                .changed
        }
        ModelOp::PruneChain { app_id, chain_id } => {
            !c::prune_chain_tx(tx, actor, &app_id, &chain_id)
                .await?
                .is_empty()
        }
        ModelOp::UpsertGrant { grant } => {
            c::upsert_grant_tx(tx, actor, revision_id, grant).await?.1
        }
        ModelOp::CreateTenant { tenant } => {
            c::create_tenant_tx(tx, actor, revision_id, tenant).await?
        }
        ModelOp::CreateUser { user } => c::create_user_tx(tx, actor, revision_id, user).await?.1,
        ModelOp::RotateUserUuid { tenant_id, user_id } => {
            c::rotate_user_uuid_tx(tx, actor, revision_id, &tenant_id, &user_id)
                .await?
                .1
        }
        ModelOp::UpdateUserStatus {
            tenant_id,
            user_id,
            status,
        } => c::update_user_status_tx(tx, actor, &tenant_id, &user_id, status).await?,
        ModelOp::UpdateNode { node_id, node } => {
            c::update_node_tx(tx, actor, revision_id, &node_id, node).await?
        }
        ModelOp::UpdateNodeStatus { node_id, status } => {
            let _ = (node_id, status);
            return Err(StoreError::Unsupported(
                "node retirement and reactivation are operational actions and cannot be stored in a draft"
                    .to_owned(),
            ));
        }
        ModelOp::SetWireGuardLinkDisabled { a, b, disabled } => {
            crate::settings::set_wireguard_link_disabled_tx(tx, actor, a, b, disabled).await?
        }
        ModelOp::UpdateSettings { mut settings } => {
            // Disabled mesh links have their own pair-scoped operation. Preserve the transaction's
            // current list here so a whole-settings form saved later in the same draft cannot
            // erase an earlier link change (and the reverse operation order behaves identically).
            settings.overlay.disabled_links = crate::settings::load_settings_tx(tx)
                .await?
                .overlay
                .disabled_links;
            crate::settings::update_settings_tx(tx, actor, settings)
                .await?
                .1
        }
    })
}

fn selector_summary(selector: &DestMatch) -> String {
    match selector {
        DestMatch::DomainSuffix(values) => format!("domain:{}", values.join(",")),
        DestMatch::DomainKeyword(values) => format!("keyword:{}", values.join(",")),
        DestMatch::DomainRegex(value) => format!("regex:{value}"),
        DestMatch::Geosite(values) => format!("geosite:{}", values.join(",")),
        _ => "不支持的匹配条件".to_owned(),
    }
}

/// Commit a draft: a run of operations completing in one transaction, stamping one revision.
///
/// Any one of them failing rolls the batch back — half a rule table landing in the database is
/// the hardest kind of state to chase: the diagnostics point at something nobody ever intended
/// to write.
pub async fn apply_ops(
    pool: &PgPool,
    actor: &AdminContext,
    ops: Vec<ModelOp>,
    note: Option<String>,
) -> Result<ApplyDraftResult> {
    let note = crate::console::note_or(note.as_deref(), || match ops.len() {
        0 => "空提交".to_owned(),
        1 => format!("提交：{}", ops[0].describe()),
        n => format!("提交 {n} 处改动：{}", summarize(&ops)),
    });

    let mut tx = pool.begin().await?;
    let previous = crate::console::lock_control_state(&mut tx).await?;
    let revision_id = crate::console::insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let mut changed = 0usize;
    for op in ops {
        if apply_op(&mut tx, actor, revision_id, op).await? {
            changed += 1;
        }
    }
    let revision_id =
        crate::console::commit_revision(&mut tx, revision_id, previous, changed > 0).await?;
    let mut client_config =
        crate::subscription_client::commit_result_tx(&mut tx, revision_id).await?;
    if changed == 0 {
        client_config.status = crate::ClientConfigCommitStatus::Unchanged;
    }
    tx.commit().await?;

    Ok(ApplyDraftResult {
        revision_id,
        changed,
        client_config,
    })
}

/// Preview a draft: execute as usual, read the result, then roll back. Not one byte in the
/// database changes.
pub async fn preview_ops(
    pool: &PgPool,
    actor: &AdminContext,
    ops: Vec<ModelOp>,
) -> Result<DraftPreview> {
    let snapshot = draft_snapshot(pool, actor, ops).await?;
    Ok(DraftPreview {
        compile: crate::console::compile_view_of(&snapshot)?,
        artifacts: crate::console::artifact_index_of(&snapshot)?,
        snapshot: crate::console::snapshot_view(snapshot)?,
    })
}

/// Run a draft into a transaction, read the snapshot, roll back. Not one byte in the database
/// changes.
async fn draft_snapshot(
    pool: &PgPool,
    actor: &AdminContext,
    ops: Vec<ModelOp>,
) -> Result<brocade_core::model::ModelSnapshot> {
    let mut tx = pool.begin().await?;
    // Borrow the current revision number rather than opening a new one. Every table's
    // `created_revision` has a foreign key into revisions, so this run needs a number that
    // exists; and inserting a row will not do — Postgres sequences do not participate in
    // rollback, so the rollback removes only the row while `nextval` has advanced
    // permanently. The UI previews on every keystroke, and the revision numbers would gap at a
    // visible rate. These rows vanish with the transaction anyway, so whatever
    // `created_revision` holds is seen by nobody.
    let revision_id = crate::console::lock_control_state(&mut tx).await?;
    for op in ops {
        apply_op(&mut tx, actor, revision_id, op).await?;
    }
    let snapshot = crate::materialize::load_current_snapshot_tx(&mut tx).await?;
    // Read and discard. Not a commit — a preview is read-only in meaning, whatever write
    // permissions the caller holds.
    tx.rollback().await?;

    Ok(crate::console::scope_snapshot(actor, snapshot))
}

/// The contents of one artifact within a draft. The index supplies the listing and sha256, and
/// contents are fetched only when somebody opens one — packing a dozen artifacts into every
/// preview response would ship them all on every keystroke.
pub async fn preview_artifact(
    pool: &PgPool,
    actor: &AdminContext,
    ops: Vec<ModelOp>,
    target_kind: &str,
    target_id: &str,
    artifact_kind: &str,
) -> Result<ArtifactContent> {
    let snapshot = draft_snapshot(pool, actor, ops).await?;
    let self_signed_names = crate::cert::self_signed_certificate_names(pool).await?;
    // Always every family: a preview exists to be diffed against the committed revision, and a
    // narrowed side would report the entries it dropped as removals.
    crate::console::artifact_content_of(
        &snapshot,
        target_kind,
        target_id,
        artifact_kind,
        !actor.is_system_admin(),
        Default::default(),
        &self_signed_names,
    )
}

fn summarize(ops: &[ModelOp]) -> String {
    let mut out = ops
        .iter()
        .take(3)
        .map(ModelOp::describe)
        .collect::<Vec<_>>();
    if ops.len() > 3 {
        out.push(format!("等 {} 条", ops.len()));
    }
    out.join("、")
}

#[cfg(test)]
mod tests {
    use super::ModelOp;

    #[test]
    fn wireguard_link_operation_uses_the_frontend_wire_spelling() {
        let value = serde_json::json!({
            "op": "set_wireguard_link_disabled",
            "a": "akile-twlite2",
            "b": "akile-laxl2",
            "disabled": true
        });

        let operation: ModelOp = serde_json::from_value(value).unwrap();
        assert!(matches!(
            operation,
            ModelOp::SetWireGuardLinkDisabled {
                ref a,
                ref b,
                disabled: true
            } if a == "akile-twlite2" && b == "akile-laxl2"
        ));
        assert_eq!(
            serde_json::to_value(operation).unwrap()["op"],
            "set_wireguard_link_disabled"
        );
    }
}
