//! Subscriber identity and topology projection for primary-NIC chain egress.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize)]
pub struct CvmChainCatalog {
    pub owner: Option<CvmOwner>,
    pub supported: bool,
    pub reason: Option<String>,
    pub chains: Vec<CvmChainOption>,
    pub hidden_unauthorized: usize,
    pub traffic: Vec<brocade_deployment::user_traffic::UserTrafficPolicyView>,
}
#[derive(Debug, Clone, Serialize)]
pub struct CvmChainOption {
    pub id: String,
    pub app_id: String,
    pub app_name: String,
    pub name: String,
    pub country: Option<String>,
    pub machines: Vec<String>,
    pub authorized: bool,
    pub unavailable_reason: Option<String>,
    pub used_by: Vec<String>,
    pub ipv6: bool,
    pub reachable: Option<bool>,
    pub rtt_ms: Option<i32>,
}

fn ipv6_supported(snapshot: &brocade_core::model::ModelSnapshot, chain: &str) -> bool {
    use brocade_core::model::Action;
    let mut found = false;
    for step in snapshot
        .apps
        .iter()
        .flat_map(|a| &a.steps)
        .filter(|s| s.chain == chain)
    {
        for rule in &step.rules {
            match &rule.action {
                Action::Egress { send_through } => {
                    found = true;
                    if send_through.is_some_and(|ip| ip.is_ipv4())
                        || send_through.is_none()
                            && !snapshot
                                .nodes
                                .iter()
                                .any(|n| n.id == step.node && n.public_ipv6.is_some())
                    {
                        return false;
                    }
                }
                Action::Proxy { .. } | Action::ReuseListener { .. } => return false,
                _ => {}
            }
        }
    }
    found
}

