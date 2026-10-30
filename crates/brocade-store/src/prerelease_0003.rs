//! Explicit offline conversions of precisely known deployed prerelease histories.
use crate::{pg::MIGRATOR, DatabaseUpgradeCheck, PgStore, Result, StoreError};
use sqlx::{Postgres, Transaction};

const PREVIOUS: &str = "629a3639adc5f87a6e6677d0b471c57872adbf3d8b735c819eb7d2467a57e1b3f4030b1dc96de68099e32df7caf7c01f";
const PREVIOUS_CHANNEL: &str = "9528c5fe2b11fb063af638d6a11a30079fd70413e91cc55ee3df1bfc5f23e274a779d877254f412623bb8cb4ee776aaf";
const PREVIOUS_SESSIONS: &str = "075d809314104489326b974f7969513a0f8917d6d65f98d70056da8d982fa7d2e58daf50785931163a9832d5b9a35e5e";
const BOUNDARY: &str = "\n-- Consolidated Agent channel notifications.\n";
const READINESS_BOUNDARY: &str = "\n-- Consolidated path-based unlock readiness.\n";
const SESSIONS_BOUNDARY: &str = "\n-- Consolidated online source session history.\n";

#[derive(Clone, Copy)]
enum PreviousSchema {
    Notifications,
    Readiness,
    Sessions,
}

fn exact_previous(history: &[(i64, bool, Vec<u8>)]) -> Option<PreviousSchema> {
    let source: Vec<_> = MIGRATOR.iter().collect();
    if history.len() != 3
        || source.len() < 3
        || history.iter().any(|row| !row.1)
        || !history
            .iter()
            .enumerate()
            .all(|(index, row)| row.0 == index as i64 + 1)
        || !history[..2]
            .iter()
            .zip(&source)
            .all(|(actual, expected)| actual.2.as_slice() == expected.checksum.as_ref())
    {
        return None;
    }
    let checksum: String = history[2]
        .2
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    match checksum.as_str() {
        PREVIOUS => Some(PreviousSchema::Notifications),
        PREVIOUS_CHANNEL => Some(PreviousSchema::Readiness),
        PREVIOUS_SESSIONS => Some(PreviousSchema::Sessions),
        _ => None,
    }
}

async fn previous_schema(tx: &mut Transaction<'_, Postgres>) -> Result<Option<PreviousSchema>> {
    let history: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version,success,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut **tx)
            .await?;
    let Some(previous) = exact_previous(&history) else {
        return Ok(None);
    };
    let shape:bool=sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM pg_class WHERE oid IN ('_sqlx_migrations'::regclass,
             'control_state'::regclass,'unlock_probe_jobs'::regclass,'unlock_probe_discovery'::regclass,
             'user_online_sources'::regclass)
             AND relowner<>current_user::regrole)
         AND EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid='unlock_probe_jobs'::regclass
             AND attname='address_family' AND atttypid='text'::regtype AND NOT attnotnull AND NOT attisdropped)
         AND EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid='control_state'::regclass
             AND attname='unlock_refresh_stale' AND atttypid='boolean'::regtype AND attnotnull AND NOT attisdropped)
         AND to_regclass('public.unlock_probe_jobs_family_worker') IS NOT NULL
         AND EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid='unlock_probe_jobs'::regclass
             AND contype='c' AND convalidated AND pg_get_constraintdef(oid) LIKE '%address_family%v4%v6%')"
    ).fetch_one(&mut **tx).await?;
    if !shape {
        return Ok(None);
    }
    let sessions: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT FROM pg_attribute WHERE attrelid='user_online_sources'::regclass
             AND attname='session_id' AND NOT attisdropped)
         AND EXISTS (SELECT FROM pg_constraint WHERE conrelid='user_online_sources'::regclass
             AND contype='p' AND convalidated AND pg_get_constraintdef(oid)=
                 'PRIMARY KEY (node_id, tenant_id, user_id, ingress_id, source_ip)')
         AND to_regclass('public.user_online_sources_active_epoch') IS NULL
         AND to_regclass('public.user_online_sources_user_history') IS NULL",
    )
    .fetch_one(&mut **tx)
    .await?;
    if !sessions {
        return Ok(None);
    }
    let readiness: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT FROM pg_attribute WHERE attrelid='unlock_probe_jobs'::regclass
             AND attname='readiness_failures' AND atttypid='smallint'::regtype AND attnotnull AND NOT attisdropped)
         AND EXISTS(SELECT FROM pg_attribute WHERE attrelid='unlock_probe_jobs'::regclass
             AND attname='dns_fingerprint' AND atttypid='text'::regtype AND NOT attisdropped)
         AND (SELECT count(*) FROM pg_attribute WHERE attrelid='control_state'::regclass
             AND attname IN ('unlock_detection_mode','unlock_detection_interval_hours','unlock_detection_requested_at','unlock_detection_next_run_at')
             AND NOT attisdropped)=4",
    ).fetch_one(&mut **tx).await?;
    let before_readiness: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS(SELECT FROM pg_attribute WHERE attrelid='unlock_probe_jobs'::regclass
             AND attname IN ('readiness_failures','dns_fingerprint') AND NOT attisdropped)
         AND NOT EXISTS(SELECT FROM pg_attribute WHERE attrelid='control_state'::regclass
             AND attname LIKE 'unlock_detection_%' AND NOT attisdropped)",
    )
    .fetch_one(&mut **tx)
    .await?;
    if !match previous {
        PreviousSchema::Sessions => readiness,
        _ => before_readiness,
    } {
        return Ok(None);
    }
    let notifications:bool=match previous {
        PreviousSchema::Notifications=>sqlx::query_scalar("SELECT to_regprocedure('public.notify_agent_channel_change()') IS NULL").fetch_one(&mut **tx).await?,
        PreviousSchema::Readiness | PreviousSchema::Sessions=>sqlx::query_scalar("SELECT EXISTS(SELECT FROM pg_proc WHERE oid=to_regprocedure('public.notify_agent_channel_change()') AND proowner=current_user::regrole) AND (SELECT count(*) FROM pg_trigger WHERE tgfoid=to_regprocedure('public.notify_agent_channel_change()') AND NOT tgisinternal AND tgenabled='O')=13").fetch_one(&mut **tx).await?,
    };
    Ok(notifications.then_some(previous))
}

