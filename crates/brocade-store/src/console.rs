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
    compile::compile,
    format::{ini, json as json_format, uri, yaml},
    hash::sha256_hex,
    model::{
        AppView, Chain, Dns, DomainStrategy, ExternalOutboundProtocol, Front, Grant, HopEncryption,
        HopWire, HysteriaCongestion, HysteriaMasquerade, HysteriaObfs, Ingress, IngressIdentity,
        IngressWires, IngressWiresWire, ModelSnapshot, NodeConnection, Projection,
        ProjectionDownloadEndpoint, ProjectionEndpoint, Reality, RealityFallbackLimits,
        RealityFallbackMode, RealityFallbackRateLimit, RealitySettings, RealityXhttp, Tls,
        TlsXhttp, Transport, User, WgTransport,
    },
    text::normalize_host_port,
};
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
    let mut value = serde_json::to_value(snapshot)?;
    redact_private_keys(&mut value);
    Ok(ConsoleSnapshot {
        snapshot: value,
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
    let changed = sqlx::query(
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
    let label = required_text(request.label, "app label")?;
    let changed = sqlx::query(
        "INSERT INTO apps (id, label, created_revision)
         VALUES ($1, $2, $3)
         ON CONFLICT (id) DO UPDATE SET
            label = EXCLUDED.label,
            created_revision = COALESCE(apps.created_revision, EXCLUDED.created_revision)
         WHERE apps.label IS DISTINCT FROM EXCLUDED.label",
    )
    .bind(&id)
    .bind(&label)
    .bind(u64_to_i64(revision_id, "revision_id")?)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    Ok(changed)
}

pub(crate) async fn upsert_external_outbound_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    request: UpsertExternalOutboundRequest,
) -> Result<bool> {
    let app_id = required_text(request.app_id, "app_id")?;
    let id = required_slug(request.id, "external outbound id")?;
    let tenant_id = required_text(request.tenant_id, "tenant_id")?;
    actor.require_tenant_access(&tenant_id, "external outbound")?;
    ensure_app_exists_tx(tx, &app_id).await?;
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
    };

    let existing = sqlx::query(
        "SELECT credential_sealed, protocol FROM external_outbounds WHERE app_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(&app_id)
    .bind(&id)
    .fetch_optional(&mut **tx)
    .await?;
    let credential = request.protocol.credential();
    let credential_sealed = if credential == "<redacted>" {
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
            required_text(credential.to_owned(), "external outbound credential")?
        };
        crate::secrets::seal(
            &crate::secrets::external_outbound_context(&app_id, &id),
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
    };
    let security = serde_json::to_value(request.security)?;
    let changed = sqlx::query(
        "INSERT INTO external_outbounds
            (app_id, id, tenant_id, name, address, port, protocol, credential_sealed,
             protocol_options, security, created_revision)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         ON CONFLICT (app_id, id) DO UPDATE SET
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
    .bind(&app_id)
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
    actor.require_tenant_access(&tenant_id, "chain")?;
    ensure_app_exists_tx(tx, &app_id).await?;
    ensure_tenant_exists_tx(tx, &tenant_id).await?;
    let head_changed = sqlx::query(
        "INSERT INTO chains (id, app_id, tenant_id, name, created_revision)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (id) DO UPDATE SET
            app_id = EXCLUDED.app_id,
            tenant_id = EXCLUDED.tenant_id,
            name = EXCLUDED.name,
            created_revision = COALESCE(chains.created_revision, EXCLUDED.created_revision)
         WHERE ROW(chains.app_id, chains.tenant_id, chains.name)
            IS DISTINCT FROM ROW(EXCLUDED.app_id, EXCLUDED.tenant_id, EXCLUDED.name)",
    )
    .bind(&id)
    .bind(&app_id)
    .bind(&tenant_id)
    .bind(&name)
    .bind(u64_to_i64(revision_id, "revision_id")?)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    Ok((
        Chain {
            id,
            tenant: tenant_id,
            name,
        },
        head_changed,
    ))
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
    let strategy = request.strategy.as_str();
    ensure_app_exists_tx(tx, &app_id).await?;
    ensure_tenant_exists_tx(tx, &tenant_id).await?;
    ensure_ingresses_in_app_tx(tx, &app_id, &via).await?;
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

    Ok((
        Front {
            id,
            tenant: tenant_id,
            name,
            via,
            strategy: request.strategy,
        },
        head_changed || via_changed,
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
    ensure_app_exists_tx(tx, &app_id).await?;
    let chain_tenant = chain_tenant_tx(tx, &app_id, &chain_id).await?;
    actor.require_tenant_access(&chain_tenant, "ingress")?;
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
    let xhttp = request.wires.xhttp();
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
    let row = sqlx::query(
        "INSERT INTO ingresses (
            id, app_id, chain_id, node_id, bind, port, front_id,
            reality_private_key, reality_public_key, reality_short_ids,
            reality_dest, reality_server_names, reality_fingerprint, reality_flow,
            reality_fallback_mode, reality_fallback_limits,
            reality_fallback_guard,
            transport_kind, hy2_enabled, xhttp_path, xhttp_host, xhttp_mux, xhttp_mode,
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
            hy2_quic_max_incoming_streams, hy2_quic_disable_pmtud
         ) VALUES (
            $1, $2, $3, $4, $5::inet, $6, $7,
            $8, $9, $10,
            $11, $12, $13, $14,
            $15, $16,
            $17,
            $18, $19, $20, $21, $22, $23,
            $24, $25, $26,
            $27, $28, $29, $30, $31, $32,
            $33, $34, $35, $36, $37, $38, $39,
            $40, $41, $42, $43, $44, $45, $46,
            $47, $48, $49, $50, $51,
            $52,
            $53,
            $54, $55, $56, $57,
            $58, $59, $60, $61
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
            reality_fingerprint = EXCLUDED.reality_fingerprint,
            reality_flow = EXCLUDED.reality_flow,
            reality_fallback_mode = EXCLUDED.reality_fallback_mode,
            reality_fallback_limits = EXCLUDED.reality_fallback_limits,
            reality_fallback_guard = EXCLUDED.reality_fallback_guard,
            transport_kind = EXCLUDED.transport_kind,
            hy2_enabled = EXCLUDED.hy2_enabled,
            xhttp_path = EXCLUDED.xhttp_path,
            xhttp_host = EXCLUDED.xhttp_host,
            xhttp_mux = EXCLUDED.xhttp_mux,
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
            created_revision = COALESCE(ingresses.created_revision, EXCLUDED.created_revision)
         WHERE ROW(ingresses.app_id, ingresses.chain_id, ingresses.node_id, ingresses.bind,
                   ingresses.port, ingresses.front_id, ingresses.reality_dest,
                   ingresses.reality_server_names, ingresses.reality_fingerprint,
                   ingresses.reality_flow, ingresses.reality_fallback_mode,
                   ingresses.reality_fallback_limits, ingresses.reality_fallback_guard,
                   ingresses.transport_kind, ingresses.hy2_enabled, ingresses.xhttp_path, ingresses.xhttp_host, ingresses.xhttp_mux,
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
                   ingresses.hy2_quic_max_incoming_streams, ingresses.hy2_quic_disable_pmtud)
            IS DISTINCT FROM
            ROW(EXCLUDED.app_id, EXCLUDED.chain_id, EXCLUDED.node_id, EXCLUDED.bind,
                EXCLUDED.port, EXCLUDED.front_id, EXCLUDED.reality_dest,
                EXCLUDED.reality_server_names, EXCLUDED.reality_fingerprint,
                EXCLUDED.reality_flow, EXCLUDED.reality_fallback_mode,
                EXCLUDED.reality_fallback_limits, EXCLUDED.reality_fallback_guard,
                EXCLUDED.transport_kind, EXCLUDED.hy2_enabled, EXCLUDED.xhttp_path,
                EXCLUDED.xhttp_host, EXCLUDED.xhttp_mux,
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
                EXCLUDED.hy2_quic_max_incoming_streams, EXCLUDED.hy2_quic_disable_pmtud)
         RETURNING reality_private_key,
                   reality_public_key,
                   reality_short_ids",
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
    .bind(&reality.fingerprint)
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
    .bind(xhttp.map(|xhttp| xhttp.path.clone()))
    .bind(xhttp.and_then(|xhttp| xhttp.host.clone()))
    .bind(xhttp.and_then(|xhttp| xhttp.mux.map(i32::from)))
    .bind(xhttp.and_then(|xhttp| xhttp.mode.as_str()))
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
    .bind(projection.v4.as_ref().and_then(|to| to.download.as_ref()).and_then(|to| to.http_host.clone()))
    .bind(projection.v4.as_ref().and_then(|to| to.download.as_ref()).and_then(|to| to.mux.map(i32::from)))
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
    .bind(projection.v6.as_ref().and_then(|to| to.download.as_ref()).and_then(|to| to.http_host.clone()))
    .bind(projection.v6.as_ref().and_then(|to| to.download.as_ref()).and_then(|to| to.mux.map(i32::from)))
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
    .fetch_optional(&mut **tx)
    .await?;
    // With the WHERE blocking it, RETURNING yields no row at all, and that is what "nothing
    // changed" means. The keys still have to be echoed, so the existing row is read as usual —
    // note that it reads the database's values rather than the keypair generated above: that pair
    // only lands on creation, and an existing ingress keeps its own (those three columns are
    // absent from DO UPDATE's SET list to begin with).
    let changed = row.is_some();
    let row = match row {
        Some(row) => row,
        None => {
            sqlx::query(
                "SELECT reality_private_key, reality_public_key, reality_short_ids
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
    let ingress = Ingress {
        id,
        chain: chain_id,
        node: node_id,
        bind: request.bind,
        port: request.port,
        front: front_id,
        identity,
        projection,
        guard: request.guard,
        wires: IngressWires::try_from(IngressWiresWire {
            vless: request.wires.vless.as_ref().map(|vless| match vless {
                TransportRequest::VlessReality => Transport::VlessReality(effective.clone()),
                TransportRequest::VlessRealityXhttp { xhttp } => {
                    Transport::VlessRealityXhttp(RealityXhttp {
                        reality: effective.clone(),
                        xhttp: xhttp.clone(),
                    })
                }
                // The echo mirrors what a read of this ingress will return, and a read of a shape
                // holding a certificate returns no borrowed site — so neither does this.
                TransportRequest::VlessTls => Transport::VlessTls(tls_echo(&effective)),
                TransportRequest::VlessTlsXhttp { xhttp } => Transport::VlessTlsXhttp(TlsXhttp {
                    tls: tls_echo(&effective),
                    xhttp: xhttp.clone(),
                }),
            }),
            hysteria2: request.wires.hysteria2.clone(),
        })
        .map_err(|error| StoreError::InvalidData(error.to_owned()))?,
    };
    Ok((ingress, changed))
}

/// The certificate-holding shapes carry the two settings that were never REALITY's — which
/// ClientHello to imitate and whether to run flow control.
fn tls_echo(effective: &RealitySettings) -> Tls {
    Tls {
        flow: effective.flow.clone(),
        fingerprint: effective.fingerprint.clone(),
    }
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
                s.runtime_reported_at::text AS runtime_reported_at,
                s.geodata_observed,
                s.last_poll_at::text AS last_poll_at,
                s.last_usage_report_at::text AS last_usage_report_at,
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
                s.runtime_reported_at::text AS runtime_reported_at,
                s.geodata_observed,
                s.last_poll_at::text AS last_poll_at,
                s.last_usage_report_at::text AS last_usage_report_at,
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
        runtime_reported_at: row.try_get("runtime_reported_at")?,
        geodata_observed: non_empty_json(row.try_get("geodata_observed").ok()),
        last_poll_at: row.try_get("last_poll_at")?,
        last_usage_report_at: row.try_get("last_usage_report_at")?,
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
        created_at: row.try_get("created_at")?,
        created_revision: row
            .try_get::<Option<i64>, _>("created_revision")?
            .map(|value| i64_to_u64(value, "created_revision"))
            .transpose()?,
    })
}

async fn load_user_item(pool: &PgPool, tenant_id: &str, user_id: &str) -> Result<UserListItem> {
    let row = sqlx::query(
        "SELECT tenant_id, id, uuid::text AS uuid, status,
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
                (SELECT count(*) FROM admin_operators o WHERE o.tenant_scope = t.id) AS operator_count
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
    if dest.is_some() && server_names.is_empty() {
        return Err(StoreError::InvalidData(
            "reality.server_names must not be empty when reality.dest is set".to_owned(),
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
        fingerprint: optional_owned_text(request.fingerprint),
        flow,
    })
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
/// landing as two NULLs.
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
    let download = match endpoint.download {
        Some(download) => {
            let host = required_text(download.host, &format!("{field} download host"))?;
            ensure_nonzero_port(download.port, &format!("{field} download port"))?;
            if let Some(origin_port) = download.origin_port {
                ensure_nonzero_port(origin_port, &format!("{field} download origin port"))?;
            }
            Some(ProjectionDownloadEndpoint {
                host,
                port: download.port,
                origin_port: download.origin_port,
                http_host: optional_owned_text(download.http_host),
                mux: download.mux,
            })
        }
        None => None,
    };
    Ok(Some(ProjectionEndpoint {
        host,
        port: endpoint.port,
        download,
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