impl PgStore {
    pub async fn cvm_chain_catalog(
        &self,
        actor: &AdminContext,
        selected: Option<&str>,
    ) -> Result<CvmChainCatalog> {
        let workspace = self.cvm_workspace(actor).await?;
        let selected = selected
            .map(|id| {
                workspace
                    .instances
                    .iter()
                    .find(|i| i.desired.id == id)
                    .ok_or_else(|| StoreError::NotFound("CVM".into()))
            })
            .transpose()?;
        let owner = selected.map(|i| CvmOwner {
            tenant_id: i.tenant_id.clone(),
            user_id: i.user_id.clone(),
            name: workspace
                .owners
                .iter()
                .find(|o| o.tenant_id == i.tenant_id && o.user_id == i.user_id)
                .map(|o| o.name.clone())
                .unwrap_or_else(|| i.user_id.clone()),
        });
        let supported = selected.is_some_and(|i| {
            workspace
                .hosts
                .iter()
                .any(|h| h.node_id == i.node_id && h.fresh && h.capabilities.chain_egress)
        });
        let reason = (!supported).then(|| {
            if selected.is_none() {
                "请选择容器".into()
            } else {
                "宿主 Agent 或 Cube 运行栈尚未验证主网卡线路出口".into()
            }
        });
        let snapshot = crate::materialize::load_snapshot(&self.pool, None).await?;
        let mut connection = self.pool.acquire().await?;
        let targets = if let Some(o) = &owner {
            match crate::grant_probe::user_grant_targets(
                &mut connection,
                &snapshot,
                &o.tenant_id,
                &o.user_id,
            )
            .await
            {
                Ok(t) => t,
                Err(StoreError::NotFound(_)) => Vec::new(),
                Err(e) => return Err(e),
            }
        } else {
            Vec::new()
        };
        drop(connection);
        let mut chains = Vec::new();
        let mut hidden = 0;
        for app in &snapshot.apps {
            for chain in &app.chains {
                let authorized = targets.iter().any(|t| t.chain_id == chain.id);
                let executable = targets
                    .iter()
                    .any(|t| t.chain_id == chain.id && t.chain_egress_reason.is_none());
                let unavailable_reason = (!executable)
                    .then(|| {
                        targets
                            .iter()
                            .find(|t| t.chain_id == chain.id)
                            .and_then(|t| t.chain_egress_reason.clone())
                    })
                    .flatten();
                let used_by = workspace
                    .instances
                    .iter()
                    .filter(|i| {
                        i.reserved
                            && i.desired.power != PowerState::Deleted
                            && i.desired
                                .spec
                                .routing
                                .chain_ids()
                                .contains(chain.id.as_str())
                    })
                    .map(|i| i.desired.id.clone())
                    .collect::<Vec<_>>();
                if !authorized && used_by.is_empty() {
                    if actor.can_access_tenant(&chain.tenant)
                        || owner.as_ref().is_some_and(|o| o.tenant_id == chain.tenant)
                    {
                        hidden += 1
                    };
                    continue;
                }
                let probe:Option<(String,Option<i32>)>=sqlx::query_as("SELECT status,ttfb_ms FROM e2e_probes WHERE chain_id=$1 AND probed_at>now()-interval '5 minutes' ORDER BY probed_at DESC LIMIT 1").bind(&chain.id).fetch_optional(&self.pool).await?;
                let machines = app
                    .steps
                    .iter()
                    .filter(|s| s.chain == chain.id)
                    .map(|s| {
                        snapshot
                            .nodes
                            .iter()
                            .find(|n| n.id == s.node)
                            .map(|n| n.name.clone())
                            .unwrap_or_else(|| s.node.clone())
                    })
                    .collect();
                chains.push(CvmChainOption {
                    id: chain.id.clone(),
                    app_id: app.id.clone(),
                    app_name: if app.label.is_empty() {
                        app.id.clone()
                    } else {
                        app.label.clone()
                    },
                    name: chain.name.clone(),
                    country: chain.subscription_country.clone(),
                    machines,
                    authorized,
                    unavailable_reason,
                    used_by,
                    ipv6: ipv6_supported(&snapshot, &chain.id),
                    reachable: probe.as_ref().map(|p| p.0 == "ok"),
                    rtt_ms: probe.and_then(|p| p.1),
                });
            }
        }
        let traffic = if let Some(o) = &owner {
            crate::user_traffic::views(&self.pool, actor, &o.tenant_id, &o.user_id).await?
        } else {
            Vec::new()
        };
        Ok(CvmChainCatalog {
            owner,
            supported,
            reason,
            chains,
            hidden_unauthorized: hidden,
            traffic,
        })
    }
}

