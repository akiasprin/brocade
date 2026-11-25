//! Container access entries. An entry belongs to one container, is delivered to the
//! container's primary NIC and never occupies a NIC slot. Its address and ports are
//! claimed in the shared network ledger, so user forwarding, NIC transports and host
//! services see the same conflicts. Releasing an entry keeps the claims until the entry
//! machine reports a generation without it.
use super::*;
use crate::cvm::releases::Scope;
use std::net::Ipv4Addr;

pub(crate) mod desired;
mod diagnostics;
pub(crate) mod policy;
mod tunnels;
pub use diagnostics::{CvmNetworkLogDomain, CvmNetworkLogs, CvmNetworkNodeLogs};

const PUBLICATION_SELECT: &str =
    "SELECT id,revision,admitting,deleted,spec FROM cvm_entry_publications";
const ENTRY_SELECT: &str = "SELECT e.id,e.publication_id,e.provider_node,e.instance_id,e.address,e.mode,e.revision,e.released,i.node_id AS host,i.tenant_id,coalesce(i.user_id,'') AS user_id FROM cvm_entries e JOIN cvm_instances i ON i.id=e.instance_id";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CvmEntryState {
    /// 节点尚未确认当前版本。
    Pending,
    Active,
    Failed,
    /// 入口机器或计算宿主近期没有回报。
    Offline,
    /// 发布已撤销该用户的授权，入口保留地址但不转发。
    Unauthorized,
    /// 容器未运行或尚未上报主网卡地址，入口保留地址但不转发。
    NoTarget,
    /// 已撤销，等待入口机器确认清理后释放地址与端口。
    Releasing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmEntryView {
    pub id: String,
    pub revision: u64,
    pub publication_id: String,
    pub provider_node: String,
    pub instance_id: String,
    /// 入口机器上匹配的地址。
    pub address: Ipv4Addr,
    /// 访问者使用的地址。
    pub visible_address: Ipv4Addr,
    pub mode: EntryMode,
    #[serde(default, skip_serializing_if = "EntryReservedPorts::is_empty")]
    pub reserved_ports: EntryReservedPorts,
    /// 容器不在入口机器上，经机器对通道投递。
    pub remote: bool,
    pub state: CvmEntryState,
    pub error: Option<String>,
    #[serde(default)]
    pub can_read_logs: bool,
    #[serde(default)]
    pub tunnel_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmEntryPortUse {
    pub protocol: L4Protocol,
    pub port_start: u16,
    pub port_end: u16,
    /// 当前账号可见的入口；他人的入口、转发、承载和宿主服务只显示为已占用。
    pub entry_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmEntryAddressUse {
    pub address: Ipv4Addr,
    /// 整 IP 已分配。
    pub whole_taken: bool,
    pub whole_entry_id: Option<String>,
    /// 共享地址在发布端口段内已被占用的端口。
    pub ports: Vec<CvmEntryPortUse>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmEntryPublicationView {
    #[serde(flatten)]
    pub publication: EntryPublication,
    pub provider_name: String,
    pub active_entries: u64,
    pub usage: Vec<CvmEntryAddressUse>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmEntryBoard {
    pub publications: Vec<CvmEntryPublicationView>,
    pub entries: Vec<CvmEntryView>,
    pub can_manage: bool,
}

#[derive(Debug, Clone)]
pub(super) struct StoredEntry {
    pub id: String,
    pub publication_id: String,
    pub provider_node: String,
    pub instance_id: String,
    pub address: Ipv4Addr,
    pub mode: EntryMode,
    pub revision: u64,
    pub released: bool,
    pub host: String,
    pub tenant_id: String,
    pub user_id: String,
}
fn stored(row: &sqlx::postgres::PgRow) -> Result<StoredEntry> {
    Ok(StoredEntry {
        id: row.try_get("id")?,
        publication_id: row.try_get("publication_id")?,
        provider_node: row.try_get("provider_node")?,
        instance_id: row.try_get("instance_id")?,
        address: row
            .try_get::<String, _>("address")?
            .parse()
            .map_err(|_| invalid("invalid stored entry address"))?,
        mode: serde_json::from_value(row.try_get("mode")?)?,
        revision: row.try_get::<i64, _>("revision")? as u64,
        released: row.try_get("released")?,
        host: row.try_get("host")?,
        tenant_id: row.try_get("tenant_id")?,
        user_id: row.try_get("user_id")?,
    })
}
fn publication_from(row: &sqlx::postgres::PgRow) -> Result<EntryPublication> {
    Ok(EntryPublication {
        id: row.try_get("id")?,
        revision: row.try_get::<i64, _>("revision")? as u64,
        admitting: row.try_get("admitting")?,
        retired: row.try_get("deleted")?,
        spec: serde_json::from_value(row.try_get("spec")?)?,
    })
}
async fn entry_publication(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<EntryPublication> {
    let row = sqlx::query(&format!("{PUBLICATION_SELECT} WHERE id=$1"))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("入口发布".into()))?;
    publication_from(&row)
}
fn entry_granted(p: &EntryPublication, tenant: &str, user: &str) -> bool {
    p.spec
        .grants
        .iter()
        .any(|g| g.tenant_id == tenant && g.user_id == user)
}
fn kind(protocol: L4Protocol) -> &'static str {
    match protocol {
        L4Protocol::Tcp => "tcp",
        L4Protocol::Udp => "udp",
    }
}

/// Raises the entry desired generation of each node once and returns the new values.
pub(super) async fn wake_entries(
    tx: &mut Transaction<'_, Postgres>,
    nodes: &[&str],
) -> Result<BTreeMap<String, u64>> {
    let mut unique: Vec<&str> = nodes.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let mut generations = BTreeMap::new();
    for row in sqlx::query("INSERT INTO cvm_entry_nodes(node_id) SELECT unnest($1::text[]) ON CONFLICT(node_id) DO UPDATE SET desired_generation=cvm_entry_nodes.desired_generation+1 RETURNING node_id,desired_generation")
        .bind(&unique)
        .fetch_all(&mut **tx)
        .await?
    {
        generations.insert(
            row.try_get("node_id")?,
            row.try_get::<i64, _>("desired_generation")? as u64,
        );
    }
    Ok(generations)
}
pub(super) async fn claim_owned(
    tx: &mut Transaction<'_, Postgres>,
    entry: Option<&str>,
    tunnel: Option<&str>,
    scope: &str,
    kind: &str,
    value: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO cvm_nic_resources(entry_id,tunnel_id,scope,kind,value) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(entry)
    .bind(tunnel)
    .bind(scope)
    .bind(kind)
    .bind(value)
    .execute(&mut **tx)
    .await
    .map_err(|error| match error {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            StoreError::Conflict("地址或端口已被占用".into())
        }
        other => other.into(),
    })?;
    Ok(())
}
async fn entry_locked(tx: &mut Transaction<'_, Postgres>, id: &str) -> Result<StoredEntry> {
    let row = sqlx::query(&format!("{ENTRY_SELECT} WHERE e.id=$1 FOR UPDATE OF e"))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("访问入口".into()))?;
    stored(&row)
}
/// A container that may receive entries: owned by the caller and not being deleted.
pub(super) async fn target_locked(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    id: &str,
) -> Result<CvmInstance> {
    let row = sqlx::query(&format!("{INSTANCE_SELECT} WHERE id=$1 FOR UPDATE"))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("CVM".into()))?;
    let instance = from_row(&row)?;
    require_manager(actor, &instance.tenant_id)?;
    if !instance.reserved || instance.desired.power == PowerState::Deleted {
        return Err(invalid("容器已进入删除流程"));
    }
    Ok(instance)
}
/// The container's host must deliver to its primary NIC; a remote entry machine also
/// needs the host's Agent to terminate the machine-pair tunnel.
async fn require_delivery(
    tx: &mut Transaction<'_, Postgres>,
    host: &str,
    provider: &str,
) -> Result<()> {
    let caps = host_locked(tx, host, false).await?;
    if !caps.primary_ip_delivery {
        return Err(StoreError::Unsupported(
            "容器宿主的运行栈尚不支持投递到主网卡".into(),
        ));
    }
    if host != provider && !capability(tx, host).await?.entries {
        return Err(StoreError::Unsupported(
            "容器宿主的 Agent 尚不支持访问入口".into(),
        ));
    }
    Ok(())
}

async fn require_protocol_policy(
    tx: &mut Transaction<'_, Postgres>,
    provider: &str,
    host: &str,
    mode: &EntryMode,
    reserved: &EntryReservedPorts,
) -> Result<()> {
    if mode.requires_protocol_rules(reserved) {
        for node in BTreeSet::from([provider, host]) {
            if !capability(tx, node).await?.entry_protocol_rules {
                return Err(StoreError::Unsupported(
                    "入口机器和计算宿主均需支持协议转发与保留端口，请先升级 Agent".into(),
                ));
            }
        }
    }
    Ok(())
}

/// Every non-entry claim conflicts on the port regardless of address, because user
/// forwarding, transports and host services listen on all addresses. Entries on other
/// addresses of the same machine do not conflict.
async fn port_available(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    provider: &NicNodeCapabilities,
    address: Ipv4Addr,
    protocol: L4Protocol,
    range: EntryPortRange,
    exclude_entry: Option<&str>,
) -> Result<()> {
    let EntryPortRange { start, end } = range;
    if exclude_entry.is_none()
        && policy::protected(tx, node, Some(address), protocol)
            .await?
            .iter()
            .any(|r| r.start <= end && start <= r.end)
    {
        return Err(StoreError::Conflict(
            "端口仍由入口机器保留，需发布并等待节点确认后才能分配".into(),
        ));
    }
    if provider
        .host_ports
        .iter()
        .any(|p| p.protocol == protocol && (start..=end).contains(&p.port))
    {
        return Err(StoreError::Conflict(
            "端口段包含入口机器正在使用的服务端口".into(),
        ));
    }
    if crate::port_reservations::static_ranges(tx, node, kind(protocol))
        .await?
        .iter()
        .any(|(low, high)| *low <= i32::from(end) && i32::from(start) <= *high)
    {
        return Err(StoreError::Conflict("端口段与机器的静态监听重叠".into()));
    }
    let forwarded: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM forwarding_rules WHERE node_id=$1 AND deleted_at IS NULL AND enabled AND listen_port BETWEEN $2 AND $3 AND (protocol=$4 OR protocol='tcp_udp'))")
        .bind(node).bind(i32::from(start)).bind(i32::from(end)).bind(kind(protocol)).fetch_one(&mut **tx).await?;
    let claimed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_nic_resources WHERE scope=$1 AND kind=$2 AND NOT released AND (split_part(value,':',array_length(string_to_array(value,':'),1)))::integer BETWEEN $3 AND $4 AND (entry_id IS NULL OR value LIKE $5) AND ($6::text IS NULL OR entry_id IS DISTINCT FROM $6))")
        .bind(format!("port:{node}")).bind(kind(protocol)).bind(i32::from(start)).bind(i32::from(end)).bind(format!("{address}:%")).bind(exclude_entry).fetch_one(&mut **tx).await?;
    if forwarded || claimed {
        return Err(StoreError::Conflict("端口段已被占用或仍在等待清理".into()));
    }
    Ok(())
}

