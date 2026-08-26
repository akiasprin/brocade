//! Artifact index and contents. Artifacts are a pure function of the snapshot — computed on
//! demand rather than looked up, which is how a draft can have artifacts too.
use brocade_core::artifacts::hy2_port_hop::Hy2PortHopArtifact;
use brocade_core::model::{IpFamily, ModelSnapshot};
use sqlx::PgPool;

use super::*;
use crate::{AdminContext, Result, StoreError};

pub async fn artifact_index(
    pool: &PgPool,
    actor: &AdminContext,
    revision: Option<u64>,
) -> Result<ArtifactIndex> {
    artifact_index_of(&load_scoped_snapshot(pool, actor, revision).await?)
}

/// Artifacts are a pure function of the snapshot — computed on demand rather than looked up.
/// That is how a draft can have them too: the preview reads the snapshot inside the
/// transaction it is about to roll back, and recomputing here yields "what this run of edits
/// would turn the artifacts into" (`draft.rs`).
pub(crate) fn artifact_index_of(snapshot: &ModelSnapshot) -> Result<ArtifactIndex> {
    let output = compile(snapshot);
    let mut artifacts = Vec::new();

    if output.summary.can_publish {
        for node in snapshot.nodes.iter().filter(|node| !node.retired) {
            if let Ok(plan) = output.project_node(&node.id) {
                let phantun = phantun::build(&plan);
                match &phantun {
                    brocade_core::artifacts::phantun::PhantunArtifact::Config(_) => {
                        push_node_artifact(
                            &mut artifacts,
                            &node.id,
                            "phantun",
                            json_format::phantun(&phantun),
                        );
                    }
                    brocade_core::artifacts::phantun::PhantunArtifact::Disabled { .. } => {
                        push_disabled_artifact(&mut artifacts, "node", &node.id, "phantun");
                    }
                }

                let wireguard = wireguard::build(&plan);
                match &wireguard {
                    brocade_core::artifacts::wireguard::WireGuardArtifact::Config(_) => {
                        push_node_artifact(
                            &mut artifacts,
                            &node.id,
                            "wireguard",
                            ini::wireguard(&wireguard),
                        );
                    }
                    brocade_core::artifacts::wireguard::WireGuardArtifact::Disabled { .. } => {
                        push_disabled_artifact(&mut artifacts, "node", &node.id, "wireguard");
                    }
                }

                let hy2_hop = hy2_port_hop::build(&plan);
                match &hy2_hop {
                    Hy2PortHopArtifact::Config(_) => {
                        let text = json_format::hy2_port_hop(&hy2_hop);
                        push_node_artifact(&mut artifacts, &node.id, "hy2_port_hop", text);
                    }
                    Hy2PortHopArtifact::Disabled { .. } => {
                        push_disabled_artifact(&mut artifacts, "node", &node.id, "hy2_port_hop");
                    }
                }

                let xray = xray::build(&plan);
                let xray_present = matches!(
                    &xray,
                    brocade_core::artifacts::xray::XrayArtifact::Config(_)
                );
                match &xray {
                    brocade_core::artifacts::xray::XrayArtifact::Config(_) => {
                        push_node_artifact(
                            &mut artifacts,
                            &node.id,
                            "xray",
                            json_format::xray(&xray),
                        );
                    }
                    brocade_core::artifacts::xray::XrayArtifact::Disabled { .. } => {
                        push_disabled_artifact(&mut artifacts, "node", &node.id, "xray");
                    }
                }

                if xray_present {
                    push_node_artifact(
                        &mut artifacts,
                        &node.id,
                        "grants",
                        json_format::grant_sync_batch(&grants::build(&plan)),
                    );
                } else {
                    push_disabled_artifact(&mut artifacts, "node", &node.id, "grants");
                }
            }
        }

        for user in &snapshot.users {
            if let Ok(plan) = output.project_user(&user.tenant, &user.id) {
                let subscription = subscription::build(&plan);
                push_user_artifact(
                    &mut artifacts,
                    &format!("{}:{}", user.tenant, user.id),
                    "uri",
                    uri::subscription(&subscription),
                );
                push_user_artifact(
                    &mut artifacts,
                    &format!("{}:{}", user.tenant, user.id),
                    "clash",
                    yaml::clash_subscription(&subscription),
                );
            }
        }
    }

    artifacts.sort_by(|a, b| {
        (
            a.target_kind.as_str(),
            a.target_id.as_str(),
            a.artifact_kind.as_str(),
        )
            .cmp(&(
                b.target_kind.as_str(),
                b.target_id.as_str(),
                b.artifact_kind.as_str(),
            ))
    });

    Ok(ArtifactIndex {
        revision: snapshot.revision,
        artifacts,
    })
}

