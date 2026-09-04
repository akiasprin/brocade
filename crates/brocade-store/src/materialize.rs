use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr},
};

use brocade_core::client_config::ClientProjectionDownloadEndpoint;
use brocade_core::hash::hex_lower;
use brocade_core::model::{
    Accept, Action, AnyTls, AnyTlsMasquerade, AppView, Chain, ConnectionSettings, DestMatch,
    DisabledWireGuardLink, Dns, ExternalOutbound, ExternalOutboundProtocol,
    ExternalOutboundSecurity, ExternalWarpBinding, Front, FrontStrategy, GeodataSettings, Grant,
    HopDial, HopIn, HopPool, Hysteria2, HysteriaBandwidth, HysteriaBbrProfile, HysteriaCongestion,
    HysteriaMasquerade, HysteriaObfs, HysteriaPortHop, HysteriaQuic, Ingress, IngressGuard,
    IngressIdentity, IngressWires, IngressWiresWire, ModelSettings, ModelSnapshot, Network, Node,
    NodeConnection, OverlaySettings, PortSettings, ProbeSettings, Projection,
    ProjectionDownloadEndpoint, ProjectionEndpoint, RealityClientPolicy, RealityFallbackLimits,
    RealityFallbackMode, RealitySettings, RealitySite, RealityXhttp, Rule, Step, Tls, TlsXhttp,
    Transport, User, WireGuardKeys, Xhttp, XhttpMode,
};
use ipnet::Ipv4Net;
use serde_json::Value;
use sha2::{Digest, Sha256};
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
            port_ingress_base, port_hop_base, port_hy2_base, \
            probe_endpoint_url, probe_timeout_secs, probe_interval_secs, \
            geodata_cron, geodata_geoip_url, geodata_geosite_url, \
            conn_idle_secs, conn_uplink_only_secs, conn_downlink_only_secs, \
            conn_buffer_size_kb, conn_handshake_secs, stats_user_online \
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
            ingress_base: u16_column(
                "control_state.port_ingress_base",
                state.try_get("port_ingress_base")?,
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
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        // Compatibility for a database created by an older build before revision snapshots were
        // mandatory. Its next successful revision creates the row and moves this path to one GET.
        return load_current_snapshot(pool).await;
    };
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
    fold_legacy_node_egress_dns_positions(&mut snapshot)?;
    fold_legacy_stream(&mut snapshot);
    fold_legacy_xhttp_mux(&mut snapshot)?;
    fold_removed_ingress_client_controls(&mut snapshot);
    fold_legacy_ingress_identity(&mut snapshot)?;
    // Last, because the two above reach into `transport` by name and this is what moves it.
    fold_legacy_transport(&mut snapshot);
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

/// Give snapshots written before machine DNS had an independent priority their canonical order.
///
/// The table always materializes policies in `(node_id, position)` order, so the array order in a
/// legacy snapshot is the only ordering fact it contains. Re-indexing each machine from that array
/// is therefore lossless. If one entry is missing the field, normalize the whole array: this also
/// repairs partially rewritten development snapshots without creating duplicate positions.
fn fold_legacy_node_egress_dns_positions(snapshot: &mut Value) -> Result<()> {
    let root = snapshot
        .as_object_mut()
        .ok_or_else(|| invalid_error("model snapshot must be an object"))?;
    let policies = root
        .entry("node_egress_dns".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| invalid_error("model snapshot node_egress_dns must be an array"))?;
    if policies
        .iter()
        .all(|policy| policy.get("position").is_some())
    {
        return Ok(());
    }

    let mut next_by_node = BTreeMap::<String, u32>::new();
    for policy in policies {
        let object = policy.as_object_mut().ok_or_else(|| {
            invalid_error("model snapshot node_egress_dns entry must be an object")
        })?;
        let node = object
            .get("node")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_error("model snapshot node_egress_dns entry is missing node"))?
            .to_owned();
        let position = next_by_node.entry(node).or_default();
        object.insert("position".to_owned(), Value::from(*position));
        *position = position
            .checked_add(1)
            .ok_or_else(|| invalid_error("model snapshot node_egress_dns position overflow"))?;
    }
    Ok(())
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
        // Snapshots written before tunnels became tenant resources used `app` as their sealing
        // scope. Open those under the old context, then remove the obsolete field before serde
        // decodes the deny-unknown-fields model. New snapshots seal directly under `tenant`.
        let legacy_app = outbound
            .get("app")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let tenant = outbound
            .get("tenant")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_error("external outbound snapshot is missing tenant"))?
            .to_owned();
        let scope = legacy_app.as_deref().unwrap_or(&tenant).to_owned();
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
                &crate::secrets::external_outbound_context(&scope, &id),
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
                    &crate::secrets::external_outbound_binding_key_context(&scope, &id, &node),
                    &private_key,
                )?;
                binding
                    .as_object_mut()
                    .unwrap()
                    .insert("private_key".to_owned(), Value::String(transformed));
            }
        }
        if legacy_app.is_some() {
            outbound.as_object_mut().unwrap().remove("app");
        }
    }
    Ok(())
}

