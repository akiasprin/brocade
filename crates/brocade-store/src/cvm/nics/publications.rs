use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNetworkProvider {
    pub node_id: String,
    pub name: String,
    pub fresh: bool,
    pub capabilities: NicNodeCapabilities,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNicAttachment {
    pub allocation_id: String,
    pub interface_id: String,
    pub instance_id: String,
    pub instance_name: String,
    pub publication_id: String,
    pub publication_revision: u64,
    pub generation: u64,
    pub attached: bool,
    pub route_references: usize,
    pub cleanup_pending: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNicCatalog {
    pub publications: Vec<NicPublication>,
    pub attachments: Vec<CvmNicAttachment>,
    pub providers: Vec<CvmNetworkProvider>,
    pub can_manage: bool,
}
impl PgStore {
    pub async fn cvm_nic_catalog(&self, actor: &AdminContext) -> Result<CvmNicCatalog> {
        let mut publications = Vec::new();
        for row in sqlx::query("SELECT id,revision,admitting,deleted,spec FROM cvm_nic_publications WHERE NOT deleted ORDER BY id LIMIT 256").fetch_all(&self.pool).await? {
            let mut p=NicPublication{id:row.try_get("id")?,revision:row.try_get::<i64,_>("revision")? as u64,admitting:row.try_get("admitting")?,retired:row.try_get("deleted")?,spec:serde_json::from_value(row.try_get("spec")?)?};
            if !actor.is_system_admin() {
                p.spec.grants.retain(|owner|view_allowed(actor,&owner.tenant_id,&owner.user_id));
                if p.spec.grants.is_empty(){continue;}
            }
            if actor.role()==AdminRole::Readonly{p.spec.ipv4_pool=None;p.spec.ipv6_pool=None;p.spec.public_ipv4_pool=None;p.spec.public_ipv6_pool=None;p.spec.shared_ipv4=None;p.spec.shared_ipv6=None;for offer in &mut p.spec.transports{offer.endpoint=if offer.endpoint.is_ipv4(){std::net::Ipv4Addr::UNSPECIFIED.into()}else{std::net::Ipv6Addr::UNSPECIFIED.into()};}}
            publications.push(p);
        }
        let providers = if actor.is_system_admin() {
            let mut providers = Vec::new();
            for row in sqlx::query("SELECT n.id,n.name,s.capabilities,s.observed_at>now()-interval '150 seconds' AS fresh FROM cvm_nic_nodes s JOIN nodes n ON n.id=s.node_id WHERE n.retired_at IS NULL ORDER BY n.name,n.id LIMIT 1024").fetch_all(&self.pool).await? {
                providers.push(CvmNetworkProvider{node_id:row.try_get("id")?,name:row.try_get("name")?,fresh:row.try_get("fresh")?,capabilities:serde_json::from_value(row.try_get("capabilities")?)?});
            }
            providers
        } else {
            Vec::new()
        };
        let mut attachments = Vec::new();
        if actor.is_system_admin() {
            let rows=sqlx::query("SELECT a.id,a.interface_id,a.instance_id,a.publication_id,a.spec,n.desired_attached,i.name,i.generation,i.desired FROM cvm_nic_allocations a JOIN cvm_interfaces n ON n.id=a.interface_id JOIN cvm_instances i ON i.id=a.instance_id WHERE NOT a.released ORDER BY a.publication_id,a.instance_id,a.interface_id,a.created_at LIMIT 16385").fetch_all(&self.pool).await?;
            if rows.len() > 16384 {
                return Err(invalid("network allocation view exceeds capacity"));
            }
            for row in rows {
                let allocation: String = row.try_get("id")?;
                let id: String = row.try_get("interface_id")?;
                let nic: AllocatedNic = serde_json::from_value(row.try_get("spec")?)?;
                let desired: InstanceDesired = serde_json::from_value(row.try_get("desired")?)?;
                let attached = row.try_get::<bool, _>("desired_attached")?
                    && desired.spec.interfaces.iter().any(|n| {
                        n.id == id
                            && n.allocated
                                .as_ref()
                                .is_some_and(|n| n.allocation_id == allocation)
                    });
                let refs = desired
                    .spec
                    .routing
                    .routes
                    .iter()
                    .filter(|route| route.interface_id == id)
                    .count()
                    + usize::from(
                        desired
                            .spec
                            .routing
                            .priorities
                            .iter()
                            .any(|p| p.interface_id == id),
                    );
                attachments.push(CvmNicAttachment {
                    allocation_id: allocation,
                    interface_id: id,
                    instance_id: row.try_get("instance_id")?,
                    instance_name: row.try_get("name")?,
                    publication_id: row.try_get("publication_id")?,
                    publication_revision: nic.publication_revision,
                    generation: row.try_get::<i64, _>("generation")? as u64,
                    attached,
                    route_references: refs,
                    cleanup_pending: !attached,
                });
            }
        }
        Ok(CvmNicCatalog {
            attachments,
            publications,
            providers,
            can_manage: actor.is_system_admin(),
        })
    }
    pub async fn write_cvm_nic_publication(
        &self,
        actor: &AdminContext,
        id: Option<&str>,
        request: PublicationWrite,
    ) -> Result<NicPublication> {
        admin(actor)?;
        request.spec.validate().map_err(invalid)?;
        let digest = request_digest(&(id, &request))?;
        let mut tx = self.pool.begin().await?;
        lock(&mut tx).await?;
        if let Some(value) = replay(&mut tx, actor, &request.request_id, &digest).await? {
            return Ok(serde_json::from_value(value)?);
        }
        let authorization_only = if let Some(id) = id {
            let old = publication(&mut tx, id).await?;
            let mut before = old.spec;
            let mut after = request.spec.clone();
            before.grants.clear();
            after.grants.clear();
            before == after
        } else {
            false
        };
        if !authorization_only {
            let provider = capability(&mut tx, &request.spec.provider_node).await?;
            if !provider.provide
                || request
                    .spec
                    .transports
                    .iter()
                    .any(|t| !provider.transports.contains(&t.kind))
            {
                return Err(StoreError::Unsupported(
                    "提供机器不支持网络的承载类型".into(),
                ));
            }
            if let Some(shared) = request.spec.shared_ipv4 {
                let whole:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_entry_publications p CROSS JOIN LATERAL jsonb_array_elements(p.spec->'addresses') a WHERE p.provider_node=$1 AND NOT p.deleted AND a->>'kind'='dedicated' AND a->>'address'=$2)")
                    .bind(&request.spec.provider_node).bind(shared.to_string()).fetch_one(&mut *tx).await?;
                if whole {
                    return Err(StoreError::Conflict(
                        "共享出网地址已作为整 IP 入口发布".into(),
                    ));
                }
            }
        }
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM cvm_nic_publications WHERE NOT deleted")
                .fetch_one(&mut *tx)
                .await?;
        let (id, revision, admitting) = if let Some(id) = id {
            let old = publication(&mut tx, id).await?;
            if old.retired || old.revision != request.expected_revision {
                return Err(StoreError::Conflict("网络项已变化，请刷新后操作".into()));
            }
            if old.spec.provider_node != request.spec.provider_node
                || old.spec.resource_domain != request.spec.resource_domain
            {
                return Err(invalid("提供机器和资源域属于网络身份，变更请创建新网络"));
            }
            (
                old.id,
                old.revision
                    .checked_add(1)
                    .ok_or_else(|| invalid("网络版本已满"))?,
                old.admitting,
            )
        } else {
            if request.expected_revision != 0 || count >= MAX_NETWORK_PUBLICATIONS as i64 {
                return Err(invalid("网络版本或数量超出限制"));
            }
            (new_id("network")?, 1, true)
        };
        for owner in &request.spec.grants {
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE tenant_id=$1 AND id=$2 AND status='active')").bind(&owner.tenant_id).bind(&owner.user_id).fetch_one(&mut *tx).await?;
            if !exists {
                return Err(invalid("授权用户不存在或已停用"));
            }
        }
        let p = NicPublication {
            id: id.clone(),
            revision,
            admitting,
            retired: false,
            spec: request.spec,
        };
        sqlx::query("INSERT INTO cvm_nic_publications(id,provider_node,revision,admitting,spec,actor_id) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,spec=excluded.spec,actor_id=excluded.actor_id,updated_at=now()") .bind(&id).bind(&p.spec.provider_node).bind(revision as i64).bind(admitting).bind(serde_json::to_value(&p.spec)?).bind(actor.operator_id()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO cvm_nic_publication_versions(publication_id,revision,spec,actor_id) VALUES($1,$2,$3,$4)").bind(&id).bind(revision as i64).bind(serde_json::to_value(&p.spec)?).bind(actor.operator_id()).execute(&mut *tx).await?;
        // Authorization is live even when a device deliberately stays on an older service revision.
        apply_grants(&mut tx, &p).await?;
        remember(&mut tx, actor, &request.request_id, &digest, Some(&id), &p).await?;
        tx.commit().await?;
        Ok(p)
    }
    pub async fn set_cvm_nic_admission(
        &self,
        actor: &AdminContext,
        id: &str,
        request: PublicationAdmission,
    ) -> Result<NicPublication> {
        admin(actor)?;
        let digest = request_digest(&(id, &request))?;
        let mut tx = self.pool.begin().await?;
        lock(&mut tx).await?;
        if let Some(value) = replay(&mut tx, actor, &request.request_id, &digest).await? {
            return Ok(serde_json::from_value(value)?);
        }
        let mut p = publication(&mut tx, id).await?;
        if p.retired || p.revision != request.expected_revision {
            return Err(StoreError::Conflict("网络项已变化".into()));
        }
        p.admitting = request.admitting;
        sqlx::query(
            "UPDATE cvm_nic_publications SET admitting=$2,actor_id=$3,updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .bind(p.admitting)
        .bind(actor.operator_id())
        .execute(&mut *tx)
        .await?;
        remember(&mut tx, actor, &request.request_id, &digest, Some(id), &p).await?;
        tx.commit().await?;
        Ok(p)
    }
    pub async fn delete_cvm_nic_publication(
        &self,
        actor: &AdminContext,
        id: &str,
        request: PublicationAdmission,
    ) -> Result<NicPublication> {
        admin(actor)?;
        let digest = request_digest(&("delete", id, &request))?;
        let mut tx = self.pool.begin().await?;
        lock(&mut tx).await?;
        if let Some(value) = replay(&mut tx, actor, &request.request_id, &digest).await? {
            return Ok(serde_json::from_value(value)?);
        }
        let mut p = publication(&mut tx, id).await?;
        if p.revision != request.expected_revision {
            return Err(StoreError::Conflict("网络项已变化".into()));
        }
        let allocated:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_nic_allocations WHERE publication_id=$1 AND NOT released)").bind(id).fetch_one(&mut *tx).await?;
        if allocated {
            return Err(StoreError::Conflict(
                "请先处理派生网卡及其路由依赖，并等待资源清理完成".into(),
            ));
        }
        p.retired = true;
        p.admitting = false;
        sqlx::query("UPDATE cvm_nic_publications SET deleted=true,admitting=false,actor_id=$2,updated_at=now() WHERE id=$1").bind(id).bind(actor.operator_id()).execute(&mut *tx).await?;
        remember(&mut tx, actor, &request.request_id, &digest, Some(id), &p).await?;
        tx.commit().await?;
        Ok(p)
    }
    pub async fn cvm_record_nic_capabilities(
        &self,
        node: &str,
        caps: NicNodeCapabilities,
    ) -> Result<()> {
        if caps.contract != NIC_PROTOCOL_VERSION
            || caps.transports.len() > 2
            || caps.addresses.len() > 256
            || caps.host_ports.len() > 2048
            || caps.max_bindings as usize > MAX_NETWORK_BINDINGS
            || caps.error.as_ref().is_some_and(|e| e.len() > 500)
            || caps
                .xray_certificate_pem
                .as_ref()
                .is_some_and(|pem| pem.len() > 16384)
        {
            return Err(invalid("无效的远端网卡能力回报"));
        }
        sqlx::query("INSERT INTO cvm_nic_nodes(node_id,capabilities) VALUES($1,$2) ON CONFLICT(node_id) DO UPDATE SET capabilities=excluded.capabilities,observed_at=now()") .bind(node).bind(serde_json::to_value(caps)?).execute(&self.pool).await?;
        Ok(())
    }
}

async fn apply_grants(tx: &mut Transaction<'_, Postgres>, p: &NicPublication) -> Result<()> {
    let rows=sqlx::query(&format!("{INSTANCE_SELECT} WHERE id IN (SELECT instance_id FROM cvm_nic_allocations WHERE publication_id=$1 AND NOT released) ORDER BY id FOR UPDATE")).bind(&p.id).fetch_all(&mut **tx).await?;
    for row in rows {
        let instance = from_row(&row)?;
        let allowed = p
            .spec
            .grants
            .iter()
            .any(|g| g.tenant_id == instance.tenant_id && g.user_id == instance.user_id);
        let mut desired = instance.desired;
        let mut changed = false;
        for nic in &mut desired.spec.interfaces {
            if let Some(a) = nic.allocated.as_mut().filter(|a| a.publication_id == p.id) {
                if a.blocked == allowed {
                    a.blocked = !allowed;
                    changed = true;
                    set_binding_blocked(tx, &nic.id, !allowed).await?;
                }
            }
        }
        if changed {
            system_network_version(tx, &mut desired, "network_authorization").await?;
        }
    }
    Ok(())
}

pub(crate) async fn system_network_version(
    tx: &mut Transaction<'_, Postgres>,
    desired: &mut InstanceDesired,
    kind: &str,
) -> Result<()> {
    bindings::sync_routes(tx, desired).await?;
    desired.generation += 1;
    desired.operation_id = new_id("op")?;
    desired.spec_sha256 = spec_digest(&desired.spec)?;
    let request = new_id("network-system")?;
    sqlx::query(
        "INSERT INTO cvm_spec_versions(instance_id,generation,spec,sha256) VALUES($1,$2,$3,$4)",
    )
    .bind(&desired.id)
    .bind(desired.generation as i64)
    .bind(serde_json::to_value(&desired.spec)?)
    .bind(&desired.spec_sha256)
    .execute(&mut **tx)
    .await?;
    sqlx::query("INSERT INTO cvm_operations(id,instance_id,generation,kind,actor_id,request_id,request_sha256) VALUES($1,$2,$3,$4,'system-network',$5,$6)").bind(&desired.operation_id).bind(&desired.id).bind(desired.generation as i64).bind(kind).bind(&request).bind(request_digest(desired)?).execute(&mut **tx).await?;
    sqlx::query("UPDATE cvm_instances SET generation=$2,desired=$3,status='pending',updated_at=now() WHERE id=$1").bind(&desired.id).bind(desired.generation as i64).bind(serde_json::to_value(&*desired)?).execute(&mut **tx).await?;
    Ok(())
}