/// One entry keeps its identity while individual protocol/port claims protect all writers.
async fn port_capacity(
    tx: &mut Transaction<'_, Postgres>,
    provider: &str,
    host: &str,
    additional_ports: u64,
    exclude: Option<&str>,
) -> Result<()> {
    for node in BTreeSet::from([provider, host]) {
        let cost:i64=sqlx::query_scalar("SELECT count(*) FROM cvm_nic_resources r JOIN cvm_entries e ON e.id=r.entry_id JOIN cvm_instances i ON i.id=e.instance_id WHERE NOT r.released AND r.kind IN ('tcp','udp') AND (e.provider_node=$1 OR i.node_id=$1) AND ($2::text IS NULL OR e.id<>$2)")
            .bind(node).bind(exclude).fetch_one(&mut **tx).await?;
        if cost as u64 + additional_ports > MAX_ENTRY_PORTS_PER_NODE {
            return Err(StoreError::Conflict(
                "机器的访问入口端口容量不足，请先缩小范围或清理已有入口".into(),
            ));
        }
    }
    Ok(())
}

/// The backbone identity remains reserved. Public host addresses and shared NAT
/// egress may also receive L3 forwarding: only new inbound flows are redirected.
async fn reserved_addresses(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
) -> Result<BTreeSet<String>> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT host(overlay_addr) FROM nodes WHERE id=$1 AND overlay_addr IS NOT NULL",
    )
    .bind(node)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect())
}

async fn require_host_l3(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    address: Ipv4Addr,
    caps: &NicNodeCapabilities,
) -> Result<()> {
    let primary:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM nodes WHERE id=$1 AND public_ipv4=$2) OR EXISTS(SELECT 1 FROM cvm_nic_publications WHERE provider_node=$1 AND NOT deleted AND spec->>'shared_ipv4'=$2)").bind(node).bind(address.to_string()).fetch_one(&mut **tx).await?;
    if !caps.entry_host_l3 && (primary || caps.addresses.contains(&address.into())) {
        return Err(StoreError::Unsupported(
            "入口机器尚不支持自身 IP 的 L3 转发，请升级 Agent".into(),
        ));
    }
    Ok(())
}

async fn require_native_reply_source(
    tx: &mut Transaction<'_, Postgres>,
    instance: &CvmInstance,
    provider: &str,
    mode: &EntryMode,
) -> Result<()> {
    let udp = mode.covers_udp() || mode.traffic().icmp || mode.traffic().other_ip;
    if provider != instance.node_id
        && !instance.desired.spec.execution.is_cube()
        && udp
        && !capability(tx, &instance.node_id).await?.entry_host_l3
    {
        return Err(StoreError::Unsupported(
            "裸机 L3／UDP 转发需要正确的回包选源，请升级裸机 Agent".into(),
        ));
    }
    Ok(())
}

/// Symmetric egress: the IPv4 source a NAT NIC on `nic.provider_node` uses for this
/// container, when the container owns a whole-IP entry on that machine.
pub(super) async fn entry_source(
    tx: &mut Transaction<'_, Postgres>,
    instance: &str,
    nic: &AllocatedNic,
) -> Result<Option<Ipv4Addr>> {
    if nic.address_mode == NicAddressMode::DedicatedRouted || nic.ipv4.is_none() {
        return Ok(None);
    }
    let address: Option<String> = sqlx::query_scalar("SELECT address FROM cvm_entries WHERE instance_id=$1 AND provider_node=$2 AND NOT released AND EXISTS(SELECT 1 FROM cvm_nic_resources r WHERE r.entry_id=cvm_entries.id AND r.kind='ipv4' AND NOT r.released) ORDER BY created_at,id LIMIT 1")
        .bind(instance)
        .bind(&nic.provider_node)
        .fetch_optional(&mut **tx)
        .await?;
    address
        .map(|a| {
            a.parse()
                .map_err(|_| invalid("invalid stored entry address"))
        })
        .transpose()
}
/// Updates the symmetric egress source of every live binding of the container.
pub(super) async fn sync_entry_sources(
    tx: &mut Transaction<'_, Postgres>,
    instance: &str,
) -> Result<()> {
    let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM cvm_nic_bindings WHERE instance_id=$1 AND NOT cleaned AND phase<>'withdrawing' ORDER BY id").bind(instance).fetch_all(&mut **tx).await?;
    for id in ids {
        let mut binding = bindings::binding_value(tx, &id).await?;
        let wanted = entry_source(tx, instance, &binding.nic).await?;
        if binding.entry_source_ipv4 != wanted {
            binding.entry_source_ipv4 = wanted;
            binding.generation += 1;
            bindings::persist_binding(tx, &binding).await?;
        }
    }
    Ok(())
}

