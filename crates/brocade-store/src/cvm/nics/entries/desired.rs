//! Per-node entry desired state, node receipts, cleanup and the status shown to users.
use super::*;

/// Joined facts for an entry, read in the caller's snapshot.
const FACTS_SELECT: &str = "SELECT e.id,e.publication_id,e.provider_node,e.instance_id,e.address,e.mode,e.revision,e.released,i.node_id AS host,i.tenant_id,i.user_id,i.reserved,i.report->>'power' AS power,i.report->>'access_address' AS target,p.spec,(SELECT t.id FROM cvm_entry_tunnels t WHERE t.entry_node=e.provider_node AND t.target_node=i.node_id AND NOT t.withdrawn) AS tunnel FROM cvm_entries e JOIN cvm_instances i ON i.id=e.instance_id JOIN cvm_entry_publications p ON p.id=e.publication_id";

struct Facts {
    entry: StoredEntry,
    granted: bool,
    /// The container's host-side primary NIC address while it runs. Cube reports an
    /// IPv4 delivery address; anything else cannot receive an entry.
    target: Option<Ipv4Addr>,
    visible_address: Ipv4Addr,
    tunnel: Option<String>,
}
impl Facts {
    fn remote(&self) -> bool {
        self.entry.host != self.entry.provider_node
    }
}
fn facts(row: &sqlx::postgres::PgRow) -> Result<Facts> {
    let entry = stored(row)?;
    let spec: EntryPublicationSpec = serde_json::from_value(row.try_get("spec")?)?;
    let granted = spec
        .grants
        .iter()
        .any(|g| g.tenant_id == entry.tenant_id && g.user_id == entry.user_id);
    let running = row.try_get::<bool, _>("reserved")?
        && row.try_get::<Option<String>, _>("power")?.as_deref() == Some("running");
    let target = if running {
        row.try_get::<Option<String>, _>("target")?
            .and_then(|value| value.parse::<Ipv4Addr>().ok())
    } else {
        None
    };
    let visible_address = spec
        .offer(entry.address)
        .map_or(entry.address, EntryAddressOffer::visible_address);
    Ok(Facts {
        entry,
        granted,
        target,
        visible_address,
        tunnel: row.try_get("tunnel")?,
    })
}

struct Receipt {
    desired: u64,
    applied: u64,
    /// Reported within the freshness window; a node that never reported is pending, not offline.
    fresh: bool,
    reported: bool,
    report: Option<EntryNodeReport>,
}
async fn receipts(
    tx: &mut Transaction<'_, Postgres>,
    nodes: Vec<String>,
) -> Result<BTreeMap<String, Receipt>> {
    let mut out = BTreeMap::new();
    for row in sqlx::query("SELECT node_id,desired_generation,applied_generation,report,coalesce(reported_at>now()-interval '150 seconds',false) AS fresh,reported_at IS NOT NULL AS reported FROM cvm_entry_nodes WHERE node_id=ANY($1::text[])")
        .bind(&nodes)
        .fetch_all(&mut **tx)
        .await?
    {
        out.insert(
            row.try_get("node_id")?,
            Receipt {
                desired: row.try_get::<i64, _>("desired_generation")? as u64,
                applied: row.try_get::<i64, _>("applied_generation")? as u64,
                fresh: row.try_get("fresh")?,
                reported: row.try_get("reported")?,
                report: row
                    .try_get::<Option<serde_json::Value>, _>("report")?
                    .map(serde_json::from_value)
                    .transpose()?,
            },
        );
    }
    Ok(out)
}

