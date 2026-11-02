use super::*;
use crate::observations::{ObservationDomain, ObservationEvents};

pub(super) async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(stream_response(
        state.realtime.observations.clone(),
        state.shutdown,
    ))
}

fn stream_response(
    events: ObservationEvents,
    shutdown: crate::lifecycle::ShutdownSignal,
) -> Response {
    // Subscribe before the first frame: reconnecting clients refresh their active read queries,
    // and any report committed during that read remains visible as a subsequent invalidation.
    let mut receiver = events.subscribe();
    let mut previous = *receiver.borrow_and_update();
    let stream = async_stream::stream! {
        yield Ok::<Event, Infallible>(Event::default().event("ready").data("{}"));
        let reauthorize = tokio::time::sleep(Duration::from_secs(60));
        tokio::pin!(reauthorize);
        loop {
            tokio::select! {
                _ = shutdown.requested() => break,
                _ = &mut reauthorize => {
                    yield Ok(Event::default().event("reset").data("authorization_refresh"));
                    break;
                }
                result = receiver.changed() => {
                    if result.is_err() { break; }
                    // Coalesce a fleet reporting together before serializing an SSE frame.
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    let next = *receiver.borrow_and_update();
                    let domains = ObservationDomain::ALL.into_iter()
                        .filter(|domain| next[*domain as usize] != previous[*domain as usize])
                        .collect::<Vec<_>>();
                    previous = next;
                    yield Ok(Event::default().event("invalidate").json_data(domains)
                        .expect("observation domain names are JSON serializable"));
                }
            }
        }
    };
    let mut response = Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

/// Both HTTP and negotiated WebSocket RPC pass through these authenticated handlers. Publish
/// only after success, when the transaction is committed; hints contain no tenant or asset data.
pub(super) async fn after_report(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let domains = changed_domains(request.method(), request.uri().path());
    let response = next.run(request).await;
    if response.status().is_success() {
        state.realtime.observations.changed(domains);
    }
    response
}

fn changed_domains(method: &axum::http::Method, path: &str) -> &'static [ObservationDomain] {
    use ObservationDomain::*;
    if *method == axum::http::Method::GET {
        return match path {
            "/agent/v1/desired" => &[Nodes],
            _ => &[],
        };
    }
    if !matches!(
        *method,
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::PATCH
            | axum::http::Method::DELETE
    ) {
        return &[];
    }
    match path {
        "/agent/v1/load" => &[Load],
        "/agent/v1/e2e-probe" => &[E2e],
        "/agent/v1/unlock-probe/report" => &[Unlock],
        "/agent/v1/usage" | "/agent/v1/forwarding/usage" => &[Traffic],
        "/agent/v1/runtime" | "/agent/v1/observation" => &[Nodes, Traffic],
        "/agent/v1/public-ip-observation" | "/agent/v1/enroll" => &[Nodes],
        "/agent/v1/agent-release/report" | "/agent/v1/xray-release/report" => &[Nodes, Deployments],
        _ if path.ends_with("/unlock-probe") => &[Unlock],
        _ if path.contains("/tunnel-probe") => &[TunnelProbes],
        _ if path.starts_with("/deployments") => &[Nodes, Deployments],
        _ if path == "/nodes" || path.starts_with("/nodes/") => &[Nodes, Traffic],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;
    use futures_util::StreamExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn successful_reports_invalidate_but_failed_reports_and_reads_do_not() {
        let state = AppState::new(PgStore::from_pool(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://unused:unused@localhost/unused")
                .unwrap(),
        ));
        let mut receiver = state.realtime.observations.subscribe();
        let app = Router::new()
            .route(
                "/agent/v1/e2e-probe",
                post(|| async { StatusCode::NO_CONTENT }),
            )
            .route("/agent/v1/load", post(|| async { StatusCode::BAD_REQUEST }))
            .route("/nodes/agent-state", get(|| async { StatusCode::OK }))
            .layer(axum::middleware::from_fn_with_state(state, after_report));
        for (method, path) in [
            (Method::POST, "/agent/v1/load"),
            (Method::GET, "/nodes/agent-state"),
        ] {
            app.clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(!receiver.has_changed().unwrap());
        }
        app.oneshot(
            axum::http::Request::post("/agent/v1/e2e-probe")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        receiver.changed().await.unwrap();
        assert_eq!(receiver.borrow()[ObservationDomain::E2e as usize], 1);
    }

    #[tokio::test]
    async fn stream_starts_ready_and_only_transmits_domain_names() {
        let events = ObservationEvents::default();
        let response = stream_response(events.clone(), crate::lifecycle::ShutdownSignal::default());
        assert_eq!(response.headers()["x-accel-buffering"], "no");
        let mut body = response.into_body().into_data_stream();
        let first = body.next().await.unwrap().unwrap();
        assert!(String::from_utf8(first.to_vec())
            .unwrap()
            .contains("event: ready"));
        events.changed(&[ObservationDomain::Nodes, ObservationDomain::E2e]);
        let frame = body.next().await.unwrap().unwrap();
        let frame = String::from_utf8(frame.to_vec()).unwrap();
        assert!(frame.contains("event: invalidate"));
        assert!(frame.contains("[\"nodes\",\"e2e\"]"));
    }
}
