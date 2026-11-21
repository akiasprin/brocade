//! Offline conversion of exact historical 0007 variants, under the installer's backup boundary.
use crate::{pg::MIGRATOR, DatabaseUpgradeCheck, PgStore, Result, StoreError};
use sha2::{Digest, Sha384};
use sqlx::{Postgres, Transaction};

const CLASH_ONLY_CHECKSUM: &str = "d166d16cce814340fb2c9c30df6aed5d6bb2577b148043b76d2548b9b9fbaf180607a7f5ad1751e6c12ca6e9c3af3f6a";
const DEPLOYED_NOTIFICATION_CHECKSUM: &str = "9e6eeccade5a7799d6392df0afe9982aa022965d280c46e49b2d83a1f4fbf734d590ae26e5e5408295543052446db9b8";
const TARGET_CHECKSUM: &str = "f3d2016a0ce40fcb920794d6e587dd291347c5c89c4435915c17be001a438875fb29b0608fe86a396ce534e9aff1d0b3";
const BOUNDARY: &str = "\n-- Optional host-egress maintenance;";
// PostgreSQL 16/17 and 18 have distinct catalog shapes (18 records NOT NULL constraints).
const CLASH_ONLY_SHAPES: &[&str] = &["45931525632fb99f6caeb689b57c2d55c3fe41e5274a1f30b2b8c3e5eb34ad09d166da83e56994e7b30f84701d2ed1d1", "f587b69510588df3d7c00e6856a59fde8074bff993c6c77c90955eb9f8c1a710340ebd0261472e8fe89a5638e31ab44f"];
const TARGET_SHAPES: &[&str] = &["7012479cad483ca5236cc036e806e93650187dddba4c0c0437fbeb1ba55a6529d4419529f9440a291a8e42701605102e", "4c8b11648ce723d693946b00362b1e0e0d771786ee025ffccaa4d9721a64d0d80af546fd82d1740897279a13bce334ce"];
const CATALOG: &str = include_str!("catalog.sql");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Previous0007 {
    ClashOnly,
    DeployedNotification,
}

