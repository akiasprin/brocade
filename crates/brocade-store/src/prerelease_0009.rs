//! Explicit conversion of the precise prerelease 0009 installed before terminal CVM history was
//! detached from removable machine identities.

use sqlx::{Postgres, Transaction};

use crate::{pg::MIGRATOR, upgrade::DatabaseUpgradeCheck, PgStore, Result, StoreError};

const ATTACHED_0009_CHECKSUM: &str =
    "3955d2e313a1e967cf6602843d774a45698fa5eb7cdc0ed8d5ebff081cbce1ce2ddbd1dcdc69b5d1099b206cd08ba1a4";
const DETACHED_0009_CHECKSUM: &str =
    "c9ca869a99860a6565f5ce2f6f490fae2d9b62b4dd9a5b80cca60fbe87ba4b672d3de9e1bb29d23f973f499189c4e579";

const NODE_FOREIGN_KEYS: &[&str] = &[
    "cvm_entries_provider_node_fkey",
    "cvm_entry_publications_provider_node_fkey",
    "cvm_entry_tunnels_entry_node_fkey",
    "cvm_entry_tunnels_target_node_fkey",
    "cvm_instances_node_id_fkey",
    "cvm_nic_allocations_consumer_node_fkey",
    "cvm_nic_allocations_provider_node_fkey",
    "cvm_nic_bindings_consumer_node_fkey",
    "cvm_nic_bindings_provider_node_fkey",
    "cvm_nic_publications_provider_node_fkey",
];

const RESET: &str = r#"
UPDATE deployment_target_state
SET cvm_revision_id = NULL,
    cvm_machine_ready = FALSE,
    desired_structure = desired_structure - 'cvm',
    observed_after = observed_after - 'cvm',
    verdict = verdict - 'cvm_pending' - 'cvm_status'
WHERE cvm_revision_id IS NOT NULL
   OR cvm_machine_ready
   OR desired_structure ? 'cvm'
   OR coalesce(observed_after, '{}'::jsonb) ? 'cvm'
   OR coalesce(verdict, '{}'::jsonb) ? 'cvm_pending'
   OR coalesce(verdict, '{}'::jsonb) ? 'cvm_status';

TRUNCATE TABLE
    cvm_guest_credentials,
    cvm_instance_access_events,
    cvm_host_policy_events,
    cvm_host_policies,
    cvm_nic_monthly_usage,
    cvm_nic_meter_cursors,
    cvm_nic_route_versions,
    cvm_nic_bindings,
    cvm_nic_resources,
    cvm_entries,
    cvm_entry_tunnels,
    cvm_entry_publication_versions,
    cvm_entry_publications,
    cvm_entry_nodes,
    cvm_nic_allocations,
    cvm_nic_requests,
    cvm_nic_publication_versions,
    cvm_nic_publications,
    cvm_nic_nodes,
    cvm_terminal_policy_events,
    cvm_terminal_policies,
    cvm_observation_events,
    cvm_observation_samples,
    cvm_observation_state,
    cvm_quota_changes,
    cvm_operations,
    cvm_spec_artifacts,
    cvm_spec_artifact_sets,
    cvm_revision_bindings,
    cvm_revision_instances,
    cvm_serving_nodes,
    cvm_revision_nodes,
    cvm_image_manifests,
    cvm_spec_versions,
    cvm_interfaces,
    cvm_volumes,
    cvm_instances,
    cvm_network_quota_events,
    cvm_network_quota_hosts,
    cvm_hosts
RESTART IDENTITY;
"#;

const DETACH_FOREIGN_KEYS: &str = r#"
ALTER TABLE cvm_instances DROP CONSTRAINT cvm_instances_node_id_fkey;
ALTER TABLE cvm_nic_publications DROP CONSTRAINT cvm_nic_publications_provider_node_fkey;
ALTER TABLE cvm_nic_allocations
    DROP CONSTRAINT cvm_nic_allocations_provider_node_fkey,
    DROP CONSTRAINT cvm_nic_allocations_consumer_node_fkey;
ALTER TABLE cvm_nic_bindings
    DROP CONSTRAINT cvm_nic_bindings_provider_node_fkey,
    DROP CONSTRAINT cvm_nic_bindings_consumer_node_fkey;
ALTER TABLE cvm_entry_publications DROP CONSTRAINT cvm_entry_publications_provider_node_fkey;
ALTER TABLE cvm_entry_tunnels
    DROP CONSTRAINT cvm_entry_tunnels_entry_node_fkey,
    DROP CONSTRAINT cvm_entry_tunnels_target_node_fkey;
