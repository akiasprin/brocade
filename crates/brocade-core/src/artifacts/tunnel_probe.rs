//! Deterministic Xray input for probing exactly one external outbound.
//!
//! This artifact is intentionally smaller than a node artifact. It has one loopback SOCKS
//! inbound, one externally managed outbound, and one explicit routing rule joining the two. In
//! particular it never contains a Freedom outbound: a successful request must prove that the
//! selected tunnel carried it.

use crate::{
    artifacts::xray::XrayOutbound,
    model::{ExternalOutbound, ExternalOutboundProtocol},
};

pub const INBOUND_TAG: &str = "probe-in";
pub const OUTBOUND_TAG: &str = "probe-out";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelProbeArtifact {
    pub socks_port: u16,
    pub outbound: XrayOutbound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelProbeBuildError {
    ManagedWarpNeedsDedicatedIdentity,
}

impl std::fmt::Display for TunnelProbeBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ManagedWarpNeedsDedicatedIdentity => {
                f.write_str("Console 拨测暂不支持 WARP：需要独立的 Console 身份")
            }
        }
    }
}

impl std::error::Error for TunnelProbeBuildError {}

pub fn build(
    outbound: &ExternalOutbound,
    socks_port: u16,
) -> Result<TunnelProbeArtifact, TunnelProbeBuildError> {
    if matches!(outbound.protocol, ExternalOutboundProtocol::Warp { .. }) {
        return Err(TunnelProbeBuildError::ManagedWarpNeedsDedicatedIdentity);
    }

    Ok(TunnelProbeArtifact {
        socks_port,
        outbound: XrayOutbound::External {
            tag: OUTBOUND_TAG.to_owned(),
            address: outbound.address.clone(),
            port: outbound.port,
            protocol: outbound.protocol.clone(),
            security: outbound.security.clone(),
            // Ordinary WireGuard tunnels do not have the per-machine worker override used by
            // managed WARP bindings. Zero delegates to Xray's bounded automatic choice.
            wireguard_workers: 0,
        },
    })
}
