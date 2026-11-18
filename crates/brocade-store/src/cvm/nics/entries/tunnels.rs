//! Machine-pair tunnels. One tunnel carries every entry from an entry machine to one
//! container host; it is created with the first such entry and withdrawn with the last.
//! Its listening port and global slot stay claimed until both ends confirm.
use super::*;

fn context(id: &str) -> String {
    format!("cvm-entry-tunnel:{id}")
}

/// Returns the live tunnel for the pair, creating it from the publication's offers.
pub(super) async fn ensure(
    tx: &mut Transaction<'_, Postgres>,
    entry_node: &str,
    target_node: &str,
    publication: &EntryPublication,
) -> Result<String> {
    let entry_caps = capability(tx, entry_node).await?;
    let target_caps = capability(tx, target_node).await?;
    let existing: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT target FROM cvm_entry_tunnels WHERE entry_node=$1 AND target_node=$2 AND NOT withdrawn",
    )
    .bind(entry_node)
    .bind(target_node)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(existing) = existing {
        let existing: EntryTunnel = serde_json::from_value(existing)?;
        if !publication
            .spec
            .transports
            .iter()
            .any(|offer| offer.kind == existing.offer.kind)
            || (existing.offer.kind == TransportKind::Wireguard
                && existing.wireguard_transport != publication.spec.wireguard_transport)
        {
            return Err(StoreError::Conflict(
                "这对机器已有其他承载的入口通道，请先撤销相关跨机器入口再更换传输方式".into(),
            ));
        }
        if !entry_caps.supports_entry_transport(existing.offer.kind, existing.wireguard_transport)
            || !target_caps
                .supports_entry_transport(existing.offer.kind, existing.wireguard_transport)
        {
            return Err(StoreError::Unsupported(
                "这对机器的 Agent 尚不支持当前入口通道，请先升级".into(),
            ));
        }
        return Ok(existing.id);
    }
    for node in [entry_node, target_node] {
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_entry_tunnels WHERE (entry_node=$1 OR target_node=$1) AND NOT cleaned")
            .bind(node)
            .fetch_one(&mut **tx)
            .await?;
        if count >= MAX_ENTRY_TUNNELS as i64 {
            return Err(invalid("机器的入口通道数量已达上限"));
        }
    }
    let template = publication
        .spec
        .transports
        .iter()
        .find(|offer| {
            entry_caps.supports_entry_transport(offer.kind, publication.spec.wireguard_transport)
                && target_caps
                    .supports_entry_transport(offer.kind, publication.spec.wireguard_transport)
        })
        .ok_or_else(|| {
            StoreError::Unsupported(
                "入口机器与计算宿主没有共同支持的入口传输，请检查两端 Agent 的能力".into(),
            )
        })?;
    let offer = bindings::resolve_offer(tx, target_node, template).await?;
    let (server, server_caps) = if offer.listen_side == TransportListenSide::Consumer {
        (target_node, &target_caps)
    } else {
        (entry_node, &entry_caps)
    };
    let protocol = if offer.kind == TransportKind::Xray
        || publication.spec.wireguard_transport == EntryWireguardTransport::Phantun
    {
        L4Protocol::Tcp
    } else {
        L4Protocol::Udp
    };
    let (scope, protocol, port) =
        bindings::transport_port_for_protocol(tx, server, &offer, server_caps, protocol).await?;
    let used = allocated_values(tx, "global", "entry-tunnel").await?;
    let slot = (0..ENTRY_TUNNEL_SLOTS)
        .find(|slot| !used.contains(&slot.to_string()))
        .ok_or_else(|| invalid("入口通道槽位已用尽"))?;
    let id = new_id("entry-tunnel")?;
    let sealed = crate::secrets::seal(
        &context(&id),
        &serde_json::to_string(&bindings::transport_credentials(offer.kind, server_caps)?)?,
    )?;
    let tunnel = EntryTunnel {
        id: id.clone(),
        side: EntryTunnelSide::Entry,
        entry_node: entry_node.into(),
        target_node: target_node.into(),
        offer: offer.clone(),
        wireguard_transport: if offer.kind == TransportKind::Wireguard {
            publication.spec.wireguard_transport
        } else {
            EntryWireguardTransport::Udp
        },
        port,
        slot,
        credential: String::new(),
        peer_credential: String::new(),
    };
    sqlx::query("INSERT INTO cvm_entry_tunnels(id,entry_node,target_node,target,sealed) VALUES($1,$2,$3,$4,$5)")
        .bind(&id)
        .bind(entry_node)
        .bind(target_node)
        .bind(serde_json::to_value(&tunnel)?)
        .bind(sealed)
        .execute(&mut **tx)
        .await?;
    claim_owned(
        tx,
        None,
        Some(&id),
        &scope,
        protocol,
        &format!("{}:{port}", offer.endpoint),
    )
    .await?;
    claim_owned(
        tx,
        None,
        Some(&id),
        "global",
        "entry-tunnel",
        &slot.to_string(),
    )
    .await?;
    wake_entries(tx, &[entry_node, target_node]).await?;
    Ok(id)
}