#[derive(Clone, Copy)]
enum Item {
    Rule,
    Delivery,
}
/// One node's evidence for an entry item and, when remote, its tunnel side.
fn evidence(
    receipt: Option<&Receipt>,
    item: Item,
    id: &str,
    tunnel: Option<&str>,
) -> std::result::Result<(), (CvmEntryState, Option<String>)> {
    let Some(receipt) = receipt else {
        return Err((CvmEntryState::Pending, None));
    };
    if !receipt.reported {
        return Err((CvmEntryState::Pending, None));
    }
    if !receipt.fresh {
        return Err((CvmEntryState::Offline, None));
    }
    let Some(report) = receipt
        .report
        .as_ref()
        .filter(|_| receipt.applied >= receipt.desired)
    else {
        return Err((CvmEntryState::Pending, None));
    };
    if let Some(error) = &report.error {
        return Err((CvmEntryState::Failed, Some(error.clone())));
    }
    let items = match item {
        Item::Rule => &report.rules,
        Item::Delivery => &report.deliveries,
    };
    match items.iter().find(|r| r.id == id) {
        Some(r) if r.error.is_some() => return Err((CvmEntryState::Failed, r.error.clone())),
        Some(r) if r.configured => {}
        _ => return Err((CvmEntryState::Pending, None)),
    }
    if let Some(tunnel) = tunnel {
        match report.tunnels.iter().find(|t| t.id == tunnel) {
            Some(t) if t.error.is_some() => return Err((CvmEntryState::Failed, t.error.clone())),
            Some(t) if t.connected => {}
            _ => return Err((CvmEntryState::Pending, None)),
        }
    }
    Ok(())
}
fn state(facts: &Facts, receipts: &BTreeMap<String, Receipt>) -> (CvmEntryState, Option<String>) {
    if facts.entry.released {
        return (CvmEntryState::Releasing, None);
    }
    if !facts.granted {
        return (CvmEntryState::Unauthorized, None);
    }
    if facts.target.is_none() {
        return (CvmEntryState::NoTarget, None);
    }
    let tunnel = if facts.remote() {
        match facts.tunnel.as_deref() {
            Some(tunnel) => Some(tunnel),
            None => return (CvmEntryState::Failed, Some("入口通道缺失".into())),
        }
    } else {
        None
    };
    let checks = [
        (facts.entry.provider_node.as_str(), Item::Rule),
        (facts.entry.host.as_str(), Item::Delivery),
    ];
    for (node, item) in checks.into_iter().take(if facts.remote() { 2 } else { 1 }) {
        if let Err(state) = evidence(receipts.get(node), item, &facts.entry.id, tunnel) {
            return state;
        }
    }
    (CvmEntryState::Active, None)
}

async fn render(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    rows: Vec<sqlx::postgres::PgRow>,
) -> Result<Vec<CvmEntryView>> {
    let all = rows.iter().map(facts).collect::<Result<Vec<_>>>()?;
    let nodes: BTreeSet<String> = all
        .iter()
        .flat_map(|f| [f.entry.provider_node.clone(), f.entry.host.clone()])
        .collect();
    let receipts = receipts(tx, nodes.into_iter().collect()).await?;
    let mut out = Vec::with_capacity(all.len());
    for f in all {
        let (state, mut error) = state(&f, &receipts);
        let readonly = actor.role() == AdminRole::Readonly;
        if readonly && error.is_some() {
            error = Some("网络执行失败，请管理员核查".into());
        }
        let remote = f.remote();
        let mask = |ip: Ipv4Addr| if readonly { Ipv4Addr::UNSPECIFIED } else { ip };
        out.push(CvmEntryView {
            revision: f.entry.revision,
            address: mask(f.entry.address),
            visible_address: mask(f.visible_address),
            id: f.entry.id,
            publication_id: f.entry.publication_id,
            provider_node: f.entry.provider_node,
            instance_id: f.entry.instance_id,
            mode: f.entry.mode,
            remote,
            state,
            error,
        });
    }
    Ok(out)
}

/// Entries visible to the caller that still hold resources, optionally for one container.
pub(super) async fn views(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    instance: Option<&str>,
) -> Result<Vec<CvmEntryView>> {
    let (tenant, pattern, user) = read_scope(actor)?;
    let rows = sqlx::query(&format!("{FACTS_SELECT} WHERE NOT e.cleaned AND ($4::text IS NULL OR e.instance_id=$4) AND {READ_SCOPE} ORDER BY e.created_at,e.id LIMIT 4097"))
        .bind(&tenant)
        .bind(&pattern)
        .bind(&user)
        .bind(instance)
        .fetch_all(&mut **tx)
        .await?;
    if rows.len() > 4096 {
        return Err(invalid("入口数量超出显示上限"));
    }
    render(tx, actor, rows).await
}
pub(super) async fn views_by_id(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    id: &str,
) -> Result<Option<CvmEntryView>> {
    let rows = sqlx::query(&format!("{FACTS_SELECT} WHERE e.id=$1"))
        .bind(id)
        .fetch_all(&mut **tx)
        .await?;
    Ok(render(tx, actor, rows).await?.pop())
}

