use super::*;
use brocade_deployment::ip_maintenance::*;

pub(super) async fn catalog(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(no_store_json(data::regions().iter().map(|r| json!({"id":r.id,"country":r.country,"state":r.state,"city":r.city,"name":r.name,"language_params":r.language_params,"latitude":r.latitude,"longitude":r.longitude})).collect::<Vec<_>>()))
}

pub(super) async fn view(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(no_store_json(
        state.store.ip_maintenance_view(&actor, &node).await?,
    ))
}
pub(super) async fn update(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
    Json(request): Json<UpdateIpMaintenance>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state
        .store
        .update_ip_maintenance(&actor, &node, request)
        .await?;
    state
        .realtime
        .wake_agents(&[brocade_deployment::agent_channel::WakeDomain::Desired])
        .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}
pub(super) async fn run(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
    Json(request): Json<IpMaintenanceCommand>,
) -> ApiResult<Response> {
    command(state, node, headers, request, false).await
}
pub(super) async fn reset(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
    Json(request): Json<IpMaintenanceCommand>,
) -> ApiResult<Response> {
    command(state, node, headers, request, true).await
}
async fn command(
    state: AppState,
    node: String,
    headers: HeaderMap,
    request: IpMaintenanceCommand,
    reset: bool,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state
        .store
        .command_ip_maintenance(&actor, &node, request, reset)
        .await?;
    state
        .realtime
        .wake_agents(&[brocade_deployment::agent_channel::WakeDomain::Desired])
        .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}
fn version(headers: &HeaderMap) -> ApiResult<()> {
    if agent_protocol_version(headers).is_none_or(|v| v < IP_MAINTENANCE_PROTOCOL_VERSION) {
        return Err(StoreError::Unsupported("IP 养护需要更新 Agent".into()).into());
    }
    Ok(())
}
pub(super) async fn desired(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_active_agent(&node)?;
    version(&headers)?;
    let desired = state.store.ip_maintenance_desired(&node.node_id).await?;
    if agent_protocol_version(&headers).is_none_or(|v| v < desired.policy.protocol_version()) {
        return Err(StoreError::Unsupported("双栈养护需要升级 Agent".into()).into());
    }
    Ok(no_store_json(desired))
}

pub(super) async fn data(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_active_agent(&node)?;
    version(&headers)?;
    Ok(no_store_json(
        state.store.ip_maintenance_data(&node.node_id).await?,
    ))
}
pub(super) async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(report): Json<IpMaintenanceReport>,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_active_agent(&node)?;
    version(&headers)?;
    state
        .store
        .report_ip_maintenance(&node.node_id, report)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(serde::Deserialize)]
pub(super) struct RunsQuery {
    cursor: Option<u64>,
}
pub(super) async fn runs(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
    Query(query): Query<RunsQuery>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(no_store_json(
        state
            .store
            .ip_maintenance_runs(&actor, &node, query.cursor)
            .await?,
    ))
}
pub(super) async fn run_detail(
    State(state): State<AppState>,
    Path((node, run)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(no_store_json(
        state.store.ip_maintenance_run(&actor, &node, &run).await?,
    ))
}
pub(super) async fn stop(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
    Json(request): Json<IpMaintenanceStop>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state
        .store
        .stop_ip_maintenance(&actor, &node, request)
        .await?;
    state
        .realtime
        .wake_agents(&[brocade_deployment::agent_channel::WakeDomain::Desired])
        .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}