/// Withdraws every live tunnel that no longer carries an entry. Cleanup is confirmed
/// per side at the generation that first excludes it.
pub(super) async fn withdraw_unused(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    let idle = sqlx::query("SELECT t.id,t.entry_node,t.target_node FROM cvm_entry_tunnels t WHERE NOT t.withdrawn AND NOT EXISTS(SELECT 1 FROM cvm_entries e JOIN cvm_instances i ON i.id=e.instance_id WHERE NOT e.released AND e.provider_node=t.entry_node AND i.node_id=t.target_node) ORDER BY t.id FOR UPDATE OF t")
        .fetch_all(&mut **tx)
        .await?;
    for row in idle {
        let id: String = row.try_get("id")?;
        let entry_node: String = row.try_get("entry_node")?;
        let target_node: String = row.try_get("target_node")?;
        let generations = wake_entries(tx, &[&entry_node, &target_node]).await?;
        let side = |node: &str| {
            generations
                .get(node)
                .copied()
                .ok_or_else(|| invalid("entry tunnel generation missing"))
        };
        sqlx::query("UPDATE cvm_entry_tunnels SET withdrawn=true,entry_cleanup_generation=$2,target_cleanup_generation=$3 WHERE id=$1")
            .bind(&id)
            .bind(side(&entry_node)? as i64)
            .bind(side(&target_node)? as i64)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// The node's own side of each live tunnel, with that side's credentials.
pub(super) async fn desired(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    check_capabilities: bool,
) -> Result<Vec<EntryTunnel>> {
    let rows = sqlx::query("SELECT target,sealed FROM cvm_entry_tunnels WHERE (entry_node=$1 OR target_node=$1) AND NOT withdrawn ORDER BY id LIMIT $2")
        .bind(node)
        .bind(MAX_ENTRY_TUNNELS as i64 + 1)
        .fetch_all(&mut **tx)
        .await?;
    if rows.len() > MAX_ENTRY_TUNNELS {
        return Err(invalid("入口通道数量超出执行上限"));
    }
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut tunnel: EntryTunnel = serde_json::from_value(row.try_get("target")?)?;
        if check_capabilities
            && (tunnel.offer.kind == TransportKind::Xray || !tunnel.wireguard_transport.is_udp())
            && !capability(tx, node)
                .await?
                .supports_entry_transport(tunnel.offer.kind, tunnel.wireguard_transport)
        {
            return Err(StoreError::Unsupported(
                "本机 Agent 尚不支持此入口传输，请升级 Agent 后重试".into(),
            ));
        }
        // The entry machine takes the provider side of the transport, the host the consumer side.
        let (consumer, provider, consumer_peer, provider_peer): (String, String, String, String) =
            serde_json::from_str(&crate::secrets::open(
                &context(&tunnel.id),
                &row.try_get::<String, _>("sealed")?,
            )?)?;
        if tunnel.entry_node == node {
            tunnel.side = EntryTunnelSide::Entry;
            tunnel.credential = provider;
            tunnel.peer_credential = provider_peer;
        } else {
            tunnel.side = EntryTunnelSide::Target;
            tunnel.credential = consumer;
            tunnel.peer_credential = consumer_peer;
        }
        tunnel.validate().map_err(invalid)?;
        out.push(tunnel);
    }
    Ok(out)
}