impl PgStore {
    /// Opt-in maintenance check. Reporting version 3 as pending preserves the existing launcher's
    /// backup -> durable migration-started -> database-migrate boundary without a new CLI shape.
    pub async fn check_prerelease_0003_upgrade(&self) -> Result<DatabaseUpgradeCheck> {
        let mut check = self.check_database_upgrade().await?;
        if check.compatible || check.installed_versions != [1, 2, 3] {
            return Ok(check);
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *tx)
            .await?;
        let allowed = previous_schema(&mut tx).await?;
        tx.commit().await?;
        if allowed.is_some() {
            check.compatible = true;
            check.pending_versions = MIGRATOR
                .iter()
                .filter(|migration| migration.version >= 3)
                .map(|migration| migration.version)
                .collect();
            check.reason = None;
        }
        Ok(check)
    }

    /// Only the offline command calls this after the launcher has made its backup and marker.
    /// Normal startup and the default maintenance commands retain strict immutable checksums.
    pub async fn migrate_prerelease_0003_schema(&self) -> Result<()> {
        if self.check_database_upgrade().await?.compatible {
            return self.migrate_database_schema().await;
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='120s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT pg_advisory_xact_lock(72625104422003)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("LOCK TABLE _sqlx_migrations,control_state,unlock_probe_jobs,unlock_probe_discovery,user_online_sources IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *tx).await?;
        let previous = previous_schema(&mut tx).await?.ok_or_else(|| {
            StoreError::InvalidData("未匹配已批准的预发布 0003 历史和结构，未转换".into())
        })?;
        let migration = MIGRATOR
            .iter()
            .find(|migration| migration.version == 3)
            .ok_or_else(|| StoreError::InvalidData("当前发行不包含目标 0003".into()))?;
        let boundary = match previous {
            PreviousSchema::Notifications => BOUNDARY,
            PreviousSchema::Readiness => READINESS_BOUNDARY,
            PreviousSchema::Sessions => SESSIONS_BOUNDARY,
        };
        let conversion_sql = migration
            .sql
            .split_once(boundary)
            .map(|(_, suffix)| suffix)
            .ok_or_else(|| StoreError::InvalidData("目标 0003 缺少指定兼容段".into()))?;
        sqlx::raw_sql(conversion_sql).execute(&mut *tx).await?;
        let triggers: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_trigger
            WHERE tgfoid='public.notify_agent_channel_change()'::regprocedure AND NOT tgisinternal AND tgenabled='O'")
            .fetch_one(&mut *tx).await?;
        if triggers != 13 {
            return Err(StoreError::InvalidData("0003 通知触发器未完整创建".into()));
        }
        let counter:bool=sqlx::query_scalar("SELECT EXISTS(SELECT FROM pg_attribute WHERE attrelid='unlock_probe_jobs'::regclass AND attname='readiness_failures' AND atttypid='smallint'::regtype AND attnotnull AND NOT attisdropped)").fetch_one(&mut *tx).await?;
        if !counter {
            return Err(StoreError::InvalidData("0003 就绪重试字段未创建".into()));
        }
        let schedule: bool = sqlx::query_scalar("SELECT
            EXISTS(SELECT FROM pg_attribute WHERE attrelid='unlock_probe_jobs'::regclass AND attname='dns_fingerprint' AND atttypid='text'::regtype AND NOT attisdropped)
            AND (SELECT count(*) FROM pg_attribute WHERE attrelid='control_state'::regclass
                AND attname IN ('unlock_detection_mode','unlock_detection_interval_hours','unlock_detection_requested_at','unlock_detection_next_run_at') AND NOT attisdropped)=4")
            .fetch_one(&mut *tx).await?;
        if !schedule {
            return Err(StoreError::InvalidData(
                "0003 解锁检测调度字段未完整创建".into(),
            ));
        }
        let sessions: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM pg_attribute WHERE attrelid='user_online_sources'::regclass
                 AND attname='session_id' AND atttypid='bigint'::regtype AND attidentity='a' AND attnotnull AND NOT attisdropped)
             AND EXISTS (SELECT FROM pg_constraint WHERE conrelid='user_online_sources'::regclass
                 AND contype='p' AND pg_get_constraintdef(oid)='PRIMARY KEY (session_id)')
             AND EXISTS (SELECT FROM pg_index WHERE indexrelid=to_regclass('public.user_online_sources_active_epoch')
                 AND indisunique AND indisvalid AND pg_get_expr(indpred,indrelid)='active')
             AND EXISTS (SELECT FROM pg_index WHERE indexrelid=to_regclass('public.user_online_sources_user_history') AND indisvalid)",
        ).fetch_one(&mut *tx).await?;
        if !sessions {
            return Err(StoreError::InvalidData(
                "0003 在线来源会话结构未完整创建".into(),
            ));
        }
        sqlx::query("UPDATE _sqlx_migrations SET checksum=$1 WHERE version=3")
            .bind(migration.checksum.as_ref())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.migrate_database_schema().await
    }
}
