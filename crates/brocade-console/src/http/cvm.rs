//! Thin authenticated HTTP boundaries for CVM operational intent.
use super::*;
pub(super) mod entries;
pub(super) mod nics;
pub(super) mod terminal;
use brocade_deployment::cvm::{
    CvmArtifactMode, CvmImageManifest, InstanceReport, InventoryReport,
    CHAIN_EGRESS_PROTOCOL_VERSION, CVM_ARTIFACT_PROTOCOL_VERSION, CVM_PROTOCOL_VERSION,
};
use brocade_store::cvm::{CvmCommand, CvmCreate, CvmQuota};

fn private(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
pub(super) fn mutation_review(
    headers: &HeaderMap,
) -> ApiResult<brocade_store::cvm::CvmMutationReview> {
    let expected = headers
        .get("x-brocade-cvm-input-sha256")
        .map(|v| v.to_str().unwrap_or("").to_owned());
    if expected
        .as_ref()
        .is_some_and(|s| !brocade_deployment::cvm::valid_digest(s))
    {
        return Err(brocade_store::StoreError::InvalidData("编译预览摘要无效".into()).into());
    }
    Ok(brocade_store::cvm::CvmMutationReview {
        preview: false,
        expected_input_sha256: expected,
    })
}
#[derive(Deserialize)]
#[serde(tag = "kind", content = "request", rename_all = "snake_case")]
pub(super) enum CvmCompilePreview {
    Command(CvmCommand),
    Mount(brocade_deployment::cvm::NicMountRequest),
    Routes(brocade_deployment::cvm::RoutesWrite),
}
pub(super) async fn compile_preview(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CvmCompilePreview>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let review = brocade_store::cvm::CvmMutationReview {
        preview: true,
        expected_input_sha256: None,
    };
    let result = match request {
        CvmCompilePreview::Command(r) => {
            state
                .store
                .command_cvm_reviewed(&actor, &id, r, review)
                .await?
        }
        CvmCompilePreview::Mount(r) => {
            state
                .store
                .mount_cvm_nic_reviewed(&actor, &id, r, review)
                .await?
        }
        CvmCompilePreview::Routes(r) => {
            state
                .store
                .set_cvm_routes_reviewed(&actor, &id, r, review)
                .await?
        }
    };
    Ok(private(Json(result)))
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

pub(super) async fn adopt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<brocade_store::cvm::CvmAdopt>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(private((
        StatusCode::ACCEPTED,
        Json(state.store.adopt_cvm_host(&actor, request).await?),
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
        Json(
            state
                .store
                .command_cvm_reviewed(&actor, &id, request, mutation_review(&headers)?)
                .await?
                .instance,
        ),
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
    if request.capabilities.baremetal.is_some()
        && agent_protocol_version(&headers)
            .is_none_or(|v| v < brocade_deployment::cvm::BAREMETAL_PROTOCOL_VERSION)
    {
        return Err(ApiError::Store(StoreError::Unsupported(
            "裸机需要新版 Agent".into(),
        )));
    }
    state
        .store
        .cvm_record_inventory(&node.node_id, request)
        .await?;
    Ok(private(Json(
        json!({"missing_image_manifests":state.store.cvm_missing_image_manifests(&node.node_id).await?}),
    )))
}
pub(super) async fn image_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(manifest): Json<CvmImageManifest>,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    if agent_protocol_version(&headers).is_none_or(|v| v < CVM_ARTIFACT_PROTOCOL_VERSION) {
        return Err(ApiError::Store(StoreError::Unsupported(
            "镜像清单需要新版 Agent".into(),
        )));
    }
    state
        .store
        .cvm_record_image_manifest(&node.node_id, manifest)
        .await?;
    Ok(private(StatusCode::NO_CONTENT))
}
#[derive(Deserialize)]
pub(super) struct ArtifactQuery {
    generation: Option<u64>,
}

#[derive(Deserialize)]
pub(super) struct ChainCatalogQuery {
    instance_id: Option<String>,
}
pub(super) async fn chain_catalog(
    State(state): State<AppState>,
    Query(query): Query<ChainCatalogQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(
        state
            .store
            .cvm_chain_catalog(&actor, query.instance_id.as_deref())
            .await?,
    )))
}
pub(super) async fn artifacts(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ArtifactQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(
        state
            .store
            .cvm_artifacts(&actor, &id, query.generation)
            .await?,
    )))
}
#[derive(Deserialize)]
pub(super) struct DesiredQuery {
    offset: Option<usize>,
}
pub(super) async fn desired(
    State(state): State<AppState>,
    Query(query): Query<DesiredQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    let current =
        agent_protocol_version(&headers).is_some_and(|v| v >= CVM_ARTIFACT_PROTOCOL_VERSION);
    let mut desired = state
        .store
        .cvm_desired_page(
            &node.node_id,
            if current {
                Some(query.offset.unwrap_or(0))
            } else {
                None
            },
        )
        .await?;
    if !desired.chain_clients.is_empty()
        && agent_protocol_version(&headers).is_none_or(|v| v < CHAIN_EGRESS_PROTOCOL_VERSION)
    {
        return Err(ApiError::Store(StoreError::Unsupported(
            "线路出口需要协议 76 或更新的 Agent".into(),
        )));
    }
    let mode = match std::env::var("BROCADE_CVM_ARTIFACT_MODE").as_deref() {
        Ok("enforce") => CvmArtifactMode::Enforce,
        Ok("shadow") | Err(std::env::VarError::NotPresent) => CvmArtifactMode::Shadow,
        _ => {
            return Err(ApiError::Store(StoreError::InvalidData(
                "BROCADE_CVM_ARTIFACT_MODE must be shadow or enforce".into(),
            )))
        }
    };
    for instance in &mut desired.instances {
        if agent_protocol_version(&headers).is_none_or(|v| v < CVM_ARTIFACT_PROTOCOL_VERSION) {
            instance.artifacts = None;
        } else if instance.artifacts.is_some() {
            instance.artifact_mode = mode;
        }
    }
    if desired
        .instances
        .iter()
        .any(|d| !d.spec.execution.is_cube())
        && agent_protocol_version(&headers)
            .is_none_or(|v| v < brocade_deployment::cvm::BAREMETAL_PROTOCOL_VERSION)
    {
        return Err(ApiError::Store(StoreError::Unsupported(
            "裸机期望状态需要新版 Agent".into(),
        )));
    }
    Ok(private(Json(desired)))
}
pub(super) async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<InstanceReport>,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    if request.host.is_some()
        && agent_protocol_version(&headers)
            .is_none_or(|v| v < brocade_deployment::cvm::BAREMETAL_PROTOCOL_VERSION)
    {
        return Err(ApiError::Store(StoreError::Unsupported(
            "裸机回执需要新版 Agent".into(),
        )));
    }
    let accepted = state
        .store
        .cvm_record_report(&node.node_id, request)
        .await?;
    Ok(private(Json(json!({"accepted":accepted}))))
}
