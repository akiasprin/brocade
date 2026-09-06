use std::collections::BTreeSet;

mod artifacts;
mod lists;
mod nodes;
mod redact;
mod steps;
mod subscriptions;
mod types;
pub use artifacts::*;
pub use lists::*;
pub use nodes::*;
pub(crate) use redact::*;
pub use steps::*;
pub use subscriptions::*;
pub use types::*;

use brocade_core::{
    artifacts::{grants, hy2_port_hop, phantun, subscription, wireguard, xray},
    client_config::ClientProjectionDownloadEndpoint,
    compile::compile,
    format::{ini, json as json_format, uri, yaml},
    hash::sha256_hex,
    model::{
        Action, AnyTlsMasquerade, AnyTlsSecurity, AppView, Chain, Dns, DomainStrategy,
        ExternalOutboundProtocol, ExternalOutboundSecurity, ExternalWarpBinding, Front, Grant,
        HopEncryption, HopWire, HysteriaCongestion, HysteriaMasquerade, HysteriaObfs, Ingress,
        IngressIdentity, IngressWires, IngressWiresWire, ModelSnapshot, NodeConnection, Projection,
        ProjectionDownloadEndpoint, ProjectionEndpoint, Reality, RealityFallbackLimits,
        RealityFallbackMode, RealityFallbackRateLimit, RealitySettings, RealityXhttp, Tls,
        TlsXhttp, Transport, User, WgTransport, Xhttp, XhttpTuning, EXTERNAL_WIREGUARD_MAX_WORKERS,
    },
    text::{
        is_nonzero_host_port, is_reality_fingerprint, is_reality_server_name, normalize_host_port,
    },
};
use ipnet::IpNet;
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::input::{
    dns_kind, ensure_nonzero_port, optional_ipv6_text, required_text, validate_dns,
};
use crate::{
    credentials::{
        generate_reality_keypair, generate_reality_short_id, generate_shadowsocks_psk,
        generate_uuid_v4,
    },
    AdminContext, AdminRole, Result, StoreError,
};

pub async fn redacted_snapshot(
    pool: &PgPool,
    actor: &AdminContext,
    revision: Option<u64>,
) -> Result<ConsoleSnapshot> {
    snapshot_view(load_scoped_snapshot(pool, actor, revision).await?)
}

/// The half that follows once a `ModelSnapshot` is in hand. A draft preview reads its snapshot
/// inside a transaction it is about to roll back (see `draft.rs`) and cannot take the pool-based
/// path above.
pub(crate) fn snapshot_view(snapshot: ModelSnapshot) -> Result<ConsoleSnapshot> {
    let node_egress_dns = snapshot
        .node_egress_dns
        .iter()
        .cloned()
        .map(|policy| ConsoleEgressDnsPolicy {
            node: policy.node,
            position: policy.position,
            selector: policy.selector,
            resolution: policy.resolution,
        })
        .collect();
    let mut value = serde_json::to_value(snapshot)?;
    redact_private_keys(&mut value);
    Ok(ConsoleSnapshot {
        snapshot: value,
        node_egress_dns,
        redacted: true,
    })
}

pub async fn compile_view(
    pool: &PgPool,
    actor: &AdminContext,
    revision: Option<u64>,
) -> Result<CompileView> {
    compile_view_of(&load_scoped_snapshot(pool, actor, revision).await?)
}

pub(crate) fn compile_view_of(snapshot: &ModelSnapshot) -> Result<CompileView> {
    let output = compile(snapshot);
    // The compile inspector must show the partial IR that produced diagnostics even when the
    // model cannot publish. Calling this explicitly keeps that exception visible; artifacts and
    // agent work lists go through CompileOutput's gated projectors.
    let ir = output.unpublishable_view();
    let mut system = serde_json::to_value(ir.system)?;
    let mut apps = serde_json::to_value(ir.apps)?;
    redact_private_keys(&mut system);
    redact_private_keys(&mut apps);

    Ok(CompileView {
        revision: snapshot.revision,
        summary: output.summary,
        diagnostics: output.diagnostics,
        system,
        apps,
        redacted: true,
    })
}

pub async fn create_tenant(
    pool: &PgPool,
    actor: &AdminContext,
    request: CreateTenantRequest,
) -> Result<UpsertTenantResult> {
    let id = required_text(request.id.clone(), "tenant id")?;
    let note = note_or(request.note.as_deref(), || format!("upsert tenant {id}"));
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let changed = create_tenant_tx(&mut tx, actor, revision_id, request).await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    let tenant = load_tenant(pool, &id).await?;
    Ok(UpsertTenantResult {
        revision_id,
        tenant,
    })
}

pub(crate) async fn create_tenant_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    request: CreateTenantRequest,
) -> Result<bool> {
    ensure_tenant_management_allowed(actor, &request.id)?;
    let id = required_text(request.id, "tenant id")?;
    let name = required_text(request.name, "tenant name")?;
    // The WHERE does not compare created_revision: an unchanged name touches the whole row not
    // at all, that field included. Compared, an older row whose created_revision is still NULL
    // would be judged changed for the sake of backfilling metadata and consume a revision number
    // for nothing; and once updated here the row references that number and it can no longer be
    // returned.
    let tenant_changed = sqlx::query(
        "INSERT INTO tenants (id, name, created_revision)
         VALUES ($1, $2, $3)
         ON CONFLICT (id) DO UPDATE SET
            name = EXCLUDED.name,
            created_revision = COALESCE(tenants.created_revision, EXCLUDED.created_revision)
         WHERE tenants.name IS DISTINCT FROM EXCLUDED.name",
    )
    .bind(&id)
    .bind(&name)
    .bind(u64_to_i64(revision_id, "revision_id")?)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    // A tenant is immediately usable as an egress scope. Keeping this in the same model
    // transaction is important: draft preview sees the resource, commit records one revision,
    // and a caller can never observe the tenant without its managed WARP target in between.
    let warp_changed = ensure_default_warp_for_tenant_tx(tx, actor, revision_id, &id).await?;
    Ok(tenant_changed || warp_changed)
}

const DEFAULT_WARP_ADDRESS: &str = "engage.cloudflareclient.com";
const DEFAULT_WARP_PORT: u16 = 2408;

/// Add the one managed WARP target every tenant receives.
///
/// Existing WARP resources win, including ones created before this invariant existed. We do not
/// rename or duplicate them: their references and operator-selected defaults remain authoritative.
/// Callers hold the control-state write lock, which serializes allocation with every ordinary
/// model write and makes the existence check + globally unique id allocation atomic.
async fn ensure_default_warp_for_tenant_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    tenant_id: &str,
) -> Result<bool> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(
             SELECT 1 FROM external_outbounds
              WHERE tenant_id = $1 AND protocol = 'warp'
         )",
    )
    .bind(tenant_id)
    .fetch_one(&mut **tx)
    .await?;
    if exists {
        return Ok(false);
    }

    let id = allocate_default_warp_id_tx(tx, tenant_id).await?;
    upsert_external_outbound_tx(
        tx,
        actor,
        revision_id,
        UpsertExternalOutboundRequest {
            id,
            tenant_id: tenant_id.to_owned(),
            name: "Cloudflare WARP".to_owned(),
            address: DEFAULT_WARP_ADDRESS.to_owned(),
            port: DEFAULT_WARP_PORT,
            protocol: ExternalOutboundProtocol::Warp {
                mtu: 1280,
                keep_alive: 25,
                allowed_ips: vec!["0.0.0.0/0".to_owned(), "::/0".to_owned()],
                no_kernel_tun: false,
                domain_strategy: "ForceIP".to_owned(),
                workers: 0,
            },
            security: ExternalOutboundSecurity::None,
            note: None,
        },
    )
    .await
}

/// Prefer a semantic id in logs and raw snapshots. Very long tenant paths, or an id already used
/// by an unrelated external target, fall back to a stable short digest. The database remains the
/// final collision authority and every fallback is checked before it is returned.
async fn allocate_default_warp_id_tx(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
) -> Result<String> {
    let readable = format!("warp.{tenant_id}");
    if readable.len() <= 32 && external_outbound_id_available_tx(tx, &readable).await? {
        return Ok(readable);
    }

    for attempt in 0..64_u8 {
        let digest = sha256_hex(format!("default-warp:{tenant_id}:{attempt}").as_bytes());
        let candidate = format!("warp-{}", &digest[..12]);
        if external_outbound_id_available_tx(tx, &candidate).await? {
            return Ok(candidate);
        }
    }
    Err(StoreError::Conflict(format!(
        "cannot allocate a unique default WARP id for tenant {tenant_id}"
    )))
}

async fn external_outbound_id_available_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<bool> {
    Ok(!sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM external_outbounds WHERE id = $1)",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?)
}

/// Reconcile development/production databases created before default WARP resources existed.
///
/// This is deliberately a normal model revision rather than migration DML: the control-state
/// number and stored snapshot move together, historical revisions stay truthful, and a second
/// process or restart becomes a no-op.
pub(crate) async fn ensure_default_warp_outbounds(pool: &PgPool) -> Result<usize> {
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let tenants = sqlx::query_scalar::<_, String>(
        "SELECT tenant.id
           FROM tenants AS tenant
          WHERE NOT EXISTS (
                    SELECT 1 FROM external_outbounds AS outbound
                     WHERE outbound.tenant_id = tenant.id AND outbound.protocol = 'warp'
                )
          ORDER BY tenant.id",
    )
    .fetch_all(&mut *tx)
    .await?;
    if tenants.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }

    let revision_id = insert_revision(
        &mut tx,
        "brocade-system",
        &format!("为 {} 个租户补齐默认 WARP", tenants.len()),
    )
    .await?;
    let actor = AdminContext::system_admin("brocade-system");
    let mut changed = 0_usize;
    for tenant_id in &tenants {
        if ensure_default_warp_for_tenant_tx(&mut tx, &actor, revision_id, tenant_id).await? {
            changed += 1;
        }
    }
    commit_revision(&mut tx, revision_id, previous, changed > 0).await?;
    tx.commit().await?;
    Ok(changed)
}

pub async fn create_user(
    pool: &PgPool,
    actor: &AdminContext,
    request: CreateUserRequest,
) -> Result<UpsertUserResult> {
    let note = note_or(request.note.as_deref(), || {
        format!("create user {}/{}", request.tenant_id, request.id)
    });
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let (user, changed) = create_user_tx(&mut tx, actor, revision_id, request).await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    Ok(UpsertUserResult { revision_id, user })
}

pub(crate) async fn create_user_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    request: CreateUserRequest,
) -> Result<(User, bool)> {
    let tenant_id = required_text(request.tenant_id, "tenant_id")?;
    actor.require_tenant_access(&tenant_id, "user")?;
    let user_id = required_slug(request.id, "user id")?;
    let uuid = generate_uuid_v4()?;
    ensure_tenant_exists_tx(tx, &tenant_id).await?;
    ensure_user_missing(tx, &tenant_id, &user_id).await?;
    // No changed-or-not test here: `ensure_user_missing` already blocked duplicate creation, so
    // reaching this point necessarily inserts a row.
    sqlx::query(
        "INSERT INTO users (tenant_id, id, uuid, created_revision)
         VALUES ($1, $2, $3::uuid, $4)",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(&uuid)
    .bind(u64_to_i64(revision_id, "revision_id")?)
    .execute(&mut **tx)
    .await?;

    Ok((
        User {
            id: user_id,
            tenant: tenant_id,
            uuid,
        },
        true,
    ))
}

pub async fn rotate_user_uuid(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<RotateUserUuidResult> {
    let note = format!("rotate user uuid {tenant_id}/{user_id}");
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let (user, changed) =
        rotate_user_uuid_tx(&mut tx, actor, revision_id, tenant_id, user_id).await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    Ok(RotateUserUuidResult { revision_id, user })
}

pub(crate) async fn rotate_user_uuid_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    tenant_id: &str,
    user_id: &str,
) -> Result<(User, bool)> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let user_id = required_text(user_id, "user id")?;
    actor.require_tenant_access(&tenant_id, "user")?;
    let uuid = generate_uuid_v4()?;
    ensure_user_exists_tx(tx, &tenant_id, &user_id).await?;
    // Rotation means replacing it with a new one, the uuid is generated afresh each time, and
    // there is no such thing as no change.
    sqlx::query(
        "UPDATE users
         SET uuid = $3::uuid,
             created_revision = COALESCE(created_revision, $4)
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(&uuid)
    .bind(u64_to_i64(revision_id, "revision_id")?)
    .execute(&mut **tx)
    .await?;

    Ok((
        User {
            id: user_id,
            tenant: tenant_id,
            uuid,
        },
        true,
    ))
}

pub async fn update_user_status(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    request: UpdateUserStatusRequest,
) -> Result<UpdateUserStatusResult> {
    let note = note_or(request.note.as_deref(), || {
        format!("set user status {tenant_id}/{user_id} {}", request.status)
    });
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let changed = update_user_status_tx(&mut tx, actor, tenant_id, user_id, request).await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    Ok(UpdateUserStatusResult {
        revision_id,
        user: load_user_item(pool, tenant_id, user_id).await?,
    })
}

pub async fn user_profile(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<UserListItem> {
    actor.require_tenant_access(tenant_id, "user profile")?;
    load_user_item(pool, tenant_id, user_id).await
}

pub async fn self_user_profile(pool: &PgPool, actor: &AdminContext) -> Result<UserListItem> {
    let user = actor
        .self_user()
        .ok_or_else(|| StoreError::Forbidden("operator is not bound to a user".to_owned()))?;
    load_user_item(pool, &user.tenant_id, &user.user_id).await
}

/// Account type is operational identity data, not compiled network state, so it changes no
/// revision and requires no deployment.
pub async fn update_user_profile(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    request: UpdateUserProfileRequest,
) -> Result<UserListItem> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let user_id = required_text(user_id, "user id")?;
    actor.require_tenant_access(&tenant_id, "user profile")?;
    let result = sqlx::query(
        "UPDATE users
         SET account_type = $3
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(request.account_type.as_str())
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(StoreError::NotFound(format!("user {tenant_id}/{user_id}")));
    }
    load_user_item(pool, &tenant_id, &user_id).await
}

pub(crate) async fn update_user_status_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    request: UpdateUserStatusRequest,
) -> Result<bool> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let user_id = required_text(user_id, "user id")?;
    let status = normalize_user_status(&request.status)?;
    actor.require_tenant_access(&tenant_id, "user")?;
    ensure_user_exists_tx(tx, &tenant_id, &user_id).await?;
    let changed = sqlx::query(
        "UPDATE users
         SET status = $3
         WHERE tenant_id = $1 AND id = $2 AND status IS DISTINCT FROM $3",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(status)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    Ok(changed)
}

pub async fn upsert_grant(
    pool: &PgPool,
    actor: &AdminContext,
    request: CreateGrantRequest,
) -> Result<UpsertGrantResult> {
    let note = note_or(request.note.as_deref(), || {
        format!(
            "{} grant {}/{} -> {}",
            if request.enabled { "enable" } else { "disable" },
            request.tenant_id,
            request.user_id,
            request.ingress_id
        )
    });
    let enabled = request.enabled;
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let (grant, changed) = upsert_grant_tx(&mut tx, actor, revision_id, request).await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    Ok(UpsertGrantResult {
        revision_id,
        grant,
        enabled,
    })
}

pub(crate) async fn upsert_grant_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    request: CreateGrantRequest,
) -> Result<(Grant, bool)> {
    let app_id = required_text(request.app_id, "app_id")?;
    let tenant_id = required_text(request.tenant_id, "tenant_id")?;
    let user_id = required_text(request.user_id, "user_id")?;
    let ingress_id = required_text(request.ingress_id, "ingress_id")?;
    actor.require_tenant_access(&tenant_id, "grant")?;
    ensure_user_exists_tx(tx, &tenant_id, &user_id).await?;
    ensure_ingress_in_app_tx(tx, &app_id, &ingress_id).await?;
    crate::quota::prepare_operator_grant_tx(
        tx,
        actor,
        &tenant_id,
        &user_id,
        &app_id,
        &ingress_id,
        request.enabled,
    )
    .await?;
    // A grant is whether the row exists and nothing else. So granting one already granted, or
    // revoking one that was never there, is no change.
    let changed = if request.enabled {
        sqlx::query(
            "INSERT INTO grants (app_id, tenant_id, user_id, ingress_id, created_revision)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (app_id, tenant_id, user_id, ingress_id) DO NOTHING",
        )
        .bind(&app_id)
        .bind(&tenant_id)
        .bind(&user_id)
        .bind(&ingress_id)
        .bind(u64_to_i64(revision_id, "revision_id")?)
        .execute(&mut **tx)
        .await?
        .rows_affected()
    } else {
        sqlx::query(
            "DELETE FROM grants
             WHERE app_id = $1 AND tenant_id = $2 AND user_id = $3 AND ingress_id = $4",
        )
        .bind(&app_id)
        .bind(&tenant_id)
        .bind(&user_id)
        .bind(&ingress_id)
        .execute(&mut **tx)
        .await?
        .rows_affected()
    } > 0;

    Ok((
        Grant {
            tenant: tenant_id,
            user: user_id,
            ingress: ingress_id,
        },
        changed,
    ))
}

pub async fn upsert_app(
    pool: &PgPool,
    actor: &AdminContext,
    request: CreateAppRequest,
) -> Result<UpsertAppResult> {
    let id = required_text(request.id.clone(), "app id")?;
    let note = note_or(request.note.as_deref(), || format!("upsert app {id}"));
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let changed = upsert_app_tx(&mut tx, actor, revision_id, request).await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    Ok(UpsertAppResult {
        revision_id,
        app: redacted_value(load_app(pool, &id).await?)?,
    })
}

