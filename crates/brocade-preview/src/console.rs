use axum::{
    body::{to_bytes, Body},
    extract::{OriginalUri, State},
    http::{header, HeaderMap, HeaderValue, Request, Response, StatusCode},
};
use serde_json::Value;

use crate::{error::PreviewError, AppState};

const MAX_PROXY_BODY_BYTES: usize = 32 * 1024 * 1024;

/// Requests not taken by `/preview/*` are forwarded unchanged to the production console's admin
/// surface.
pub(crate) async fn proxy(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    request: Request<Body>,
) -> Result<Response<Body>, PreviewError> {
    let (parts, body) = request.into_parts();
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    let url = format!("{}{}", state.config.console_url, path_and_query);
    let bytes = to_bytes(body, MAX_PROXY_BODY_BYTES)
        .await
        .map_err(|error| PreviewError::bad_request(format!("read request body: {error}")))?;
    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())
        .map_err(|error| PreviewError::bad_request(format!("invalid method: {error}")))?;
    let mut builder = state.client.request(method, url);
    for (name, value) in parts.headers.iter() {
        if should_forward_header(name.as_str()) {
            builder = builder.header(name.as_str(), value.as_bytes());
        }
    }
    let response = builder
        .body(bytes)
        .send()
        .await
        .map_err(|error| PreviewError::upstream(format!("proxy request failed: {error}")))?;
    response_from_reqwest(response).await
}

fn should_forward_header(name: &str) -> bool {
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "host" | "connection" | "content-length" | "transfer-encoding"
    )
}

async fn response_from_reqwest(
    response: reqwest::Response,
) -> Result<Response<Body>, PreviewError> {
    let status = StatusCode::from_u16(response.status().as_u16())
        .map_err(|error| PreviewError::internal(format!("invalid upstream status: {error}")))?;
    let mut builder = Response::builder().status(status);
    for (name, value) in response.headers() {
        if should_forward_header(name.as_str()) {
            let value = HeaderValue::from_bytes(value.as_bytes()).map_err(|error| {
                PreviewError::internal(format!("invalid upstream header {}: {error}", name))
            })?;
            builder = builder.header(name.as_str(), value);
        }
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| PreviewError::upstream(format!("read upstream body: {error}")))?;
    builder
        .body(Body::from(bytes))
        .map_err(|error| PreviewError::internal(format!("build response: {error}")))
}

pub(crate) async fn get_console_json(
    state: &AppState,
    path: &str,
    headers: &HeaderMap,
) -> Result<Value, String> {
    let mut request = state
        .client
        .get(format!("{}{}", state.config.console_url, path));
    request = copy_auth_headers(request, headers);
    let response = request
        .send()
        .await
        .map_err(|error| format!("console GET {path} failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("console GET {path} returned {}", response.status()));
    }
    response
        .json::<Value>()
        .await
        .map_err(|error| format!("console GET {path} returned invalid JSON: {error}"))
}

pub(crate) async fn post_console_json(
    state: &AppState,
    path: &str,
    headers: &HeaderMap,
    body: &Value,
) -> Result<Value, String> {
    let mut request = state
        .client
        .post(format!("{}{}", state.config.console_url, path));
    request = copy_auth_headers(request, headers).json(body);
    let response = request
        .send()
        .await
        .map_err(|error| format!("console POST {path} failed: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("console POST {path} returned {status}: {text}"));
    }
    response
        .json::<Value>()
        .await
        .map_err(|error| format!("console POST {path} returned invalid JSON: {error}"))
}

// It carries the browser request's identity; preview holds no credential of its own.
fn copy_auth_headers(
    mut request: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    for name in [header::COOKIE, header::AUTHORIZATION] {
        if let Some(value) = headers.get(&name) {
            request = request.header(name.as_str(), value.as_bytes());
        }
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_forward_header_drops_hop_by_hop_headers() {
        assert!(should_forward_header("cookie"));
        assert!(should_forward_header("content-type"));
        assert!(!should_forward_header("host"));
        assert!(!should_forward_header("Host"));
        assert!(!should_forward_header("Content-Length"));
        assert!(!should_forward_header("transfer-encoding"));
    }
}
