use super::*;

/// Move the complete chain in the draft transaction. Historical snapshots and usage rows keep
/// their original app; future accounting follows the configuration's published generation.
pub(crate) async fn move_chain_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    app_id: String,
    chain_id: String,
    target_app_id: String,
) -> Result<bool> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can move chains between apps".to_owned(),
        ));
    }
    let app_id = required_text(app_id, "app_id")?;
    let chain_id = required_slug(chain_id, "chain id")?;
    let target_app_id = required_text(target_app_id, "target_app_id")?;
    let actual_app: String =
        sqlx::query_scalar("SELECT app_id FROM chains WHERE id = $1 FOR UPDATE")
            .bind(&chain_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("chain {chain_id}")))?;
    if actual_app != app_id {
        return Err(StoreError::Conflict(format!(
            "chain {chain_id} no longer belongs to app {app_id}; refresh before moving it"
        )));
    }
    ensure_app_exists_tx(tx, &target_app_id).await?;
    if app_id == target_app_id {
        return Ok(false);
    }

    // Pools are app-owned and may include other chains. Moving one member must not silently
    // change a pool's topology or leave an app pointing at an ingress in another app.
    let pooled: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM ingresses i WHERE i.chain_id = $1
            AND (i.front_id IS NOT NULL OR EXISTS (
                SELECT 1 FROM front_vias v WHERE v.ingress_id = i.id
            ))
        )",
    )
    .bind(&chain_id)
    .fetch_one(&mut **tx)
    .await?;
    if pooled {
        return Err(StoreError::Unsupported(
            "请先解除这条链的接入池成员和前置入口引用，再移动分组".to_owned(),
        ));
    }

    let position = new_chain_position_tx(tx, &target_app_id).await?;
    // These are mutable authoring associations, unlike the append-only accounting ledger.
    for table in ["grants", "quota_suspensions"] {
        let query = format!(
            "UPDATE {table} SET app_id = $1
             WHERE ingress_id IN (SELECT id FROM ingresses WHERE chain_id = $2)"
        );
        sqlx::query(&query)
            .bind(&target_app_id)
            .bind(&chain_id)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("UPDATE ingresses SET app_id = $1 WHERE chain_id = $2")
        .bind(&target_app_id)
        .bind(&chain_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE chains SET app_id = $1, position = $2 WHERE id = $3")
        .bind(&target_app_id)
        .bind(position)
        .bind(&chain_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE e2e_probes SET app_id = $1 WHERE chain_id = $2")
        .bind(&target_app_id)
        .bind(&chain_id)
        .execute(&mut **tx)
        .await?;
    // The composite FK cascades app_id, but the diagnostic lookup key also embeds the app.
    // Keep family suffixes and results intact, including jobs claimed before this commit.
    let old_key = format!("chain:{app_id}/{chain_id}");
    let new_key = format!("chain:{target_app_id}/{chain_id}");
    sqlx::query(
        "UPDATE unlock_probe_jobs
         SET scope_key = $1 || substr(scope_key, length($2) + 1)
         WHERE chain_id = $3 AND starts_with(scope_key, $2)",
    )
    .bind(new_key)
    .bind(old_key)
    .bind(&chain_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE first_run_onboarding SET app_id = $1 WHERE chain_id = $2")
        .bind(target_app_id)
        .bind(chain_id)
        .execute(&mut **tx)
        .await?;
    Ok(true)
}
