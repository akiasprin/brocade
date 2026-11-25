//! One edit of the container and all its related network intent.
use super::*;
use std::net::Ipv4Addr;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNetworkEditRequest {
    pub request_id: String,
    pub expected_generation: u64,
    #[serde(default)]
    pub mounts: Vec<CvmNetworkMount>,
    #[serde(default)]
    pub detach_interface_ids: Vec<String>,
    #[serde(default)]
    pub entry_additions: Vec<CvmNetworkEntryAddition>,
    #[serde(default)]
    pub entry_releases: Vec<CvmNetworkEntryRelease>,
    #[serde(default)]
    pub routes: Option<CvmNetworkRoutesWrite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNetworkMount {
    /// Local to this save; also accepted as `interface_id` in its final routes.
    pub draft_id: String,
    pub publication_id: String,
    pub transport: TransportKind,
    #[serde(default)]
    pub use_dns_suggestions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNetworkEntryAddition {
    pub publication_id: String,
    pub address: Ipv4Addr,
    pub mode: EntryMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNetworkEntryRelease {
    pub id: String,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNetworkRoutesWrite {
    pub expected_revision: u64,
    /// Complete final route table. Omit the write to apply mount defaults and drop detached targets.
    pub routes: ContainerRoutes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNetworkEditResult {
    pub instance: CvmInstance,
    pub mounted_interfaces: BTreeMap<String, String>,
    pub input_sha256: String,
}

impl PgStore {
    pub async fn edit_cvm_network(
        &self,
        actor: &AdminContext,
        id: &str,
        request: CvmNetworkEditRequest,
        review: CvmMutationReview,
    ) -> Result<CvmNetworkEditResult> {
        request_id(&request.request_id)?;
        request.validate()?;
        let digest = request_digest(&("network-edit", id, &request))?;
        let mut tx = self.pool.begin().await?;
        releases::lock(&mut tx).await?;

        // Entry-only edits do not get a container operation row. Replay all edits through the
        // same receipt before checking the optimistic generation or capturing release scope.
        if let Some(saved) = replay(&mut tx, actor, &request.request_id, &digest).await? {
            let tenant: String =
                sqlx::query_scalar("SELECT tenant_id FROM cvm_instances WHERE id=$1")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| StoreError::NotFound("CVM".into()))?;
            require_manager(actor, &tenant)?;
            tx.rollback().await?;
            return Ok(serde_json::from_value(saved)?);
        }
        operation_replay(&mut tx, actor, &request.request_id, &digest).await?;
        let instance = entries::target_locked(&mut tx, actor, id).await?;
        if instance.desired.generation != request.expected_generation {
            return Err(StoreError::Conflict("容器已变化，请刷新后操作".into()));
        }
        let before_first_apply = if instance.status == ApplyStatus::Applied {
            false
        } else if instance.status == ApplyStatus::Pending
            && request.detach_interface_ids.is_empty()
            && unpublished_create(&mut tx, &instance).await?
        {
            true
        } else if request.routes_only()
            && matches!(instance.status, ApplyStatus::Failed | ApplyStatus::Unknown)
        {
            false
        } else {
            return Err(StoreError::Conflict("请先完成或核查当前容器操作".into()));
        };
        if let Some(table) = &request.routes {
            check_route_revision(&instance.desired, table.expected_revision)?;
        }
        if request.mounts.iter().any(|mount| {
            instance
                .desired
                .spec
                .interfaces
                .iter()
                .any(|nic| nic.id == mount.draft_id)
        }) {
            return Err(invalid("草稿网卡标识不能与已有网卡相同"));
        }
        let before = releases::begin(
            &mut tx,
            Scope {
                instances: vec![id],
                nic_publications: request
                    .mounts
                    .iter()
                    .map(|m| m.publication_id.as_str())
                    .collect(),
                entry_publications: request
                    .entry_additions
                    .iter()
                    .map(|e| e.publication_id.as_str())
                    .collect(),
                entries: request
                    .entry_releases
                    .iter()
                    .map(|e| e.id.as_str())
                    .collect(),
                ..Scope::default()
            },
        )
        .await?;

        let mut desired = instance.desired.clone();
        if !request.detach_interface_ids.is_empty() {
            let caps = host_locked(&mut tx, &instance.node_id, false).await?;
            for interface_id in &request.detach_interface_ids {
                desired.spec.routing.remove(interface_id);
                detach_nic_tx(&mut tx, &mut desired, &caps, interface_id).await?;
            }
        }
        for entry in &request.entry_releases {
            entries::release_entry_tx(&mut tx, actor, &entry.id, entry.expected_revision, Some(id))
                .await?;
        }
        for (index, entry) in request.entry_additions.iter().enumerate() {
            let key = request_digest(&(
                "network-edit-entry",
                actor.operator_id(),
                id,
                &request.request_id,
                index,
            ))?;
            let entry_id = format!("entry-{}", &key[..32]);
            entries::add_entry_tx(&mut tx, actor, &instance, &entry_id, entry).await?;
        }
        let mut mounted_interfaces = BTreeMap::new();
        for mount in &request.mounts {
            let key =
                request_digest(&("network-edit-mount", &request.request_id, &mount.draft_id))?;
            let nic_id = mount_nic_tx(
                &mut tx,
                actor,
                &instance,
                &mut desired,
                NicMountEdit {
                    key: &key,
                    publication_id: &mount.publication_id,
                    transport: mount.transport,
                    use_dns_suggestions: mount.use_dns_suggestions && request.routes.is_none(),
                    default_routes: request.routes.is_none(),
                },
                before_first_apply,
            )
            .await?;
            mounted_interfaces.insert(mount.draft_id.clone(), nic_id);
        }
        if let Some(table) = request.routes.as_ref() {
            let mut routes = table.routes.clone();
            resolve_draft_targets(&mut routes, &mounted_interfaces);
            set_routes_tx(&mut tx, &instance.node_id, &mut desired, routes).await?;
        }
        desired.spec.routing.normalize();
        if desired.spec.routing != instance.desired.spec.routing {
            desired
                .spec
                .routing
                .validate(&devices(&desired))
                .map_err(invalid)?;
            advance_routes(&mut desired.spec.routing)?;
            persist_routes(&mut tx, actor, &desired).await?;
        }
        if desired != instance.desired {
            save_network(
                &mut tx,
                actor,
                &request.request_id,
                &digest,
                "network_edit",
                desired,
            )
            .await?;
        }
        if !request.entry_releases.is_empty() || !request.entry_additions.is_empty() {
            entries::finish_entry_edits(&mut tx, id, !request.entry_releases.is_empty()).await?;
        }

        let input_sha256 = edit_input_sha(&mut tx, id, &digest, &mounted_interfaces).await?;
        if review
            .expected_input_sha256
            .as_ref()
            .is_some_and(|sha| sha != &input_sha256)
        {
            return Err(StoreError::Conflict(
                "网络配置输入已变化，请重新预览后提交；未保存任何改动".into(),
            ));
        }
        if review.preview {
            releases::preview(&mut tx, before).await?;
        } else {
            releases::commit(&mut tx, actor, before).await?;
        }
        let row = sqlx::query(&format!("{INSTANCE_SELECT} WHERE id=$1"))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let mut instance = from_row(&row)?;
        if !review.preview {
            annotate_pending_entries(&mut tx, std::slice::from_mut(&mut instance)).await?;
        }
        let result = CvmNetworkEditResult {
            instance,
            mounted_interfaces,
            input_sha256,
        };
        if review.preview {
            tx.rollback().await?;
        } else {
            remember(&mut tx, actor, &request.request_id, &digest, None, &result).await?;
            tx.commit().await?;
        }
        Ok(result)
    }
}

impl CvmNetworkEditRequest {
    fn validate(&self) -> Result<()> {
        if self.mounts.len() > MAX_INTERFACES
            || self.detach_interface_ids.len() > MAX_INTERFACES
            || self.entry_additions.len() > MAX_ENTRIES_PER_INSTANCE
            || self.entry_releases.len() > MAX_ENTRIES_PER_INSTANCE
        {
            return Err(invalid("单次编辑的网卡或入口数量超出上限"));
        }
        unique_ids(self.mounts.iter().map(|m| m.draft_id.as_str()))?;
        unique_ids(self.detach_interface_ids.iter().map(String::as_str))?;
        unique_ids(self.entry_releases.iter().map(|entry| entry.id.as_str()))?;
        Ok(())
    }

    fn routes_only(&self) -> bool {
        self.routes.is_some()
            && self.mounts.is_empty()
            && self.detach_interface_ids.is_empty()
            && self.entry_additions.is_empty()
            && self.entry_releases.is_empty()
    }
}

fn unique_ids<'a>(ids: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if !valid_id(id) || !seen.insert(id) {
            return Err(invalid("草稿网卡、卸载网卡或撤销入口的标识无效或重复"));
        }
    }
    Ok(())
}

fn resolve_draft_targets(routes: &mut ContainerRoutes, mounts: &BTreeMap<String, String>) {
    let targets = routes
        .priorities
        .iter_mut()
        .map(|p| &mut p.target)
        .chain(routes.routes.iter_mut().map(|r| &mut r.target));
    for target in targets {
        if let RouteTarget::Interface { interface_id } = target {
            if let Some(real) = mounts.get(interface_id) {
                interface_id.clone_from(real);
            }
        }
    }
}

/// A newer revision may supersede unpublished create intent, never an attempted first launch.
/// Release and deployment selection share the isolation lock held by the caller.
async fn unpublished_create(
    tx: &mut Transaction<'_, Postgres>,
    instance: &CvmInstance,
) -> Result<bool> {
    if !instance.desired.spec.execution.is_cube()
        || instance.report.is_some()
        || !matches!(
            instance.operation_kind.as_deref(),
            Some("create" | "network_edit")
        )
    {
        return Ok(false);
    }
    let never_selected: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_revision_instances c JOIN cvm_operations o ON o.instance_id=c.instance_id AND o.generation=c.generation AND o.kind='create' WHERE c.instance_id=$1 AND c.generation=1 AND NOT EXISTS(SELECT 1 FROM cvm_serving_nodes s WHERE s.node_id=$2 AND s.revision_id>=c.revision_id) AND NOT EXISTS(SELECT 1 FROM deployment_target_state s WHERE s.node_id=$2 AND s.cvm_revision_id>=c.revision_id))")
        .bind(&instance.desired.id).bind(&instance.node_id).fetch_one(&mut **tx).await?;
    Ok(never_selected)
}

