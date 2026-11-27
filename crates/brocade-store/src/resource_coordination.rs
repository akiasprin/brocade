//! One transaction lock for host resources shared by proxy and CVM writers.
use crate::{Result, StoreError};
use sqlx::{Postgres, Transaction};

pub(crate) async fn lock(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    // Acquire before instance, user and participating-node row locks in both products.
    sqlx::query("SELECT pg_advisory_xact_lock(1112687438,65)")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// This table lock conflicts with lease mutations, but not reads by other resource writers.
/// Take it before the first snapshot in a repeatable-read claim transaction. It also allows
/// binary release claims to synchronize with stack approval without nesting their own mutex
/// under the ordinary resource lock.
pub(crate) async fn synchronize_stack_claim(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    sqlx::query("LOCK TABLE cvm_stack_releases IN ROW SHARE MODE")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Called under the resource lock or claim table lock. Elapsed, unclaimed approval cannot block ordinary work,
/// while an applying or blocked attempt keeps the fence until its state is proven.
pub(crate) async fn stack_busy<N: AsRef<str>>(
    tx: &mut Transaction<'_, Postgres>,
    nodes: &[N],
) -> Result<bool> {
    if nodes.is_empty() {
        return Ok(false);
    }
    let ids: Vec<&str> = nodes.iter().map(AsRef::as_ref).collect();
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM cvm_stack_releases WHERE node_id=ANY($1)
          AND status IN ('approved','applying','blocked')
          AND (status <> 'approved' OR expires_at > floor(extract(epoch FROM clock_timestamp()))))",
    )
    .bind(ids)
    .fetch_one(&mut **tx)
    .await?)
}

pub(crate) async fn ensure_stack_idle<N: AsRef<str>>(
    tx: &mut Transaction<'_, Postgres>,
    nodes: &[N],
) -> Result<()> {
    if stack_busy(tx, nodes).await? {
        return Err(StoreError::Conflict(
            "the host is reserved for Cube stack maintenance".into(),
        ));
    }
    Ok(())
}