/// Move credentials written inside legacy transport objects to the ingress-owned identity.
fn fold_legacy_ingress_identity(snapshot: &mut Value) -> Result<()> {
    let Some(apps) = snapshot.get_mut("apps").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for ingress in apps
        .iter_mut()
        .filter_map(|app| app.get_mut("ingresses").and_then(Value::as_array_mut))
        .flatten()
    {
        let Some(object) = ingress.as_object_mut() else {
            continue;
        };
        if object.contains_key("identity") {
            continue;
        }
        let ingress_id = object
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Some(transport) = object.get_mut("transport").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(private_key) = transport
            .remove("private_key")
            .and_then(|value| value.as_str().map(str::to_owned))
        else {
            continue;
        };
        let public_key = match transport
            .remove("public_key")
            .and_then(|value| value.as_str().map(str::to_owned))
        {
            Some(public_key) => public_key,
            None => crate::credentials::reality_public_key(&private_key)?,
        };
        let short_ids = transport.remove("short_ids").unwrap_or_else(|| {
            let digest = Sha256::digest(format!("{private_key}:{ingress_id}").as_bytes());
            Value::Array(vec![Value::String(hex_lower(&digest[..8]))])
        });
        object.insert(
            "identity".to_owned(),
            serde_json::json!({
                "private_key": private_key,
                "public_key": public_key,
                "short_ids": short_ids,
            }),
        );
    }
    Ok(())
}

/// Fold a pre-`Transport` snapshot's `stream` field into the shape that replaced it.
///
/// A revision's snapshot is an immutable record of what the model was, so it is read as written
/// and never rewritten — which means this build has to be able to read what earlier builds wrote.
/// Those wrote the network layer as its own `stream` field beside `transport`, and `Ingress`
/// denies unknown fields, so without this every historical revision fails to load: no rollback,
/// no recompile, no artifact view. The failure does not appear until a *newer* revision exists,
/// because the current one is materialized from the tables and never touches this row.
///
/// Dropping the field instead of folding it would be worse than the error. An old revision whose
/// ingress ran over XHTTP would come back as a TCP one, recompile into a different machine
/// configuration than it originally produced, and roll back to something nobody ever deployed.
fn fold_legacy_stream(snapshot: &mut Value) {
    let Some(apps) = snapshot.get_mut("apps").and_then(Value::as_array_mut) else {
        return;
    };
    for app in apps {
        let Some(ingresses) = app.get_mut("ingresses").and_then(Value::as_array_mut) else {
            continue;
        };
        for ingress in ingresses {
            let Some(stream) = ingress
                .as_object_mut()
                .and_then(|ingress| ingress.remove("stream"))
            else {
                continue;
            };
            // `{"t":"xhttp","v":{...}}` was the only shape that carried anything; `tcp` and an
            // absent field both mean the transport already says everything there is to say.
            if stream.get("t").and_then(Value::as_str) != Some("xhttp") {
                continue;
            }
            let Some(xhttp) = stream.get("v") else {
                continue;
            };
            let Some(transport) = ingress.get_mut("transport").and_then(Value::as_object_mut)
            else {
                continue;
            };
            transport.insert(
                "kind".to_owned(),
                Value::String("vless-reality-xhttp".to_owned()),
            );
            // `mode` did not exist then, and its absence is the value it would have had.
            transport.insert("xhttp".to_owned(), xhttp.clone());
        }
    }
}

/// Expand the old scalar XHTTP `mux` shorthand without removing the `xhttp` layer.
///
/// The scalar always meant the client-side `xhttp.xmux.maxConcurrency`; it was never the generic
/// VLESS mux.  Once any XMUX value is present Xray stops injecting its request-count and lifetime
/// defaults, so the canonical model stores one complete policy. Historical snapshots are immutable
/// rollback inputs and may contain the shorthand under either the old `transport` shape or the
/// current `wires.vless` shape. Normalize only those two exact XHTTP objects: download projection
/// and imported outbound structures also have legitimate fields named `mux` with different shapes.
fn fold_legacy_xhttp_mux(snapshot: &mut Value) -> Result<()> {
    let Some(apps) = snapshot.get_mut("apps").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for ingress in apps
        .iter_mut()
        .filter_map(|app| app.get_mut("ingresses").and_then(Value::as_array_mut))
        .flatten()
    {
        let Some(ingress) = ingress.as_object_mut() else {
            continue;
        };
        let xhttp = if ingress.contains_key("wires") {
            ingress
                .get_mut("wires")
                .and_then(|wires| wires.get_mut("vless"))
                .and_then(|vless| vless.get_mut("xhttp"))
        } else {
            ingress
                .get_mut("transport")
                .and_then(|transport| transport.get_mut("xhttp"))
        };
        let Some(xhttp) = xhttp.and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(legacy) = xhttp.remove("mux") else {
            continue;
        };
        // `Option<u16>` serialized None as null. Removing the obsolete key restores the exact
        // meaning: no custom XMUX object, so the client uses Xray's own defaults.
        if legacy.is_null() || xhttp.contains_key("xmux") {
            continue;
        }
        let concurrency = legacy.as_u64().ok_or_else(|| {
            invalid_error("historical xhttp.mux must be a positive integer or null")
        })?;
        if !(1..=128).contains(&concurrency) {
            return Err(invalid_error(format!(
                "historical xhttp.mux {concurrency} is outside 1..=128"
            )));
        }
        xhttp.insert(
            "xmux".to_owned(),
            serde_json::json!({
                "max_concurrency": concurrency,
                "h_max_request_times": { "from": 600, "to": 900 },
                "h_max_reusable_secs": { "from": 1800, "to": 3000 }
            }),
        );
    }
    Ok(())
}

