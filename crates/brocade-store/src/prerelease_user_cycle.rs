//! Explicit offline conversion of the exact deployed reset schemas to unreleased 0003.
use crate::{pg::MIGRATOR, DatabaseUpgradeCheck, PgStore, Result, StoreError};
use sha2::{Digest, Sha384};
use sqlx::{Postgres, Transaction};

const PUBLISHED_SECOND: &str = "54300599c9b4cce71f9e574b3f43141fee2fbd50279f829feda08f537bcde441a725ff1af4e0fbbf96fb9d80966552f1";
const MISPLACED_SECOND: &str = "a28f8fca174df0c854d77581b7cf3f73e85e9df99dbf65e54eed7d8536420a3dc43389948cfdfbfd2d5f003fb32ec7db";
const PRE_RESET_THIRD: &str = "0c8f0c30cb8cfc07fead32837a7090b13f4ef5bd8e812c9a6c4818db370bfcc8e937ea7562209955aa96c94eb3a12347";
const BOUNDARY: &str = "\n-- BEGIN USER CYCLE RESET\n";
// Exact catalog shapes of the same reviewed DDL: PostgreSQL 18 additionally records NOT NULL
// constraints in pg_constraint. Keep their validation metadata in the check. Both shapes exclude
// rows, sequence positions and database-specific OIDs, and are verified against fresh migrations.
const RESET_SCHEMAS: &[&str] = &[
    "b07061755171f51607b3cf704572ad4c02dc25387e186a39e5784fcd66c478494f43b5a7c6744b679c6400c2522b1521",
    "454b9ca806b706c2cad0fce6953b81f8670ef5f1b8f90cb875a18086e18f02c9163a53414045d76b53c748a2bece99ed",
];
const SCHEMA_QUERY: &str = r#"
SELECT jsonb_build_object(
 'relations',(SELECT jsonb_agg(jsonb_build_object(
   'name',c.relname,'kind',c.relkind,'owner',c.relowner=current_user::regrole,
   'rls',c.relrowsecurity,'force_rls',c.relforcerowsecurity,'persistence',c.relpersistence,
   'columns',(SELECT jsonb_agg(jsonb_build_object('name',a.attname,
      'type',format_type(a.atttypid,a.atttypmod),
      'collation',CASE WHEN a.attcollation=0 THEN NULL ELSE a.attcollation::regcollation::text END,'not_null',a.attnotnull,'identity',a.attidentity,
      'generated',a.attgenerated,'default',pg_get_expr(d.adbin,d.adrelid)) ORDER BY a.attnum)
      FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum
      WHERE a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped),
   'constraints',(SELECT jsonb_agg(jsonb_build_object('name',k.conname,
      'definition',pg_get_constraintdef(k.oid),'validated',k.convalidated) ORDER BY k.conname)
      FROM pg_constraint k WHERE k.conrelid=c.oid),
   'indexes',(SELECT jsonb_agg(jsonb_build_object('definition',pg_get_indexdef(i.indexrelid),
      'valid',i.indisvalid,'ready',i.indisready) ORDER BY pg_get_indexdef(i.indexrelid))
      FROM pg_index i WHERE i.indrelid=c.oid),
   'sequence',(SELECT jsonb_build_object('type',s.seqtypid::regtype::text,'start',s.seqstart,
      'increment',s.seqincrement,'min',s.seqmin,'max',s.seqmax,'cache',s.seqcache,'cycle',s.seqcycle)
      FROM pg_sequence s WHERE s.seqrelid=c.oid)) ORDER BY c.relname)
   FROM pg_class c WHERE c.relnamespace='public'::regnamespace AND c.relname LIKE 'user_cycle_%'),
 'functions',(SELECT jsonb_agg(jsonb_build_object('name',p.proname,
   'arguments',pg_get_function_identity_arguments(p.oid),'result',pg_get_function_result(p.oid),
   'body',btrim(p.prosrc),'language',l.lanname,'volatility',p.provolatile,
   'security_definer',p.prosecdef,'leakproof',p.proleakproof,'parallel',p.proparallel,
   'strict',p.proisstrict,'config',p.proconfig,
   'owner',p.proowner=current_user::regrole) ORDER BY p.proname,pg_get_function_identity_arguments(p.oid))
   FROM pg_proc p JOIN pg_language l ON l.oid=p.prolang
   WHERE p.pronamespace='public'::regnamespace
     AND (p.proname LIKE 'user_cycle_%' OR p.proname='user_traffic_usage')),
 'triggers',(SELECT jsonb_agg(jsonb_build_object('definition',pg_get_triggerdef(t.oid),
   'enabled',t.tgenabled) ORDER BY pg_get_triggerdef(t.oid)) FROM pg_trigger t
   JOIN pg_proc p ON p.oid=t.tgfoid WHERE NOT t.tgisinternal
     AND (t.tgname LIKE 'user_cycle_%' OR
       (p.pronamespace='public'::regnamespace AND p.proname LIKE 'user_cycle_%')))
)::text
"#;