/// Marks an entry released at the next entry-machine generation and wakes its host.
async fn release(
    tx: &mut Transaction<'_, Postgres>,
    entry: &StoredEntry,
    actor: &str,
) -> Result<()> {
    let generations = wake_entries(tx, &[&entry.provider_node, &entry.host]).await?;
    let cleanup = generations
        .get(&entry.provider_node)
        .copied()
        .ok_or_else(|| invalid("entry machine generation missing"))?;
    sqlx::query("UPDATE cvm_entries SET released=true,cleanup_generation=$2,revision=revision+1,actor_id=$3,updated_at=now() WHERE id=$1")
        .bind(&entry.id)
        .bind(cleanup as i64)
        .bind(actor)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Wakes the entry machines and the host of a container whose delivery address changed.
pub(crate) async fn wake_instance(
    tx: &mut Transaction<'_, Postgres>,
    instance: &str,
    host: &str,
) -> Result<()> {
    let mut nodes: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT provider_node FROM cvm_entries WHERE instance_id=$1 AND NOT released",
    )
    .bind(instance)
    .fetch_all(&mut **tx)
    .await?;
    if nodes.is_empty() {
        return Ok(());
    }
    nodes.push(host.into());
    wake_entries(tx, &nodes.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    Ok(())
}

/// Releases every live entry of a container in the deletion transaction (Q3).
pub(crate) async fn release_instance(
    tx: &mut Transaction<'_, Postgres>,
    instance: &str,
) -> Result<()> {
    let rows = sqlx::query(&format!(
        "{ENTRY_SELECT} WHERE e.instance_id=$1 AND NOT e.released ORDER BY e.id FOR UPDATE OF e"
    ))
    .bind(instance)
    .fetch_all(&mut **tx)
    .await?;
    for row in rows {
        release(tx, &stored(&row)?, "system-container-delete").await?;
    }
    tunnels::withdraw_unused(tx).await?;
    sync_entry_sources(tx, instance).await
}

/// A retired machine can never confirm cleanup. Its entries and tunnels end at once;
/// the surviving peer of each tunnel still converges to the withdrawal.
pub(crate) async fn retire_node(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<()> {
    lock(tx).await?;
    sqlx::query("UPDATE cvm_entry_publications SET admitting=false,deleted=true,updated_at=now() WHERE provider_node=$1").bind(node).execute(&mut **tx).await?;
    let rows = sqlx::query(&format!("{ENTRY_SELECT} WHERE (e.provider_node=$1 OR i.node_id=$1) AND NOT e.released ORDER BY e.id FOR UPDATE OF e")).bind(node).fetch_all(&mut **tx).await?;
    let mut instances = BTreeSet::new();
    for row in rows {
        let entry = stored(&row)?;
        release(tx, &entry, "system-node-retired").await?;
        instances.insert(entry.instance_id);
    }
    tunnels::withdraw_unused(tx).await?;
    sqlx::query(
        "UPDATE cvm_entry_nodes SET applied_generation=desired_generation WHERE node_id=$1",
    )
    .bind(node)
    .execute(&mut **tx)
    .await?;
    desired::cleanup(tx, node).await?;
    for instance in instances {
        sync_entry_sources(tx, &instance).await?;
    }
    Ok(())
}

impl PgStore {
    pub async fn write_cvm_entry_publication(
        &self,
        actor: &AdminContext,
        id: Option<&str>,
        mut request: EntryPublicationWrite,
    ) -> Result<EntryPublication> {
        admin(actor)?;
        for offer in &mut request.spec.addresses {
            offer.reserved_ports = offer.reserved_ports.normalized().map_err(invalid)?;
        }
        request.spec.validate().map_err(invalid)?;
        let digest = request_digest(&("entry-publication", id, &request))?;
        let mut tx = self.pool.begin().await?;
        crate::cvm::releases::lock(&mut tx).await?;
        let cvm_before = crate::cvm::releases::begin(
            &mut tx,
            Scope {
                nodes: vec![&request.spec.provider_node],
                entry_publications: id.into_iter().collect(),
                ..Scope::default()
            },
        )
        .await?;
        if let Some(value) = replay(&mut tx, actor, &request.request_id, &digest).await? {
            return Ok(serde_json::from_value(value)?);
        }
        let spec = &request.spec;
        let old = match id {
            Some(id) => Some(entry_publication(&mut tx, id).await?),
            None => None,
        };
        if let Some(old) = &old {
            if old.spec.transports != spec.transports
                || old.spec.wireguard_transport != spec.wireguard_transport
            {
                let in_use: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_entries e JOIN cvm_instances i ON i.id=e.instance_id WHERE e.publication_id=$1 AND NOT e.cleaned AND e.provider_node<>i.node_id)").bind(&old.id).fetch_one(&mut *tx).await?;
                if in_use {
                    return Err(StoreError::Conflict(
                        "仍有跨机器入口在用，请先撤销并等待清理后更换传输方式".into(),
                    ));
                }
            }
        }
        let (id, revision, admitting) = match &old {
            Some(old) => {
                if old.retired || old.revision != request.expected_revision {
                    return Err(StoreError::Conflict("入口发布已变化，请刷新后操作".into()));
                }
                if old.spec.provider_node != spec.provider_node {
                    return Err(invalid("入口机器属于发布身份，更换机器请新建发布"));
                }
                (
                    old.id.clone(),
                    old.revision
                        .checked_add(1)
                        .ok_or_else(|| invalid("发布版本已满"))?,
                    old.admitting,
                )
            }
            None => {
                let count: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM cvm_entry_publications WHERE NOT deleted",
                )
                .fetch_one(&mut *tx)
                .await?;
                if request.expected_revision != 0 || count >= MAX_ENTRY_PUBLICATIONS as i64 {
                    return Err(invalid("入口发布版本或数量超出限制"));
                }
                (new_id("entrypub")?, 1, true)
            }
        };
        // Authorization stays editable while the machine is offline, so a grant can
        // always be revoked; every other change needs fresh machine evidence.
        let authorization_only = old.as_ref().is_some_and(|old| {
            let mut before = old.spec.clone();
            let mut after = spec.clone();
            before.grants.clear();
            after.grants.clear();
            before == after
        });
        if !authorization_only {
            let provider = capability(&mut tx, &spec.provider_node).await?;
            if !provider.entries {
                return Err(StoreError::Unsupported(
                    "入口机器的 Agent 尚不支持访问入口".into(),
                ));
            }
            if spec
                .transports
                .iter()
                .any(|t| !provider.supports_entry_transport(t.kind, spec.wireguard_transport))
            {
                return Err(StoreError::Unsupported(
                    "入口机器不支持所选的通道承载".into(),
                ));
            }
            if spec.addresses.iter().any(|o| !o.reserved_ports.is_empty())
                && !provider.entry_protocol_rules
            {
                return Err(StoreError::Unsupported(
                    "保留端口需要支持协议入口规则的 Agent".into(),
                ));
            }
            let reserved = reserved_addresses(&mut tx, &spec.provider_node).await?;
            for offer in &spec.addresses {
                match offer.kind {
                    EntryAddressKind::Shared { .. } => {
                        if !provider.addresses.contains(&offer.address.into()) {
                            return Err(invalid(format!(
                                "共享入口地址 {} 不是入口机器网卡上的实际地址",
                                offer.address
                            )));
                        }
                    }
                    EntryAddressKind::Dedicated => {
                        require_host_l3(&mut tx, &spec.provider_node, offer.address, &provider)
                            .await?;
                        if reserved.contains(&offer.address.to_string()) {
                            return Err(invalid(format!(
                                "{} 是组网通道地址，不能作为 L3 转发入口",
                                offer.address
                            )));
                        }
                    }
                }
            }
            let others:Vec<serde_json::Value>=sqlx::query_scalar("SELECT spec FROM cvm_entry_publications WHERE provider_node=$1 AND NOT deleted AND id<>$2").bind(&spec.provider_node).bind(&id).fetch_all(&mut *tx).await?;
            for other in others {
                let other: EntryPublicationSpec = serde_json::from_value(other)?;
                if spec
                    .addresses
                    .iter()
                    .any(|offer| other.offer(offer.address).is_some())
                {
                    return Err(StoreError::Conflict(
                        "同一地址已在这台机器的另一份入口发布中".into(),
                    ));
                }
            }
        }
        for offer in &spec.addresses {
            for protocol in [L4Protocol::Tcp, L4Protocol::Udp] {
                for range in offer.reserved_ports.for_protocol(protocol) {
                    let occupied:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_nic_resources WHERE scope=$1 AND kind=$2 AND NOT released AND entry_id IS NULL AND (substring(value from '[0-9]+$'))::integer BETWEEN $3 AND $4)").bind(format!("port:{}",spec.provider_node)).bind(kind(protocol)).bind(i32::from(range.start)).bind(i32::from(range.end)).fetch_one(&mut *tx).await?;
                    if occupied {
                        return Err(StoreError::Conflict(
                            "保留端口与已有网卡或入口通道承载重叠，请先更换承载并等待清理".into(),
                        ));
                    }
                    if matches!(offer.kind, EntryAddressKind::Shared { .. }) {
                        let leased:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_nic_resources WHERE scope=$1 AND kind=$2 AND NOT released AND entry_id IS NOT NULL AND value LIKE $3 AND (substring(value from '[0-9]+$'))::integer BETWEEN $4 AND $5)").bind(format!("port:{}",spec.provider_node)).bind(kind(protocol)).bind(format!("{}:%",offer.address)).bind(i32::from(range.start)).bind(i32::from(range.end)).fetch_one(&mut *tx).await?;
                        if leased {
                            return Err(StoreError::Conflict(
                                "保留端口已分配给共享入口，请先撤销相关入口并等待清理".into(),
                            ));
                        }
                    }
                }
            }
        }
        for owner in &spec.grants {
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE tenant_id=$1 AND id=$2 AND status='active')").bind(&owner.tenant_id).bind(&owner.user_id).fetch_one(&mut *tx).await?;
            if !exists {
                return Err(invalid("授权用户不存在或已停用"));
            }
        }
        // Live entries keep their address and mode; a revision may not strand them.
        for row in sqlx::query(&format!(
            "{ENTRY_SELECT} WHERE e.publication_id=$1 AND NOT e.cleaned ORDER BY e.id"
        ))
        .bind(&id)
        .fetch_all(&mut *tx)
        .await?
        {
            let entry = stored(&row)?;
            if !spec
                .offer(entry.address)
                .is_some_and(|offer| offer.permits(&entry.mode))
            {
                return Err(StoreError::Conflict(format!(
                    "{} 仍有入口在用，请先撤销或保留该地址与端口",
                    entry.address
                )));
            }
            let offer = spec
                .offer(entry.address)
                .expect("validated live entry offer");
            if !authorization_only
                && old.as_ref().and_then(|p| p.spec.offer(entry.address)) != Some(offer)
                && matches!(offer.kind, EntryAddressKind::Shared { .. })
                && !entry.released
            {
                let mode = offer.resolve_mode(&entry.mode).map_err(invalid)?;
                let count: u64 = [L4Protocol::Tcp, L4Protocol::Udp]
                    .into_iter()
                    .map(|p| {
                        mode.protocol_segments(p, &offer.reserved_ports)
                            .map(|s| s.iter().map(|s| s.listen.count()).sum::<u64>())
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(invalid)?
                    .into_iter()
                    .sum();
                if count == 0 || count > u64::from(MAX_ENTRY_RANGE_PORTS) * 2 {
                    return Err(invalid("共享入口的有效端口为空或超出容量"));
                }
                let provider = capability(&mut tx, &spec.provider_node).await?;
                for protocol in [L4Protocol::Tcp, L4Protocol::Udp] {
                    for segment in mode
                        .protocol_segments(protocol, &offer.reserved_ports)
                        .map_err(invalid)?
                    {
                        port_available(
                            &mut tx,
                            &spec.provider_node,
                            &provider,
                            offer.address,
                            protocol,
                            segment.listen,
                            Some(&entry.id),
                        )
                        .await?;
                        sqlx::query("INSERT INTO cvm_nic_resources(entry_id,scope,kind,value) SELECT $1,$2,$3,$4||':'||p FROM generate_series($5::integer,$6::integer) p WHERE NOT EXISTS(SELECT 1 FROM cvm_nic_resources WHERE entry_id=$1 AND kind=$3 AND NOT released AND value=$4||':'||p)").bind(&entry.id).bind(format!("port:{}",spec.provider_node)).bind(kind(protocol)).bind(offer.address.to_string()).bind(i32::from(segment.listen.start)).bind(i32::from(segment.listen.end)).execute(&mut *tx).await?;
                    }
                }
            }
        }
        if !authorization_only {
            let hosts:Vec<String>=sqlx::query_scalar("SELECT DISTINCT i.node_id FROM cvm_entries e JOIN cvm_instances i ON i.id=e.instance_id WHERE e.publication_id=$1 AND NOT e.cleaned").bind(&id).fetch_all(&mut *tx).await?;
            for host in hosts {
                port_capacity(&mut tx, &spec.provider_node, &host, 0, None).await?;
            }
        }
        let p = EntryPublication {
            id: id.clone(),
            revision,
            admitting,
            retired: false,
            spec: request.spec.clone(),
        };
        sqlx::query("INSERT INTO cvm_entry_publications(id,provider_node,revision,admitting,spec,actor_id) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,spec=excluded.spec,actor_id=excluded.actor_id,updated_at=now()")
            .bind(&id).bind(&p.spec.provider_node).bind(revision as i64).bind(admitting).bind(serde_json::to_value(&p.spec)?).bind(actor.operator_id()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO cvm_entry_publication_versions(publication_id,revision,spec,actor_id) VALUES($1,$2,$3,$4)")
            .bind(&id).bind(revision as i64).bind(serde_json::to_value(&p.spec)?).bind(actor.operator_id()).execute(&mut *tx).await?;
        // Authorization is live: rules of revoked owners stop forwarding at once.
        let mut nodes = vec![p.spec.provider_node.clone()];
        nodes.extend(sqlx::query_scalar::<_,String>("SELECT DISTINCT i.node_id FROM cvm_entries e JOIN cvm_instances i ON i.id=e.instance_id WHERE e.publication_id=$1 AND NOT e.released").bind(&id).fetch_all(&mut *tx).await?);
        wake_entries(
            &mut tx,
            &nodes.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .await?;
        remember(&mut tx, actor, &request.request_id, &digest, Some(&id), &p).await?;
        crate::cvm::releases::commit(&mut tx, actor, cvm_before).await?;
        tx.commit().await?;
        Ok(p)
    }
    pub async fn set_cvm_entry_admission(
        &self,
        actor: &AdminContext,
        id: &str,
        request: PublicationAdmission,
    ) -> Result<EntryPublication> {
        admin(actor)?;
        let digest = request_digest(&("entry-admission", id, &request))?;
        let mut tx = self.pool.begin().await?;
        crate::cvm::releases::lock(&mut tx).await?;
        let cvm_before = crate::cvm::releases::begin(
            &mut tx,
            Scope {
                entry_publications: vec![id],
                ..Scope::default()
            },
        )
        .await?;
        if let Some(value) = replay(&mut tx, actor, &request.request_id, &digest).await? {
            return Ok(serde_json::from_value(value)?);
        }
        let mut p = entry_publication(&mut tx, id).await?;
        if p.retired || p.revision != request.expected_revision {
            return Err(StoreError::Conflict("入口发布已变化".into()));
        }
        p.admitting = request.admitting;
        sqlx::query("UPDATE cvm_entry_publications SET admitting=$2,actor_id=$3,updated_at=now() WHERE id=$1")
            .bind(id).bind(p.admitting).bind(actor.operator_id()).execute(&mut *tx).await?;
        remember(&mut tx, actor, &request.request_id, &digest, Some(id), &p).await?;
        crate::cvm::releases::commit(&mut tx, actor, cvm_before).await?;
        tx.commit().await?;
        Ok(p)
    }
    pub async fn delete_cvm_entry_publication(
        &self,
        actor: &AdminContext,
        id: &str,
        request: PublicationAdmission,
    ) -> Result<EntryPublication> {
        admin(actor)?;
        let digest = request_digest(&("entry-delete", id, &request))?;
        let mut tx = self.pool.begin().await?;
        crate::cvm::releases::lock(&mut tx).await?;
        let cvm_before = crate::cvm::releases::begin(
            &mut tx,
            Scope {
                entry_publications: vec![id],
                ..Scope::default()
            },
        )
        .await?;
        if let Some(value) = replay(&mut tx, actor, &request.request_id, &digest).await? {
            return Ok(serde_json::from_value(value)?);
        }
        let mut p = entry_publication(&mut tx, id).await?;
        if p.revision != request.expected_revision {
            return Err(StoreError::Conflict("入口发布已变化".into()));
        }
        let live: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM cvm_entries WHERE publication_id=$1 AND NOT cleaned)",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if live {
            return Err(StoreError::Conflict(
                "请先撤销该发布上的入口，并等待入口机器确认清理".into(),
            ));
        }
        p.retired = true;
        p.admitting = false;
        sqlx::query("UPDATE cvm_entry_publications SET deleted=true,admitting=false,actor_id=$2,updated_at=now() WHERE id=$1")
            .bind(id).bind(actor.operator_id()).execute(&mut *tx).await?;
        remember(&mut tx, actor, &request.request_id, &digest, Some(id), &p).await?;
        crate::cvm::releases::commit(&mut tx, actor, cvm_before).await?;
        tx.commit().await?;
        Ok(p)
    }

    pub async fn create_cvm_entry(
        &self,
        actor: &AdminContext,
        request: EntryCreate,
    ) -> Result<CvmEntryView> {
        request_id(&request.request_id)?;
        let digest = request_digest(&("entry-create", &request))?;
        let mut tx = self.pool.begin().await?;
        crate::cvm::releases::lock(&mut tx).await?;
        let cvm_before = crate::cvm::releases::begin(
            &mut tx,
            Scope {
                instances: vec![&request.instance_id],
                entry_publications: vec![&request.publication_id],
                ..Scope::default()
            },
        )
        .await?;
        if let Some(value) = replay(&mut tx, actor, &request.request_id, &digest).await? {
            tx.commit().await?;
            return Ok(serde_json::from_value(value)?);
        }
        let instance = target_locked(&mut tx, actor, &request.instance_id).await?;
        let publication_id = request.publication_id;
        let id = new_id("entry")?;
        add_entry_tx(
            &mut tx,
            actor,
            &instance,
            &id,
            &super::network_edit::CvmNetworkEntryAddition {
                publication_id: publication_id.clone(),
                address: request.address,
                mode: request.mode,
            },
        )
        .await?;
        sync_entry_sources(&mut tx, &instance.desired.id).await?;
        let view = entry_view_tx(&mut tx, actor, &id).await?;
        remember(
            &mut tx,
            actor,
            &request.request_id,
            &digest,
            Some(&publication_id),
            &view,
        )
        .await?;
        crate::cvm::releases::commit(&mut tx, actor, cvm_before).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Moves an entry to another container of the same user. The address and ports
    /// stay claimed throughout; only the delivery target changes.
    pub async fn repoint_cvm_entry(
        &self,
        actor: &AdminContext,
        id: &str,
        request: EntryRepoint,
    ) -> Result<CvmEntryView> {
        request_id(&request.request_id)?;
        let digest = request_digest(&("entry-repoint", id, &request))?;
        let mut tx = self.pool.begin().await?;
        crate::cvm::releases::lock(&mut tx).await?;
        let cvm_before = crate::cvm::releases::begin(
            &mut tx,
            Scope {
                instances: vec![&request.instance_id],
                entries: vec![id],
                ..Scope::default()
            },
        )
        .await?;
        if let Some(value) = replay(&mut tx, actor, &request.request_id, &digest).await? {
            tx.commit().await?;
            return Ok(serde_json::from_value(value)?);
        }
        let entry = entry_locked(&mut tx, id).await?;
        require_manager(actor, &entry.tenant_id)?;
        if entry.released || entry.revision != request.expected_revision {
            return Err(StoreError::Conflict("入口已变化，请刷新后操作".into()));
        }
        if entry.instance_id == request.instance_id {
            return Err(invalid("入口已经指向这个容器"));
        }
        // Lock both containers in a stable order so concurrent swaps cannot deadlock.
        let mut ids = [entry.instance_id.as_str(), request.instance_id.as_str()];
        ids.sort_unstable();
        sqlx::query("SELECT id FROM cvm_instances WHERE id=ANY($1::text[]) ORDER BY id FOR UPDATE")
            .bind(&ids[..])
            .fetch_all(&mut *tx)
            .await?;
        let target = target_locked(&mut tx, actor, &request.instance_id).await?;
        if target.tenant_id != entry.tenant_id || target.user() != entry.user_id {
            return Err(StoreError::Forbidden("入口只能改派给同一用户的容器".into()));
        }
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM cvm_entries WHERE instance_id=$1 AND NOT released",
        )
        .bind(&target.desired.id)
        .fetch_one(&mut *tx)
        .await?;
        if count >= MAX_ENTRIES_PER_INSTANCE as i64 {
            return Err(invalid("目标容器的入口数量已达上限"));
        }
        if !capability(&mut tx, &entry.provider_node).await?.entries {
            return Err(StoreError::Unsupported(
                "入口机器的 Agent 尚不支持访问入口".into(),
            ));
        }
        let p = entry_publication(&mut tx, &entry.publication_id).await?;
        let reserved = p
            .spec
            .offer(entry.address)
            .map(|o| o.reserved_ports.clone())
            .unwrap_or_default();
        require_protocol_policy(
            &mut tx,
            &entry.provider_node,
            &target.node_id,
            &entry.mode,
            &reserved,
        )
        .await?;
        require_delivery(&mut tx, &target.node_id, &entry.provider_node).await?;
        require_native_reply_source(&mut tx, &target, &entry.provider_node, &entry.mode).await?;
        let entry_port_count=sqlx::query_scalar::<_,i64>("SELECT count(*) FROM cvm_nic_resources WHERE entry_id=$1 AND NOT released AND kind IN ('tcp','udp')").bind(&entry.id).fetch_one(&mut *tx).await? as u64;
        port_capacity(
            &mut tx,
            &entry.provider_node,
            &target.node_id,
            entry_port_count,
            Some(&entry.id),
        )
        .await?;
        if target.node_id != entry.provider_node {
            tunnels::ensure(&mut tx, &entry.provider_node, &target.node_id, &p).await?;
        }
        sqlx::query("UPDATE cvm_entries SET instance_id=$2,revision=revision+1,actor_id=$3,updated_at=now() WHERE id=$1")
            .bind(id).bind(&target.desired.id).bind(actor.operator_id()).execute(&mut *tx).await?;
        tunnels::withdraw_unused(&mut tx).await?;
        wake_entries(
            &mut tx,
            &[&entry.provider_node, &entry.host, &target.node_id],
        )
        .await?;
        sync_entry_sources(&mut tx, &entry.instance_id).await?;
        sync_entry_sources(&mut tx, &target.desired.id).await?;
        let view = entry_view_tx(&mut tx, actor, id).await?;
        remember(
            &mut tx,
            actor,
            &request.request_id,
            &digest,
            Some(&entry.publication_id),
            &view,
        )
        .await?;
        crate::cvm::releases::commit(&mut tx, actor, cvm_before).await?;
        tx.commit().await?;
        Ok(view)
    }

    pub async fn release_cvm_entry(
        &self,
        actor: &AdminContext,
        id: &str,
        request: EntryRelease,
    ) -> Result<CvmEntryView> {
        request_id(&request.request_id)?;
        let digest = request_digest(&("entry-release", id, &request))?;
        let mut tx = self.pool.begin().await?;
        crate::cvm::releases::lock(&mut tx).await?;
        let cvm_before = crate::cvm::releases::begin(
            &mut tx,
            Scope {
                entries: vec![id],
                ..Scope::default()
            },
        )
        .await?;
        if let Some(value) = replay(&mut tx, actor, &request.request_id, &digest).await? {
            tx.commit().await?;
            return Ok(serde_json::from_value(value)?);
        }
        let entry = release_entry_tx(&mut tx, actor, id, request.expected_revision, None).await?;
        finish_entry_edits(&mut tx, &entry.instance_id, true).await?;
        let view = entry_view_tx(&mut tx, actor, id).await?;
        remember(
            &mut tx,
            actor,
            &request.request_id,
            &digest,
            Some(&entry.publication_id),
            &view,
        )
        .await?;
        crate::cvm::releases::commit(&mut tx, actor, cvm_before).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Everything the composition board draws on the entry side, scoped to the caller.
    pub async fn cvm_entry_board(&self, actor: &AdminContext) -> Result<CvmEntryBoard> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let entries = desired::views(&mut tx, actor, None).await?;
        let visible: BTreeSet<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        let mut publications = Vec::new();
        for row in sqlx::query(&format!("SELECT p.id,p.revision,p.admitting,p.deleted,p.spec,n.name FROM cvm_entry_publications p JOIN nodes n ON n.id=p.provider_node WHERE NOT p.deleted ORDER BY n.name,p.id LIMIT {MAX_ENTRY_PUBLICATIONS}")).fetch_all(&mut *tx).await? {
            let mut publication = publication_from(&row)?;
            if !actor.is_system_admin() {
                publication
                    .spec
                    .grants
                    .retain(|owner| view_allowed(actor, &owner.tenant_id, &owner.user_id));
                if publication.spec.grants.is_empty() {
                    continue;
                }
                publication.spec.transports.clear();
            }
            let node = publication.spec.provider_node.clone();
            let active: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_entries WHERE publication_id=$1 AND NOT cleaned")
                .bind(&publication.id).fetch_one(&mut *tx).await?;
            let mut usage = Vec::new();
            for offer in &publication.spec.addresses {
                usage.push(address_use(&mut tx, &node, offer, &visible).await?);
            }
            if actor.role() == AdminRole::Readonly {
                for offer in &mut publication.spec.addresses {
                    offer.address = Ipv4Addr::UNSPECIFIED;
                    offer.public_address = None;
                }
                for item in &mut usage {
                    item.address = Ipv4Addr::UNSPECIFIED;
                }
            }
            publications.push(CvmEntryPublicationView {
                publication,
                provider_name: row.try_get("name")?,
                active_entries: active as u64,
                usage,
            });
        }
        tx.commit().await?;
        Ok(CvmEntryBoard {
            publications,
            entries,
            can_manage: actor.is_system_admin(),
        })
    }
    pub async fn cvm_instance_entries(
        &self,
        actor: &AdminContext,
        instance: &str,
    ) -> Result<Vec<CvmEntryView>> {
        // Ownership is checked through the container itself.
        self.cvm_instance(actor, instance).await?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let entries = desired::views(&mut tx, actor, Some(instance)).await?;
        tx.commit().await?;
        Ok(entries)
    }
}