/// Remove fields written during the short period when ordinary TLS ClientHello selection and
/// POST upload tuning were managed. They no longer belong to the model: TLS clients use their
/// own defaults, and XHTTP retains only the settings Brocade applies coherently at both ends.
fn fold_removed_ingress_client_controls(snapshot: &mut Value) {
    let Some(apps) = snapshot.get_mut("apps").and_then(Value::as_array_mut) else {
        return;
    };
    for ingress in apps
        .iter_mut()
        .filter_map(|app| app.get_mut("ingresses").and_then(Value::as_array_mut))
        .flatten()
    {
        let Some(ingress) = ingress.as_object_mut() else {
            continue;
        };
        let vless = if ingress.contains_key("wires") {
            ingress
                .get_mut("wires")
                .and_then(|wires| wires.get_mut("vless"))
        } else {
            ingress.get_mut("transport")
        };
        let Some(vless) = vless.and_then(Value::as_object_mut) else {
            continue;
        };
        match vless.get("kind").and_then(Value::as_str) {
            Some("vless-tls") => {
                vless.remove("fingerprint");
            }
            Some("vless-tls-xhttp") => {
                vless.remove("fingerprint");
                vless.remove("alpn");
            }
            _ => {}
        }
        let Some(xhttp) = vless.get_mut("xhttp").and_then(Value::as_object_mut) else {
            continue;
        };
        let remove_tuning =
            if let Some(tuning) = xhttp.get_mut("tuning").and_then(Value::as_object_mut) {
                for field in [
                    "sc_max_each_post_bytes",
                    "sc_min_posts_interval_ms",
                    "sc_max_buffered_posts",
                    "uplink_chunk_size",
                ] {
                    tuning.remove(field);
                }
                tuning.is_empty()
            } else {
                false
            };
        if remove_tuning {
            xhttp.remove("tuning");
        }
    }
}

