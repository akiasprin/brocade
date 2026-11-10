//! Small control-plane mutations which intentionally use HTTP rather than live-channel RPC.
use super::*;

pub(crate) fn control_client(maximum_idle: usize) -> Result<reqwest::Client, String> {
    client::asynchronous_control(maximum_idle)
}

impl HttpClient {
    /// The absolute deadline includes the caller's bounded queue as well as the complete body.
    pub(crate) async fn post_json(
        &self,
        client: &reqwest::Client,
        path: &str,
        token: &str,
        body: String,
        deadline: Instant,
        maximum_body: usize,
    ) -> Result<HttpResponse, String> {
        let started = Instant::now();
        let mut progress = diagnostic::Progress::new(started);
        let request = async {
            let mut response = client
                .post(format!("{}{}", self.channel_origin(), path))
                .bearer_auth(token)
                .header(
                    "X-Brocade-Protocol-Version",
                    brocade_deployment::protocol::AGENT_PROTOCOL_VERSION,
                )
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .body(body)
                .timeout(remaining(deadline).map_err(|_| progress.message("request", "timeout"))?)
                .send()
                .await
                .map_err(|error| progress.error("send", &error))?;
            let status = response.status().as_u16();
            progress.status = Some(status);
            progress.content_length = response.content_length();
            if status == 101 {
                return Err(progress.message("headers", "unsupported_upgrade"));
            }
            if progress
                .content_length
                .is_some_and(|size| size > maximum_body as u64)
            {
                return Err(progress.message("headers", "body_exceeds_limit"));
            }
            let headers = response_headers(response.headers(), &progress)?;
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| progress.error("body", &error))?
            {
                progress.received_bytes += chunk.len();
                if chunk.len() > maximum_body.saturating_sub(bytes.len()) {
                    return Err(progress.message("body", "body_exceeds_limit"));
                }
                bytes.extend_from_slice(&chunk);
            }
            let body =
                String::from_utf8(bytes).map_err(|_| progress.message("body", "invalid_utf8"))?;
            Ok(HttpResponse {
                status,
                body,
                headers,
            })
        };
        tokio::time::timeout_at(deadline.into(), request)
            .await
            .map_err(|_| "HTTP asynchronous request deadline exceeded".to_owned())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener, sync::mpsc, thread};

    #[tokio::test]
    async fn asynchronous_posts_keep_prefix_auth_framing_and_bounded_bodies() {
        crate::identity::self_identity();
        let client = control_client(4).unwrap();
        for (wire, expected) in [
            (
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n"
                    .as_slice(),
                true,
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 10000\r\n\r\n".as_slice(),
                false,
            ),
            (
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\n12345678\r\n0\r\n\r\n"
                    .as_slice(),
                false,
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{".as_slice(),
                false,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let origin = format!("http://{}/prefix", listener.local_addr().unwrap());
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let (headers, body) = super::super::tests::read_request(&mut stream);
                assert!(headers.starts_with("post /prefix/budget http/1.1\r\n"));
                assert!(headers.contains("authorization: bearer synthetic-async-token\r\n"));
                assert!(headers.contains("x-brocade-protocol-version:"));
                assert_eq!(body, b"{}");
                stream.write_all(wire).unwrap();
            });
            let response = HttpClient::new(&origin)
                .unwrap()
                .post_json(
                    &client,
                    "/budget",
                    "synthetic-async-token",
                    "{}".into(),
                    Instant::now() + Duration::from_secs(2),
                    4,
                )
                .await;
            assert_eq!(response.is_ok(), expected);
            if expected {
                assert_eq!(response.unwrap().body, "{}");
            }
            server.join().unwrap();
        }
    }

    #[tokio::test]
    async fn asynchronous_posts_bound_stalled_bodies_and_do_not_follow_redirects() {
        crate::identity::self_identity();
        let client = control_client(4).unwrap();
        for redirect in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let (release, wait) = mpsc::channel();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                super::super::tests::read_request(&mut stream);
                let wire = if redirect {
                    b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/secret\r\nContent-Length: 0\r\n\r\n".as_slice()
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n{".as_slice()
                };
                stream.write_all(wire).unwrap();
                wait.recv_timeout(Duration::from_secs(2)).unwrap();
            });
            let response = HttpClient::new(&origin)
                .unwrap()
                .post_json(
                    &client,
                    "/budget",
                    "synthetic-async-token",
                    "{}".into(),
                    Instant::now() + Duration::from_millis(100),
                    4096,
                )
                .await;
            if redirect {
                assert_eq!(response.unwrap().status, 302);
            } else {
                let error = response.unwrap_err();
                assert!(
                    error.contains("timeout") || error.contains("deadline"),
                    "{error}"
                );
                assert!(!error.contains("synthetic-async-token"));
            }
            release.send(()).unwrap();
            server.join().unwrap();
        }
    }
}
