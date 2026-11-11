use super::*;

pub(super) fn spec_digest(spec: &InstanceSpec) -> Result<String> {
    Ok(sha256_hex(&serde_json::to_vec(spec)?))
}

pub(super) fn request_digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(sha256_hex(&serde_json::to_vec(value)?))
}

pub(super) fn request_id(value: &str) -> Result<()> {
    if !valid_id(value) || value.len() < 16 {
        return Err(invalid(
            "request_id must be a stable 16–96 character identifier",
        ));
    }
    Ok(())
}

pub(super) fn new_id(prefix: &str) -> Result<String> {
    Ok(format!("{prefix}-{}", crate::generate_uuid_v4()?))
}

pub(super) async fn operation_replay(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    request: &str,
    digest: &str,
) -> Result<Option<CvmInstance>> {
    // Lock a request identity even before its first row exists; concurrent duplicates cannot allocate twice.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("cvm:{}:{request}", actor.operator_id()))
        .execute(&mut **tx)
        .await?;
    let row = sqlx::query(
        "SELECT instance_id,request_sha256 FROM cvm_operations WHERE actor_id=$1 AND request_id=$2",
    )
    .bind(actor.operator_id())
    .bind(request)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(row) = row {
        if row.try_get::<String, _>("request_sha256")? != digest {
            return Err(StoreError::Conflict(
                "该请求标识已用于另一项 CVM 操作".into(),
            ));
        }
        let instance = sqlx::query(&format!("{INSTANCE_SELECT} WHERE id=$1"))
            .bind(row.try_get::<String, _>("instance_id")?)
            .fetch_one(&mut **tx)
            .await?;
        let instance = from_row(&instance)?;
        require_owner(actor, &instance.tenant_id, &instance.user_id, true)?;
        return Ok(Some(instance));
    }
    Ok(None)
}

pub(super) async fn save_version(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    request: &str,
    digest: &str,
    kind: &str,
    desired: &InstanceDesired,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO cvm_spec_versions(instance_id,generation,spec,sha256) VALUES($1,$2,$3,$4)",
    )
    .bind(&desired.id)
    .bind(desired.generation as i64)
    .bind(serde_json::to_value(&desired.spec)?)
    .bind(&desired.spec_sha256)
    .execute(&mut **tx)
    .await?;
    sqlx::query("INSERT INTO cvm_operations(id,instance_id,generation,kind,actor_id,request_id,request_sha256) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(&desired.operation_id).bind(&desired.id).bind(desired.generation as i64).bind(kind).bind(actor.operator_id()).bind(request).bind(digest).execute(&mut **tx).await?;
    sqlx::query("UPDATE cvm_instances SET generation=$2,desired=$3,name=$4,status='pending',updated_at=now() WHERE id=$1").bind(&desired.id).bind(desired.generation as i64).bind(serde_json::to_value(desired)?).bind(&desired.spec.name).execute(&mut **tx).await?;
    Ok(())
}

