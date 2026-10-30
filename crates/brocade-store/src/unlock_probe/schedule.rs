use super::*;
use crate::UnlockDetectionMode;

#[derive(Clone, Copy, Serialize)]
pub struct UnlockDetectionPolicy {
    pub mode: UnlockDetectionMode,
    pub interval_hours: u32,
    pub requested_at: i64,
    pub next_run_at: Option<i64>,
}

pub(super) async fn policy(pool: &PgPool) -> Result<UnlockDetectionPolicy> {
    let row = sqlx::query(
        "SELECT unlock_detection_mode,unlock_detection_interval_hours,
        extract(epoch FROM unlock_detection_requested_at)::bigint AS requested,
        extract(epoch FROM unlock_detection_next_run_at)::bigint AS next
        FROM control_state WHERE id=TRUE",
    )
    .fetch_one(pool)
    .await?;
    Ok(UnlockDetectionPolicy {
        mode: UnlockDetectionMode::parse(row.try_get("unlock_detection_mode")?)?,
        interval_hours: u32::try_from(row.try_get::<i32, _>("unlock_detection_interval_hours")?)
            .map_err(|_| StoreError::InvalidData("invalid unlock detection interval".into()))?,
        requested_at: row.try_get("requested")?,
        next_run_at: row.try_get("next")?,
    })
}

/// Durable discovery invalidation plus a commit-scoped hint; probing remains on Agents.
pub(crate) async fn wake_discovery(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    sqlx::query(
        "UPDATE unlock_probe_discovery SET next_discovery_at=LEAST(next_discovery_at,now())",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "SELECT pg_notify('brocade_agent_change','{\"domain\":\"unlock_probe\",\"node\":null}')",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(super) async fn advance_cycle(pool: &PgPool) -> Result<()> {
    let mut tx = pool.begin().await?;
    let changed=sqlx::query("UPDATE control_state SET unlock_detection_requested_at=now(),
        unlock_detection_next_run_at=now()+make_interval(hours=>unlock_detection_interval_hours)
        WHERE id=TRUE AND unlock_detection_mode='scheduled' AND unlock_detection_next_run_at<=now()")
        .execute(&mut *tx).await?.rows_affected()!=0;
    if changed {
        wake_discovery(&mut tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

impl PgStore {
    /// One bounded full sweep. Each scope has at most one queued or running attempt.
    pub async fn refresh_all_unlock_probes(
        &self,
        actor: &AdminContext,
    ) -> Result<UnlockDetectionPolicy> {
        if !actor.is_system_admin() {
            return Err(StoreError::Forbidden(
                "only system-admin can refresh all unlock probes".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT id FROM control_state WHERE id=TRUE FOR UPDATE")
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query("UPDATE control_state SET unlock_detection_requested_at=now() WHERE id=TRUE")
            .execute(&mut *tx)
            .await?;
        wake_discovery(&mut tx).await?;
        tx.commit().await?;
        policy(self.pool()).await
    }
}