/// Create the one built-in line group on a genuinely fresh installation.
///
/// The durable flag matters more than the current row count: an operator may intentionally remove
/// every group later, and a restart must not silently recreate one. Existing installations which
/// predate the flag are marked initialized without changing their groups.
pub(crate) async fn ensure_default_app_group(pool: &PgPool) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let initialized: bool = sqlx::query_scalar(
        "SELECT default_app_group_initialized FROM control_state WHERE id = TRUE",
    )
    .fetch_one(&mut *tx)
    .await?;
    if initialized {
        tx.commit().await?;
        return Ok(false);
    }

    let has_apps: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM apps)")
        .fetch_one(&mut *tx)
        .await?;
    if has_apps {
        sqlx::query(
            "UPDATE control_state SET default_app_group_initialized = TRUE WHERE id = TRUE",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(false);
    }

    let revision_id = insert_revision(&mut tx, "brocade-system", "初始化线路默认分组").await?;
    let actor = AdminContext::system_admin("brocade-system");
    let default_app_id = random_app_model_id_tx(&mut tx).await?;
    let changed = upsert_app_tx(
        &mut tx,
        &actor,
        revision_id,
        CreateAppRequest {
            id: default_app_id,
            label: "默认分组".to_owned(),
            note: None,
        },
    )
    .await?;
    sqlx::query("UPDATE control_state SET default_app_group_initialized = TRUE WHERE id = TRUE")
        .execute(&mut *tx)
        .await?;
    commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;
    Ok(changed)
}

pub(crate) async fn upsert_app_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    request: CreateAppRequest,
) -> Result<bool> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can upsert apps".to_owned(),
        ));
    }
    let id = required_text(request.id, "app id")?;
    validate_model_id("app", &id)?;
    let label = required_text(request.label, "app label")?;
    let revision_id = u64_to_i64(revision_id, "revision_id")?;

    let existing_label: Option<String> =
        sqlx::query_scalar("SELECT label FROM apps WHERE id = $1 FOR UPDATE")
            .bind(&id)
            .fetch_optional(&mut **tx)
            .await?;
    if let Some(existing_label) = existing_label {
        if existing_label == label {
            return Ok(false);
        }
        sqlx::query(
            "UPDATE apps
             SET label = $2,
                 created_revision = COALESCE(created_revision, $3)
             WHERE id = $1",
        )
        .bind(&id)
        .bind(&label)
        .bind(revision_id)
        .execute(&mut **tx)
        .await?;
        return Ok(true);
    }

    // A fresh database has no operator-defined order yet: migration backfill establishes the
    // legacy ID order. Keep that fallback exact as new lines arrive. Once the current sequence no
    // longer equals ID order, it is operator-owned; a new line appends instead of silently moving
    // any of those choices. The deferred uniqueness constraint makes the one-statement shift safe.
    let rows = sqlx::query("SELECT id, position FROM apps ORDER BY position, id FOR UPDATE")
        .fetch_all(&mut **tx)
        .await?;
    let positioned = rows
        .iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>("id")?,
                row.try_get::<i32, _>("position")?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let follows_id = positioned.windows(2).all(|pair| pair[0].0 < pair[1].0);
    let append_position = positioned
        .iter()
        .map(|(_, position)| *position)
        .max()
        .map(|position| {
            position
                .checked_add(1)
                .ok_or_else(|| StoreError::InvalidData("app position exceeds i32 range".to_owned()))
        })
        .transpose()?
        .unwrap_or(0);
    let position = if follows_id {
        positioned
            .iter()
            .find(|(existing_id, _)| existing_id > &id)
            .map(|(_, position)| *position)
            .unwrap_or(append_position)
    } else {
        append_position
    };
    if position != append_position {
        sqlx::query("UPDATE apps SET position = position + 1 WHERE position >= $1")
            .bind(position)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO apps (id, label, position, created_revision)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(&id)
    .bind(&label)
    .bind(position)
    .bind(revision_id)
    .execute(&mut **tx)
    .await?;
    Ok(true)
}

/// Insert a new app without inheriting upsert's rename semantics. Draft-created random IDs must
/// fail closed on a collision: treating one as an update would rename an unrelated existing app.
pub(crate) async fn create_app_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    request: CreateAppRequest,
) -> Result<bool> {
    let id = required_text(request.id.clone(), "app id")?;
    let exists = sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM apps WHERE id = $1)")
        .bind(&id)
        .fetch_one(&mut **tx)
        .await?;
    if exists {
        return Err(StoreError::Conflict(format!("app id {id} already exists")));
    }
    upsert_app_tx(tx, actor, revision_id, request).await
}

/// Persist the final order produced by one drag gesture.
///
/// The complete ID list is intentional: pointer movement is transient UI state, and a draft holds
/// one final ordering document rather than a potentially long crossing log. Exact membership checking
/// turns a stale page into a clear conflict instead of silently dropping a concurrently-added line.
pub(crate) async fn reorder_apps_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    ids: Vec<String>,
) -> Result<bool> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can reorder apps".to_owned(),
        ));
    }
    let rows = sqlx::query("SELECT id FROM apps ORDER BY position, id FOR UPDATE")
        .fetch_all(&mut **tx)
        .await?;
    let current = rows
        .iter()
        .map(|row| row.try_get::<String, _>("id"))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let ids = complete_order(ids, &current, "app")?;
    if ids == current {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE apps AS app
         SET position = requested.position
         FROM (
             SELECT id, (ordinality - 1)::integer AS position
             FROM unnest($1::text[]) WITH ORDINALITY AS ordered(id, ordinality)
         ) AS requested
         WHERE app.id = requested.id",
    )
    .bind(&ids)
    .execute(&mut **tx)
    .await?;
    Ok(true)
}

pub(crate) async fn upsert_external_outbound_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    request: UpsertExternalOutboundRequest,
) -> Result<bool> {
    let id = required_slug(request.id, "external outbound id")?;
    let tenant_id = required_text(request.tenant_id, "tenant_id")?;
    actor.require_tenant_access(&tenant_id, "external outbound")?;
    ensure_tenant_exists_tx(tx, &tenant_id).await?;
    let name = required_text(request.name, "external outbound name")?;
    let address = required_text(request.address, "external outbound address")?;
    ensure_nonzero_port(request.port, "external outbound port")?;
    let requested_protocol = match &request.protocol {
        ExternalOutboundProtocol::Vless { .. } => "vless",
        ExternalOutboundProtocol::Shadowsocks2022 { .. } => "shadowsocks2022",
        ExternalOutboundProtocol::Socks5 { .. } => "socks5",
        ExternalOutboundProtocol::HttpConnect { .. } => "http_connect",
        ExternalOutboundProtocol::Wireguard { .. } => "wireguard",
        ExternalOutboundProtocol::Warp { .. } => "warp",
    };

    let existing = sqlx::query(
        "SELECT tenant_id, credential_sealed, protocol FROM external_outbounds WHERE id = $1 FOR UPDATE",
    )
    .bind(&id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(row) = existing.as_ref() {
        if row.try_get::<String, _>("tenant_id")? != tenant_id {
            return Err(StoreError::InvalidData(
                "an existing tunnel cannot be moved to another tenant; create a new tunnel instead"
                    .to_owned(),
            ));
        }
    }
    let credential = request.protocol.credential();
    // WARP has no resource-level credential: its private key and provider token are generated per
    // machine binding and sealed in external_outbound_bindings. Storing an encrypted empty string
    // here made an otherwise harmless default target depend on BROCADE_SECRET_KEY and conveyed no
    // security property, so the column carries an explicit empty sentinel for this protocol.
    let credential_sealed = if requested_protocol == "warp" {
        String::new()
    } else if credential == "<redacted>" {
        let existing = existing.as_ref().ok_or_else(|| {
            StoreError::InvalidData(
                "new external outbound cannot use <redacted> as its credential".to_owned(),
            )
        })?;
        if existing.try_get::<String, _>("protocol")? != requested_protocol {
            return Err(StoreError::InvalidData(
                "changing an external outbound protocol requires a new credential".to_owned(),
            ));
        }
        existing.try_get::<String, _>("credential_sealed")?
    } else {
        let credential = if request.protocol.allows_empty_credential() && credential.is_empty() {
            String::new()
        } else {
            required_text(credential, "external outbound credential")?
        };
        crate::secrets::seal(
            &crate::secrets::external_outbound_context(&tenant_id, &id),
            &credential,
        )?
    };

    let protocol_options = match request.protocol {
        ExternalOutboundProtocol::Vless {
            encryption,
            flow,
            transport,
            ..
        } => json!({ "encryption": encryption, "flow": flow, "transport": transport }),
        ExternalOutboundProtocol::Shadowsocks2022 { method, .. } => {
            json!({ "method": method })
        }
        ExternalOutboundProtocol::Socks5 { username, .. } => {
            json!({ "username": username })
        }
        ExternalOutboundProtocol::HttpConnect { username, .. } => {
            json!({ "username": username })
        }
        ExternalOutboundProtocol::Wireguard {
            peer_public_key,
            local_addresses,
            mtu,
            reserved,
            keep_alive,
            allowed_ips,
            no_kernel_tun,
            domain_strategy,
            ..
        } => json!({
            "peer_public_key": peer_public_key,
            "local_addresses": local_addresses,
            "mtu": mtu,
            "reserved": reserved,
            "keep_alive": keep_alive,
            "allowed_ips": allowed_ips,
            "no_kernel_tun": no_kernel_tun,
            "domain_strategy": domain_strategy,
        }),
        ExternalOutboundProtocol::Warp {
            mtu,
            keep_alive,
            allowed_ips,
            no_kernel_tun,
            domain_strategy,
            workers,
        } => json!({
            "mtu": mtu,
            "keep_alive": keep_alive,
            "allowed_ips": allowed_ips,
            "no_kernel_tun": no_kernel_tun,
            "domain_strategy": domain_strategy,
            "workers": workers,
        }),
    };
    let security = serde_json::to_value(request.security)?;
    let changed = sqlx::query(
        "INSERT INTO external_outbounds
            (id, tenant_id, name, address, port, protocol, credential_sealed,
             protocol_options, security, created_revision)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (id) DO UPDATE SET
            tenant_id = EXCLUDED.tenant_id,
            name = EXCLUDED.name,
            address = EXCLUDED.address,
            port = EXCLUDED.port,
            protocol = EXCLUDED.protocol,
            credential_sealed = EXCLUDED.credential_sealed,
            protocol_options = EXCLUDED.protocol_options,
            security = EXCLUDED.security,
            created_revision = COALESCE(external_outbounds.created_revision, EXCLUDED.created_revision)
         WHERE (external_outbounds.tenant_id, external_outbounds.name, external_outbounds.address,
                external_outbounds.port, external_outbounds.protocol,
                external_outbounds.credential_sealed, external_outbounds.protocol_options,
                external_outbounds.security)
               IS DISTINCT FROM
               (EXCLUDED.tenant_id, EXCLUDED.name, EXCLUDED.address, EXCLUDED.port,
                EXCLUDED.protocol, EXCLUDED.credential_sealed, EXCLUDED.protocol_options,
                EXCLUDED.security)",
    )
    .bind(&id)
    .bind(&tenant_id)
    .bind(&name)
    .bind(&address)
    .bind(i32::from(request.port))
    .bind(requested_protocol)
    .bind(credential_sealed)
    .bind(protocol_options)
    .bind(security)
    .bind(u64_to_i64(revision_id, "revision_id")?)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    Ok(changed)
}

/// Commits one successfully registered WARP device to a tunnel.
///
/// Cloudflare has already been contacted by the HTTP layer when this begins. Keeping this outside
/// `ModelOp` is a hard side-effect boundary: draft preview replays every model operation inside a
/// rolled-back transaction, and a provider registration cannot be rolled back with PostgreSQL.
pub async fn register_warp_binding(
    pool: &PgPool,
    actor: &AdminContext,
    request: RegisterWarpBindingRequest,
) -> Result<RegisterWarpBindingResult> {
    let outbound_id = required_slug(request.outbound_id, "tunnel id")?;
    let node_id = required_slug(request.node_id, "node id")?;
    let device_id = required_text(request.device_id, "WARP device id")?;
    let account_id = required_text(request.account_id, "WARP account id")?;
    let access_token = required_text(request.access_token, "WARP access token")?;
    let private_key = required_text(request.private_key, "WARP private key")?;
    let peer_public_key = required_text(request.peer_public_key, "WARP peer public key")?;
    if request.local_addresses.is_empty()
        || request
            .local_addresses
            .iter()
            .any(|address| address.trim().is_empty())
    {
        return Err(StoreError::InvalidData(
            "WARP registration returned no tunnel addresses".to_owned(),
        ));
    }
    if !request.reserved.is_empty() && request.reserved.len() != 3 {
        return Err(StoreError::InvalidData(
            "WARP client_id must decode to exactly three reserved bytes".to_owned(),
        ));
    }

    let note = note_or(request.note.as_deref(), || {
        format!("bind WARP tunnel {outbound_id} to {node_id}")
    });
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;

    let target = sqlx::query(
        "SELECT external_outbounds.tenant_id, external_outbounds.protocol,
                nodes.tenant_id AS node_tenant_id, nodes.retired_at::text AS retired_at
         FROM external_outbounds
         JOIN nodes ON nodes.id = $2
         WHERE external_outbounds.id = $1
         FOR UPDATE OF external_outbounds, nodes",
    )
    .bind(&outbound_id)
    .bind(&node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        StoreError::NotFound(format!(
            "WARP tunnel {outbound_id} or node {node_id} not found"
        ))
    })?;
    let tenant_id = target.try_get::<String, _>("tenant_id")?;
    actor.require_tenant_access(&tenant_id, "WARP tunnel")?;
    if target.try_get::<String, _>("protocol")? != "warp" {
        return Err(StoreError::InvalidData(format!(
            "tunnel {outbound_id} is not a managed WARP tunnel"
        )));
    }
    if target.try_get::<String, _>("node_tenant_id")? != tenant_id {
        return Err(StoreError::InvalidData(
            "a WARP tunnel can only bind a machine in the same tenant".to_owned(),
        ));
    }
    if target.try_get::<Option<String>, _>("retired_at")?.is_some() {
        return Err(StoreError::InvalidData(
            "a retired machine cannot receive a new WARP identity".to_owned(),
        ));
    }
    if sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(
             SELECT 1 FROM external_outbound_bindings
             WHERE outbound_id = $1 AND node_id = $2
         )",
    )
    .bind(&outbound_id)
    .bind(&node_id)
    .fetch_one(&mut *tx)
    .await?
    {
        return Err(StoreError::InvalidData(format!(
            "WARP tunnel {outbound_id} is already bound to {node_id}"
        )));
    }

    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let access_token_sealed = crate::secrets::seal(
        &crate::secrets::external_outbound_binding_token_context(
            &tenant_id,
            &outbound_id,
            &node_id,
        ),
        &access_token,
    )?;
    let private_key_sealed = crate::secrets::seal(
        &crate::secrets::external_outbound_binding_key_context(&tenant_id, &outbound_id, &node_id),
        &private_key,
    )?;
    let row = sqlx::query(
        "INSERT INTO external_outbound_bindings
            (outbound_id, node_id, device_id, account_id, access_token_sealed,
             private_key_sealed, peer_public_key, local_addresses, reserved, created_revision)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         RETURNING to_char(registered_at AT TIME ZONE 'UTC',
                           'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS registered_at",
    )
    .bind(&outbound_id)
    .bind(&node_id)
    .bind(&device_id)
    .bind(&account_id)
    .bind(access_token_sealed)
    .bind(private_key_sealed)
    .bind(&peer_public_key)
    .bind(serde_json::to_value(&request.local_addresses)?)
    .bind(serde_json::to_value(&request.reserved)?)
    .bind(u64_to_i64(revision_id, "revision_id")?)
    .fetch_one(&mut *tx)
    .await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, true).await?;
    tx.commit().await?;

    let binding = ExternalWarpBinding {
        node: node_id,
        device_id,
        account_id,
        registered_at: row.try_get("registered_at")?,
        endpoint_address: None,
        endpoint_port: None,
        mtu: None,
        keep_alive: None,
        allowed_ips: None,
        no_kernel_tun: None,
        domain_strategy: None,
        workers: None,
        private_key,
        peer_public_key,
        local_addresses: request.local_addresses,
        reserved: request.reserved,
    };
    Ok(RegisterWarpBindingResult {
        revision_id,
        binding: redacted_value(binding)?,
    })
}

