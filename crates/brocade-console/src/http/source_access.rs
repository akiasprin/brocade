use super::*;
use brocade_deployment::source_access::{
    SourceAccessObservation, SourceAccessView, SourceAccessWrite, SOURCE_ACCESS_PROTOCOL_VERSION,
};

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

/// A denied address may never have appeared in the user's presence history, so the list carries
/// the same local country/operator decoration as presence instead of relying on that history.
async fn located(state: &AppState, view: SourceAccessView) -> ApiResult<Response> {
    let addresses = view.ips.clone();
    Ok(no_store(
        with_source_locations(state, view, addresses)
            .await?
            .into_response(),
    ))
}

pub(super) async fn get_user_source_access(
    State(state): State<AppState>,
    Path((tenant, user)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let view = state
        .store
        .source_access_view(&actor, None, Some(&tenant), Some(&user))
        .await?;
    located(&state, view).await
}
pub(super) async fn put_user_source_access(
    State(state): State<AppState>,
    Path((tenant, user)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<SourceAccessWrite>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let view = state
        .store
        .update_source_access(&actor, None, Some(&tenant), Some(&user), request)
        .await?;
    located(&state, view).await
}
pub(super) async fn get_node_source_access(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let view = state
        .store
        .source_access_view(&actor, Some(&node), None, None)
        .await?;
    located(&state, view).await
}
pub(super) async fn put_node_source_access(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SourceAccessWrite>,
) -> ApiResult<Response> {
    let actor = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let view = state
        .store
        .update_source_access(&actor, Some(&node), None, None, request)
        .await?;
    located(&state, view).await
}
fn require_protocol(headers: &HeaderMap) -> ApiResult<()> {
    if agent_protocol_version(headers)
        .is_none_or(|version| version < SOURCE_ACCESS_PROTOCOL_VERSION)
    {
        return Err(ApiError::Store(StoreError::Unsupported(
            "来源封禁需要 Agent v45".into(),
        )));
    }
    Ok(())
}
#[derive(serde::Deserialize)]
pub(super) struct SourceAccessQuery {
    after_revision: Option<i64>,
}
pub(super) async fn agent_source_access(
    State(state): State<AppState>,
    Query(query): Query<SourceAccessQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_protocol(&headers)?;
    if query.after_revision == Some(state.store.source_access_revision().await?) {
        return Ok(no_store(StatusCode::NO_CONTENT.into_response()));
    }
    Ok(no_store(
        Json(state.store.source_access_desired(&node.node_id).await?).into_response(),
    ))
}
pub(super) async fn agent_source_access_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(report): Json<SourceAccessObservation>,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_protocol(&headers)?;
    state
        .store
        .report_source_access(&node.node_id, report)
        .await?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_access_routes_are_never_exposed_to_visitors_or_ordinary_users() {
        for path in [
            "/users/tenant/alice/source-access",
            "/nodes/n1/source-access",
        ] {
            for method in [axum::http::Method::GET, axum::http::Method::PUT] {
                assert!(!public_may(&method, path));
                assert!(!user_may(&method, path));
            }
        }
    }
}
