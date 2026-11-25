//! Address reservations outlive mutable edits until the serving node acknowledges replacement.
use super::*;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Protection {
    pub address: Ipv4Addr,
    pub ports: EntryReservedPorts,
}
fn protections(specs: impl IntoIterator<Item = EntryPublicationSpec>) -> Vec<Protection> {
    specs
        .into_iter()
        .flat_map(|s| s.addresses)
        .filter(|o| !o.reserved_ports.is_empty())
        .map(|o| Protection {
            address: o.address,
            ports: o.reserved_ports,
        })
        .collect()
}
pub(super) async fn serving(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
) -> Result<Vec<Protection>> {
    if let Some(config) = crate::cvm::releases::serving_tx(tx, node).await? {
        let specs = config
            .entry_publications
            .into_iter()
            .filter(|v| v["deleted"] != true)
            .map(|v| serde_json::from_value(v["spec"].clone()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(protections(specs))
    } else {
        current(tx, node).await
    }
}
async fn current(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<Vec<Protection>> {
    let specs: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT spec FROM cvm_entry_publications WHERE provider_node=$1 AND NOT deleted",
    )
    .bind(node)
    .fetch_all(&mut **tx)
    .await?;
    Ok(protections(
        specs
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<Vec<_>, _>>()?,
    ))
}
pub(crate) async fn protected(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    address: Option<Ipv4Addr>,
    protocol: L4Protocol,
) -> Result<Vec<EntryPortRange>> {
    let mut all = current(tx, node).await?;
    all.extend(serving(tx, node).await?);
    let previous: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT report->'protected_entry_ports' FROM cvm_entry_nodes WHERE node_id=$1",
    )
    .bind(node)
    .fetch_optional(&mut **tx)
    .await?
    .flatten();
    if let Some(previous) = previous {
        all.extend(serde_json::from_value::<Vec<Protection>>(previous)?);
    }
    merge_entry_ranges(
        &all.into_iter()
            .filter(|p| address.is_none_or(|a| a == p.address))
            .flat_map(|p| p.ports.for_protocol(protocol).to_vec())
            .collect::<Vec<_>>(),
    )
    .map_err(invalid)
}