/// Releases claims of entries and tunnels whose cleanup the nodes have proven.
pub(super) async fn cleanup(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<()> {
    let entries: Vec<String> = sqlx::query_scalar("UPDATE cvm_entries e SET cleaned=true FROM cvm_entry_nodes n WHERE n.node_id=e.provider_node AND e.provider_node=$1 AND e.released AND NOT e.cleaned AND e.cleanup_generation<=n.applied_generation RETURNING e.id")
        .bind(node)
        .fetch_all(&mut **tx)
        .await?;
    sqlx::query("UPDATE cvm_nic_resources SET released=true WHERE entry_id=ANY($1::text[]) AND NOT released")
        .bind(&entries)
        .execute(&mut **tx)
        .await?;
    let tunnels: Vec<String> = sqlx::query_scalar("UPDATE cvm_entry_tunnels t SET cleaned=true FROM cvm_entry_nodes a,cvm_entry_nodes b WHERE a.node_id=t.entry_node AND b.node_id=t.target_node AND (t.entry_node=$1 OR t.target_node=$1) AND t.withdrawn AND NOT t.cleaned AND a.applied_generation>=t.entry_cleanup_generation AND b.applied_generation>=t.target_cleanup_generation RETURNING t.id")
        .bind(node)
        .fetch_all(&mut **tx)
        .await?;
    sqlx::query("UPDATE cvm_nic_resources SET released=true WHERE tunnel_id=ANY($1::text[]) AND NOT released")
        .bind(&tunnels)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

impl PgStore {
    pub async fn cvm_entry_desired(&self, node: &str) -> Result<EntryDesired> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let generation: Option<i64> =
            sqlx::query_scalar("SELECT desired_generation FROM cvm_entry_nodes WHERE node_id=$1")
                .bind(node)
                .fetch_optional(&mut *tx)
                .await?;
        let tunnels = tunnels::desired(&mut tx, node).await?;
        let limit = MAX_ENTRIES_PER_NODE as i64 + 1;
        let rules = sqlx::query(&format!(
            "{FACTS_SELECT} WHERE e.provider_node=$1 AND NOT e.released ORDER BY e.id LIMIT $2"
        ))
        .bind(node)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        let deliveries = sqlx::query(&format!("{FACTS_SELECT} WHERE i.node_id=$1 AND e.provider_node<>$1 AND NOT e.released ORDER BY e.id LIMIT $2"))
            .bind(node)
            .bind(limit)
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        let mut desired = EntryDesired {
            node_id: node.into(),
            generation: generation.unwrap_or(0) as u64,
            tunnels,
            ..Default::default()
        };
        for row in &rules {
            let f = facts(row)?;
            let tunnel = if f.remote() { f.tunnel.clone() } else { None };
            // Without an authorized, running target (or its tunnel) the entry machine
            // still owns the address and ports but drops the traffic.
            let deliverable = f.granted && (!f.remote() || tunnel.is_some());
            desired.rules.push(EntryRule {
                id: f.entry.id,
                address: f.entry.address,
                mode: f.entry.mode,
                target: f.target.filter(|_| deliverable),
                tunnel,
            });
        }
        for row in &deliveries {
            let f = facts(row)?;
            let (true, Some(target), Some(tunnel)) = (f.granted, f.target, f.tunnel) else {
                continue;
            };
            desired.deliveries.push(EntryDelivery {
                id: f.entry.id,
                instance_id: f.entry.instance_id,
                mode: f.entry.mode,
                target,
                tunnel,
            });
        }
        desired.validate().map_err(invalid)?;
        Ok(desired)
    }

    /// Records a node's entry receipt. A report without a global error proves that the
    /// node converged to its generation, including the removal of everything withdrawn.
    pub async fn cvm_record_entry_report(
        &self,
        node: &str,
        report: EntryNodeReport,
    ) -> Result<bool> {
        report.validate().map_err(invalid)?;
        let mut tx = self.pool.begin().await?;
        lock(&mut tx).await?;
        let row: Option<(i64, i64)> = sqlx::query_as("SELECT desired_generation,applied_generation FROM cvm_entry_nodes WHERE node_id=$1 FOR UPDATE")
            .bind(node)
            .fetch_optional(&mut *tx)
            .await?;
        let Some((desired, applied)) = row else {
            return Ok(false);
        };
        if report.generation > desired as u64 {
            return Err(invalid("入口回报版本超出期望状态"));
        }
        if report.generation < applied as u64 {
            return Ok(false);
        }
        sqlx::query("UPDATE cvm_entry_nodes SET report=$2,reported_at=now(),applied_generation=CASE WHEN $3 THEN greatest(applied_generation,$4) ELSE applied_generation END WHERE node_id=$1")
            .bind(node)
            .bind(serde_json::to_value(&report)?)
            .bind(report.error.is_none())
            .bind(report.generation as i64)
            .execute(&mut *tx)
            .await?;
        if report.error.is_none() {
            cleanup(&mut tx, node).await?;
        }
        tx.commit().await?;
        Ok(true)
    }
}
