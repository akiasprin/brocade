use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr},
};

use brocade_core::client_config::ClientProjectionDownloadEndpoint;
use brocade_core::model::{
    Accept, AnyTls, AnyTlsMasquerade, AnyTlsSecurity, AppView, Chain, ConnectionSettings,
    DisabledWireGuardLink, Dns, ExternalOutbound, ExternalOutboundProtocol,
    ExternalOutboundSecurity, ExternalWarpBinding, Front, FrontStrategy, GeodataSettings, Grant,
    HopIn, HopMux, Hysteria2, HysteriaBandwidth, HysteriaBbrProfile, HysteriaCongestion,
    HysteriaMasquerade, HysteriaObfs, HysteriaPortHop, HysteriaQuic, Ingress, IngressGuard,
    IngressIdentity, IngressWires, IngressWiresWire, ModelSettings, ModelSnapshot, Node,
    NodeConnection, OverlaySettings, PortSettings, ProbeSettings, Projection,
    ProjectionDownloadEndpoint, ProjectionEndpoint, RealityClientPolicy, RealityFallbackLimits,
    RealityFallbackMode, RealitySettings, RealitySite, RealityXhttp, Rule, Step, Tls, TlsXhttp,
    Transport, User, WireGuardKeys, Xhttp, XhttpMode,
};
use ipnet::Ipv4Net;
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{Result, StoreError};

pub async fn current_revision(pool: &PgPool) -> Result<u64> {
    let row = sqlx::query("SELECT current_revision FROM control_state WHERE id = TRUE")
        .fetch_one(pool)
        .await?;
    revision_to_u64(row.try_get::<i64, _>("current_revision")?)
}

pub async fn load_snapshot(pool: &PgPool, revision: Option<u64>) -> Result<ModelSnapshot> {
    let current = current_revision(pool).await?;
    let revision = revision.unwrap_or(current);

    if revision == current {
        return load_current_snapshot(pool).await;
    }

    if let Some(snapshot) = load_stored_snapshot(pool, revision).await? {
        return Ok(snapshot);
    }

    let exists = sqlx::query("SELECT 1 FROM revisions WHERE id = $1")
        .bind(revision_to_i64(revision)?)
        .fetch_optional(pool)
        .await?
        .is_some();
    if exists {
        Err(StoreError::Unsupported(format!(
            "model snapshot for historical revision {revision} is not available"
        )))
    } else {
        Err(StoreError::NotFound(format!("revision {revision}")))
    }
}

/// The transaction-scoped counterpart to `load_snapshot`.
///
/// Write paths must not hold one pool connection in a transaction and then ask the pool for a
/// second one merely to read a snapshot. Besides making the reads disagree under concurrency,
/// enough simultaneous writers exhaust the pool with every connection waiting for another.
pub(crate) async fn load_snapshot_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision: Option<u64>,
) -> Result<ModelSnapshot> {
    let current = current_revision_tx(tx).await?;
    let revision = revision.unwrap_or(current);

    if revision == current {
        return load_current_snapshot_tx(tx).await;
    }

    let row = sqlx::query("SELECT snapshot FROM model_snapshots WHERE revision_id = $1")
        .bind(revision_to_i64(revision)?)
        .fetch_optional(&mut **tx)
        .await?;
    if let Some(row) = row {
        return decode_stored_snapshot(row.try_get("snapshot")?, revision);
    }

    let exists = sqlx::query("SELECT 1 FROM revisions WHERE id = $1")
        .bind(revision_to_i64(revision)?)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    if exists {
        Err(StoreError::Unsupported(format!(
            "model snapshot for historical revision {revision} is not available"
        )))
    } else {
        Err(StoreError::NotFound(format!("revision {revision}")))
    }
}

pub async fn ensure_current_snapshot(pool: &PgPool) -> Result<()> {
    let mut tx = pool.begin().await?;
    let current = current_revision_tx(&mut tx).await?;
    let exists = sqlx::query("SELECT 1 FROM model_snapshots WHERE revision_id = $1")
        .bind(revision_to_i64(current)?)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
    if !exists {
        store_current_snapshot_tx(&mut tx, current).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn store_current_snapshot_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
) -> Result<()> {
    let snapshot = load_current_snapshot_tx(tx).await?;
    if snapshot.revision != revision_id {
        return Err(StoreError::InvalidData(format!(
            "cannot store snapshot for revision {revision_id}; control_state points at {}",
            snapshot.revision
        )));
    }
    let mut value = serde_json::to_value(&snapshot)?;
    seal_snapshot_external_credentials(&mut value)?;
    sqlx::query(
        "INSERT INTO model_snapshots (revision_id, snapshot)
         VALUES ($1, $2)
         ON CONFLICT (revision_id) DO UPDATE SET
            snapshot = EXCLUDED.snapshot",
    )
    .bind(revision_to_i64(revision_id)?)
    .bind(value)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn load_current_snapshot(pool: &PgPool) -> Result<ModelSnapshot> {
    let state = sqlx::query(
        "SELECT current_revision,
            overlay_cidr::text AS overlay_cidr,
            reality_min_client_ver,
            reality_max_client_ver,
            reality_max_time_diff_ms, \
            overlay_keepalive_secs, overlay_mtu, overlay_disabled_links, \
            reality_dest, \
            reality_server_names, \
            reality_fingerprint, \
            reality_flow, \
            anytls_padding_scheme, \
            port_ingress_base, port_anytls_base, port_vless_encryption_base, port_hop_base, port_hy2_base, \
            probe_endpoint_url, probe_timeout_secs, probe_interval_secs, \
            geodata_cron, geodata_geoip_url, geodata_geosite_url, \
            conn_idle_secs, conn_uplink_only_secs, conn_downlink_only_secs, \
            conn_buffer_size_kb, conn_handshake_secs, stats_user_online, \
            reverse_health, reverse_health_overrides, relay_mux_concurrency, relay_mux_prewarm_workers, relay_mux_reuse_threshold, \
            relay_mux_max_probing_workers, relay_mux_probe_interval_ms, \
            relay_mux_probe_timeout_ms, relay_mux_idle_ttl_ms, relay_mux_max_requests_per_worker \
         FROM control_state WHERE id = TRUE",
    )
    .fetch_one(pool)
    .await?;

    let revision = revision_to_u64(state.try_get::<i64, _>("current_revision")?)?;
    let overlay_cidr = text(&state, "overlay_cidr")?.parse::<Ipv4Net>()?;
    let settings = ModelSettings {
        reality_client: RealityClientPolicy {
            min_client_ver: state.try_get("reality_min_client_ver")?,
            max_client_ver: state.try_get("reality_max_client_ver")?,
            max_time_diff_ms: optional_u64(
                "control_state.reality_max_time_diff_ms",
                state.try_get("reality_max_time_diff_ms")?,
            )?,
        },
        reality_site: RealitySite {
            dest: state.try_get("reality_dest")?,
            server_names: json_string_array(
                "control_state.reality_server_names",
                &state.try_get::<Value, _>("reality_server_names")?,
            )?,
            fingerprint: state.try_get("reality_fingerprint")?,
            flow: state.try_get("reality_flow")?,
        },
        anytls_padding_scheme: json_string_array(
            "control_state.anytls_padding_scheme",
            &state.try_get::<Value, _>("anytls_padding_scheme")?,
        )?,
        overlay: OverlaySettings {
            keepalive_secs: u16_column(
                "control_state.overlay_keepalive_secs",
                state.try_get("overlay_keepalive_secs")?,
            )?,
            mtu: u16_column("control_state.overlay_mtu", state.try_get("overlay_mtu")?)?,
            disabled_links: disabled_wireguard_links(
                "control_state.overlay_disabled_links",
                &state.try_get::<Value, _>("overlay_disabled_links")?,
            )?,
        },
        ports: PortSettings {
            vless_encryption_base: u16_column(
                "control_state.port_vless_encryption_base",
                state.try_get("port_vless_encryption_base")?,
            )?,
            ingress_base: u16_column(
                "control_state.port_ingress_base",
                state.try_get("port_ingress_base")?,
            )?,
            anytls_base: u16_column(
                "control_state.port_anytls_base",
                state.try_get("port_anytls_base")?,
            )?,
            hop_base: u16_column(
                "control_state.port_hop_base",
                state.try_get("port_hop_base")?,
            )?,
            hy2_base: u16_column(
                "control_state.port_hy2_base",
                state.try_get("port_hy2_base")?,
            )?,
        },
        probe: ProbeSettings {
            endpoint_url: state.try_get("probe_endpoint_url")?,
            timeout_secs: u16_column(
                "control_state.probe_timeout_secs",
                state.try_get("probe_timeout_secs")?,
            )?,
            interval_secs: u32::try_from(state.try_get::<i32, _>("probe_interval_secs")?)
                .map_err(|_| invalid_error("control_state.probe_interval_secs 是负数"))?,
        },
        connection: ConnectionSettings {
            conn_idle_secs: u32_column(
                "control_state.conn_idle_secs",
                state.try_get("conn_idle_secs")?,
            )?,
            uplink_only_secs: u32_column(
                "control_state.conn_uplink_only_secs",
                state.try_get("conn_uplink_only_secs")?,
            )?,
            downlink_only_secs: u32_column(
                "control_state.conn_downlink_only_secs",
                state.try_get("conn_downlink_only_secs")?,
            )?,
            buffer_size_kb: state
                .try_get::<Option<i32>, _>("conn_buffer_size_kb")?
                .map(|kb| u32_column("control_state.conn_buffer_size_kb", kb))
                .transpose()?,
            handshake_secs: u32_column(
                "control_state.conn_handshake_secs",
                state.try_get("conn_handshake_secs")?,
            )?,
        },
        relay_mux: hop_mux_from_row(&state)?,
        reverse_health: serde_json::from_value(state.try_get("reverse_health")?)?,
        reverse_health_overrides: serde_json::from_value(
            state.try_get("reverse_health_overrides")?,
        )?,
        stats_user_online: state.try_get("stats_user_online")?,
        geodata: GeodataSettings {
            cron: state.try_get("geodata_cron")?,
            geoip_url: state.try_get("geodata_geoip_url")?,
            geosite_url: state.try_get("geodata_geosite_url")?,
        },
    };

    let site = settings.reality_site.clone();
    let apps = load_apps(pool, &site).await?;
    let egress_dns = load_node_egress_dns(pool).await?;
    Ok(ModelSnapshot {
        revision,
        overlay_cidr,
        settings,
        nodes: load_nodes(pool).await?,
        node_egress_dns: crate::egress_dns::model_policies(&egress_dns),
        users: load_users(pool).await?,
        external_outbounds: load_external_outbounds(pool).await?,
        apps,
    })
}

/// The immutable JSON already written for the current revision, fetched in one round trip.
/// Agent work-list GETs need a coherent published model, not six live-table scans per node.
pub(crate) async fn load_current_immutable_snapshot(pool: &PgPool) -> Result<ModelSnapshot> {
    let row = sqlx::query(
        "SELECT state.current_revision, snapshots.snapshot
           FROM control_state AS state
           JOIN model_snapshots AS snapshots ON snapshots.revision_id = state.current_revision
          WHERE state.id = TRUE",
    )
    .fetch_one(pool)
    .await?;
    let revision = revision_to_u64(row.try_get::<i64, _>("current_revision")?)?;
    decode_stored_snapshot(row.try_get("snapshot")?, revision)
}

async fn load_stored_snapshot(pool: &PgPool, revision: u64) -> Result<Option<ModelSnapshot>> {
    let row = sqlx::query("SELECT snapshot FROM model_snapshots WHERE revision_id = $1")
        .bind(revision_to_i64(revision)?)
        .fetch_optional(pool)
        .await?;
    row.map(|row| decode_stored_snapshot(row.try_get("snapshot")?, revision))
        .transpose()
}

/// Load only the immutable snapshot written at the revision boundary. Serving paths must not use
/// `load_snapshot`: for the current revision that convenience function intentionally rematerializes
/// live tables, which is useful to the console but would let an out-of-band table mutation change
/// a serving subscription without a successful deployment.
pub(crate) async fn load_immutable_snapshot(pool: &PgPool, revision: u64) -> Result<ModelSnapshot> {
    load_stored_snapshot(pool, revision).await?.ok_or_else(|| {
        StoreError::InvalidData(format!(
            "serving revision {revision} has no immutable model snapshot"
        ))
    })
}

/// Transaction-scoped immutable snapshot load for serving checkpoint composition. Unlike
/// `load_snapshot_tx`, this never rematerializes the current mutable tables.
pub(crate) async fn load_immutable_snapshot_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision: u64,
) -> Result<ModelSnapshot> {
    let row = sqlx::query("SELECT snapshot FROM model_snapshots WHERE revision_id = $1")
        .bind(revision_to_i64(revision)?)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(|row| decode_stored_snapshot(row.try_get("snapshot")?, revision))
        .transpose()?
        .ok_or_else(|| {
            StoreError::InvalidData(format!(
                "serving revision {revision} has no immutable model snapshot"
            ))
        })
}

fn decode_stored_snapshot(mut snapshot: Value, revision: u64) -> Result<ModelSnapshot> {
    open_snapshot_external_credentials(&mut snapshot)?;
    let snapshot = serde_json::from_value::<ModelSnapshot>(snapshot)?;
    if snapshot.revision != revision {
        return Err(StoreError::InvalidData(format!(
            "model_snapshots row {revision} contains snapshot revision {}",
            snapshot.revision
        )));
    }
    Ok(snapshot)
}

/// Historical snapshots remain fully compilable, but their proxy credentials must not turn the
/// JSONB history table into a plaintext secret archive. The model's tagged protocol enum places
/// the shared credential at `protocol.v.credential`; only that narrowly identified field is
/// transformed, leaving ordinary ids and labels untouched.
pub(crate) fn seal_snapshot_external_credentials(snapshot: &mut Value) -> Result<()> {
    transform_snapshot_external_credentials(snapshot, |context, credential| {
        crate::secrets::seal(context, credential)
    })
}

pub(crate) fn open_snapshot_external_credentials(snapshot: &mut Value) -> Result<()> {
    transform_snapshot_external_credentials(snapshot, |context, credential| {
        crate::secrets::open(context, credential)
    })
}

fn transform_snapshot_external_credentials(
    snapshot: &mut Value,
    transform: impl Fn(&str, &str) -> Result<String>,
) -> Result<()> {
    let Some(outbounds) = snapshot
        .get_mut("external_outbounds")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for outbound in outbounds {
        let tenant = outbound
            .get("tenant")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_error("external outbound snapshot is missing tenant"))?
            .to_owned();
        let id = outbound
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_error("external outbound snapshot is missing id"))?
            .to_owned();
        if let Some(credential) = outbound
            .pointer("/protocol/v/credential")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let transformed = transform(
                &crate::secrets::external_outbound_context(&tenant, &id),
                &credential,
            )?;
            *outbound.pointer_mut("/protocol/v/credential").unwrap() = Value::String(transformed);
        } else if outbound.pointer("/protocol/t").and_then(Value::as_str) != Some("warp") {
            return Err(invalid_error(format!(
                "external outbound snapshot {tenant}/{id} is missing credential"
            )));
        }

        if let Some(bindings) = outbound.get_mut("bindings").and_then(Value::as_array_mut) {
            for binding in bindings {
                let node = binding
                    .get("node")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        invalid_error(format!(
                            "external outbound snapshot {tenant}/{id} has a binding without node"
                        ))
                    })?
                    .to_owned();
                let private_key = binding
                    .get("private_key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        invalid_error(format!(
                            "external outbound snapshot {tenant}/{id}/{node} is missing private_key"
                        ))
                    })?
                    .to_owned();
                let transformed = transform(
                    &crate::secrets::external_outbound_binding_key_context(&tenant, &id, &node),
                    &private_key,
                )?;
                binding
                    .as_object_mut()
                    .unwrap()
                    .insert("private_key".to_owned(), Value::String(transformed));
            }
        }
    }
    Ok(())
}

