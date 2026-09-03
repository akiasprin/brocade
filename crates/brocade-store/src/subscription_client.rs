//! Durable client-only subscription checkpoint.
//!
//! The head is advanced in the same transaction as a committed model revision. A separate
//! pointer on `subscription_serving_state` lets subscription requests read topology,
//! permissions, and client input atomically without consulting mutable model tables.

use brocade_core::{
    client_config::{SubscriptionClientConfig, SUBSCRIPTION_CLIENT_CONFIG_SCHEMA},
    compile::compile,
    hash::sha256_hex,
    model::ModelSnapshot,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{Result, StoreError};

#[derive(Debug, Clone)]
pub(crate) struct LoadedClientSnapshot {
    pub(crate) id: u64,
    pub(crate) content_sha256: String,
    pub(crate) config: SubscriptionClientConfig,
    stored_document: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClientConfigCommitStatus {
    Unchanged,
    Activated,
    AwaitingFirstTopology,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfigCommitResult {
    pub snapshot_id: u64,
    pub status: ClientConfigCommitStatus,
    pub serving_generation: Option<u64>,
    pub pending_topology: Vec<String>,
}

/// Initialize old databases without changing what an existing subscription renders.
pub(crate) async fn ensure_checkpoint(pool: &PgPool) -> Result<()> {
    let mut tx = pool.begin().await?;
    ensure_state_row_tx(&mut tx).await?;
    let head = lock_head_id_tx(&mut tx).await?;
    let serving = sqlx::query(
        "SELECT topology_revision_id, client_snapshot_id
           FROM subscription_serving_state
          WHERE id = TRUE
          FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?;

    match serving {
        Some(row) => {
            let topology_revision = to_u64(
                "topology_revision_id",
                row.try_get::<i64, _>("topology_revision_id")?,
            )?;
            let serving_client = row
                .try_get::<Option<i64>, _>("client_snapshot_id")?
                .map(|id| to_u64("client_snapshot_id", id))
                .transpose()?;
            match serving_client {
                Some(snapshot_id) => {
                    // Verify before declaring the store ready. A broken immutable checkpoint is
                    // evidence to repair, not a reason to silently rebuild from mutable head.
                    load_client_snapshot_tx(&mut tx, snapshot_id).await?;
                    match head {
                        Some(head_id) if head_id != snapshot_id => {
                            // A different head is valid after an explicit serving rollback, but it
                            // is still part of the durable checkpoint and must pass the same
                            // integrity check before startup is considered ready.
                            load_client_snapshot_tx(&mut tx, head_id).await?;
                        }
                        None => update_head_tx(&mut tx, snapshot_id).await?,
                        Some(_) => {}
                    }
                }
                None => {
                    // Preserve the pre-upgrade bytes: the topology revision is the only client
                    // input which was previously visible.
                    let topology =
                        crate::materialize::load_immutable_snapshot_tx(&mut tx, topology_revision)
                            .await?;
                    let config = SubscriptionClientConfig::from_snapshot(&topology);
                    let snapshot = insert_snapshot_tx(
                        &mut tx,
                        topology_revision,
                        &config,
                        "preserve-serving-backfill",
                    )
                    .await?;
                    update_head_tx(&mut tx, snapshot.id).await?;
                    sqlx::query(
                        "UPDATE subscription_serving_state
                            SET client_snapshot_id = $1,
                                updated_at = now()
                          WHERE id = TRUE",
                    )
                    .bind(to_i64("client_snapshot_id", snapshot.id)?)
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }
        None if head.is_none() => {
            // There is no old subscription output to preserve. Retain the latest committed client
            // intent so the first topology release can compose it immediately.
            let revision = sqlx::query_scalar::<_, i64>(
                "SELECT current_revision FROM control_state WHERE id = TRUE",
            )
            .fetch_one(&mut *tx)
            .await?;
            let revision = to_u64("current_revision", revision)?;
            let snapshot =
                crate::materialize::load_immutable_snapshot_tx(&mut tx, revision).await?;
            let config = SubscriptionClientConfig::from_snapshot(&snapshot);
            let client = insert_snapshot_tx(
                &mut tx,
                revision,
                &config,
                "initialize-before-first-serving",
            )
            .await?;
            update_head_tx(&mut tx, client.id).await?;
        }
        None => {
            load_client_snapshot_tx(&mut tx, head.expect("matched Some head")).await?;
        }
    }

    tx.commit().await?;
    Ok(())
}

/// Advance the committed client head and, where topology already serves, its active pointer.
pub(crate) async fn advance_from_committed_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
) -> Result<()> {
    ensure_state_row_tx(tx).await?;
    let head_id = lock_head_id_tx(tx).await?;
    let serving = lock_serving_tx(tx).await?;
    let desired = crate::materialize::load_immutable_snapshot_tx(tx, revision_id).await?;
    let previous = match head_id {
        Some(id) => Some(load_client_snapshot_tx(tx, id).await?),
        None => None,
    };
    let config = SubscriptionClientConfig::advance(
        previous.as_ref().map(|snapshot| &snapshot.config),
        &desired,
    );
    let (document, content_sha256) = encode_document(&config, previous.as_ref())?;
    if previous
        .as_ref()
        .is_some_and(|snapshot| snapshot.content_sha256 == content_sha256)
    {
        return Ok(());
    }

    if let Some((topology_revision, permissions_revision, _)) = serving {
        validate_revision_combination_tx(
            tx,
            topology_revision,
            permissions_revision,
            &config,
            "current serving state",
        )
        .await?;
    }
    validate_open_topologies_tx(tx, serving.map(|(_, permissions, _)| permissions), &config)
        .await?;

    let snapshot = insert_encoded_snapshot_tx(
        tx,
        revision_id,
        config,
        document,
        content_sha256,
        "model-commit",
    )
    .await?;
    update_head_tx(tx, snapshot.id).await?;
    if serving.is_some() {
        sqlx::query(
            "UPDATE subscription_serving_state
                SET client_snapshot_id = $1,
                    generation = generation + 1,
                    updated_at = now()
              WHERE id = TRUE",
        )
        .bind(to_i64("client_snapshot_id", snapshot.id)?)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Lock and return the current committed client head, creating one from `revision_id` only for a
/// database initialized before the checkpoint existed.
pub(crate) async fn locked_head_for_revision_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
) -> Result<LoadedClientSnapshot> {
    ensure_state_row_tx(tx).await?;
    match lock_head_id_tx(tx).await? {
        Some(id) => load_client_snapshot_tx(tx, id).await,
        None => {
            let topology = crate::materialize::load_immutable_snapshot_tx(tx, revision_id).await?;
            let config = SubscriptionClientConfig::from_snapshot(&topology);
            let snapshot =
                insert_snapshot_tx(tx, revision_id, &config, "first-topology-fallback").await?;
            update_head_tx(tx, snapshot.id).await?;
            Ok(snapshot)
        }
    }
}

pub(crate) async fn load_client_snapshot(
    pool: &PgPool,
    snapshot_id: u64,
) -> Result<LoadedClientSnapshot> {
    let row = sqlx::query(
        "SELECT id, source_revision_id, schema_version, document, content_sha256
           FROM subscription_client_snapshots
          WHERE id = $1",
    )
    .bind(to_i64("client_snapshot_id", snapshot_id)?)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| {
        StoreError::InvalidData(format!(
            "subscription client snapshot {snapshot_id} does not exist"
        ))
    })?;
    decode_snapshot_row(&row)
}

pub(crate) async fn load_client_snapshot_tx(
    tx: &mut Transaction<'_, Postgres>,
    snapshot_id: u64,
) -> Result<LoadedClientSnapshot> {
    let row = sqlx::query(
        "SELECT id, source_revision_id, schema_version, document, content_sha256
           FROM subscription_client_snapshots
          WHERE id = $1",
    )
    .bind(to_i64("client_snapshot_id", snapshot_id)?)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| {
        StoreError::InvalidData(format!(
            "subscription client snapshot {snapshot_id} does not exist"
        ))
    })?;
    decode_snapshot_row(&row)
}

pub(crate) fn compose(
    topology: ModelSnapshot,
    permissions: &ModelSnapshot,
    client: &SubscriptionClientConfig,
) -> Result<ModelSnapshot> {
    let topology = client.apply(topology).map_err(StoreError::InvalidData)?;
    Ok(crate::serving::permission_projection(topology, permissions))
}

pub(crate) async fn validate_revision_combination_tx(
    tx: &mut Transaction<'_, Postgres>,
    topology_revision: u64,
    permissions_revision: u64,
    client: &SubscriptionClientConfig,
    context: &str,
) -> Result<()> {
    let topology = crate::materialize::load_immutable_snapshot_tx(tx, topology_revision).await?;
    let permissions =
        crate::materialize::load_immutable_snapshot_tx(tx, permissions_revision).await?;
    let composed = compose(topology, &permissions, client)?;
    compile(&composed).ensure_publishable().map_err(|blocked| {
        StoreError::InvalidData(format!(
            "subscription client config is invalid with {context}: {:?}",
            blocked.diagnostics
        ))
    })?;
    Ok(())
}

pub(crate) async fn commit_result_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
) -> Result<ClientConfigCommitResult> {
    let row = sqlx::query(
        "SELECT client.head_snapshot_id,
                snapshot.source_revision_id,
                serving.client_snapshot_id AS serving_client_snapshot_id,
                serving.topology_revision_id,
                serving.generation
           FROM subscription_client_state client
           LEFT JOIN subscription_client_snapshots snapshot
             ON snapshot.id = client.head_snapshot_id
           LEFT JOIN subscription_serving_state serving ON serving.id = TRUE
          WHERE client.id = TRUE",
    )
    .fetch_one(&mut **tx)
    .await?;
    let snapshot_id = to_u64(
        "head_snapshot_id",
        row.try_get::<Option<i64>, _>("head_snapshot_id")?
            .ok_or_else(|| {
                StoreError::InvalidData("subscription client head is empty".to_owned())
            })?,
    )?;
    let source_revision = to_u64(
        "source_revision_id",
        row.try_get::<i64, _>("source_revision_id")?,
    )?;
    let serving_snapshot = row
        .try_get::<Option<i64>, _>("serving_client_snapshot_id")?
        .map(|id| to_u64("serving_client_snapshot_id", id))
        .transpose()?;
    let serving_generation = row
        .try_get::<Option<i64>, _>("generation")?
        .map(|generation| to_u64("subscription generation", generation))
        .transpose()?;
    let status = if source_revision != revision_id {
        ClientConfigCommitStatus::Unchanged
    } else if serving_snapshot == Some(snapshot_id) {
        ClientConfigCommitStatus::Activated
    } else {
        ClientConfigCommitStatus::AwaitingFirstTopology
    };

    let pending_topology = match row.try_get::<Option<i64>, _>("topology_revision_id")? {
        Some(topology_revision) => {
            let client = load_client_snapshot_tx(tx, snapshot_id).await?;
            let topology = crate::materialize::load_immutable_snapshot_tx(
                tx,
                to_u64("topology_revision_id", topology_revision)?,
            )
            .await?;
            client.config.pending_topology(&topology)
        }
        None => Vec::new(),
    };
    Ok(ClientConfigCommitResult {
        snapshot_id,
        status,
        serving_generation,
        pending_topology,
    })
}

async fn validate_open_topologies_tx(
    tx: &mut Transaction<'_, Postgres>,
    serving_permissions: Option<u64>,
    client: &SubscriptionClientConfig,
) -> Result<()> {
    let revisions = sqlx::query_scalar::<_, i64>(
        "SELECT DISTINCT revision_id
           FROM deployments
          WHERE kind = 'config'
            AND active = TRUE
            AND status IN ('planned', 'running', 'halted')
          ORDER BY revision_id",
    )
    .fetch_all(&mut **tx)
    .await?;
    for revision in revisions {
        let revision = to_u64("open deployment revision_id", revision)?;
        validate_revision_combination_tx(
            tx,
            revision,
            serving_permissions.unwrap_or(revision),
            client,
            &format!("open config deployment revision {revision}"),
        )
        .await?;
    }
    Ok(())
}

async fn ensure_state_row_tx(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    sqlx::query(
        "INSERT INTO subscription_client_state (id, head_snapshot_id)
         VALUES (TRUE, NULL)
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn lock_head_id_tx(tx: &mut Transaction<'_, Postgres>) -> Result<Option<u64>> {
    let id = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT head_snapshot_id
           FROM subscription_client_state
          WHERE id = TRUE
          FOR UPDATE",
    )
    .fetch_one(&mut **tx)
    .await?;
    id.map(|id| to_u64("head_snapshot_id", id)).transpose()
}

async fn lock_serving_tx(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Option<(u64, u64, Option<u64>)>> {
    let row = sqlx::query(
        "SELECT topology_revision_id, permissions_revision_id, client_snapshot_id
           FROM subscription_serving_state
          WHERE id = TRUE
          FOR UPDATE",
    )
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|row| {
        Ok((
            to_u64(
                "topology_revision_id",
                row.try_get::<i64, _>("topology_revision_id")?,
            )?,
            to_u64(
                "permissions_revision_id",
                row.try_get::<i64, _>("permissions_revision_id")?,
            )?,
            row.try_get::<Option<i64>, _>("client_snapshot_id")?
                .map(|id| to_u64("client_snapshot_id", id))
                .transpose()?,
        ))
    })
    .transpose()
}

async fn update_head_tx(tx: &mut Transaction<'_, Postgres>, snapshot_id: u64) -> Result<()> {
    sqlx::query(
        "UPDATE subscription_client_state
            SET head_snapshot_id = $1,
                updated_at = now()
          WHERE id = TRUE",
    )
    .bind(to_i64("head_snapshot_id", snapshot_id)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_snapshot_tx(
    tx: &mut Transaction<'_, Postgres>,
    source_revision: u64,
    config: &SubscriptionClientConfig,
    reason: &str,
) -> Result<LoadedClientSnapshot> {
    let (document, content_sha256) = encode_document(config, None)?;
    insert_encoded_snapshot_tx(
        tx,
        source_revision,
        config.clone(),
        document,
        content_sha256,
        reason,
    )
    .await
}

async fn insert_encoded_snapshot_tx(
    tx: &mut Transaction<'_, Postgres>,
    source_revision: u64,
    config: SubscriptionClientConfig,
    document: Value,
    content_sha256: String,
    reason: &str,
) -> Result<LoadedClientSnapshot> {
    let inserted = sqlx::query_scalar::<_, i64>(
        "INSERT INTO subscription_client_snapshots (
             source_revision_id, schema_version, document, content_sha256, reason
         ) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (source_revision_id, content_sha256) DO NOTHING
         RETURNING id",
    )
    .bind(to_i64("source_revision_id", source_revision)?)
    .bind(i32::try_from(SUBSCRIPTION_CLIENT_CONFIG_SCHEMA).expect("schema fits i32"))
    .bind(&document)
    .bind(&content_sha256)
    .bind(reason)
    .fetch_optional(&mut **tx)
    .await?;
    let id = match inserted {
        Some(id) => id,
        None => {
            sqlx::query_scalar::<_, i64>(
                "SELECT id
               FROM subscription_client_snapshots
              WHERE source_revision_id = $1 AND content_sha256 = $2",
            )
            .bind(to_i64("source_revision_id", source_revision)?)
            .bind(&content_sha256)
            .fetch_one(&mut **tx)
            .await?
        }
    };
    Ok(LoadedClientSnapshot {
        id: to_u64("client snapshot id", id)?,
        content_sha256,
        config,
        stored_document: document,
    })
}

fn decode_snapshot_row(row: &sqlx::postgres::PgRow) -> Result<LoadedClientSnapshot> {
    let id = to_u64("client snapshot id", row.try_get::<i64, _>("id")?)?;
    let _source_revision = to_u64(
        "source_revision_id",
        row.try_get::<i64, _>("source_revision_id")?,
    )?;
    let schema_version: i32 = row.try_get("schema_version")?;
    if schema_version != i32::try_from(SUBSCRIPTION_CLIENT_CONFIG_SCHEMA).expect("schema fits") {
        return Err(StoreError::InvalidData(format!(
            "subscription client snapshot {id} uses unsupported schema {schema_version}"
        )));
    }
    let stored_document: Value = row.try_get("document")?;
    let expected: String = row.try_get("content_sha256")?;
    let actual = stored_document_sha256(&stored_document)?;
    if actual != expected {
        return Err(StoreError::InvalidData(format!(
            "subscription client snapshot {id} content hash mismatch"
        )));
    }
    let mut document = stored_document.clone();
    crate::materialize::open_snapshot_external_credentials(&mut document)?;
    fold_removed_projection_controls(&mut document);
    let config = serde_json::from_value::<SubscriptionClientConfig>(document)?;
    if config.schema != SUBSCRIPTION_CLIENT_CONFIG_SCHEMA {
        return Err(StoreError::InvalidData(format!(
            "subscription client snapshot {id} document schema {} does not match row schema {schema_version}",
            config.schema
        )));
    }
    Ok(LoadedClientSnapshot {
        id,
        content_sha256: expected,
        config,
        stored_document,
    })
}

fn encode_document(
    config: &SubscriptionClientConfig,
    previous: Option<&LoadedClientSnapshot>,
) -> Result<(Value, String)> {
    let mut document = serde_json::to_value(config)?;
    let values = document
        .get_mut("external_outbounds")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            StoreError::InvalidData(
                "subscription client document external_outbounds must be an array".to_owned(),
            )
        })?;
    for (index, outbound) in config.external_outbounds.iter().enumerate() {
        if matches!(
            outbound.protocol,
            brocade_core::model::ExternalOutboundProtocol::Warp { .. }
        ) {
            continue;
        }
        let sealed = previous
            .and_then(|previous| {
                let old = previous
                    .config
                    .external_outbounds
                    .iter()
                    .find(|old| old.id == outbound.id)?;
                (old.tenant == outbound.tenant
                    && old.protocol.credential() == outbound.protocol.credential())
                .then(|| {
                    previous
                        .stored_document
                        .get("external_outbounds")?
                        .as_array()?
                        .iter()
                        .find(|value| {
                            value.get("id").and_then(Value::as_str) == Some(&outbound.id)
                        })?
                        .pointer("/protocol/v/credential")?
                        .as_str()
                        .map(str::to_owned)
                })?
            })
            .map(Ok)
            .unwrap_or_else(|| {
                crate::secrets::seal(
                    &crate::secrets::external_outbound_context(&outbound.tenant, &outbound.id),
                    outbound.protocol.credential(),
                )
            })?;
        let credential = values[index]
            .pointer_mut("/protocol/v/credential")
            .ok_or_else(|| {
                StoreError::InvalidData(format!(
                    "external outbound {}/{} has no client credential field",
                    outbound.tenant, outbound.id
                ))
            })?;
        *credential = Value::String(sealed);
    }
    let content_sha256 = stored_document_sha256(&document)?;
    Ok((document, content_sha256))
}

fn stored_document_sha256(document: &Value) -> Result<String> {
    Ok(sha256_hex(&serde_json::to_vec(document)?))
}

/// Client checkpoints written while TLS fingerprint and HTTP version were independently managed
/// placed both values on every projection candidate. Ignore those obsolete overrides while
/// retaining the immutable stored bytes for integrity verification and credential reuse.
fn fold_removed_projection_controls(document: &mut Value) {
    let Some(ingresses) = document.get_mut("ingresses").and_then(Value::as_object_mut) else {
        return;
    };
    for ingress in ingresses.values_mut() {
        let Some(projections) = ingress
            .get_mut("projections")
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        for projection in projections.values_mut() {
            let Some(projection) = projection.as_object_mut() else {
                continue;
            };
            projection.remove("xhttp_alpn");
            projection.remove("tls_fingerprint");
        }
    }
}

fn to_u64(field: &str, value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{field} is negative: {value}")))
}

fn to_i64(field: &str, value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{field} is too large: {value}")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn obsolete_projection_controls_are_folded_before_decoding() {
        let mut document = serde_json::json!({
            "ingresses": {
                "i-main": {
                    "projections": {
                        "contract": {
                            "v4": { "host": "edge.example", "port": 443 },
                            "xhttp_alpn": "http1",
                            "tls_fingerprint": "none"
                        }
                    }
                }
            }
        });

        super::fold_removed_projection_controls(&mut document);

        let projection = &document["ingresses"]["i-main"]["projections"]["contract"];
        assert!(projection.get("xhttp_alpn").is_none());
        assert!(projection.get("tls_fingerprint").is_none());
        assert_eq!(projection["v4"]["host"], "edge.example");
    }
}
