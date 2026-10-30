//! Additive, negotiated RPCs on the always-connected Agent channel.
use serde::{Deserialize, Serialize};

pub const CAPABILITY_HEADER: &str = "x-brocade-channel-rpc";
pub const CHANNEL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 512 * 1024;
pub const MAX_RPC_BODY_BYTES: usize = 128 * 1024;
pub const MAX_IN_FLIGHT: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(usize)]
pub enum WakeDomain {
    Desired,
    AgentRelease,
    XrayRelease,
    SourceAccess,
    TrafficLimit,
    UserTraffic,
    Probe,
    UnlockProbe,
    ForwardingProbe,
}
impl WakeDomain {
    pub const ALL: [Self; 9] = [
        Self::Desired,
        Self::AgentRelease,
        Self::XrayRelease,
        Self::SourceAccess,
        Self::TrafficLimit,
        Self::UserTraffic,
        Self::Probe,
        Self::UnlockProbe,
        Self::ForwardingProbe,
    ];
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentChannelRequest {
    Rpc {
        id: u64,
        method: String,
        path: String,
        body: String,
        headers: Vec<(String, String)>,
        timeout_millis: u32,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentChannelResponse {
    Ready {
        version: u16,
    },
    Wake {
        domains: Vec<WakeDomain>,
    },
    RpcResult {
        id: u64,
        status: u16,
        body: String,
        headers: Vec<(String, String)>,
    },
    /// Large GET results use the original bounded HTTP path. No POST is replayed this way.
    HttpRequired {
        id: u64,
    },
}

/// A finite surface: no arbitrary URL, enrollment, accounting, budget or binary transfer.
pub fn rpc_allowed(method: &str, path: &str) -> bool {
    let (path, query) = path
        .split_once('?')
        .map_or((path, None), |(p, q)| (p, Some(q)));
    if let Some(query) = query {
        if path != "/agent/v1/source-access"
            || method != "GET"
            || query
                .strip_prefix("after_revision=")
                .is_none_or(|value| value.parse::<u64>().is_err())
        {
            return false;
        }
    }
    match method {
        "GET" => matches!(
            path,
            "/agent/v1/desired"
                | "/agent/v1/agent-release"
                | "/agent/v1/xray-release"
                | "/agent/v1/source-access"
                | "/agent/v1/traffic-limit"
                | "/agent/v1/user-traffic/policies"
                | "/agent/v1/forwarding/desired"
                | "/agent/v1/probe-targets"
                | "/agent/v1/e2e-targets"
                | "/agent/v1/ping-probe-targets"
                | "/agent/v1/unlock-probe/target"
                | "/agent/v1/forwarding/tcp-ping-target"
        ),
        "POST" => matches!(
            path,
            "/agent/v1/agent-release/report"
                | "/agent/v1/xray-release/report"
                | "/agent/v1/source-access"
                | "/agent/v1/traffic-limit"
                | "/agent/v1/user-traffic/policies/ack"
                | "/agent/v1/forwarding/reconcile"
                | "/agent/v1/observation"
                | "/agent/v1/runtime"
                | "/agent/v1/load"
                | "/agent/v1/link-health"
                | "/agent/v1/public-ip-observation"
                | "/agent/v1/link-probe"
                | "/agent/v1/e2e-probe"
                | "/agent/v1/ping-probe"
                | "/agent/v1/unlock-probe/report"
                | "/agent/v1/forwarding/tcp-ping-report"
                | "/agent/v1/xray-handoff/force"
        ),
        _ => false,
    }
}

pub fn request_header_allowed(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "x-brocade-arch"
            | "x-brocade-agent-release-receipt"
            | "x-brocade-route-ipv4"
            | "x-brocade-route-ipv6"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rpc_surface_is_explicit_and_queries_are_bounded() {
        assert!(rpc_allowed(
            "GET",
            "/agent/v1/source-access?after_revision=12"
        ));
        for path in [
            "/agent/v1/enroll",
            "/agent/v1/usage",
            "/agent/v1/user-traffic/budget",
            "https://attacker/agent/v1/runtime",
            "/agent/v1/source-access?after_revision=1&x=2",
        ] {
            assert!(!rpc_allowed("POST", path));
            assert!(!rpc_allowed("GET", path));
        }
        assert!(!request_header_allowed("authorization"));
        assert!(!request_header_allowed("x-brocade-protocol-version"));
    }
}
