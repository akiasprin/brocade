use super::*;

fn address_fits(pool: Option<&IpPrefix>, address: Option<IpAddr>) -> bool {
    match (pool, address) {
        (None, None) => true,
        (Some(pool), Some(address)) => pool.contains(address),
        _ => false,
    }
}

impl PgStore {
    pub async fn update_cvm_nic_version(
        &self,
        actor: &AdminContext,
        instance_id: &str,
        nic_id: &str,
        request: NicVersionChange,
    ) -> Result<CvmInstance> {
        self.update_cvm_nic_versions(
            actor,
            instance_id,
            NicVersionsChange {
                request_id: request.request_id,
                expected_generation: request.expected_generation,
                updates: vec![NicVersionUpdate {
                    interface_id: nic_id.into(),
                    publication_revision: request.publication_revision,
                    reallocate: request.reallocate,
                    transport: request.transport,
                }],
            },
        )
        .await
    }
    pub async fn update_cvm_nic_versions(
        &self,
        actor: &AdminContext,
        instance_id: &str,
        request: NicVersionsChange,
    ) -> Result<CvmInstance> {
        if request.updates.is_empty()
            || request.updates.len() > MAX_INTERFACES
            || request
                .updates
                .iter()
                .map(|update| &update.interface_id)
                .collect::<BTreeSet<_>>()
                .len()
                != request.updates.len()
        {
            return Err(invalid("请选择本容器内互不重复的网卡"));
        }
        let digest = request_digest(&(instance_id, &request))?;
        let mut tx = self.pool.begin().await?;
        lock(&mut tx).await?;
        if let Some(instance) =
            operation_replay(&mut tx, actor, &request.request_id, &digest).await?
        {
            tx.commit().await?;
            return Ok(instance);
        }
        let instance =
            instance_locked(&mut tx, actor, instance_id, request.expected_generation).await?;
        let mut desired = instance.desired.clone();
        for update in &request.updates {
            apply_version(
                &mut tx,
                actor,
                instance_id,
                &instance,
                &mut desired,
                &update.interface_id,
                update,
            )
            .await?;
        }
        save_network(
            &mut tx,
            actor,
            &request.request_id,
            &digest,
            "update_nic_specification",
            desired,
        )
        .await?;
        tx.commit().await?;
        self.cvm_instance(actor, instance_id).await
    }
}
async fn apply_version(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    instance_id: &str,
    instance: &CvmInstance,
    desired: &mut InstanceDesired,
    nic_id: &str,
    request: &NicVersionUpdate,
) -> Result<()> {
    let old = desired
        .spec
        .interfaces
        .iter()
        .find(|n| n.id == nic_id)
        .and_then(|n| n.allocated.as_ref())
        .ok_or_else(|| StoreError::NotFound("受管网卡".into()))?
        .clone();
    let publication = publication(tx, &old.publication_id).await?;
    if publication.retired || !granted(actor, &instance.tenant_id, &instance.user_id, &publication)
    {
        return Err(StoreError::Forbidden("网卡挂载授权已撤销".into()));
    }
    let raw: serde_json::Value = sqlx::query_scalar(
        "SELECT spec FROM cvm_nic_publication_versions WHERE publication_id=$1 AND revision=$2",
    )
    .bind(&old.publication_id)
    .bind(i64::try_from(request.publication_revision).map_err(|_| invalid("网络版本无效"))?)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::NotFound("网络规格版本".into()))?;
    let spec: NicPublicationSpec = serde_json::from_value(raw)?;
    if request.publication_revision == old.publication_revision {
        return Err(invalid("网卡已使用此规格版本"));
    }
    let transitioning:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_nic_bindings WHERE interface_id=$1 AND NOT cleaned AND phase<>'active')").bind(nic_id).fetch_one(&mut **tx).await?;
    if transitioning {
        return Err(StoreError::Conflict(
            "请先完成当前网卡承载操作与清理".into(),
        ));
    }
    let consumer = capability(tx, &instance.node_id).await?;
    let provider = capability(tx, &old.provider_node).await?;
    let transport = request.transport.unwrap_or(old.transport);
    let offer = spec
        .transports
        .iter()
        .find(|t| t.kind == transport)
        .ok_or_else(|| invalid("新规格不支持当前承载，请明确选择新承载"))?;
    if !consumer.transports.contains(&transport) || !provider.transports.contains(&transport) {
        return Err(StoreError::Unsupported("双方没有共同支持的所选承载".into()));
    }
    let needs_addresses = !address_fits(spec.ipv4_pool.as_ref(), old.ipv4)
        || !address_fits(spec.ipv6_pool.as_ref(), old.ipv6)
        || old.address_mode != spec.address_mode
        || match spec.address_mode {
            NicAddressMode::SharedNat => {
                old.public_ipv4 != spec.shared_ipv4 || old.public_ipv6 != spec.shared_ipv6
            }
            NicAddressMode::DedicatedRouted => {
                old.public_ipv4 != old.ipv4 || old.public_ipv6 != old.ipv6
            }
            NicAddressMode::DedicatedNat => {
                !address_fits(spec.public_ipv4_pool.as_ref(), old.public_ipv4)
                    || !address_fits(spec.public_ipv6_pool.as_ref(), old.public_ipv6)
            }
        };
    if needs_addresses && !request.reallocate {
        return Err(StoreError::Conflict(
            "新规格会改变地址，请先关机并明确允许重新分配".into(),
        ));
    }
    if needs_addresses
        && (desired.power != PowerState::Stopped
            || instance.report.as_ref().and_then(|r| r.power) != Some(PowerState::Stopped))
    {
        return Err(StoreError::Conflict(
            "重新分配地址前必须确认容器已关机".into(),
        ));
    }
    let mut next = old.clone();
    next.publication_revision = request.publication_revision;
    next.destinations = spec.destinations.clone();
    next.address_mode = spec.address_mode;
    next.limits = spec.limits.clone();
    next.mtu = offer.mtu;
    next.transport = transport;
    next.transport_generation += 1;
    if needs_addresses {
        next.allocation_id = new_id("allocation")?;
        let private_scope = format!("private:{}", old.resource_domain);
        let public_scope = format!("public:{}", old.provider_node);
        let mut avoid: BTreeSet<IpAddr> = devices(desired)
            .into_iter()
            .filter(|(id, _)| id != nic_id)
            .flat_map(|(_, nic)| [nic.ipv4, nic.ipv6])
            .flatten()
            .collect();
        let public_avoid = public_exclusions(tx, &old.provider_node).await?;
        if spec.address_mode == NicAddressMode::DedicatedRouted {
            avoid.extend(public_avoid.iter().copied());
        }
        next.ipv4 = if address_fits(spec.ipv4_pool.as_ref(), old.ipv4) {
            old.ipv4
        } else {
            allocate_address(tx, spec.ipv4_pool.as_ref(), &private_scope, "ipv4", &avoid).await?
        };
        next.ipv6 = if address_fits(spec.ipv6_pool.as_ref(), old.ipv6) {
            old.ipv6
        } else {
            allocate_address(tx, spec.ipv6_pool.as_ref(), &private_scope, "ipv6", &avoid).await?
        };
        next.public_ipv4 = match spec.address_mode {
            NicAddressMode::SharedNat => spec.shared_ipv4,
            NicAddressMode::DedicatedRouted => next.ipv4,
            NicAddressMode::DedicatedNat => {
                if old.address_mode == NicAddressMode::DedicatedNat
                    && address_fits(spec.public_ipv4_pool.as_ref(), old.public_ipv4)
                {
                    old.public_ipv4
                } else {
                    allocate_address(
                        tx,
                        spec.public_ipv4_pool.as_ref(),
                        &public_scope,
                        "ipv4",
                        &public_avoid,
                    )
                    .await?
                }
            }
        };
        next.public_ipv6 = match spec.address_mode {
            NicAddressMode::SharedNat => spec.shared_ipv6,
            NicAddressMode::DedicatedRouted => next.ipv6,
            NicAddressMode::DedicatedNat => {
                if old.address_mode == NicAddressMode::DedicatedNat
                    && address_fits(spec.public_ipv6_pool.as_ref(), old.public_ipv6)
                {
                    old.public_ipv6
                } else {
                    allocate_address(
                        tx,
                        spec.public_ipv6_pool.as_ref(),
                        &public_scope,
                        "ipv6",
                        &public_avoid,
                    )
                    .await?
                }
            }
        };
        sqlx::query("INSERT INTO cvm_nic_allocations(id,interface_id,instance_id,publication_id,provider_node,consumer_node,spec) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(&next.allocation_id).bind(nic_id).bind(instance_id).bind(&old.publication_id).bind(&old.provider_node).bind(&instance.node_id).bind(serde_json::to_value(&next)?).execute(&mut **tx).await?;
        transfer_or_claim(
            tx,
            nic_id,
            &next.allocation_id,
            "global",
            "slot",
            &next.slot.to_string(),
        )
        .await?;
        for (address, kind) in [(next.ipv4, "ipv4"), (next.ipv6, "ipv6")] {
            if let Some(address) = address {
                transfer_or_claim(
                    tx,
                    nic_id,
                    &next.allocation_id,
                    &private_scope,
                    kind,
                    &address.to_string(),
                )
                .await?;
            }
        }
        if next.address_mode != NicAddressMode::SharedNat {
            for (address, kind) in [(next.public_ipv4, "ipv4"), (next.public_ipv6, "ipv6")] {
                if let Some(address) = address {
                    transfer_or_claim(
                        tx,
                        nic_id,
                        &next.allocation_id,
                        &public_scope,
                        kind,
                        &address.to_string(),
                    )
                    .await?;
                }
            }
        }
    } else {
        sqlx::query("UPDATE cvm_nic_allocations SET spec=$2 WHERE id=$1")
            .bind(&next.allocation_id)
            .bind(serde_json::to_value(&next)?)
            .execute(&mut **tx)
            .await?;
    }
    for nic in &mut desired.spec.interfaces {
        if nic.id == nic_id {
            nic.allocated = Some(next.clone());
        }
    }
    desired
        .spec
        .routing
        .validate(&devices(desired))
        .map_err(|error| {
            StoreError::Conflict(format!("请先修改与新规格不兼容的路由依赖：{error}"))
        })?;
    create_binding(
        tx,
        instance_id,
        nic_id,
        &instance.node_id,
        &next,
        offer,
        &provider,
    )
    .await?;
    Ok(())
}

