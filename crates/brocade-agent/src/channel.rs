//! A bounded bridge from existing blocking workers to the negotiated live socket.
use crate::{http::HttpResponse, options::Options};
use brocade_deployment::agent_channel::{self, AgentChannelRequest, MAX_IN_FLIGHT};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Mutex, OnceLock,
    },
    time::Instant,
};
use tokio::sync::mpsc as asynchronous;

type Reply = mpsc::SyncSender<Result<Option<HttpResponse>, String>>;
pub(crate) struct Outgoing {
    pub(crate) id: u64,
    pub(crate) allow_http: bool,
    pub(crate) request: AgentChannelRequest,
    pub(crate) deadline: Instant,
    pub(crate) reply: Reply,
}
impl Outgoing {
    /// The caller's deadline includes queueing, serialization and the complete response.
    pub(crate) fn encode(&mut self, now: Instant) -> Result<Option<String>, String> {
        if now >= self.deadline {
            return Ok(None);
        }
        let AgentChannelRequest::Rpc { timeout_millis, .. } = &mut self.request;
        *timeout_millis = self
            .deadline
            .duration_since(now)
            .as_millis()
            .clamp(1, 30_000) as u32;
        serde_json::to_string(&self.request)
            .map(Some)
            .map_err(|_| "cannot encode Agent channel RPC".into())
    }
}
struct Session {
    id: u64,
    origin: String,
    token: String,
    sender: asynchronous::Sender<Outgoing>,
}
static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn session() -> &'static Mutex<Option<Session>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

pub(crate) struct Registration {
    id: u64,
}
impl Drop for Registration {
    fn drop(&mut self) {
        let mut state = session()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.as_ref().is_some_and(|s| s.id == self.id) {
            *state = None;
            crate::wake::set_connected(false);
        }
    }
}
pub(crate) fn register(
    options: &Options,
) -> Result<(Registration, asynchronous::Receiver<Outgoing>), String> {
    let client = crate::http::HttpClient::new(&options.server)?;
    let (sender, receiver) = asynchronous::channel(MAX_IN_FLIGHT);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    *session()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Session {
        id,
        origin: client.channel_origin(),
        token: options.token.clone(),
        sender,
    });
    crate::wake::set_connected(true);
    crate::wake::notify(&agent_channel::WakeDomain::ALL);
    Ok((Registration { id }, receiver))
}

/// None means the request has not been sent, or the server explicitly requested a GET via HTTP.
/// Once queued, transport failures are returned to the original business retry boundary.
pub(crate) fn request(
    origin: &str,
    token: &str,
    method: &str,
    path: &str,
    body: &[u8],
    headers: &[(&str, String)],
    deadline: Instant,
) -> Option<Result<HttpResponse, String>> {
    if !agent_channel::rpc_allowed(method, path)
        || body.len() > agent_channel::MAX_RPC_BODY_BYTES
        || headers.len() > 16
        || headers
            .iter()
            .any(|(name, value)| !agent_channel::request_header_allowed(name) || value.len() > 4096)
    {
        return None;
    }
    let sender = {
        let state = session()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = state.as_ref()?;
        if state.origin != origin || state.token != token {
            return None;
        }
        state.sender.clone()
    };
    let body = std::str::from_utf8(body).ok()?.to_owned();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let request = AgentChannelRequest::Rpc {
        id,
        method: method.into(),
        path: path.into(),
        body,
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
        timeout_millis: deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, 30_000) as u32,
    };
    let text = serde_json::to_string(&request).ok()?;
    if text.len() > agent_channel::MAX_FRAME_BYTES {
        return None;
    }
    let (reply, receive) = mpsc::sync_channel(1);
    if sender
        .try_send(Outgoing {
            id,
            allow_http: method == "GET",
            request,
            deadline,
            reply,
        })
        .is_err()
    {
        return None;
    }
    match receive.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(response)) => response.map(Ok),
        Ok(Err(error)) => Some(Err(error)),
        Err(_) => Some(Err(
            "Agent channel RPC did not complete before its deadline".into(),
        )),
    }
}

