use super::*;
use crate::cvm::releases::Scope;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmAdopt {
    pub request_id: String,
    pub tenant_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
    pub node_id: String,
    pub name: String,
    /// Freeze the inventory selected by the operator; never silently adopt another OS.
    pub identity: HostIdentity,
}

impl PgStore {
    pub async fn adopt_cvm_host(
        &self,
        actor: &AdminContext,
        request: CvmAdopt,
    ) -> Result<CvmInstance> {
        if !actor.is_system_admin() {
            return Err(StoreError::Forbidden("只有系统管理员可以纳管裸机".into()));
        }
        require_manager(actor, &request.tenant_id)?;
        request_id(&request.request_id)?;
        request.identity.validate().map_err(invalid)?;
        if request.name.trim().is_empty()
            || request.name.chars().count() > 80
            || request.name.chars().any(char::is_control)
        {
            return Err(invalid("实例名称须为 1–80 字"));
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
        let caps = host_locked(&mut tx, &request.node_id, false).await?;
        let host_system = caps
            .host_system
            .as_ref()
            .ok_or_else(|| StoreError::Unsupported("该机器尚未上报宿主系统管理能力".into()))?;
        let host = host_system.adoptable().ok_or_else(|| {
            StoreError::Unsupported("请先升级 Agent 以启用动态宿主系统管理".into())
        })?;
        let node_tenant: String = sqlx::query_scalar("SELECT tenant_id FROM nodes WHERE id=$1")
            .bind(&request.node_id)
            .fetch_one(&mut *tx)
            .await?;
        if !actor.is_global_scope() && !actor.can_access_tenant(&node_tenant) {
            return Err(StoreError::Forbidden("无权纳管该机器".into()));
        }
        if host.identity != request.identity {
            return Err(StoreError::Conflict(
                "机器身份已变化，请刷新后重新核对".into(),
            ));
        }
        // Cube workloads may coexist. Only an existing physical-host binding and its unfinished
        // cleanup fence a second adoption of the same operating system.
        let occupied: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_instances i WHERE i.node_id=$1 AND i.desired#>>'{spec,execution,kind}'='baremetal' AND (i.reserved OR EXISTS(SELECT 1 FROM cvm_interfaces n WHERE n.instance_id=i.id AND NOT n.detached_confirmed) OR EXISTS(SELECT 1 FROM cvm_entries e WHERE e.instance_id=i.id AND NOT e.cleaned)))")
            .bind(&request.node_id).fetch_one(&mut *tx).await?;
        if occupied {
            return Err(StoreError::Conflict(
                "该宿主系统已被纳管或仍有待清理资源".into(),
            ));
        }
        let resources = host_system
            .resources
            .clone()
            .ok_or_else(|| StoreError::Unsupported("请先升级 Agent 以核验宿主系统容量".into()))?;
        if resources.vcpus == 0 || resources.memory_bytes == 0 || resources.root_disk_bytes == 0 {
            return Err(invalid("裸机容量尚未核验"));
        }
        let spec = InstanceSpec {
            execution: ExecutionTarget::BareMetal {
                identity: request.identity,
            },
            routing: ContainerRoutes::default(),
            name: request.name.trim().into(),
            image: None,
            resources,
            login_revision: 0,
            legacy_ssh_public_key: None,
            interfaces: vec![InterfaceSpec {
                id: new_id("nic")?,
                allocated: None,
                connection: InterfaceRequest {
                    source_id: "host".into(),
                    purpose: InterfacePurpose::Contact,
                    default_ipv4: false,
                    default_ipv6: false,
                },
            }],
        };
        let desired = InstanceDesired {
            artifacts: None,
            id: new_id("cvm")?,
            operation_id: new_id("op")?,
            generation: 1,
            spec_sha256: spec_digest(&spec)?,
            spec,
            power: PowerState::Running,
            delete_data: false,
            restart_epoch: 0,
            reinstall_epoch: 0,
            detached_interfaces: vec![],
        };
        sqlx::query("INSERT INTO cvm_instances(id,tenant_id,user_id,node_id,name,generation,desired) VALUES($1,$2,$3,$4,$5,1,$6)")
            .bind(&desired.id).bind(&request.tenant_id).bind(&request.user_id).bind(&request.node_id).bind(&desired.spec.name).bind(serde_json::to_value(&desired)?).execute(&mut *tx).await?;
        insert_interface(&mut tx, &desired.id, &desired.spec.interfaces[0]).await?;
        save_version(
            &mut tx,
            actor,
            &request.request_id,
            &digest,
            "adopt",
            &desired,
        )
        .await?;
        crate::cvm::releases::commit(&mut tx, actor, cvm_before).await?;
        tx.commit().await?;
        self.cvm_instance(actor, &desired.id).await
    }
}
