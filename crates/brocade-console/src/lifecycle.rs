use std::{future::IntoFuture, io, sync::Arc, time::Duration};

use axum::Router;
use tokio::{net::TcpListener, sync::watch};

/// The application gets less time than systemd's outer stop deadline. Normal requests may drain
/// inside this window; a forgotten stream or handler may not keep the process alive indefinitely.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct ShutdownSignal {
    inner: Arc<ShutdownSignalInner>,
}

struct ShutdownSignalInner {
    requested: watch::Sender<bool>,
}

impl Default for ShutdownSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownSignal {
    pub fn new() -> Self {
        let (requested, _) = watch::channel(false);
        Self {
            inner: Arc::new(ShutdownSignalInner { requested }),
        }
    }

    pub fn request(&self) {
        self.inner.requested.send_replace(true);
    }

    pub fn is_requested(&self) -> bool {
        *self.inner.requested.borrow()
    }

    pub async fn requested(&self) {
        let mut receiver = self.inner.requested.subscribe();
        if *receiver.borrow() {
            return;
        }
        let _ = receiver.changed().await;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownOutcome {
    Drained,
    DeadlineExceeded,
}

impl ShutdownOutcome {
    fn combine(self, other: Self) -> Self {
        if self == Self::DeadlineExceeded || other == Self::DeadlineExceeded {
            Self::DeadlineExceeded
        } else {
            Self::Drained
        }
    }
}

/// Serve the same one- or two-listener layout used by the production binary.
///
/// Axum's graceful shutdown deliberately has no deadline: it stops accepting and then waits for
/// every HTTP response body. That is correct for ordinary requests but an accidental infinite
/// body, long poll, or handler would otherwise make systemd kill the process at its outer timeout.
/// This boundary gives cooperative work a grace window and then returns from the application even
/// when a future endpoint forgot to observe [`ShutdownSignal`]. Returning lets the Tokio runtime
/// drop its remaining connection tasks before the process exits.
pub async fn serve_http_surfaces(
    admin: (TcpListener, Router),
    agent: Option<(TcpListener, Router)>,
    shutdown: ShutdownSignal,
    grace: Duration,
) -> io::Result<ShutdownOutcome> {
    match agent {
        Some(agent) => {
            let (admin, agent) = tokio::try_join!(
                serve_http_surface(admin.0, admin.1, shutdown.clone(), grace),
                serve_http_surface(agent.0, agent.1, shutdown, grace),
            )?;
            Ok(admin.combine(agent))
        }
        None => serve_http_surface(admin.0, admin.1, shutdown, grace).await,
    }
}

async fn serve_http_surface(
    listener: TcpListener,
    router: Router,
    shutdown: ShutdownSignal,
    grace: Duration,
) -> io::Result<ShutdownOutcome> {
    let graceful_signal = shutdown.clone();
    let deadline_signal = shutdown.clone();
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            graceful_signal.requested().await;
        })
        .into_future();
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => {
            result?;
            Ok(ShutdownOutcome::Drained)
        }
        _ = async move {
            deadline_signal.requested().await;
            tokio::time::sleep(grace).await;
        } => Ok(ShutdownOutcome::DeadlineExceeded),
    }
}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, future::pending, sync::Arc, time::Duration};

    use axum::{
        body::{Body, Bytes},
        extract::State,
        response::Response,
        routing::get,
        Router,
    };
    use futures_util::stream;
    use tokio::{net::TcpListener, sync::Notify};

    use super::{serve_http_surfaces, ShutdownOutcome, ShutdownSignal};

    #[derive(Clone, Default)]
    struct TestState {
        finite_entered: Arc<Notify>,
        release_finite: Arc<Notify>,
        handler_entered: Arc<Notify>,
        body_entered: Arc<Notify>,
    }

    async fn finite(State(state): State<TestState>) -> &'static str {
        state.finite_entered.notify_one();
        state.release_finite.notified().await;
        "completed"
    }

    async fn pending_handler(State(state): State<TestState>) -> Response {
        state.handler_entered.notify_one();
        pending::<Response>().await
    }

    async fn pending_body(State(state): State<TestState>) -> Response {
        state.body_entered.notify_one();
        let body = Body::from_stream(stream::pending::<Result<Bytes, Infallible>>());
        Response::new(body)
    }

    /// This is intentionally not an SSE test. The two stubborn routes model any future handler
    /// that never returns and any future response body that never finishes. The assertion lives at
    /// the listener lifecycle boundary, so adding a new streaming protocol cannot weaken it.
    #[tokio::test]
    async fn shutdown_drains_finite_requests_but_has_a_hard_deadline_for_unknown_work() {
        let state = TestState::default();
        let router = Router::new()
            .route("/finite", get(finite))
            .route("/pending-handler", get(pending_handler))
            .route("/pending-body", get(pending_body))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = ShutdownSignal::new();
        let server_shutdown = shutdown.clone();
        let server = tokio::spawn(async move {
            serve_http_surfaces(
                (listener, router),
                None,
                server_shutdown,
                Duration::from_millis(250),
            )
            .await
            .unwrap()
        });

        let client = reqwest::Client::new();
        let finite_client = client.clone();
        let finite = tokio::spawn(async move {
            finite_client
                .get(format!("http://{address}/finite"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        });
        state.finite_entered.notified().await;

        let handler_client = client.clone();
        let pending_handler = tokio::spawn(async move {
            let _ = handler_client
                .get(format!("http://{address}/pending-handler"))
                .send()
                .await;
        });
        state.handler_entered.notified().await;

        let pending_body = client
            .get(format!("http://{address}/pending-body"))
            .send()
            .await
            .unwrap();
        state.body_entered.notified().await;

        shutdown.request();
        state.release_finite.notify_one();

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), finite)
                .await
                .expect("a finite in-flight request should finish inside the grace period")
                .unwrap(),
            "completed"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .expect("unknown non-terminating work must not hold the server forever")
                .unwrap(),
            ShutdownOutcome::DeadlineExceeded
        );

        drop(pending_body);
        pending_handler.abort();
    }

    #[tokio::test]
    async fn shutdown_reports_drained_when_all_in_flight_work_finishes() {
        let state = TestState::default();
        let router = Router::new()
            .route("/finite", get(finite))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = ShutdownSignal::new();
        let server_shutdown = shutdown.clone();
        let server = tokio::spawn(async move {
            serve_http_surfaces(
                (listener, router),
                None,
                server_shutdown,
                Duration::from_secs(1),
            )
            .await
            .unwrap()
        });

        let request = tokio::spawn(async move {
            reqwest::get(format!("http://{address}/finite"))
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        });
        state.finite_entered.notified().await;
        shutdown.request();
        state.release_finite.notify_one();

        assert_eq!(request.await.unwrap(), "completed");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .expect("a drained server should return without waiting for its deadline")
                .unwrap(),
            ShutdownOutcome::Drained
        );
    }
}
