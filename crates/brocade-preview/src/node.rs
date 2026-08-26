use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    addr::allocate_address,
    console::post_console_json,
    docker::{container_name, docker_output},
    error::PreviewError,
    param::required_slug,
    provision::{ensure_agent_binary, start_node_container, INSTALL_LOG_PATH},
    AppState,
};

#[derive(Debug, Serialize)]
struct PreviewStatus {
    enabled: bool,
    runtime: &'static str,
    console_url: String,
    agent_url: String,
    preview_public_url: String,
    network: String,
    subnet: String,
    subnet_ipv6: String,
    node_image: String,
    agent_binary_ready: bool,
}

/// The UI decides between preview enrollment and the production manual install flow by whether
/// this endpoint exists.
pub(crate) async fn preview_status(State(state): State<AppState>) -> impl IntoResponse {
    let agent_binary_ready = state.config.agent_bin_path.is_file();
    Json(PreviewStatus {
        enabled: true,
        runtime: "docker",
        console_url: state.config.console_url,
        agent_url: state.config.agent_url,
        preview_public_url: state.config.preview_public_url,
        network: state.config.network,
        subnet: state.config.subnet,
        subnet_ipv6: state.config.subnet_ipv6,
        node_image: state.config.node_image,
        agent_binary_ready,
    })
}

/// Where the install script inside a container fetches the locally built agent binary.
pub(crate) async fn preview_agent_binary(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, PreviewError> {
    ensure_agent_binary(&state.config).await?;
    let bytes = tokio::fs::read(&state.config.agent_bin_path)
        .await
        .map_err(|error| {
            PreviewError::internal(format!(
                "read {}: {error}",
                state.config.agent_bin_path.display()
            ))
        })?;
    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from(bytes))
        .map_err(|error| PreviewError::internal(format!("build binary response: {error}")))?;
    Ok(response)
}

#[derive(Debug, Deserialize)]
pub(crate) struct PreviewProvisionRequest {
    id: String,
    tenant_id: String,
    name: Option<String>,
    #[serde(default)]
    public_ipv4_nat: bool,
    #[serde(default)]
    public_ipv6_nat: bool,
    #[serde(default)]
    egress_allowed: bool,
    #[serde(default)]
    dns: Option<Value>,
    #[serde(default)]
    domain_strategy: Option<Value>,
}

/// Allocate an address, obtain an enrollment token through the production provision path, start a
/// container, and run the install script inside it.
pub(crate) async fn preview_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PreviewProvisionRequest>,
) -> Result<impl IntoResponse, PreviewError> {
    let node_id = required_slug(&request.id, "id")?;
    let tenant_id = request.tenant_id.trim();
    if tenant_id.is_empty() {
        return Err(PreviewError::bad_request("tenant_id is required"));
    }
    let address = allocate_address(&state, &headers).await?;
    let mut provision_body = json!({
        "id": node_id,
        "tenant_id": tenant_id,
        "name": request.name.as_deref().unwrap_or(node_id),
        "public_ipv4": address.ipv4.to_string(),
        "public_ipv6": address.ipv6.to_string(),
        "public_ipv4_nat": request.public_ipv4_nat,
        "public_ipv6_nat": request.public_ipv6_nat,
        "wg_listen_port": 51820,
        "api_port": 10085,
        "overlay": true,
        "egress_allowed": request.egress_allowed,
        "dns": request.dns.unwrap_or_else(|| json!({ "t": "system" })),
        "note": format!("preview provision {node_id}")
    });
    // Inserted only when supplied rather than defaulted here: the provision request already
    // defaults it, and an explicit null is not the same as an absent key to serde — the
    // former would be rejected where the latter takes the default.
    if let Some(strategy) = request.domain_strategy {
        provision_body["domain_strategy"] = strategy;
    }
    let mut provision = post_console_json(&state, "/nodes/provision", &headers, &provision_body)
        .await
        .map_err(PreviewError::upstream)?;
    let token = provision
        .get("enrollment")
        .and_then(|value| value.get("token"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            PreviewError::upstream("provision response did not contain enrollment token")
        })?
        .to_owned();

    let runtime = start_node_container(&state.config, node_id, address, &token).await?;
    provision["preview"] = serde_json::to_value(runtime)
        .map_err(|error| PreviewError::internal(format!("serialize preview runtime: {error}")))?;
    Ok((StatusCode::CREATED, Json(provision)))
}

pub(crate) async fn preview_node_logs(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
) -> Result<impl IntoResponse, PreviewError> {
    let node_id = required_slug(&node_id, "node_id")?;
    let container = container_name(node_id);
    let install = docker_output(
        &state.config,
        &[
            "exec",
            &container,
            "sh",
            "-lc",
            &format!("cat {INSTALL_LOG_PATH} 2>/dev/null || true"),
        ],
    )
    .await
    .unwrap_or_else(|error| format!("install log unavailable: {}", error.message));
    let logs = docker_output(&state.config, &["logs", "--tail", "200", &container])
        .await
        .unwrap_or_else(|error| format!("container log unavailable: {}", error.message));
    Ok(Json(json!({
        "node_id": node_id,
        "container_name": container,
        "install_log": install,
        "container_log": logs
    })))
}

pub(crate) async fn preview_node_delete(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
) -> Result<impl IntoResponse, PreviewError> {
    let node_id = required_slug(&node_id, "node_id")?;
    let container = container_name(node_id);
    docker_output(&state.config, &["rm", "-f", &container]).await?;
    Ok(Json(
        json!({ "removed": true, "container_name": container }),
    ))
}
