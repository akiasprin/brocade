//! Shared host listener conflicts, independent of feature authorization or usage.
use crate::{Result, StoreError};
use brocade_deployment::protocol::ForwardingProtocol;
use sqlx::{Postgres, Transaction};

pub(crate) async fn check_user_listener(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    protocol: ForwardingProtocol,
    port: u16,
) -> Result<()> {
    // All allocation and forwarding writers take this lock before row locks.
    crate::resource_coordination::lock(tx).await?;
    let kinds: Vec<&str> = match protocol {
        ForwardingProtocol::Tcp => vec!["tcp"],
        ForwardingProtocol::Udp => vec!["udp"],
        ForwardingProtocol::TcpUdp => vec!["tcp", "udp"],
    };
    let occupied:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_nic_resources WHERE scope=$1 AND NOT released AND kind=ANY($2::text[]) AND split_part(value,':',array_length(string_to_array(value,':'),1))=$3)")
        .bind(format!("port:{node}")).bind(&kinds).bind(port.to_string()).fetch_one(&mut **tx).await?;
    if occupied {
        return Err(StoreError::Conflict(
            "协议端口由容器网卡或访问入口占用，撤销或修改后需等待清理确认".into(),
        ));
    }
    Ok(())
}

/// Model writers take the network lock before changing listener rows. Rollback
/// and multi-op drafts take it before their first write, in the same order.
pub(crate) async fn check_static_nic_conflicts(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    // Prior-release schema conversion proofs can edit the proxy model before
    // migration 5. There cannot be NIC allocations without their ledger table.
    let enabled: bool =
        sqlx::query_scalar("SELECT to_regclass('public.cvm_nic_resources') IS NOT NULL")
            .fetch_one(&mut **tx)
            .await?;
    if !enabled {
        return Ok(());
    }
    let conflict:Option<(String,String,i32)>=sqlx::query_as("SELECT s.node_id,s.protocol,(substring(r.value from '[0-9]+$'))::integer FROM cvm_nic_resources r JOIN managed_static_listener_ranges s ON r.scope='port:'||s.node_id AND r.kind=s.protocol WHERE NOT r.released AND (substring(r.value from '[0-9]+$'))::integer BETWEEN s.port_start AND s.port_end ORDER BY s.node_id,s.protocol,r.value LIMIT 1").fetch_optional(&mut **tx).await?;
    if let Some((node, protocol, port)) = conflict {
        return Err(StoreError::Conflict(format!(
            "机器 {node} 的 {protocol}/{port} 已分配给容器网卡或访问入口，请先撤销或修改并等待清理"
        )));
    }
    Ok(())
}
#[cfg(feature = "cvm")]
pub(crate) async fn static_ranges(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    protocol: &str,
) -> Result<Vec<(i32, i32)>> {
    sqlx::query_as("SELECT port_start,port_end FROM managed_static_listener_ranges WHERE node_id=$1 AND protocol=$2 ORDER BY port_start,port_end").bind(node).bind(protocol).fetch_all(&mut **tx).await.map_err(Into::into)
}