async fn current_revision_tx(tx: &mut Transaction<'_, Postgres>) -> Result<u64> {
    let row = sqlx::query("SELECT current_revision FROM control_state WHERE id = TRUE")
        .fetch_one(&mut **tx)
        .await?;
    revision_to_u64(row.try_get::<i64, _>("current_revision")?)
}

pub(crate) async fn load_current_snapshot_tx(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<ModelSnapshot> {
    let state = sqlx::query(
        "SELECT current_revision,
            overlay_cidr::text AS overlay_cidr,
            reality_min_client_ver,
            reality_max_client_ver,
            reality_max_time_diff_ms, \
            overlay_keepalive_secs, overlay_mtu, overlay_disabled_links, \
            reality_dest, \
            reality_server_names, \
            reality_fingerprint, \
            reality_flow, \
            anytls_padding_scheme, \
            port_ingress_base, port_anytls_base, port_vless_encryption_base, port_hop_base, port_hy2_base, \
            probe_endpoint_url, probe_timeout_secs, probe_interval_secs, \
            geodata_cron, geodata_geoip_url, geodata_geosite_url, \
            conn_idle_secs, conn_uplink_only_secs, conn_downlink_only_secs, \
            conn_buffer_size_kb, conn_handshake_secs, stats_user_online, \
            reverse_health, reverse_health_overrides, relay_mux_concurrency, relay_mux_prewarm_workers, relay_mux_reuse_threshold, \
            relay_mux_max_probing_workers, relay_mux_probe_interval_ms, \
            relay_mux_probe_timeout_ms, relay_mux_idle_ttl_ms, relay_mux_max_requests_per_worker \
         FROM control_state WHERE id = TRUE",
    )
    .fetch_one(&mut **tx)
    .await?;

    let revision = revision_to_u64(state.try_get::<i64, _>("current_revision")?)?;
    let overlay_cidr = text(&state, "overlay_cidr")?.parse::<Ipv4Net>()?;
    let settings = ModelSettings {
        reality_client: RealityClientPolicy {
            min_client_ver: state.try_get("reality_min_client_ver")?,
            max_client_ver: state.try_get("reality_max_client_ver")?,
            max_time_diff_ms: optional_u64(
                "control_state.reality_max_time_diff_ms",
                state.try_get("reality_max_time_diff_ms")?,
            )?,
        },
        reality_site: RealitySite {
            dest: state.try_get("reality_dest")?,
            server_names: json_string_array(
                "control_state.reality_server_names",
                &state.try_get::<Value, _>("reality_server_names")?,
            )?,
            fingerprint: state.try_get("reality_fingerprint")?,
            flow: state.try_get("reality_flow")?,
        },
        anytls_padding_scheme: json_string_array(
            "control_state.anytls_padding_scheme",
            &state.try_get::<Value, _>("anytls_padding_scheme")?,
        )?,
        overlay: OverlaySettings {
            keepalive_secs: u16_column(
                "control_state.overlay_keepalive_secs",
                state.try_get("overlay_keepalive_secs")?,
            )?,
            mtu: u16_column("control_state.overlay_mtu", state.try_get("overlay_mtu")?)?,
            disabled_links: disabled_wireguard_links(
                "control_state.overlay_disabled_links",
                &state.try_get::<Value, _>("overlay_disabled_links")?,
            )?,
        },
        ports: PortSettings {
            vless_encryption_base: u16_column(
                "control_state.port_vless_encryption_base",
                state.try_get("port_vless_encryption_base")?,
            )?,
            ingress_base: u16_column(
                "control_state.port_ingress_base",
                state.try_get("port_ingress_base")?,
            )?,
            anytls_base: u16_column(
                "control_state.port_anytls_base",
                state.try_get("port_anytls_base")?,
            )?,
            hop_base: u16_column(
                "control_state.port_hop_base",
                state.try_get("port_hop_base")?,
            )?,
            hy2_base: u16_column(
                "control_state.port_hy2_base",
                state.try_get("port_hy2_base")?,
            )?,
        },
        probe: ProbeSettings {
            endpoint_url: state.try_get("probe_endpoint_url")?,
            timeout_secs: u16_column(
                "control_state.probe_timeout_secs",
                state.try_get("probe_timeout_secs")?,
            )?,
            interval_secs: u32::try_from(state.try_get::<i32, _>("probe_interval_secs")?)
                .map_err(|_| invalid_error("control_state.probe_interval_secs 是负数"))?,
        },
        connection: ConnectionSettings {
            conn_idle_secs: u32_column(
                "control_state.conn_idle_secs",
                state.try_get("conn_idle_secs")?,
            )?,
            uplink_only_secs: u32_column(
                "control_state.conn_uplink_only_secs",
                state.try_get("conn_uplink_only_secs")?,
            )?,
            downlink_only_secs: u32_column(
                "control_state.conn_downlink_only_secs",
                state.try_get("conn_downlink_only_secs")?,
            )?,
            buffer_size_kb: state
                .try_get::<Option<i32>, _>("conn_buffer_size_kb")?
                .map(|kb| u32_column("control_state.conn_buffer_size_kb", kb))
                .transpose()?,
            handshake_secs: u32_column(
                "control_state.conn_handshake_secs",
                state.try_get("conn_handshake_secs")?,
            )?,
        },
        relay_mux: hop_mux_from_row(&state)?,
        reverse_health: serde_json::from_value(state.try_get("reverse_health")?)?,
        reverse_health_overrides: serde_json::from_value(
            state.try_get("reverse_health_overrides")?,
        )?,
        stats_user_online: state.try_get("stats_user_online")?,
        geodata: GeodataSettings {
            cron: state.try_get("geodata_cron")?,
            geoip_url: state.try_get("geodata_geoip_url")?,
            geosite_url: state.try_get("geodata_geosite_url")?,
        },
    };

    let site = settings.reality_site.clone();
    let apps = load_apps_tx(tx, &site).await?;
    let egress_dns = load_node_egress_dns_tx(tx).await?;
    Ok(ModelSnapshot {
        revision,
        overlay_cidr,
        settings,
        nodes: load_nodes_tx(tx).await?,
        node_egress_dns: crate::egress_dns::model_policies(&egress_dns),
        users: load_users_tx(tx).await?,
        external_outbounds: load_external_outbounds_tx(tx).await?,
        apps,
    })
}

async fn load_node_egress_dns(pool: &PgPool) -> Result<Vec<crate::egress_dns::StoredPolicy>> {
    let rows = sqlx::query(
        "SELECT node_id, position, selector, resolution
         FROM node_egress_dns
         ORDER BY node_id, position, selector",
    )
    .fetch_all(pool)
    .await?;
    decode_node_egress_dns(rows)
}

async fn load_node_egress_dns_tx(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<crate::egress_dns::StoredPolicy>> {
    let rows = sqlx::query(
        "SELECT node_id, position, selector, resolution
         FROM node_egress_dns
         ORDER BY node_id, position, selector",
    )
    .fetch_all(&mut **tx)
    .await?;
    decode_node_egress_dns(rows)
}

fn decode_node_egress_dns(
    rows: Vec<sqlx::postgres::PgRow>,
) -> Result<Vec<crate::egress_dns::StoredPolicy>> {
    rows.into_iter()
        .map(|row| {
            let node_id = text(&row, "node_id")?;
            let position = u32::try_from(row.try_get::<i32, _>("position")?).map_err(|_| {
                invalid_error(format!(
                    "node_egress_dns.position 超出范围（node {node_id}）"
                ))
            })?;
            let selector =
                serde_json::from_value(row.try_get::<Value, _>("selector")?).map_err(|error| {
                    invalid_error(format!(
                        "node_egress_dns.selector 解不开（node {node_id}）: {error}"
                    ))
                })?;
            let resolution = serde_json::from_value(row.try_get::<Value, _>("resolution")?)
                .map_err(|error| {
                    invalid_error(format!(
                        "node_egress_dns.resolution 解不开（node {node_id}）: {error}"
                    ))
                })?;
            Ok((node_id, position, selector, resolution))
        })
        .collect()
}

async fn load_nodes(pool: &PgPool) -> Result<Vec<Node>> {
    let rows = sqlx::query(
        "SELECT id, tenant_id, name, public_ipv4, public_ipv6, public_ipv4_nat, public_ipv6_nat, overlay_addr::text AS overlay_addr, \
            wg_private_key, wg_public_key, wg_listen_port, api_port, \
            overlay, egress_allowed, dns_kind, dns_servers, domain_strategy, mtu, wg_transport, \
            conn_idle_secs, conn_uplink_only_secs, conn_downlink_only_secs, conn_buffer_size_kb, \
            (retired_at IS NOT NULL) AS retired, \
            -- The machine's SNI: its group's name, and only once that group actually serves a
            -- certificate. A group with nothing serving yields NULL here, which is what makes
            -- `ingress.tls-no-certificate` fire instead of publishing an ingress whose every
            -- connection would fail at the TLS handshake.
            (SELECT label_id FROM node_cert_label WHERE node_id = nodes.id) AS certificate_group_id, \
            (SELECT COALESCE(c.certificate_name, l.certificate_name, l.label || '.' || d.domain) \
               FROM node_cert_label m \
               JOIN cert_labels l ON l.id = m.label_id \
               JOIN cert_domains d ON d.id = l.domain_id \
               JOIN certificates c ON c.label_id = l.id AND c.status = 'serving' \
              WHERE m.node_id = nodes.id
                AND c.expires_at > now()) AS certificate_name, \
            ARRAY(SELECT COALESCE(slot.certificate_name, l.certificate_name, l.label || '.' || d.domain) \
               FROM node_cert_label m \
               JOIN cert_labels l ON l.id = m.label_id \
               JOIN cert_domains d ON d.id = l.domain_id \
               JOIN certificates slot ON slot.label_id = l.id \
              WHERE m.node_id = nodes.id \
                AND slot.status IN ('ready', 'serving', 'compatible') \
                AND slot.expires_at > now() \
                AND (slot.acme_directory = 'self-signed') = ( \
                    SELECT serving.acme_directory = 'self-signed' \
                      FROM certificates serving \
                     WHERE serving.label_id = l.id AND serving.status = 'serving' \
                       AND serving.expires_at > now() \
                ) \
              ORDER BY CASE slot.status WHEN 'serving' THEN 0 WHEN 'compatible' THEN 1 ELSE 2 END, \
                       slot.runtime_slot, slot.id) AS certificate_names, \
            (SELECT CASE WHEN c.acme_directory = 'self-signed' THEN 'self-signed' ELSE 'public-ca' END \
               FROM node_cert_label m \
               JOIN certificates c ON c.label_id = m.label_id AND c.status = 'serving' \
              WHERE m.node_id = nodes.id AND c.expires_at > now()) AS certificate_track \
         FROM nodes \
         ORDER BY id",
    )
    .fetch_all(pool)
    .await?;

    rows.iter().map(node_from_row).collect()
}

async fn load_nodes_tx(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<Node>> {
    let rows = sqlx::query(
        "SELECT id, tenant_id, name, public_ipv4, public_ipv6, public_ipv4_nat, public_ipv6_nat, overlay_addr::text AS overlay_addr, \
            wg_private_key, wg_public_key, wg_listen_port, api_port, \
            overlay, egress_allowed, dns_kind, dns_servers, domain_strategy, mtu, wg_transport, \
            conn_idle_secs, conn_uplink_only_secs, conn_downlink_only_secs, conn_buffer_size_kb, \
            (retired_at IS NOT NULL) AS retired, \
            -- The machine's SNI: its group's name, and only once that group actually serves a
            -- certificate. A group with nothing serving yields NULL here, which is what makes
            -- `ingress.tls-no-certificate` fire instead of publishing an ingress whose every
            -- connection would fail at the TLS handshake.
            (SELECT label_id FROM node_cert_label WHERE node_id = nodes.id) AS certificate_group_id, \
            (SELECT COALESCE(c.certificate_name, l.certificate_name, l.label || '.' || d.domain) \
               FROM node_cert_label m \
               JOIN cert_labels l ON l.id = m.label_id \
               JOIN cert_domains d ON d.id = l.domain_id \
               JOIN certificates c ON c.label_id = l.id AND c.status = 'serving' \
              WHERE m.node_id = nodes.id
                AND c.expires_at > now()) AS certificate_name, \
            ARRAY(SELECT COALESCE(slot.certificate_name, l.certificate_name, l.label || '.' || d.domain) \
               FROM node_cert_label m \
               JOIN cert_labels l ON l.id = m.label_id \
               JOIN cert_domains d ON d.id = l.domain_id \
               JOIN certificates slot ON slot.label_id = l.id \
              WHERE m.node_id = nodes.id \
                AND slot.status IN ('ready', 'serving', 'compatible') \
                AND slot.expires_at > now() \
                AND (slot.acme_directory = 'self-signed') = ( \
                    SELECT serving.acme_directory = 'self-signed' \
                      FROM certificates serving \
                     WHERE serving.label_id = l.id AND serving.status = 'serving' \
                       AND serving.expires_at > now() \
                ) \
              ORDER BY CASE slot.status WHEN 'serving' THEN 0 WHEN 'compatible' THEN 1 ELSE 2 END, \
                       slot.runtime_slot, slot.id) AS certificate_names, \
            (SELECT CASE WHEN c.acme_directory = 'self-signed' THEN 'self-signed' ELSE 'public-ca' END \
               FROM node_cert_label m \
               JOIN certificates c ON c.label_id = m.label_id AND c.status = 'serving' \
              WHERE m.node_id = nodes.id AND c.expires_at > now()) AS certificate_track \
         FROM nodes \
         ORDER BY id",
    )
    .fetch_all(&mut **tx)
    .await?;

    rows.iter().map(node_from_row).collect()
}

fn node_from_row(row: &sqlx::postgres::PgRow) -> Result<Node> {
    let dns_kind = text(row, "dns_kind")?;
    let dns = match dns_kind.as_str() {
        "system" => Dns::System,
        "servers" => Dns::Servers(json_string_array(
            "nodes.dns_servers",
            &row.try_get::<Value, _>("dns_servers")?,
        )?),
        value => return invalid(format!("unknown dns kind {value}")),
    };

    let node_conn = |column: &str| -> Result<Option<u32>> {
        row.try_get::<Option<i32>, _>(column)?
            .map(|value| u32_column(column, value))
            .transpose()
    };
    let mut certificate_names = row.try_get::<Vec<String>, _>("certificate_names")?;
    let mut seen_certificate_names = std::collections::BTreeSet::new();
    certificate_names.retain(|name| seen_certificate_names.insert(name.clone()));
    Ok(Node {
        // Only a certificate that reached 'ready' counts. One still being issued, or whose last
        // attempt failed, is a name nothing answers to yet — and an ingress compiled against it
        // would hand out subscriptions naming a certificate that does not exist.
        certificate_name: row.try_get("certificate_name")?,
        certificate_names,
        certificate_group_id: row.try_get("certificate_group_id")?,
        certificate_track: match row
            .try_get::<Option<String>, _>("certificate_track")?
            .as_deref()
        {
            Some("public-ca") => Some(brocade_core::model::CertificateTrack::PublicCa),
            Some("self-signed") => Some(brocade_core::model::CertificateTrack::SelfSigned),
            Some(value) => return invalid(format!("unknown certificate track {value}")),
            None => None,
        },
        id: text(row, "id")?,
        tenant: text(row, "tenant_id")?,
        name: text(row, "name")?,
        public_ipv4: row.try_get("public_ipv4")?,
        public_ipv6: row.try_get("public_ipv6")?,
        public_ipv4_nat: row.try_get("public_ipv4_nat")?,
        public_ipv6_nat: row.try_get("public_ipv6_nat")?,
        overlay_addr: parse_ipv4(&text(row, "overlay_addr")?)?,
        wireguard: WireGuardKeys {
            private_key: text(row, "wg_private_key")?,
            public_key: text(row, "wg_public_key")?,
            listen_port: port(row.try_get::<i32, _>("wg_listen_port")?)?,
            // The shape is defined by WgTransport's serde; the database only uses a CHECK to
            // block unheard-of `t` values and low ports, while whether the material is
            // complete is reported here, and reported more clearly.
            transport: serde_json::from_value(row.try_get::<Value, _>("wg_transport")?).map_err(
                |error| {
                    StoreError::InvalidData(format!(
                        "nodes.wg_transport 解不开（node {}）: {error}",
                        row.try_get::<String, _>("id").unwrap_or_default()
                    ))
                },
            )?,
        },
        api_port: optional_port(row.try_get::<Option<i32>, _>("api_port")?)?,
        overlay: row.try_get("overlay")?,
        egress_allowed: row.try_get("egress_allowed")?,
        // A decommissioned node is materialized all the same: it still has to receive a
        // desired state turning all four configuration artifacts off. See the note in model.rs.
        retired: row.try_get("retired")?,
        mtu: optional_port(row.try_get::<Option<i32>, _>("mtu")?)?,
        connection: NodeConnection {
            conn_idle_secs: node_conn("conn_idle_secs")?,
            uplink_only_secs: node_conn("conn_uplink_only_secs")?,
            downlink_only_secs: node_conn("conn_downlink_only_secs")?,
            buffer_size_kb: node_conn("conn_buffer_size_kb")?,
        },
        dns,
        // Stored as the serde spelling of `DomainStrategy`, so the column is decoded
        // by serde rather than by a match here: the CHECK constraint already blocks an
        // unheard-of value, and duplicating the variant list in Rust is one more place
        // to forget when a variant is added.
        domain_strategy: serde_json::from_value(Value::String(text(row, "domain_strategy")?))
            .map_err(|error| {
                StoreError::InvalidData(format!(
                    "nodes.domain_strategy 解不开（node {}）: {error}",
                    row.try_get::<String, _>("id").unwrap_or_default()
                ))
            })?,
    })
}

async fn load_users(pool: &PgPool) -> Result<Vec<User>> {
    let rows = sqlx::query(
        "SELECT tenant_id, id, uuid::text AS uuid \
         FROM users \
         WHERE status = 'active' \
         ORDER BY tenant_id, id",
    )
    .fetch_all(pool)
    .await?;

    rows.iter().map(user_from_row).collect()
}

async fn load_users_tx(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<User>> {
    let rows = sqlx::query(
        "SELECT tenant_id, id, uuid::text AS uuid \
         FROM users \
         WHERE status = 'active' \
         ORDER BY tenant_id, id",
    )
    .fetch_all(&mut **tx)
    .await?;

    rows.iter().map(user_from_row).collect()
}

fn user_from_row(row: &sqlx::postgres::PgRow) -> Result<User> {
    Ok(User {
        id: text(row, "id")?,
        tenant: text(row, "tenant_id")?,
        uuid: text(row, "uuid")?,
    })
}

async fn load_external_outbounds(pool: &PgPool) -> Result<Vec<ExternalOutbound>> {
    let rows = sqlx::query(
        "SELECT id, tenant_id, name, address, port, protocol, credential_sealed,
                protocol_options, security
         FROM external_outbounds
         ORDER BY tenant_id, id",
    )
    .fetch_all(pool)
    .await?;
    let bindings = load_external_outbound_bindings(pool).await?;
    rows.iter()
        .map(|row| external_outbound_from_row(row, &bindings))
        .collect()
}

async fn load_external_outbounds_tx(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<ExternalOutbound>> {
    let rows = sqlx::query(
        "SELECT id, tenant_id, name, address, port, protocol, credential_sealed,
                protocol_options, security
         FROM external_outbounds
         ORDER BY tenant_id, id",
    )
    .fetch_all(&mut **tx)
    .await?;
    let bindings = load_external_outbound_bindings_tx(tx).await?;
    rows.iter()
        .map(|row| external_outbound_from_row(row, &bindings))
        .collect()
}

fn external_outbound_from_row(
    row: &sqlx::postgres::PgRow,
    bindings: &[(String, ExternalWarpBinding)],
) -> Result<ExternalOutbound> {
    let id = text(row, "id")?;
    let tenant = text(row, "tenant_id")?;
    let stored_protocol = text(row, "protocol")?;
    // WARP credentials belong to each machine binding, not to the tenant resource. Its
    // resource-level credential is therefore an empty sentinel.
    let credential = if stored_protocol == "warp" {
        String::new()
    } else {
        crate::secrets::open(
            &crate::secrets::external_outbound_context(&tenant, &id),
            &text(row, "credential_sealed")?,
        )?
    };
    let options = row.try_get::<Value, _>("protocol_options")?;
    let protocol = match stored_protocol.as_str() {
        "anytls" => ExternalOutboundProtocol::Anytls { credential },
        "vless" => ExternalOutboundProtocol::Vless {
            credential,
            encryption: options
                .get("encryption")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    invalid_error(format!(
                        "external_outbounds.protocol_options 缺少 encryption（{tenant}/{id}）"
                    ))
                })?
                .to_owned(),
            flow: options
                .get("flow")
                .and_then(Value::as_str)
                .map(str::to_owned),
            transport: options
                .get("transport")
                .cloned()
                .ok_or_else(|| {
                    invalid_error(format!(
                        "external_outbounds.protocol_options 缺少 transport（{tenant}/{id}）"
                    ))
                })
                .and_then(|value| serde_json::from_value(value).map_err(StoreError::from))
                .map_err(|error| {
                    invalid_error(format!(
                        "external_outbounds.protocol_options.transport 无效（{tenant}/{id}）：{error}"
                    ))
                })?,
        },
        "shadowsocks2022" => ExternalOutboundProtocol::Shadowsocks2022 {
            credential,
            method: options
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    invalid_error(format!(
                        "external_outbounds.protocol_options 缺少 method（{tenant}/{id}）"
                    ))
                })?
                .to_owned(),
        },
        "socks5" => ExternalOutboundProtocol::Socks5 {
            credential,
            username: options
                .get("username")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        "http_connect" => ExternalOutboundProtocol::HttpConnect {
            credential,
            username: options
                .get("username")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        "wireguard" => ExternalOutboundProtocol::Wireguard {
            credential,
            peer_public_key: external_option_string(&options, "peer_public_key", &tenant, &id)?,
            local_addresses: external_option_string_vec(&options, "local_addresses", &tenant, &id)?,
            mtu: external_option_u16(&options, "mtu", &tenant, &id)?,
            reserved: serde_json::from_value(options.get("reserved").cloned().ok_or_else(
                || {
                    invalid_error(format!(
                        "external_outbounds.protocol_options 缺少 reserved（{tenant}/{id}）"
                    ))
                },
            )?)?,
            keep_alive: external_option_u16(&options, "keep_alive", &tenant, &id)?,
            allowed_ips: external_option_string_vec(&options, "allowed_ips", &tenant, &id)?,
            no_kernel_tun: options
                .get("no_kernel_tun")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    invalid_error(format!(
                        "external_outbounds.protocol_options 缺少 no_kernel_tun（{tenant}/{id}）"
                    ))
                })?,
            domain_strategy: external_option_string(&options, "domain_strategy", &tenant, &id)?,
        },
        "warp" => ExternalOutboundProtocol::Warp {
            mtu: external_option_u16(&options, "mtu", &tenant, &id)?,
            keep_alive: external_option_u16(&options, "keep_alive", &tenant, &id)?,
            allowed_ips: external_option_string_vec(&options, "allowed_ips", &tenant, &id)?,
            no_kernel_tun: options
                .get("no_kernel_tun")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    invalid_error(format!(
                        "external_outbounds.protocol_options 缺少 no_kernel_tun（{tenant}/{id}）"
                    ))
                })?,
            domain_strategy: external_option_string(&options, "domain_strategy", &tenant, &id)?,
            workers: external_option_u16(&options, "workers", &tenant, &id)?,
        },
        protocol => return invalid(format!("unknown external outbound protocol {protocol}")),
    };
    let security =
        serde_json::from_value::<ExternalOutboundSecurity>(row.try_get::<Value, _>("security")?)?;
    let bindings = bindings
        .iter()
        .filter(|(binding_outbound, _)| binding_outbound == &id)
        .map(|(_, binding)| binding.clone())
        .collect();
    Ok(ExternalOutbound {
        id,
        tenant,
        name: text(row, "name")?,
        address: text(row, "address")?,
        port: u16_column("external_outbounds.port", row.try_get("port")?)?,
        protocol,
        security,
        bindings,
    })
}