#[derive(Clone, Copy)]
enum PreviousSchema {
    MissingReset,
    MisplacedReset,
}

fn conversion() -> Result<(&'static str, String)> {
    let second = MIGRATOR
        .iter()
        .find(|m| m.version == 2)
        .ok_or_else(|| StoreError::InvalidData("当前发行缺少 0002".into()))?;
    let third = MIGRATOR
        .iter()
        .find(|m| m.version == 3)
        .ok_or_else(|| StoreError::InvalidData("当前发行缺少 0003".into()))?;
    let (prefix, suffix) = third
        .sql
        .split_once(BOUNDARY)
        .ok_or_else(|| StoreError::InvalidData("0003 缺少本期归零边界".into()))?;
    if format!("{:x}", Sha384::digest(second.sql.as_bytes())) != PUBLISHED_SECOND
        || format!("{:x}", Sha384::digest(prefix.as_bytes())) != PRE_RESET_THIRD
        || format!(
            "{:x}",
            Sha384::digest(format!("{}{BOUNDARY}{suffix}", second.sql))
        ) != MISPLACED_SECOND
    {
        return Err(StoreError::InvalidData(
            "迁移来源不是已核验的发布版和归零结构".into(),
        ));
    }
    let old_function = second
        .sql
        .split_once("CREATE FUNCTION user_traffic_usage(")
        .and_then(|(_, rest)| rest.split_once("AS $$"))
        .and_then(|(_, rest)| rest.split_once("$$;"))
        .map(|(body, _)| body.trim().to_owned())
        .ok_or_else(|| StoreError::InvalidData("0002 缺少旧计量函数".into()))?;
    Ok((suffix, old_function))
}

async fn schema_fingerprint(tx: &mut Transaction<'_, Postgres>) -> Result<String> {
    let shape: String = sqlx::query_scalar(SCHEMA_QUERY)
        .fetch_one(&mut **tx)
        .await?;
    Ok(format!("{:x}", Sha384::digest(shape.as_bytes())))
}

async fn previous_schema(tx: &mut Transaction<'_, Postgres>) -> Result<Option<PreviousSchema>> {
    let history: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version,success,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut **tx)
            .await?;
    let source: Vec<_> = MIGRATOR.iter().collect();
    if history.len() != 3
        || source.len() < 3
        || history
            .iter()
            .enumerate()
            .any(|(i, r)| r.0 != i as i64 + 1 || !r.1)
        || history[0].2.as_slice() != source[0].checksum.as_ref()
        || brocade_core::hash::hex_lower(&history[2].2) != PRE_RESET_THIRD
    {
        return Ok(None);
    }
    let owner: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT FROM pg_class WHERE oid IN
        ('users'::regclass,'_sqlx_migrations'::regclass,'usage_rollups'::regclass,
         'forwarding_usage_rollups'::regclass,'usage_samples'::regclass,'forwarding_usage_samples'::regclass)
        AND relowner<>current_user::regrole)").fetch_one(&mut **tx).await?;
    if !owner {
        return Ok(None);
    }
    let (_, old_function) = conversion()?;
    match brocade_core::hash::hex_lower(&history[1].2).as_str() {
        PUBLISHED_SECOND => {
            let absent: bool = sqlx::query_scalar("SELECT
                NOT EXISTS(SELECT FROM pg_class WHERE relnamespace='public'::regnamespace AND relname LIKE 'user_cycle_%')
                AND NOT EXISTS(SELECT FROM pg_proc WHERE pronamespace='public'::regnamespace AND proname LIKE 'user_cycle_%')
                AND EXISTS(SELECT FROM pg_proc WHERE oid=to_regprocedure('public.user_traffic_usage(text,text,text,text)')
                    AND proowner=current_user::regrole AND NOT prosecdef
                    AND btrim(prosrc,chr(9)||chr(10)||chr(13)||' ')=btrim($1,chr(9)||chr(10)||chr(13)||' '))")
                .bind(old_function).fetch_one(&mut **tx).await?;
            Ok(absent.then_some(PreviousSchema::MissingReset))
        }
        MISPLACED_SECOND => Ok(RESET_SCHEMAS
            .contains(&schema_fingerprint(tx).await?.as_str())
            .then_some(PreviousSchema::MisplacedReset)),
        _ => Ok(None),
    }
}