/// Changes how one registered WARP identity reaches Cloudflare without rotating that identity.
///
/// The nullable fields are override state, not effective values. Keeping them nullable is what
/// lets a later edit of the logical tunnel default flow through to every machine which has not
/// deliberately pinned that field.
pub async fn update_warp_binding(
    pool: &PgPool,
    actor: &AdminContext,
    request: UpdateWarpBindingRequest,
) -> Result<UpdateWarpBindingResult> {
    let requested_tenant_id = required_text(request.tenant_id, "tenant id")?;
    let outbound_id = required_slug(request.outbound_id, "tunnel id")?;
    let node_id = required_slug(request.node_id, "node id")?;
    // Only JSON null means inheritance. Treating whitespace as null would make a malformed form
    // submission silently erase an intentional machine override.
    let endpoint_address = request
        .endpoint_address
        .map(|address| required_text(address, "WARP endpoint address"))
        .transpose()?;
    if endpoint_address
        .as_deref()
        .is_some_and(|address| address.chars().any(char::is_whitespace))
    {
        return Err(StoreError::InvalidData(
            "WARP endpoint address must not contain whitespace".to_owned(),
        ));
    }
    if let Some(port) = request.endpoint_port {
        ensure_nonzero_port(port, "WARP endpoint port")?;
    }
    if request.mtu.is_some_and(|mtu| !(576..=9000).contains(&mtu)) {
        return Err(StoreError::InvalidData(
            "WARP MTU must be between 576 and 9000".to_owned(),
        ));
    }
    if request.allowed_ips.is_some() != request.domain_strategy.is_some() {
        return Err(StoreError::InvalidData(
            "WARP address policy must override allowed IPs and domain strategy together".to_owned(),
        ));
    }
    if request.allowed_ips.as_ref().is_some_and(|networks| {
        networks.is_empty()
            || networks
                .iter()
                .any(|network| network.parse::<IpNet>().is_err())
    }) {
        return Err(StoreError::InvalidData(
            "WARP allowed IPs must contain valid CIDR networks".to_owned(),
        ));
    }
    if request.domain_strategy.as_deref().is_some_and(|strategy| {
        !matches!(
            strategy,
            "ForceIP" | "ForceIPv4" | "ForceIPv6" | "ForceIPv4v6" | "ForceIPv6v4"
        )
    }) {
        return Err(StoreError::InvalidData(
            "WARP domain strategy is not supported by Xray".to_owned(),
        ));
    }
    if request
        .workers
        .is_some_and(|workers| workers > EXTERNAL_WIREGUARD_MAX_WORKERS)
    {
        return Err(StoreError::InvalidData(format!(
            "WARP workers must be between 0 and {EXTERNAL_WIREGUARD_MAX_WORKERS}"
        )));
    }
    let allowed_ips_json = request
        .allowed_ips
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?;

    let note = note_or(request.note.as_deref(), || {
        format!("update WARP route {outbound_id} on {node_id}")
    });
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let row = sqlx::query(
        "SELECT external_outbounds.tenant_id, external_outbounds.protocol,
                external_outbound_bindings.device_id,
                external_outbound_bindings.account_id,
                external_outbound_bindings.peer_public_key,
                external_outbound_bindings.local_addresses,
                external_outbound_bindings.reserved,
                to_char(external_outbound_bindings.registered_at AT TIME ZONE 'UTC',
                        'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS registered_at
         FROM external_outbound_bindings
         JOIN external_outbounds ON external_outbounds.id = external_outbound_bindings.outbound_id
         WHERE external_outbound_bindings.outbound_id = $1
           AND external_outbound_bindings.node_id = $2
         FOR UPDATE OF external_outbound_bindings, external_outbounds",
    )
    .bind(&outbound_id)
    .bind(&node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        StoreError::NotFound(format!(
            "WARP tunnel {outbound_id} is not registered on {node_id}"
        ))
    })?;
    let tenant_id: String = row.try_get("tenant_id")?;
    if tenant_id != requested_tenant_id {
        return Err(StoreError::NotFound(format!(
            "WARP tunnel {requested_tenant_id}/{outbound_id} is not registered on {node_id}"
        )));
    }
    actor.require_tenant_access(&tenant_id, "WARP tunnel")?;
    if row.try_get::<String, _>("protocol")? != "warp" {
        return Err(StoreError::InvalidData(format!(
            "tunnel {outbound_id} is not a managed WARP tunnel"
        )));
    }

    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let changed = sqlx::query(
        "UPDATE external_outbound_bindings
         SET endpoint_address = $3, endpoint_port = $4, mtu = $5,
             keep_alive = $6, allowed_ips = $7, no_kernel_tun = $8,
             domain_strategy = $9, workers = $10
         WHERE outbound_id = $1 AND node_id = $2
           AND (endpoint_address IS DISTINCT FROM $3
             OR endpoint_port IS DISTINCT FROM $4
             OR mtu IS DISTINCT FROM $5
             OR keep_alive IS DISTINCT FROM $6
             OR allowed_ips IS DISTINCT FROM $7
             OR no_kernel_tun IS DISTINCT FROM $8
             OR domain_strategy IS DISTINCT FROM $9
             OR workers IS DISTINCT FROM $10)",
    )
    .bind(&outbound_id)
    .bind(&node_id)
    .bind(&endpoint_address)
    .bind(request.endpoint_port.map(i32::from))
    .bind(request.mtu.map(i32::from))
    .bind(request.keep_alive.map(i32::from))
    .bind(&allowed_ips_json)
    .bind(request.no_kernel_tun)
    .bind(&request.domain_strategy)
    .bind(request.workers.map(i32::from))
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    let revision_id = commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    Ok(UpdateWarpBindingResult {
        revision_id,
        binding: json!({
            "node": node_id,
            "device_id": row.try_get::<String, _>("device_id")?,
            "account_id": row.try_get::<String, _>("account_id")?,
            "registered_at": row.try_get::<String, _>("registered_at")?,
            "endpoint_address": endpoint_address,
            "endpoint_port": request.endpoint_port,
            "mtu": request.mtu,
            "keep_alive": request.keep_alive,
            "allowed_ips": request.allowed_ips,
            "no_kernel_tun": request.no_kernel_tun,
            "domain_strategy": request.domain_strategy,
            "workers": request.workers,
            "peer_public_key": row.try_get::<String, _>("peer_public_key")?,
            "local_addresses": row.try_get::<Value, _>("local_addresses")?,
            "reserved": row.try_get::<Value, _>("reserved")?,
        }),
    })
}

/// Reads the provider credential only after proving that retiring it cannot cut a live route.
///
/// A WARP identity has two consumers to consider. The current model may still refer to it, and a
/// just-edited model may no longer refer to it while the machine is still running the last
/// successfully published revision. Both must be clear before the irreversible provider call.
pub async fn prepare_warp_binding_removal(
    pool: &PgPool,
    actor: &AdminContext,
    requested_tenant_id: &str,
    requested_outbound_id: &str,
    requested_node_id: &str,
) -> Result<WarpBindingRemoval> {
    let requested_tenant_id = required_text(requested_tenant_id, "tenant id")?;
    let outbound_id = required_slug(requested_outbound_id.to_owned(), "tunnel id")?;
    let node_id = required_slug(requested_node_id.to_owned(), "node id")?;
    let row = sqlx::query(
        "SELECT external_outbounds.tenant_id, external_outbounds.protocol,
                external_outbound_bindings.device_id,
                external_outbound_bindings.access_token_sealed,
                nodes.name AS node_name,
                lifecycle.phase AS lifecycle_phase
         FROM external_outbound_bindings
         JOIN external_outbounds
           ON external_outbounds.id = external_outbound_bindings.outbound_id
         JOIN nodes ON nodes.id = external_outbound_bindings.node_id
         JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = nodes.id
         WHERE external_outbound_bindings.outbound_id = $1
           AND external_outbound_bindings.node_id = $2",
    )
    .bind(&outbound_id)
    .bind(&node_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| {
        StoreError::NotFound(format!(
            "WARP tunnel {requested_tenant_id}/{outbound_id} is not registered on {node_id}"
        ))
    })?;
    let tenant_id: String = row.try_get("tenant_id")?;
    if tenant_id != requested_tenant_id {
        return Err(StoreError::NotFound(format!(
            "WARP tunnel {requested_tenant_id}/{outbound_id} is not registered on {node_id}"
        )));
    }
    actor.require_tenant_access(&tenant_id, "WARP tunnel")?;
    if row.try_get::<String, _>("protocol")? != "warp" {
        return Err(StoreError::InvalidData(format!(
            "tunnel {outbound_id} is not a managed WARP tunnel"
        )));
    }
    let lifecycle_complete = matches!(
        row.try_get::<String, _>("lifecycle_phase")?.as_str(),
        "retired" | "abandoned"
    );

    let current_references = sqlx::query_scalar::<_, i64>(
        "SELECT count(*)
         FROM steps
         CROSS JOIN LATERAL jsonb_array_elements(steps.rules) AS item(rule)
         WHERE steps.node_id = $1
           AND COALESCE(item.rule->'action', item.rule->'a')->>'t' = 'proxy'
           AND COALESCE(item.rule->'action', item.rule->'a')->>'outbound' = $2",
    )
    .bind(&node_id)
    .bind(&outbound_id)
    .fetch_one(pool)
    .await?;
    let node_name: String = row.try_get("node_name")?;
    if current_references > 0 && !lifecycle_complete {
        return Err(StoreError::InvalidData(format!(
            "{node_name} 的 WARP 身份仍被当前规则引用；请先解除这台机器上的引用并完成发布，再注销身份"
        )));
    }

    // A successful deployment containing this target is the newest revision we know this
    // particular machine runs. A global latest deployment can belong to another tenant and says
    // nothing about this node, hence the target join. Deployment success is the aggregate
    // invariant; repeating its allowed target statuses here would create a second definition.
    let deployed_revision = sqlx::query_scalar::<_, i64>(
        "SELECT deployments.revision_id
         FROM deployments
         JOIN deployment_targets
           ON deployment_targets.deployment_id = deployments.id
         WHERE deployments.kind = 'config'
           AND deployments.status = 'succeeded'
           AND deployment_targets.node_id = $1
         ORDER BY deployments.finished_at DESC NULLS LAST, deployments.id DESC
         LIMIT 1",
    )
    .bind(&node_id)
    .fetch_optional(pool)
    .await?;
    if let Some(revision) = deployed_revision.filter(|_| !lifecycle_complete) {
        let revision = u64::try_from(revision).map_err(|_| {
            StoreError::InvalidData("published WARP revision is negative".to_owned())
        })?;
        let published = crate::materialize::load_snapshot(pool, Some(revision)).await?;
        if warp_binding_is_referenced(&published, &outbound_id, &node_id) {
            return Err(StoreError::InvalidData(format!(
                "{node_name} 最后成功发布的版本仍在使用这个 WARP 身份；请先发布解除引用后的配置，再注销身份"
            )));
        }
    }

    let device_id: String = row.try_get("device_id")?;
    let access_token = crate::secrets::open(
        &crate::secrets::external_outbound_binding_token_context(
            &tenant_id,
            &outbound_id,
            &node_id,
        ),
        &row.try_get::<String, _>("access_token_sealed")?,
    )?;
    Ok(WarpBindingRemoval {
        device_id,
        access_token,
    })
}

fn warp_binding_is_referenced(snapshot: &ModelSnapshot, outbound_id: &str, node_id: &str) -> bool {
    snapshot.apps.iter().any(|app| {
        app.steps.iter().any(|step| {
            step.node == node_id
                && step.rules.iter().any(|rule| {
                    matches!(
                        &rule.action,
                        Action::Proxy { outbound } if outbound == outbound_id
                    )
                })
        })
    })
}

/// Deletes only the exact local identity that the provider step prepared.
///
/// This deliberately does not repeat the reference preflight. Cloudflare has already retired the
/// credential when this begins; refusing to remove the now-dead local row would make a retry less
/// repairable, not safer. A changed device id is different: it means another registration won the
/// race and must never be removed by a stale provider response.
pub async fn remove_warp_binding(
    pool: &PgPool,
    actor: &AdminContext,
    request: RemoveWarpBindingRequest,
) -> Result<RemoveWarpBindingResult> {
    let requested_tenant_id = required_text(request.tenant_id, "tenant id")?;
    let outbound_id = required_slug(request.outbound_id, "tunnel id")?;
    let node_id = required_slug(request.node_id, "node id")?;
    let expected_device_id = required_text(request.expected_device_id, "WARP device id")?;
    let note = note_or(request.note.as_deref(), || {
        format!("unregister WARP tunnel {outbound_id} from {node_id}")
    });

    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let row = sqlx::query(
        "SELECT external_outbounds.tenant_id, external_outbounds.protocol,
                external_outbound_bindings.device_id
         FROM external_outbound_bindings
         JOIN external_outbounds
           ON external_outbounds.id = external_outbound_bindings.outbound_id
         WHERE external_outbound_bindings.outbound_id = $1
           AND external_outbound_bindings.node_id = $2
         FOR UPDATE OF external_outbound_bindings, external_outbounds",
    )
    .bind(&outbound_id)
    .bind(&node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        StoreError::NotFound(format!(
            "WARP tunnel {requested_tenant_id}/{outbound_id} is not registered on {node_id}"
        ))
    })?;
    let tenant_id: String = row.try_get("tenant_id")?;
    if tenant_id != requested_tenant_id {
        return Err(StoreError::NotFound(format!(
            "WARP tunnel {requested_tenant_id}/{outbound_id} is not registered on {node_id}"
        )));
    }
    actor.require_tenant_access(&tenant_id, "WARP tunnel")?;
    if row.try_get::<String, _>("protocol")? != "warp" {
        return Err(StoreError::InvalidData(format!(
            "tunnel {outbound_id} is not a managed WARP tunnel"
        )));
    }
    let device_id: String = row.try_get("device_id")?;
    if device_id != expected_device_id {
        return Err(StoreError::InvalidData(format!(
            "WARP identity on {node_id} changed while it was being removed; refresh and try again"
        )));
    }

    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let removed = sqlx::query(
        "DELETE FROM external_outbound_bindings
         WHERE outbound_id = $1 AND node_id = $2 AND device_id = $3",
    )
    .bind(&outbound_id)
    .bind(&node_id)
    .bind(&device_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    let revision_id = commit_revision(&mut tx, revision_id, previous, removed).await?;
    tx.commit().await?;

    Ok(RemoveWarpBindingResult {
        revision_id,
        node_id,
        device_id,
        removed,
    })
}

pub async fn upsert_chain(
    pool: &PgPool,
    actor: &AdminContext,
    app_id: &str,
    request: CreateChainRequest,
) -> Result<UpsertChainResult> {
    let note = note_or(request.note.as_deref(), || {
        format!("upsert chain {app_id}/{}", request.id)
    });
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let (chain, changed) = upsert_chain_tx(&mut tx, actor, revision_id, app_id, request).await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    Ok(UpsertChainResult { revision_id, chain })
}

pub(crate) async fn upsert_chain_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    app_id: &str,
    request: CreateChainRequest,
) -> Result<(Chain, bool)> {
    let app_id = required_text(app_id, "app_id")?;
    let id = required_slug(request.id, "chain id")?;
    let tenant_id = required_text(request.tenant_id, "tenant_id")?;
    let name = required_text(request.name, "chain name")?;
    let subscription_country = normalize_subscription_country(request.subscription_country)?;
    actor.require_tenant_access(&tenant_id, "chain")?;
    validate_model_id("chain", &id)?;
    ensure_app_exists_tx(tx, &app_id).await?;
    ensure_tenant_exists_tx(tx, &tenant_id).await?;
    let revision_id = u64_to_i64(revision_id, "revision_id")?;
    let existing = sqlx::query(
        "SELECT app_id, tenant_id, name, subscription_country, position
         FROM chains
         WHERE id = $1
         FOR UPDATE",
    )
    .bind(&id)
    .fetch_optional(&mut **tx)
    .await?;
    let existing_app = existing
        .as_ref()
        .map(|row| row.try_get::<String, _>("app_id"))
        .transpose()?;
    let position = match (&existing, existing_app.as_deref()) {
        (Some(row), Some(current_app)) if current_app == app_id => row.try_get("position")?,
        _ => new_chain_position_tx(tx, &app_id, &id).await?,
    };
    let head_changed = match existing {
        Some(row) => {
            let changed = row.try_get::<String, _>("app_id")? != app_id
                || row.try_get::<String, _>("tenant_id")? != tenant_id
                || row.try_get::<String, _>("name")? != name
                || row.try_get::<Option<String>, _>("subscription_country")?
                    != subscription_country;
            if changed {
                sqlx::query(
                    "UPDATE chains
                     SET app_id = $2,
                         tenant_id = $3,
                         name = $4,
                         subscription_country = $5,
                         position = $6,
                         created_revision = COALESCE(created_revision, $7)
                     WHERE id = $1",
                )
                .bind(&id)
                .bind(&app_id)
                .bind(&tenant_id)
                .bind(&name)
                .bind(&subscription_country)
                .bind(position)
                .bind(revision_id)
                .execute(&mut **tx)
                .await?;
            }
            changed
        }
        None => {
            sqlx::query(
                "INSERT INTO chains
                    (id, app_id, tenant_id, name, subscription_country, position, created_revision)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(&id)
            .bind(&app_id)
            .bind(&tenant_id)
            .bind(&name)
            .bind(&subscription_country)
            .bind(position)
            .bind(revision_id)
            .execute(&mut **tx)
            .await?;
            true
        }
    };
    Ok((
        Chain {
            id,
            tenant: tenant_id,
            name,
            subscription_country,
        },
        head_changed,
    ))
}

fn normalize_subscription_country(value: Option<String>) -> Result<Option<String>> {
    let Some(value) = value.map(|value| value.trim().to_ascii_uppercase()) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() != 2 || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(StoreError::InvalidData(
            "subscription country must be a two-letter ISO country code".to_owned(),
        ));
    }
    Ok(Some(value))
}

/// The create-only counterpart used by the chain wizard. See [`create_app_tx`].
pub(crate) async fn create_chain_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    app_id: &str,
    request: CreateChainRequest,
) -> Result<(Chain, bool)> {
    let id = required_slug(request.id.clone(), "chain id")?;
    let exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM chains WHERE id = $1)")
            .bind(&id)
            .fetch_one(&mut **tx)
            .await?;
    if exists {
        return Err(StoreError::Conflict(format!(
            "chain id {id} already exists"
        )));
    }
    upsert_chain_tx(tx, actor, revision_id, app_id, request).await
}