async fn load_external_outbound_bindings(
    pool: &PgPool,
) -> Result<Vec<(String, ExternalWarpBinding)>> {
    let rows = sqlx::query(
        "SELECT external_outbound_bindings.outbound_id, external_outbounds.tenant_id,
                node_id, device_id, account_id, private_key_sealed,
                peer_public_key, local_addresses, reserved,
                endpoint_address, endpoint_port, mtu, keep_alive, allowed_ips,
                no_kernel_tun, domain_strategy, workers,
                to_char(registered_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS registered_at
         FROM external_outbound_bindings
         JOIN external_outbounds ON external_outbounds.id = external_outbound_bindings.outbound_id
         ORDER BY external_outbound_bindings.outbound_id, node_id",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(external_outbound_binding_from_row)
        .collect()
}

async fn load_external_outbound_bindings_tx(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<(String, ExternalWarpBinding)>> {
    let rows = sqlx::query(
        "SELECT external_outbound_bindings.outbound_id, external_outbounds.tenant_id,
                node_id, device_id, account_id, private_key_sealed,
                peer_public_key, local_addresses, reserved,
                endpoint_address, endpoint_port, mtu, keep_alive, allowed_ips,
                no_kernel_tun, domain_strategy, workers,
                to_char(registered_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS registered_at
         FROM external_outbound_bindings
         JOIN external_outbounds ON external_outbounds.id = external_outbound_bindings.outbound_id
         ORDER BY external_outbound_bindings.outbound_id, node_id",
    )
    .fetch_all(&mut **tx)
    .await?;
    rows.iter()
        .map(external_outbound_binding_from_row)
        .collect()
}

fn external_outbound_binding_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<(String, ExternalWarpBinding)> {
    let tenant = text(row, "tenant_id")?;
    let outbound = text(row, "outbound_id")?;
    let node = text(row, "node_id")?;
    let private_key = crate::secrets::open(
        &crate::secrets::external_outbound_binding_key_context(&tenant, &outbound, &node),
        &text(row, "private_key_sealed")?,
    )?;
    Ok((
        outbound,
        ExternalWarpBinding {
            node,
            device_id: text(row, "device_id")?,
            account_id: text(row, "account_id")?,
            registered_at: text(row, "registered_at")?,
            endpoint_address: row.try_get("endpoint_address")?,
            endpoint_port: row
                .try_get::<Option<i32>, _>("endpoint_port")?
                .map(|value| u16_column("external_outbound_bindings.endpoint_port", value))
                .transpose()?,
            mtu: row
                .try_get::<Option<i32>, _>("mtu")?
                .map(|value| u16_column("external_outbound_bindings.mtu", value))
                .transpose()?,
            keep_alive: row
                .try_get::<Option<i32>, _>("keep_alive")?
                .map(|value| u16_column("external_outbound_bindings.keep_alive", value))
                .transpose()?,
            allowed_ips: row
                .try_get::<Option<Value>, _>("allowed_ips")?
                .map(serde_json::from_value)
                .transpose()?,
            no_kernel_tun: row.try_get("no_kernel_tun")?,
            domain_strategy: row.try_get("domain_strategy")?,
            workers: row
                .try_get::<Option<i32>, _>("workers")?
                .map(|value| u16_column("external_outbound_bindings.workers", value))
                .transpose()?,
            private_key,
            peer_public_key: text(row, "peer_public_key")?,
            local_addresses: serde_json::from_value(row.try_get::<Value, _>("local_addresses")?)?,
            reserved: serde_json::from_value(row.try_get::<Value, _>("reserved")?)?,
        },
    ))
}

fn external_option_string(options: &Value, key: &str, tenant: &str, id: &str) -> Result<String> {
    options
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            invalid_error(format!(
                "external_outbounds.protocol_options 缺少 {key}（{tenant}/{id}）"
            ))
        })
}

fn external_option_string_vec(
    options: &Value,
    key: &str,
    tenant: &str,
    id: &str,
) -> Result<Vec<String>> {
    serde_json::from_value(options.get(key).cloned().ok_or_else(|| {
        invalid_error(format!(
            "external_outbounds.protocol_options 缺少 {key}（{tenant}/{id}）"
        ))
    })?)
    .map_err(Into::into)
}

fn external_option_u16(options: &Value, key: &str, tenant: &str, id: &str) -> Result<u16> {
    let value = options.get(key).and_then(Value::as_u64).ok_or_else(|| {
        invalid_error(format!(
            "external_outbounds.protocol_options 缺少 {key}（{tenant}/{id}）"
        ))
    })?;
    u16::try_from(value).map_err(|_| {
        invalid_error(format!(
            "external_outbounds.protocol_options 的 {key} 超出 u16（{tenant}/{id}）"
        ))
    })
}

async fn load_apps(pool: &PgPool, site: &RealitySite) -> Result<Vec<AppView>> {
    let rows = sqlx::query("SELECT id, label FROM apps ORDER BY position, id")
        .fetch_all(pool)
        .await?;
    let mut apps = Vec::with_capacity(rows.len());

    for row in rows {
        let app_id = text(&row, "id")?;
        apps.push(AppView {
            id: app_id.clone(),
            label: text(&row, "label")?,
            chains: load_chains(pool, &app_id).await?,
            ingresses: load_ingresses(pool, &app_id, site).await?,
            fronts: load_fronts(pool, &app_id).await?,
            steps: load_steps(pool, &app_id).await?,
            grants: load_grants(pool, &app_id).await?,
        });
    }

    Ok(apps)
}

async fn load_apps_tx(
    tx: &mut Transaction<'_, Postgres>,
    site: &RealitySite,
) -> Result<Vec<AppView>> {
    let rows = sqlx::query("SELECT id, label FROM apps ORDER BY position, id")
        .fetch_all(&mut **tx)
        .await?;
    let mut apps = Vec::with_capacity(rows.len());

    for row in rows {
        let app_id = text(&row, "id")?;
        apps.push(AppView {
            id: app_id.clone(),
            label: text(&row, "label")?,
            chains: load_chains_tx(tx, &app_id).await?,
            ingresses: load_ingresses_tx(tx, &app_id, site).await?,
            fronts: load_fronts_tx(tx, &app_id).await?,
            steps: load_steps_tx(tx, &app_id).await?,
            grants: load_grants_tx(tx, &app_id).await?,
        });
    }

    Ok(apps)
}

async fn load_chains(pool: &PgPool, app_id: &str) -> Result<Vec<Chain>> {
    let rows = sqlx::query(
        "SELECT id, tenant_id, name, subscription_country \
         FROM chains \
         WHERE app_id = $1 \
         ORDER BY position, id",
    )
    .bind(app_id)
    .fetch_all(pool)
    .await?;
    let mut chains = Vec::with_capacity(rows.len());

    for row in rows {
        let chain_id = text(&row, "id")?;
        chains.push(Chain {
            id: chain_id.clone(),
            tenant: text(&row, "tenant_id")?,
            name: text(&row, "name")?,
            subscription_country: row.try_get("subscription_country")?,
        });
    }

    Ok(chains)
}

async fn load_chains_tx(tx: &mut Transaction<'_, Postgres>, app_id: &str) -> Result<Vec<Chain>> {
    let rows = sqlx::query(
        "SELECT id, tenant_id, name, subscription_country \
         FROM chains \
         WHERE app_id = $1 \
         ORDER BY position, id",
    )
    .bind(app_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut chains = Vec::with_capacity(rows.len());

    for row in rows {
        let chain_id = text(&row, "id")?;
        chains.push(Chain {
            id: chain_id.clone(),
            tenant: text(&row, "tenant_id")?,
            name: text(&row, "name")?,
            subscription_country: row.try_get("subscription_country")?,
        });
    }

    Ok(chains)
}

async fn load_fronts(pool: &PgPool, app_id: &str) -> Result<Vec<Front>> {
    let rows = sqlx::query(
        "SELECT id, tenant_id, name, strategy \
         FROM fronts \
         WHERE app_id = $1 \
         ORDER BY id",
    )
    .bind(app_id)
    .fetch_all(pool)
    .await?;
    let mut fronts = Vec::with_capacity(rows.len());

    for row in rows {
        let front_id = text(&row, "id")?;
        fronts.push(Front {
            id: front_id.clone(),
            tenant: text(&row, "tenant_id")?,
            name: text(&row, "name")?,
            via: load_front_via(pool, &front_id).await?,
            external_via: load_front_external_via(pool, &front_id).await?,
            strategy: parse_front_strategy(&text(&row, "strategy")?)?,
        });
    }

    Ok(fronts)
}

async fn load_fronts_tx(tx: &mut Transaction<'_, Postgres>, app_id: &str) -> Result<Vec<Front>> {
    let rows = sqlx::query(
        "SELECT id, tenant_id, name, strategy \
         FROM fronts \
         WHERE app_id = $1 \
         ORDER BY id",
    )
    .bind(app_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut fronts = Vec::with_capacity(rows.len());

    for row in rows {
        let front_id = text(&row, "id")?;
        fronts.push(Front {
            id: front_id.clone(),
            tenant: text(&row, "tenant_id")?,
            name: text(&row, "name")?,
            via: load_front_via_tx(tx, &front_id).await?,
            external_via: load_front_external_via_tx(tx, &front_id).await?,
            strategy: parse_front_strategy(&text(&row, "strategy")?)?,
        });
    }

    Ok(fronts)
}

async fn load_front_via(pool: &PgPool, front_id: &str) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT ingress_id \
         FROM front_vias \
         WHERE front_id = $1 \
         ORDER BY ordinal, ingress_id",
    )
    .bind(front_id)
    .fetch_all(pool)
    .await?;

    rows.iter().map(|row| text(row, "ingress_id")).collect()
}

async fn load_front_via_tx(
    tx: &mut Transaction<'_, Postgres>,
    front_id: &str,
) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT ingress_id \
         FROM front_vias \
         WHERE front_id = $1 \
         ORDER BY ordinal, ingress_id",
    )
    .bind(front_id)
    .fetch_all(&mut **tx)
    .await?;

    rows.iter().map(|row| text(row, "ingress_id")).collect()
}