fn delta() -> Result<&'static str> {
    let target = MIGRATOR
        .iter()
        .find(|m| m.version == 7)
        .ok_or_else(|| StoreError::InvalidData("缺少养护迁移".into()))?;
    let (prefix, suffix) = target
        .sql
        .split_once(BOUNDARY)
        .ok_or_else(|| StoreError::InvalidData("养护迁移边界缺失".into()))?;
    if format!("{:x}", Sha384::digest(prefix.as_bytes())) != CLASH_ONLY_CHECKSUM
        || format!("{:x}", Sha384::digest(target.sql.as_bytes())) != TARGET_CHECKSUM
    {
        return Err(StoreError::InvalidData(
            "养护迁移不是已核验的旧前缀和正式 v0.3.8 目标".into(),
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

async fn exact_receipt(tx: &mut Transaction<'_, Postgres>, checksum: &str) -> Result<bool> {
    let rows: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version,success,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut **tx)
            .await?;
    let source: Vec<_> = MIGRATOR.iter().collect();
    Ok(rows.len() == 7
        && source.len() >= 8
        && rows.iter().enumerate().all(|(i, row)| {
            row.0 == i as i64 + 1
                && row.1
                && if i == 6 {
                    brocade_core::hash::hex_lower(&row.2) == checksum
                } else {
                    row.2.as_slice() == source[i].checksum.as_ref()
                }
        }))
}

async fn notification_schema_is_exact(tx: &mut Transaction<'_, Postgres>) -> Result<bool> {
    if !sqlx::query_scalar::<_, bool>(
        "SELECT to_regclass('public.node_notification_preferences') IS NOT NULL",
    )
    .fetch_one(&mut **tx)
    .await?
    {
        return Ok(false);
    }
    sqlx::query_scalar(
        r#"SELECT c.relowner=current_user::regrole
             AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND c.relpersistence='p'
             AND (SELECT jsonb_agg(jsonb_build_array(a.attname,format_type(a.atttypid,a.atttypmod),a.attnotnull,pg_get_expr(d.adbin,d.adrelid)) ORDER BY a.attnum)
                  FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum
                  WHERE a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped) =
                 '[["node_id","text",true,null],["public_ip_changes","boolean",true,"true"],["changed_by","text",true,null],["changed_at","timestamp with time zone",true,"now()"]]'::jsonb
             AND (SELECT jsonb_agg(jsonb_build_array(k.conname,pg_get_constraintdef(k.oid),k.convalidated) ORDER BY k.conname)
                  FROM pg_constraint k WHERE k.conrelid=c.oid AND k.contype IN ('c','f','p')) =
                 '[
                   ["node_notification_preferences_changed_by_check","CHECK (((length(btrim(changed_by)) >= 1) AND (length(btrim(changed_by)) <= 256)))",true],
                   ["node_notification_preferences_node_id_fkey","FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE",true],
                   ["node_notification_preferences_pkey","PRIMARY KEY (node_id)",true]
                 ]'::jsonb
             AND (SELECT jsonb_agg(jsonb_build_array(pg_get_indexdef(i.indexrelid),i.indisvalid,i.indisready) ORDER BY pg_get_indexdef(i.indexrelid))
                  FROM pg_index i WHERE i.indrelid=c.oid) =
                 '[["CREATE UNIQUE INDEX node_notification_preferences_pkey ON public.node_notification_preferences USING btree (node_id)",true,true]]'::jsonb
             AND NOT EXISTS (SELECT 1 FROM pg_trigger t WHERE t.tgrelid=c.oid AND NOT t.tgisinternal)
          FROM pg_class c WHERE c.oid='public.node_notification_preferences'::regclass"#,
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(Into::into)
}

async fn previous_0007(tx: &mut Transaction<'_, Postgres>) -> Result<Option<Previous0007>> {
    delta()?;
    let owner: bool = sqlx::query_scalar("SELECT relowner=current_user::regrole FROM pg_class WHERE oid='_sqlx_migrations'::regclass")
        .fetch_one(&mut **tx).await?;
    if !owner {
        return Ok(None);
    }
    let shape = fingerprint(tx).await?;
    if exact_receipt(tx, CLASH_ONLY_CHECKSUM).await? && CLASH_ONLY_SHAPES.contains(&shape.as_str())
    {
        return Ok(Some(Previous0007::ClashOnly));
    }
    if exact_receipt(tx, DEPLOYED_NOTIFICATION_CHECKSUM).await?
        && TARGET_SHAPES.contains(&shape.as_str())
        && notification_schema_is_exact(tx).await?
    {
        return Ok(Some(Previous0007::DeployedNotification));
    }
    Ok(None)
}

impl PgStore {
    pub async fn check_prerelease_0007_upgrade(&self) -> Result<DatabaseUpgradeCheck> {
        self.check_ip_maintenance_upgrade().await
    }

    pub async fn migrate_prerelease_0007_schema(&self) -> Result<()> {
        self.migrate_ip_maintenance_schema().await
    }

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
        if previous_0007(&mut tx).await?.is_some() {
            check.compatible = true;
            check.pending_versions.insert(0, 7);
            check.reason = None;
        }
        tx.commit().await?;
        Ok(check)
    }

    /// Convert only exact known history, preserving business rows and all other receipts.
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
        let previous = previous_0007(&mut tx).await?.ok_or_else(|| {
            StoreError::InvalidData("未匹配已核验的 0007 历史和结构，未转换".into())
        })?;
        match previous {
            Previous0007::ClashOnly => {
                sqlx::raw_sql(delta()?).execute(&mut *tx).await?;
            }
            Previous0007::DeployedNotification => {
                sqlx::query("LOCK TABLE node_notification_preferences IN ACCESS EXCLUSIVE MODE")
                    .execute(&mut *tx)
                    .await?;
            }
        }
        if !TARGET_SHAPES.contains(&fingerprint(&mut tx).await?.as_str()) {
            return Err(StoreError::InvalidData(
                "养护结构验证失败，未更新迁移回执".into(),
            ));
        }
        if previous == Previous0007::DeployedNotification
            && !notification_schema_is_exact(&mut tx).await?
        {
            return Err(StoreError::InvalidData(
                "通知偏好结构验证失败，未更新迁移回执".into(),
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
