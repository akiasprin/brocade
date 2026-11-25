//! Thin authenticated HTTP boundaries for CVM operational intent.
use super::*;
pub(super) mod entries;
pub(super) mod nics;
pub(super) mod terminal;
use brocade_deployment::cvm::{CvmImageManifest, InstanceReport, InventoryReport};
use brocade_store::cvm::{CvmCommand, CvmCreate};

/// User configuration errors have the same JSON/Chinese feedback as Store validation.
/// Preserve extractor status codes, including body-size and content-type failures.
pub(super) struct Configuration<T>(pub T);
impl<S, T> axum::extract::FromRequest<S> for Configuration<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned + Send,
{
    type Rejection = Response;

    async fn from_request(
        request: axum::extract::Request,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        <Json<T> as axum::extract::FromRequest<S>>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|error| {
                let message = match error.status() {
                    StatusCode::PAYLOAD_TOO_LARGE => "配置内容过大，请减少配置项后重试。",
                    StatusCode::UNSUPPORTED_MEDIA_TYPE => "请以 JSON 格式提交配置。",
                    _ => "配置格式不正确，请检查 IP／网段、端口和数值字段后重试。",
                };
                private((error.status(), Json(json!({ "error": message }))).into_response())
            })
    }
}

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
    Configuration(request): Configuration<CvmCompilePreview>,
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

pub(super) async fn host_install_command(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let distribution = state.distribution().await?;
    let server = &distribution.agent_public_url;
    let digest = install_script_sha256(&distribution);
    let command = format!(
        "installer=$(mktemp) && curl -fsSL {} -o \"$installer\" && printf '%s  %s\\n' {} \"$installer\" | sha256sum -c - && sudo sh \"$installer\" --server {} --reuse-node-token",
        shell_quote(&format!("{server}/enroll/install.sh")),
        shell_quote(&digest),
        shell_quote(server),
    );
    Ok(private(Json(json!({ "install_command": command }))))
}

pub(super) async fn instance(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(state.store.cvm_instance(&actor, &id).await?)))
}
pub(super) async fn pending_review(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    Ok(private(Json(
        state.store.cvm_pending_review(&actor, &id).await?,
    )))
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
    Configuration(request): Configuration<CvmCreate>,
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
    Configuration(request): Configuration<CvmCommand>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let access_changed = matches!(request.change, brocade_store::cvm::CvmChange::Assign { .. });
    let result = state
        .store
        .command_cvm_reviewed(&actor, &id, request, mutation_review(&headers)?)
        .await?;
    if access_changed {
        state.terminals.close_instance_sessions(&id);
    }
    Ok(private((StatusCode::ACCEPTED, Json(result.instance))))
}

pub(super) async fn login_credential(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(private(Json(
        state.store.cvm_login_credential(&actor, &id).await?,
    )))
}
#[derive(Deserialize)]
pub(super) struct CredentialQuery {
    revision: u64,
}
pub(super) async fn node_credential(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<CredentialQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    Ok(private(Json(
        state
            .store
            .cvm_node_credential(&node.node_id, &id, query.revision)
            .await?,
    )))
}
pub(super) async fn network_quota_desired(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
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
    let node = agent_node(&state, &headers).await?;
    Ok(private(Json(
        json!({"accepted":state.store.cvm_record_network_quotas(&node.node_id,request.reports).await?}),
    )))
}
async fn agent_node(state: &AppState, headers: &HeaderMap) -> ApiResult<AuthenticatedNode> {
    let node = authenticate_agent(&state.store, headers).await?;
    require_active_agent(&node)?;
    if agent_protocol_version(headers) != Some(brocade_deployment::protocol::AGENT_PROTOCOL_VERSION)
    {
        return Err(StoreError::Unsupported("容器节点必须使用当前执行契约".into()).into());
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
    let desired = state
        .store
        .cvm_desired_page(&node.node_id, query.offset.unwrap_or(0))
        .await?;
    Ok(private(Json(desired)))
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

#[cfg(test)]
mod configuration_tests {
    use super::*;
    use axum::extract::FromRequest;

    #[tokio::test]
    async fn malformed_routes_keep_status_and_return_chinese_json_without_echoing_input() {
        for dns in [json!(["127.0.0.1"]), json!(["bad-dns"])] {
            let request = Request::builder().header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"request_id":"configuration-test-01", "expected_generation":1,
                    "expected_revision":1,"routes":{"revision":1,"dns":dns,"priorities":[],"routes":[
                        {"destination":"10.0.0.1/24","interface_id":"nic-test"}
                    ]}}).to_string())).unwrap();
            let response =
                match Configuration::<brocade_deployment::cvm::RoutesWrite>::from_request(
                    request,
                    &(),
                )
                .await
                {
                    Ok(_) => panic!("noncanonical prefix must be rejected"),
                    Err(response) => response,
                };
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                value["error"],
                "配置格式不正确，请检查 IP／网段、端口和数值字段后重试。"
            );
            assert!(!String::from_utf8(body.to_vec())
                .unwrap()
                .contains("bad-dns"));
        }
        let request = Request::builder().body(Body::from("{}")).unwrap();
        let response = match Configuration::<CvmCreate>::from_request(request, &()).await {
            Ok(_) => panic!("JSON content type is required"),
            Err(response) => response,
        };
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
}
