//! Store is the allocation authority; remote execution never happens inside these transactions.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};

mod bindings;
pub(crate) mod entries;
mod metering;
mod publications;
mod versions;
pub use bindings::*;
pub use entries::{
    CvmEntryAddressUse, CvmEntryBoard, CvmEntryPortUse, CvmEntryPublicationView, CvmEntryState,
    CvmEntryView,
};
pub use metering::CvmNicUsage;
pub use publications::*;

use crate::resource_coordination::lock;

fn admin(actor: &AdminContext) -> Result<()> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "只有系统管理员可以发布网络能力".into(),
        ));
    }
    Ok(())
}
async fn wake(tx: &mut Transaction<'_, Postgres>, nodes: &[&str]) -> Result<()> {
    sqlx::query("UPDATE cvm_nic_nodes SET desired_generation=desired_generation+1 WHERE node_id=ANY($1::text[])") .bind(nodes).execute(&mut **tx).await?;
    Ok(())
}
async fn replay(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    request: &str,
    digest: &str,
) -> Result<Option<serde_json::Value>> {
    request_id(request)?;
    let row = sqlx::query(
        "SELECT digest,result FROM cvm_nic_requests WHERE actor_id=$1 AND request_id=$2",
    )
    .bind(actor.operator_id())
    .bind(request)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(row) = row {
        if row.try_get::<String, _>("digest")? != digest {
            return Err(StoreError::Conflict("请求标识已用于另一项网络操作".into()));
        }
        return Ok(Some(row.try_get("result")?));
    }
    Ok(None)
}
async fn remember(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    request: &str,
    digest: &str,
    publication: Option<&str>,
    result: &impl Serialize,
) -> Result<()> {
    sqlx::query("INSERT INTO cvm_nic_requests(actor_id,request_id,digest,publication_id,result) VALUES($1,$2,$3,$4,$5)").bind(actor.operator_id()).bind(request).bind(digest).bind(publication).bind(serde_json::to_value(result)?).execute(&mut **tx).await?;
    Ok(())
}
async fn publication(tx: &mut Transaction<'_, Postgres>, id: &str) -> Result<NicPublication> {
    let row = sqlx::query(
        "SELECT id,revision,admitting,deleted,spec FROM cvm_nic_publications WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::NotFound("网卡发布项".into()))?;
    Ok(NicPublication {
        id: row.try_get("id")?,
        revision: row.try_get::<i64, _>("revision")? as u64,
        admitting: row.try_get("admitting")?,
        retired: row.try_get("deleted")?,
        spec: serde_json::from_value(row.try_get("spec")?)?,
    })
}
async fn capability(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<NicNodeCapabilities> {
    let row=sqlx::query("SELECT s.capabilities,s.observed_at>now()-interval '150 seconds' AS fresh FROM cvm_nic_nodes s JOIN nodes n ON n.id=s.node_id WHERE s.node_id=$1 AND n.retired_at IS NULL").bind(node).fetch_optional(&mut **tx).await?.ok_or_else(||StoreError::Unsupported("机器尚未上报远端网卡能力".into()))?;
    let caps: NicNodeCapabilities = serde_json::from_value(row.try_get("capabilities")?)?;
    if !row.try_get::<bool, _>("fresh")?
        || caps.contract != NIC_PROTOCOL_VERSION
        || caps.error.is_some()
    {
        return Err(StoreError::Unsupported("机器网络能力已过期或不可用".into()));
    }
    Ok(caps)
}
fn granted(_actor: &AdminContext, tenant: &str, user: &str, p: &NicPublication) -> bool {
    p.spec
        .grants
        .iter()
        .any(|g| g.tenant_id == tenant && g.user_id == user)
}
fn devices(desired: &InstanceDesired) -> BTreeMap<String, AllocatedNic> {
    desired
        .spec
        .interfaces
        .iter()
        .filter_map(|n| n.allocated.clone().map(|a| (n.id.clone(), a)))
        .collect()
}
async fn instance_locked(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    id: &str,
    generation: u64,
) -> Result<CvmInstance> {
    let row = sqlx::query(&format!("{INSTANCE_SELECT} WHERE id=$1 FOR UPDATE"))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("CVM".into()))?;
    let instance = from_row(&row)?;
    require_owner(actor, &instance.tenant_id, &instance.user_id, true)?;
    if instance.desired.generation != generation {
        return Err(StoreError::Conflict("容器已变化，请刷新后操作".into()));
    }
    if instance.desired.power == PowerState::Deleted {
        return Err(invalid("容器已进入删除流程"));
    }
    if instance.status != ApplyStatus::Applied {
        return Err(StoreError::Conflict("请先完成或核查当前容器操作".into()));
    }
    Ok(instance)
}
async fn save_network(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    request: &str,
    digest: &str,
    kind: &str,
    mut desired: InstanceDesired,
) -> Result<()> {
    desired.generation = desired
        .generation
        .checked_add(1)
        .ok_or_else(|| invalid("实例版本已满"))?;
    desired.operation_id = new_id("op")?;
    desired.spec_sha256 = spec_digest(&desired.spec)?;
    desired.detached_interfaces=sqlx::query_scalar("SELECT id FROM cvm_interfaces WHERE instance_id=$1 AND NOT desired_attached AND NOT detached_confirmed ORDER BY id").bind(&desired.id).fetch_all(&mut **tx).await?;
    bindings::sync_routes(tx, &desired).await?;
    save_version(tx, actor, request, digest, kind, &desired).await
}
async fn claim(
    tx: &mut Transaction<'_, Postgres>,
    allocation: &str,
    binding: Option<&str>,
    scope: &str,
    kind: &str,
    value: &str,
) -> Result<()> {
    sqlx::query("INSERT INTO cvm_nic_resources(allocation_id,binding_id,scope,kind,value) VALUES($1,$2,$3,$4,$5)").bind(allocation).bind(binding).bind(scope).bind(kind).bind(value).execute(&mut **tx).await?;
    Ok(())
}
async fn allocated_values(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    kind: &str,
) -> Result<BTreeSet<String>> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT value FROM cvm_nic_resources WHERE scope=$1 AND kind=$2 AND NOT released",
    )
    .bind(scope)
    .bind(kind)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect())
}
async fn allocate_address(
    tx: &mut Transaction<'_, Postgres>,
    pool: Option<&IpPrefix>,
    scope: &str,
    kind: &str,
    avoid: &BTreeSet<IpAddr>,
) -> Result<Option<IpAddr>> {
    let Some(pool) = pool else { return Ok(None) };
    let used = allocated_values(tx, scope, kind).await?;
    let hosts = if pool.length() == if pool.is_ipv4() { 32 } else { 128 } {
        0..1
    } else {
        1..65536
    };
    for offset in hosts {
        let Some(address) = pool.host(offset) else {
            break;
        };
        if address.is_unspecified()
            || address.is_multicast()
            || address.is_loopback()
            || avoid.contains(&address)
            || used.contains(&address.to_string())
        {
            continue;
        }
        if pool.is_ipv4() && pool.length() < 31 && pool.host(offset + 1).is_none() {
            continue;
        }
        return Ok(Some(address));
    }
    Err(StoreError::Conflict("可分配地址已耗尽".into()))
}
fn mac(id: &str) -> String {
    let hex = sha256_hex(id.as_bytes());
    format!(
        "02:{}:{}:{}:{}:{}",
        &hex[..2],
        &hex[2..4],
        &hex[4..6],
        &hex[6..8],
        &hex[8..10]
    )
}

