//! Machine network logs can include shared transports, so access is system-admin only.
//! Existing node report JSON retains optional diagnostics without changing the schema.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNetworkLogDomain {
    pub domain: String,
    pub desired_generation: u64,
    pub reported_generation: Option<u64>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNetworkNodeLogs {
    pub node_id: String,
    pub name: String,
    pub fresh: bool,
    pub domains: Vec<CvmNetworkLogDomain>,
    pub diagnostics: Option<NetworkDiagnostics>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNetworkLogs {
    pub nodes: Vec<CvmNetworkNodeLogs>,
}
impl PgStore {
    pub async fn cvm_network_logs(
        &self,
        actor: &AdminContext,
        nodes: &[String],
    ) -> Result<CvmNetworkLogs> {
        if !actor.is_system_admin() {
            return Err(StoreError::Forbidden(
                "只有系统管理员可以查看机器网络日志".into(),
            ));
        }
        if nodes.is_empty() || nodes.len() > 8 || nodes.iter().any(|id| !valid_id(id)) {
            return Err(invalid("选择 1 至 8 台有效机器查看日志"));
        }
        let unique: BTreeSet<_> = nodes.iter().collect();
        let rows = sqlx::query("SELECT n.id,n.name,e.desired_generation AS entry_generation,e.report AS entry_report,c.desired_generation AS nic_generation,c.report AS nic_report FROM nodes n LEFT JOIN cvm_entry_nodes e ON e.node_id=n.id LEFT JOIN cvm_nic_nodes c ON c.node_id=n.id WHERE n.id=ANY($1::text[]) ORDER BY n.id")
            .bind(nodes).fetch_all(&self.pool).await?;
        if rows.len() != unique.len() {
            return Err(StoreError::NotFound("机器不存在".into()));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut out = Vec::new();
        for row in rows {
            let entry: Option<EntryNodeReport> = row
                .try_get::<Option<serde_json::Value>, _>("entry_report")?
                .map(serde_json::from_value)
                .transpose()?;
            let nic: Option<NicNodeReport> = row
                .try_get::<Option<serde_json::Value>, _>("nic_report")?
                .map(serde_json::from_value)
                .transpose()?;
            let mut domains = Vec::new();
            if let Some(generation) = row.try_get::<Option<i64>, _>("entry_generation")? {
                domains.push(CvmNetworkLogDomain {
                    domain: "entry".into(),
                    desired_generation: generation as u64,
                    reported_generation: entry.as_ref().map(|r| r.generation),
                    error: entry.as_ref().and_then(|r| r.error.clone()),
                });
            }
            if let Some(generation) = row.try_get::<Option<i64>, _>("nic_generation")? {
                domains.push(CvmNetworkLogDomain {
                    domain: "egress".into(),
                    desired_generation: generation as u64,
                    reported_generation: nic.as_ref().map(|r| r.generation),
                    error: None,
                });
            }
            let diagnostics = [
                entry.and_then(|r| r.diagnostics),
                nic.and_then(|r| r.diagnostics),
            ]
            .into_iter()
            .flatten()
            .max_by_key(|d| (d.collected_at, d.events.len()));
            let fresh = diagnostics.as_ref().is_some_and(|d| {
                d.collected_at <= now.saturating_add(30) && now.saturating_sub(d.collected_at) < 150
            });
            out.push(CvmNetworkNodeLogs {
                node_id: row.try_get("id")?,
                name: row.try_get("name")?,
                fresh,
                domains,
                diagnostics,
            });
        }
        Ok(CvmNetworkLogs { nodes: out })
    }
}