/// Stage an entry and its claims without committing the request or a global revision.
pub(super) async fn add_entry_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    instance: &CvmInstance,
    id: &str,
    request: &super::network_edit::CvmNetworkEntryAddition,
) -> Result<()> {
    if !instance.reserved || instance.desired.power == PowerState::Deleted {
        return Err(invalid("容器已进入删除流程"));
    }
    let p = entry_publication(tx, &request.publication_id).await?;
    if p.retired || !p.admitting {
        return Err(StoreError::Conflict("该入口发布已停止新增".into()));
    }
    if !entry_granted(&p, &instance.tenant_id, instance.user()) {
        return Err(StoreError::Forbidden("尚未获得该入口地址的授权".into()));
    }
    let offer = p
        .spec
        .offer(request.address)
        .ok_or_else(|| invalid("地址不属于这份入口发布"))?;
    if !request.mode.valid() {
        return Err(invalid(format!(
            "端口须在 1–65535，入口与目标范围必须等长，且每条最多 {MAX_ENTRY_RANGE_PORTS} 个端口"
        )));
    }
    if !offer.permits(&request.mode) {
        return Err(invalid(match offer.kind {
            EntryAddressKind::Shared { .. } => "共享地址只能按协议端口在发布的端口段内分配",
            EntryAddressKind::Dedicated => "独立地址只能整 IP 交给一个容器",
        }));
    }
    let node = p.spec.provider_node.clone();
    let counts: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM cvm_entries WHERE publication_id=$1 AND NOT cleaned),(SELECT count(*) FROM cvm_entries WHERE instance_id=$2 AND NOT released),(SELECT count(*) FROM cvm_entries WHERE provider_node=$3 AND NOT cleaned)")
        .bind(&p.id).bind(&instance.desired.id).bind(&node).fetch_one(&mut **tx).await?;
    if counts.0 >= i64::from(p.spec.max_entries) {
        return Err(StoreError::Conflict("这份入口发布的容量已满".into()));
    }
    if counts.1 >= MAX_ENTRIES_PER_INSTANCE as i64 || counts.2 >= MAX_ENTRIES_PER_NODE as i64 {
        return Err(invalid("入口数量超出容器或机器上限"));
    }
    let provider = capability(tx, &node).await?;
    if !provider.entries {
        return Err(StoreError::Unsupported(
            "入口机器的 Agent 尚不支持访问入口".into(),
        ));
    }
    require_delivery(tx, &instance.node_id, &node).await?;
    let mode = offer.resolve_mode(&request.mode).map_err(invalid)?;
    require_protocol_policy(tx, &node, &instance.node_id, &mode, &offer.reserved_ports).await?;
    require_native_reply_source(tx, instance, &node, &mode).await?;
    let segments = if matches!(offer.kind, EntryAddressKind::Shared { .. }) {
        [L4Protocol::Tcp, L4Protocol::Udp]
            .into_iter()
            .map(|p| {
                mode.protocol_segments(p, &offer.reserved_ports)
                    .map(|s| (p, s))
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(invalid)?
    } else {
        vec![]
    };
    let cost = segments
        .iter()
        .flat_map(|(_, s)| s)
        .map(|s| s.listen.count())
        .sum::<u64>();
    if matches!(offer.kind, EntryAddressKind::Shared { .. })
        && (cost == 0 || cost > u64::from(MAX_ENTRY_RANGE_PORTS) * 2)
    {
        return Err(invalid("共享入口有效端口为空或超过容量上限"));
    }
    port_capacity(tx, &node, &instance.node_id, cost, None).await?;
    sqlx::query("INSERT INTO cvm_entries(id,publication_id,provider_node,instance_id,address,mode,actor_id) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(id).bind(&p.id).bind(&node).bind(&instance.desired.id).bind(request.address.to_string()).bind(serde_json::to_value(request.mode.normalized().map_err(invalid)?)?).bind(actor.operator_id()).execute(&mut **tx).await?;
    match offer.kind {
        EntryAddressKind::Dedicated => {
            require_host_l3(tx, &node, request.address, &provider).await?;
            if reserved_addresses(tx, &node)
                .await?
                .contains(&request.address.to_string())
            {
                return Err(invalid("该地址由组网通道使用，不能作为 L3 转发入口"));
            }
            let ports:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_nic_resources WHERE scope=$1 AND NOT released AND entry_id IS NOT NULL AND value LIKE $2)")
                .bind(format!("port:{node}")).bind(format!("{}:%", request.address)).fetch_one(&mut **tx).await?;
            if ports {
                return Err(StoreError::Conflict("该地址上仍有端口占用".into()));
            }
            claim_owned(
                tx,
                Some(id),
                None,
                &format!("public:{node}"),
                "ipv4",
                &request.address.to_string(),
            )
            .await?;
        }
        EntryAddressKind::Shared { .. } => {
            for (protocol, parts) in &segments {
                for segment in parts {
                    port_available(
                        tx,
                        &node,
                        &provider,
                        request.address,
                        *protocol,
                        segment.listen,
                        None,
                    )
                    .await?;
                    sqlx::query("INSERT INTO cvm_nic_resources(entry_id,scope,kind,value) SELECT $1,$2,$3,$4||':'||port::text FROM generate_series($5::integer,$6::integer) port")
                        .bind(id).bind(format!("port:{node}")).bind(kind(*protocol)).bind(request.address.to_string()).bind(i32::from(segment.listen.start)).bind(i32::from(segment.listen.end)).execute(&mut **tx).await?;
                }
            }
        }
    }

    if instance.node_id != node {
        tunnels::ensure(tx, &node, &instance.node_id, &p).await?;
    }
    wake_entries(tx, &[&node, &instance.node_id]).await?;
    Ok(())
}

pub(super) async fn release_entry_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    id: &str,
    expected_revision: u64,
    expected_instance: Option<&str>,
) -> Result<StoredEntry> {
    let entry = entry_locked(tx, id).await?;
    require_manager(actor, &entry.tenant_id)?;
    if expected_instance.is_some_and(|instance| instance != entry.instance_id) {
        return Err(StoreError::NotFound("此容器没有该访问入口".into()));
    }
    if entry.released || entry.revision != expected_revision {
        return Err(StoreError::Conflict("入口已变化，请刷新后操作".into()));
    }
    sqlx::query("SELECT id FROM cvm_instances WHERE id=$1 FOR UPDATE")
        .bind(&entry.instance_id)
        .fetch_one(&mut **tx)
        .await?;
    release(tx, &entry, actor.operator_id()).await?;
    Ok(entry)
}