pub async fn artifact_content(
    pool: &PgPool,
    actor: &AdminContext,
    revision: Option<u64>,
    target_kind: &str,
    target_id: &str,
    artifact_kind: &str,
    family: Option<IpFamily>,
) -> Result<ArtifactContent> {
    let snapshot = load_scoped_snapshot(pool, actor, revision).await?;
    artifact_content_of(
        &snapshot,
        target_kind,
        target_id,
        artifact_kind,
        !actor.is_system_admin(),
        family,
    )
}

/// `redact` says whether this copy has its private keys masked. It cannot be derived from the
/// snapshot — the snapshot is already scoped per tenant, but scoping and redaction are two
/// different things: a tenant administrator can see their own machine and still should not see
/// its wg private key. There is one test: whether the caller is a system-admin.
///
/// `family` narrows a user subscription to one address family, for handing to a client that can
/// only reach one of them. `None` is every family, and it is what the fleet itself is served —
/// nothing but this console's read path passes anything else.
pub(crate) fn artifact_content_of(
    snapshot: &ModelSnapshot,
    target_kind: &str,
    target_id: &str,
    artifact_kind: &str,
    redact: bool,
    family: Option<IpFamily>,
) -> Result<ArtifactContent> {
    let target_kind = required_text(target_kind, "target_kind")?;
    let target_id = required_text(target_id, "target_id")?;
    let artifact_kind = required_text(artifact_kind, "artifact_kind")?;
    let output = compile(snapshot);
    output.ensure_publishable().map_err(|blocked| {
        StoreError::InvalidData(format!("revision is not publishable: {blocked:?}"))
    })?;

    match (target_kind.as_str(), artifact_kind.as_str()) {
        ("node", "phantun") => {
            ensure_node_visible(snapshot, &target_id)?;
            let plan = output.project_node(&target_id).map_err(|blocked| {
                StoreError::InvalidData(format!("cannot project node {target_id}: {blocked:?}"))
            })?;
            let artifact = phantun::build(&plan);
            match &artifact {
                brocade_core::artifacts::phantun::PhantunArtifact::Config(_) => {
                    artifact_content_from_text(
                        snapshot.revision,
                        &target_kind,
                        &target_id,
                        &artifact_kind,
                        json_format::phantun(&artifact),
                        redact,
                    )
                }
                brocade_core::artifacts::phantun::PhantunArtifact::Disabled { .. } => {
                    Ok(disabled_artifact_content(
                        snapshot.revision,
                        &target_kind,
                        &target_id,
                        &artifact_kind,
                    ))
                }
            }
        }
        ("node", "wireguard") => {
            ensure_node_visible(snapshot, &target_id)?;
            let plan = output.project_node(&target_id).map_err(|blocked| {
                StoreError::InvalidData(format!("cannot project node {target_id}: {blocked:?}"))
            })?;
            let artifact = wireguard::build(&plan);
            match &artifact {
                brocade_core::artifacts::wireguard::WireGuardArtifact::Config(_) => {
                    artifact_content_from_text(
                        snapshot.revision,
                        &target_kind,
                        &target_id,
                        &artifact_kind,
                        ini::wireguard(&artifact),
                        redact,
                    )
                }
                brocade_core::artifacts::wireguard::WireGuardArtifact::Disabled { .. } => {
                    Ok(disabled_artifact_content(
                        snapshot.revision,
                        &target_kind,
                        &target_id,
                        &artifact_kind,
                    ))
                }
            }
        }
        ("node", "hy2_port_hop") => {
            ensure_node_visible(snapshot, &target_id)?;
            let plan = output.project_node(&target_id).map_err(|blocked| {
                StoreError::InvalidData(format!("cannot project node {target_id}: {blocked:?}"))
            })?;
            let artifact = hy2_port_hop::build(&plan);
            match &artifact {
                Hy2PortHopArtifact::Config(_) => artifact_content_from_text(
                    snapshot.revision,
                    &target_kind,
                    &target_id,
                    &artifact_kind,
                    json_format::hy2_port_hop(&artifact),
                    redact,
                ),
                Hy2PortHopArtifact::Disabled { .. } => Ok(disabled_artifact_content(
                    snapshot.revision,
                    &target_kind,
                    &target_id,
                    &artifact_kind,
                )),
            }
        }
        ("node", "xray") => {
            ensure_node_visible(snapshot, &target_id)?;
            let plan = output.project_node(&target_id).map_err(|blocked| {
                StoreError::InvalidData(format!("cannot project node {target_id}: {blocked:?}"))
            })?;
            let artifact = xray::build(&plan);
            match &artifact {
                brocade_core::artifacts::xray::XrayArtifact::Config(_) => {
                    artifact_content_from_text(
                        snapshot.revision,
                        &target_kind,
                        &target_id,
                        &artifact_kind,
                        json_format::xray(&artifact),
                        redact,
                    )
                }
                brocade_core::artifacts::xray::XrayArtifact::Disabled { .. } => {
                    Ok(disabled_artifact_content(
                        snapshot.revision,
                        &target_kind,
                        &target_id,
                        &artifact_kind,
                    ))
                }
            }
        }
        ("node", "grants") => {
            ensure_node_visible(snapshot, &target_id)?;
            let plan = output.project_node(&target_id).map_err(|blocked| {
                StoreError::InvalidData(format!("cannot project node {target_id}: {blocked:?}"))
            })?;
            let xray = xray::build(&plan);
            if matches!(xray, brocade_core::artifacts::xray::XrayArtifact::Config(_)) {
                artifact_content_from_text(
                    snapshot.revision,
                    &target_kind,
                    &target_id,
                    &artifact_kind,
                    json_format::grant_sync_batch(&grants::build(&plan)),
                    false,
                )
            } else {
                Ok(disabled_artifact_content(
                    snapshot.revision,
                    &target_kind,
                    &target_id,
                    &artifact_kind,
                ))
            }
        }
        ("user", "uri") | ("user", "clash") => {
            let (tenant_id, user_id) = split_user_target(&target_id)?;
            ensure_user_visible(snapshot, tenant_id, user_id)?;
            let mut plan = output.project_user(tenant_id, user_id).map_err(|blocked| {
                StoreError::InvalidData(format!(
                    "cannot project user {tenant_id}/{user_id}: {blocked:?}"
                ))
            })?;
            if let Some(family) = family {
                plan.retain_family(family);
            }
            let subscription = subscription::build(&plan);
            let content = if artifact_kind == "uri" {
                uri::subscription(&subscription)
            } else {
                yaml::clash_subscription(&subscription)
            };
            artifact_content_from_text(
                snapshot.revision,
                &target_kind,
                &target_id,
                &artifact_kind,
                content,
                false,
            )
        }
        _ => Err(StoreError::NotFound(format!(
            "artifact {target_kind}/{target_id}/{artifact_kind}"
        ))),
    }
}

pub async fn verify_deployment(
    pool: &PgPool,
    actor: &AdminContext,
    request: VerifyDeploymentRequest,
) -> Result<DeploymentVerification> {
    let revision_id = match request.revision_id {
        Some(revision_id) => revision_id,
        None => crate::materialize::current_revision(pool).await?,
    };
    let mut plan = crate::deployment::plan_deployment(pool, actor, revision_id).await?;
    if let Some(node_id) = request.node_id.as_deref().and_then(optional_text) {
        plan.targets.retain(|target| target.node_id == node_id);
        if plan.targets.is_empty() {
            return Err(StoreError::NotFound(format!(
                "node {node_id} in revision {revision_id}"
            )));
        }
        plan.summary = brocade_deployment::plan::summarize_targets(&plan.targets);
    }

    Ok(DeploymentVerification {
        revision_id,
        converged: plan.summary.changed_targets == 0,
        summary: plan.summary,
        targets: plan.targets,
    })
}
