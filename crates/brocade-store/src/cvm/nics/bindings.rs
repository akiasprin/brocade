use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNicBindingView {
    pub consumer_fresh: bool,
    pub provider_fresh: bool,
    pub id: String,
    pub interface_id: String,
    pub transport: TransportKind,
    pub generation: u64,
    pub phase: BindingPhase,
    pub consumer: Option<NicBindingReport>,
    pub provider: Option<NicBindingReport>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNicAvailability {
    pub transports: Vec<TransportKind>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNicState {
    pub recent_errors: Vec<CvmNicBindingView>,
    pub usage: BTreeMap<String, CvmNicUsage>,
    pub bindings: Vec<CvmNicBindingView>,
    pub mount_options: BTreeMap<String, CvmNicAvailability>,
    pub interface_options: BTreeMap<String, CvmNicAvailability>,
}

fn phase_text(phase: BindingPhase) -> &'static str {
    match phase {
        BindingPhase::Preparing => "preparing",
        BindingPhase::Active => "active",
        BindingPhase::Standby => "standby",
        BindingPhase::Withdrawing => "withdrawing",
    }
}
fn credential_context(id: &str) -> String {
    format!("cvm-nic-binding:{id}")
}
/// Resolves an automatic listener to the provider and fills a consumer listener's endpoint.
pub(super) async fn resolve_offer(
    tx: &mut Transaction<'_, Postgres>,
    consumer: &str,
    template: &TransportOffer,
) -> Result<TransportOffer> {
    let mut offer = template.clone();
    if offer.listen_side == TransportListenSide::Auto {
        offer.listen_side = TransportListenSide::Provider;
    }
    if offer.listen_side == TransportListenSide::Consumer {
        let row = sqlx::query(
            "SELECT public_ipv4,public_ipv6 FROM nodes WHERE id=$1 AND retired_at IS NULL",
        )
        .bind(consumer)
        .fetch_one(&mut **tx)
        .await?;
        let address: Option<String> = row.try_get(if template.endpoint.is_ipv4() {
            "public_ipv4"
        } else {
            "public_ipv6"
        })?;
        offer.endpoint = address
            .and_then(|address| address.parse().ok())
            .ok_or_else(|| invalid("消费机器缺少所选地址族的承载监听地址"))?;
    }
    Ok(offer)
}
pub(super) async fn binding_value(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<NicBinding> {
    let row = sqlx::query("SELECT target FROM cvm_nic_bindings WHERE id=$1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(serde_json::from_value(row.try_get("target")?)?)
}
pub(super) async fn persist_binding(
    tx: &mut Transaction<'_, Postgres>,
    binding: &NicBinding,
) -> Result<()> {
    sqlx::query("UPDATE cvm_nic_bindings SET generation=$2,phase=$3,target=$4,consumer_report=NULL,provider_report=NULL,consumer_reported_at=NULL,provider_reported_at=NULL WHERE id=$1").bind(&binding.id).bind(binding.generation as i64).bind(phase_text(binding.phase)).bind(serde_json::to_value(binding)?).execute(&mut **tx).await?;
    wake(tx, &[&binding.provider_node, &binding.consumer_node]).await
}
async fn move_phase(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    phase: BindingPhase,
) -> Result<NicBinding> {
    let mut binding = binding_value(tx, id).await?;
    binding.generation += 1;
    binding.phase = phase;
    persist_binding(tx, &binding).await?;
    Ok(binding)
}

/// Picks the first transport port on the listening machine that no ledger claim, user
/// forwarding rule, managed static listener or reported host service already uses.
/// Ports are compared without addresses: a wildcard listener conflicts with every address.
pub(super) async fn transport_port(
    tx: &mut Transaction<'_, Postgres>,
    server: &str,
    offer: &TransportOffer,
    server_caps: &NicNodeCapabilities,
) -> Result<(String, &'static str, u16)> {
    let scope = format!("port:{server}");
    let protocol = if offer.kind == TransportKind::Wireguard {
        "udp"
    } else {
        "tcp"
    };
    let used = allocated_values(tx, &scope, protocol).await?;
    let forwarding:Vec<i32>=sqlx::query_scalar("SELECT listen_port FROM forwarding_rules WHERE node_id=$1 AND deleted_at IS NULL AND enabled AND (protocol=$2 OR protocol='tcp_udp')") .bind(server).bind(protocol).fetch_all(&mut **tx).await?;
    let static_ranges = crate::port_reservations::static_ranges(tx, server, protocol).await?;
    let port = (offer.port_start..=offer.port_end)
        .find(|port| {
            !static_ranges
                .iter()
                .any(|(start, end)| i32::from(*port) >= *start && i32::from(*port) <= *end)
                && !used.iter().any(|value| {
                    value
                        .rsplit_once(':')
                        .is_some_and(|(_, p)| p == port.to_string())
                })
                && !forwarding.contains(&i32::from(*port))
                && !server_caps.host_ports.iter().any(|p| {
                    p.port == *port
                        && matches!(
                            (p.protocol, offer.kind),
                            (L4Protocol::Tcp, TransportKind::Xray)
                                | (L4Protocol::Udp, TransportKind::Wireguard)
                        )
                })
        })
        .ok_or_else(|| StoreError::Conflict("承载端口已分配完毕或被宿主服务占用".into()))?;
    Ok((scope, protocol, port))
}

/// Fresh credentials for one transport: `(consumer, provider, consumer_peer, provider_peer)`.
/// WireGuard gets one key pair per side; Xray shares an identity and pins the
/// listener's certificate.
pub(super) fn transport_credentials(
    kind: TransportKind,
    server_caps: &NicNodeCapabilities,
) -> Result<(String, String, String, String)> {
    Ok(match kind {
        TransportKind::Wireguard => {
            let mut consumer_key = [0; 32];
            let mut provider_key = [0; 32];
            getrandom::fill(&mut consumer_key).map_err(|_| invalid("网络密钥生成失败"))?;
            getrandom::fill(&mut provider_key).map_err(|_| invalid("网络密钥生成失败"))?;
            let consumer_public =
                x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(consumer_key));
            let provider_public =
                x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(provider_key));
            (
                STANDARD.encode(consumer_key),
                STANDARD.encode(provider_key),
                STANDARD.encode(provider_public.as_bytes()),
                STANDARD.encode(consumer_public.as_bytes()),
            )
        }
        TransportKind::Xray => {
            let uuid = crate::generate_uuid_v4()?;
            let mut bytes = [0; 32];
            getrandom::fill(&mut bytes).map_err(|_| invalid("网络凭据生成失败"))?;
            let peer = serde_json::to_string(
                &serde_json::json!({"packet_key":brocade_core::hash::hex_lower(&bytes),"certificate_pem":server_caps.xray_certificate_pem.as_ref().ok_or_else(||invalid("监听机器缺少 Xray 身份证书"))?}),
            )?;
            (uuid.clone(), uuid, peer.clone(), peer)
        }
    })
}

pub(super) async fn create_binding(
    tx: &mut Transaction<'_, Postgres>,
    instance: &str,
    nic: &str,
    consumer: &str,
    allocated: &AllocatedNic,
    offer: &TransportOffer,
    provider: &NicNodeCapabilities,
) -> Result<()> {
    let offer = resolve_offer(tx, consumer, offer).await?;
    let server = if offer.listen_side == TransportListenSide::Consumer {
        consumer
    } else {
        &allocated.provider_node
    };
    let server_caps = if server == allocated.provider_node {
        provider.clone()
    } else {
        capability(tx, server).await?
    };
    let id = new_id("binding")?;
    let (scope, protocol, port) = transport_port(tx, server, &offer, &server_caps).await?;
    let sealed = crate::secrets::seal(
        &credential_context(&id),
        &serde_json::to_string(&transport_credentials(offer.kind, &server_caps)?)?,
    )?;
    let raw: serde_json::Value =
        sqlx::query_scalar("SELECT desired#>'{spec,routing}' FROM cvm_instances WHERE id=$1")
            .bind(instance)
            .fetch_one(&mut **tx)
            .await?;
    let routing: ContainerRoutes = serde_json::from_value(raw)?;
    let binding = NicBinding {
        routing,
        side: BindingSide::Consumer,
        id: id.clone(),
        interface_id: nic.into(),
        instance_id: instance.into(),
        consumer_node: consumer.into(),
        provider_node: allocated.provider_node.clone(),
        generation: 1,
        phase: BindingPhase::Preparing,
        nic: allocated.clone(),
        offer: offer.clone(),
        port,
        credential: String::new(),
        peer_credential: String::new(),
        entry_source_ipv4: super::entries::entry_source(tx, instance, allocated).await?,
    };
    sqlx::query("INSERT INTO cvm_nic_bindings(id,interface_id,instance_id,allocation_id,provider_node,consumer_node,generation,phase,target,sealed) VALUES($1,$2,$3,$4,$5,$6,1,'preparing',$7,$8)") .bind(&id).bind(nic).bind(instance).bind(&allocated.allocation_id).bind(&allocated.provider_node).bind(consumer).bind(serde_json::to_value(&binding)?).bind(sealed).execute(&mut **tx).await?;
    claim(
        tx,
        &allocated.allocation_id,
        Some(&id),
        &scope,
        protocol,
        &format!("{}:{port}", offer.endpoint),
    )
    .await?;
    wake(tx, &[&allocated.provider_node, consumer]).await
}

impl PgStore {
    pub async fn cvm_nic_state(&self, actor: &AdminContext, instance: &str) -> Result<CvmNicState> {
        let container = self.cvm_instance(actor, instance).await?;
        let mut bindings = Vec::new();
        for row in sqlx::query("SELECT id,target,consumer_report,provider_report,last_error,coalesce(consumer_reported_at>now()-interval '150 seconds',false) AS consumer_fresh,coalesce(provider_reported_at>now()-interval '150 seconds',false) AS provider_fresh FROM cvm_nic_bindings WHERE instance_id=$1 AND NOT cleaned ORDER BY created_at,id LIMIT 64").bind(instance).fetch_all(&self.pool).await? {
            let target:NicBinding=serde_json::from_value(row.try_get("target")?)?;
            bindings.push(CvmNicBindingView{consumer_fresh:row.try_get("consumer_fresh")?,provider_fresh:row.try_get("provider_fresh")?,id:target.id,interface_id:target.interface_id,transport:target.nic.transport,generation:target.generation,phase:target.phase,consumer:row.try_get::<Option<serde_json::Value>,_>("consumer_report")?.map(serde_json::from_value).transpose()?,provider:row.try_get::<Option<serde_json::Value>,_>("provider_report")?.map(serde_json::from_value).transpose()?,error:row.try_get("last_error")?});
        }
        let mut nodes = BTreeMap::new();
        for row in sqlx::query("SELECT s.node_id,s.capabilities,s.observed_at>now()-interval '150 seconds' AND n.retired_at IS NULL AS fresh FROM cvm_nic_nodes s JOIN nodes n ON n.id=s.node_id ORDER BY s.node_id LIMIT 2048").fetch_all(&self.pool).await? {
            let caps:NicNodeCapabilities=serde_json::from_value(row.try_get("capabilities")?)?;
            if row.try_get::<bool,_>("fresh")?&&caps.contract==NIC_PROTOCOL_VERSION&&caps.error.is_none(){nodes.insert(row.try_get::<String,_>("node_id")?,caps);}
        }
        let catalog = self.cvm_nic_catalog(actor).await?;
        let mut mount_options = BTreeMap::new();
        for p in catalog.publications {
            let consumer = nodes.get(&container.node_id).filter(|caps| caps.consume);
            let provider = nodes.get(&p.spec.provider_node).filter(|caps| caps.provide);
            let authorized = granted(actor, &container.tenant_id, &container.user_id, &p);
            let transports = if authorized && p.admitting {
                p.spec
                    .transports
                    .iter()
                    .map(|offer| offer.kind)
                    .filter(|kind| {
                        consumer.is_some_and(|caps| caps.transports.contains(kind))
                            && provider.is_some_and(|caps| caps.transports.contains(kind))
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };
            let error = if !authorized {
                Some("容器归属用户尚未获得挂载授权".into())
            } else if !p.admitting {
                Some("已停止新增挂载".into())
            } else if consumer.is_none() {
                Some("宿主尚未上报可用的新网卡执行能力，请检查运行栈和 Agent".into())
            } else if provider.is_none() {
                Some("提供机器离线或网络能力不可用".into())
            } else if transports.is_empty() {
                Some("双方没有共同支持的承载类型".into())
            } else {
                None
            };
            mount_options.insert(p.id, CvmNicAvailability { transports, error });
        }
        let mut interface_options = BTreeMap::new();
        for device in &container.desired.spec.interfaces {
            let Some(nic) = &device.allocated else {
                continue;
            };
            let raw:serde_json::Value=sqlx::query_scalar("SELECT spec FROM cvm_nic_publication_versions WHERE publication_id=$1 AND revision=$2").bind(&nic.publication_id).bind(nic.publication_revision as i64).fetch_one(&self.pool).await?;
            let pinned: NicPublicationSpec = serde_json::from_value(raw)?;
            let consumer = nodes.get(&container.node_id).filter(|caps| caps.consume);
            let provider = nodes.get(&nic.provider_node).filter(|caps| caps.provide);
            let transports = if !nic.blocked {
                pinned
                    .transports
                    .iter()
                    .map(|offer| offer.kind)
                    .filter(|kind| {
                        consumer.is_some_and(|caps| caps.transports.contains(kind))
                            && provider.is_some_and(|caps| caps.transports.contains(kind))
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };
            let error = if nic.blocked {
                Some("网卡授权已撤销，业务已进入阻断收敛".into())
            } else if transports.is_empty() {
                Some("两端当前没有共同可用的承载".into())
            } else {
                None
            };
            interface_options.insert(device.id.clone(), CvmNicAvailability { transports, error });
        }
        let mut recent_errors = Vec::new();
        for row in sqlx::query("SELECT target,last_error FROM cvm_nic_bindings WHERE instance_id=$1 AND cleaned AND last_error IS NOT NULL ORDER BY created_at DESC,id DESC LIMIT 8").bind(instance).fetch_all(&self.pool).await?{
            let target:NicBinding=serde_json::from_value(row.try_get("target")?)?;recent_errors.push(CvmNicBindingView{consumer_fresh:false,provider_fresh:false,id:target.id,interface_id:target.interface_id,transport:target.nic.transport,generation:target.generation,phase:target.phase,consumer:None,provider:None,error:row.try_get("last_error")?});
        }
        if actor.role() == AdminRole::Readonly {
            for error in &mut recent_errors {
                error.error = Some("历史网络执行失败，请管理员核查".into());
            }
            for binding in &mut bindings {
                if binding.error.is_some() {
                    binding.error = Some("网络执行失败，请管理员核查".into());
                }
                for report in [&mut binding.consumer, &mut binding.provider]
                    .into_iter()
                    .flatten()
                {
                    if report.error.is_some() {
                        report.error = Some("网络执行失败，请管理员核查".into());
                    }
                }
            }
        }
        let mut usage = BTreeMap::new();
        for row in sqlx::query("SELECT u.interface_id,u.period::text AS period,u.uploaded,u.downloaded,u.coverage_gap,u.observed_at FROM cvm_nic_monthly_usage u JOIN cvm_interfaces n ON n.id=u.interface_id WHERE n.instance_id=$1 AND u.period=date_trunc('month',now() AT TIME ZONE 'UTC')::date ORDER BY u.interface_id").bind(instance).fetch_all(&self.pool).await?{
            usage.insert(row.try_get("interface_id")?,CvmNicUsage{period:row.try_get("period")?,uploaded_bytes:row.try_get::<i64,_>("uploaded")? as u64,downloaded_bytes:row.try_get::<i64,_>("downloaded")? as u64,coverage_gap:row.try_get("coverage_gap")?,observed_at:row.try_get::<i64,_>("observed_at")? as u64});
        }
        Ok(CvmNicState {
            recent_errors,
            usage,
            bindings,
            mount_options,
            interface_options,
        })
    }
    pub async fn cvm_nic_desired(&self, node: &str) -> Result<NicDesired> {
        self.reset_cvm_nic_period(node).await?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let generation: Option<i64> =
            sqlx::query_scalar("SELECT desired_generation FROM cvm_nic_nodes WHERE node_id=$1")
                .bind(node)
                .fetch_optional(&mut *tx)
                .await?;
        let mut bindings = Vec::new();
        for row in sqlx::query("SELECT target,sealed FROM cvm_nic_bindings WHERE (provider_node=$1 OR consumer_node=$1) AND NOT cleaned ORDER BY id LIMIT 2048").bind(node).fetch_all(&mut *tx).await? {
            let mut binding:NicBinding=serde_json::from_value(row.try_get("target")?)?;
            let (consumer,provider,consumer_peer,provider_peer):(String,String,String,String)=serde_json::from_str(&crate::secrets::open(&credential_context(&binding.id),&row.try_get::<String,_>("sealed")?)?)?;
            if binding.provider_node==node {let mut side=binding.clone();side.side=BindingSide::Provider;side.credential=provider;side.peer_credential=provider_peer;bindings.push(side);}
            if binding.consumer_node==node {binding.side=BindingSide::Consumer;binding.credential=consumer;binding.peer_credential=consumer_peer;bindings.push(binding);}
        }
        if bindings.len() > MAX_NETWORK_BINDINGS * 2 {
            return Err(invalid("承载过渡数量超出执行上限"));
        }
        // Dedicated public addresses stay fenced until their allocation is released, so
        // nothing on the provider can answer for an address it no longer routes.
        let mut fences = Vec::new();
        for row in sqlx::query("SELECT a.interface_id,r.value FROM cvm_nic_resources r JOIN cvm_nic_allocations a ON a.id=r.allocation_id WHERE NOT r.released AND r.scope=$1 AND r.kind IN ('ipv4','ipv6') ORDER BY a.interface_id,r.kind,r.value LIMIT 32769").bind(format!("public:{node}")).fetch_all(&mut *tx).await? {
            let value: String = row.try_get("value")?;
            fences.push(NicResourceFence {
                interface_id: row.try_get("interface_id")?,
                address: value
                    .parse()
                    .map_err(|_| invalid("invalid allocated public address"))?,
                protocol: None,
                port_start: 0,
                port_end: 0,
            });
        }
        if fences.len() > 32768 {
            return Err(invalid("network resource fence capacity exceeded"));
        }
        let mut management_networks = BTreeSet::new();
        for value in sqlx::query_scalar::<_,String>("SELECT host(overlay_addr) FROM nodes WHERE retired_at IS NULL AND overlay AND overlay_addr IS NOT NULL UNION SELECT public_ipv4 FROM nodes WHERE retired_at IS NULL AND public_ipv4 IS NOT NULL UNION SELECT public_ipv6 FROM nodes WHERE retired_at IS NULL AND public_ipv6 IS NOT NULL UNION SELECT report->>'access_address' FROM cvm_instances WHERE node_id=$1 AND reserved AND report->>'access_address' IS NOT NULL LIMIT 2048").bind(node).fetch_all(&mut *tx).await?{if let Ok(ip)=value.parse::<IpAddr>(){management_networks.insert(format!("{ip}/{}",if ip.is_ipv4(){32}else{128}).parse::<IpPrefix>().map_err(invalid)?);}}
        let period:i64=sqlx::query_scalar("SELECT (extract(epoch FROM (date_trunc('month',now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'))*1000)::bigint").fetch_one(&mut *tx).await?;
        let mut budgets = BTreeMap::new();
        for binding in &bindings {
            if binding.nic.limits.monthly_bytes.is_none()
                || budgets.contains_key(&binding.interface_id)
            {
                continue;
            }
            let used:i64=sqlx::query_scalar("SELECT least(9223372036854775807::numeric,coalesce(sum(uploaded::numeric+downloaded::numeric),0))::bigint FROM cvm_nic_monthly_usage WHERE interface_id=$1 AND period=date_trunc('month',now() AT TIME ZONE 'UTC')::date").bind(&binding.interface_id).fetch_one(&mut *tx).await?;
            let cursor:Option<(String,i64,i64,i64)>=sqlx::query_as("SELECT epoch,observed_at,uploaded,downloaded FROM cvm_nic_meter_cursors WHERE interface_id=$1 ORDER BY observed_at DESC LIMIT 1").bind(&binding.interface_id).fetch_optional(&mut *tx).await?;
            budgets.insert(
                binding.interface_id.clone(),
                NicTrafficBudget {
                    period_start: period as u64,
                    used_bytes: used as u64,
                    sample: cursor.map(|(epoch, at, uploaded, downloaded)| NicTrafficSample {
                        epoch,
                        observed_at: at as u64,
                        uploaded_bytes: uploaded as u64,
                        downloaded_bytes: downloaded as u64,
                    }),
                },
            );
        }
        tx.commit().await?;
        Ok(NicDesired {
            node_id: node.into(),
            budgets,
            generation: generation.unwrap_or(0) as u64,
            bindings,
            fences,
            management_networks: management_networks.into_iter().collect(),
        })
    }
    pub async fn change_cvm_nic_transport(
        &self,
        actor: &AdminContext,
        instance_id: &str,
        nic_id: &str,
        request: NicTransportChange,
    ) -> Result<CvmInstance> {
        let digest = request_digest(&(instance_id, nic_id, &request))?;
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
        let mut desired = instance.desired;
        let nic = desired
            .spec
            .interfaces
            .iter_mut()
            .find(|n| n.id == nic_id)
            .and_then(|n| n.allocated.as_mut())
            .ok_or_else(|| StoreError::NotFound("受管网卡".into()))?;
        if nic.blocked {
            return Err(StoreError::Forbidden("网卡授权已撤销".into()));
        }
        let p = publication(&mut tx, &nic.publication_id).await?;
        if p.retired || !granted(actor, &instance.tenant_id, &instance.user_id, &p) {
            return Err(StoreError::Forbidden("网卡授权已撤销".into()));
        }
        let pinned: serde_json::Value = sqlx::query_scalar(
            "SELECT spec FROM cvm_nic_publication_versions WHERE publication_id=$1 AND revision=$2",
        )
        .bind(&p.id)
        .bind(nic.publication_revision as i64)
        .fetch_one(&mut *tx)
        .await?;
        let pinned: NicPublicationSpec = serde_json::from_value(pinned)?;
        let offer = pinned
            .transports
            .iter()
            .find(|t| t.kind == request.transport)
            .ok_or_else(|| invalid("当前网卡规格不支持所选承载，请先升级规格"))?;
        let consumer = capability(&mut tx, &instance.node_id).await?;
        let provider = capability(&mut tx, &nic.provider_node).await?;
        if !consumer.transports.contains(&request.transport)
            || !provider.transports.contains(&request.transport)
        {
            return Err(StoreError::Unsupported("双方没有共同支持的所选承载".into()));
        }
        let transitions:Vec<(String,String)>=sqlx::query_as("SELECT id,phase FROM cvm_nic_bindings WHERE interface_id=$1 AND NOT cleaned AND phase<>'active' ORDER BY id").bind(nic_id).fetch_all(&mut *tx).await?;
        if !transitions.is_empty() {
            let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_nic_bindings WHERE interface_id=$1 AND NOT cleaned AND phase IN ('active','standby'))").bind(nic_id).fetch_one(&mut *tx).await?;
            if active || transitions.len() != 1 || transitions[0].1 != "preparing" {
                return Err(StoreError::Conflict("请先完成当前承载切换或清理".into()));
            }
            // Initial activation can be replaced without deleting the NIC or
            // releasing its allocation; its old transport still needs cleanup.
            move_phase(&mut tx, &transitions[0].0, BindingPhase::Withdrawing).await?;
        }
        if nic.transport == request.transport && !request.rebuild {
            return Err(invalid("网卡已使用所选承载；需要更新连接身份时选择重建"));
        }
        nic.transport = request.transport;
        nic.transport_generation += 1;
        nic.mtu = offer.mtu;
        create_binding(
            &mut tx,
            instance_id,
            nic_id,
            &instance.node_id,
            nic,
            offer,
            &provider,
        )
        .await?;
        sqlx::query("UPDATE cvm_nic_allocations SET spec=$2 WHERE id=$1")
            .bind(&nic.allocation_id)
            .bind(serde_json::to_value(&nic)?)
            .execute(&mut *tx)
            .await?;
        save_network(
            &mut tx,
            actor,
            &request.request_id,
            &digest,
            if request.rebuild {
                "rebuild_transport"
            } else {
                "change_transport"
            },
            desired,
        )
        .await?;
        tx.commit().await?;
        self.cvm_instance(actor, instance_id).await
    }
    pub async fn cvm_record_nic_report(&self, node: &str, report: NicNodeReport) -> Result<bool> {
        if report.bindings.len() > MAX_NETWORK_BINDINGS * 2 {
            return Err(invalid("网卡回报超出数量上限"));
        }
        let mut tx = self.pool.begin().await?;
        lock(&mut tx).await?;
        let mut seen = BTreeSet::new();
        let mut accepted = false;
        for evidence in &report.bindings {
            if !seen.insert((&evidence.id, matches!(evidence.side, BindingSide::Provider)))
                || evidence.error.as_ref().is_some_and(|e| e.len() > 500)
                || evidence.local_block.as_ref().is_some_and(|e| e.len() > 500)
            {
                return Err(invalid("无效或重复的网卡回报"));
            }
            let Some(row)=sqlx::query("SELECT target,consumer_report,provider_report,cleaned FROM cvm_nic_bindings WHERE id=$1 AND (provider_node=$2 OR consumer_node=$2) FOR UPDATE").bind(&evidence.id).bind(node).fetch_optional(&mut *tx).await? else{return Err(StoreError::Forbidden("回报不属于该执行机器".into()))};
            let target: NicBinding = serde_json::from_value(row.try_get("target")?)?;
            if match evidence.side {
                BindingSide::Consumer => target.consumer_node != node,
                BindingSide::Provider => target.provider_node != node,
            } {
                return Err(StoreError::Forbidden("回报角色不属于该机器".into()));
            }
            if row.try_get::<bool, _>("cleaned")? || evidence.generation < target.generation {
                continue;
            }
            if evidence.generation != target.generation || evidence.phase != target.phase {
                return Err(invalid("回报不匹配当前承载版本"));
            }
            metering::record(&mut tx, &target, evidence).await?;
            let current: i64 =
                sqlx::query_scalar("SELECT generation FROM cvm_nic_bindings WHERE id=$1")
                    .bind(&target.id)
                    .fetch_one(&mut *tx)
                    .await?;
            if current as u64 != target.generation {
                accepted = true;
                continue;
            }
            let stamp = if evidence.side == BindingSide::Provider {
                "provider_reported_at"
            } else {
                "consumer_reported_at"
            };
            let column = if evidence.side == BindingSide::Provider {
                "provider_report"
            } else {
                "consumer_report"
            };
            sqlx::query(&format!("UPDATE cvm_nic_bindings SET {column}=$2,{stamp}=now(),last_error=CASE WHEN phase='withdrawing' THEN coalesce(last_error,$3) ELSE $3 END WHERE id=$1")).bind(&target.id).bind(serde_json::to_value(evidence)?).bind(&evidence.error).execute(&mut *tx).await?;
            accepted = true;
            if evidence.error.is_some()
                && matches!(target.phase, BindingPhase::Preparing | BindingPhase::Active)
            {
                restore_previous(&mut tx, &target).await?;
                continue;
            }
            let other = if evidence.side == BindingSide::Provider {
                row.try_get::<Option<serde_json::Value>, _>("consumer_report")?
            } else {
                row.try_get::<Option<serde_json::Value>, _>("provider_report")?
            };
            let other: Option<NicBindingReport> = other.map(serde_json::from_value).transpose()?;
            let both = evidence.configured
                && evidence.error.is_none()
                && other.as_ref().is_some_and(|r| {
                    r.generation == target.generation
                        && r.phase == target.phase
                        && r.configured
                        && r.error.is_none()
                        && (target.phase == BindingPhase::Withdrawing
                            || (evidence.reachable == Some(true) && r.reachable == Some(true)))
                });
            if !both {
                continue;
            }
            match target.phase {
                BindingPhase::Preparing => {
                    let old:Vec<String>=sqlx::query_scalar("SELECT id FROM cvm_nic_bindings WHERE interface_id=$1 AND id<>$2 AND phase='active' AND NOT cleaned").bind(&target.interface_id).bind(&target.id).fetch_all(&mut *tx).await?;
                    for old in old {
                        move_phase(&mut tx, &old, BindingPhase::Standby).await?;
                    }
                    move_phase(&mut tx, &target.id, BindingPhase::Active).await?;
                }
                BindingPhase::Active => {
                    let old:Vec<String>=sqlx::query_scalar("SELECT id FROM cvm_nic_bindings WHERE interface_id=$1 AND id<>$2 AND phase='standby' AND NOT cleaned").bind(&target.interface_id).bind(&target.id).fetch_all(&mut *tx).await?;
                    for old in old {
                        move_phase(&mut tx, &old, BindingPhase::Withdrawing).await?;
                    }
                }
                BindingPhase::Withdrawing => {
                    sqlx::query("UPDATE cvm_nic_bindings SET cleaned=true WHERE id=$1")
                        .bind(&target.id)
                        .execute(&mut *tx)
                        .await?;
                    sqlx::query("UPDATE cvm_nic_resources SET released=true WHERE binding_id=$1")
                        .bind(&target.id)
                        .execute(&mut *tx)
                        .await?;
                    release_confirmed(&mut tx, &target.instance_id).await?;
                    wake(&mut tx, &[&target.provider_node, &target.consumer_node]).await?;
                }
                BindingPhase::Standby => {}
            }
        }
        sqlx::query("UPDATE cvm_nic_nodes SET report=$2 WHERE node_id=$1")
            .bind(node)
            .bind(serde_json::to_value(&report)?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(accepted)
    }
}

async fn restore_previous(tx: &mut Transaction<'_, Postgres>, failed: &NicBinding) -> Result<()> {
    let previous:Option<String>=sqlx::query_scalar("SELECT id FROM cvm_nic_bindings WHERE interface_id=$1 AND id<>$2 AND phase IN ('active','standby') AND NOT cleaned ORDER BY created_at DESC LIMIT 1").bind(&failed.interface_id).bind(&failed.id).fetch_optional(&mut **tx).await?;
    let Some(previous) = previous else {
        return Ok(());
    };
    let old = binding_value(tx, &previous).await?;
    if old.phase == BindingPhase::Standby {
        move_phase(tx, &previous, BindingPhase::Active).await?;
    }
    move_phase(tx, &failed.id, BindingPhase::Withdrawing).await?;
    let row = sqlx::query(&format!("{INSTANCE_SELECT} WHERE id=$1 FOR UPDATE"))
        .bind(&failed.instance_id)
        .fetch_one(&mut **tx)
        .await?;
    let mut desired = from_row(&row)?.desired;
    if let Some(nic) = desired
        .spec
        .interfaces
        .iter_mut()
        .find(|n| n.id == failed.interface_id)
        .and_then(|n| n.allocated.as_mut())
    {
        let blocked = nic.blocked;
        let quota = nic.quota_exceeded;
        let epoch = nic.transport_generation + 1;
        *nic = old.nic.clone();
        nic.blocked = blocked;
        nic.quota_exceeded = quota;
        nic.transport_generation = epoch;
        super::versions::restore_allocation_ownership(tx, &failed.interface_id, nic).await?;
        sqlx::query("UPDATE cvm_nic_allocations SET spec=$2 WHERE id=$1")
            .bind(&nic.allocation_id)
            .bind(serde_json::to_value(&nic)?)
            .execute(&mut **tx)
            .await?;
        system_network_version(tx, &mut desired, "transport_recovery").await?;
    }
    Ok(())
}
pub(super) async fn sync_routes(
    tx: &mut Transaction<'_, Postgres>,
    desired: &InstanceDesired,
) -> Result<()> {
    let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM cvm_nic_bindings WHERE instance_id=$1 AND NOT cleaned AND phase<>'withdrawing'").bind(&desired.id).fetch_all(&mut **tx).await?;
    for id in ids {
        let mut binding = binding_value(tx, &id).await?;
        if binding.routing != desired.spec.routing {
            binding.routing = desired.spec.routing.clone();
            binding.generation += 1;
            persist_binding(tx, &binding).await?;
        }
    }
    Ok(())
}
pub(super) async fn set_binding_blocked(
    tx: &mut Transaction<'_, Postgres>,
    nic: &str,
    blocked: bool,
) -> Result<()> {
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM cvm_nic_bindings WHERE interface_id=$1 AND NOT cleaned")
            .bind(nic)
            .fetch_all(&mut **tx)
            .await?;
    for id in ids {
        let mut binding = binding_value(tx, &id).await?;
        binding.generation += 1;
        binding.nic.blocked = blocked;
        persist_binding(tx, &binding).await?;
    }
    Ok(())
}
pub(crate) async fn withdraw(
    tx: &mut Transaction<'_, Postgres>,
    instance: &str,
    nic: Option<&str>,
) -> Result<()> {
    let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM cvm_nic_bindings WHERE instance_id=$1 AND ($2::text IS NULL OR interface_id=$2) AND phase<>'withdrawing' AND NOT cleaned").bind(instance).bind(nic).fetch_all(&mut **tx).await?;
    for id in ids {
        move_phase(tx, &id, BindingPhase::Withdrawing).await?;
    }
    Ok(())
}
pub(crate) async fn release_confirmed(
    tx: &mut Transaction<'_, Postgres>,
    instance: &str,
) -> Result<()> {
    let allocations:Vec<String>=sqlx::query_scalar("SELECT a.id FROM cvm_nic_allocations a JOIN cvm_interfaces n ON n.id=a.interface_id JOIN cvm_instances i ON i.id=a.instance_id WHERE a.instance_id=$1 AND NOT a.released AND NOT EXISTS(SELECT 1 FROM cvm_nic_bindings b WHERE b.allocation_id=a.id AND NOT b.cleaned) AND ((NOT n.desired_attached AND n.detached_confirmed) OR (i.report->>'status'='applied' AND i.report->>'generation'=i.generation::text AND NOT EXISTS(SELECT 1 FROM jsonb_array_elements(i.desired#>'{spec,interfaces}') device WHERE device->>'id'=n.id AND device#>>'{allocated,allocation_id}'=a.id)))").bind(instance).fetch_all(&mut **tx).await?;
    for allocation in allocations {
        let endpoints: (String, String) = sqlx::query_as(
            "SELECT provider_node,consumer_node FROM cvm_nic_allocations WHERE id=$1",
        )
        .bind(&allocation)
        .fetch_one(&mut **tx)
        .await?;
        wake(tx, &[&endpoints.0, &endpoints.1]).await?;
        sqlx::query("UPDATE cvm_nic_resources SET released=true WHERE allocation_id=$1")
            .bind(&allocation)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE cvm_nic_allocations SET released=true WHERE id=$1")
            .bind(&allocation)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

pub(crate) async fn retire_provider(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<()> {
    lock(tx).await?;
    sqlx::query("UPDATE cvm_nic_publications SET admitting=false,deleted=true,updated_at=now() WHERE provider_node=$1").bind(node).execute(&mut **tx).await?;
    sqlx::query(
        "UPDATE cvm_nic_allocations SET retired=true WHERE provider_node=$1 AND NOT released",
    )
    .bind(node)
    .execute(&mut **tx)
    .await?;
    let rows=sqlx::query(&format!("{INSTANCE_SELECT} WHERE id IN(SELECT instance_id FROM cvm_nic_allocations WHERE provider_node=$1 AND NOT released) ORDER BY id FOR UPDATE")).bind(node).fetch_all(&mut **tx).await?;
    for row in rows {
        let mut desired = from_row(&row)?.desired;
        let removed: Vec<String> = desired
            .spec
            .interfaces
            .iter()
            .filter(|n| {
                n.allocated
                    .as_ref()
                    .is_some_and(|a| a.provider_node == node)
            })
            .map(|n| n.id.clone())
            .collect();
        for nic in &removed {
            desired.spec.routing.remove(nic);
            sqlx::query("UPDATE cvm_interfaces SET desired_attached=false WHERE id=$1")
                .bind(nic)
                .execute(&mut **tx)
                .await?;
            withdraw(tx, &desired.id, Some(nic)).await?;
        }
        if removed.is_empty() {
            continue;
        }
        desired.spec.interfaces.retain(|n| !removed.contains(&n.id));
        desired.detached_interfaces.extend(removed);
        desired.detached_interfaces.sort();
        desired.detached_interfaces.dedup();
        desired.spec.routing.revision += 1;
        sqlx::query("INSERT INTO cvm_nic_route_versions(instance_id,revision,routes,actor_id) VALUES($1,$2,$3,'system-network-retirement')").bind(&desired.id).bind(desired.spec.routing.revision as i64).bind(serde_json::to_value(&desired.spec.routing)?).execute(&mut **tx).await?;
        system_network_version(tx, &mut desired, "network_provider_retired").await?;
    }
    Ok(())
}