pub(super) async fn finish_entry_edits(
    tx: &mut Transaction<'_, Postgres>,
    instance: &str,
    released: bool,
) -> Result<()> {
    if released {
        // Decide which tunnels are idle only after additions and releases are staged.
        tunnels::withdraw_unused(tx).await?;
    }
    sync_entry_sources(tx, instance).await
}

async fn entry_view_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    id: &str,
) -> Result<CvmEntryView> {
    desired::views_by_id(tx, actor, id)
        .await?
        .ok_or_else(|| StoreError::NotFound("访问入口".into()))
}

/// Occupancy of one published address, without revealing other owners.
async fn address_use(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    offer: &EntryAddressOffer,
    visible: &BTreeSet<&str>,
) -> Result<CvmEntryAddressUse> {
    let reveal = |id: Option<String>| id.filter(|id| visible.contains(id.as_str()));
    let whole: Option<Option<String>> = sqlx::query_scalar("SELECT entry_id FROM cvm_nic_resources WHERE scope=$1 AND kind='ipv4' AND value=$2 AND NOT released LIMIT 1")
        .bind(format!("public:{node}")).bind(offer.address.to_string()).fetch_optional(&mut **tx).await?;
    let mut usage = CvmEntryAddressUse {
        address: offer.visible_address(),
        whole_taken: whole.is_some(),
        whole_entry_id: reveal(whole.flatten()),
        ports: vec![],
    };
    let EntryAddressKind::Shared {
        port_start,
        port_end,
    } = offer.kind
    else {
        return Ok(usage);
    };
    let host_ports = match sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT capabilities FROM cvm_nic_nodes WHERE node_id=$1",
    )
    .bind(node)
    .fetch_optional(&mut **tx)
    .await?
    {
        Some(raw) => serde_json::from_value::<NicNodeCapabilities>(raw)?.host_ports,
        None => vec![],
    };
    for protocol in [L4Protocol::Tcp, L4Protocol::Udp] {
        // Coalesce adjacent claims with the same visible owner, including a range entry.
        let mut segments: Vec<(u16, u16, Option<String>)> = Vec::new();
        for row in sqlx::query("SELECT entry_id,(split_part(value,':',array_length(string_to_array(value,':'),1)))::integer AS port FROM cvm_nic_resources WHERE scope=$1 AND kind=$2 AND NOT released AND (entry_id IS NULL OR value LIKE $3) LIMIT 65536")
            .bind(format!("port:{node}")).bind(kind(protocol)).bind(format!("{}:%", offer.address)).fetch_all(&mut **tx).await? {
            if let Ok(port) = u16::try_from(row.try_get::<i32, _>("port")?) {
                segments.push((port, port, reveal(row.try_get("entry_id")?)));
            }
        }
        for port in sqlx::query_scalar::<_,i32>("SELECT listen_port FROM forwarding_rules WHERE node_id=$1 AND deleted_at IS NULL AND enabled AND (protocol=$2 OR protocol='tcp_udp') AND listen_port BETWEEN $3 AND $4 LIMIT 65536")
            .bind(node).bind(kind(protocol)).bind(i32::from(port_start)).bind(i32::from(port_end)).fetch_all(&mut **tx).await? {
            if let Ok(port) = u16::try_from(port) {
                segments.push((port, port, None));
            }
        }
        for (start, end) in
            crate::port_reservations::static_ranges(tx, node, kind(protocol)).await?
        {
            if let (Ok(start), Ok(end)) = (u16::try_from(start), u16::try_from(end)) {
                segments.push((start, end, None));
            }
        }
        segments.extend(
            policy::protected(tx, node, Some(offer.address), protocol)
                .await?
                .into_iter()
                .map(|r| (r.start, r.end, None)),
        );
        for host in host_ports.iter().filter(|p| p.protocol == protocol) {
            segments.push((host.port, host.port, None));
        }
        segments.retain(|(start, end, _)| *start <= port_end && *end >= port_start);
        segments.sort();
        let mut merged: Vec<(u16, u16, Option<String>)> = Vec::new();
        for (start, end, entry) in segments {
            let (start, end) = (start.max(port_start), end.min(port_end));
            match merged.last_mut() {
                Some(last) if entry == last.2 && u32::from(start) <= u32::from(last.1) + 1 => {
                    last.1 = last.1.max(end)
                }
                _ => merged.push((start, end, entry)),
            }
        }
        usage
            .ports
            .extend(
                merged
                    .into_iter()
                    .map(|(port_start, port_end, entry_id)| CvmEntryPortUse {
                        protocol,
                        port_start,
                        port_end,
                        entry_id,
                    }),
            );
    }
    Ok(usage)
}

