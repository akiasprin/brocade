//! brocade's local preview environment.
//!
//! It replaces exactly one step — somebody copying the install command onto a machine and running
//! it. `/preview/*` is orchestrated here and every other path is proxied unchanged to the
//! production console. The control plane, the compiler, and the agent know nothing of preview.
mod addr;
mod config;
mod console;
mod docker;
mod error;
mod node;
mod param;
mod probe;
mod provision;

use axum::{
    routing::{any, delete, get, post},
    Router,
};
use reqwest::Client;

use crate::config::Config;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState {
        config: Config::from_env()?,
        client: Client::new(),
    };
    let bind = state.config.bind;
    let app = Router::new()
        .route("/preview/status", get(node::preview_status))
        .route(
            "/preview/dist/brocade-agent",
            get(node::preview_agent_binary),
        )
        .route("/preview/nodes", post(node::preview_node))
        .route(
            "/preview/nodes/{node_id}/logs",
            get(node::preview_node_logs),
        )
        .route(
            "/preview/nodes/{node_id}",
            delete(node::preview_node_delete),
        )
        .route(
            "/preview/verify/subscription",
            post(probe::preview_verify_subscription),
        )
        .fallback(any(console::proxy))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    eprintln!("brocade-preview listening on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Clone)]
pub(crate) struct AppState {
    config: Config,
    client: Client,
}
