use super::*;

pub(super) async fn host_locked(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    recovery: bool,
) -> Result<Capabilities> {
    let row=sqlx::query("SELECT h.capabilities,h.observed_at>now()-interval '150 seconds' AS fresh,h.error,n.retired_at IS NULL AS active FROM cvm_hosts h JOIN nodes n ON n.id=h.node_id LEFT JOIN cvm_terminal_policies p ON p.node_id=n.id WHERE h.node_id=$1 FOR UPDATE OF h,n").bind(node).fetch_optional(&mut **tx).await?.ok_or_else(||invalid("机器尚未上报 CVM 执行能力"))?;
    if !recovery
        && (!row.try_get::<bool, _>("fresh")?
            || !row.try_get::<bool, _>("active")?
            || row.try_get::<Option<String>, _>("error")?.is_some())
    {
        return Err(invalid("机器能力已过期或机器已退役"));
    }
    if !recovery {
        let enabled: bool = sqlx::query_scalar(
            "SELECT coalesce((SELECT enabled FROM cvm_host_policies WHERE node_id=$1),true)",
        )
        .bind(node)
        .fetch_one(&mut **tx)
        .await?;
        if !enabled {
            return Err(StoreError::Conflict(
                "该机器已移除容器宿主身份，请先重新添加宿主".into(),
            ));
        }
    }
    let caps: Capabilities = serde_json::from_value(row.try_get("capabilities")?)?;
    caps.validate().map_err(invalid)?;
    Ok(caps)
}

// Keep the approved specification reserved while an edit can still be withdrawn.
// Runtime-only generations also record their original revision in this journal.
pub(super) const COMPUTE_RESERVATIONS: &str = "SELECT i.id,i.tenant_id,i.user_id,
    greatest((i.desired#>>'{spec,resources,vcpus}')::bigint,coalesce((approved.spec#>>'{resources,vcpus}')::bigint,0)) AS vcpus,
    greatest((i.desired#>>'{spec,resources,memory_bytes}')::bigint,coalesce((approved.spec#>>'{resources,memory_bytes}')::bigint,0)) AS memory,
    coalesce((approved.spec#>>'{resources,vcpus}')::bigint,0) AS approved_vcpus,
    coalesce((approved.spec#>>'{resources,memory_bytes}')::bigint,0) AS approved_memory
    FROM cvm_instances i LEFT JOIN cvm_serving_nodes s ON s.node_id=i.node_id
    LEFT JOIN LATERAL (
      SELECT v.spec FROM cvm_revision_instances r JOIN cvm_spec_versions v
        ON v.instance_id=r.instance_id AND v.generation=r.generation
      WHERE r.instance_id=i.id AND r.revision_id<=s.revision_id
      ORDER BY r.generation DESC LIMIT 1
    ) approved ON true WHERE i.reserved";