async fn load_front_external_via(pool: &PgPool, front_id: &str) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT outbound_id
         FROM front_external_vias
         WHERE front_id = $1
         ORDER BY ordinal, outbound_id",
    )
    .bind(front_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(|row| text(row, "outbound_id")).collect()
}

async fn load_front_external_via_tx(
    tx: &mut Transaction<'_, Postgres>,
    front_id: &str,
) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT outbound_id
         FROM front_external_vias
         WHERE front_id = $1
         ORDER BY ordinal, outbound_id",
    )
    .bind(front_id)
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(|row| text(row, "outbound_id")).collect()
}

async fn load_ingresses(pool: &PgPool, app_id: &str, site: &RealitySite) -> Result<Vec<Ingress>> {
    let rows = sqlx::query(
        "SELECT id, chain_id, node_id, bind::text AS bind, port, front_id, \
            reality_private_key, reality_public_key, reality_short_ids, reality_dest, \
            reality_server_names, reality_fingerprint, reality_flow, \
            reality_fallback_mode, reality_fallback_limits, reality_fallback_guard, \
            hy2_port, hy2_hop_start, hy2_hop_end, \
            vless_encryption_port, vless_encryption_private_key, vless_encryption_public_key, vless_encryption_options, \
            transport_kind, anytls_enabled, anytls_security, anytls_reality, \
            anytls_reality_private_key, anytls_reality_public_key, anytls_reality_short_ids, \
            anytls_port, anytls_padding_scheme, \
            client.anytls_idle_session_check_interval, client.anytls_idle_session_timeout, \
            client.anytls_min_idle_session, \
            anytls_masquerade_kind, anytls_masquerade_content, anytls_masquerade_headers, anytls_masquerade_status_code, \
            hy2_enabled, xhttp_path, xhttp_host, xhttp_xmux, xhttp_tuning, xhttp_mode, \
            xhttp_download_v4_origin_port, xhttp_download_v6_origin_port, \
            hy2_up, hy2_down, hy2_congestion, hy2_obfs_password, \
            hy2_bbr_profile, \
            hy2_quic_init_stream_window, hy2_quic_max_stream_window, \
            hy2_quic_init_conn_window, hy2_quic_max_conn_window, \
            hy2_quic_max_idle_secs, hy2_quic_keepalive_secs, \
            hy2_quic_max_incoming_streams, hy2_quic_disable_pmtud, \
            hy2_masquerade_kind, hy2_masquerade_url, \
            guard_no_private, guard_no_bittorrent, guard_no_mail, \
            guard_no_udp_amplification, guard_tcp_and_quic_only, \
            projection_v4_host, projection_v4_port, \
            projection_v6_host, projection_v6_port, \
            client.xhttp_download_v4, client.xhttp_download_v6 \
         FROM ingresses \
         LEFT JOIN ingress_client_settings client ON client.ingress_id = ingresses.id \
         WHERE ingresses.app_id = $1 \
         ORDER BY (SELECT chains.position FROM chains WHERE chains.id = ingresses.chain_id), \
                  chain_id, id",
    )
    .bind(app_id)
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(|row| ingress_from_row_with_site(row, site))
        .collect()
}