ALTER TABLE cvm_entries DROP CONSTRAINT cvm_entries_provider_node_fkey;
"#;

#[derive(Clone, Copy)]
enum Prerelease0009 {
    Attached,
    Detached,
}

async fn node_foreign_key_count(tx: &mut Transaction<'_, Postgres>) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT count(*)
           FROM pg_constraint
          WHERE contype='f' AND confrelid='nodes'::regclass AND confdeltype='r'
            AND conname=ANY($1)",
    )
    .bind(NODE_FOREIGN_KEYS)
    .fetch_one(&mut **tx)
    .await?)
}

async fn previous_0009(tx: &mut Transaction<'_, Postgres>) -> Result<Option<Prerelease0009>> {
    let history: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version,success,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut **tx)
            .await?;
    let source: Vec<_> = MIGRATOR.iter().collect();
    if history.len() != 9
        || source.len() != 9
        || !history[..8]
            .iter()
            .zip(&source[..8])
            .all(|(actual, expected)| {
                actual.0 == expected.version
                    && actual.1
                    && actual.2.as_slice() == expected.checksum.as_ref()
            })
        || history[8].0 != 9
        || !history[8].1
    {
        return Ok(None);
    }
    let checksum = brocade_core::hash::hex_lower(&history[8].2);
    let retired_tables_absent: bool = sqlx::query_scalar(
        "SELECT to_regclass('cvm_user_host_network_quotas') IS NULL
             AND to_regclass('cvm_quotas') IS NULL
             AND to_regclass('cvm_instance_access_events') IS NOT NULL
             AND to_regclass('cvm_guest_credentials') IS NOT NULL",
    )
    .fetch_one(&mut **tx)
    .await?;
    if !retired_tables_absent {
        return Ok(None);
    }
    let foreign_keys = node_foreign_key_count(tx).await?;
    Ok(match (checksum.as_str(), foreign_keys) {
        (ATTACHED_0009_CHECKSUM, count) if count == NODE_FOREIGN_KEYS.len() as i64 => {
            Some(Prerelease0009::Attached)
        }
        (DETACHED_0009_CHECKSUM, 0) => Some(Prerelease0009::Detached),
        _ => None,
    })
}

impl PgStore {
    /// Read-only opt-in for the exact 0009 bytes briefly deployed before the migration was
    /// consolidated. Reporting 9 as pending makes the installer back up before conversion.
    pub async fn check_prerelease_0009_upgrade(&self) -> Result<DatabaseUpgradeCheck> {
        let mut check = self.check_database_upgrade().await?;
        if check.compatible || check.installed_versions != [1, 2, 3, 4, 5, 6, 7, 8, 9] {
            return Ok(check);
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL search_path=public,pg_catalog")
            .execute(&mut *tx)
            .await?;
        if previous_0009(&mut tx).await?.is_some() {
            check.compatible = true;
            check.pending_versions = vec![9];
            check.reason = None;
        }
        tx.commit().await?;
        Ok(check)
    }

    /// Destructively reset CVM data only for the exact known prerelease receipt and schema.
    pub async fn migrate_prerelease_0009_schema(&self) -> Result<()> {
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
        sqlx::query("SELECT pg_advisory_xact_lock(72625104422009)")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "LOCK TABLE _sqlx_migrations,cvm_instances,cvm_nic_publications,
                        cvm_nic_allocations,cvm_nic_bindings,cvm_entry_publications,
                        cvm_entry_tunnels,cvm_entries IN ACCESS EXCLUSIVE MODE",
        )
        .execute(&mut *tx)
        .await?;
        let source = previous_0009(&mut tx).await?.ok_or_else(|| {
            StoreError::InvalidData("未匹配已核验的 0009 历史和结构，未转换".into())
        })?;
        sqlx::raw_sql(RESET).execute(&mut *tx).await?;
        if matches!(source, Prerelease0009::Attached) {
            sqlx::raw_sql(DETACH_FOREIGN_KEYS).execute(&mut *tx).await?;
        }
        if node_foreign_key_count(&mut tx).await? != 0 {
            return Err(StoreError::InvalidData(
                "CVM 历史机器外键转换验证失败，未更新迁移回执".into(),
            ));
        }
        let checksum = MIGRATOR
            .iter()
            .find(|migration| migration.version == 9)
            .ok_or_else(|| StoreError::InvalidData("缺少 0009 迁移".into()))?
            .checksum
            .as_ref();
        sqlx::query("UPDATE _sqlx_migrations SET checksum=$1 WHERE version=9")
            .bind(checksum)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.migrate_database_schema().await
    }
}