/// Select the slot for a new chain inside one app.
///
/// As with apps, ID order is the fallback only while the operator has not established a custom
/// sequence. A chain created after a custom reorder appends and cannot disturb that sequence.
async fn new_chain_position_tx(
    tx: &mut Transaction<'_, Postgres>,
    app_id: &str,
    id: &str,
) -> Result<i32> {
    let rows = sqlx::query(
        "SELECT id, position
         FROM chains
         WHERE app_id = $1
         ORDER BY position, id
         FOR UPDATE",
    )
    .bind(app_id)
    .fetch_all(&mut **tx)
    .await?;
    let positioned = rows
        .iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>("id")?,
                row.try_get::<i32, _>("position")?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let follows_id = positioned.windows(2).all(|pair| pair[0].0 < pair[1].0);
    let append_position = positioned
        .iter()
        .map(|(_, position)| *position)
        .max()
        .map(|position| {
            position.checked_add(1).ok_or_else(|| {
                StoreError::InvalidData(format!("chain position exceeds i32 range in app {app_id}"))
            })
        })
        .transpose()?
        .unwrap_or(0);
    let position = if follows_id {
        positioned
            .iter()
            .find(|(existing_id, _)| existing_id.as_str() > id)
            .map(|(_, position)| *position)
            .unwrap_or(append_position)
    } else {
        append_position
    };
    if position != append_position {
        sqlx::query(
            "UPDATE chains
             SET position = position + 1
             WHERE app_id = $1 AND position >= $2",
        )
        .bind(app_id)
        .bind(position)
        .execute(&mut **tx)
        .await?;
    }
    Ok(position)
}

/// Persist one app's complete final chain order while keeping every stable chain ID intact.
pub(crate) async fn reorder_chains_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    app_id: String,
    ids: Vec<String>,
) -> Result<bool> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can reorder chains".to_owned(),
        ));
    }
    let app_id = required_text(app_id, "app_id")?;
    ensure_app_exists_tx(tx, &app_id).await?;
    let rows = sqlx::query(
        "SELECT id
         FROM chains
         WHERE app_id = $1
         ORDER BY position, id
         FOR UPDATE",
    )
    .bind(&app_id)
    .fetch_all(&mut **tx)
    .await?;
    let current = rows
        .iter()
        .map(|row| row.try_get::<String, _>("id"))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let ids = complete_order(ids, &current, &format!("chain in app {app_id}"))?;
    if ids == current {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE chains AS chain
         SET position = requested.position
         FROM (
             SELECT id, (ordinality - 1)::integer AS position
             FROM unnest($2::text[]) WITH ORDINALITY AS ordered(id, ordinality)
         ) AS requested
         WHERE chain.app_id = $1 AND chain.id = requested.id",
    )
    .bind(&app_id)
    .bind(&ids)
    .execute(&mut **tx)
    .await?;
    Ok(true)
}

/// Normalize and verify a complete ordering document against rows locked by the caller.
fn complete_order(ids: Vec<String>, current: &[String], resource: &str) -> Result<Vec<String>> {
    if ids.len() > i32::MAX as usize {
        return Err(StoreError::InvalidData(format!(
            "{resource} order exceeds i32 range"
        )));
    }
    let ids = ids
        .into_iter()
        .map(|id| required_text(id, &format!("{resource} id")))
        .collect::<Result<Vec<_>>>()?;
    let requested = ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if requested.len() != ids.len() {
        return Err(StoreError::InvalidData(format!(
            "{resource} order contains duplicate ids"
        )));
    }
    let existing = current.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if requested != existing {
        let missing = existing.difference(&requested).copied().collect::<Vec<_>>();
        let unknown = requested.difference(&existing).copied().collect::<Vec<_>>();
        return Err(StoreError::InvalidData(format!(
            "stale {resource} order: missing [{}], unknown [{}]",
            missing.join(", "),
            unknown.join(", ")
        )));
    }
    Ok(ids)
}

pub async fn upsert_front(
    pool: &PgPool,
    actor: &AdminContext,
    app_id: &str,
    request: CreateFrontRequest,
) -> Result<UpsertFrontResult> {
    let note = note_or(request.note.as_deref(), || {
        format!("upsert front {app_id}/{}", request.id)
    });
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let (front, changed) = upsert_front_tx(&mut tx, actor, revision_id, app_id, request).await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    Ok(UpsertFrontResult { revision_id, front })
}

pub(crate) async fn upsert_front_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    app_id: &str,
    request: CreateFrontRequest,
) -> Result<(Front, bool)> {
    let app_id = required_text(app_id, "app_id")?;
    let id = required_slug(request.id, "front id")?;
    let tenant_id = required_text(request.tenant_id, "tenant_id")?;
    let name = required_text(request.name, "front name")?;
    actor.require_tenant_access(&tenant_id, "front")?;
    let via = normalize_id_list(request.via, "front via")?;
    let external_via = normalize_id_list(request.external_via, "front external via")?;
    let strategy = request.strategy.as_str();
    ensure_app_exists_tx(tx, &app_id).await?;
    ensure_tenant_exists_tx(tx, &tenant_id).await?;
    ensure_ingresses_in_app_tx(tx, &app_id, &via).await?;
    ensure_external_outbounds_for_tenant_tx(tx, &tenant_id, &external_via).await?;
    let head_changed = sqlx::query(
        "INSERT INTO fronts (id, app_id, tenant_id, name, strategy, created_revision)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (id) DO UPDATE SET
            app_id = EXCLUDED.app_id,
            tenant_id = EXCLUDED.tenant_id,
            name = EXCLUDED.name,
            strategy = EXCLUDED.strategy,
            created_revision = COALESCE(fronts.created_revision, EXCLUDED.created_revision)
         WHERE ROW(fronts.app_id, fronts.tenant_id, fronts.name, fronts.strategy)
            IS DISTINCT FROM
            ROW(EXCLUDED.app_id, EXCLUDED.tenant_id, EXCLUDED.name, EXCLUDED.strategy)",
    )
    .bind(&id)
    .bind(&app_id)
    .bind(&tenant_id)
    .bind(&name)
    .bind(strategy)
    .bind(u64_to_i64(revision_id, "revision_id")?)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    let via_changed = replace_front_via(tx, &id, &via).await?;
    let external_via_changed = replace_front_external_via(tx, &id, &external_via).await?;

    Ok((
        Front {
            id,
            tenant: tenant_id,
            name,
            via,
            external_via,
            strategy: request.strategy,
        },
        head_changed || via_changed || external_via_changed,
    ))
}

pub async fn upsert_ingress(
    pool: &PgPool,
    actor: &AdminContext,
    app_id: &str,
    request: CreateIngressRequest,
) -> Result<UpsertIngressResult> {
    let note = note_or(request.note.as_deref(), || {
        format!("upsert ingress {app_id}/{}", request.id)
    });
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let (ingress, changed) =
        upsert_ingress_tx(&mut tx, actor, revision_id, app_id, request).await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    Ok(UpsertIngressResult {
        revision_id,
        ingress: redacted_value(ingress)?,
    })
}