async fn load_ingresses_tx(
    tx: &mut Transaction<'_, Postgres>,
    app_id: &str,
    site: &RealitySite,
) -> Result<Vec<Ingress>> {
    let rows = sqlx::query(
        "SELECT id, chain_id, node_id, bind::text AS bind, port, front_id, \
            reality_private_key, reality_public_key, reality_short_ids, reality_dest, \
            reality_server_names, reality_fingerprint, reality_flow, \
            reality_fallback_mode, reality_fallback_limits, reality_fallback_guard, \
            hy2_port, hy2_hop_start, hy2_hop_end, \
            vless_encryption_port, vless_encryption_private_key, vless_encryption_public_key, vless_encryption_options, \
            transport_kind, anytls_enabled, anytls_security, anytls_reality, \
            anytls_reality_private_key, anytls_reality_public_key, anytls_reality_short_ids, \
            anytls_port, anytls_padding_scheme, \
            client.anytls_idle_session_check_interval, client.anytls_idle_session_timeout, \
            client.anytls_min_idle_session, \
            anytls_masquerade_kind, anytls_masquerade_content, anytls_masquerade_headers, anytls_masquerade_status_code, \
            hy2_enabled, xhttp_path, xhttp_host, xhttp_xmux, xhttp_tuning, xhttp_mode, \
            xhttp_download_v4_origin_port, xhttp_download_v6_origin_port, \
            hy2_up, hy2_down, hy2_congestion, hy2_obfs_password, \
            hy2_bbr_profile, \
            hy2_quic_init_stream_window, hy2_quic_max_stream_window, \
            hy2_quic_init_conn_window, hy2_quic_max_conn_window, \
            hy2_quic_max_idle_secs, hy2_quic_keepalive_secs, \
            hy2_quic_max_incoming_streams, hy2_quic_disable_pmtud, \
            hy2_masquerade_kind, hy2_masquerade_url, \
            guard_no_private, guard_no_bittorrent, guard_no_mail, \
            guard_no_udp_amplification, guard_tcp_and_quic_only, \
            projection_v4_host, projection_v4_port, \
            projection_v6_host, projection_v6_port, \
            client.xhttp_download_v4, client.xhttp_download_v6 \
         FROM ingresses \
         LEFT JOIN ingress_client_settings client ON client.ingress_id = ingresses.id \
         WHERE ingresses.app_id = $1 \
         ORDER BY (SELECT chains.position FROM chains WHERE chains.id = ingresses.chain_id), \
                  chain_id, id",
    )
    .bind(app_id)
    .fetch_all(&mut **tx)
    .await?;

    rows.iter()
        .map(|row| ingress_from_row_with_site(row, site))
        .collect()
}

