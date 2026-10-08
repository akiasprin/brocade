//! Xray-specific compatibility entry points. Accounting is owned by binary_release;
//! Xray execution and report validation remain separate from Agent self-update.
use crate::{binary_release as ledger, AdminContext, Result};
use ledger::BinaryComponent::Xray;
pub use ledger::{
    BinaryRelease as XrayRelease, BinaryReleaseArtifact as XrayReleaseArtifact,
    BinaryReleaseAssignment as XrayReleaseAssignment, BinaryReleaseEvent as XrayReleaseEvent,
    BinaryReleaseList as XrayReleaseList, BinaryReleaseStatus as XrayReleaseStatus,
    BinaryReleaseSummary as XrayReleaseSummary, BinaryReleaseTarget as XrayReleaseTarget,
    BinaryReleaseTargetStatus as XrayReleaseTargetStatus,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};

#[derive(Debug, Clone, Copy)]
pub struct XrayBuildInfo<'a> {
    pub release_id: &'a str,
    pub version: &'a str,
    pub artifacts: &'a [XrayReleaseArtifact],
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateXrayReleaseRequest {
    pub idempotency_key: String,
    #[serde(alias = "build_id")]
    pub release_id: String,
    pub nodes: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}
pub async fn list_xray_releases(pool: &PgPool, limit: u32) -> Result<XrayReleaseList> {
    ledger::list_binary_releases(pool, Xray, limit).await
}
pub async fn get_xray_release(pool: &PgPool, id: i64) -> Result<XrayRelease> {
    ledger::get_binary_release(pool, Xray, id).await
}
pub async fn get_xray_release_overview(pool: &PgPool, id: i64) -> Result<XrayRelease> {
    ledger::get_binary_release_overview(pool, Xray, id).await
}
pub async fn list_xray_release_summaries(
    pool: &PgPool,
    limit: u32,
    before: Option<i64>,
) -> Result<Vec<XrayReleaseSummary>> {
    ledger::list_binary_release_summaries(pool, Xray, limit, before).await
}
pub async fn create_xray_release(
    pool: &PgPool,
    actor: &AdminContext,
    r: CreateXrayReleaseRequest,
    b: XrayBuildInfo<'_>,
) -> Result<XrayRelease> {
    ledger::create_binary_release(
        pool,
        actor,
        ledger::CreateBinaryReleaseRequest {
            idempotency_key: r.idempotency_key,
            build_id: r.release_id,
            nodes: r.nodes,
            note: r.note,
        },
        ledger::BinaryBuildInfo {
            component: Xray,
            build_id: b.release_id,
            version: b.version,
            artifacts: b.artifacts,
        },
    )
    .await
}
pub async fn cancel_xray_release(
    pool: &PgPool,
    actor: &AdminContext,
    id: i64,
) -> Result<XrayRelease> {
    ledger::cancel_binary_release(pool, Xray, actor, id).await
}
pub async fn retry_xray_release_target(
    pool: &PgPool,
    actor: &AdminContext,
    id: i64,
    node: &str,
    build: &str,
) -> Result<XrayRelease> {
    ledger::retry_binary_release_target(pool, Xray, actor, id, node, build).await
}
pub async fn claim_xray_release(
    pool: &PgPool,
    node: &str,
    arch: &str,
    build: &str,
) -> Result<Option<XrayReleaseAssignment>> {
    ledger::claim_binary_release(pool, Xray, node, arch, build, true).await
}
pub async fn report_xray_release(
    pool: &PgPool,
    node: &str,
    report: &brocade_deployment::protocol::XrayReleaseReport,
) -> Result<bool> {
    ledger::report_xray_release(pool, node, report).await
}
pub(crate) async fn cancel_for_node_lifecycle_tx(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    actor: &str,
) -> Result<Option<i64>> {
    ledger::cancel_for_node_lifecycle_tx(tx, node, actor).await
}