/// Approval freezes topology, while executing it still requires the current machine capability.
/// A downgraded executor must not receive a previously valid transport it can no longer apply.
pub(crate) async fn validate_serving(
    tx: &mut Transaction<'_, Postgres>,
    desired: &EntryDesired,
) -> Result<()> {
    let policy_required = desired
        .rules
        .iter()
        .any(|r| r.mode.requires_protocol_rules(&r.reserved_ports))
        || desired
            .deliveries
            .iter()
            .any(|r| r.mode.requires_protocol_rules(&r.reserved_ports));
    if policy_required && !capability(tx, &desired.node_id).await?.entry_protocol_rules {
        return Err(StoreError::Unsupported(
            "本机 Agent 尚不支持协议转发与保留端口".into(),
        ));
    }
    if desired
        .rules
        .iter()
        .any(|r| r.mode == EntryMode::L3 || r.mode.traffic().icmp || r.mode.traffic().other_ip)
    {
        let caps = capability(tx, &desired.node_id).await?;
        for rule in &desired.rules {
            if rule.mode == EntryMode::L3
                || rule.mode.traffic().icmp
                || rule.mode.traffic().other_ip
            {
                require_host_l3(tx, &desired.node_id, rule.address, &caps).await?;
            }
        }
    }
    for tunnel in &desired.tunnels {
        if (tunnel.offer.kind == TransportKind::Xray || !tunnel.wireguard_transport.is_udp())
            && !capability(tx, &desired.node_id)
                .await?
                .supports_entry_transport(tunnel.offer.kind, tunnel.wireguard_transport)
        {
            return Err(StoreError::Unsupported(
                "本机 Agent 尚不支持此入口传输，请升级 Agent 后重试".into(),
            ));
        }
    }
    Ok(())
}