// An ingress's own site column is now nullable, and blank means following the global setting.
// It is filled in here, so that by the time a ModelSnapshot reaches the compiler every
// ingress's site is a concrete value — the model layer permits blanks, the IR has none.
fn ingress_from_row_with_site(row: &sqlx::postgres::PgRow, site: &RealitySite) -> Result<Ingress> {
    let dest = row
        .try_get::<Option<String>, _>("reality_dest")?
        .filter(|v| !v.trim().is_empty())
        .or_else(|| site.dest.clone())
        .unwrap_or_default();
    let mut server_names = json_string_array(
        "ingresses.reality_server_names",
        &row.try_get::<Value, _>("reality_server_names")?,
    )?;
    if server_names.is_empty() {
        server_names = site.server_names.clone();
    }
    let fingerprint = row
        .try_get::<Option<String>, _>("reality_fingerprint")?
        .filter(|v| !v.trim().is_empty())
        .or_else(|| site.fingerprint.clone())
        .unwrap_or_else(|| "chrome".to_owned());
    // flow differs from the other three: it has no fallback default. Blank really does mean
    // shipping no flow (plain VLESS over TLS) — a decision rather than missing configuration.
    //
    // So NULL and the empty string are *not* the same thing here, and conflating them used to make
    // one state unreachable. NULL is "nothing set on this ingress, follow the global"; the empty
    // string is "this ingress, specifically, has it off". Before the distinction existed the only
    // way to turn Vision off for one ingress was to turn it off for the whole fleet — and XHTTP
    // cannot run with Vision, so that was the difference between "one ingress on XHTTP" and
    // "every client's flow changed".
    //
    // A fresh database defaults the global value to on, so an ingress that has never been touched
    // still lands on Vision.
    let flow = match row.try_get::<Option<String>, _>("reality_flow")? {
        Some(value) if value.trim().is_empty() => None,
        Some(value) => Some(value),
        None => site.flow.clone(),
    };

    let identity = IngressIdentity {
        private_key: text(row, "reality_private_key")?,
        public_key: text(row, "reality_public_key")?,
        short_ids: json_string_array(
            "ingresses.reality_short_ids",
            &row.try_get::<Value, _>("reality_short_ids")?,
        )?,
    };
    let reality = RealitySettings {
        dest,
        server_names,
        fingerprint,
        flow,
        fallback_mode: match text(row, "reality_fallback_mode")?.as_str() {
            "global-site" => RealityFallbackMode::GlobalSite,
            "node-certificate" => RealityFallbackMode::NodeCertificate,
            "custom-site" => RealityFallbackMode::CustomSite,
            value => {
                return invalid(format!(
                    "ingresses.reality_fallback_mode has unknown value {value}"
                ));
            }
        },
        fallback_limits: serde_json::from_value::<RealityFallbackLimits>(
            row.try_get("reality_fallback_limits")?,
        )
        .map_err(|error| {
            StoreError::InvalidData(format!(
                "ingresses.reality_fallback_limits is invalid: {error}"
            ))
        })?,
        fallback_guard: row.try_get("reality_fallback_guard")?,
    };

    let anytls = if row.try_get::<bool, _>("anytls_enabled")? {
        let padding_scheme = json_string_array(
            "ingresses.anytls_padding_scheme",
            &row.try_get::<Value, _>("anytls_padding_scheme")?,
        )?;
        let headers = serde_json::from_value::<BTreeMap<String, String>>(
            row.try_get::<Value, _>("anytls_masquerade_headers")?,
        )
        .map_err(|error| {
            StoreError::InvalidData(format!(
                "ingresses.anytls_masquerade_headers is invalid: {error}"
            ))
        })?;
        let masquerade = match text(row, "anytls_masquerade_kind")?.as_str() {
            "string" => AnyTlsMasquerade::String {
                content: text(row, "anytls_masquerade_content")?,
                headers,
                status_code: {
                    let value = row.try_get::<i32, _>("anytls_masquerade_status_code")?;
                    if !(200..=599).contains(&value) {
                        return invalid(format!(
                            "ingresses.anytls_masquerade_status_code out of range: {value}"
                        ));
                    }
                    u16::try_from(value).map_err(|_| {
                        invalid_error(format!(
                            "ingresses.anytls_masquerade_status_code out of range: {value}"
                        ))
                    })?
                },
            },
            "404" => AnyTlsMasquerade::NotFound { headers },
            value => {
                return invalid(format!(
                    "ingresses.anytls_masquerade_kind has unknown value {value}"
                ));
            }
        };
        let security = match text(row, "anytls_security")?.as_str() {
            "tls" => AnyTlsSecurity::Tls,
            "reality" => AnyTlsSecurity::Reality,
            value => {
                return invalid(format!(
                    "ingresses.anytls_security has unknown value {value}"
                ));
            }
        };
        let reality = match security {
            AnyTlsSecurity::Tls => None,
            AnyTlsSecurity::Reality => Some(anytls_reality_from_row(row, site)?),
        };
        let raw_port = row
            .try_get::<Option<i32>, _>("anytls_port")?
            .ok_or_else(|| invalid_error("enabled AnyTLS ingress has no port"))?;
        Some(AnyTls {
            port: port(raw_port)?,
            security,
            reality,
            padding_scheme,
            idle_session_check_interval_secs: optional_u32_bigint(
                row,
                "anytls_idle_session_check_interval",
            )?,
            idle_session_timeout_secs: optional_u32_bigint(row, "anytls_idle_session_timeout")?,
            min_idle_session: optional_u32_bigint(row, "anytls_min_idle_session")?,
            masquerade,
        })
    } else {
        None
    };
    let tls = Tls {
        flow: reality.flow.clone(),
    };
    let hysteria2 = if row.try_get::<bool, _>("hy2_enabled")? {
        let raw_port = row
            .try_get::<Option<i32>, _>("hy2_port")?
            .ok_or_else(|| invalid_error("enabled Hysteria 2 ingress has no port"))?;
        let hop = match (
            row.try_get::<Option<i32>, _>("hy2_hop_start")?,
            row.try_get::<Option<i32>, _>("hy2_hop_end")?,
        ) {
            (Some(start), Some(end)) => Some(HysteriaPortHop {
                start: port(start)?,
                end: port(end)?,
            }),
            (None, None) => None,
            _ => return invalid("ingresses Hysteria 2 hop range is only partially populated"),
        };
        let congestion = match text(row, "hy2_congestion")?.as_str() {
            "brutal" => HysteriaCongestion::Brutal,
            "bbr" => HysteriaCongestion::Bbr,
            "reno" => HysteriaCongestion::Reno,
            "force-brutal" => HysteriaCongestion::ForceBrutal,
            value => {
                return invalid(format!(
                    "ingresses.hy2_congestion has unknown value {value}"
                ));
            }
        };
        let bbr_profile = match text(row, "hy2_bbr_profile")?.as_str() {
            "standard" => HysteriaBbrProfile::Standard,
            "conservative" => HysteriaBbrProfile::Conservative,
            "aggressive" => HysteriaBbrProfile::Aggressive,
            value => {
                return invalid(format!(
                    "ingresses.hy2_bbr_profile has unknown value {value}"
                ));
            }
        };
        let masquerade = match text(row, "hy2_masquerade_kind")?.as_str() {
            "not-found" => HysteriaMasquerade::NotFound,
            "proxy" => HysteriaMasquerade::Proxy {
                url: row
                    .try_get::<Option<String>, _>("hy2_masquerade_url")?
                    .ok_or_else(|| {
                        invalid_error("Hysteria 2 proxy masquerade has no target URL")
                    })?,
            },
            value => {
                return invalid(format!(
                    "ingresses.hy2_masquerade_kind has unknown value {value}"
                ));
            }
        };
        Some(Hysteria2 {
            port: port(raw_port)?,
            hop,
            bandwidth: HysteriaBandwidth {
                up: row.try_get("hy2_up")?,
                down: row.try_get("hy2_down")?,
            },
            congestion,
            bbr_profile,
            quic: HysteriaQuic {
                init_stream_receive_window: window(row, "hy2_quic_init_stream_window")?,
                max_stream_receive_window: window(row, "hy2_quic_max_stream_window")?,
                init_connection_receive_window: window(row, "hy2_quic_init_conn_window")?,
                max_connection_receive_window: window(row, "hy2_quic_max_conn_window")?,
                max_idle_timeout_secs: secs(row, "hy2_quic_max_idle_secs")?,
                keep_alive_period_secs: secs(row, "hy2_quic_keepalive_secs")?,
                max_incoming_streams: secs(row, "hy2_quic_max_incoming_streams")?,
                disable_path_mtu_discovery: row.try_get("hy2_quic_disable_pmtud")?,
            },
            obfs: row
                .try_get::<Option<String>, _>("hy2_obfs_password")?
                .map(|password| HysteriaObfs::Salamander { password }),
            masquerade,
        })
    } else {
        None
    };
    let vless = match row
        .try_get::<Option<String>, _>("transport_kind")?
        .as_deref()
    {
        None => None,
        Some("vless-reality") => Some(Transport::VlessReality(reality)),
        Some("vless-reality-xhttp") => Some(Transport::VlessRealityXhttp(RealityXhttp {
            reality,
            xhttp: xhttp_from_row(row)?,
        })),
        Some("vless-tls") => Some(Transport::VlessTls(tls)),
        Some("vless-tls-xhttp") => Some(Transport::VlessTlsXhttp(TlsXhttp {
            tls,
            xhttp: xhttp_from_row(row)?,
        })),
        Some(value) => {
            return invalid(format!(
                "ingresses.transport_kind has unknown value {value}"
            ));
        }
    };
    let wires = IngressWires::try_from(IngressWiresWire {
        vless_encryption: row
            .try_get::<Option<i32>, _>("vless_encryption_port")?
            .map(|port| -> Result<_> {
                Ok(brocade_core::model::VlessEncryption {
                    port: u16::try_from(port).map_err(|_| {
                        StoreError::InvalidData("invalid VLESS Encryption port".to_owned())
                    })?,
                    private_key: text(row, "vless_encryption_private_key")?,
                    public_key: text(row, "vless_encryption_public_key")?,
                    options: serde_json::from_value(row.try_get("vless_encryption_options")?)?,
                })
            })
            .transpose()?,
        vless,
        anytls,
        hysteria2,
    })
    .map_err(|error| StoreError::InvalidData(format!("ingresses 行没有任何一条线：{error}")))?;

    Ok(Ingress {
        id: text(row, "id")?,
        chain: text(row, "chain_id")?,
        node: text(row, "node_id")?,
        bind: parse_ip(&text(row, "bind")?)?,
        port: port(row.try_get::<i32, _>("port")?)?,
        front: row.try_get("front_id")?,
        identity,
        anytls_identity: anytls_identity_from_row(row)?,
        wires,
        projection: Projection {
            v4: projection_endpoint(row, "v4")?,
            v6: projection_endpoint(row, "v6")?,
        },
        guard: IngressGuard {
            no_private: row.try_get("guard_no_private")?,
            no_bittorrent: row.try_get("guard_no_bittorrent")?,
            no_mail: row.try_get("guard_no_mail")?,
            no_udp_amplification: row.try_get("guard_no_udp_amplification")?,
            tcp_and_quic_only: row.try_get("guard_tcp_and_quic_only")?,
        },
    })
}

