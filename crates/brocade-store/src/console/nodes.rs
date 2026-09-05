//! Writes on machines: status (decommission/restore), field updates, and parsing a relay
//! port's transport layer.
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use super::*;
use crate::{AdminContext, Result, StoreError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateNodeStatusRequest {
    /// `active` or `retired`
    pub status: String,
}

pub(crate) async fn update_node_status_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    node_id: &str,
    request: UpdateNodeStatusRequest,
) -> Result<bool> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can retire nodes".to_owned(),
        ));
    }
    let node_id = required_text(node_id, "node id")?;
    let retired = match request.status.trim() {
        "retired" => true,
        "active" => false,
        other => {
            return Err(StoreError::InvalidData(format!(
                "node status must be active or retired, got {other}"
            )));
        }
    };
    ensure_node_exists_tx(tx, &node_id).await?;
    // Decommissioning an already-decommissioned node leaves `COALESCE(retired_at, now())` at
    // the original instant, which compares equal to itself — the decommission time is not
    // refreshed, and no revision number is consumed for nothing.
    let changed = sqlx::query(
        "UPDATE nodes
         SET retired_at = CASE WHEN $2::boolean THEN COALESCE(retired_at, now()) ELSE NULL END
         WHERE id = $1
           AND retired_at IS DISTINCT FROM
               (CASE WHEN $2::boolean THEN COALESCE(retired_at, now()) ELSE NULL END)",
    )
    .bind(&node_id)
    .bind(retired)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    if changed {
        crate::lifecycle::advance_intent_tx(tx, &node_id, retired, revision_id).await?;
    }
    Ok(changed)
}

/// Echo the machine back after a write. Node results all come through here: `update_node` and
/// `update_node_status` return the same `UpdateNodeResult`, and there should likewise be one
/// way of reading it.
pub(crate) async fn load_node_result(
    pool: &PgPool,
    node_id: &str,
    revision_id: u64,
) -> Result<UpdateNodeResult> {
    let snapshot = crate::materialize::load_current_snapshot(pool).await?;
    let node = snapshot
        .nodes
        .into_iter()
        .find(|node| node.id == node_id)
        .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
    Ok(UpdateNodeResult {
        revision_id,
        node: redacted_value(node)?,
    })
}

pub async fn update_node(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    request: UpdateNodeRequest,
) -> Result<UpdateNodeResult> {
    let note = note_or(request.note.as_deref(), || format!("update node {node_id}"));
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let changed = update_node_tx(&mut tx, actor, revision_id, node_id, request).await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    load_node_result(pool, node_id, revision_id).await
}

