//! Authenticated boundaries for reusable network publications and allocated container NICs.
use super::*;
use brocade_deployment::cvm::*;

pub(crate) async fn catalog(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(state.store.cvm_nic_catalog(&actor).await?)))
}
pub(crate) async fn create_publication(
    State(state): State<AppState>,
    headers: HeaderMap,
    Configuration(request): Configuration<PublicationWrite>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(private((
        StatusCode::CREATED,
        Json(
            state
                .store
                .write_cvm_nic_publication(&actor, None, request)
                .await?,
        ),
    )))
}
pub(crate) async fn update_publication(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Configuration(request): Configuration<PublicationWrite>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(private(Json(
        state
            .store
            .write_cvm_nic_publication(&actor, Some(&id), request)
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
            .set_cvm_nic_admission(&actor, &id, request)
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
            .delete_cvm_nic_publication(&actor, &id, request)
            .await?,
    )))
}
pub(crate) async fn route_match(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Configuration(request): Configuration<brocade_store::cvm::CvmRouteMatchRequest>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    Ok(private(Json(
        state.store.match_cvm_route(&actor, &id, request).await?,
    )))
}
pub(crate) async fn instance_network(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(state.store.cvm_nic_state(&actor, &id).await?)))
}
pub(crate) async fn mount(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<NicMountRequest>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private((
        StatusCode::ACCEPTED,
        Json(
            state
                .store
                .mount_cvm_nic_reviewed(&actor, &id, request, mutation_review(&headers)?)
                .await?
                .instance,
        ),
    )))
}
pub(crate) async fn routes(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Configuration(request): Configuration<RoutesWrite>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private((
        StatusCode::ACCEPTED,
        Json(
            state
                .store
                .set_cvm_routes_reviewed(&actor, &id, request, mutation_review(&headers)?)
                .await?
                .instance,
        ),
    )))
}
#[derive(Deserialize)]
pub(crate) struct NetworkEditQuery {
    #[serde(default)]
    preview: bool,
}

pub(crate) async fn network_edit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<NetworkEditQuery>,
    headers: HeaderMap,
    Configuration(request): Configuration<brocade_store::cvm::CvmNetworkEditRequest>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let mut review = mutation_review(&headers)?;
    review.preview = query.preview;
    let result = state
        .store
        .edit_cvm_network(&actor, &id, request, review)
        .await?;
    let status = if query.preview {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok(private((status, Json(result))))
}

pub(crate) async fn transport(
    State(state): State<AppState>,
    Path((id, nic)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<NicTransportChange>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private((
        StatusCode::ACCEPTED,
        Json(
            state
                .store
                .change_cvm_nic_transport(&actor, &id, &nic, request)
                .await?,
        ),
    )))
}
pub(crate) async fn version(
    State(state): State<AppState>,
    Path((id, nic)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<NicVersionChange>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private((
        StatusCode::ACCEPTED,
        Json(
            state
                .store
                .update_cvm_nic_version(&actor, &id, &nic, request)
                .await?,
        ),
    )))
}
pub(crate) async fn inventory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<NicNodeCapabilities>,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    state
        .store
        .cvm_record_nic_capabilities(&node.node_id, request)
        .await?;
    Ok(private(StatusCode::NO_CONTENT))
}
pub(crate) async fn desired(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    Ok(private(Json(
        state.store.cvm_nic_desired(&node.node_id).await?,
    )))
}
pub(crate) async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<NicNodeReport>,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    Ok(private(Json(
        json!({"accepted":state.store.cvm_record_nic_report(&node.node_id,request).await?}),
    )))
}

pub(crate) async fn versions(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<NicVersionsChange>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private((
        StatusCode::ACCEPTED,
        Json(
            state
                .store
                .update_cvm_nic_versions(&actor, &id, request)
                .await?,
        ),
    )))
}