/// Fence the actual allocated inputs, not opaque operation IDs or sealed credentials. The digest
/// also covers entry-only and bare-metal edits, for which no new Cube compiler bundle may exist.
async fn edit_input_sha(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    request: &str,
    mounts: &BTreeMap<String, String>,
) -> Result<String> {
    let instance: (String, Option<String>) = sqlx::query_as("SELECT i.desired->>'spec_sha256',a.input_sha256 FROM cvm_instances i LEFT JOIN cvm_spec_artifact_sets a ON a.instance_id=i.id AND a.generation=i.generation WHERE i.id=$1")
        .bind(id).fetch_one(&mut **tx).await?;
    let entry_rows: Vec<serde_json::Value> = sqlx::query_scalar("SELECT jsonb_build_array(e.id,e.revision,e.publication_id,p.revision,e.provider_node,e.address,e.mode,e.released) FROM cvm_entries e JOIN cvm_entry_publications p ON p.id=e.publication_id WHERE e.instance_id=$1 AND NOT e.cleaned ORDER BY e.id")
        .bind(id).fetch_all(&mut **tx).await?;
    let tunnels: Vec<serde_json::Value> = sqlx::query_scalar("SELECT t.target-'id' FROM cvm_entry_tunnels t WHERE NOT t.cleaned AND EXISTS(SELECT 1 FROM cvm_entries e JOIN cvm_instances i ON i.id=e.instance_id WHERE e.instance_id=$1 AND NOT e.cleaned AND e.provider_node=t.entry_node AND i.node_id=t.target_node) ORDER BY t.target-'id'")
        .bind(id).fetch_all(&mut **tx).await?;
    let bindings: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT target-'id' FROM cvm_nic_bindings WHERE interface_id=ANY($1) ORDER BY interface_id",
    )
    .bind(mounts.values().map(String::as_str).collect::<Vec<_>>())
    .fetch_all(&mut **tx)
    .await?;
    request_digest(&(request, instance, entry_rows, tunnels, bindings))
}