pub(crate) async fn update_node_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    node_id: &str,
    request: UpdateNodeRequest,
) -> Result<bool> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update nodes".to_owned(),
        ));
    }
    let node_id = required_text(node_id, "node id")?;
    if let Some(port) = request.wg_listen_port {
        ensure_nonzero_port(port, "wg_listen_port")?;
    }
    if let Some(port) = request.api_port {
        ensure_nonzero_port(port, "api_port")?;
    }
    if let Some(dns) = &request.dns {
        validate_dns(dns)?;
    }
    let tenant_id = request.tenant_id.as_deref().and_then(optional_text);
    let name = request.name.as_deref().and_then(optional_text);
    let public_ipv4 = request.public_ipv4.as_deref().and_then(optional_text);
    let public_ipv6 = optional_ipv6_text(request.public_ipv6.as_deref(), "public_ipv6")?;
    let dns_kind = request.dns.as_ref().map(dns_kind);
    let dns_servers = request.dns.as_ref().map(dns_servers_json).transpose()?;
    let domain_strategy = request
        .domain_strategy
        .map(domain_strategy_column)
        .transpose()?;
    let mtu = match request.mtu {
        Some(0) | None => None,
        Some(value) if (1000..=9000).contains(&value) => Some(i32::from(value)),
        Some(value) => {
            return Err(StoreError::InvalidData(format!(
                "mtu 必须在 1000 到 9000 之间，收到 {value}"
            )));
        }
    };
    if let Some(connection) = &request.connection {
        crate::settings::validate_node_connection(connection)?;
    }
    // Four separate binds rather than one JSON blob: they are four columns, and the compiler
    // reads them as four. `conn_secs` narrows to what the columns hold — a value past i32 was
    // already refused above, so the fallback is unreachable and exists only to avoid a panic
    // path.
    let conn_secs = |value: Option<u32>| value.map(|v| i32::try_from(v).unwrap_or(i32::MAX));
    // The same two-stage form as `mtu` and `public_ipv4`: one boolean says whether the request
    // spoke about the policy at all, and the values are only consulted when it did.
    let sets_connection = request.connection.is_some();
    let connection = request.connection.unwrap_or_default();

    ensure_node_exists_tx(tx, &node_id).await?;
    if let Some(tenant_id) = tenant_id {
        ensure_tenant_exists_tx(tx, tenant_id).await?;
    }
    // A fake-TCP port colliding with another on this machine is reported by node.port-clash at
    // compile time, but the revision has been stamped by then. This blocks the most common
    // self-collisions first — against this machine's own wg / overlay / api ports.
    if let Some(WgTransport::FakeTcp { port }) = &request.wg_transport {
        ensure_nonzero_port(*port, "wg_transport.fake_tcp.port")?;
    }
    // The bulk of that WHERE mirrors SET field by field: compute what it would look like
    // afterwards, compare against now, and touch not one row when they match. This page submits
    // the whole form (fields not supplied keep their value through COALESCE), so "open the
    // detail view, change nothing, press save" is the most-travelled path — and it used to
    // stamp a new revision every time. Both sides must be edited together; missing one field
    // means a change to it is judged as no change and the revision number stays put, while the
    // value has in fact been written.
    let changed = sqlx::query(
        "UPDATE nodes
         SET tenant_id = COALESCE($2, tenant_id),
             name = COALESCE($3, name),
             public_ipv4 = CASE WHEN $4::boolean THEN $5 ELSE public_ipv4 END,
             public_ipv6 = CASE WHEN $6::boolean THEN $7 ELSE public_ipv6 END,
             wg_listen_port = COALESCE($8, wg_listen_port),
             api_port = CASE WHEN $9::boolean THEN $10 ELSE api_port END,
             overlay = COALESCE($11, overlay),
             egress_allowed = COALESCE($12, egress_allowed),
             dns_kind = COALESCE($13, dns_kind),
             dns_servers = COALESCE($14, dns_servers),
             domain_strategy = COALESCE($21, domain_strategy),
             mtu = CASE WHEN $16::boolean THEN $17 ELSE mtu END,
             wg_transport = COALESCE($18, wg_transport),
             public_ipv4_nat = COALESCE($19, public_ipv4_nat),
             public_ipv6_nat = COALESCE($20, public_ipv6_nat),
             conn_idle_secs = CASE WHEN $22::boolean THEN $23 ELSE conn_idle_secs END,
             conn_uplink_only_secs = CASE WHEN $22::boolean THEN $24 ELSE conn_uplink_only_secs END,
             conn_downlink_only_secs = CASE WHEN $22::boolean THEN $25 ELSE conn_downlink_only_secs END,
             conn_buffer_size_kb = CASE WHEN $22::boolean THEN $26 ELSE conn_buffer_size_kb END,
             created_revision = COALESCE(created_revision, $15)
         WHERE id = $1
           AND ROW(tenant_id, name, public_ipv4, public_ipv6, wg_listen_port, api_port,
                   overlay, egress_allowed, dns_kind, dns_servers, domain_strategy, mtu,
                   wg_transport, public_ipv4_nat, public_ipv6_nat,
                   conn_idle_secs, conn_uplink_only_secs, conn_downlink_only_secs,
                   conn_buffer_size_kb)
            IS DISTINCT FROM
               ROW(COALESCE($2, tenant_id),
                   COALESCE($3, name),
                   CASE WHEN $4::boolean THEN $5 ELSE public_ipv4 END,
                   CASE WHEN $6::boolean THEN $7 ELSE public_ipv6 END,
                   COALESCE($8, wg_listen_port),
                   CASE WHEN $9::boolean THEN $10 ELSE api_port END,
                   COALESCE($11, overlay),
                   COALESCE($12, egress_allowed),
                   COALESCE($13, dns_kind),
                   COALESCE($14, dns_servers),
                   COALESCE($21, domain_strategy),
                   CASE WHEN $16::boolean THEN $17 ELSE mtu END,
                   COALESCE($18, wg_transport),
                   COALESCE($19, public_ipv4_nat),
                   COALESCE($20, public_ipv6_nat),
                   CASE WHEN $22::boolean THEN $23 ELSE conn_idle_secs END,
                   CASE WHEN $22::boolean THEN $24 ELSE conn_uplink_only_secs END,
                   CASE WHEN $22::boolean THEN $25 ELSE conn_downlink_only_secs END,
                   CASE WHEN $22::boolean THEN $26 ELSE conn_buffer_size_kb END)",
    )
    .bind(&node_id)
    .bind(tenant_id)
    .bind(name)
    .bind(request.public_ipv4.is_some())
    .bind(public_ipv4)
    .bind(request.public_ipv6.is_some())
    .bind(public_ipv6)
    .bind(request.wg_listen_port.map(i32::from))
    .bind(request.api_port.is_some())
    .bind(request.api_port.map(i32::from))
    .bind(request.overlay)
    .bind(request.egress_allowed)
    .bind(dns_kind)
    .bind(dns_servers)
    .bind(u64_to_i64(revision_id, "revision_id")?)
    // The same two-stage form as public_ipv4: a field that appeared is written (an empty string
    // normalizing to NULL = cleared), one that did not keeps its value. Without this switch,
    // "this request did not mention the field" and "clear it" look identical in JSON.
    .bind(request.mtu.is_some())
    .bind(mtu)
    .bind(
        request
            .wg_transport
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?,
    )
    .bind(request.public_ipv4_nat)
    .bind(request.public_ipv6_nat)
    .bind(domain_strategy)
    .bind(sets_connection)
    .bind(conn_secs(connection.conn_idle_secs))
    .bind(conn_secs(connection.uplink_only_secs))
    .bind(conn_secs(connection.downlink_only_secs))
    .bind(conn_secs(connection.buffer_size_kb))
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    Ok(changed)
}