pub(crate) struct Pending {
    pub(crate) allow_http: bool,
    pub(crate) deadline: Instant,
    pub(crate) reply: Reply,
}
pub(crate) fn complete(
    pending: &mut BTreeMap<u64, Pending>,
    id: u64,
    response: Option<HttpResponse>,
) {
    if let Some(request) = pending.remove(&id) {
        let result = if response.is_none() && !request.allow_http {
            Err("Agent channel cannot replay an already-sent mutation over HTTP".into())
        } else {
            Ok(response)
        };
        let _ = request.reply.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn queue_time_is_subtracted_from_the_rpc_server_budget() {
        let start = Instant::now();
        let (reply, _) = mpsc::sync_channel(1);
        let mut outgoing = Outgoing {
            id: 1,
            allow_http: true,
            deadline: start + Duration::from_secs(2),
            reply,
            request: AgentChannelRequest::Rpc {
                id: 1,
                method: "GET".into(),
                path: "/agent/v1/desired".into(),
                body: String::new(),
                headers: Vec::new(),
                timeout_millis: 2000,
            },
        };
        let text = outgoing
            .encode(start + Duration::from_millis(1800))
            .unwrap()
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<AgentChannelRequest>(&text).unwrap(),
            AgentChannelRequest::Rpc {
                timeout_millis: 200,
                ..
            }
        ));
        assert!(outgoing
            .encode(start + Duration::from_secs(2))
            .unwrap()
            .is_none());
    }
    #[test]
    fn only_explicit_get_fallbacks_can_be_replayed_over_http() {
        for allow_http in [false, true] {
            let (reply, receive) = mpsc::sync_channel(1);
            let mut pending = BTreeMap::from([(
                1,
                Pending {
                    allow_http,
                    deadline: Instant::now() + Duration::from_secs(1),
                    reply,
                },
            )]);
            complete(&mut pending, 1, None);
            assert_eq!(receive.recv().unwrap().is_ok(), allow_http);
            assert!(pending.is_empty());
        }
    }
    #[tokio::test]
    async fn existing_worker_requests_use_rpc_and_ambiguous_disconnect_is_not_replayed() {
        let options = Options {
            command: "run".into(),
            server: "http://127.0.0.1:9/channel-test".into(),
            token: "channel-test-token".into(),
            state_dir: std::env::temp_dir(),
            apply_mode: crate::ApplyMode::StateDir,
            vpngate_stats_window: Duration::from_secs(900),
        };
        let (guard, mut receiver) = register(&options).unwrap();
        let origin = options.server.clone();
        let token = options.token.clone();
        let request = tokio::task::spawn_blocking(move || {
            crate::http::HttpClient::new(&origin)
                .unwrap()
                .request_timeout(
                    "POST",
                    "/agent/v1/runtime",
                    &token,
                    Some("{}"),
                    Duration::from_secs(3),
                )
        });
        let outgoing = receiver.recv().await.unwrap();
        assert!(matches!(&outgoing.request,
            AgentChannelRequest::Rpc { ref path, .. } if path == "/agent/v1/runtime"));
        outgoing
            .reply
            .send(Ok(Some(HttpResponse {
                status: 200,
                body: "accepted".into(),
                headers: BTreeMap::new(),
            })))
            .unwrap();
        assert_eq!(request.await.unwrap().unwrap().body, "accepted");
        let origin = options.server.clone();
        let token = options.token.clone();
        let request = tokio::task::spawn_blocking(move || {
            crate::http::HttpClient::new(&origin)
                .unwrap()
                .request_timeout(
                    "POST",
                    "/agent/v1/runtime",
                    &token,
                    Some("{}"),
                    Duration::from_secs(3),
                )
        });
        drop(receiver.recv().await.unwrap());
        assert!(request
            .await
            .unwrap()
            .unwrap_err()
            .contains("Agent channel RPC"));
        drop(guard);
        assert!(self::request(
            &options.server,
            &options.token,
            "GET",
            "/agent/v1/desired",
            b"",
            &[],
            Instant::now() + Duration::from_secs(1)
        )
        .is_none());
    }
}