impl PgStore {
    pub async fn create_cvm(
        &self,
        actor: &AdminContext,
        request: CvmCreate,
    ) -> Result<CvmInstance> {
        require_owner(actor, &request.tenant_id, &request.user_id, true)?;
        request_id(&request.request_id)?;
        if request.name.trim().is_empty()
            || request.name.chars().count() > 80
            || request.name.chars().any(char::is_control)
            || !valid_public_key(&request.ssh_public_key)
        {
            return Err(invalid("实例名称或 SSH 公钥无效"));
        }
        let digest = request_digest(&request)?;
        let mut tx = self.pool.begin().await?;
        crate::resource_coordination::lock(&mut tx).await?;
        if let Some(instance) =
            operation_replay(&mut tx, actor, &request.request_id, &digest).await?
        {
            tx.commit().await?;
            return Ok(instance);
        }
        let active: bool = sqlx::query_scalar(
            "SELECT status='active' FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
        )
        .bind(&request.tenant_id)
        .bind(&request.user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("user".into()))?;
        if !active {
            return Err(StoreError::Forbidden("用户已停用".into()));
        }
        let quota = quota_locked(&mut tx, &request.tenant_id, &request.user_id).await?;
        let caps = host_locked(&mut tx, &request.node_id, false).await?;
        network_quota::admission(
            &mut tx,
            &request.tenant_id,
            &request.user_id,
            &request.node_id,
            &caps,
        )
        .await?;
        if !caps.create || !caps.managed_networks {
            return Err(StoreError::Unsupported("机器尚未支持创建 CVM".into()));
        }
        caps.validate_interfaces(&request.interfaces)
            .map_err(invalid)?;
        if request
            .interfaces
            .iter()
            .any(|n| n.purpose != InterfacePurpose::Contact)
        {
            return Err(invalid(
                "创建容器保留管理主网卡；业务网卡请从已发布网络能力挂载",
            ));
        }
        let image = caps
            .images
            .iter()
            .find(|x| x.id == request.image_id)
            .ok_or_else(|| invalid("系统镜像不可用"))?
            .clone();
        let resources = request.resources.clone();
        caps.resource_limits
            .as_ref()
            .ok_or_else(|| StoreError::Unsupported("机器尚未开放 CPU、内存独立配置".into()))?
            .validate_resources(&resources)
            .map_err(invalid)?;
        admission(
            &mut tx,
            &quota,
            &request.node_id,
            Some(&resources),
            request.interfaces.len() as u64,
        )
        .await?;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM cvm_instances WHERE node_id=$1 AND reserved")
                .bind(&request.node_id)
                .fetch_one(&mut *tx)
                .await?;
        if count >= MAX_INSTANCES_PER_MACHINE as i64 {
            return Err(invalid("机器实例数量已达上限"));
        }
        let held_host_disks:i64=sqlx::query_scalar("SELECT coalesce(sum(v.bytes),0)::bigint FROM cvm_volumes v JOIN cvm_instances i ON i.id=v.instance_id WHERE i.node_id=$1 AND v.state<>'released'").bind(&request.node_id).fetch_one(&mut *tx).await?;
        let host_usage=sqlx::query("SELECT coalesce(sum((desired#>>'{spec,resources,vcpus}')::bigint),0)::bigint AS cpu,coalesce(sum((desired#>>'{spec,resources,memory_bytes}')::bigint),0)::bigint AS memory,coalesce(sum((desired#>>'{spec,resources,root_disk_bytes}')::bigint),0)::bigint AS disk FROM cvm_instances WHERE node_id=$1 AND reserved").bind(&request.node_id).fetch_one(&mut *tx).await?;
        if host_usage.try_get::<i64, _>("cpu")? as u64 + u64::from(resources.vcpus)
            > u64::from(caps.allocatable_vcpus)
            || host_usage.try_get::<i64, _>("memory")? as u64 + resources.memory_bytes
                > caps.allocatable_memory_bytes
            || held_host_disks as u64 + resources.root_disk_bytes > caps.allocatable_disk_bytes
        {
            return Err(invalid("承载机器的可预约容量不足"));
        }
        let id = new_id("cvm")?;
        let operation_id = new_id("op")?;
        let spec = InstanceSpec {
            routing: ContainerRoutes::default(),
            name: request.name.trim().into(),
            image,
            resources,
            ssh_public_key: request.ssh_public_key.trim().into(),
            interfaces: request
                .interfaces
                .into_iter()
                .map(|connection| {
                    Ok(InterfaceSpec {
                        allocated: None,
                        id: new_id("nic")?,
                        connection,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        };
        let desired = InstanceDesired {
            detached_interfaces: Vec::new(),
            id: id.clone(),
            operation_id,
            generation: 1,
            spec_sha256: spec_digest(&spec)?,
            spec,
            power: PowerState::Running,
            delete_data: false,
            restart_epoch: 0,
            reinstall_epoch: 0,
        };
        sqlx::query("INSERT INTO cvm_instances(id,tenant_id,user_id,node_id,name,generation,desired) VALUES($1,$2,$3,$4,$5,1,$6)").bind(&id).bind(&request.tenant_id).bind(&request.user_id).bind(&request.node_id).bind(&desired.spec.name).bind(serde_json::to_value(&desired)?).execute(&mut *tx).await?;
        sqlx::query(
            "INSERT INTO cvm_volumes(instance_id,tenant_id,user_id,bytes) VALUES($1,$2,$3,$4)",
        )
        .bind(&id)
        .bind(&request.tenant_id)
        .bind(&request.user_id)
        .bind(desired.spec.resources.root_disk_bytes as i64)
        .execute(&mut *tx)
        .await?;
        for nic in &desired.spec.interfaces {
            insert_interface(&mut tx, &id, nic).await?;
        }
        save_version(
            &mut tx,
            actor,
            &request.request_id,
            &digest,
            "create",
            &desired,
        )
        .await?;
        tx.commit().await?;
        self.cvm_instance(actor, &id).await
    }
}

impl PgStore {
    pub async fn command_cvm(
        &self,
        actor: &AdminContext,
        id: &str,
        request: CvmCommand,
    ) -> Result<CvmInstance> {
        let before = self.cvm_instance(actor, id).await?;
        require_owner(actor, &before.tenant_id, &before.user_id, true)?;

        request_id(&request.request_id)?;
        let digest = request_digest(&(id, &request))?;
        let mut tx = self.pool.begin().await?;
        crate::resource_coordination::lock(&mut tx).await?;
        if let Some(instance) =
            operation_replay(&mut tx, actor, &request.request_id, &digest).await?
        {
            tx.commit().await?;
            return Ok(instance);
        }
        sqlx::query("SELECT id FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE")
            .bind(&before.tenant_id)
            .bind(&before.user_id)
            .fetch_one(&mut *tx)
            .await?;
        let quota = quota_locked(&mut tx, &before.tenant_id, &before.user_id).await?;
        let recovery = matches!(request.change, CvmChange::Delete { .. } | CvmChange::Retry);
        let caps = host_locked(&mut tx, &before.node_id, recovery).await?;
        let row = sqlx::query(&format!("{INSTANCE_SELECT} WHERE id=$1 FOR UPDATE"))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let instance = from_row(&row)?;
        if instance.desired.generation != request.expected_generation {
            return Err(StoreError::Conflict(
                "CVM 已发生变化，请重新读取后再操作".into(),
            ));
        }
        if matches!(request.change, CvmChange::Retry) {
            sqlx::query("UPDATE cvm_instances SET status='pending',updated_at=now() WHERE id=$1 AND status IN ('failed','unknown','restart_required')").bind(id).execute(&mut *tx).await?;
            sqlx::query("UPDATE cvm_operations SET status='pending',updated_at=now() WHERE id=$1 AND status IN ('failed','unknown','restart_required')").bind(&instance.desired.operation_id).execute(&mut *tx).await?;
            tx.commit().await?;
            return self.cvm_instance(actor, id).await;
        }
        if instance.desired.power == PowerState::Deleted {
            return Err(invalid("已进入删除流程的实例不能重新创建或重放"));
        }
        if instance.desired.generation >= 10_000 {
            return Err(invalid("实例规格历史达到本版本上限"));
        }
        let mut desired = instance.desired.clone();
        let kind = match request.change {
            CvmChange::Restart => {
                if !caps.restart || !caps.stable_storage || desired.power != PowerState::Running {
                    return Err(StoreError::Unsupported("该实例当前不能重启".into()));
                }
                desired.restart_epoch += 1;
                "restart"
            }
            CvmChange::Reinstall {
                image_id,
                ssh_public_key,
            } => {
                if !caps.reinstall || !caps.stable_storage {
                    return Err(StoreError::Unsupported("机器尚未开放重装".into()));
                }
                if !valid_public_key(&ssh_public_key) {
                    return Err(invalid("SSH 公钥无效"));
                }
                desired.spec.image = caps
                    .images
                    .iter()
                    .find(|image| image.id == image_id)
                    .ok_or_else(|| invalid("系统镜像不可用"))?
                    .clone();
                desired.spec.ssh_public_key = ssh_public_key.trim().into();
                desired.reinstall_epoch += 1;
                desired.power = PowerState::Running;
                "reinstall"
            }
            CvmChange::Configure {
                name,
                vcpus,
                memory_bytes,
            } => {
                if !caps.configure || !caps.stable_storage || desired.power != PowerState::Stopped {
                    return Err(StoreError::Unsupported("请先关机，再修改实例配置".into()));
                }
                if name.trim().is_empty() || name.trim().chars().count() > 80 {
                    return Err(invalid("实例名称须为 1–80 字"));
                }
                let resources = Resources {
                    vcpus,
                    memory_bytes,
                    root_disk_bytes: desired.spec.resources.root_disk_bytes,
                };
                caps.resource_limits
                    .as_ref()
                    .ok_or_else(|| invalid("机器未提供独立配置范围"))?
                    .validate_resources(&resources)
                    .map_err(invalid)?;
                let old = &desired.spec.resources;
                let user_usage = sqlx::query("SELECT coalesce(sum((desired#>>'{spec,resources,vcpus}')::bigint),0)::bigint AS cpu,coalesce(sum((desired#>>'{spec,resources,memory_bytes}')::bigint),0)::bigint AS memory FROM cvm_instances WHERE tenant_id=$1 AND user_id=$2 AND reserved AND id<>$3")
                    .bind(&instance.tenant_id).bind(&instance.user_id).bind(id).fetch_one(&mut *tx).await?;
                let host_usage = sqlx::query("SELECT coalesce(sum((desired#>>'{spec,resources,vcpus}')::bigint),0)::bigint AS cpu,coalesce(sum((desired#>>'{spec,resources,memory_bytes}')::bigint),0)::bigint AS memory FROM cvm_instances WHERE node_id=$1 AND reserved AND id<>$2")
                    .bind(&instance.node_id).bind(id).fetch_one(&mut *tx).await?;
                if user_usage.try_get::<i64, _>("cpu")? as u64 + u64::from(resources.vcpus)
                    > quota.max_vcpus
                    || user_usage.try_get::<i64, _>("memory")? as u64 + resources.memory_bytes
                        > quota.max_memory_bytes
                    || host_usage.try_get::<i64, _>("cpu")? as u64 + u64::from(resources.vcpus)
                        > u64::from(caps.allocatable_vcpus)
                    || host_usage.try_get::<i64, _>("memory")? as u64 + resources.memory_bytes
                        > caps.allocatable_memory_bytes
                {
                    return Err(invalid("修改后的 CPU 或内存超过可用额度"));
                }
                // Only an observed shutdown can release the old compute allocation.
                if old != &resources
                    && instance.report.as_ref().and_then(|r| r.power) != Some(PowerState::Stopped)
                {
                    return Err(invalid("尚未确认实际关机，不能修改容量"));
                }
                desired.spec.resources = resources;
                desired.spec.name = name.trim().into();
                "configure"
            }
            CvmChange::Start => {
                network_quota::admission(
                    &mut tx,
                    &before.tenant_id,
                    &before.user_id,
                    &before.node_id,
                    &caps,
                )
                .await?;
                if !caps.start || !caps.stable_storage {
                    return Err(StoreError::Unsupported("该后端尚未通过原盘启动验证".into()));
                }
                desired.power = PowerState::Running;
                "start"
            }
            CvmChange::Stop => {
                if !caps.stop || !caps.stable_storage {
                    return Err(StoreError::Unsupported(
                        "该后端尚未通过停止并保盘验证".into(),
                    ));
                }
                desired.power = PowerState::Stopped;
                "stop"
            }
            CvmChange::Delete { delete_data } => {
                if !caps.delete
                    || (!delete_data && (!caps.stable_storage || !caps.retain_on_delete))
                {
                    return Err(StoreError::Unsupported(
                        "后端不支持所选的删除和保盘策略".into(),
                    ));
                }
                desired.power = PowerState::Deleted;
                desired.delete_data = delete_data;
                desired.spec.routing.routes.clear();
                desired.spec.routing.priorities.clear();
                desired.spec.routing.revision += 1;
                nics::withdraw(&mut tx, id, None).await?;
                // Entries belong to the container and are released with it (Q3).
                nics::entries::release_instance(&mut tx, id).await?;
                "delete"
            }
            CvmChange::Detach { interface_id } => {
                if instance.status != ApplyStatus::Applied {
                    return Err(invalid("请先完成当前实例操作，再卸载网卡"));
                }
                if desired.power == PowerState::Running && !caps.hot_detach {
                    return Err(StoreError::Unsupported(
                        "这台机器需要停止实例后卸载网卡".into(),
                    ));
                }
                if !desired.spec.interfaces.iter().any(|x| x.id == interface_id) {
                    return Err(StoreError::NotFound("网卡挂载".into()));
                }
                if desired.spec.routing.references(&interface_id) {
                    return Err(StoreError::Conflict(
                        "请先修改引用该网卡的路由和优先级，再卸载网卡".into(),
                    ));
                }
                if desired.spec.interfaces.iter().any(|n| {
                    n.id == interface_id && n.connection.purpose == InterfacePurpose::Contact
                }) {
                    return Err(invalid("管理主网卡不能卸载"));
                }
                nics::withdraw(&mut tx, id, Some(&interface_id)).await?;
                desired.spec.interfaces.retain(|x| x.id != interface_id);
                caps.validate_interfaces(
                    &desired
                        .spec
                        .interfaces
                        .iter()
                        .filter(|x| x.allocated.is_none())
                        .map(|x| x.connection.clone())
                        .collect::<Vec<_>>(),
                )
                .map_err(invalid)?;
                sqlx::query("UPDATE cvm_interfaces SET desired_attached=false WHERE id=$1 AND instance_id=$2").bind(interface_id).bind(id).execute(&mut *tx).await?;
                "detach"
            }
            CvmChange::Retry => {
                unreachable!("retry returned before changing the immutable specification")
            }
        };
        if !matches!(kind, "delete") && instance.status != ApplyStatus::Applied {
            return Err(invalid("请先核查或重试当前操作"));
        }
        desired.generation += 1;
        desired.operation_id = new_id("op")?;
        desired.spec_sha256 = spec_digest(&desired.spec)?;
        // Cleanup assignment belongs to this generation, outside the immutable
        // specification. Keep it stable when the matching receipt is accepted.
        desired.detached_interfaces = sqlx::query_scalar("SELECT id FROM cvm_interfaces WHERE instance_id=$1 AND NOT desired_attached AND NOT detached_confirmed ORDER BY id")
            .bind(id).fetch_all(&mut *tx).await?;
        save_version(&mut tx, actor, &request.request_id, &digest, kind, &desired).await?;
        tx.commit().await?;
        self.cvm_instance(actor, id).await
    }
}