fn xhttp_from_row(row: &sqlx::postgres::PgRow) -> Result<Xhttp> {
    let path = row
        .try_get::<Option<String>, _>("xhttp_path")?
        .ok_or_else(|| invalid_error("XHTTP ingress has no path"))?;
    let mode = match row.try_get::<Option<String>, _>("xhttp_mode")?.as_deref() {
        None => XhttpMode::Auto,
        Some("packet-up") => XhttpMode::PacketUp,
        Some("stream-up") => XhttpMode::StreamUp,
        Some("stream-one") => XhttpMode::StreamOne,
        Some(value) => {
            return invalid(format!("ingresses.xhttp_mode has unknown value {value}"));
        }
    };
    Ok(Xhttp {
        path,
        host: row.try_get("xhttp_host")?,
        xmux: row
            .try_get::<Option<Value>, _>("xhttp_xmux")?
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                StoreError::InvalidData(format!("ingresses.xhttp_xmux is invalid: {error}"))
            })?,
        tuning: row
            .try_get::<Option<Value>, _>("xhttp_tuning")?
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                StoreError::InvalidData(format!("ingresses.xhttp_tuning is invalid: {error}"))
            })?,
        mode,
        download: xhttp_download_from_row(row)?,
    })
}

fn anytls_identity_from_row(row: &sqlx::postgres::PgRow) -> Result<Option<IngressIdentity>> {
    let private_key = row.try_get::<Option<String>, _>("anytls_reality_private_key")?;
    let public_key = row.try_get::<Option<String>, _>("anytls_reality_public_key")?;
    let short_ids = row.try_get::<Option<Value>, _>("anytls_reality_short_ids")?;
    match (private_key, public_key, short_ids) {
        (None, None, None) => Ok(None),
        (Some(private_key), Some(public_key), Some(short_ids)) => Ok(Some(IngressIdentity {
            private_key,
            public_key,
            short_ids: json_string_array("ingresses.anytls_reality_short_ids", &short_ids)?,
        })),
        _ => Err(StoreError::InvalidData(
            "ingresses AnyTLS REALITY identity is only partially populated".to_owned(),
        )),
    }
}

/// Resolve AnyTLS's own REALITY target and apply the current global site where requested.
fn anytls_reality_from_row(
    row: &sqlx::postgres::PgRow,
    site: &RealitySite,
) -> Result<RealitySettings> {
    let stored = row
        .try_get::<Option<Value>, _>("anytls_reality")?
        .map(serde_json::from_value::<RealitySettings>)
        .transpose()
        .map_err(|error| {
            StoreError::InvalidData(format!("ingresses.anytls_reality is invalid: {error}"))
        })?;
    let mut reality = stored.ok_or_else(|| {
        StoreError::InvalidData("AnyTLS REALITY ingress has no target settings".to_owned())
    })?;
    if reality.fallback_mode == RealityFallbackMode::NodeCertificate {
        return Err(StoreError::InvalidData(
            "AnyTLS REALITY target must be global-site or custom-site".to_owned(),
        ));
    }
    if reality.fallback_mode == RealityFallbackMode::GlobalSite {
        reality.dest = site.dest.clone().unwrap_or_default();
        reality.server_names = site.server_names.clone();
        reality.fingerprint = site
            .fingerprint
            .clone()
            .unwrap_or_else(|| "chrome".to_owned());
    }
    // Flow is a VLESS account setting, not an AnyTLS stream setting.
    reality.flow = None;
    Ok(reality)
}

fn xhttp_download_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<Option<brocade_core::model::XhttpDownload>> {
    let v4 = xhttp_download_endpoint_from_row(row, "v4")?;
    let v6 = xhttp_download_endpoint_from_row(row, "v6")?;
    Ok((v4.is_some() || v6.is_some()).then_some(brocade_core::model::XhttpDownload { v4, v6 }))
}

fn xhttp_download_endpoint_from_row(
    row: &sqlx::postgres::PgRow,
    family: &str,
) -> Result<Option<ProjectionDownloadEndpoint>> {
    let client_column = format!("xhttp_download_{family}");
    let origin_column = format!("xhttp_download_{family}_origin_port");
    let Some(value) = row.try_get::<Option<Value>, _>(client_column.as_str())? else {
        return Ok(None);
    };
    let client =
        serde_json::from_value::<ClientProjectionDownloadEndpoint>(value).map_err(|error| {
            StoreError::InvalidData(format!(
                "ingress_client_settings.{client_column} is invalid: {error}"
            ))
        })?;
    Ok(Some(ProjectionDownloadEndpoint {
        host: client.host,
        port: client.port,
        origin_port: row
            .try_get::<Option<i32>, _>(origin_column.as_str())?
            .map(port)
            .transpose()?,
        http_host: client.http_host,
        mux: client.mux,
    }))
}

/// One family's projected endpoint. Both columns NULL means no projection.
///
/// A half-filled pair cannot be stored in the first place
/// (`ingresses_projection_v4_check`), so a half row encountered here is not glossed over as
/// "no projection" — doing so would quietly read an already-corrupt database as a healthy
/// snapshot.
/// `family` is `v4` or `v6`; the two columns are that prefix plus a fixed suffix.
///
/// Taken as one family rather than two column names because swapped names would compile and read the
/// v6 columns into the v4 endpoint.
fn projection_endpoint(
    row: &sqlx::postgres::PgRow,
    family: &str,
) -> Result<Option<ProjectionEndpoint>> {
    let column = |suffix: &str| format!("projection_{family}_{suffix}");
    let host_column = column("host");
    let port_column = column("port");
    let host = row.try_get::<Option<String>, _>(host_column.as_str())?;
    let raw_port = row.try_get::<Option<i32>, _>(port_column.as_str())?;
    match (host, raw_port) {
        (None, None) => Ok(None),
        (Some(host), Some(raw_port)) => Ok(Some(ProjectionEndpoint {
            host,
            port: port(raw_port)?,
        })),
        _ => Err(StoreError::InvalidData(format!(
            "ingresses.{host_column}/{port_column} 只填了一半"
        ))),
    }
}

async fn load_steps(pool: &PgPool, app_id: &str) -> Result<Vec<Step>> {
    let rows = sqlx::query(
        "SELECT s.chain_id, s.node_id, s.accept_uuid::text AS accept_uuid, \
            s.accept_label, s.rules, s.hop_in_port, s.hop_in_wire \
         FROM steps s \
         JOIN chains c ON c.id = s.chain_id \
         WHERE c.app_id = $1 \
         ORDER BY s.chain_id, s.node_id",
    )
    .bind(app_id)
    .fetch_all(pool)
    .await?;

    rows.iter().map(step_from_row).collect()
}

async fn load_steps_tx(tx: &mut Transaction<'_, Postgres>, app_id: &str) -> Result<Vec<Step>> {
    let rows = sqlx::query(
        "SELECT s.chain_id, s.node_id, s.accept_uuid::text AS accept_uuid, \
            s.accept_label, s.rules, s.hop_in_port, s.hop_in_wire \
         FROM steps s \
         JOIN chains c ON c.id = s.chain_id \
         WHERE c.app_id = $1 \
         ORDER BY s.chain_id, s.node_id",
    )
    .bind(app_id)
    .fetch_all(&mut **tx)
    .await?;

    rows.iter().map(step_from_row).collect()
}

