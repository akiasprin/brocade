//! Bound wire costs without changing credentials, streaming or binary-download semantics.
use super::*;
use axum::body::HttpBody;
use flate2::{write::GzEncoder, Compression};
use std::io::Write;
use tokio::sync::Semaphore;

const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
static COMPRESSORS: Semaphore = Semaphore::const_new(2);

fn compressible_read(path: &str) -> bool {
    matches!(
        path,
        "/nodes/agent-state" | "/node-traffic" | "/probes/e2e" | "/agent/v1/vpngate/desired"
    ) || ["/load/", "/usage/", "/links/", "/ping-probe/"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
        || (path.starts_with("/realtime/") && path.ends_with("/snapshot"))
}

pub(super) async fn compress_json(request: Request, next: Next) -> Response {
    let eligible =
        request.method() == axum::http::Method::GET && compressible_read(request.uri().path());
    let accepts_gzip = crate::assets::accepts_gzip(request.headers());
    let mut response = next.run(request).await;
    if !eligible
        || !response.status().is_success()
        || response.headers().contains_key(header::CONTENT_ENCODING)
        || response.headers().contains_key(header::SET_COOKIE)
        || response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| value.split(';').next() != Some("application/json"))
    {
        return response;
    }
    response
        .headers_mut()
        .append(header::VARY, HeaderValue::from_static("accept-encoding"));
    if !accepts_gzip
        || response
            .body()
            .size_hint()
            .exact()
            .is_none_or(|size| !(1024..=MAX_JSON_BYTES as u64).contains(&size))
    {
        return response;
    }
    // Saturation serves identity bytes rather than queueing unbounded compression work.
    let Ok(permit) = COMPRESSORS.try_acquire() else {
        return response;
    };
    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_JSON_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot read JSON response",
            )
                .into_response()
        }
    };
    let input = bytes.clone();
    let compressed = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&input)?;
        encoder.finish()
    })
    .await;
    match compressed {
        Ok(Ok(compressed)) if compressed.len() < bytes.len() => {
            parts.headers.remove(header::CONTENT_LENGTH);
            parts
                .headers
                .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
            Response::from_parts(parts, Body::from(compressed))
        }
        Ok(Ok(_)) => Response::from_parts(parts, Body::from(bytes)),
        _ => {
            eprintln!("JSON response compression failed");
            Response::from_parts(parts, Body::from(bytes))
        }
    }
}

/// Weak validators identify the JSON state across identity and gzip representations.
/// Authentication and fresh desired-state construction always run before this comparison.
pub(super) fn conditional_json(value: &impl Serialize, request: &HeaderMap) -> ApiResult<Response> {
    let bytes = serde_json::to_vec(value).map_err(StoreError::from)?;
    let etag = format!("W/\"{}\"", sha256_hex(&bytes));
    let unchanged = request
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|candidate| {
            let candidate = candidate.trim();
            candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == &etag[2..]
        });
    let mut response = if unchanged {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let mut response = Body::from(bytes).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        response
    };
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).expect("a hex SHA-256 validator is a valid header"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    response
        .headers_mut()
        .append(header::VARY, HeaderValue::from_static("accept-encoding"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tower::ServiceExt;

    #[tokio::test]
    async fn conditional_state_sends_no_body_until_state_changes() {
        let state = json!({"generation": 1, "candidates": ["fixture"]});
        let original = conditional_json(&state, &HeaderMap::new()).unwrap();
        assert_eq!(original.status(), StatusCode::OK);
        let etag = original.headers()[header::ETAG].clone();
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag);
        let unchanged = conditional_json(&state, &headers).unwrap();
        assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
        assert!(axum::body::to_bytes(unchanged.into_body(), 1)
            .await
            .unwrap()
            .is_empty());
        let changed = conditional_json(&json!({"generation": 2}), &headers).unwrap();
        assert_eq!(changed.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(changed.into_body(), 1024)
                .await
                .unwrap(),
            r#"{"generation":2}"#
        );
    }

    #[tokio::test]
    async fn only_negotiated_observation_json_is_compressed_and_streams_and_auth_are_untouched() {
        let payload = json!({"nodes": "fixture".repeat(2048)});
        let app = Router::new()
            .route(
                "/nodes/agent-state",
                get({
                    let payload = payload.clone();
                    move || async move { Json(payload) }
                }),
            )
            .route(
                "/auth/login",
                get({
                    let payload = payload.clone();
                    move || async move { Json(payload) }
                }),
            )
            .route(
                "/realtime/nodes/events",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        "data: fixture\n\n",
                    )
                }),
            )
            .layer(axum::middleware::from_fn(compress_json));
        for (path, encoding, expected) in [
            ("/nodes/agent-state", "gzip", true),
            ("/nodes/agent-state", "identity", false),
            ("/nodes/agent-state", "gzip;q=0, *;q=1", false),
            ("/auth/login", "gzip", false),
            ("/realtime/nodes/events", "gzip", false),
        ] {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::get(path)
                        .header(header::ACCEPT_ENCODING, encoding)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.headers().contains_key(header::CONTENT_ENCODING),
                expected
            );
            let body = axum::body::to_bytes(response.into_body(), MAX_JSON_BYTES)
                .await
                .unwrap();
            if expected {
                assert!(body.len() < 1024);
                let mut decoded = Vec::new();
                flate2::read::GzDecoder::new(body.as_ref())
                    .read_to_end(&mut decoded)
                    .unwrap();
                assert_eq!(serde_json::from_slice::<Value>(&decoded).unwrap(), payload);
            }
        }
    }
}
