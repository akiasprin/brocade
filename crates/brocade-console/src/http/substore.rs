use super::*;
use brocade_store::subscription_connector as connector;

fn context() -> ApiResult<AdminContext> {
    request_admin()
        .map(|a| AdminContext::from_authenticated(&a))
        .ok_or(ApiError::Unauthorized)
}
pub(super) fn is_connector_path(method: &axum::http::Method, path: &str) -> bool {
    let p: Vec<_> = path.trim_matches('/').split('/').collect();
    match p.as_slice() {
        ["users", _, _, "subscription-connectors", "sub-store"] => matches!(
            *method,
            axum::http::Method::GET | axum::http::Method::PUT | axum::http::Method::DELETE
        ),
        ["users", _, _, "subscription-connectors", "sub-store", "test" | "sync" | "verify" | "cleanup"] => {
            *method == axum::http::Method::POST
        }
        ["users", _, _, "subscription-connectors", "sub-store", "runs"] => {
            *method == axum::http::Method::GET
        }
        ["users", _, _, "subscription-connectors", "sub-store", "runs", id] => {
            id.parse::<i64>().is_ok()
                && matches!(
                    *method,
                    axum::http::Method::GET | axum::http::Method::DELETE
                )
        }
        ["users", _, _, "subscription-connectors", "sub-store", "runs", id, "retry"] => {
            id.parse::<i64>().is_ok() && *method == axum::http::Method::POST
        }
        _ => false,
    }
}
pub(super) async fn info(
    State(s): State<AppState>,
    Path((t, u)): Path<(String, String)>,
) -> ApiResult<Response> {
    Ok(Json(connector::view(s.store.pool(), &context()?, &t, &u).await?).into_response())
}
pub(super) async fn save(
    State(s): State<AppState>,
    Path((t, u)): Path<(String, String)>,
    Json(input): Json<connector::ConfigWrite>,
) -> ApiResult<Response> {
    if let Some(url) = input.endpoint.as_ref().filter(|s| !s.trim().is_empty()) {
        crate::substore::validate_endpoint(url.trim())?;
    }
    connector::save(s.store.pool(), &context()?, &t, &u, input).await?;
    info(State(s), Path((t, u))).await
}
pub(super) async fn remove(
    State(s): State<AppState>,
    Path((t, u)): Path<(String, String)>,
) -> ApiResult<Response> {
    connector::remove(s.store.pool(), &context()?, &t, &u).await?;
    info(State(s), Path((t, u))).await
}
pub(super) async fn test(
    State(s): State<AppState>,
    Path((t, u)): Path<(String, String)>,
    Json(input): Json<connector::ConfigWrite>,
) -> ApiResult<Response> {
    connector::access(&context()?, &t, &u, true)?;
    let old = connector::credentials(s.store.pool(), &t, &u).await?;
    let endpoint = input
        .endpoint
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().trim_end_matches('/').to_owned())
        .or_else(|| old.as_ref().map(|s| s.endpoint.clone()))
        .ok_or(StoreError::InvalidData("填写后端地址".into()))?;
    crate::substore::validate_endpoint(&endpoint)?;
    let same = old.as_ref().is_some_and(|c| c.endpoint == endpoint);
    let auth = connector::Credentials {
        endpoint,
        login: if input.auth {
            if input.login.is_empty() {
                old.as_ref()
                    .filter(|_| same)
                    .map(|c| c.login.clone())
                    .unwrap_or_default()
            } else {
                input.login
            }
        } else {
            String::new()
        },
        password: if input.auth {
            input
                .password
                .filter(|s| !s.is_empty())
                .or_else(|| old.filter(|_| same).map(|s| s.password))
                .unwrap_or_default()
        } else {
            String::new()
        },
    };
    crate::substore::test(auth).await?;
    Ok(Json(json!({"ok":true})).into_response())
}
pub(super) async fn sync(
    State(s): State<AppState>,
    Path((t, u)): Path<(String, String)>,
) -> ApiResult<Response> {
    enqueue(s, t, u, "sync", None).await
}
pub(super) async fn verify(
    State(s): State<AppState>,
    Path((t, u)): Path<(String, String)>,
) -> ApiResult<Response> {
    enqueue(s, t, u, "verify", None).await
}
pub(super) async fn cleanup(
    State(s): State<AppState>,
    Path((t, u)): Path<(String, String)>,
) -> ApiResult<Response> {
    enqueue(s, t, u, "cleanup", None).await
}
async fn enqueue(
    s: AppState,
    t: String,
    u: String,
    kind: &str,
    parent: Option<i64>,
) -> ApiResult<Response> {
    let id = connector::enqueue(
        s.store.pool(),
        &context()?,
        &t,
        &u,
        kind,
        parent,
        "手动操作",
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(json!({"id":id}))).into_response())
}
#[derive(Default, Deserialize)]
pub(super) struct PageQuery {
    before: Option<i64>,
    #[serde(default)]
    notable: bool,
    scope: Option<connector::RunScope>,
}
pub(super) async fn list(
    State(s): State<AppState>,
    Path((t, u)): Path<(String, String)>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Response> {
    let scope = q.scope.unwrap_or(if q.notable {
        connector::RunScope::Errors
    } else {
        connector::RunScope::All
    });
    Ok(
        Json(connector::list_scoped(s.store.pool(), &context()?, &t, &u, q.before, scope).await?)
            .into_response(),
    )
}
pub(super) async fn run(
    State(s): State<AppState>,
    Path((t, u, id)): Path<(String, String, i64)>,
) -> ApiResult<Response> {
    Ok(Json(connector::run(s.store.pool(), &context()?, &t, &u, id).await?).into_response())
}
pub(super) async fn cancel(
    State(s): State<AppState>,
    Path((t, u, id)): Path<(String, String, i64)>,
) -> ApiResult<Response> {
    connector::cancel(s.store.pool(), &context()?, &t, &u, id).await?;
    run(State(s), Path((t, u, id))).await
}
pub(super) async fn retry(
    State(s): State<AppState>,
    Path((t, u, id)): Path<(String, String, i64)>,
) -> ApiResult<Response> {
    let r = connector::run(s.store.pool(), &context()?, &t, &u, id).await?;
    if !matches!(r.status.as_str(), "failed" | "partial" | "canceled") {
        return Err(StoreError::InvalidData("该任务不需要重试".into()).into());
    }
    let kind = if r.status == "partial" {
        "verify"
    } else {
        &r.kind
    };
    enqueue(s, t, u, kind, Some(id)).await
}

pub(super) fn is_for_self(
    admin: &AuthenticatedAdmin,
    method: &axum::http::Method,
    path: &str,
) -> bool {
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    is_connector_path(method, path)
        && admin.self_user.as_ref().is_some_and(|u| {
            parts.get(1) == Some(&u.tenant_id.as_str()) && parts.get(2) == Some(&u.user_id.as_str())
        })
}