async fn transfer_or_claim(
    tx: &mut Transaction<'_, Postgres>,
    nic: &str,
    allocation: &str,
    scope: &str,
    kind: &str,
    value: &str,
) -> Result<()> {
    let owner:Option<(i64,String)>=sqlx::query_as("SELECT r.id,a.interface_id FROM cvm_nic_resources r JOIN cvm_nic_allocations a ON a.id=r.allocation_id WHERE r.scope=$1 AND r.kind=$2 AND r.value=$3 AND NOT r.released AND r.binding_id IS NULL").bind(scope).bind(kind).bind(value).fetch_optional(&mut **tx).await?;
    if let Some((id, interface)) = owner {
        if interface != nic {
            return Err(StoreError::Conflict("新地址仍归属于另一张网卡".into()));
        }
        sqlx::query("UPDATE cvm_nic_resources SET allocation_id=$2 WHERE id=$1")
            .bind(id)
            .bind(allocation)
            .execute(&mut **tx)
            .await?;
    } else {
        claim(tx, allocation, None, scope, kind, value).await?;
    }
    Ok(())
}

pub(super) async fn restore_allocation_ownership(
    tx: &mut Transaction<'_, Postgres>,
    nic: &str,
    old: &AllocatedNic,
) -> Result<()> {
    transfer_or_claim(
        tx,
        nic,
        &old.allocation_id,
        "global",
        "slot",
        &old.slot.to_string(),
    )
    .await?;
    for (address, kind) in [(old.ipv4, "ipv4"), (old.ipv6, "ipv6")] {
        if let Some(address) = address {
            transfer_or_claim(
                tx,
                nic,
                &old.allocation_id,
                &format!("private:{}", old.resource_domain),
                kind,
                &address.to_string(),
            )
            .await?;
        }
    }
    if old.address_mode != NicAddressMode::SharedNat {
        for (address, kind) in [(old.public_ipv4, "ipv4"), (old.public_ipv6, "ipv6")] {
            if let Some(address) = address {
                transfer_or_claim(
                    tx,
                    nic,
                    &old.allocation_id,
                    &format!("public:{}", old.provider_node),
                    kind,
                    &address.to_string(),
                )
                .await?;
            }
        }
    }
    Ok(())
}
