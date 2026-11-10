//! Thin authenticated HTTP boundaries for CVM operational intent.
use super::*;
pub(super) mod entries;
pub(super) mod nics;
pub(super) mod terminal;
use brocade_deployment::cvm::{InstanceReport, InventoryReport, CVM_PROTOCOL_VERSION};
use brocade_store::cvm::{CvmCommand, CvmCreate, CvmQuota};

fn private(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
pub(super) async fn workspace(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(state.store.cvm_workspace(&actor).await?)))
}

pub(super) async fn host_policy(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
    Json(request): Json<brocade_store::cvm::HostPolicyUpdate>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state
        .store
        .set_cvm_host_policy(&actor, &node, request)
        .await?;
    Ok(private(Json(state.store.cvm_workspace(&actor).await?)))
}

pub(super) async fn instance(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(state.store.cvm_instance(&actor, &id).await?)))
}
pub(super) async fn readings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(state.store.cvm_readings(&actor).await?)))
}
pub(super) async fn monitoring(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<MonitoringQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(
        state
            .store
            .cvm_monitoring_range(&actor, &id, query.window_secs.unwrap_or(3600))
            .await?,
    )))
}
#[derive(Deserialize)]
pub(super) struct MonitoringQuery {
    window_secs: Option<u32>,
}
#[derive(Deserialize)]
pub(super) struct ObservationUpload {
    observations: Vec<brocade_deployment::cvm::InstanceObservation>,
}
pub(super) async fn observation_targets(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    Ok(private(Json(
        json!({"targets":state.store.cvm_observation_targets(&node.node_id).await?}),
    )))
}
pub(super) async fn observations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ObservationUpload>,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    Ok(private(Json(
        json!({"accepted":state.store.cvm_record_observations(&node.node_id,request.observations).await?}),
    )))
}

pub(super) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CvmCreate>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private((
        StatusCode::ACCEPTED,
        Json(state.store.create_cvm(&actor, request).await?),
    )))
}
pub(super) async fn command(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CvmCommand>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private((
        StatusCode::ACCEPTED,
        Json(state.store.command_cvm(&actor, &id, request).await?),
    )))
}
pub(super) async fn quota(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CvmQuota>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state.store.set_cvm_quota(&actor, request).await?;
    Ok(private(StatusCode::NO_CONTENT))
}
pub(super) async fn network_quota_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<brocade_store::cvm::CvmNetworkQuotaPreviewRequest>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(private(Json(
        state
            .store
            .cvm_network_quota_preview(&actor, request)
            .await?,
    )))
}
async fn quota_agent_node(state: &AppState, headers: &HeaderMap) -> ApiResult<AuthenticatedNode> {
    let node = agent_node(state, headers).await?;
    if agent_protocol_version(headers)
        .is_none_or(|v| v < brocade_deployment::cvm::NETWORK_QUOTA_PROTOCOL_VERSION)
    {
        return Err(ApiError::Store(StoreError::Unsupported(
            "宿主网络额度需要新版 Agent".into(),
        )));
    }
    Ok(node)
}
pub(super) async fn network_quota_desired(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = quota_agent_node(&state, &headers).await?;
    Ok(private(Json(
        state.store.cvm_network_quota_desired(&node.node_id).await?,
    )))
}
#[derive(Deserialize)]
pub(super) struct NetworkQuotaUpload {
    reports: Vec<brocade_deployment::cvm::NetworkQuotaReport>,
}
pub(super) async fn network_quota_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<NetworkQuotaUpload>,
) -> ApiResult<Response> {
    let node = quota_agent_node(&state, &headers).await?;
    Ok(private(Json(
        json!({"accepted":state.store.cvm_record_network_quotas(&node.node_id,request.reports).await?}),
    )))
}
async fn agent_node(state: &AppState, headers: &HeaderMap) -> ApiResult<AuthenticatedNode> {
    let node = authenticate_agent(&state.store, headers).await?;
    require_active_agent(&node)?;
    if agent_protocol_version(headers).is_none_or(|v| v < CVM_PROTOCOL_VERSION) {
        return Err(ApiError::Store(StoreError::Unsupported(
            "CVM requires an Agent with the current CVM contract".into(),
        )));
    }
    Ok(node)
}
pub(super) async fn inventory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<InventoryReport>,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    state
        .store
        .cvm_record_inventory(&node.node_id, request)
        .await?;
    Ok(private(StatusCode::NO_CONTENT))
}
pub(super) async fn desired(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    Ok(private(Json(state.store.cvm_desired(&node.node_id).await?)))
}
pub(super) async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<InstanceReport>,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    let accepted = state
        .store
        .cvm_record_report(&node.node_id, request)
        .await?;
    Ok(private(Json(json!({"accepted":accepted}))))
}
