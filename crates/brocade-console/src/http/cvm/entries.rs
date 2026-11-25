//! Authenticated boundaries for container access entries and their node receipts.
//! Tunnel credentials only leave through the node-authenticated desired endpoint.
use super::*;
use brocade_deployment::cvm::*;

pub(crate) async fn board(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(state.store.cvm_entry_board(&actor).await?)))
}
pub(crate) async fn instance_entries(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(
        state.store.cvm_instance_entries(&actor, &id).await?,
    )))
}
#[derive(Deserialize)]
pub(crate) struct NetworkLogQuery {
    nodes: String,
}
pub(crate) async fn diagnostics(
    State(state): State<AppState>,
    Query(query): Query<NetworkLogQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let nodes = query
        .nodes
        .split(',')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Ok(private(Json(
        state.store.cvm_network_logs(&actor, &nodes).await?,
    )))
}
/// Ownership and write scope are enforced by Store against the target container.
pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EntryCreate>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private((
        StatusCode::CREATED,
        Json(state.store.create_cvm_entry(&actor, request).await?),
    )))
}
pub(crate) async fn repoint(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<EntryRepoint>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(
        state.store.repoint_cvm_entry(&actor, &id, request).await?,
    )))
}
pub(crate) async fn release(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<EntryRelease>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private((
        StatusCode::ACCEPTED,
        Json(state.store.release_cvm_entry(&actor, &id, request).await?),
    )))
}
pub(crate) async fn create_publication(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EntryPublicationWrite>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(private((
        StatusCode::CREATED,
        Json(
            state
                .store
                .write_cvm_entry_publication(&actor, None, request)
                .await?,
        ),
    )))
}
pub(crate) async fn update_publication(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<EntryPublicationWrite>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(private(Json(
        state
            .store
            .write_cvm_entry_publication(&actor, Some(&id), request)
            .await?,
    )))
}
pub(crate) async fn admission(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<PublicationAdmission>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(private(Json(
        state
            .store
            .set_cvm_entry_admission(&actor, &id, request)
            .await?,
    )))
}
pub(crate) async fn delete_publication(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<PublicationAdmission>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(private(Json(
        state
            .store
            .delete_cvm_entry_publication(&actor, &id, request)
            .await?,
    )))
}

pub(crate) async fn desired(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    let desired = state.store.cvm_entry_desired(&node.node_id).await?;
    Ok(private(Json(desired)))
}

pub(crate) async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EntryNodeReport>,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    Ok(private(Json(
        json!({"accepted":state.store.cvm_record_entry_report(&node.node_id,request).await?}),
    )))
}
