use super::*;
use crate::cvm::releases::Scope;

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
        require_owner(actor, &instance.tenant_id, instance.user(), true)?;
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
    artifacts::freeze(tx, desired, kind).await?;
    sqlx::query("INSERT INTO cvm_operations(id,instance_id,generation,kind,actor_id,request_id,request_sha256) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(&desired.operation_id).bind(&desired.id).bind(desired.generation as i64).bind(kind).bind(actor.operator_id()).bind(request).bind(digest).execute(&mut **tx).await?;
    sqlx::query("UPDATE cvm_instances SET generation=$2,desired=$3,name=$4,status='pending',updated_at=now() WHERE id=$1").bind(&desired.id).bind(desired.generation as i64).bind(serde_json::to_value(desired)?).bind(&desired.spec.name).execute(&mut **tx).await?;
    Ok(())
}

/// Runtime-only generations can belong to the serving lineage while a newer user intent is
/// staged. Freeze either branch without replacing the other branch's authoritative desired.
pub(super) async fn save_system_version(
    tx: &mut Transaction<'_, Postgres>,
    desired: &mut InstanceDesired,
    kind: &str,
    update_current: bool,
) -> Result<()> {
    ensure_access_generation(tx, &desired.id, desired.generation).await?;
    desired.generation = desired
        .generation
        .checked_add(1)
        .filter(|g| *g <= i64::MAX as u64)
        .ok_or_else(|| invalid("CVM execution generation exhausted"))?;
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
    artifacts::freeze(tx, desired, kind).await?;
    sqlx::query("INSERT INTO cvm_operations(id,instance_id,generation,kind,actor_id,request_id,request_sha256) VALUES($1,$2,$3,$4,'system-network',$5,$6)")
        .bind(&desired.operation_id).bind(&desired.id).bind(desired.generation as i64).bind(kind).bind(&request).bind(request_digest(desired)?).execute(&mut **tx).await?;
    if update_current {
        sqlx::query("UPDATE cvm_instances SET generation=$2,desired=$3,status='pending',updated_at=now() WHERE id=$1")
            .bind(&desired.id).bind(desired.generation as i64).bind(serde_json::to_value(&*desired)?).execute(&mut **tx).await?;
    }
    Ok(())
}

