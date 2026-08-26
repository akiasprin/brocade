//! The console front end, compiled into this binary.
//!
//! `build.rs` builds `frontend/` and writes the table below; the argument for embedding it rather
//! than reading a directory at runtime is at the top of that file. What lives here is only the
//! serving side: find the file, say what it is, say how long it may be cached, and hand over the
//! compressed copy to whoever can read it.
//!
//! # Caching
//!
//! The two kinds of resource need opposite policies, and getting it wrong has been measured once:
//! the control plane was already returning `warnings: 0` while the UI still showed the count from
//! the old algorithm, and only a hard refresh fixed it.
//!
//! - `index.html`: `no-cache`. Not "do not cache" but "revalidate every time" — it is the table
//!   pointing at the current JS, and a stale table is worse than none.
//! - `assets/*`: the filenames carry a content hash and change when the content does, so they can
//!   be cached long and `immutable`. Together these two are the correct use of vite's hashed
//!   filenames.

use axum::{
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
};

/// One file the console serves.
pub struct ConsoleAsset {
    /// The URL path it answers on, leading slash included — `/assets/index-abc123.js`.
    pub path: &'static str,
    pub content_type: &'static str,
    pub bytes: &'static [u8],
    /// The gzipped copy, where compressing it was worth the bytes it costs in this binary.
    pub gzip: Option<&'static [u8]>,
}

include!(concat!(env!("OUT_DIR"), "/console_assets.rs"));

/// The asset answering a request path, if any.
///
/// A linear scan: this table holds a handful of entries (vite emits one JS, one CSS and the HTML),
/// and an index over three items costs more to explain than it saves.
///
/// `/` is the only path rewritten. The front end routes on the hash fragment (`forge/route.ts`),
/// which the browser never sends, so every path the server sees is a real file — there is no deep
/// link needing to fall back to `index.html`, and answering one with the SPA would turn a mistyped
/// asset URL into a 200 holding HTML, which is how a missing file comes back as `SyntaxError:
/// Unexpected token '<'` instead of a 404.
///
/// The comparison is against the raw request path, with no decoding and no normalizing. That is
/// deliberate: the reachable set is exactly this table, so `..`, symlinks and encoding tricks have
/// nothing to reach for — there is no filesystem behind this. The other half of that bargain is
/// that build.rs refuses to embed a filename a browser would percent-encode, or a request for it
/// would arrive spelled differently from the table and 404 with no explanation.
fn lookup(path: &str) -> Option<&'static ConsoleAsset> {
    let path = if path == "/" { "/index.html" } else { path };
    CONSOLE_ASSETS.iter().find(|asset| asset.path == path)
}

/// Whether this client wants the compressed copy.
///
/// `q=0` counts as a refusal — that is how a client says "anything but this one", and honouring
/// `Accept-Encoding: gzip;q=0` costs one line here against an unreadable page there.
fn accepts_gzip(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    value.split(',').any(|entry| {
        let mut parts = entry.split(';');
        let coding = parts.next().unwrap_or_default().trim();
        if !coding.eq_ignore_ascii_case("gzip") && coding != "*" {
            return false;
        }
        !parts.any(|parameter| {
            let parameter = parameter.trim();
            parameter
                .strip_prefix("q=")
                .or_else(|| parameter.strip_prefix("Q="))
                .and_then(|quality| quality.trim().parse::<f32>().ok())
                .is_some_and(|quality| quality <= 0.0)
        })
    })
}

/// See the caching section at the top of this file.
pub(crate) fn cache_control_for(path: &str) -> &'static str {
    if path.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Serve one file out of the embedded table, or 404.
