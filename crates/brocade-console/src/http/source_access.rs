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

fn source_addresses(view: &SourceAccessView) -> Vec<String> {
    view.ips
        .iter()
        .chain(view.user_bans.iter().flatten().flat_map(|user| &user.ips))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Include both scopes: a user-only denial may never have appeared in presence or the node list.
async fn located(state: &AppState, view: SourceAccessView) -> ApiResult<Response> {
    let addresses = source_addresses(&view);
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
    use brocade_deployment::source_access::SourceAccessUser;

    #[test]
    fn location_addresses_include_user_only_bans_and_deduplicate_scopes() {
        let mut view = SourceAccessView {
            revision: 7,
            ips: vec!["192.0.2.1".into()],
            nodes: vec![],
            user_bans: Some(vec![SourceAccessUser {
                tenant_id: "platform".into(),
                user_id: "alice".into(),
                ips: vec!["192.0.2.1".into(), "2001:db8::1".into()],
            }]),
        };
        assert_eq!(source_addresses(&view), ["192.0.2.1", "2001:db8::1"]);
        view.ips.clear();
        assert_eq!(source_addresses(&view), ["192.0.2.1", "2001:db8::1"]);
        view.user_bans = None;
        assert!(source_addresses(&view).is_empty());
    }

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
