//! Read-only release compatibility checks. Migration receipts remain immutable.
use crate::{pg::MIGRATOR, PgStore, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct DatabaseUpgradeCheck {
    pub compatible: bool,
    pub installed_versions: Vec<i64>,
    pub pending_versions: Vec<i64>,
    pub reason: Option<String>,
}

impl PgStore {
    /// Inspect an existing installation without creating a database, tables or receipts.
    pub async fn check_database_upgrade(&self) -> Result<DatabaseUpgradeCheck> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *tx)
            .await?;
        let exists: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
            .fetch_one(&mut *tx)
            .await?;
        let installed: Vec<(i64, bool, Vec<u8>)> = if exists {
            sqlx::query_as(
                "SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version",
            )
            .fetch_all(&mut *tx)
            .await?
        } else {
            Vec::new()
        };
        tx.commit().await?;
        Ok(check_history(&installed))
    }

    /// Schema-only migration for the offline upgrader. Normal startup performs its initializers
    /// separately, so a failed initializer cannot be mistaken for a rolled-back schema change.
    pub async fn migrate_database_schema(&self) -> Result<()> {
        MIGRATOR.run(self.pool()).await?;
        Ok(())
    }
}

fn check_history(installed: &[(i64, bool, Vec<u8>)]) -> DatabaseUpgradeCheck {
    let source: Vec<_> = MIGRATOR.iter().collect();
    let mut check = DatabaseUpgradeCheck {
        compatible: false,
        installed_versions: installed.iter().map(|r| r.0).collect(),
        pending_versions: source
            .iter()
            .filter(|m| !installed.iter().any(|r| r.0 == m.version))
            .map(|m| m.version)
            .collect(),
        reason: None,
    };
    check.reason = if installed.is_empty() {
        Some("未找到既有迁移历史；升级不会初始化空数据库，请核对数据库配置".into())
    } else if installed.len() > source.len() {
        Some("数据库包含本程序不认识的较新迁移；不能降级".into())
    } else {
        installed
            .iter()
            .zip(&source)
            .find_map(|(actual, expected)| {
                if actual.0 != expected.version {
                    Some("迁移历史不连续或包含未知版本".into())
                } else if !actual.1 {
                    Some(format!("迁移 {} 存在失败回执，请先恢复", actual.0))
                } else if actual.2.as_slice() != expected.checksum.as_ref() {
                    Some(format!(
                        "迁移 {} 校验值不匹配；仅支持 v0.3.1 起的兼容基线，不会改写迁移历史",
                        actual.0
                    ))
                } else {
                    None
                }
            })
    };
    check.compatible = check.reason.is_none();
    check
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_every_receipt_without_accepting_unknown_or_rewritten_history() {
        let history: Vec<_> = MIGRATOR
            .iter()
            .map(|m| (m.version, true, m.checksum.to_vec()))
            .collect();
        assert!(history.len() >= 2);
        let initial = check_history(&history[..1]);
        assert!(initial.compatible);
        assert_eq!(
            initial.pending_versions,
            history[1..].iter().map(|m| m.0).collect::<Vec<_>>()
        );
        assert!(check_history(&history).compatible);
        assert!(!check_history(&[]).compatible);
        let mut invalid = history.clone();
        invalid[0].2[0] ^= 1;
        assert!(!check_history(&invalid).compatible);
        invalid = history.clone();
        invalid[0].1 = false;
        assert!(!check_history(&invalid).compatible);
        assert!(!check_history(&history[1..]).compatible);
        invalid = history;
        invalid.push((9999, true, vec![0; 48]));
        assert!(!check_history(&invalid).compatible);
    }
}