pub(super) async fn freeze_clients(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    instances: &[InstanceDesired],
) -> Result<Vec<ChainClientArtifact>> {
    if !instances
        .iter()
        .any(|i| i.power == PowerState::Running && !i.spec.routing.chain_ids().is_empty())
    {
        return Ok(Vec::new());
    }
    let snapshot = crate::materialize::load_snapshot_tx(tx, None).await?;
    let previous: Option<(i64, String)> = sqlx::query_as("SELECT revision_id,payload FROM cvm_revision_nodes WHERE node_id=$1 ORDER BY revision_id DESC LIMIT 1")
        .bind(node).fetch_optional(&mut **tx).await?;
    let previous = previous
        .map(|(r, p)| releases::decode(node, r, &p))
        .transpose()?
        .map(|c| c.chain_clients)
        .unwrap_or_default();
    let mut owners = BTreeMap::new();
    let mut clients = Vec::new();
    let mut marks = BTreeSet::new();
    for instance in instances
        .iter()
        .filter(|i| i.power == PowerState::Running && !i.spec.routing.chain_ids().is_empty())
    {
        let (tenant, user): (String, String) = sqlx::query_as(
            "SELECT tenant_id,user_id FROM cvm_instances WHERE id=$1 AND node_id=$2",
        )
        .bind(&instance.id)
        .bind(node)
        .fetch_one(&mut **tx)
        .await?;
        let key = (tenant.clone(), user.clone());
        if !owners.contains_key(&key) {
            let targets =
                crate::grant_probe::user_grant_targets(&mut *tx, &snapshot, &tenant, &user).await;
            let targets = match targets {
                Ok(t) => t,
                Err(StoreError::NotFound(_)) => Vec::new(),
                Err(e) => return Err(e),
            };
            owners.insert(key.clone(), targets);
        }
        for chain in instance.spec.routing.chain_ids() {
            let client = if let Some(target) = owners[&key]
                .iter()
                .find(|t| t.chain_id == chain && t.chain_egress_reason.is_none())
            {
                let ingress_node = snapshot
                    .apps
                    .iter()
                    .flat_map(|a| &a.ingresses)
                    .find(|i| i.id == target.ingress_id)
                    .ok_or_else(|| invalid("线路入口已不存在"))?
                    .node
                    .as_str();
                brocade_cvm_compile::compile_chain_client(
                    &instance.id,
                    &tenant,
                    &user,
                    ingress_node,
                    &target.target,
                )
                .map_err(invalid)?
            } else if let Some(old) = previous
                .iter()
                .find(|c| c.instance_id == instance.id && c.chain_id == chain)
            {
                // Retain the selector identity and terminal guards, but stop old sessions too.
                // Removing the route here would silently restore the host's default NAT path.
                let mut blocked = old.clone();
                blocked.blocked = true;
                blocked
            } else {
                return Err(StoreError::Forbidden(
                    "容器使用者没有这条线路的有效接入授权".into(),
                ));
            };
            if !marks.insert(client.binding.mark) {
                return Err(StoreError::Conflict(
                    "线路出口执行标识发生冲突，请调整所选线路".into(),
                ));
            }
            clients.push(client);
        }
    }
    clients.sort_by(|a, b| (&a.instance_id, &a.chain_id).cmp(&(&b.instance_id, &b.chain_id)));
    Ok(clients)
}

pub(super) async fn validate_selection(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    routing: &ContainerRoutes,
) -> Result<()> {
    if routing.chain_ids().is_empty() {
        return Ok(());
    }
    let (node, tenant, user): (String, String, String) =
        sqlx::query_as("SELECT node_id,tenant_id,user_id FROM cvm_instances WHERE id=$1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    let caps = host_locked(tx, &node, false).await?;
    if !caps.chain_egress {
        return Err(StoreError::Unsupported(
            "宿主 Agent 或 Cube 运行栈尚不支持主网卡线路出口".into(),
        ));
    }
    let snapshot = crate::materialize::load_snapshot_tx(tx, None).await?;
    let targets = crate::grant_probe::user_grant_targets(&mut *tx, &snapshot, &tenant, &user)
        .await
        .map_err(|e| match e {
            StoreError::NotFound(_) => StoreError::Forbidden("容器使用者没有有效线路授权".into()),
            e => e,
        })?;
    for chain in routing.chain_ids() {
        if !targets.iter().any(|t| t.chain_id == chain) {
            return Err(StoreError::Forbidden("容器使用者未获所选线路授权".into()));
        }
        if !targets
            .iter()
            .any(|t| t.chain_id == chain && t.chain_egress_reason.is_none())
        {
            return Err(StoreError::Unsupported(
                targets
                    .iter()
                    .find(|t| t.chain_id == chain)
                    .and_then(|t| t.chain_egress_reason.clone())
                    .unwrap_or_else(|| "线路入口没有可执行的客户端配置".into()),
            ));
        }
        if routing
            .routes
            .iter()
            .any(|r| r.target.chain_id() == Some(chain) && !r.destination.is_ipv4())
            && !ipv6_supported(&snapshot, chain)
        {
            return Err(StoreError::Unsupported(
                "所选线路的 IPv6 落地尚未确认，不能配置 IPv6 线路出口".into(),
            ));
        }
    }
    Ok(())
}