impl PgStore {
    pub async fn create_cvm(
        &self,
        actor: &AdminContext,
        request: CvmCreate,
    ) -> Result<CvmInstance> {
        require_manager(actor, &request.tenant_id)?;
        request_id(&request.request_id)?;
        if request.name.trim().is_empty()
            || request.name.chars().count() > 80
            || request.name.chars().any(char::is_control)
        {
            return Err(invalid("实例名称无效"));
        }
        let digest = request_digest(&request)?;
        let mut tx = self.pool.begin().await?;
        crate::cvm::releases::lock(&mut tx).await?;
        let cvm_before =
            crate::cvm::releases::begin(&mut tx, Scope::node(&request.node_id)).await?;
        if let Some(instance) =
            operation_replay(&mut tx, actor, &request.request_id, &digest).await?
        {
            tx.commit().await?;
            return Ok(instance);
        }
        check_assignee(&mut tx, &request.tenant_id, request.user_id.as_deref()).await?;
        require_host_manager(&mut tx, actor, &request.node_id).await?;
        let caps = host_locked(&mut tx, &request.node_id, false).await?;
        if !caps.create || !caps.managed_networks {
            return Err(StoreError::Unsupported("机器尚未支持创建 CVM".into()));
        }
        caps.validate_interfaces(&request.interfaces)
            .map_err(invalid)?;
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
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM cvm_instances WHERE node_id=$1 AND reserved")
                .bind(&request.node_id)
                .fetch_one(&mut *tx)
                .await?;
        if count >= MAX_INSTANCES_PER_MACHINE as i64 {
            return Err(invalid("机器实例数量已达上限"));
        }
        // Host inventory is an observed baseline, not a logical allocation ceiling.
        let id = new_id("cvm")?;
        let operation_id = new_id("op")?;
        let spec = InstanceSpec {
            execution: ExecutionTarget::Cube,
            routing: ContainerRoutes::default(),
            name: request.name.trim().into(),
            image: Some(image),
            resources,
            login_revision: 1,
            legacy_ssh_public_key: None,
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
            artifacts: None,
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
        credentials::generate(&mut tx, &id).await?;
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
        crate::cvm::releases::commit(&mut tx, actor, cvm_before).await?;
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
        Ok(self
            .command_cvm_reviewed(actor, id, request, CvmMutationReview::default())
            .await?
            .instance)
    }
    pub async fn command_cvm_reviewed(
        &self,
        actor: &AdminContext,
        id: &str,
        request: CvmCommand,
        review: CvmMutationReview,
    ) -> Result<CvmMutationResult> {
        if review.preview
            && !matches!(
                request.change,
                CvmChange::Configure { .. } | CvmChange::Detach { .. }
            )
        {
            return Err(invalid("该操作不支持配置编译预览"));
        }
        let before = self.cvm_instance(actor, id).await?;
        require_owner(actor, &before.tenant_id, before.user(), true)?;
        if !matches!(
            request.change,
            CvmChange::Start | CvmChange::Stop | CvmChange::Restart
        ) {
            require_manager(actor, &before.tenant_id)?;
        }

        request_id(&request.request_id)?;
        let digest = request_digest(&(id, &request))?;
        let mut tx = self.pool.begin().await?;
        crate::cvm::releases::lock(&mut tx).await?;
        let cvm_before = crate::cvm::releases::begin(&mut tx, Scope::instance(id)).await?;
        if let Some(instance) =
            operation_replay(&mut tx, actor, &request.request_id, &digest).await?
        {
            tx.commit().await?;
            return Ok(CvmMutationResult::existing(instance));
        }
        if matches!(request.change, CvmChange::UndoPending { .. }) {
            if review.preview || review.expected_input_sha256.is_some() {
                return Err(invalid("撤回操作使用当前容器版本"));
            }
            return self
                .undo_pending_cvm(tx, actor, id, request, digest, cvm_before)
                .await;
        }
        let recovery = matches!(
            request.change,
            CvmChange::Delete { .. } | CvmChange::ReleaseHost | CvmChange::Retry
        );
        let caps = host_locked(&mut tx, &before.node_id, recovery).await?;
        let row = sqlx::query(&format!("{INSTANCE_SELECT} WHERE id=$1 FOR UPDATE"))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let instance = from_row(&row)?;
        // Recheck under the mutation lock: a concurrent revocation may have changed ownership.
        require_owner(actor, &instance.tenant_id, instance.user(), true)?;
        if instance.desired.generation != request.expected_generation {
            return Err(StoreError::Conflict(
                "CVM 已发生变化，请重新读取后再操作".into(),
            ));
        }
        if matches!(request.change, CvmChange::Retry) {
            sqlx::query("UPDATE cvm_instances SET status='pending',updated_at=now() WHERE id=$1 AND status IN ('failed','unknown')").bind(id).execute(&mut *tx).await?;
            sqlx::query("UPDATE cvm_operations SET status='pending',updated_at=now() WHERE id=$1 AND status IN ('failed','unknown')").bind(&instance.desired.operation_id).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(CvmMutationResult::existing(
                self.cvm_instance(actor, id).await?,
            ));
        }
        if instance.desired.power == PowerState::Deleted {
            return Err(invalid("已进入删除流程的实例不能重新创建或重放"));
        }
        if instance.desired.generation >= 10_000 {
            return Err(invalid("实例规格历史达到本版本上限"));
        }
        let baremetal = !instance.desired.spec.execution.is_cube();
        if baremetal
            && !matches!(
                request.change,
                CvmChange::ReleaseHost | CvmChange::Detach { .. }
            )
        {
            return Err(StoreError::Unsupported(
                "裸机不支持容器启停、改配、重装或清盘删除；请使用解除纳管".into(),
            ));
        }
        if !baremetal && matches!(request.change, CvmChange::ReleaseHost) {
            return Err(invalid("只有裸机实例可以解除纳管"));
        }
        let mut desired = instance.desired.clone();
        let kind = match request.change {
            CvmChange::Assign { user_id } => {
                if baremetal {
                    return Err(StoreError::Unsupported("裸机使用者由纳管关系管理".into()));
                }
                if user_id == instance.user_id {
                    return self.finish_cvm_review(tx, actor, id, review, true).await;
                }
                check_assignee(&mut tx, &instance.tenant_id, user_id.as_deref()).await?;
                if instance.user_id.is_some() && user_id.is_some() {
                    return Err(StoreError::Conflict(
                        "请先收回原使用授权并完成关机和网络清理，再授权其他用户".into(),
                    ));
                }
                if user_id.is_some()
                    && (!desired.spec.routing.routes.is_empty()
                        || desired
                            .spec
                            .interfaces
                            .iter()
                            .any(|n| n.allocated.is_some()))
                {
                    return Err(StoreError::Conflict(
                        "请先完成旧网络清理，再授权使用者".into(),
                    ));
                }
                if user_id.is_some() {
                    let cleanup: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_nic_allocations WHERE instance_id=$1 AND NOT released) OR EXISTS(SELECT 1 FROM cvm_entries WHERE instance_id=$1 AND NOT cleaned)")
                        .bind(id).fetch_one(&mut *tx).await?;
                    let revoked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_instance_access_events WHERE instance_id=$1 AND user_id IS NULL)")
                        .bind(id).fetch_one(&mut *tx).await?;
                    if cleanup
                        || (revoked
                            && (desired.power != PowerState::Stopped
                                || instance.status != ApplyStatus::Applied
                                || !instance.report.as_ref().is_some_and(|report| {
                                    report.power == Some(PowerState::Stopped)
                                        && report.generation == desired.generation
                                        && report.spec_sha256 == desired.spec_sha256
                                })))
                    {
                        return Err(StoreError::Conflict(
                            "请先发布收回授权，并等待关机和网络清理确认".into(),
                        ));
                    }
                }
                if user_id.is_none() {
                    desired.power = PowerState::Stopped;
                    desired.spec.routing = ContainerRoutes::default();
                    nics::withdraw(&mut tx, id, None).await?;
                    nics::entries::release_instance(&mut tx, id).await?;
                    desired
                        .spec
                        .interfaces
                        .retain(|n| n.connection.purpose == InterfacePurpose::Contact);
                    sqlx::query("UPDATE cvm_interfaces SET desired_attached=false WHERE instance_id=$1 AND purpose='business'").bind(id).execute(&mut *tx).await?;
                }
                desired.spec.login_revision = credentials::generate(&mut tx, id).await?;
                desired.spec.legacy_ssh_public_key = None;
                sqlx::query("UPDATE cvm_instances SET user_id=$2 WHERE id=$1")
                    .bind(id)
                    .bind(&user_id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("UPDATE cvm_volumes SET user_id=$2 WHERE instance_id=$1")
                    .bind(id)
                    .bind(&user_id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("INSERT INTO cvm_instance_access_events(instance_id,tenant_id,previous_user_id,user_id,actor_id,generation) VALUES($1,$2,$3,$4,$5,$6)")
                    .bind(id).bind(&instance.tenant_id).bind(&instance.user_id).bind(&user_id).bind(actor.operator_id()).bind((desired.generation+1) as i64).execute(&mut *tx).await?;
                "assign"
            }
            CvmChange::ResetPassword => {
                if !caps.stable_storage {
                    return Err(StoreError::Unsupported(
                        "当前宿主不支持安全更新登录密码".into(),
                    ));
                }
                desired.spec.login_revision = credentials::generate(&mut tx, id).await?;
                desired.spec.legacy_ssh_public_key = None;
                "reset_password"
            }

            CvmChange::Restart => {
                if !caps.restart || !caps.stable_storage || desired.power != PowerState::Running {
                    return Err(StoreError::Unsupported("该实例当前不能重启".into()));
                }
                desired.restart_epoch += 1;
                "restart"
            }
            CvmChange::Reinstall { image_id } => {
                if !caps.reinstall || !caps.stable_storage {
                    return Err(StoreError::Unsupported("机器尚未开放重装".into()));
                }
                desired.spec.image = Some(
                    caps.images
                        .iter()
                        .find(|image| image.id == image_id)
                        .ok_or_else(|| invalid("系统镜像不可用"))?
                        .clone(),
                );
                desired.spec.login_revision = credentials::generate(&mut tx, id).await?;
                desired.spec.legacy_ssh_public_key = None;
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
            CvmChange::ReleaseHost => {
                desired.power = PowerState::Deleted;
                desired.delete_data = false;
                desired.spec.routing.routes.clear();
                desired.spec.routing.priorities.clear();
                desired.spec.routing.revision += 1;
                nics::withdraw(&mut tx, id, None).await?;
                nics::entries::release_instance(&mut tx, id).await?;
                "release_host"
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
                nics::detach_nic_tx(&mut tx, &mut desired, &caps, &interface_id).await?;
                "detach"
            }
            CvmChange::Retry | CvmChange::UndoPending { .. } => {
                unreachable!("recovery returned before changing the immutable specification")
            }
        };
        if !matches!(kind, "delete" | "release_host" | "assign")
            && instance.status != ApplyStatus::Applied
        {
            return Err(invalid("请先核查或重试当前操作"));
        }
        if desired == instance.desired {
            return self.finish_cvm_review(tx, actor, id, review, true).await;
        }
        desired.generation += 1;
        desired.operation_id = new_id("op")?;
        desired.spec_sha256 = spec_digest(&desired.spec)?;
        // Cleanup assignment belongs to this generation, outside the immutable
        // specification. Keep it stable when the matching receipt is accepted.
        desired.detached_interfaces = sqlx::query_scalar("SELECT id FROM cvm_interfaces WHERE instance_id=$1 AND NOT desired_attached AND NOT detached_confirmed ORDER BY id")
            .bind(id).fetch_all(&mut *tx).await?;
        save_version(&mut tx, actor, &request.request_id, &digest, kind, &desired).await?;
        artifacts::check_review(&mut tx, id, &review).await?;
        if !review.preview {
            crate::cvm::releases::commit(&mut tx, actor, cvm_before).await?;
        }
        self.finish_cvm_review(tx, actor, id, review, false).await
    }
}