pub(crate) async fn upsert_ingress_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    app_id: &str,
    request: CreateIngressRequest,
) -> Result<(Ingress, bool)> {
    let app_id = required_text(app_id, "app_id")?;
    let id = required_slug(request.id, "ingress id")?;
    let chain_id = required_text(request.chain_id, "chain_id")?;
    let node_id = required_text(request.node_id, "node_id")?;
    ensure_nonzero_port(request.port, "port")?;
    let front_id = optional_owned_text(request.front_id);
    let mut reality = normalize_reality_request(request.reality)?;
    let projection = Projection {
        v4: normalized_projection(request.projection.v4, "projection v4")?,
        v6: normalized_projection(request.projection.v6, "projection v6")?,
    };
    // The site default must be read inside the transaction: during a batch commit an earlier
    // operation may have just changed the global REALITY site, reading from the pool would take
    // the old value, and the dest echoed back would disagree with what actually landed.
    let site = crate::settings::load_settings_tx(tx).await?.reality_site;
    let same_site = reality.dest.as_deref() == site.dest.as_deref()
        && reality.server_names == site.server_names
        && reality.fingerprint.as_deref().unwrap_or("chrome")
            == site.fingerprint.as_deref().unwrap_or("chrome");
    let fallback_mode = reality.fallback_mode.unwrap_or_else(|| {
        if (reality.dest.is_none() && reality.server_names.is_empty()) || same_site {
            RealityFallbackMode::GlobalSite
        } else {
            RealityFallbackMode::CustomSite
        }
    });
    match fallback_mode {
        RealityFallbackMode::GlobalSite | RealityFallbackMode::NodeCertificate => {
            reality.dest = None;
            reality.server_names.clear();
            reality.fingerprint = None;
        }
        RealityFallbackMode::CustomSite => {
            if reality.dest.is_none() || reality.server_names.is_empty() {
                return Err(StoreError::InvalidData(
                    "custom REALITY fallback requires dest and server_names".to_owned(),
                ));
            }
        }
    }
    let fallback_limits = reality
        .fallback_limits
        .clone()
        .unwrap_or(RealityFallbackLimits::Balanced);
    // Absent means on. Old callers submit the whole ingress on every edit, so treating absence as
    // off would turn any unrelated change made by one of them into silently removing the guard.
    let fallback_guard = reality.fallback_guard.unwrap_or(true);
    let (anytls_reality_json, anytls_reality_effective) =
        normalize_anytls_reality_request(request.wires.anytls.as_ref(), &site)?;
    ensure_app_exists_tx(tx, &app_id).await?;
    let chain_tenant = chain_tenant_tx(tx, &app_id, &chain_id).await?;
    actor.require_tenant_access(&chain_tenant, "ingress")?;
    validate_model_id("ingress", &id)?;
    validate_model_id_pair(&id, &chain_id)?;
    ensure_node_exists_tx(tx, &node_id).await?;
    if let Some(front_id) = &front_id {
        ensure_front_in_app_tx(tx, &app_id, front_id).await?;
    }
    ensure_existing_ingress_same_app(tx, &app_id, &id).await?;
    let keypair = generate_reality_keypair()?;
    let short_id = generate_reality_short_id()?;
    let server_names = serde_json::to_value(&reality.server_names)?;
    let fallback_limits_json = serde_json::to_value(&fallback_limits)?;
    let short_ids = serde_json::json!([short_id]);
    // The console owns only Padding. POST upload controls and client-only transport selectors may
    // still arrive from a stale browser or an old immutable snapshot, but a new managed write
    // must not persist them.
    let reality_split = matches!(
        request.wires.vless.as_ref(),
        Some(TransportRequest::VlessRealityXhttp { .. })
    );
    let xhttp = request
        .wires
        .xhttp()
        .map(|xhttp| managed_xhttp(xhttp, reality_split));
    let xhttp_xmux = xhttp
        .as_ref()
        .and_then(|xhttp| xhttp.xmux.as_ref())
        .map(serde_json::to_value)
        .transpose()?;
    let xhttp_tuning = xhttp
        .as_ref()
        .and_then(|xhttp| xhttp.tuning.as_ref())
        .map(serde_json::to_value)
        .transpose()?;
    let xhttp_download_v4 = xhttp
        .as_ref()
        .and_then(|xhttp| xhttp.download.as_ref())
        .and_then(|download| download.v4.as_ref())
        .map(client_download_json)
        .transpose()?;
    let xhttp_download_v6 = xhttp
        .as_ref()
        .and_then(|xhttp| xhttp.download.as_ref())
        .and_then(|download| download.v6.as_ref())
        .map(client_download_json)
        .transpose()?;
    let xhttp_download_v4_origin_port = reality_split
        .then(|| {
            xhttp
                .as_ref()
                .and_then(|xhttp| xhttp.download.as_ref())
                .and_then(|download| download.v4.as_ref())
                .and_then(|download| download.origin_port)
                .map(i32::from)
        })
        .flatten();
    let xhttp_download_v6_origin_port = reality_split
        .then(|| {
            xhttp
                .as_ref()
                .and_then(|xhttp| xhttp.download.as_ref())
                .and_then(|download| download.v6.as_ref())
                .and_then(|download| download.origin_port)
                .map(i32::from)
        })
        .flatten();
    let hysteria2 = request.wires.hysteria2.as_ref();
    /* 四个接收窗口在模型里是 u64（跟上游 `uint64` 同宽），列是 BIGINT。超出 i64 的值只会
    来自手写请求，宁可在这里拒绝也不要截断——截断之后剩下的那个数很可能还满足 CHECK，
    于是写进去的是一个谁都没要求过的窗口。 */
    let quic = hysteria2.map(|h| h.quic).unwrap_or_default();
    let window = |value: Option<u64>| -> Result<Option<i64>> {
        value
            .map(|value| u64_to_i64(value, "hysteria2 接收窗口"))
            .transpose()
    };
    let quic_init_stream_window = window(quic.init_stream_receive_window)?;
    let quic_max_stream_window = window(quic.max_stream_receive_window)?;
    let quic_init_conn_window = window(quic.init_connection_receive_window)?;
    let quic_max_conn_window = window(quic.max_connection_receive_window)?;
    let requested_hy2_obfs_password = hysteria2.and_then(|h| {
        h.obfs.as_ref().map(|obfs| match obfs {
            HysteriaObfs::Salamander { password } => password.clone(),
        })
    });
    let hy2_obfs_password = if requested_hy2_obfs_password.as_deref() == Some("<redacted>") {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT hy2_obfs_password FROM ingresses WHERE id = $1",
        )
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await?
        .flatten()
    } else {
        requested_hy2_obfs_password
    };
    let (hy2_masquerade_kind, hy2_masquerade_url) = match hysteria2.map(|h| &h.masquerade) {
        Some(HysteriaMasquerade::Proxy { url }) => ("proxy", Some(url.clone())),
        _ => ("not-found", None),
    };
    let anytls = request.wires.anytls.as_ref();
    let anytls_security = match anytls.map(|settings| settings.security) {
        Some(AnyTlsSecurity::Reality) => "reality",
        _ => "tls",
    };
    let anytls_padding_scheme = anytls
        .map(|settings| serde_json::to_value(&settings.padding_scheme))
        .transpose()?;
    let anytls_keypair = anytls.map(|_| generate_reality_keypair()).transpose()?;
    let anytls_short_ids = anytls
        .map(|_| generate_reality_short_id().map(|short_id| json!([short_id])))
        .transpose()?;
    let (
        anytls_masquerade_kind,
        anytls_masquerade_content,
        anytls_masquerade_headers,
        anytls_masquerade_status_code,
    ) = match anytls.map(|settings| &settings.masquerade) {
        Some(AnyTlsMasquerade::String {
            content,
            headers,
            status_code,
        }) => (
            "string",
            Some(content.clone()),
            Some(serde_json::to_value(headers)?),
            Some(i32::from(*status_code)),
        ),
        Some(AnyTlsMasquerade::NotFound { headers }) if !headers.is_empty() => {
            ("404", None, Some(serde_json::to_value(headers)?), None)
        }
        _ => ("404", None, None, None),
    };
    let row = sqlx::query(
        "INSERT INTO ingresses (
            id, app_id, chain_id, node_id, bind, port, front_id,
            reality_private_key, reality_public_key, reality_short_ids,
            reality_dest, reality_server_names, reality_flow,
            reality_fallback_mode, reality_fallback_limits,
            reality_fallback_guard,
            transport_kind, hy2_enabled, xhttp_path, xhttp_mode,
            hy2_port, hy2_hop_start, hy2_hop_end,
            hy2_up, hy2_down, hy2_congestion, hy2_obfs_password,
            hy2_masquerade_kind, hy2_masquerade_url,
            projection_v4_host, projection_v4_port,
            projection_v4_download_host, projection_v4_download_port,
            projection_v4_download_origin_port,
            projection_v4_download_http_host, projection_v4_download_mux,
            projection_v6_host, projection_v6_port,
            projection_v6_download_host, projection_v6_download_port,
            projection_v6_download_origin_port,
            projection_v6_download_http_host, projection_v6_download_mux,
            guard_no_private, guard_no_bittorrent, guard_no_mail,
            guard_no_udp_amplification, guard_tcp_and_quic_only,
            created_revision,
            hy2_bbr_profile,
            hy2_quic_init_stream_window, hy2_quic_max_stream_window,
            hy2_quic_init_conn_window, hy2_quic_max_conn_window,
            hy2_quic_max_idle_secs, hy2_quic_keepalive_secs,
            hy2_quic_max_incoming_streams, hy2_quic_disable_pmtud,
            xhttp_tuning,
            xhttp_download_v4_origin_port, xhttp_download_v6_origin_port,
            anytls_enabled, anytls_port, anytls_padding_scheme,
            anytls_masquerade_kind, anytls_masquerade_content,
            anytls_masquerade_headers, anytls_masquerade_status_code,
            anytls_security, anytls_reality,
            anytls_reality_private_key, anytls_reality_public_key,
            anytls_reality_short_ids
         ) VALUES (
            $1, $2, $3, $4, $5::inet, $6, $7,
            $8, $9, $10,
            $11, $12, $13,
            $14, $15,
            $16,
            $17, $18, $19, $20,
            $21, $22, $23,
            $24, $25, $26, $27, $28, $29,
            $30, $31, $32, $33, $34, $35, $36,
            $37, $38, $39, $40, $41, $42, $43,
            $44, $45, $46, $47, $48,
            $49,
            $50,
            $51, $52, $53, $54,
            $55, $56, $57, $58,
            $59, $60, $61,
            $62, $63, $64, $65, $66, $67, $68, $69, $70,
            $71, $72, $73
         )
         ON CONFLICT (id) DO UPDATE SET
            app_id = EXCLUDED.app_id,
            chain_id = EXCLUDED.chain_id,
            node_id = EXCLUDED.node_id,
            bind = EXCLUDED.bind,
            port = EXCLUDED.port,
            front_id = EXCLUDED.front_id,
            reality_dest = EXCLUDED.reality_dest,
            reality_server_names = EXCLUDED.reality_server_names,
            reality_flow = EXCLUDED.reality_flow,
            reality_fallback_mode = EXCLUDED.reality_fallback_mode,
            reality_fallback_limits = EXCLUDED.reality_fallback_limits,
            reality_fallback_guard = EXCLUDED.reality_fallback_guard,
            transport_kind = EXCLUDED.transport_kind,
            hy2_enabled = EXCLUDED.hy2_enabled,
            xhttp_path = EXCLUDED.xhttp_path,
            xhttp_mode = EXCLUDED.xhttp_mode,
            hy2_port = EXCLUDED.hy2_port,
            hy2_hop_start = EXCLUDED.hy2_hop_start,
            hy2_hop_end = EXCLUDED.hy2_hop_end,
            hy2_up = EXCLUDED.hy2_up,
            hy2_down = EXCLUDED.hy2_down,
            hy2_congestion = EXCLUDED.hy2_congestion,
            hy2_obfs_password = EXCLUDED.hy2_obfs_password,
            hy2_masquerade_kind = EXCLUDED.hy2_masquerade_kind,
            hy2_masquerade_url = EXCLUDED.hy2_masquerade_url,
            projection_v4_host = EXCLUDED.projection_v4_host,
            projection_v4_port = EXCLUDED.projection_v4_port,
            projection_v4_download_host = EXCLUDED.projection_v4_download_host,
            projection_v4_download_port = EXCLUDED.projection_v4_download_port,
            projection_v4_download_origin_port = EXCLUDED.projection_v4_download_origin_port,
            projection_v4_download_http_host = EXCLUDED.projection_v4_download_http_host,
            projection_v4_download_mux = EXCLUDED.projection_v4_download_mux,
            projection_v6_host = EXCLUDED.projection_v6_host,
            projection_v6_port = EXCLUDED.projection_v6_port,
            projection_v6_download_host = EXCLUDED.projection_v6_download_host,
            projection_v6_download_port = EXCLUDED.projection_v6_download_port,
            projection_v6_download_origin_port = EXCLUDED.projection_v6_download_origin_port,
            projection_v6_download_http_host = EXCLUDED.projection_v6_download_http_host,
            projection_v6_download_mux = EXCLUDED.projection_v6_download_mux,
            guard_no_private = EXCLUDED.guard_no_private,
            guard_no_bittorrent = EXCLUDED.guard_no_bittorrent,
            guard_no_mail = EXCLUDED.guard_no_mail,
            guard_no_udp_amplification = EXCLUDED.guard_no_udp_amplification,
            guard_tcp_and_quic_only = EXCLUDED.guard_tcp_and_quic_only,
            hy2_bbr_profile = EXCLUDED.hy2_bbr_profile,
            hy2_quic_init_stream_window = EXCLUDED.hy2_quic_init_stream_window,
            hy2_quic_max_stream_window = EXCLUDED.hy2_quic_max_stream_window,
            hy2_quic_init_conn_window = EXCLUDED.hy2_quic_init_conn_window,
            hy2_quic_max_conn_window = EXCLUDED.hy2_quic_max_conn_window,
            hy2_quic_max_idle_secs = EXCLUDED.hy2_quic_max_idle_secs,
            hy2_quic_keepalive_secs = EXCLUDED.hy2_quic_keepalive_secs,
            hy2_quic_max_incoming_streams = EXCLUDED.hy2_quic_max_incoming_streams,
            hy2_quic_disable_pmtud = EXCLUDED.hy2_quic_disable_pmtud,
            xhttp_tuning = EXCLUDED.xhttp_tuning,
            xhttp_download_v4_origin_port = EXCLUDED.xhttp_download_v4_origin_port,
            xhttp_download_v6_origin_port = EXCLUDED.xhttp_download_v6_origin_port,
            anytls_enabled = EXCLUDED.anytls_enabled,
            anytls_port = EXCLUDED.anytls_port,
            anytls_padding_scheme = EXCLUDED.anytls_padding_scheme,
            anytls_masquerade_kind = EXCLUDED.anytls_masquerade_kind,
            anytls_masquerade_content = EXCLUDED.anytls_masquerade_content,
            anytls_masquerade_headers = EXCLUDED.anytls_masquerade_headers,
            anytls_masquerade_status_code = EXCLUDED.anytls_masquerade_status_code,
            anytls_security = EXCLUDED.anytls_security,
            anytls_reality = EXCLUDED.anytls_reality,
            anytls_reality_private_key = COALESCE(
                ingresses.anytls_reality_private_key,
                EXCLUDED.anytls_reality_private_key
            ),
            anytls_reality_public_key = COALESCE(
                ingresses.anytls_reality_public_key,
                EXCLUDED.anytls_reality_public_key
            ),
            anytls_reality_short_ids = COALESCE(
                ingresses.anytls_reality_short_ids,
                EXCLUDED.anytls_reality_short_ids
            ),
            created_revision = COALESCE(ingresses.created_revision, EXCLUDED.created_revision)
         WHERE ROW(ingresses.app_id, ingresses.chain_id, ingresses.node_id, ingresses.bind,
                   ingresses.port, ingresses.front_id, ingresses.reality_dest,
                   ingresses.reality_server_names, ingresses.reality_flow,
                   ingresses.reality_fallback_mode,
                   ingresses.reality_fallback_limits, ingresses.reality_fallback_guard,
                   ingresses.transport_kind, ingresses.hy2_enabled, ingresses.xhttp_path,
                   ingresses.hy2_port, ingresses.hy2_hop_start, ingresses.hy2_hop_end,
                   ingresses.xhttp_mode, ingresses.hy2_up, ingresses.hy2_down,
                   ingresses.hy2_congestion, ingresses.hy2_obfs_password,
                   ingresses.hy2_masquerade_kind, ingresses.hy2_masquerade_url,
                   ingresses.projection_v4_host, ingresses.projection_v4_port,
                   ingresses.projection_v4_download_host, ingresses.projection_v4_download_port,
                   ingresses.projection_v4_download_origin_port,
                   ingresses.projection_v4_download_http_host, ingresses.projection_v4_download_mux,
                   ingresses.projection_v6_host, ingresses.projection_v6_port,
                   ingresses.projection_v6_download_host, ingresses.projection_v6_download_port,
                   ingresses.projection_v6_download_origin_port,
                   ingresses.projection_v6_download_http_host, ingresses.projection_v6_download_mux,
                   ingresses.guard_no_private, ingresses.guard_no_bittorrent,
                   ingresses.guard_no_mail, ingresses.guard_no_udp_amplification,
                   ingresses.guard_tcp_and_quic_only,
                   ingresses.hy2_bbr_profile,
                   ingresses.hy2_quic_init_stream_window, ingresses.hy2_quic_max_stream_window,
                   ingresses.hy2_quic_init_conn_window, ingresses.hy2_quic_max_conn_window,
                   ingresses.hy2_quic_max_idle_secs, ingresses.hy2_quic_keepalive_secs,
                   ingresses.hy2_quic_max_incoming_streams, ingresses.hy2_quic_disable_pmtud,
                   ingresses.xhttp_tuning,
                   ingresses.xhttp_download_v4_origin_port,
                   ingresses.xhttp_download_v6_origin_port,
                   ingresses.anytls_enabled, ingresses.anytls_port, ingresses.anytls_padding_scheme,
                   ingresses.anytls_masquerade_kind, ingresses.anytls_masquerade_content,
                   ingresses.anytls_masquerade_headers, ingresses.anytls_masquerade_status_code,
                   ingresses.anytls_security, ingresses.anytls_reality)
            IS DISTINCT FROM
            ROW(EXCLUDED.app_id, EXCLUDED.chain_id, EXCLUDED.node_id, EXCLUDED.bind,
                EXCLUDED.port, EXCLUDED.front_id, EXCLUDED.reality_dest,
                EXCLUDED.reality_server_names, EXCLUDED.reality_flow,
                EXCLUDED.reality_fallback_mode,
                EXCLUDED.reality_fallback_limits, EXCLUDED.reality_fallback_guard,
                EXCLUDED.transport_kind, EXCLUDED.hy2_enabled, EXCLUDED.xhttp_path,
                EXCLUDED.hy2_port, EXCLUDED.hy2_hop_start, EXCLUDED.hy2_hop_end,
                EXCLUDED.xhttp_mode, EXCLUDED.hy2_up, EXCLUDED.hy2_down,
                EXCLUDED.hy2_congestion, EXCLUDED.hy2_obfs_password,
                EXCLUDED.hy2_masquerade_kind, EXCLUDED.hy2_masquerade_url,
                EXCLUDED.projection_v4_host, EXCLUDED.projection_v4_port,
                EXCLUDED.projection_v4_download_host, EXCLUDED.projection_v4_download_port,
                EXCLUDED.projection_v4_download_origin_port,
                EXCLUDED.projection_v4_download_http_host, EXCLUDED.projection_v4_download_mux,
                EXCLUDED.projection_v6_host, EXCLUDED.projection_v6_port,
                EXCLUDED.projection_v6_download_host, EXCLUDED.projection_v6_download_port,
                EXCLUDED.projection_v6_download_origin_port,
                EXCLUDED.projection_v6_download_http_host, EXCLUDED.projection_v6_download_mux,
                EXCLUDED.guard_no_private, EXCLUDED.guard_no_bittorrent,
                EXCLUDED.guard_no_mail, EXCLUDED.guard_no_udp_amplification,
                EXCLUDED.guard_tcp_and_quic_only,
                EXCLUDED.hy2_bbr_profile,
                EXCLUDED.hy2_quic_init_stream_window, EXCLUDED.hy2_quic_max_stream_window,
                EXCLUDED.hy2_quic_init_conn_window, EXCLUDED.hy2_quic_max_conn_window,
                EXCLUDED.hy2_quic_max_idle_secs, EXCLUDED.hy2_quic_keepalive_secs,
                EXCLUDED.hy2_quic_max_incoming_streams, EXCLUDED.hy2_quic_disable_pmtud,
                EXCLUDED.xhttp_tuning,
                EXCLUDED.xhttp_download_v4_origin_port,
                EXCLUDED.xhttp_download_v6_origin_port,
                EXCLUDED.anytls_enabled, EXCLUDED.anytls_port, EXCLUDED.anytls_padding_scheme,
                EXCLUDED.anytls_masquerade_kind, EXCLUDED.anytls_masquerade_content,
                EXCLUDED.anytls_masquerade_headers, EXCLUDED.anytls_masquerade_status_code,
                EXCLUDED.anytls_security, EXCLUDED.anytls_reality)
            OR (EXCLUDED.anytls_enabled AND ingresses.anytls_reality_private_key IS NULL)
         RETURNING reality_private_key,
                   reality_public_key,
                   reality_short_ids,
                   anytls_reality_private_key,
                   anytls_reality_public_key,
                   anytls_reality_short_ids",
    )
    .bind(&id)
    .bind(&app_id)
    .bind(&chain_id)
    .bind(&node_id)
    .bind(request.bind.to_string())
    .bind(i32::from(request.port))
    .bind(&front_id)
    .bind(&keypair.private_key)
    .bind(&keypair.public_key)
    .bind(short_ids)
    .bind(&reality.dest)
    .bind(server_names)
    .bind(&reality.flow)
    .bind(match fallback_mode {
        RealityFallbackMode::GlobalSite => "global-site",
        RealityFallbackMode::NodeCertificate => "node-certificate",
        RealityFallbackMode::CustomSite => "custom-site",
    })
    .bind(fallback_limits_json)
    .bind(fallback_guard)
    .bind(request.wires.vless_kind())
    .bind(request.wires.hysteria2.is_some())
    .bind(xhttp.as_ref().map(|xhttp| xhttp.path.clone()))
    .bind(xhttp.as_ref().and_then(|xhttp| xhttp.mode.as_str()))
    .bind(hysteria2.map(|h| i32::from(h.port)))
    .bind(hysteria2.and_then(|h| h.hop.map(|hop| i32::from(hop.start))))
    .bind(hysteria2.and_then(|h| h.hop.map(|hop| i32::from(hop.end))))
    .bind(hysteria2.and_then(|h| h.bandwidth.up.clone()))
    .bind(hysteria2.and_then(|h| h.bandwidth.down.clone()))
    .bind(
        hysteria2
            .map(|h| h.congestion)
            .unwrap_or(HysteriaCongestion::Brutal)
            .as_str(),
    )
    .bind(hy2_obfs_password)
    .bind(hy2_masquerade_kind)
    .bind(hy2_masquerade_url)
    .bind(projection.v4.as_ref().map(|to| to.host.clone()))
    .bind(projection.v4.as_ref().map(|to| i32::from(to.port)))
    .bind(
        projection
            .v4
            .as_ref()
            .and_then(|to| to.download.as_ref())
            .map(|to| to.host.clone()),
    )
    .bind(
        projection
            .v4
            .as_ref()
            .and_then(|to| to.download.as_ref())
            .map(|to| i32::from(to.port)),
    )
    .bind(
        projection
            .v4
            .as_ref()
            .and_then(|to| to.download.as_ref())
            .and_then(|to| to.origin_port.map(i32::from)),
    )
    .bind(
        projection
            .v4
            .as_ref()
            .and_then(|to| to.download.as_ref())
            .and_then(|to| to.http_host.clone()),
    )
    .bind(
        projection
            .v4
            .as_ref()
            .and_then(|to| to.download.as_ref())
            .and_then(|to| to.mux.map(i32::from)),
    )
    .bind(projection.v6.as_ref().map(|to| to.host.clone()))
    .bind(projection.v6.as_ref().map(|to| i32::from(to.port)))
    .bind(
        projection
            .v6
            .as_ref()
            .and_then(|to| to.download.as_ref())
            .map(|to| to.host.clone()),
    )
    .bind(
        projection
            .v6
            .as_ref()
            .and_then(|to| to.download.as_ref())
            .map(|to| i32::from(to.port)),
    )
    .bind(
        projection
            .v6
            .as_ref()
            .and_then(|to| to.download.as_ref())
            .and_then(|to| to.origin_port.map(i32::from)),
    )
    .bind(
        projection
            .v6
            .as_ref()
            .and_then(|to| to.download.as_ref())
            .and_then(|to| to.http_host.clone()),
    )
    .bind(
        projection
            .v6
            .as_ref()
            .and_then(|to| to.download.as_ref())
            .and_then(|to| to.mux.map(i32::from)),
    )
    .bind(request.guard.no_private)
    .bind(request.guard.no_bittorrent)
    .bind(request.guard.no_mail)
    .bind(request.guard.no_udp_amplification)
    .bind(request.guard.tcp_and_quic_only)
    .bind(u64_to_i64(revision_id, "revision_id")?)
    /* 九个 QUIC 调优项排在 created_revision 之后，是为了不动前面 53 个占位符的编号。
    这条 INSERT 的参数是位置式的，插在中间会把后面每一个绑定挪到邻居身上——不报错、
    不失败，只是这个接入面读回来带着别人的值（见 tests/pg_reality_guard.rs 的模块头）。 */
    .bind(
        hysteria2
            .map(|h| h.bbr_profile)
            .unwrap_or_default()
            .as_str(),
    )
    .bind(quic_init_stream_window)
    .bind(quic_max_stream_window)
    .bind(quic_init_conn_window)
    .bind(quic_max_conn_window)
    .bind(quic.max_idle_timeout_secs.map(i64::from))
    .bind(quic.keep_alive_period_secs.map(i64::from))
    .bind(quic.max_incoming_streams.map(i64::from))
    .bind(quic.disable_path_mtu_discovery)
    .bind(xhttp_tuning)
    .bind(xhttp_download_v4_origin_port)
    .bind(xhttp_download_v6_origin_port)
    .bind(anytls.is_some())
    .bind(anytls.map(|settings| i32::from(settings.port)))
    .bind(anytls_padding_scheme.unwrap_or_else(|| json!([])))
    .bind(anytls_masquerade_kind)
    .bind(anytls_masquerade_content.unwrap_or_default())
    .bind(anytls_masquerade_headers.unwrap_or_else(|| json!({})))
    .bind(anytls_masquerade_status_code.unwrap_or(200))
    .bind(anytls_security)
    .bind(anytls_reality_json)
    .bind(anytls_keypair.as_ref().map(|keypair| &keypair.private_key))
    .bind(anytls_keypair.as_ref().map(|keypair| &keypair.public_key))
    .bind(anytls_short_ids)
    .fetch_optional(&mut **tx)
    .await?;
    let client_changed = sqlx::query(
        "INSERT INTO ingress_client_settings (
             ingress_id, reality_fingerprint, xhttp_host, xhttp_xmux,
             xhttp_download_v4, xhttp_download_v6,
             anytls_idle_session_check_interval, anytls_idle_session_timeout,
             anytls_min_idle_session
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (ingress_id) DO UPDATE SET
             reality_fingerprint = EXCLUDED.reality_fingerprint,
             xhttp_host = EXCLUDED.xhttp_host,
             xhttp_xmux = EXCLUDED.xhttp_xmux,
             xhttp_download_v4 = EXCLUDED.xhttp_download_v4,
             xhttp_download_v6 = EXCLUDED.xhttp_download_v6,
             anytls_idle_session_check_interval = EXCLUDED.anytls_idle_session_check_interval,
             anytls_idle_session_timeout = EXCLUDED.anytls_idle_session_timeout,
             anytls_min_idle_session = EXCLUDED.anytls_min_idle_session,
             updated_at = now()
         WHERE ROW(
                 ingress_client_settings.reality_fingerprint,
                 ingress_client_settings.xhttp_host,
                 ingress_client_settings.xhttp_xmux,
                 ingress_client_settings.xhttp_download_v4,
                 ingress_client_settings.xhttp_download_v6,
                 ingress_client_settings.anytls_idle_session_check_interval,
                 ingress_client_settings.anytls_idle_session_timeout,
                 ingress_client_settings.anytls_min_idle_session
               ) IS DISTINCT FROM ROW(
                 EXCLUDED.reality_fingerprint,
                 EXCLUDED.xhttp_host,
                 EXCLUDED.xhttp_xmux,
                 EXCLUDED.xhttp_download_v4,
                 EXCLUDED.xhttp_download_v6,
                 EXCLUDED.anytls_idle_session_check_interval,
                 EXCLUDED.anytls_idle_session_timeout,
                 EXCLUDED.anytls_min_idle_session
               )",
    )
    .bind(&id)
    .bind(&reality.fingerprint)
    .bind(xhttp.as_ref().and_then(|xhttp| xhttp.host.clone()))
    .bind(xhttp_xmux)
    .bind(xhttp_download_v4)
    .bind(xhttp_download_v6)
    .bind(
        anytls
            .and_then(|settings| settings.idle_session_check_interval_secs)
            .map(i64::from),
    )
    .bind(
        anytls
            .and_then(|settings| settings.idle_session_timeout_secs)
            .map(i64::from),
    )
    .bind(
        anytls
            .and_then(|settings| settings.min_idle_session)
            .map(i64::from),
    )
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    // With the WHERE blocking it, RETURNING yields no row at all, and that is what "nothing
    // changed" means. The keys still have to be echoed, so the existing row is read as usual —
    // note that it reads the database's values rather than the keypair generated above: that pair
    // only lands on creation, and an existing ingress keeps its own (those three columns are
    // absent from DO UPDATE's SET list to begin with).
    let changed = row.is_some() || client_changed;
    let row = match row {
        Some(row) => row,
        None => {
            sqlx::query(
                "SELECT reality_private_key, reality_public_key, reality_short_ids,
                        anytls_reality_private_key, anytls_reality_public_key,
                        anytls_reality_short_ids
             FROM ingresses WHERE id = $1",
            )
            .bind(&id)
            .fetch_one(&mut **tx)
            .await?
        }
    };

    let identity = IngressIdentity {
        private_key: row.try_get("reality_private_key")?,
        public_key: row.try_get("reality_public_key")?,
        short_ids: json_string_array(&row.try_get::<Value, _>("reality_short_ids")?)?,
    };
    let anytls_identity = match (
        row.try_get::<Option<String>, _>("anytls_reality_private_key")?,
        row.try_get::<Option<String>, _>("anytls_reality_public_key")?,
        row.try_get::<Option<Value>, _>("anytls_reality_short_ids")?,
    ) {
        (None, None, None) => None,
        (Some(private_key), Some(public_key), Some(short_ids)) => Some(IngressIdentity {
            private_key,
            public_key,
            short_ids: json_string_array(&short_ids)?,
        }),
        _ => {
            return Err(StoreError::InvalidData(
                "AnyTLS REALITY identity is only partially populated".to_owned(),
            ))
        }
    };
    let effective = RealitySettings {
        // The response echoes the effective value: an unwritten site is filled in from
        // the global settings, so that a caller receiving an empty dest does not conclude
        // something is wrong
        dest: reality
            .dest
            .clone()
            .or_else(|| site.dest.clone())
            .unwrap_or_default(),
        server_names: if reality.server_names.is_empty() {
            site.server_names.clone()
        } else {
            reality.server_names.clone()
        },
        fingerprint: reality
            .fingerprint
            .clone()
            .or_else(|| site.fingerprint.clone())
            .unwrap_or_else(|| "chrome".to_owned()),
        // flow likewise echoes its effective value, except that it has no fallback
        // default: blank on both sides means shipping no flow
        flow: reality.flow.clone().or_else(|| site.flow.clone()),
        fallback_mode,
        fallback_limits,
        fallback_guard,
    };
    let mut response_anytls = request.wires.anytls.clone();
    if let Some(anytls) = &mut response_anytls {
        anytls.reality = anytls_reality_effective;
    }
    let ingress = Ingress {
        id,
        chain: chain_id,
        node: node_id,
        bind: request.bind,
        port: request.port,
        front: front_id,
        identity,
        anytls_identity,
        projection,
        guard: request.guard,
        wires: IngressWires::try_from(IngressWiresWire {
            vless: request.wires.vless.as_ref().map(|vless| match vless {
                TransportRequest::VlessReality => Transport::VlessReality(effective.clone()),
                TransportRequest::VlessRealityXhttp { .. } => {
                    Transport::VlessRealityXhttp(RealityXhttp {
                        reality: effective.clone(),
                        xhttp: xhttp
                            .clone()
                            .expect("XHTTP request has normalized settings"),
                    })
                }
                // The echo mirrors what a read of this ingress will return, and a read of a shape
                // holding a certificate returns no borrowed site — so neither does this.
                TransportRequest::VlessTls => Transport::VlessTls(tls_echo(&effective)),
                TransportRequest::VlessTlsXhttp { .. } => Transport::VlessTlsXhttp(TlsXhttp {
                    tls: tls_echo(&effective),
                    xhttp: xhttp
                        .clone()
                        .expect("XHTTP request has normalized settings"),
                }),
            }),
            anytls: response_anytls,
            hysteria2: request.wires.hysteria2.clone(),
        })
        .map_err(|error| StoreError::InvalidData(error.to_owned()))?,
    };
    Ok((ingress, changed))
}