impl PgStore {
    /// Maintenance-only check; pending version 3 preserves the launcher's backup/marker boundary.
    pub async fn check_user_cycle_reset_upgrade(&self) -> Result<DatabaseUpgradeCheck> {
        let mut check = self.check_database_upgrade().await?;
        if check.compatible || check.installed_versions != [1, 2, 3] {
            return Ok(check);
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL search_path=public")
            .execute(&mut *tx)
            .await?;
        let allowed = previous_schema(&mut tx).await?;
        tx.commit().await?;
        if allowed.is_some() {
            check.compatible = true;
            check.pending_versions = MIGRATOR
                .iter()
                .filter(|m| m.version >= 3)
                .map(|m| m.version)
                .collect();
            check.reason = None;
        }
        Ok(check)
    }

    /// Installs missing reset DDL or rebinds the exact deployed reset schema to 0003. Existing
    /// events, baselines, counter heads and sequence positions survive; unknown shapes are refused.
    pub async fn migrate_user_cycle_reset_schema(&self) -> Result<()> {
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
        sqlx::query("SET LOCAL search_path=public")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT pg_advisory_xact_lock(72625104422003)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("LOCK TABLE _sqlx_migrations,control_state,users,usage_samples,forwarding_usage_samples IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *tx).await?;
        let previous = previous_schema(&mut tx).await?.ok_or_else(|| {
            StoreError::InvalidData("未匹配已核验的归零历史和完整结构，未转换".into())
        })?;
        let (suffix, _) = conversion()?;
        match previous {
            PreviousSchema::MissingReset => {
                sqlx::raw_sql(suffix).execute(&mut *tx).await?;
            }
            PreviousSchema::MisplacedReset => {
                sqlx::query("LOCK TABLE user_cycle_resets,user_cycle_reset_state,user_cycle_access_baselines,user_cycle_forwarding_baselines IN ACCESS EXCLUSIVE MODE")
                    .execute(&mut *tx).await?;
                if !RESET_SCHEMAS.contains(&schema_fingerprint(&mut tx).await?.as_str()) {
                    return Err(StoreError::InvalidData("归零结构在转换前发生变化".into()));
                }
                // Replay the reviewed calculation/trigger function DDL without recreating tables
                // or triggers. DDL and both receipt changes roll back together on any failure.
                for statement in suffix.split("\nCREATE ").skip(1) {
                    let statement = statement.strip_prefix("OR REPLACE ").unwrap_or(statement);
                    if !statement.starts_with("FUNCTION ") {
                        continue;
                    }
                    let (definition, _) = statement.split_once("$$;").ok_or_else(|| {
                        StoreError::InvalidData("归零函数定义缺少结束边界".into())
                    })?;
                    sqlx::raw_sql(&format!("CREATE OR REPLACE {definition}$$;"))
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
        if !RESET_SCHEMAS.contains(&schema_fingerprint(&mut tx).await?.as_str()) {
            return Err(StoreError::InvalidData(
                "归零结构验证失败，未更新迁移回执".into(),
            ));
        }
        for migration in MIGRATOR.iter().filter(|m| matches!(m.version, 2 | 3)) {
            sqlx::query("UPDATE _sqlx_migrations SET checksum=$1 WHERE version=$2")
                .bind(migration.checksum.as_ref())
                .bind(migration.version)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        self.migrate_database_schema().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conversion_preserves_the_published_second_and_exact_deployed_reset() {
        let (suffix, old_function) = conversion().unwrap();
        assert!(suffix.contains("CREATE TABLE user_cycle_resets"));
        assert!(old_function.contains("FROM usage_rollups"));
        assert!(!old_function.contains("user_cycle_access_usage"));
    }
}
