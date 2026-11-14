//! Offline conversion of the exact deployed Clash-only 0007, under the installer's backup boundary.
use crate::{pg::MIGRATOR, DatabaseUpgradeCheck, PgStore, Result, StoreError};
use sha2::{Digest, Sha384};
use sqlx::{Postgres, Transaction};

const OLD_CHECKSUM: &str = "d166d16cce814340fb2c9c30df6aed5d6bb2577b148043b76d2548b9b9fbaf180607a7f5ad1751e6c12ca6e9c3af3f6a";
const TARGET_CHECKSUM: &str = "f3d2016a0ce40fcb920794d6e587dd291347c5c89c4435915c17be001a438875fb29b0608fe86a396ce534e9aff1d0b3";
const BOUNDARY: &str = "\n-- Optional host-egress maintenance;";
// PostgreSQL 16/17 and 18 have distinct catalog shapes (18 records NOT NULL constraints).
const OLD_SHAPES: &[&str] = &["45931525632fb99f6caeb689b57c2d55c3fe41e5274a1f30b2b8c3e5eb34ad09d166da83e56994e7b30f84701d2ed1d1", "f587b69510588df3d7c00e6856a59fde8074bff993c6c77c90955eb9f8c1a710340ebd0261472e8fe89a5638e31ab44f"];
const TARGET_SHAPES: &[&str] = &["7012479cad483ca5236cc036e806e93650187dddba4c0c0437fbeb1ba55a6529d4419529f9440a291a8e42701605102e", "4c8b11648ce723d693946b00362b1e0e0d771786ee025ffccaa4d9721a64d0d80af546fd82d1740897279a13bce334ce"];
const CATALOG: &str = include_str!("catalog.sql");

fn delta() -> Result<&'static str> {
    let target = MIGRATOR
        .iter()
        .find(|m| m.version == 7)
        .ok_or_else(|| StoreError::InvalidData("缺少养护迁移".into()))?;
    let (prefix, suffix) = target
        .sql
        .split_once(BOUNDARY)
        .ok_or_else(|| StoreError::InvalidData("养护迁移边界缺失".into()))?;
    if format!("{:x}", Sha384::digest(prefix.as_bytes())) != OLD_CHECKSUM
        || format!("{:x}", Sha384::digest(target.sql.as_bytes())) != TARGET_CHECKSUM
    {
        return Err(StoreError::InvalidData(
            "养护迁移不是已核验的旧前缀和完整目标".into(),
        ));
    }
    // Skip the rest of the boundary comment; execute only the authoritative DDL suffix.
    let (_, ddl) = suffix
        .split_once('\n')
        .ok_or_else(|| StoreError::InvalidData("养护迁移正文缺失".into()))?;
    Ok(ddl)
}

async fn fingerprint(tx: &mut Transaction<'_, Postgres>) -> Result<String> {
    let catalog: String = sqlx::query_scalar(CATALOG).fetch_one(&mut **tx).await?;
    Ok(format!("{:x}", Sha384::digest(catalog.as_bytes())))
}

async fn exact_previous(tx: &mut Transaction<'_, Postgres>) -> Result<bool> {
    let rows: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version,success,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut **tx)
            .await?;
    let source: Vec<_> = MIGRATOR.iter().collect();
    if rows.len() != 7
        || source.len() != 7
        || rows.iter().enumerate().any(|(i, r)| {
            r.0 != i as i64 + 1
                || !r.1
                || if i == 6 {
                    brocade_core::hash::hex_lower(&r.2) != OLD_CHECKSUM
                } else {
                    r.2.as_slice() != source[i].checksum.as_ref()
                }
        })
    {
        return Ok(false);
    }
    delta()?;
    let owner: bool = sqlx::query_scalar("SELECT relowner=current_user::regrole FROM pg_class WHERE oid='_sqlx_migrations'::regclass")
        .fetch_one(&mut **tx).await?;
    Ok(owner && OLD_SHAPES.contains(&fingerprint(tx).await?.as_str()))
}

impl PgStore {
    /// Read-only; pending 7 makes the installer back up and mark migration-started before DDL.
    pub async fn check_ip_maintenance_upgrade(&self) -> Result<DatabaseUpgradeCheck> {
        let mut check = self.check_database_upgrade().await?;
        if check.compatible || check.installed_versions != [1, 2, 3, 4, 5, 6, 7] {
            return Ok(check);
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL search_path=public,pg_catalog")
            .execute(&mut *tx)
            .await?;
        if exact_previous(&mut tx).await? {
            check.compatible = true;
            check.pending_versions = vec![7];
            check.reason = None;
        }
        tx.commit().await?;
        Ok(check)
    }

    /// Add missing tables and align only the exact old receipt in one bounded transaction.
    /// Ordinary startup never calls this method. Unknown or partial structures are refused.
    pub async fn migrate_ip_maintenance_schema(&self) -> Result<()> {
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
        sqlx::query("SET LOCAL search_path=public,pg_catalog")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT pg_advisory_xact_lock(72625104422007)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("LOCK TABLE _sqlx_migrations,user_clash_rules IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await?;
        if !exact_previous(&mut tx).await? {
            return Err(StoreError::InvalidData(
                "未匹配已核验的 Clash-only 0007 历史和结构，未转换".into(),
            ));
        }
        sqlx::raw_sql(delta()?).execute(&mut *tx).await?;
        if !TARGET_SHAPES.contains(&fingerprint(&mut tx).await?.as_str()) {
            return Err(StoreError::InvalidData(
                "养护结构验证失败，未更新迁移回执".into(),
            ));
        }
        let checksum = MIGRATOR
            .iter()
            .find(|m| m.version == 7)
            .ok_or_else(|| StoreError::InvalidData("缺少养护迁移".into()))?
            .checksum
            .as_ref();
        sqlx::query("UPDATE _sqlx_migrations SET checksum=$1 WHERE version=7")
            .bind(checksum)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.migrate_database_schema().await
    }
}