fn step_from_row(row: &sqlx::postgres::PgRow) -> Result<Step> {
    let accept_uuid: Option<String> = row.try_get("accept_uuid")?;
    let accept_label: Option<String> = row.try_get("accept_label")?;
    let accept = match (accept_uuid, accept_label) {
        (Some(uuid), Some(label)) => Some(Accept { uuid, label }),
        (None, None) => None,
        _ => return invalid("steps.accept_uuid and accept_label must be set together"),
    };

    // The two columns live and die together, watched by a CHECK in the database. The test is
    // repeated here because `hop_in_wire` has to deserialize — the constraint governs
    // presence, not whether the value is a valid HopWire.
    let hop_in_port: Option<i32> = row.try_get("hop_in_port")?;
    let hop_in_wire: Option<Value> = row.try_get("hop_in_wire")?;
    let hop_in = match (hop_in_port, hop_in_wire) {
        (Some(port), Some(security)) => Some(HopIn {
            port: optional_port(Some(port))?
                .ok_or_else(|| invalid_error("steps.hop_in_port must be 1..=65535"))?,
            security: serde_json::from_value(security)
                .map_err(|err| invalid_error(format!("steps.hop_in_wire is invalid: {err}")))?,
        }),
        (None, None) => None,
        _ => return invalid("steps.hop_in_port and hop_in_wire must be set together"),
    };

    Ok(Step {
        chain: text(row, "chain_id")?,
        node: text(row, "node_id")?,
        accept,
        hop_in,
        rules: parse_rules(&row.try_get::<Value, _>("rules")?)?,
    })
}

async fn load_grants(pool: &PgPool, app_id: &str) -> Result<Vec<Grant>> {
    let rows = sqlx::query(
        "SELECT g.tenant_id, g.user_id, g.ingress_id \
         FROM grants g \
         JOIN users u \
           ON u.tenant_id = g.tenant_id \
          AND u.id = g.user_id \
          AND u.status = 'active' \
         WHERE g.app_id = $1 \
         ORDER BY g.tenant_id, g.user_id, g.ingress_id",
    )
    .bind(app_id)
    .fetch_all(pool)
    .await?;

    rows.iter().map(grant_from_row).collect()
}

async fn load_grants_tx(tx: &mut Transaction<'_, Postgres>, app_id: &str) -> Result<Vec<Grant>> {
    let rows = sqlx::query(
        "SELECT g.tenant_id, g.user_id, g.ingress_id \
         FROM grants g \
         JOIN users u \
           ON u.tenant_id = g.tenant_id \
          AND u.id = g.user_id \
          AND u.status = 'active' \
         WHERE g.app_id = $1 \
         ORDER BY g.tenant_id, g.user_id, g.ingress_id",
    )
    .bind(app_id)
    .fetch_all(&mut **tx)
    .await?;

    rows.iter().map(grant_from_row).collect()
}

fn grant_from_row(row: &sqlx::postgres::PgRow) -> Result<Grant> {
    Ok(Grant {
        tenant: text(row, "tenant_id")?,
        user: text(row, "user_id")?,
        ingress: text(row, "ingress_id")?,
    })
}

fn parse_front_strategy(value: &str) -> Result<FrontStrategy> {
    match value {
        "url-test" => Ok(FrontStrategy::UrlTest),
        "select" => Ok(FrontStrategy::Select),
        "fallback" => Ok(FrontStrategy::Fallback),
        value => invalid(format!("unknown front strategy {value}")),
    }
}

fn parse_rules(value: &Value) -> Result<Vec<Rule>> {
    serde_json::from_value(value.clone())
        .map_err(|error| invalid_error(format!("steps.rules is invalid: {error}")))
}

pub(crate) fn json_string_array(field: &str, value: &Value) -> Result<Vec<String>> {
    let Value::Array(items) = value else {
        return invalid(format!("{field} must be a JSON array"));
    };

    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid_error(format!("{field}[{index}] must be a string")))
        })
        .collect()
}

fn disabled_wireguard_links(field: &str, value: &Value) -> Result<Vec<DisabledWireGuardLink>> {
    serde_json::from_value(value.clone())
        .map_err(|error| invalid_error(format!("{field} has invalid entries: {error}")))
}

fn text(row: &sqlx::postgres::PgRow, field: &str) -> Result<String> {
    Ok(row.try_get(field)?)
}

/// A nullable BIGINT read back as the model's `u64`.
fn window(row: &sqlx::postgres::PgRow, field: &str) -> Result<Option<u64>> {
    row.try_get::<Option<i64>, _>(field)?
        .map(|value| {
            u64::try_from(value)
                .map_err(|_| invalid_error(format!("ingresses.{field} out of range: {value}")))
        })
        .transpose()
}

/// The same for the second/count columns, which the model holds as `u32`.
fn secs(row: &sqlx::postgres::PgRow, field: &str) -> Result<Option<u32>> {
    row.try_get::<Option<i32>, _>(field)?
        .map(|value| {
            u32::try_from(value)
                .map_err(|_| invalid_error(format!("ingresses.{field} out of range: {value}")))
        })
        .transpose()
}

fn port(value: i32) -> Result<u16> {
    u16::try_from(value).map_err(|_| invalid_error(format!("port out of range: {value}")))
}

fn optional_port(value: Option<i32>) -> Result<Option<u16>> {
    value.map(port).transpose()
}

fn revision_to_u64(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| invalid_error(format!("revision out of range: {value}")))
}

fn revision_to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| invalid_error(format!("revision out of range: {value}")))
}

fn optional_u64(location: &str, value: Option<i64>) -> Result<Option<u64>> {
    value
        .map(|value| {
            u64::try_from(value)
                .map_err(|_| invalid_error(format!("{location} out of range: {value}")))
        })
        .transpose()
}

fn optional_u32_bigint(row: &sqlx::postgres::PgRow, field: &str) -> Result<Option<u32>> {
    row.try_get::<Option<i64>, _>(field)?
        .map(|value| {
            u32::try_from(value).map_err(|_| {
                invalid_error(format!(
                    "ingress_client_settings.{field} out of range: {value}"
                ))
            })
        })
        .transpose()
}

fn parse_ip(value: &str) -> Result<IpAddr> {
    let value = value.split_once('/').map_or(value, |(addr, _)| addr);
    value
        .parse()
        .map_err(|error| invalid_error(format!("invalid IP {value}: {error}")))
}

fn parse_ipv4(value: &str) -> Result<Ipv4Addr> {
    match parse_ip(value)? {
        IpAddr::V4(value) => Ok(value),
        IpAddr::V6(value) => invalid(format!("expected IPv4 address, got {value}")),
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(invalid_error(message))
}

fn invalid_error(message: impl Into<String>) -> StoreError {
    StoreError::InvalidData(message.into())
}

impl From<ipnet::AddrParseError> for StoreError {
    fn from(error: ipnet::AddrParseError) -> Self {
        StoreError::InvalidData(format!("invalid network: {error}"))
    }
}

fn u32_bigint_column(location: &str, value: i64) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{location} out of range: {value}")))
}

fn u32_column(location: &str, value: i32) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{location} out of range: {value}")))
}

fn hop_mux_from_row(row: &sqlx::postgres::PgRow) -> Result<HopMux> {
    Ok(HopMux {
        concurrency: u16_column(
            "control_state.relay_mux_concurrency",
            row.try_get("relay_mux_concurrency")?,
        )?,
        prewarm_workers: u32_bigint_column(
            "control_state.relay_mux_prewarm_workers",
            row.try_get("relay_mux_prewarm_workers")?,
        )?,
        reuse_threshold: u32_bigint_column(
            "control_state.relay_mux_reuse_threshold",
            row.try_get("relay_mux_reuse_threshold")?,
        )?,
        max_probing_workers: u32_bigint_column(
            "control_state.relay_mux_max_probing_workers",
            row.try_get("relay_mux_max_probing_workers")?,
        )?,
        probe_interval_ms: u32_column(
            "control_state.relay_mux_probe_interval_ms",
            row.try_get("relay_mux_probe_interval_ms")?,
        )?,
        probe_timeout_ms: u32_column(
            "control_state.relay_mux_probe_timeout_ms",
            row.try_get("relay_mux_probe_timeout_ms")?,
        )?,
        idle_ttl_ms: u32_bigint_column(
            "control_state.relay_mux_idle_ttl_ms",
            row.try_get("relay_mux_idle_ttl_ms")?,
        )?,
        max_requests_per_worker: u16_column(
            "control_state.relay_mux_max_requests_per_worker",
            row.try_get("relay_mux_max_requests_per_worker")?,
        )?,
    })
}

fn u16_column(location: &str, value: i32) -> Result<u16> {
    u16::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{location} out of range: {value}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use brocade_core::model::{Action, DestMatch, HopDial, HopPool, Network};
    use serde_json::json;

    #[test]
    fn parse_rules_accepts_the_current_wire_shape() {
        let rules = parse_rules(&json!([
            {
                "m": { "t": "geosite", "v": ["netflix", "openai"] },
                "a": {
                    "t": "forward",
                    "to": "au-01",
                    "dial": { "t": "overlay" },
                    "pool": { "t": "none" }
                }
            },
            {
                "m": {
                    "t": "all",
                    "v": [
                        { "t": "domain_suffix", "v": ["example.com"] },
                        { "t": "network", "v": "tcp" },
                        { "t": "port", "v": ["443"] }
                    ]
                },
                "a": { "t": "egress", "send_through": "10.66.0.4" }
            },
            {
                "m": { "t": "front_downstream" },
                "a": { "t": "block" }
            },
            {
                "m": { "t": "sniffing_failed" },
                "a": { "t": "block" }
            },
            {
                "m": { "t": "geosite", "v": ["netflix"] },
                "a": { "t": "egress", "send_through": null }
            }
        ]))
        .unwrap();

        assert_eq!(
            rules,
            vec![
                Rule {
                    dest_match: DestMatch::Geosite(vec!["netflix".to_owned(), "openai".to_owned()]),
                    action: Action::Forward {
                        to: "au-01".to_owned(),
                        dial: HopDial::Overlay,
                        pool: HopPool::None,
                    },
                },
                Rule {
                    dest_match: DestMatch::All(vec![
                        DestMatch::DomainSuffix(vec!["example.com".to_owned()]),
                        DestMatch::Network(Network::Tcp),
                        DestMatch::Port(vec!["443".to_owned()]),
                    ]),
                    action: Action::Egress {
                        send_through: Some("10.66.0.4".parse().unwrap()),
                    },
                },
                Rule {
                    dest_match: DestMatch::FrontDownstream,
                    action: Action::Block,
                },
                Rule {
                    dest_match: DestMatch::SniffingFailed,
                    action: Action::Block,
                },
                Rule {
                    dest_match: DestMatch::Geosite(vec!["netflix".to_owned()]),
                    action: Action::Egress { send_through: None },
                },
            ]
        );
    }

    #[test]
    fn parse_rules_reports_missing_action() {
        let error = parse_rules(&json!([{ "m": { "t": "any" } }])).unwrap_err();
        assert!(error.to_string().contains("missing field `a`"));
    }

    #[test]
    fn parse_rules_rejects_non_contract_field_names_and_incomplete_forwards() {
        assert!(parse_rules(&json!([{
            "match": { "t": "any" },
            "action": { "t": "block" }
        }]))
        .is_err());
        assert!(parse_rules(&json!([{
            "m": { "t": "any" },
            "a": { "t": "forward", "to": "au-01" }
        }]))
        .is_err());
    }
}
