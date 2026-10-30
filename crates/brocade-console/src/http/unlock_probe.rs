use super::*;
use brocade_deployment::unlock_probe::{
    UnlockProbeReport, UNLOCK_DUAL_STACK_PROTOCOL_VERSION, UNLOCK_PROBE_PROTOCOL_VERSION,
};

#[derive(serde::Deserialize)]
pub(super) struct UnlockFamilyQuery {
    family: Option<IpFamily>,
    #[serde(default)]
    automatic: bool,
}

#[derive(serde::Deserialize)]
pub(super) struct ChainUnlockPageQuery {
    after: Option<String>,
    limit: Option<u32>,
}

pub(super) async fn refresh_all_unlock_probes(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(no_store_json(
        state.store.refresh_all_unlock_probes(&actor).await?,
    ))
}

pub(super) async fn get_chain_unlock_probes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ChainUnlockPageQuery>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(no_store_json(
        state
            .store
            .chain_unlock_probe_page(&actor, query.after.as_deref(), query.limit.unwrap_or(100))
            .await?,
    ))
}

pub(super) async fn get_node_unlock_probe(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
    Query(query): Query<UnlockFamilyQuery>,
) -> ApiResult<Response> {
    if query.automatic {
        return Err(StoreError::InvalidData("automatic mode is chain-only".into()).into());
    }
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(no_store_json(
        state
            .store
            .node_unlock_probe_on_family(&actor, &node, false, query.family)
            .await?,
    ))
}
pub(super) async fn start_node_unlock_probe(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
    Query(query): Query<UnlockFamilyQuery>,
) -> ApiResult<Response> {
    if query.automatic {
        return Err(StoreError::InvalidData("automatic mode is chain-only".into()).into());
    }
    let actor = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let visible = state.store.list_node_agent_states(&actor).await?;
    let machine = visible
        .nodes
        .iter()
        .find(|machine| machine.node_id == node)
        .ok_or_else(|| StoreError::NotFound("node".into()))?;
    if machine.lifecycle_phase != "active" {
        return Err(StoreError::Conflict("机器当前不在运行状态".into()).into());
    }
    if machine.agent_protocol_version.is_none_or(|version| {
        version
            < if query.family.is_some() {
                UNLOCK_DUAL_STACK_PROTOCOL_VERSION
            } else {
                UNLOCK_PROBE_PROTOCOL_VERSION
            } as i32
    }) {
        return Err(StoreError::Unsupported(
            "分族解锁检测需要 Agent 协议 v50，请先升级 Agent".into(),
        )
        .into());
    }
    Ok(no_store_json(
        state
            .store
            .node_unlock_probe_on_family(&actor, &node, true, query.family)
            .await?,
    ))
}
pub(super) async fn get_chain_unlock_probe(
    State(state): State<AppState>,
    Path((app, chain)): Path<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<UnlockFamilyQuery>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    if query.automatic && query.family.is_some() {
        return Err(StoreError::InvalidData("automatic probe cannot force a family".into()).into());
    }
    Ok(no_store_json(if query.automatic {
        state
            .store
            .chain_unlock_probe_automatic(&actor, &app, &chain, false)
            .await?
    } else {
        state
            .store
            .chain_unlock_probe_on_family(&actor, &app, &chain, false, query.family)
            .await?
    }))
}
pub(super) async fn start_chain_unlock_probe(
    State(state): State<AppState>,
    Path((app, chain)): Path<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<UnlockFamilyQuery>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    if query.automatic && query.family.is_some() {
        return Err(StoreError::InvalidData("automatic probe cannot force a family".into()).into());
    }
    Ok(no_store_json(if query.automatic {
        state
            .store
            .chain_unlock_probe_automatic(&actor, &app, &chain, true)
            .await?
    } else {
        state
            .store
            .chain_unlock_probe_on_family(&actor, &app, &chain, true, query.family)
            .await?
    }))
}
fn require_protocol(headers: &HeaderMap) -> ApiResult<()> {
    if agent_protocol_version(headers).is_none_or(|version| version < UNLOCK_PROBE_PROTOCOL_VERSION)
    {
        return Err(StoreError::Unsupported("解锁检测需要 Agent 协议 v46".into()).into());
    }
    Ok(())
}
pub(super) async fn agent_unlock_probe_target(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_active_agent(&node)?;
    require_protocol(&headers)?;
    Ok(no_store_json(
        state
            .store
            .claim_unlock_probe(
                &node.node_id,
                agent_protocol_version(&headers).unwrap_or_default(),
            )
            .await?,
    ))
}
pub(super) async fn agent_unlock_probe_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(report): Json<UnlockProbeReport>,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_active_agent(&node)?;
    require_protocol(&headers)?;
    state
        .store
        .report_unlock_probe(&node.node_id, report)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub(super) async fn get_node_unlock_capabilities(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(no_store_json(
        state.store.node_unlock_capabilities(&actor, &node).await?,
    ))
}
pub(super) async fn get_chain_unlock_capabilities(
    State(state): State<AppState>,
    Path((app, chain)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(no_store_json(
        state
            .store
            .chain_unlock_capabilities(&actor, &app, &chain)
            .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;

    #[test]
    fn unlock_reports_are_readable_without_opening_writes_or_other_agent_routes() {
        for path in [
            "/chains/unlock-probes",
            "/nodes/n1/unlock-probe",
            "/nodes/n1/unlock-probe/capabilities",
            "/apps/app-one/chains/chain-one/unlock-probe",
            "/apps/app-one/chains/chain-one/unlock-probe/capabilities",
        ] {
            assert!(public_may(&Method::GET, path), "guest GET {path}");
            assert!(user_may(&Method::GET, path), "user GET {path}");
            for method in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
                assert!(!public_may(&method, path), "guest {method} {path}");
                assert!(!user_may(&method, path), "user {method} {path}");
            }
        }
        for path in [
            "/nodes//unlock-probe",
            "/apps//chains/chain-one/unlock-probe",
            "/apps/app-one/chains//unlock-probe",
            "/nodes/n1/unlock-probe/",
            "/nodes/n1/unlock-probe/report",
            "/nodes/n1/unlock-probe/capabilities/extra",
            "/apps/app-one/chains/chain-one/unlock-probe/target",
            "/apps/app-one/chains/chain-one/unlock-probe/capabilities/extra",
            "/agent/v1/unlock-probe/target",
            "/agent/v1/unlock-probe/report",
        ] {
            assert!(!public_may(&Method::GET, path), "guest GET {path}");
            assert!(!user_may(&Method::GET, path), "user GET {path}");
        }
    }
}