/// Addresses already serving the provider cannot become an exclusive Guest identity.
async fn public_exclusions(
    tx: &mut Transaction<'_, Postgres>,
    provider: &str,
) -> Result<BTreeSet<IpAddr>> {
    let mut avoid: BTreeSet<IpAddr> = capability(tx, provider)
        .await?
        .addresses
        .into_iter()
        .collect();
    let values:Vec<String>=sqlx::query_scalar("SELECT public_ipv4 FROM nodes WHERE id=$1 AND public_ipv4 IS NOT NULL UNION SELECT public_ipv6 FROM nodes WHERE id=$1 AND public_ipv6 IS NOT NULL UNION SELECT spec->>'shared_ipv4' FROM cvm_nic_publications WHERE provider_node=$1 AND NOT deleted AND spec->>'shared_ipv4' IS NOT NULL UNION SELECT spec->>'shared_ipv6' FROM cvm_nic_publications WHERE provider_node=$1 AND NOT deleted AND spec->>'shared_ipv6' IS NOT NULL UNION SELECT spec->>'public_ipv4' FROM cvm_nic_allocations WHERE provider_node=$1 AND NOT released AND spec->>'address_mode'='shared_nat' AND spec->>'public_ipv4' IS NOT NULL UNION SELECT spec->>'public_ipv6' FROM cvm_nic_allocations WHERE provider_node=$1 AND NOT released AND spec->>'address_mode'='shared_nat' AND spec->>'public_ipv6' IS NOT NULL").bind(provider).fetch_all(&mut **tx).await?;
    for value in values {
        avoid.insert(
            value
                .parse()
                .map_err(|_| invalid("invalid provider address resource"))?,
        );
    }
    Ok(avoid)
}
impl PgStore {
    pub async fn mount_cvm_nic(
        &self,
        actor: &AdminContext,
        id: &str,
        request: NicMountRequest,
    ) -> Result<CvmInstance> {
        request_id(&request.request_id)?;
        let digest = request_digest(&(id, &request))?;
        let mut tx = self.pool.begin().await?;
        lock(&mut tx).await?;
        if let Some(instance) =
            operation_replay(&mut tx, actor, &request.request_id, &digest).await?
        {
            tx.commit().await?;
            return Ok(instance);
        }
        let instance = instance_locked(&mut tx, actor, id, request.expected_generation).await?;
        let p = publication(&mut tx, &request.publication_id).await?;
        if p.retired || !p.admitting {
            return Err(StoreError::Conflict("该网卡发布项已停止新增挂载".into()));
        }
        if !granted(actor, &instance.tenant_id, &instance.user_id, &p) {
            return Err(StoreError::Forbidden("尚未获得该网卡的挂载授权".into()));
        }
        let quota = quota_locked(&mut tx, &instance.tenant_id, &instance.user_id).await?;
        admission(&mut tx, &quota, &instance.node_id, None, 1).await?;
        let host = host_locked(&mut tx, &instance.node_id, false).await?;
        if instance.desired.spec.interfaces.len() >= usize::from(host.max_interfaces)
            || instance.desired.spec.interfaces.len() >= MAX_INTERFACES
        {
            return Err(invalid("已达到宿主实际网卡容量"));
        }
        if instance.desired.power == PowerState::Running && !host.hot_attach {
            return Err(StoreError::Unsupported("请先关机，再挂载网卡".into()));
        }
        let consumer = capability(&mut tx, &instance.node_id).await?;
        let provider = capability(&mut tx, &p.spec.provider_node).await?;
        if !consumer.consume
            || !provider.provide
            || !consumer.transports.contains(&request.transport)
            || !provider.transports.contains(&request.transport)
        {
            return Err(StoreError::Unsupported("双方没有共同支持的所选承载".into()));
        }
        let offer = p
            .spec
            .transports
            .iter()
            .find(|t| t.kind == request.transport)
            .ok_or_else(|| invalid("发布项不支持所选承载"))?
            .clone();
        let count:i64=sqlx::query_scalar("SELECT count(DISTINCT interface_id) FROM cvm_nic_allocations WHERE publication_id=$1 AND NOT released").bind(&p.id).fetch_one(&mut *tx).await?;
        if count >= i64::from(p.spec.limits.max_attachments) {
            return Err(invalid("发布项可用挂载容量已满"));
        }
        let provider_count:i64=sqlx::query_scalar("SELECT count(DISTINCT interface_id) FROM cvm_nic_allocations WHERE provider_node=$1 AND NOT released").bind(&p.spec.provider_node).fetch_one(&mut *tx).await?;
        let consumer_count:i64=sqlx::query_scalar("SELECT count(DISTINCT interface_id) FROM cvm_nic_allocations WHERE consumer_node=$1 AND NOT released").bind(&instance.node_id).fetch_one(&mut *tx).await?;
        if provider_count >= i64::from(provider.max_bindings)
            || consumer_count >= i64::from(consumer.max_bindings)
        {
            return Err(invalid("执行机器承载容量不足"));
        }
        let mut desired = instance.desired.clone();
        let mut avoid: BTreeSet<IpAddr> = devices(&desired)
            .values()
            .flat_map(|n| [n.ipv4, n.ipv6])
            .flatten()
            .collect();
        if let Some(report) = &instance.report {
            for actual in &report.interfaces {
                for raw in &actual.addresses {
                    if let Ok(address) = raw.split('/').next().unwrap_or_default().parse::<IpAddr>()
                    {
                        avoid.insert(address);
                    }
                }
            }
        }
        let public_avoid = public_exclusions(&mut tx, &p.spec.provider_node).await?;
        if p.spec.address_mode == NicAddressMode::DedicatedRouted {
            avoid.extend(public_avoid.iter().copied());
        }
        let private_scope = format!("private:{}", p.spec.resource_domain);
        let public_scope = format!("public:{}", p.spec.provider_node);
        let used_slots = allocated_values(&mut tx, "global", "slot").await?;
        let slot = (0..8192u16)
            .find(|s| !used_slots.contains(&s.to_string()))
            .ok_or_else(|| invalid("网卡隔离上下文已达上限"))?;
        let ipv4 = allocate_address(
            &mut tx,
            p.spec.ipv4_pool.as_ref(),
            &private_scope,
            "ipv4",
            &avoid,
        )
        .await?;
        let ipv6 = allocate_address(
            &mut tx,
            p.spec.ipv6_pool.as_ref(),
            &private_scope,
            "ipv6",
            &avoid,
        )
        .await?;
        let public_ipv4 = match p.spec.address_mode {
            NicAddressMode::SharedNat => p.spec.shared_ipv4,
            NicAddressMode::DedicatedRouted => ipv4,
            NicAddressMode::DedicatedNat => {
                allocate_address(
                    &mut tx,
                    p.spec.public_ipv4_pool.as_ref(),
                    &public_scope,
                    "ipv4",
                    &public_avoid,
                )
                .await?
            }
        };
        let public_ipv6 = match p.spec.address_mode {
            NicAddressMode::SharedNat => p.spec.shared_ipv6,
            NicAddressMode::DedicatedRouted => ipv6,
            NicAddressMode::DedicatedNat => {
                allocate_address(
                    &mut tx,
                    p.spec.public_ipv6_pool.as_ref(),
                    &public_scope,
                    "ipv6",
                    &public_avoid,
                )
                .await?
            }
        };
        let nic_id = new_id("nic")?;
        let allocation_id = new_id("allocation")?;
        let allocated = AllocatedNic {
            slot,
            publication_id: p.id.clone(),
            publication_revision: p.revision,
            provider_node: p.spec.provider_node.clone(),
            resource_domain: p.spec.resource_domain.clone(),
            allocation_id: allocation_id.clone(),
            mac: mac(&nic_id),
            ipv4,
            ipv6,
            public_ipv4,
            public_ipv6,
            address_mode: p.spec.address_mode,
            destinations: p.spec.destinations.clone(),
            transport: request.transport,
            transport_generation: 1,
            mtu: offer.mtu,
            blocked: false,
            quota_exceeded: false,
            limits: p.spec.limits.clone(),
        };
        let nic = InterfaceSpec {
            id: nic_id.clone(),
            allocated: Some(allocated.clone()),
            connection: InterfaceRequest {
                source_id: p.id.clone(),
                purpose: InterfacePurpose::Business,
                default_ipv4: false,
                default_ipv6: false,
            },
        };
        insert_interface(&mut tx, id, &nic).await?;
        sqlx::query("INSERT INTO cvm_nic_allocations(id,interface_id,instance_id,publication_id,provider_node,consumer_node,spec) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(&allocation_id).bind(&nic_id).bind(id).bind(&p.id).bind(&p.spec.provider_node).bind(&instance.node_id).bind(serde_json::to_value(&allocated)?).execute(&mut *tx).await?;
        claim(
            &mut tx,
            &allocation_id,
            None,
            "global",
            "slot",
            &slot.to_string(),
        )
        .await?;
        for (address, kind) in [(ipv4, "ipv4"), (ipv6, "ipv6")] {
            if let Some(address) = address {
                claim(
                    &mut tx,
                    &allocation_id,
                    None,
                    &private_scope,
                    kind,
                    &address.to_string(),
                )
                .await?;
            }
        }
        if p.spec.address_mode != NicAddressMode::SharedNat {
            for (address, kind) in [(public_ipv4, "ipv4"), (public_ipv6, "ipv6")] {
                if let Some(address) = address {
                    claim(
                        &mut tx,
                        &allocation_id,
                        None,
                        &public_scope,
                        kind,
                        &address.to_string(),
                    )
                    .await?;
                }
            }
        }
        create_binding(
            &mut tx,
            id,
            &nic_id,
            &instance.node_id,
            &allocated,
            &offer,
            &provider,
        )
        .await?;
        desired.spec.interfaces.push(nic);
        let priority = desired
            .spec
            .routing
            .priorities
            .iter()
            .flat_map(|p| [p.ipv4, p.ipv6])
            .flatten()
            .max()
            .unwrap_or(0)
            .checked_add(10)
            .ok_or_else(|| invalid("请先调整网卡优先级"))?;
        desired.spec.routing.priorities.push(InterfacePriority {
            interface_id: nic_id.clone(),
            ipv4: ipv4.map(|_| priority),
            ipv6: ipv6.map(|_| priority),
        });
        for destination in &p.spec.route_presets {
            desired.spec.routing.routes.push(ContainerRoute {
                destination: destination.clone(),
                interface_id: nic_id.clone(),
                service: None,
            });
        }
        if request.use_dns_suggestions {
            desired.spec.routing.dns = p.spec.dns_suggestions.clone();
        }
        desired.spec.routing.revision += 1;
        desired.spec.routing.normalize();
        desired
            .spec
            .routing
            .validate(&devices(&desired))
            .map_err(invalid)?;
        persist_routes(&mut tx, actor, &desired).await?;
        save_network(
            &mut tx,
            actor,
            &request.request_id,
            &digest,
            "mount_nic",
            desired,
        )
        .await?;
        tx.commit().await?;
        self.cvm_instance(actor, id).await
    }
    pub async fn set_cvm_routes(
        &self,
        actor: &AdminContext,
        id: &str,
        request: RoutesWrite,
    ) -> Result<CvmInstance> {
        let digest = request_digest(&(id, &request))?;
        let mut tx = self.pool.begin().await?;
        lock(&mut tx).await?;
        if let Some(instance) =
            operation_replay(&mut tx, actor, &request.request_id, &digest).await?
        {
            tx.commit().await?;
            return Ok(instance);
        }
        let mut desired = instance_locked(&mut tx, actor, id, request.expected_generation)
            .await?
            .desired;
        if desired.spec.routing.revision != request.expected_revision {
            return Err(StoreError::Conflict("路由表已变化，请刷新后操作".into()));
        }
        let mut routes = request.routes;
        routes.revision = request
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| invalid("路由版本已满"))?;
        routes.normalize();
        routes.validate(&devices(&desired)).map_err(invalid)?;
        // Port rules need both the host Agent (host firewall) and Cubelet (Guest policy
        // routing); an older executor would apply them as whole-prefix routes.
        if routes.routes.iter().any(|route| route.service.is_some()) {
            let instance =
                sqlx::query_scalar::<_, String>("SELECT node_id FROM cvm_instances WHERE id=$1")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
            if !host_locked(&mut tx, &instance, false).await?.route_services
                || !capability(&mut tx, &instance).await?.route_service
            {
                return Err(StoreError::Unsupported(
                    "容器宿主的运行栈或 Agent 尚不支持按协议端口选择出口".into(),
                ));
            }
        }
        desired.spec.routing = routes;
        persist_routes(&mut tx, actor, &desired).await?;
        save_network(
            &mut tx,
            actor,
            &request.request_id,
            &digest,
            "routes",
            desired,
        )
        .await?;
        tx.commit().await?;
        self.cvm_instance(actor, id).await
    }
}
async fn persist_routes(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    desired: &InstanceDesired,
) -> Result<()> {
    sqlx::query("INSERT INTO cvm_nic_route_versions(instance_id,revision,routes,actor_id) VALUES($1,$2,$3,$4)").bind(&desired.id).bind(desired.spec.routing.revision as i64).bind(serde_json::to_value(&desired.spec.routing)?).bind(actor.operator_id()).execute(&mut **tx).await?;
    Ok(())
}