/// The create-only counterpart used by the chain wizard. An ingress ID is embedded in user
/// counter labels, so colliding with one is data corruption rather than an ordinary edit.
pub(crate) async fn create_ingress_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    app_id: &str,
    request: CreateIngressRequest,
) -> Result<(Ingress, bool)> {
    let id = required_slug(request.id.clone(), "ingress id")?;
    let exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM ingresses WHERE id = $1)")
            .bind(&id)
            .fetch_one(&mut **tx)
            .await?;
    if exists {
        return Err(StoreError::Conflict(format!(
            "ingress id {id} already exists"
        )));
    }
    upsert_ingress_tx(tx, actor, revision_id, app_id, request).await
}

/// The certificate-holding shapes retain only their flow-control policy. ClientHello selection is
/// deliberately left to each client implementation's default.
fn tls_echo(effective: &RealitySettings) -> Tls {
    Tls {
        flow: effective.flow.clone(),
    }
}

fn managed_xhttp(value: &Xhttp, reality_split: bool) -> Xhttp {
    let mut value = value.clone();
    value.tuning = value.tuning.as_ref().and_then(|tuning| {
        tuning
            .x_padding_bytes
            .clone()
            .map(|x_padding_bytes| XhttpTuning {
                x_padding_bytes: Some(x_padding_bytes),
            })
    });
    if !reality_split {
        if let Some(download) = &mut value.download {
            if let Some(v4) = &mut download.v4 {
                v4.origin_port = None;
            }
            if let Some(v6) = &mut download.v6 {
                v6.origin_port = None;
            }
        }
    }
    value
}

fn client_download_json(download: &ProjectionDownloadEndpoint) -> Result<Value> {
    Ok(serde_json::to_value(ClientProjectionDownloadEndpoint {
        host: download.host.clone(),
        port: download.port,
        http_host: download.http_host.clone(),
        mux: download.mux,
    })?)
}

fn is_lower_hex4(value: &str) -> bool {
    value.len() == 4
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn ingress_model_id_token(id: &str) -> Option<&str> {
    let token = id.strip_prefix("ing-")?;
    is_lower_hex4(token).then_some(token)
}

fn app_model_id_token(id: &str) -> Option<&str> {
    let token = id.strip_prefix("app-")?;
    is_lower_hex4(token).then_some(token)
}

fn chain_model_id_parts(id: &str) -> Option<(&str, &str)> {
    let (ingress_token, chain_token) = id.strip_prefix("chn-")?.split_once('-')?;
    (is_lower_hex4(ingress_token) && is_lower_hex4(chain_token))
        .then_some((ingress_token, chain_token))
}

fn validate_model_id(kind: &str, id: &str) -> Result<()> {
    let valid = match kind {
        "app" => app_model_id_token(id).is_some(),
        "chain" => chain_model_id_parts(id).is_some(),
        "ingress" => ingress_model_id_token(id).is_some(),
        _ => {
            return Err(StoreError::InvalidData(format!(
                "unknown model id kind {kind}"
            )));
        }
    };
    if valid {
        return Ok(());
    }
    let shape = match kind {
        "app" => "app-<4 lowercase hex>",
        "chain" => "chn-<4 lowercase hex>-<4 lowercase hex>",
        "ingress" => "ing-<4 lowercase hex>",
        _ => unreachable!(),
    };
    Err(StoreError::InvalidData(format!(
        "{kind} id must use {shape}"
    )))
}

async fn random_app_model_id_tx(tx: &mut Transaction<'_, Postgres>) -> Result<String> {
    for _ in 0..256 {
        let mut random = [0_u8; 2];
        getrandom::fill(&mut random)?;
        let id = format!("app-{:02x}{:02x}", random[0], random[1]);
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM apps WHERE id = $1)")
            .bind(&id)
            .fetch_one(&mut **tx)
            .await?;
        if !exists {
            return Ok(id);
        }
    }
    Err(StoreError::Conflict(
        "could not allocate a unique app id after 256 attempts".to_owned(),
    ))
}

fn validate_model_id_pair(ingress_id: &str, chain_id: &str) -> Result<()> {
    let ingress_token = ingress_model_id_token(ingress_id).ok_or_else(|| {
        StoreError::InvalidData("ingress id must use ing-<4 lowercase hex>".to_owned())
    })?;
    let (chain_ingress_token, _) = chain_model_id_parts(chain_id).ok_or_else(|| {
        StoreError::InvalidData(
            "chain id must use chn-<4 lowercase hex>-<4 lowercase hex>".to_owned(),
        )
    })?;
    if ingress_token != chain_ingress_token {
        return Err(StoreError::InvalidData(format!(
            "ingress {ingress_id} and chain {chain_id} must share the same four-character token"
        )));
    }
    Ok(())
}

fn node_agent_state_sql(scoped: bool) -> &'static str {
    if scoped {
        "SELECT n.id AS node_id,
                n.tenant_id,
                n.name,
                n.public_ipv4,
                n.public_ipv6,
                n.public_ipv4_nat,
                n.public_ipv6_nat,
                n.mtu,
                n.overlay,
                n.egress_allowed,
                n.dns_kind,
                n.dns_servers,
                n.domain_strategy,
                n.conn_idle_secs,
                n.conn_uplink_only_secs,
                n.conn_downlink_only_secs,
                n.conn_buffer_size_kb,
                n.retired_at::text AS retired_at,
                COALESCE(l.phase, CASE WHEN n.retired_at IS NULL THEN 'active' ELSE 'retiring' END) AS lifecycle_phase,
                COALESCE(l.lifecycle_epoch, 0) AS lifecycle_epoch,
                l.deployment_id AS lifecycle_deployment_id,
                l.completed_at::text AS lifecycle_completed_at,
                l.last_error AS lifecycle_last_error,
                oi.isolated_at::text AS isolated_at,
                oi.node_id IS NOT NULL AS operationally_isolated,
                COALESCE(debt.debt_count, 0) AS convergence_debt_count,
                COALESCE(debt.debt_failed, FALSE) AS convergence_debt_failed,
                COALESCE(s.last_poll_at >= now() - interval '90 seconds', FALSE) AS reentry_poll_fresh,
                COALESCE(s.runtime_reported_at >= now() - interval '2 minutes', FALSE) AS reentry_runtime_fresh,
                n.wg_transport,
                s.route_ipv4,
                s.route_ipv6,
                s.token_prefix,
                s.token_created_at::text AS token_created_at,
                s.token_last_used_at::text AS token_last_used_at,
                s.token_revoked_at::text AS token_revoked_at,
                s.agent_version,
                s.agent_protocol_version,
                s.runtime_versions,
                s.spool_backlog,
                s.last_local_reconcile,
                s.wireguard_health,
                s.runtime_reported_at::text AS runtime_reported_at,
                s.geodata_observed,
                s.last_poll_at::text AS last_poll_at,
                s.last_usage_report_at::text AS last_usage_report_at,
                s.usage_last_result,
                s.usage_generation_id,
                s.xray_started_at::text AS xray_started_at,
                a.phantun_state,
                a.phantun_sha256,
                a.wireguard_state,
                a.wireguard_sha256,
                a.xray_state,
                a.xray_sha256,
                a.hy2_port_hop_state,
                a.hy2_port_hop_sha256,
                a.grants_state,
                a.source_deployment_id,
                a.observed_at::text AS observed_at
         FROM nodes n
         LEFT JOIN node_agent_state s ON s.node_id = n.id
         LEFT JOIN node_applied_state a ON a.node_id = n.id
         LEFT JOIN node_lifecycle_state l ON l.node_id = n.id
         LEFT JOIN node_operational_isolations oi ON oi.node_id = n.id
         LEFT JOIN LATERAL (
             SELECT count(*) FILTER (
                        WHERE o.status IN ('pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty')
                    ) AS debt_count,
                    bool_or(o.status IN ('failed-recovered', 'failed-dirty')) FILTER (
                        WHERE o.status IN ('pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty')
                    ) AS debt_failed
               FROM node_convergence_obligations o
              WHERE o.node_id = n.id
                AND o.lifecycle_epoch = COALESCE(l.lifecycle_epoch, 0)
         ) debt ON TRUE
         WHERE n.tenant_id = $1 OR n.tenant_id LIKE $2 ESCAPE '\\'
         ORDER BY n.id"
    } else {
        "SELECT n.id AS node_id,
                n.tenant_id,
                n.name,
                n.public_ipv4,
                n.public_ipv6,
                n.public_ipv4_nat,
                n.public_ipv6_nat,
                n.mtu,
                n.overlay,
                n.egress_allowed,
                n.dns_kind,
                n.dns_servers,
                n.domain_strategy,
                n.conn_idle_secs,
                n.conn_uplink_only_secs,
                n.conn_downlink_only_secs,
                n.conn_buffer_size_kb,
                n.retired_at::text AS retired_at,
                COALESCE(l.phase, CASE WHEN n.retired_at IS NULL THEN 'active' ELSE 'retiring' END) AS lifecycle_phase,
                COALESCE(l.lifecycle_epoch, 0) AS lifecycle_epoch,
                l.deployment_id AS lifecycle_deployment_id,
                l.completed_at::text AS lifecycle_completed_at,
                l.last_error AS lifecycle_last_error,
                oi.isolated_at::text AS isolated_at,
                oi.node_id IS NOT NULL AS operationally_isolated,
                COALESCE(debt.debt_count, 0) AS convergence_debt_count,
                COALESCE(debt.debt_failed, FALSE) AS convergence_debt_failed,
                COALESCE(s.last_poll_at >= now() - interval '90 seconds', FALSE) AS reentry_poll_fresh,
                COALESCE(s.runtime_reported_at >= now() - interval '2 minutes', FALSE) AS reentry_runtime_fresh,
                n.wg_transport,
                s.route_ipv4,
                s.route_ipv6,
                s.token_prefix,
                s.token_created_at::text AS token_created_at,
                s.token_last_used_at::text AS token_last_used_at,
                s.token_revoked_at::text AS token_revoked_at,
                s.agent_version,
                s.agent_protocol_version,
                s.runtime_versions,
                s.spool_backlog,
                s.last_local_reconcile,
                s.wireguard_health,
                s.runtime_reported_at::text AS runtime_reported_at,
                s.geodata_observed,
                s.last_poll_at::text AS last_poll_at,
                s.last_usage_report_at::text AS last_usage_report_at,
                s.usage_last_result,
                s.usage_generation_id,
                s.xray_started_at::text AS xray_started_at,
                a.phantun_state,
                a.phantun_sha256,
                a.wireguard_state,
                a.wireguard_sha256,
                a.xray_state,
                a.xray_sha256,
                a.hy2_port_hop_state,
                a.hy2_port_hop_sha256,
                a.grants_state,
                a.source_deployment_id,
                a.observed_at::text AS observed_at
         FROM nodes n
         LEFT JOIN node_agent_state s ON s.node_id = n.id
         LEFT JOIN node_applied_state a ON a.node_id = n.id
         LEFT JOIN node_lifecycle_state l ON l.node_id = n.id
         LEFT JOIN node_operational_isolations oi ON oi.node_id = n.id
         LEFT JOIN LATERAL (
             SELECT count(*) FILTER (
                        WHERE o.status IN ('pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty')
                    ) AS debt_count,
                    bool_or(o.status IN ('failed-recovered', 'failed-dirty')) FILTER (
                        WHERE o.status IN ('pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty')
                    ) AS debt_failed
               FROM node_convergence_obligations o
              WHERE o.node_id = n.id
                AND o.lifecycle_epoch = COALESCE(l.lifecycle_epoch, 0)
         ) debt ON TRUE
         ORDER BY n.id"
    }
}

/// One of the four nullable per-node policy columns. NULL stays NULL all the way to the
/// console: it is what tells the detail view to show the global default rather than a
/// number this machine chose.
fn node_conn_column(row: &sqlx::postgres::PgRow, column: &str) -> Result<Option<u32>> {
    row.try_get::<Option<i32>, _>(column)?
        .map(|value| {
            u32::try_from(value)
                .map_err(|_| StoreError::InvalidData(format!("nodes.{column} 是负数")))
        })
        .transpose()
}

