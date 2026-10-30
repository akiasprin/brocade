//! Dispatch the finite negotiated RPC surface through the original authenticated handlers.
use super::*;
use brocade_deployment::agent_channel::{self, AgentChannelRequest, AgentChannelResponse};
use tower::ServiceExt;

pub(super) async fn dispatch(
    router: Router,
    authenticated: HeaderMap,
    request: AgentChannelRequest,
) -> AgentChannelResponse {
    let AgentChannelRequest::Rpc {
        id,
        method,
        path,
        body,
        headers,
        timeout_millis,
    } = request;
    let failure = |status, message: &str| AgentChannelResponse::RpcResult {
        id,
        status,
        body: message.into(),
        headers: Vec::new(),
    };
    if !agent_channel::rpc_allowed(&method, &path)
        || body.len() > agent_channel::MAX_RPC_BODY_BYTES
        || headers.len() > 16
        || !(1..=30_000).contains(&timeout_millis)
    {
        return failure(400, "invalid Agent channel RPC");
    }
    let mut request = axum::http::Request::builder()
        .method(method.as_str())
        .uri(&path);
    for (name, value) in authenticated.iter() {
        request = request.header(name, value);
    }
    for (name, value) in headers {
        if !agent_channel::request_header_allowed(&name) || value.len() > 4096 {
            return failure(400, "invalid Agent channel RPC header");
        }
        request = request.header(name, value);
    }
    let Ok(request) = request
        .header("content-type", "application/json")
        .body(Body::from(body))
    else {
        return failure(400, "invalid Agent channel RPC request");
    };
    let result = tokio::time::timeout(Duration::from_millis(u64::from(timeout_millis)), async {
        let response = router
            .oneshot(request)
            .await
            .expect("Router's service error is Infallible");
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.to_string(), value.to_owned()))
            })
            .collect();
        let body = axum::body::to_bytes(response.into_body(), agent_channel::MAX_RPC_BODY_BYTES)
            .await
            .ok()?;
        Some(AgentChannelResponse::RpcResult {
            id,
            status,
            body: String::from_utf8(body.to_vec()).ok()?,
            headers,
        })
    })
    .await;
    match result {
        Ok(Some(response))
            if serde_json::to_vec(&response)
                .is_ok_and(|bytes| bytes.len() <= agent_channel::MAX_FRAME_BYTES) =>
        {
            response
        }
        Ok(_) if method == "GET" => AgentChannelResponse::HttpRequired { id },
        Ok(_) => failure(502, "Agent channel RPC response exceeds its bound"),
        Err(_) => failure(504, "Agent channel RPC deadline exceeded"),
    }
}

/// Covers process-local task queues too. Durable database changes have their own commit signal.
pub(super) async fn wake_after_admin_change(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    use brocade_deployment::agent_channel::WakeDomain;
    let mutation = matches!(
        *request.method(),
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::DELETE
            | axum::http::Method::PATCH
    );
    let path = request.uri().path();
    let domain = if path.ends_with("/tcp-ping") {
        Some(WakeDomain::ForwardingProbe)
    } else if path.ends_with("/settings/probe") || path.ends_with("/ping-probe/settings") {
        Some(WakeDomain::Probe)
    } else {
        None
    };
    let response = next.run(request).await;
    if mutation && response.status().is_success() {
        if let Some(domain) = domain {
            state.realtime.wake_agents(&[domain]).await;
        }
    }
    response
}