/// Restore the entry ownership changed by one unpublished operation. All
/// participants are fenced by the caller before releasing or reviving claims.
pub(in crate::cvm) async fn undo_instance_release(
    tx: &mut Transaction<'_, Postgres>,
    instance: &str,
    before: &releases::NodeConfig,
    after: &releases::NodeConfig,
) -> Result<()> {
    let mut machines = BTreeSet::new();
    let mut instances = BTreeSet::from([instance.to_owned()]);
    let mut restored_pairs = BTreeSet::new();
    let ids = before
        .entry_instances
        .keys()
        .chain(after.entry_instances.keys())
        .collect::<BTreeSet<_>>();
    for id in ids {
        let original = before.entry_instances.get(id);
        let staged = after.entry_instances.get(id);
        if original == staged
            || ![original, staged]
                .into_iter()
                .any(|owner| owner.map(String::as_str) == Some(instance))
        {
            continue;
        }
        let entry = entry_locked(tx, id).await?;
        machines.extend([entry.provider_node.clone(), entry.host.clone()]);
        instances.insert(entry.instance_id.clone());
        if let Some(owner) = original {
            let host: String = sqlx::query_scalar("SELECT node_id FROM cvm_instances WHERE id=$1")
                .bind(owner)
                .fetch_one(&mut **tx)
                .await?;
            let updated = sqlx::query(
                "UPDATE cvm_entries e SET instance_id=$2,released=false,cleanup_generation=NULL,revision=e.revision+1,updated_at=now()
                 FROM cvm_entry_publications p WHERE e.id=$1 AND NOT e.cleaned AND p.id=e.publication_id AND NOT p.deleted
                   AND NOT EXISTS(SELECT 1 FROM cvm_nic_resources r WHERE r.entry_id=e.id AND r.released)",
            ).bind(id).bind(owner).execute(&mut **tx).await?.rows_affected();
            if updated != 1 {
                return Err(StoreError::Conflict(
                    "原入口资源已变化，无法安全恢复旧入口".into(),
                ));
            }
            restored_pairs.insert((entry.provider_node, host.clone()));
            machines.insert(host);
            instances.insert(owner.clone());
        } else {
            release(tx, &entry, "system-undo-unpublished").await?;
            sqlx::query("UPDATE cvm_entries SET cleaned=true WHERE id=$1")
                .bind(id)
                .execute(&mut **tx)
                .await?;
            sqlx::query("UPDATE cvm_nic_resources SET released=true WHERE entry_id=$1")
                .bind(id)
                .execute(&mut **tx)
                .await?;
        }
    }
    tunnels::withdraw_unused(tx).await?;
    for (provider, host) in restored_pairs
        .iter()
        .filter(|(provider, host)| provider != host)
    {
        let original = before
            .entry
            .tunnels
            .iter()
            .find(|t| &t.entry_node == provider && &t.target_node == host)
            .ok_or_else(|| invalid("原入口通道记录缺失"))?;
        let updated = sqlx::query("UPDATE cvm_entry_tunnels t SET withdrawn=false,entry_cleanup_generation=NULL,target_cleanup_generation=NULL
            WHERE t.id=$1 AND NOT t.cleaned
              AND NOT EXISTS(SELECT 1 FROM cvm_entry_tunnels other WHERE other.entry_node=t.entry_node AND other.target_node=t.target_node AND other.id<>t.id AND NOT other.withdrawn)
              AND NOT EXISTS(SELECT 1 FROM cvm_nic_resources r WHERE r.tunnel_id=t.id AND r.released)")
            .bind(&original.id).execute(&mut **tx).await?.rows_affected();
        if updated != 1 {
            return Err(StoreError::Conflict(
                "原入口通道已替换或清理，无法安全恢复".into(),
            ));
        }
    }
    let original_tunnels = before
        .entry
        .tunnels
        .iter()
        .map(|t| &t.id)
        .collect::<BTreeSet<_>>();
    for tunnel in after
        .entry
        .tunnels
        .iter()
        .filter(|t| !original_tunnels.contains(&t.id))
    {
        let updated = sqlx::query(
            "UPDATE cvm_entry_tunnels SET cleaned=true WHERE id=$1 AND withdrawn AND NOT cleaned",
        )
        .bind(&tunnel.id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        if updated > 0 {
            sqlx::query("UPDATE cvm_nic_resources SET released=true WHERE tunnel_id=$1")
                .bind(&tunnel.id)
                .execute(&mut **tx)
                .await?;
        }
    }
    if !machines.is_empty() {
        wake_entries(tx, &machines.iter().map(String::as_str).collect::<Vec<_>>()).await?;
        for id in instances {
            sync_entry_sources(tx, &id).await?;
        }
    }
    Ok(())
}