fn node_agent_state_from_row(row: &sqlx::postgres::PgRow) -> Result<NodeAgentStateItem> {
    let applied = row
        .try_get::<Option<String>, _>("wireguard_state")?
        .map(|wireguard_state| {
            json!({
                "phantun": {
                    "state": row.try_get::<Option<String>, _>("phantun_state").ok().flatten(),
                    "sha256": row.try_get::<Option<String>, _>("phantun_sha256").ok().flatten()
                },
                "wireguard": {
                    "state": wireguard_state,
                    "sha256": row.try_get::<Option<String>, _>("wireguard_sha256").ok().flatten()
                },
                "xray": {
                    "state": row.try_get::<Option<String>, _>("xray_state").ok().flatten(),
                    "sha256": row.try_get::<Option<String>, _>("xray_sha256").ok().flatten()
                },
                "hy2_port_hop": {
                    "state": row.try_get::<Option<String>, _>("hy2_port_hop_state").ok().flatten(),
                    "sha256": row.try_get::<Option<String>, _>("hy2_port_hop_sha256").ok().flatten()
                },
                "grants": {
                    "state": row.try_get::<Option<String>, _>("grants_state").ok().flatten()
                },
                "source_deployment_id": row.try_get::<Option<i64>, _>("source_deployment_id").ok().flatten(),
                "observed_at": row.try_get::<Option<String>, _>("observed_at").ok().flatten()
            })
        });

    let operationally_isolated: bool = row.try_get("operationally_isolated")?;
    let convergence_debt_count = u64::try_from(row.try_get::<i64, _>("convergence_debt_count")?)
        .map_err(|_| {
            StoreError::InvalidData("node convergence debt count is negative".to_owned())
        })?;
    let lifecycle_phase: String = row.try_get("lifecycle_phase")?;
    let mut service_reentry_blockers = Vec::new();
    if operationally_isolated {
        if lifecycle_phase != "active" {
            service_reentry_blockers.push("节点生命周期不是 active".to_owned());
        }
        if convergence_debt_count > 0 {
            service_reentry_blockers.push(format!("仍有 {convergence_debt_count} 项收敛债务"));
        }
        if !row.try_get::<bool, _>("reentry_poll_fresh")? {
            service_reentry_blockers.push("最近 90 秒没有领取期望状态".to_owned());
        }
        if !row.try_get::<bool, _>("reentry_runtime_fresh")? {
            service_reentry_blockers.push("最近 2 分钟没有运行时上报".to_owned());
        }
        for (column, label) in [
            ("phantun_state", "Phantun"),
            ("wireguard_state", "WireGuard"),
            ("xray_state", "Xray"),
            ("hy2_port_hop_state", "HY2 端口跳转"),
            ("grants_state", "授权名单"),
        ] {
            if row
                .try_get::<Option<String>, _>(column)?
                .as_deref()
                .is_none_or(|state| matches!(state, "unknown" | "dirty"))
            {
                service_reentry_blockers.push(format!("{label} 尚未确认"));
            }
        }
        if row.try_get::<bool, _>("overlay")? {
            let wg_ready = row
                .try_get::<Option<Value>, _>("wireguard_health")?
                .and_then(|health| {
                    let enabled = health.get("enabled")?.as_bool()?;
                    let no_error = health
                        .get("error")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty);
                    Some(enabled && no_error)
                })
                .unwrap_or(false);
            if !wg_ready {
                service_reentry_blockers.push("WireGuard 运行状态尚未就绪".to_owned());
            }
        }
    }
    let service_reentry_ready = operationally_isolated && service_reentry_blockers.is_empty();

    Ok(NodeAgentStateItem {
        node_id: row.try_get("node_id")?,
        tenant_id: row.try_get("tenant_id")?,
        name: row.try_get("name")?,
        public_ipv4: row.try_get("public_ipv4")?,
        public_ipv4_country: None,
        public_ipv6: row.try_get("public_ipv6")?,
        public_ipv4_nat: row.try_get("public_ipv4_nat")?,
        public_ipv6_nat: row.try_get("public_ipv6_nat")?,
        route_ipv4: row.try_get("route_ipv4")?,
        route_ipv6: row.try_get("route_ipv6")?,
        token_prefix: row.try_get("token_prefix")?,
        token_created_at: row.try_get("token_created_at")?,
        token_last_used_at: row.try_get("token_last_used_at")?,
        token_revoked_at: row.try_get("token_revoked_at")?,
        agent_version: row.try_get("agent_version")?,
        agent_protocol_version: row.try_get("agent_protocol_version")?,
        // An empty object counts as never reported, as NULL does: the column is
        // NOT NULL DEFAULT '{}', a newly enrolled machine is born with an empty object, and that
        // is the same thing as an older agent's NULL.
        runtime_versions: non_empty_json(row.try_get("runtime_versions").ok()),
        spool_backlog: non_empty_json(row.try_get("spool_backlog").ok()),
        last_local_reconcile: row.try_get("last_local_reconcile").ok().flatten(),
        wireguard_health: non_empty_json(row.try_get("wireguard_health").ok()),
        runtime_reported_at: row.try_get("runtime_reported_at")?,
        geodata_observed: non_empty_json(row.try_get("geodata_observed").ok()),
        last_poll_at: row.try_get("last_poll_at")?,
        last_usage_report_at: row.try_get("last_usage_report_at")?,
        usage_last_result: non_empty_json(row.try_get("usage_last_result").ok()),
        usage_generation_id: row.try_get("usage_generation_id")?,
        xray_started_at: row.try_get("xray_started_at")?,
        overlay: row.try_get("overlay")?,
        egress_allowed: row.try_get("egress_allowed")?,
        mtu: row
            .try_get::<Option<i32>, _>("mtu")?
            .map(|value| u16::try_from(value).unwrap_or_default()),
        connection: NodeConnection {
            conn_idle_secs: node_conn_column(row, "conn_idle_secs")?,
            uplink_only_secs: node_conn_column(row, "conn_uplink_only_secs")?,
            downlink_only_secs: node_conn_column(row, "conn_downlink_only_secs")?,
            buffer_size_kb: node_conn_column(row, "conn_buffer_size_kb")?,
        },
        dns: match row.try_get::<String, _>("dns_kind")?.as_str() {
            "servers" => Dns::Servers(
                serde_json::from_value(row.try_get::<Value, _>("dns_servers")?).map_err(
                    |error| StoreError::InvalidData(format!("nodes.dns_servers 解不开: {error}")),
                )?,
            ),
            _ => Dns::System,
        },
        domain_strategy: serde_json::from_value(Value::String(
            row.try_get::<String, _>("domain_strategy")?,
        ))
        .map_err(|error| {
            StoreError::InvalidData(format!("nodes.domain_strategy 解不开: {error}"))
        })?,
        retired_at: row.try_get("retired_at")?,
        lifecycle_phase,
        lifecycle_epoch: u64::try_from(row.try_get::<i64, _>("lifecycle_epoch")?)
            .map_err(|_| StoreError::InvalidData("node lifecycle epoch is negative".to_owned()))?,
        lifecycle_deployment_id: row.try_get("lifecycle_deployment_id")?,
        lifecycle_completed_at: row.try_get("lifecycle_completed_at")?,
        lifecycle_last_error: row.try_get("lifecycle_last_error")?,
        operationally_isolated,
        isolated_at: row.try_get("isolated_at")?,
        convergence_debt_count,
        convergence_debt_failed: row.try_get("convergence_debt_failed")?,
        service_reentry_ready,
        service_reentry_blockers,
        wg_transport_kind: {
            let value = row.try_get::<Value, _>("wg_transport")?;
            value
                .get("t")
                .and_then(Value::as_str)
                .unwrap_or("udp")
                .to_owned()
        },
        wg_fake_tcp_port: row
            .try_get::<Value, _>("wg_transport")?
            .get("v")
            .and_then(|v| v.get("port"))
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok()),
        applied,
    })
}

fn tenant_item_from_row(row: &sqlx::postgres::PgRow) -> Result<TenantListItem> {
    Ok(TenantListItem {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        created_at: row.try_get("created_at")?,
        created_revision: row
            .try_get::<Option<i64>, _>("created_revision")?
            .map(|value| i64_to_u64(value, "created_revision"))
            .transpose()?,
        node_count: i64_to_u64(row.try_get("node_count")?, "node_count")?,
        user_count: i64_to_u64(row.try_get("user_count")?, "user_count")?,
        operator_count: i64_to_u64(row.try_get("operator_count")?, "operator_count")?,
    })
}

fn revision_item_from_row(
    row: &sqlx::postgres::PgRow,
    current_revision: u64,
    include_note: bool,
) -> Result<RevisionListItem> {
    let id = i64_to_u64(row.try_get("id")?, "revision_id")?;
    let author = if include_note {
        row.try_get("author")?
    } else {
        None
    };
    let note = if include_note {
        row.try_get("note")?
    } else {
        None
    };
    Ok(RevisionListItem {
        id,
        created_at: row.try_get("created_at")?,
        author,
        note,
        status: row.try_get("status")?,
        current: id == current_revision,
        has_snapshot: row.try_get("has_snapshot")?,
    })
}

fn user_item_from_row(row: &sqlx::postgres::PgRow) -> Result<UserListItem> {
    Ok(UserListItem {
        tenant_id: row.try_get("tenant_id")?,
        id: row.try_get("id")?,
        uuid: row.try_get("uuid")?,
        status: row.try_get("status")?,
        account_type: parse_user_account_type(row.try_get("account_type")?)?,
        login_enabled: row.try_get("login_enabled")?,
        created_at: row.try_get("created_at")?,
        created_revision: row
            .try_get::<Option<i64>, _>("created_revision")?
            .map(|value| i64_to_u64(value, "created_revision"))
            .transpose()?,
    })
}

async fn load_user_item(pool: &PgPool, tenant_id: &str, user_id: &str) -> Result<UserListItem> {
    let row = sqlx::query(
        "SELECT tenant_id, id, uuid::text AS uuid, status, account_type,
                EXISTS (
                    SELECT 1 FROM admin_operators o
                    WHERE o.role = 'user'
                      AND o.user_tenant_id = users.tenant_id
                      AND o.user_id = users.id
                ) AS login_enabled,
                created_at::text AS created_at, created_revision
         FROM users
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("user {tenant_id}/{user_id}")))?;
    user_item_from_row(&row)
}

async fn load_tenant(pool: &PgPool, tenant_id: &str) -> Result<TenantListItem> {
    let row = sqlx::query(
        "SELECT t.id,
                t.name,
                t.created_at::text AS created_at,
                t.created_revision,
                (SELECT count(*) FROM nodes n WHERE n.tenant_id = t.id) AS node_count,
                (SELECT count(*) FROM users u WHERE u.tenant_id = t.id) AS user_count,
                (SELECT count(*) FROM admin_operators o WHERE o.tenant_scope = t.id AND o.role <> 'user') AS operator_count
         FROM tenants t
         WHERE t.id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("tenant {tenant_id}")))?;
    tenant_item_from_row(&row)
}

async fn load_app(pool: &PgPool, app_id: &str) -> Result<AppView> {
    crate::materialize::load_current_snapshot(pool)
        .await?
        .apps
        .into_iter()
        .find(|app| app.id == app_id)
        .ok_or_else(|| StoreError::NotFound(format!("app {app_id}")))
}

fn push_node_artifact(
    out: &mut Vec<ArtifactIndexEntry>,
    node_id: &str,
    artifact_kind: &str,
    content: String,
) {
    push_artifact(out, "node", node_id, artifact_kind, content);
}

fn push_user_artifact(
    out: &mut Vec<ArtifactIndexEntry>,
    user_key: &str,
    artifact_kind: &str,
    content: String,
) {
    push_artifact(out, "user", user_key, artifact_kind, content);
}

fn push_artifact(
    out: &mut Vec<ArtifactIndexEntry>,
    target_kind: &str,
    target_id: &str,
    artifact_kind: &str,
    content: String,
) {
    out.push(ArtifactIndexEntry {
        target_kind: target_kind.to_owned(),
        target_id: target_id.to_owned(),
        artifact_kind: artifact_kind.to_owned(),
        state: "present".to_owned(),
        sha256: Some(sha256_hex(content.as_bytes())),
        byte_len: Some(content.len() as u64),
    });
}

fn push_disabled_artifact(
    out: &mut Vec<ArtifactIndexEntry>,
    target_kind: &str,
    target_id: &str,
    artifact_kind: &str,
) {
    out.push(ArtifactIndexEntry {
        target_kind: target_kind.to_owned(),
        target_id: target_id.to_owned(),
        artifact_kind: artifact_kind.to_owned(),
        state: "disabled".to_owned(),
        sha256: None,
        byte_len: None,
    });
}

/// Take the write lock, returning the current revision number along the way — a commit that
/// changed nothing returns it (see `commit_revision`), and this row is read anyway.
pub(crate) async fn lock_control_state(tx: &mut Transaction<'_, Postgres>) -> Result<u64> {
    let row = sqlx::query("SELECT current_revision FROM control_state WHERE id = TRUE FOR UPDATE")
        .fetch_one(&mut **tx)
        .await?;
    i64_to_u64(row.try_get("current_revision")?, "current_revision")
}

/// Once the writes are done, whether this change is worth a new number. Returns the revision
/// number that actually took effect.
///
/// The rule is that a changed field produces a revision — a *changed* field. A commit that
/// touched no row (storing a form again unchanged, editing a value back to what it was) should
/// leave nothing in the history: the revision list would fill with contentless entries, and
/// questions like "how many machines did revision N move" would all answer "none". The same
/// reasoning as "artifacts byte-for-byte unchanged means no restart", except that gate sits on
/// the artifacts and this one on the model.
///
/// Whether something changed is stated by each write site's own SQL (`IS DISTINCT FROM` plus
/// `rows_affected`) and cannot be judged from the materialized snapshot: the snapshot has no
/// `tenants` table at all, creating an empty tenant is a real write, and yet the snapshot does
/// not move one byte — judged by the snapshot it would count as an empty commit, while that
/// row's `created_revision` already points at the number and returning it walks straight into
/// the foreign key.
pub(crate) async fn commit_revision(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
    previous: u64,
    changed: bool,
) -> Result<u64> {
    commit_revision_inner(tx, revision_id, previous, changed, true).await
}

/// Quota enforcement already creates its own grants deployment synchronously and returns that
/// deployment id to the caller.  It uses this variant so the general automatic worker cannot
/// race it or create a duplicate job for the same revision.
pub(crate) async fn commit_revision_without_grant_automation(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
    previous: u64,
    changed: bool,
) -> Result<u64> {
    commit_revision_inner(tx, revision_id, previous, changed, false).await
}

async fn commit_revision_inner(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
    previous: u64,
    changed: bool,
    automate_grants: bool,
) -> Result<u64> {
    if changed {
        set_current_revision(tx, revision_id).await?;
        if automate_grants {
            crate::grant_automation::enqueue_if_changed_tx(tx, previous, revision_id).await?;
        }
        return Ok(revision_id);
    }
    // Return the number just taken. No row references it — changed being false means no row was
    // written, and the `ON CONFLICT ... WHERE` clauses do not touch even created_revision where
    // the values match.
    sqlx::query("DELETE FROM revisions WHERE id = $1")
        .bind(u64_to_i64(revision_id, "revision_id")?)
        .execute(&mut **tx)
        .await?;
    // An IDENTITY cursor does not move back when rows are deleted. Without winding it back, every
    // empty commit advances the next real change by one and the revision list looks as though
    // entries went missing. The write lock on control_state is still held, so nobody takes a
    // number in between.
    sqlx::query(
        "SELECT setval(
            pg_get_serial_sequence('revisions', 'id'),
            COALESCE((SELECT MAX(id) FROM revisions), 1),
            true
         )",
    )
    .execute(&mut **tx)
    .await?;
    Ok(previous)
}

/// The caller's note, falling back to a default sentence when empty.
///
/// The note stays in the outer layer rather than the transactional core: one commit is one
/// revision and one note, and the core only writes its own table. In a batch commit ten
/// operations share a single "committed 10 changes", and the core's individual wordings go
/// unused.
pub(crate) fn note_or(note: Option<&str>, fallback: impl FnOnce() -> String) -> String {
    note.and_then(optional_text)
        .map(str::to_owned)
        .unwrap_or_else(fallback)
}

pub(crate) async fn insert_revision(
    tx: &mut Transaction<'_, Postgres>,
    author: &str,
    note: &str,
) -> Result<u64> {
    let row = sqlx::query(
        "INSERT INTO revisions (author, note)
         VALUES ($1, $2)
         RETURNING id",
    )
    .bind(author)
    .bind(note)
    .fetch_one(&mut **tx)
    .await?;
    i64_to_u64(row.try_get("id")?, "revision_id")
}

async fn set_current_revision(tx: &mut Transaction<'_, Postgres>, revision_id: u64) -> Result<()> {
    sqlx::query("UPDATE control_state SET current_revision = $1 WHERE id = TRUE")
        .bind(u64_to_i64(revision_id, "revision_id")?)
        .execute(&mut **tx)
        .await?;
    crate::materialize::store_current_snapshot_tx(tx, revision_id).await?;
    crate::subscription_client::advance_from_committed_tx(tx, revision_id).await?;
    Ok(())
}

async fn ensure_tenant_exists_tx(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
) -> Result<()> {
    let exists = sqlx::query("SELECT 1 FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    exists
        .then_some(())
        .ok_or_else(|| StoreError::NotFound(format!("tenant {tenant_id}")))
}

async fn ensure_app_exists_tx(tx: &mut Transaction<'_, Postgres>, app_id: &str) -> Result<()> {
    let exists = sqlx::query("SELECT 1 FROM apps WHERE id = $1")
        .bind(app_id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    exists
        .then_some(())
        .ok_or_else(|| StoreError::NotFound(format!("app {app_id}")))
}

pub(crate) async fn ensure_node_exists_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
) -> Result<()> {
    let exists = sqlx::query("SELECT 1 FROM nodes WHERE id = $1")
        .bind(node_id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    exists
        .then_some(())
        .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))
}