/// Turn "which kind is wanted" into a `HopWire` carrying its material.
///
/// An unchanged kind keeps its keys. Changing the keys costs every relay dialing this machine
/// its handshake until the next release, whereas changing REALITY's borrowed site has nothing
/// to do with the keys at all. New material is generated only when moving from one kind to
/// another — which is a chain-breaking change to begin with.
///
/// This moved from the node onto the chain, and the test moved with it: from "what this
/// machine is now" to "what this chain on this machine is now".
pub(crate) async fn resolve_hop_security(
    tx: &mut Transaction<'_, Postgres>,
    chain_id: &str,
    node_id: &str,
    wanted: &HopWireRequest,
) -> Result<Value> {
    let existing: Option<Value> =
        sqlx::query("SELECT hop_in_wire FROM steps WHERE chain_id = $1 AND node_id = $2")
            .bind(chain_id)
            .bind(node_id)
            .fetch_optional(&mut **tx)
            .await?
            .and_then(|row| row.try_get("hop_in_wire").ok())
            .flatten();
    let existing: HopWire = match existing {
        Some(value) => serde_json::from_value(value).map_err(|error| {
            StoreError::InvalidData(format!("steps.hop_in_wire 解不开：{error}"))
        })?,
        None => HopWire::None,
    };

    let resolved = match wanted {
        HopWireRequest::None => HopWire::None,
        // Same rule as the pairs below: an unchanged kind keeps its key, because rotating it
        // costs every relay dialing this port its connection until the next release.
        HopWireRequest::Shadowsocks2022 => match &existing {
            HopWire::Shadowsocks2022 {
                server_psk,
                user_psk,
            } => HopWire::Shadowsocks2022 {
                server_psk: server_psk.clone(),
                user_psk: user_psk.clone(),
            },
            // Drawn separately rather than one derived from the other: the account key is
            // layered under the port-wide one, and a derivation would mean anyone holding the
            // port could compute the account and the layering would buy nothing.
            _ => HopWire::Shadowsocks2022 {
                server_psk: generate_shadowsocks_psk()?,
                user_psk: generate_shadowsocks_psk()?,
            },
        },
        HopWireRequest::Encryption => match &existing {
            HopWire::Encryption(keys) => HopWire::Encryption(keys.clone()),
            _ => {
                // VLESS Encryption and REALITY are both X25519, base64url without padding,
                // from one generator.
                let keypair = generate_reality_keypair()?;
                HopWire::Encryption(HopEncryption {
                    private_key: keypair.private_key,
                    public_key: keypair.public_key,
                })
            }
        },
        HopWireRequest::Reality {
            dest,
            server_names,
            fingerprint,
        } => {
            let dest = brocade_core::text::normalize_host_port(&required_text(
                dest.clone(),
                "hop_security.dest",
            )?);
            if server_names.is_empty() {
                return Err(StoreError::InvalidData(
                    "hop_security.server_names 不能为空：REALITY 得知道伪装成哪个站点".to_owned(),
                ));
            }
            let (private_key, public_key, short_ids) = match &existing {
                HopWire::Reality(reality) => (
                    reality.private_key.clone(),
                    reality.public_key.clone(),
                    reality.short_ids.clone(),
                ),
                _ => {
                    let keypair = generate_reality_keypair()?;
                    (
                        keypair.private_key,
                        keypair.public_key,
                        vec![generate_reality_short_id()?],
                    )
                }
            };
            HopWire::Reality(Reality {
                private_key,
                public_key,
                short_ids,
                dest,
                server_names: server_names.clone(),
                fingerprint: fingerprint
                    .clone()
                    .and_then(|value| optional_text(&value).map(str::to_owned))
                    .unwrap_or_else(|| "chrome".to_owned()),
                // A relay hop carries no flow: any disagreement between the two ends takes it
                // down outright, and vision's gain on this segment is small (argued on
                // `HopDialWire::Reality` in ir/hops.rs).
                flow: None,
            })
        }
    };

    serde_json::to_value(resolved)
        .map_err(|error| StoreError::InvalidData(format!("hop_security 序列化不了：{error}")))
}