/// Move the single `transport` an earlier build wrote into the two-wire `wires`.
///
/// Same reasoning as `fold_legacy_stream`, and the same blast radius: stored snapshots are
/// written once and never rewritten, `Ingress` denies unknown fields, so a renamed field makes
/// every historical revision unreadable — no rollback, no recompile, no artifact view. It does
/// not show up until a newer revision exists, because the current one is materialized from the
/// tables and never touches these rows. It showed up exactly that way.
///
/// Two shapes were written before `wires`:
///
/// - the four VLESS ones, which become the TCP half verbatim;
/// - a short-lived `kind: "hysteria2"`, from when Hysteria 2 was a fifth `Transport` variant
///   rather than the second wire. Its settings sat flattened beside `kind`, so the whole object
///   minus `kind` is the QUIC half.
///
/// Dropping the field instead of folding it would be worse than the error: an ingress would come
/// back with no wire at all, fail `IngressWires`' own "at least one" check, and take the whole
/// revision down with a message about something that was never wrong.
fn fold_legacy_transport(snapshot: &mut Value) {
    let Some(apps) = snapshot.get_mut("apps").and_then(Value::as_array_mut) else {
        return;
    };
    for app in apps {
        let Some(ingresses) = app.get_mut("ingresses").and_then(Value::as_array_mut) else {
            continue;
        };
        for ingress in ingresses {
            let Some(object) = ingress.as_object_mut() else {
                continue;
            };
            // Already the new shape: a snapshot written after the rename carries `wires` and no
            // `transport`, and must be left exactly as it is.
            if object.contains_key("wires") {
                object.remove("transport");
                continue;
            }
            let Some(transport) = object.remove("transport") else {
                continue;
            };
            let quic = transport.get("kind").and_then(Value::as_str) == Some("hysteria2");
            let mut wires = serde_json::Map::new();
            if quic {
                let mut hysteria2 = transport.as_object().cloned().unwrap_or_default();
                hysteria2.remove("kind");
                wires.insert("hysteria2".to_owned(), Value::Object(hysteria2));
            } else {
                wires.insert("vless".to_owned(), transport);
            }
            object.insert("wires".to_owned(), Value::Object(wires));
        }
    }
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
            port_ingress_base, port_hop_base, port_hy2_base, \
            probe_endpoint_url, probe_timeout_secs, probe_interval_secs, \
            geodata_cron, geodata_geoip_url, geodata_geosite_url, \
            conn_idle_secs, conn_uplink_only_secs, conn_downlink_only_secs, \
            conn_buffer_size_kb, conn_handshake_secs, stats_user_online \
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
            ingress_base: u16_column(
                "control_state.port_ingress_base",
                state.try_get("port_ingress_base")?,
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
            (SELECT l.label || '.' || d.domain \
               FROM node_cert_label m \
               JOIN cert_labels l ON l.id = m.label_id \
               JOIN cert_domains d ON d.id = l.domain_id \
               JOIN certificates c ON c.label_id = l.id AND c.status = 'serving' \
              WHERE m.node_id = nodes.id
                AND c.expires_at > now()) AS certificate_name \
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
            (SELECT l.label || '.' || d.domain \
               FROM node_cert_label m \
               JOIN cert_labels l ON l.id = m.label_id \
               JOIN cert_domains d ON d.id = l.domain_id \
               JOIN certificates c ON c.label_id = l.id AND c.status = 'serving' \
              WHERE m.node_id = nodes.id
                AND c.expires_at > now()) AS certificate_name \
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
    Ok(Node {
        // Only a certificate that reached 'ready' counts. One still being issued, or whose last
        // attempt failed, is a name nothing answers to yet — and an ingress compiled against it
        // would hand out subscriptions naming a certificate that does not exist.
        certificate_name: row.try_get("certificate_name")?,
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
    // WARP credentials belong to each machine binding, not to the tenant resource. New rows use
    // an empty sentinel and older rows may still contain a sealed empty string; neither is data
    // the model needs, so do not require a secret key merely to materialize an unbound default.
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
        "vless" => ExternalOutboundProtocol::Vless {
            credential,
            encryption: options
                .get("encryption")
                .and_then(Value::as_str)
                .unwrap_or("none")
                .to_owned(),
            flow: options
                .get("flow")
                .and_then(Value::as_str)
                .map(str::to_owned),
            transport: options
                .get("transport")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| {
                    invalid_error(format!(
                        "external_outbounds.protocol_options.transport 无效（{tenant}/{id}）：{error}"
                    ))
                })?
                .unwrap_or_default(),
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
            // Rows and snapshots written before worker tuning existed deliberately inherit
            // wireguard-go's automatic fallback.
            workers: match options.get("workers") {
                None => 0,
                Some(value) => u16::try_from(value.as_u64().ok_or_else(|| {
                    invalid_error(format!(
                        "external_outbounds.protocol_options.workers 不是非负整数（{tenant}/{id}）"
                    ))
                })?)
                .map_err(|_| {
                    invalid_error(format!(
                        "external_outbounds.protocol_options.workers 超出 u16（{tenant}/{id}）"
                    ))
                })?,
            },
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
            transport_kind, anytls_enabled, anytls_port, anytls_padding_scheme, \
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
            projection_v4_download_host, projection_v4_download_port, \
            projection_v4_download_origin_port, \
            projection_v4_download_http_host, projection_v4_download_mux, \
            projection_v6_host, projection_v6_port, \
            projection_v6_download_host, projection_v6_download_port, \
            projection_v6_download_origin_port, \
            projection_v6_download_http_host, projection_v6_download_mux, \
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
            transport_kind, anytls_enabled, anytls_port, anytls_padding_scheme, \
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
            projection_v4_download_host, projection_v4_download_port, \
            projection_v4_download_origin_port, \
            projection_v4_download_http_host, projection_v4_download_mux, \
            projection_v6_host, projection_v6_port, \
            projection_v6_download_host, projection_v6_download_port, \
            projection_v6_download_origin_port, \
            projection_v6_download_http_host, projection_v6_download_mux, \
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
            "node-certificate" => RealityFallbackMode::NodeCertificate,
            "custom-site" => RealityFallbackMode::CustomSite,
            _ => RealityFallbackMode::GlobalSite,
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

    // Absent columns cannot happen — the schema defaults them — but an unknown kind can, if a
    // future build wrote one and this one is a rollback. Treated as the plain shape rather than
    // refused: the machine keeps serving what it has, which beats a control plane that cannot
    // compile at all.
    let xhttp = Xhttp {
        path: row
            .try_get::<Option<String>, _>("xhttp_path")?
            .unwrap_or_default(),
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
        // Same rollback reasoning: a value this build does not know reads as the default,
        // which is the one shape every client can speak.
        mode: match row.try_get::<Option<String>, _>("xhttp_mode")?.as_deref() {
            Some("packet-up") => XhttpMode::PacketUp,
            Some("stream-up") => XhttpMode::StreamUp,
            Some("stream-one") => XhttpMode::StreamOne,
            _ => XhttpMode::Auto,
        },
        download: xhttp_download_from_row(row)?,
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
                status_code: u16::try_from(row.try_get::<i32, _>("anytls_masquerade_status_code")?)
                    .unwrap_or(200),
            },
            _ => AnyTlsMasquerade::NotFound { headers },
        };
        Some(AnyTls {
            port: row
                .try_get::<Option<i32>, _>("anytls_port")?
                .and_then(|port| u16::try_from(port).ok())
                .unwrap_or(brocade_core::model::ANYTLS_PORT_BASE),
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
    // The borrowed-site columns are read for every shape and simply go unused by the ones that
    // hold a certificate: an ingress keeps its REALITY identity in storage while it is on TLS, so
    // moving it back does not mint a new public key behind everybody's back.
    let tls = Tls {
        flow: reality.flow.clone(),
    };
    let hysteria2 = Hysteria2 {
        // The column is NULL exactly where this ingress has no UDP wire, and the value is then
        // never read — `quic` below discards the whole struct. Falling back to the allocator's
        // base rather than erroring keeps a row with an inconsistent pair (which the CHECK
        // forbids anyway) from taking the whole snapshot down with it.
        port: row
            .try_get::<Option<i32>, _>("hy2_port")?
            .and_then(|port| u16::try_from(port).ok())
            .unwrap_or(brocade_core::model::HYSTERIA2_PORT_BASE),
        hop: match (
            row.try_get::<Option<i32>, _>("hy2_hop_start")?,
            row.try_get::<Option<i32>, _>("hy2_hop_end")?,
        ) {
            (Some(start), Some(end)) => Some(HysteriaPortHop {
                start: u16::try_from(start).unwrap_or_default(),
                end: u16::try_from(end).unwrap_or_default(),
            }),
            _ => None,
        },
        bandwidth: HysteriaBandwidth {
            up: row.try_get("hy2_up")?,
            down: row.try_get("hy2_down")?,
        },
        congestion: match text(row, "hy2_congestion")?.as_str() {
            "bbr" => HysteriaCongestion::Bbr,
            "reno" => HysteriaCongestion::Reno,
            "force-brutal" => HysteriaCongestion::ForceBrutal,
            _ => HysteriaCongestion::Brutal,
        },
        bbr_profile: match text(row, "hy2_bbr_profile")?.as_str() {
            "conservative" => HysteriaBbrProfile::Conservative,
            "aggressive" => HysteriaBbrProfile::Aggressive,
            _ => HysteriaBbrProfile::Standard,
        },
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
        masquerade: match text(row, "hy2_masquerade_kind")?.as_str() {
            "proxy" => HysteriaMasquerade::Proxy {
                url: row
                    .try_get::<Option<String>, _>("hy2_masquerade_url")?
                    .unwrap_or_default(),
            },
            _ => HysteriaMasquerade::NotFound,
        },
        // Same column as the TLS shapes read: one certificate, one question about it.
    };
    // Two nullable halves in storage, one non-optional pair in the model. A row with neither is
    // refused by the schema's own CHECK, so the `None` arm below is unreachable through the
    // console — it can only be a hand-edited database, and reading it as "TCP with the defaults"
    // would quietly serve a shape nobody asked for.
    let vless = row
        .try_get::<Option<String>, _>("transport_kind")?
        .map(|kind| match kind.as_str() {
            "vless-reality-xhttp" => Transport::VlessRealityXhttp(RealityXhttp { reality, xhttp }),
            "vless-tls" => Transport::VlessTls(tls),
            "vless-tls-xhttp" => Transport::VlessTlsXhttp(TlsXhttp { tls, xhttp }),
            _ => Transport::VlessReality(reality),
        });
    let quic = row.try_get::<bool, _>("hy2_enabled")?.then_some(hysteria2);
    let wires = IngressWires::try_from(IngressWiresWire {
        vless,
        anytls,
        hysteria2: quic,
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
    if let Some(value) = row.try_get::<Option<Value>, _>(client_column.as_str())? {
        let client =
            serde_json::from_value::<ClientProjectionDownloadEndpoint>(value).map_err(|error| {
                StoreError::InvalidData(format!(
                    "ingress_client_settings.{client_column} is invalid: {error}"
                ))
            })?;
        return Ok(Some(ProjectionDownloadEndpoint {
            host: client.host,
            port: client.port,
            origin_port: row
                .try_get::<Option<i32>, _>(origin_column.as_str())?
                .map(port)
                .transpose()?,
            http_host: client.http_host,
            mux: client.mux,
        }));
    }

    // Old snapshots stored the entire download under projection. Keep this read fallback until
    // all existing rows have passed through the idempotent migration and a normal save.
    Ok(projection_endpoint(row, family)?.and_then(|endpoint| endpoint.download))
}

/// One family's projected endpoint. Both columns NULL means no projection.
///
/// A half-filled pair cannot be stored in the first place
/// (`ingresses_projection_v4_check`), so a half row encountered here is not glossed over as
/// "no projection" — doing so would quietly read an already-corrupt database as a healthy
/// snapshot.
/// `family` is `v4` or `v6`; the seven columns are that prefix plus a fixed suffix.
///
/// Taken as one family rather than seven column names because seven `&str` parameters in a row is
/// an argument order nobody can check by reading — two swapped names would compile, and read the
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
        (Some(host), Some(raw_port)) => {
            let download_host_column = column("download_host");
            let download_port_column = column("download_port");
            let download_mux_column = column("download_mux");
            let download_host = row.try_get::<Option<String>, _>(download_host_column.as_str())?;
            let download_port = row.try_get::<Option<i32>, _>(download_port_column.as_str())?;
            let download = match (download_host, download_port) {
                (None, None) => None,
                (Some(host), Some(raw_port)) => Some(ProjectionDownloadEndpoint {
                    host,
                    port: port(raw_port)?,
                    origin_port: row
                        .try_get::<Option<i32>, _>(column("download_origin_port").as_str())?
                        .map(port)
                        .transpose()?,
                    http_host: row.try_get(column("download_http_host").as_str())?,
                    mux: row
                        .try_get::<Option<i32>, _>(download_mux_column.as_str())?
                        .map(u16::try_from)
                        .transpose()
                        .map_err(|_| {
                            StoreError::InvalidData(format!(
                                "ingresses.{download_mux_column} 超出 u16"
                            ))
                        })?,
                }),
                _ => {
                    return Err(StoreError::InvalidData(format!(
                        "ingresses.{download_host_column}/{download_port_column} 只填了一半"
                    )));
                }
            };
            Ok(Some(ProjectionEndpoint {
                host,
                port: port(raw_port)?,
                download,
            }))
        }
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
    let Value::Array(items) = value else {
        return invalid("steps.rules must be a JSON array");
    };

    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let object = item
                .as_object()
                .ok_or_else(|| invalid_error(format!("steps.rules[{index}] must be an object")))?;
            let dest_match = object
                .get("match")
                .or_else(|| object.get("dest_match"))
                .or_else(|| object.get("m"))
                .ok_or_else(|| invalid_error(format!("steps.rules[{index}].match is missing")))
                .and_then(parse_dest_match)?;
            let action = object
                .get("action")
                .or_else(|| object.get("a"))
                .ok_or_else(|| invalid_error(format!("steps.rules[{index}].action is missing")))
                .and_then(parse_action)?;
            Ok(Rule { dest_match, action })
        })
        .collect()
}

fn parse_dest_match(value: &Value) -> Result<DestMatch> {
    let tag = tagged_kind(value)?;
    match tag {
        "any" => Ok(DestMatch::Any),
        "domain_suffix" => Ok(DestMatch::DomainSuffix(tagged_string_array(value)?)),
        "domain_keyword" => Ok(DestMatch::DomainKeyword(tagged_string_array(value)?)),
        "domain_regex" => Ok(DestMatch::DomainRegex(tagged_string(value)?)),
        "geosite" => Ok(DestMatch::Geosite(tagged_string_array(value)?)),
        "ip_cidr" => Ok(DestMatch::IpCidr(tagged_string_array(value)?)),
        "geoip" => Ok(DestMatch::Geoip(tagged_string_array(value)?)),
        "port" => Ok(DestMatch::Port(tagged_string_array(value)?)),
        "network" => match tagged_string(value)?.as_str() {
            "tcp" => Ok(DestMatch::Network(Network::Tcp)),
            "udp" => Ok(DestMatch::Network(Network::Udp)),
            value => invalid(format!("unknown network match {value}")),
        },
        "all" => {
            let value = tagged_value(value)?;
            let Value::Array(items) = value else {
                return invalid("all match value must be an array");
            };
            Ok(DestMatch::All(
                items
                    .iter()
                    .map(parse_dest_match)
                    .collect::<Result<Vec<_>>>()?,
            ))
        }
        "front_downstream" => Ok(DestMatch::FrontDownstream),
        value => invalid(format!("unknown dest match {value}")),
    }
}

fn parse_action(value: &Value) -> Result<Action> {
    let tag = tagged_kind(value)?;
    match tag {
        "forward" => {
            let object = value
                .as_object()
                .ok_or_else(|| invalid_error("forward action must be an object"))?;
            let to = object
                .get("to")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_error("forward action requires string field to"))?;
            // This is a hand-written parser, not serde — without recognizing `dial` here, the
            // address written on the chain is lost on the way back while the artifacts look
            // entirely correct (the default overlay is a legitimate path). The value itself is
            // judged by serde, so as not to write a second copy of `HopDial`'s definition that
            // slowly diverges.
            let dial = match object.get("dial") {
                None | Some(Value::Null) => HopDial::Overlay,
                // The reason must come along. Swallowing it for a "must be overlay or addr"
                // makes that sentence stale with every variant added to `HopDial`, and someone
                // following it only turns a correct value into a wrong one.
                Some(value) => serde_json::from_value(value.clone()).map_err(|err| {
                    invalid_error(format!("forward action dial 解析不了：{value}（{err}）"))
                })?,
            };
            // Same treatment as `dial`, and for the same reason: a hand-written parser that
            // does not know the key drops it silently, and the artifacts still compile —
            // the hop just stops pooling and nobody can see why.
            let pool = match object.get("pool") {
                None | Some(Value::Null) => HopPool::None,
                Some(value) => serde_json::from_value(value.clone()).map_err(|err| {
                    invalid_error(format!("forward action pool 解析不了：{value}（{err}）"))
                })?,
            };
            Ok(Action::Forward {
                to: to.to_owned(),
                dial,
                pool,
            })
        }
        "egress" => {
            let object = value
                .as_object()
                .ok_or_else(|| invalid_error("egress action must be an object"))?;
            let send_through = match object.get("send_through") {
                None | Some(Value::Null) => None,
                Some(value) => Some(parse_ip(value.as_str().ok_or_else(|| {
                    invalid_error("egress send_through must be null or a string IP")
                })?)?),
            };
            // Old development rows may still carry `resolution` or the later `dns` activation
            // flag. Ignore both: machine DNS is the only authoritative source, and re-serializing
            // the rule naturally cleans those keys.
            Ok(Action::Egress { send_through })
        }
        "proxy" => {
            let outbound = value
                .as_object()
                .and_then(|object| object.get("outbound"))
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_error("proxy action requires string field outbound"))?;
            Ok(Action::Proxy {
                outbound: outbound.to_owned(),
            })
        }
        "block" => Ok(Action::Block),
        value => invalid(format!("unknown action {value}")),
    }
}

fn tagged_kind(value: &Value) -> Result<&str> {
    value
        .as_object()
        .and_then(|object| object.get("t"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_error("tagged JSON object requires string field t"))
}

fn tagged_value(value: &Value) -> Result<&Value> {
    value
        .as_object()
        .and_then(|object| object.get("v"))
        .ok_or_else(|| invalid_error("tagged JSON object requires field v"))
}

fn tagged_string(value: &Value) -> Result<String> {
    tagged_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid_error("tagged JSON value must be a string"))
}

fn tagged_string_array(value: &Value) -> Result<Vec<String>> {
    json_string_array("tagged.v", tagged_value(value)?)
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

/// A nullable BIGINT read back as the model's `u64`. Negative values cannot get past the column's
/// CHECK, so a negative here means the constraint is gone; taking `None` rather than wrapping
/// keeps the artifact from carrying a window of 18 exabytes.
fn window(row: &sqlx::postgres::PgRow, field: &str) -> Result<Option<u64>> {
    Ok(row
        .try_get::<Option<i64>, _>(field)?
        .and_then(|value| u64::try_from(value).ok()))
}

/// The same for the second/count columns, which the model holds as `u32`.
fn secs(row: &sqlx::postgres::PgRow, field: &str) -> Result<Option<u32>> {
    Ok(row
        .try_get::<Option<i32>, _>(field)?
        .and_then(|value| u32::try_from(value).ok()))
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

fn u32_column(location: &str, value: i32) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{location} out of range: {value}")))
}

fn u16_column(location: &str, value: i32) -> Result<u16> {
    u16::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{location} out of range: {value}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_rules_supports_core_match_and_action_shapes() {
        let rules = parse_rules(&json!([
            {
                "match": { "t": "geosite", "v": ["netflix", "openai"] },
                "action": { "t": "forward", "to": "au-01" }
            },
            {
                "match": {
                    "t": "all",
                    "v": [
                        { "t": "domain_suffix", "v": ["example.com"] },
                        { "t": "network", "v": "tcp" },
                        { "t": "port", "v": ["443"] }
                    ]
                },
                "action": { "t": "egress", "send_through": "10.66.0.4" }
            },
            {
                "match": { "t": "front_downstream" },
                "action": { "t": "block" }
            },
            {
                "match": { "t": "geosite", "v": ["netflix"] },
                "action": {
                    "t": "egress",
                    "send_through": null,
                    "dns": true,
                    "resolution": {
                        "address": "192.0.2.53",
                        "port": 5353,
                        "transport": "tcp",
                        "address_strategy": "use_ipv4",
                        "fallback": "machine"
                    }
                }
            }
        ]))
        .unwrap();

        assert_eq!(
            rules,
            vec![
                Rule {
                    dest_match: DestMatch::Geosite(vec!["netflix".to_owned(), "openai".to_owned()]),
                    // This JSON has no dial and no pool — absent means the overlay, and a
                    // connection per stream.
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
                    dest_match: DestMatch::Geosite(vec!["netflix".to_owned()]),
                    action: Action::Egress { send_through: None },
                },
            ]
        );
    }

    #[test]
    fn parse_rules_reports_missing_action() {
        let error = parse_rules(&json!([{ "match": { "t": "any" } }])).unwrap_err();
        assert!(error.to_string().contains("action is missing"));
    }

    #[test]
    fn legacy_machine_dns_uses_its_snapshot_order_as_position() {
        let mut snapshot = serde_json::json!({
            "node_egress_dns": [
                { "node": "n1", "selector": { "t": "geosite", "v": ["first"] } },
                { "node": "n2", "selector": { "t": "geosite", "v": ["other"] } },
                {
                    "node": "n1",
                    "position": 99,
                    "selector": { "t": "geosite", "v": ["second"] }
                }
            ]
        });

        super::fold_legacy_node_egress_dns_positions(&mut snapshot).unwrap();

        let policies = snapshot["node_egress_dns"].as_array().unwrap();
        assert_eq!(policies[0]["position"], 0);
        assert_eq!(policies[1]["position"], 0);
        assert_eq!(policies[2]["position"], 1);
    }

    #[test]
    fn snapshot_before_machine_dns_gets_an_empty_policy_list() {
        let mut snapshot = serde_json::json!({});
        super::fold_legacy_node_egress_dns_positions(&mut snapshot).unwrap();
        assert_eq!(snapshot["node_egress_dns"], serde_json::json!([]));
    }
    /// Snapshots written before `Transport` gained the XHTTP shape carry the network layer as a
    /// separate `stream` field. `Ingress` denies unknown fields, so those rows fail to load
    /// outright — and the failure is invisible until a newer revision exists, because the current
    /// revision is materialized from the tables and never reads this row.
    #[test]
    fn a_snapshot_written_before_the_transport_shape_still_loads() {
        let mut snapshot = serde_json::json!({
            "apps": [{
                "ingresses": [
                    {
                        "id": "i-xhttp",
                        "transport": {"kind": "vless-reality", "dest": "a:443"},
                        "stream": {"t": "xhttp", "v": {"path": "/probe", "mux": 16}},
                    },
                    {
                        "id": "i-tcp",
                        "transport": {"kind": "vless-reality", "dest": "a:443"},
                        "stream": {"t": "tcp"},
                    },
                ]
            }]
        });

        super::fold_legacy_stream(&mut snapshot);
        super::fold_legacy_xhttp_mux(&mut snapshot).unwrap();

        let ingresses = &snapshot["apps"][0]["ingresses"];
        assert!(ingresses[0].get("stream").is_none());
        assert_eq!(ingresses[0]["transport"]["kind"], "vless-reality-xhttp");
        assert_eq!(ingresses[0]["transport"]["xhttp"]["path"], "/probe");
        assert!(ingresses[0]["transport"]["xhttp"].get("mux").is_none());
        assert_eq!(
            ingresses[0]["transport"]["xhttp"]["xmux"],
            serde_json::json!({
                "max_concurrency": 16,
                "h_max_request_times": { "from": 600, "to": 900 },
                "h_max_reusable_secs": { "from": 1800, "to": 3000 }
            })
        );
        // The REALITY parameters stay where they were, which is what the new shape expects too.
        assert_eq!(ingresses[0]["transport"]["dest"], "a:443");

        assert!(ingresses[1].get("stream").is_none());
        assert_eq!(ingresses[1]["transport"]["kind"], "vless-reality");
        assert!(ingresses[1]["transport"].get("xhttp").is_none());
    }

    #[test]
    fn legacy_null_xhttp_mux_is_removed_inside_wires_without_touching_other_mux_fields() {
        let mut snapshot = serde_json::json!({
            "apps": [{
                "ingresses": [{
                    "wires": {
                        "vless": {
                            "kind": "vless-reality-xhttp",
                            "xhttp": { "path": "/probe", "mux": null, "mode": "auto" }
                        }
                    },
                    "projection": {
                        "v4": { "download": { "host": "download.example", "port": 443, "mux": 8 } }
                    }
                }]
            }]
        });

        super::fold_legacy_xhttp_mux(&mut snapshot).unwrap();

        let ingress = &snapshot["apps"][0]["ingresses"][0];
        assert!(ingress["wires"]["vless"]["xhttp"].get("mux").is_none());
        assert!(ingress["wires"]["vless"]["xhttp"].get("xmux").is_none());
        assert_eq!(ingress["projection"]["v4"]["download"]["mux"], 8);
    }

    #[test]
    fn removed_tls_and_post_controls_are_folded_out_of_historical_snapshots() {
        let mut snapshot = serde_json::json!({
            "apps": [{
                "ingresses": [
                    {
                        "transport": {
                            "kind": "vless-tls",
                            "flow": "",
                            "fingerprint": "none"
                        }
                    },
                    {
                        "wires": { "vless": {
                            "kind": "vless-tls-xhttp",
                            "flow": "",
                            "fingerprint": "chrome",
                            "alpn": "http1",
                            "xhttp": {
                                "path": "/tls",
                                "tuning": {
                                    "x_padding_bytes": { "from": 200, "to": 600 },
                                    "sc_max_each_post_bytes": { "from": 500000, "to": 1000000 },
                                    "sc_min_posts_interval_ms": { "from": 10, "to": 30 },
                                    "sc_max_buffered_posts": 64,
                                    "uplink_chunk_size": { "from": 65536, "to": 131072 }
                                }
                            }
                        }}
                    },
                    {
                        "wires": { "vless": {
                            "kind": "vless-reality-xhttp",
                            "fingerprint": "chrome",
                            "xhttp": {
                                "path": "/reality",
                                "tuning": { "sc_max_buffered_posts": 64 }
                            }
                        }}
                    }
                ]
            }]
        });

        super::fold_removed_ingress_client_controls(&mut snapshot);

        let ingresses = &snapshot["apps"][0]["ingresses"];
        assert!(ingresses[0]["transport"].get("fingerprint").is_none());
        assert!(ingresses[1]["wires"]["vless"].get("fingerprint").is_none());
        assert!(ingresses[1]["wires"]["vless"].get("alpn").is_none());
        assert_eq!(
            ingresses[1]["wires"]["vless"]["xhttp"]["tuning"],
            serde_json::json!({
                "x_padding_bytes": { "from": 200, "to": 600 }
            })
        );
        assert_eq!(ingresses[2]["wires"]["vless"]["fingerprint"], "chrome");
        assert!(ingresses[2]["wires"]["vless"]["xhttp"]
            .get("tuning")
            .is_none());
    }

    /// Every revision written before `wires` existed carries `transport`, and `Ingress` denies
    /// unknown fields. Without the fold, opening any of them fails with
    /// `unknown field \`transport\`` — which is how this was found, on a control plane holding
    /// 52 of them.
    #[test]
    fn a_legacy_transport_folds_into_the_tcp_wire() {
        let mut snapshot = serde_json::json!({
            "apps": [{
                "ingresses": [
                    { "id": "i-vless", "transport": { "kind": "vless-reality", "dest": "a:443" } },
                    { "id": "i-quic", "transport": {
                        "kind": "hysteria2",
                        "congestion": "brutal",
                        "masquerade": { "kind": "not-found" }
                    }},
                    // A snapshot written after the rename must come back untouched.
                    { "id": "i-new", "wires": { "vless": { "kind": "vless-tls" } } },
                ]
            }]
        });

        super::fold_legacy_transport(&mut snapshot);
        let ingresses = &snapshot["apps"][0]["ingresses"];

        assert_eq!(ingresses[0]["wires"]["vless"]["kind"], "vless-reality");
        assert_eq!(ingresses[0]["wires"]["vless"]["dest"], "a:443");
        assert!(ingresses[0].get("transport").is_none());

        // The fifth-variant era wrote Hysteria's settings flattened beside `kind`; everything but
        // `kind` is the QUIC half, and `kind` itself has no home in the new shape.
        assert_eq!(ingresses[1]["wires"]["hysteria2"]["congestion"], "brutal");
        assert!(ingresses[1]["wires"]["hysteria2"].get("kind").is_none());
        assert!(ingresses[1]["wires"].get("vless").is_none());

        assert_eq!(ingresses[2]["wires"]["vless"]["kind"], "vless-tls");
    }

    #[test]
    fn legacy_ingress_credentials_move_out_of_every_transport_shape() {
        let pair = crate::credentials::generate_reality_keypair().unwrap();
        let mut snapshot = serde_json::json!({
            "apps": [{"ingresses": [
                {
                    "id": "reality",
                    "transport": {
                        "kind": "vless-reality",
                        "private_key": pair.private_key,
                        "public_key": pair.public_key,
                        "short_ids": ["0123abcd"],
                        "dest": "a:443"
                    }
                },
                {
                    "id": "tls",
                    "transport": {
                        "kind": "vless-tls",
                        "private_key": pair.private_key,
                        "fingerprint": "chrome"
                    }
                }
            ]}]
        });

        super::fold_legacy_ingress_identity(&mut snapshot).unwrap();

        let ingresses = snapshot["apps"][0]["ingresses"].as_array().unwrap();
        assert_eq!(ingresses[0]["identity"]["public_key"], pair.public_key);
        assert_eq!(ingresses[0]["identity"]["short_ids"][0], "0123abcd");
        assert_eq!(ingresses[1]["identity"]["public_key"], pair.public_key);
        assert_eq!(
            ingresses[1]["identity"]["short_ids"][0]
                .as_str()
                .unwrap()
                .len(),
            16
        );
        for ingress in ingresses {
            assert!(ingress["transport"].get("private_key").is_none());
            assert!(ingress["transport"].get("public_key").is_none());
            assert!(ingress["transport"].get("short_ids").is_none());
        }
    }
}