async fn ensure_user_exists_tx(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    user_id: &str,
) -> Result<()> {
    let exists = sqlx::query("SELECT 1 FROM users WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(user_id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    exists
        .then_some(())
        .ok_or_else(|| StoreError::NotFound(format!("user {tenant_id}/{user_id}")))
}

async fn ensure_user_missing(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    user_id: &str,
) -> Result<()> {
    let exists = sqlx::query("SELECT 1 FROM users WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(user_id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    if exists {
        Err(StoreError::Unsupported(format!(
            "user {tenant_id}/{user_id} already exists"
        )))
    } else {
        Ok(())
    }
}

pub(crate) async fn chain_tenant_tx(
    tx: &mut Transaction<'_, Postgres>,
    app_id: &str,
    chain_id: &str,
) -> Result<String> {
    sqlx::query(
        "SELECT tenant_id
         FROM chains
         WHERE app_id = $1 AND id = $2",
    )
    .bind(app_id)
    .bind(chain_id)
    .fetch_optional(&mut **tx)
    .await?
    .map(|row| row.try_get("tenant_id").map_err(StoreError::from))
    .transpose()?
    .ok_or_else(|| StoreError::NotFound(format!("chain {app_id}/{chain_id}")))
}

async fn ensure_ingress_in_app_tx(
    tx: &mut Transaction<'_, Postgres>,
    app_id: &str,
    ingress_id: &str,
) -> Result<()> {
    let exists = sqlx::query("SELECT 1 FROM ingresses WHERE app_id = $1 AND id = $2")
        .bind(app_id)
        .bind(ingress_id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    exists
        .then_some(())
        .ok_or_else(|| StoreError::NotFound(format!("ingress {app_id}/{ingress_id}")))
}

async fn ensure_ingresses_in_app_tx(
    tx: &mut Transaction<'_, Postgres>,
    app_id: &str,
    ingress_ids: &[String],
) -> Result<()> {
    for ingress_id in ingress_ids {
        ensure_ingress_in_app_tx(tx, app_id, ingress_id).await?;
    }
    Ok(())
}

async fn ensure_front_in_app_tx(
    tx: &mut Transaction<'_, Postgres>,
    app_id: &str,
    front_id: &str,
) -> Result<()> {
    let exists = sqlx::query("SELECT 1 FROM fronts WHERE app_id = $1 AND id = $2")
        .bind(app_id)
        .bind(front_id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    exists
        .then_some(())
        .ok_or_else(|| StoreError::NotFound(format!("front {app_id}/{front_id}")))
}

async fn ensure_existing_ingress_same_app(
    tx: &mut Transaction<'_, Postgres>,
    app_id: &str,
    ingress_id: &str,
) -> Result<()> {
    let app = sqlx::query("SELECT app_id FROM ingresses WHERE id = $1")
        .bind(ingress_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(|row| row.try_get::<String, _>("app_id"))
        .transpose()?;
    if app.as_deref().is_some_and(|existing| existing != app_id) {
        return Err(StoreError::Unsupported(format!(
            "ingress {ingress_id} already belongs to app {}",
            app.unwrap()
        )));
    }
    Ok(())
}

pub(crate) async fn existing_step_accept_uuid(
    tx: &mut Transaction<'_, Postgres>,
    chain_id: &str,
    node_id: &str,
) -> Result<Option<String>> {
    sqlx::query(
        "SELECT accept_uuid::text AS accept_uuid
         FROM steps
         WHERE chain_id = $1 AND node_id = $2",
    )
    .bind(chain_id)
    .bind(node_id)
    .fetch_optional(&mut **tx)
    .await?
    // The column is nullable: a step that exists without an accept holds NULL, decoding it as
    // String errors on the spot, and the symptom is that editing a step without an accept always
    // 500s.
    .map(|row| {
        row.try_get::<Option<String>, _>("accept_uuid")
            .map_err(StoreError::from)
    })
    .transpose()
    .map(Option::flatten)
}

/// Returns whether the ingress list really moved.
async fn replace_front_via(
    tx: &mut Transaction<'_, Postgres>,
    front_id: &str,
    via: &[String],
) -> Result<bool> {
    let existing: Vec<String> = sqlx::query_scalar(
        "SELECT ingress_id FROM front_vias WHERE front_id = $1 ORDER BY ordinal",
    )
    .bind(front_id)
    .fetch_all(&mut **tx)
    .await?;
    // As on the trunk: rewriting the table unchanged is a write whose contents did not
    // change.
    if existing == via {
        return Ok(false);
    }
    sqlx::query("DELETE FROM front_vias WHERE front_id = $1")
        .bind(front_id)
        .execute(&mut **tx)
        .await?;
    for (ordinal, ingress_id) in via.iter().enumerate() {
        sqlx::query(
            "INSERT INTO front_vias (front_id, ingress_id, ordinal)
             VALUES ($1, $2, $3)",
        )
        .bind(front_id)
        .bind(ingress_id)
        .bind(
            i32::try_from(ordinal).map_err(|_| {
                StoreError::InvalidData("front via ordinal out of range".to_owned())
            })?,
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(true)
}

async fn ensure_external_outbounds_for_tenant_tx(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    outbound_ids: &[String],
) -> Result<()> {
    for outbound_id in outbound_ids {
        let row = sqlx::query(
            "SELECT tenant_id, protocol
             FROM external_outbounds
             WHERE id = $1",
        )
        .bind(outbound_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| {
            StoreError::InvalidData(format!(
                "front references external tunnel {outbound_id}, which does not exist"
            ))
        })?;
        if row.try_get::<String, _>("tenant_id")? != tenant_id {
            return Err(StoreError::InvalidData(format!(
                "front and external tunnel {outbound_id} must belong to the same tenant"
            )));
        }
        if row.try_get::<String, _>("protocol")? == "warp" {
            return Err(StoreError::InvalidData(format!(
                "WARP tunnel {outbound_id} has machine-specific identities and cannot be exported to a user Clash subscription"
            )));
        }
    }
    Ok(())
}

async fn replace_front_external_via(
    tx: &mut Transaction<'_, Postgres>,
    front_id: &str,
    via: &[String],
) -> Result<bool> {
    let existing: Vec<String> = sqlx::query_scalar(
        "SELECT outbound_id
         FROM front_external_vias
         WHERE front_id = $1
         ORDER BY ordinal",
    )
    .bind(front_id)
    .fetch_all(&mut **tx)
    .await?;
    if existing == via {
        return Ok(false);
    }
    sqlx::query("DELETE FROM front_external_vias WHERE front_id = $1")
        .bind(front_id)
        .execute(&mut **tx)
        .await?;
    for (ordinal, outbound_id) in via.iter().enumerate() {
        sqlx::query(
            "INSERT INTO front_external_vias (front_id, outbound_id, ordinal)
             VALUES ($1, $2, $3)",
        )
        .bind(front_id)
        .bind(outbound_id)
        .bind(i32::try_from(ordinal).map_err(|_| {
            StoreError::InvalidData("front external via ordinal out of range".to_owned())
        })?)
        .execute(&mut **tx)
        .await?;
    }
    Ok(true)
}

fn ensure_tenant_management_allowed(actor: &AdminContext, tenant_id: &str) -> Result<()> {
    if actor.is_system_admin()
        || (actor.role() == AdminRole::TenantAdmin && actor.can_access_tenant(tenant_id))
    {
        Ok(())
    } else {
        Err(StoreError::Forbidden(
            "tenant management requires tenant-admin".to_owned(),
        ))
    }
}

fn normalize_reality_request(
    request: CreateRealityIngressRequest,
) -> Result<CreateRealityIngressRequest> {
    // The site may be omitted entirely: it then follows the global settings, filled in by
    // materialize. Written, it must be written completely — a dest without server_names is a
    // half configuration whose handshake is certain to fail.
    let dest = optional_owned_text(request.dest).map(|dest| normalize_host_port(&dest));
    let server_names = normalize_id_list(request.server_names, "reality.server_names")?;
    if dest
        .as_deref()
        .is_some_and(|dest| !is_nonzero_host_port(dest))
    {
        return Err(StoreError::InvalidData(
            "reality.dest must use host:port with a port between 1 and 65535".to_owned(),
        ));
    }
    if dest.is_some() && server_names.is_empty() {
        return Err(StoreError::InvalidData(
            "reality.server_names must not be empty when reality.dest is set".to_owned(),
        ));
    }
    if let Some(server_name) = server_names
        .iter()
        .find(|server_name| !is_reality_server_name(server_name))
    {
        return Err(StoreError::InvalidData(format!(
            "reality.server_names contains invalid name {server_name:?}"
        )));
    }
    let fingerprint = optional_owned_text(request.fingerprint);
    if fingerprint
        .as_deref()
        .is_some_and(|fingerprint| !is_reality_fingerprint(fingerprint))
    {
        return Err(StoreError::InvalidData(
            "reality.fingerprint is unsupported by the pinned Xray REALITY client".to_owned(),
        ));
    }
    // Not `optional_owned_text`: that turns an empty string into `None`, and for flow those two
    // are different states — `None` is "follow the global setting", empty is "this ingress has it
    // off". Collapsing them makes the second unreachable, which is what kept XHTTP from being
    // usable on a single ingress.
    let flow = request.flow.map(|value| value.trim().to_owned());
    crate::settings::validate_flow(flow.as_deref(), "reality.flow")?;
    if let Some(RealityFallbackLimits::Custom { upload, download }) =
        request.fallback_limits.as_ref()
    {
        validate_fallback_rate(upload, "reality.fallback_limits.upload")?;
        validate_fallback_rate(download, "reality.fallback_limits.download")?;
    }
    Ok(CreateRealityIngressRequest {
        fallback_mode: request.fallback_mode,
        fallback_limits: request.fallback_limits,
        fallback_guard: request.fallback_guard,
        dest,
        server_names,
        fingerprint,
        flow,
    })
}

/// AnyTLS owns a REALITY target separate from VLESS. Its request is nested in the AnyTLS wire,
/// while the older top-level `reality` object remains VLESS-only. Only global and custom targets
/// are accepted: a node certificate is the TLS mode, not an AnyTLS REALITY fallback mode.
fn normalize_anytls_reality_request(
    anytls: Option<&brocade_core::model::AnyTls>,
    site: &brocade_core::model::RealitySite,
) -> Result<(Option<Value>, Option<RealitySettings>)> {
    let Some(anytls) = anytls else {
        return Ok((None, None));
    };
    let requested = anytls.reality.clone().unwrap_or(RealitySettings {
        dest: String::new(),
        server_names: Vec::new(),
        fingerprint: String::new(),
        flow: None,
        fallback_mode: RealityFallbackMode::GlobalSite,
        fallback_limits: RealityFallbackLimits::Balanced,
        fallback_guard: true,
    });
    if requested.fallback_mode == RealityFallbackMode::NodeCertificate {
        return Err(StoreError::InvalidData(
            "AnyTLS REALITY target must be global-site or custom-site".to_owned(),
        ));
    }
    let mut normalized = normalize_reality_request(CreateRealityIngressRequest {
        fallback_mode: Some(requested.fallback_mode),
        fallback_limits: Some(requested.fallback_limits),
        fallback_guard: Some(requested.fallback_guard),
        dest: (!requested.dest.trim().is_empty()).then_some(requested.dest),
        server_names: requested.server_names,
        fingerprint: (!requested.fingerprint.trim().is_empty()).then_some(requested.fingerprint),
        // Vision is a VLESS account flow. AnyTLS never stores or emits it.
        flow: None,
    })?;
    let fallback_mode = normalized
        .fallback_mode
        .unwrap_or(RealityFallbackMode::GlobalSite);
    match fallback_mode {
        RealityFallbackMode::GlobalSite => {
            normalized.dest = None;
            normalized.server_names.clear();
            normalized.fingerprint = None;
        }
        RealityFallbackMode::CustomSite => {
            if normalized.dest.is_none() || normalized.server_names.is_empty() {
                return Err(StoreError::InvalidData(
                    "custom AnyTLS REALITY target requires dest and server_names".to_owned(),
                ));
            }
        }
        RealityFallbackMode::NodeCertificate => unreachable!("rejected above"),
    }
    let stored = RealitySettings {
        dest: normalized.dest.unwrap_or_default(),
        server_names: normalized.server_names,
        fingerprint: normalized.fingerprint.unwrap_or_default(),
        flow: None,
        fallback_mode,
        fallback_limits: normalized
            .fallback_limits
            .unwrap_or(RealityFallbackLimits::Balanced),
        fallback_guard: normalized.fallback_guard.unwrap_or(true),
    };
    let mut effective = stored.clone();
    if fallback_mode == RealityFallbackMode::GlobalSite {
        effective.dest = site.dest.clone().unwrap_or_default();
        effective.server_names = site.server_names.clone();
        effective.fingerprint = site
            .fingerprint
            .clone()
            .unwrap_or_else(|| "chrome".to_owned());
    }
    Ok((Some(serde_json::to_value(stored)?), Some(effective)))
}

fn validate_fallback_rate(rate: &RealityFallbackRateLimit, field: &str) -> Result<()> {
    if rate.bytes_per_sec == 0
        || rate.burst_bytes_per_sec == 0
        || rate.burst_bytes_per_sec < rate.bytes_per_sec
    {
        return Err(StoreError::InvalidData(format!(
            "{field} must have positive rates and burst_bytes_per_sec >= bytes_per_sec"
        )));
    }
    Ok(())
}

pub(crate) fn normalize_step_accept(
    request: Option<StepAcceptRequest>,
    existing_uuid: Option<String>,
    chain_id: &str,
    node_id: &str,
) -> Result<Option<brocade_core::model::Accept>> {
    let Some(request) = request else {
        return Ok(None);
    };
    let uuid = request
        .uuid
        .and_then(optional_text_owned)
        .or(existing_uuid)
        .map(Ok)
        .unwrap_or_else(generate_uuid_v4)?;
    let label = request
        .label
        .and_then(optional_text_owned)
        .unwrap_or_else(|| format!("{chain_id}@{node_id}"));
    Ok(Some(brocade_core::model::Accept { uuid, label }))
}

fn normalize_id_list(values: Vec<String>, field: &str) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        out.push(required_text(value, field)?);
    }
    Ok(out)
}

fn normalize_user_status(value: &str) -> Result<&'static str> {
    match value.trim() {
        "active" => Ok("active"),
        "disabled" => Ok("disabled"),
        value => Err(StoreError::InvalidData(format!(
            "user status must be active or disabled, got {value}"
        ))),
    }
}

fn parse_user_account_type(value: String) -> Result<UserAccountType> {
    match value.as_str() {
        "formal" => Ok(UserAccountType::Formal),
        "test" => Ok(UserAccountType::Test),
        _ => Err(StoreError::InvalidData(format!(
            "unknown user account type {value}"
        ))),
    }
}

/// An id becomes a slug in the artifacts verbatim, so it is blocked at the write path rather
/// than at compile time — by then the object is referenced by others and deleting it means
/// clearing several tables. The test itself comes from brocade-core, shared with the
/// compiler.
pub(crate) fn required_slug(value: String, field: &str) -> Result<String> {
    let value = required_text(value, field)?;
    if !brocade_core::model::is_valid_slug(&value) {
        return Err(StoreError::InvalidData(format!(
            "{field}「{value}」不满足 slug 字符集 [a-z0-9._-]{{1,32}}（不能有大写）；\
             显示用的名字放 name / label 字段，那里没有字符限制"
        )));
    }
    Ok(value)
}

fn optional_text(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn optional_text_owned(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn optional_owned_text(value: Option<String>) -> Option<String> {
    value.and_then(optional_text_owned)
}

/// Normalization of one family's projected endpoint. `None` on the way in means no projection,
/// landing as two NULLs. The nested download shape is historical client state and is deliberately
/// discarded here; XHTTP downloads are persisted from `wires.*.xhttp.download` instead.
///
/// An empty host is turned back here rather than left to the database's CHECK: what a constraint
/// reports is "violates ingresses_projection_v4_check", from which the UI cannot tell which field
/// was filled in wrongly. The constraint still stays — it guards against whoever writes to the
/// database bypassing this path.
fn normalized_projection(
    endpoint: Option<ProjectionEndpoint>,
    field: &str,
) -> Result<Option<ProjectionEndpoint>> {
    let Some(endpoint) = endpoint else {
        return Ok(None);
    };
    let host = required_text(endpoint.host, &format!("{field} host"))?;
    ensure_nonzero_port(endpoint.port, &format!("{field} port"))?;
    Ok(Some(ProjectionEndpoint {
        host,
        port: endpoint.port,
        download: None,
    }))
}

/// The column holds `DomainStrategy`'s serde spelling, which is what the CHECK constraint
/// lists too. Going through serde rather than a match keeps the two in step.
fn domain_strategy_column(strategy: DomainStrategy) -> Result<String> {
    match serde_json::to_value(strategy)? {
        Value::String(value) => Ok(value),
        other => Err(StoreError::InvalidData(format!(
            "domain strategy did not serialize to a string: {other}"
        ))),
    }
}

fn dns_servers_json(dns: &Dns) -> Result<Value> {
    match dns {
        Dns::System => Ok(json!([])),
        Dns::Servers(servers) => Ok(serde_json::to_value(servers)?),
    }
}

fn json_string_array(value: &Value) -> Result<Vec<String>> {
    let Value::Array(items) = value else {
        return Err(StoreError::InvalidData(
            "expected JSON string array".to_owned(),
        ));
    };
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| StoreError::InvalidData(format!("array[{index}] must be a string")))
        })
        .collect()
}

fn require_actor_tenant_scope(actor: &AdminContext) -> Result<&str> {
    actor
        .tenant_scope()
        .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))
}

fn i64_to_u64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| StoreError::InvalidData(format!("{field} out of range")))
}

pub(crate) fn u64_to_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::InvalidData(format!("{field} out of range")))
}

#[cfg(test)]
mod model_id_tests {
    use super::*;

    #[test]
    fn accepts_only_the_random_model_id_shapes() {
        assert!(validate_model_id("app", "app-8f3a").is_ok());
        assert!(validate_model_id("ingress", "ing-8f3a").is_ok());
        assert!(validate_model_id("chain", "chn-8f3a-2d71").is_ok());
        assert!(validate_model_id("ingress", "i-bacemu").is_err());
        assert!(validate_model_id("chain", "c-lumira").is_err());
        assert!(validate_model_id("chain", "chn-8F3A-2d71").is_err());
        assert!(validate_model_id("app", "app-main").is_err());
    }

    #[test]
    fn requires_the_chain_to_carry_its_ingress_token() {
        assert!(validate_model_id_pair("ing-8f3a", "chn-8f3a-2d71").is_ok());
        assert!(validate_model_id_pair("ing-8f3a", "chn-a410-2d71").is_err());
    }
}