///
/// Mounted as the fallback, so it sees only what no API route claimed.
pub async fn serve(uri: Uri, headers: HeaderMap) -> Response {
    let Some(asset) = lookup(uri.path()) else {
        // A plain sentence rather than an empty body: this surface is also where an operator lands
        // after mistyping a URL, and a blank page says nothing about which of the two — the front
        // end or the API — was expected to answer.
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "控制面没带这个文件。API 路由都不带 /api 前缀，前端资源只有 / 和 /assets/ 下面那几个。\n",
        )
            .into_response();
    };

    let compressed = asset.gzip.filter(|_| accepts_gzip(&headers));
    let mut response = compressed.unwrap_or(asset.bytes).into_response();
    let out = response.headers_mut();
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(asset.content_type),
    );
    out.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control_for(asset.path)),
    );
    if compressed.is_some() {
        out.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    if asset.gzip.is_some() {
        // Stated whenever a compressed copy exists, not only when one was sent: without it a proxy
        // that cached the compressed answer would hand it to the next client whether or not that
        // client can read gzip.
        out.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    response
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    /// The table has to hold a usable front end. Nothing else in the test suite would notice an
    /// empty one — every route still answers, the console is just blank.
    #[test]
    fn index_and_assets_are_embedded() {
        let index = lookup("/").expect("/ 要给出 index.html");
        assert_eq!(index.path, "/index.html");
        assert_eq!(index.content_type, "text/html; charset=utf-8");
        assert!(
            index.bytes.windows(5).any(|w| w == b"<html"),
            "index.html 应该是 HTML"
        );
        assert!(
            CONSOLE_ASSETS
                .iter()
                .any(|a| a.path.starts_with("/assets/") && a.path.ends_with(".js")),
            "没有 JS 的话是编出了一个空壳（表里有 {} 个文件）",
            CONSOLE_ASSETS.len()
        );
    }

    /// The hashed filenames are the whole reason `immutable` is safe. Handing `index.html` the
    /// same header is the failure this distinction exists to prevent.
    #[test]
    fn only_hashed_assets_are_cached_forever() {
        assert_eq!(cache_control_for("/index.html"), "no-cache");
        assert_eq!(
            cache_control_for("/assets/index-abc123.js"),
            "public, max-age=31536000, immutable"
        );
    }

    #[test]
    fn gzip_is_offered_only_to_clients_that_asked() {
        let header = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_str(value).unwrap(),
            );
            headers
        };
        assert!(accepts_gzip(&header("gzip, deflate, br")));
        assert!(accepts_gzip(&header("br;q=1.0, gzip;q=0.8")));
        assert!(accepts_gzip(&header("*")));
        assert!(!accepts_gzip(&header("br, deflate")));
        assert!(!accepts_gzip(&header("gzip;q=0")));
        assert!(!accepts_gzip(&HeaderMap::new()));
    }

    /// The JS bundle is half a megabyte uncompressed and the control plane is often exposed with
    /// nothing in front of it to compress. If this stops holding, every first visit pays for it.
    ///
    /// Only text is asserted on. build.rs drops the compressed copy where it would not pay for the
    /// bytes it costs, which is the right call for a PNG or a font — requiring one of everything
    /// large would fail this test the day somebody adds an image, over a decision that was correct.
    #[test]
    fn the_big_text_files_carry_a_compressed_copy() {
        let text = CONSOLE_ASSETS
            .iter()
            .filter(|asset| asset.content_type.starts_with("text/"))
            .filter(|asset| asset.bytes.len() > 4096);
        for asset in text {
            let gzip = asset
                .gzip
                .unwrap_or_else(|| panic!("{} 有 {} 字节却没压缩", asset.path, asset.bytes.len()));
            assert!(
                gzip.len() < asset.bytes.len(),
                "{} 压完反而更大",
                asset.path
            );
        }
    }

    /// Sending compressed bytes without saying so is the one way to get this wrong that produces a
    /// blank page and a console full of binary — so the body and the header are asserted together,
    /// in both directions.
    #[tokio::test]
    async fn the_body_matches_what_the_encoding_header_claims() {
        let js = CONSOLE_ASSETS
            .iter()
            .find(|asset| asset.path.ends_with(".js"))
            .expect("前端至少有一个 JS");
        let request = |accept: Option<&str>| {
            let mut headers = HeaderMap::new();
            if let Some(accept) = accept {
                headers.insert(
                    header::ACCEPT_ENCODING,
                    HeaderValue::from_str(accept).unwrap(),
                );
            }
            serve(Uri::from_str(js.path).unwrap(), headers)
        };
        let body = |response: Response| async {
            axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
                .await
                .unwrap()
        };

        let compressed = request(Some("gzip")).await;
        assert_eq!(compressed.headers()[header::CONTENT_ENCODING], "gzip");
        assert_eq!(compressed.headers()[header::VARY], "accept-encoding");
        // Content-Type stays the file's own. Saying `application/gzip` here is the other half of
        // the same mistake: the browser would download the script instead of running it.
        assert_eq!(
            compressed.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        assert_eq!(body(compressed).await.as_ref(), js.gzip.unwrap());

        let plain = request(None).await;
        assert!(!plain.headers().contains_key(header::CONTENT_ENCODING));
        // Vary all the same: a proxy in between must not hand the compressed copy it cached to a
        // client that never asked for one.
        assert_eq!(plain.headers()[header::VARY], "accept-encoding");
        assert_eq!(body(plain).await.as_ref(), js.bytes);
    }
}
