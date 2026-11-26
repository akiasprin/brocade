//! Persistent Guest terminal transport, independent of desired-state convergence.
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "2";
pub const VERSION_HEADER: &str = "x-brocade-terminal-version";
pub const MAX_DATA: usize = 16 * 1024;
pub const MAX_CONTROL: usize = 2048;
pub use cube_terminal::{SessionInfo, HISTORY_BYTES};
pub const MAX_PER_NODE: usize = cube_terminal::MAX_SESSIONS;
/// A handover briefly needs both old and new transports for the same process.
pub const MAX_ATTACHMENTS_PER_NODE: usize = MAX_PER_NODE * 2;
pub const MAX_SESSIONS: usize = 128;
pub const MAX_SECONDS: u64 = 7200;
/// Negotiated independently; legacy v2 peers never receive shortcut messages.
pub const RTC_VERSION: &str = "1";
pub const RTC_HEADER: &str = "x-brocade-terminal-webrtc";
pub const TRICKLE_VERSION: &str = "1";
pub const TRICKLE_HEADER: &str = "x-brocade-terminal-trickle";
pub const MAX_ICE_CANDIDATES: usize = 32;
pub const MAX_ICE_CANDIDATE_BYTES: usize = 1024;
pub const TRICKLE_CONNECT_SECONDS: u64 = 15;
/// Independently negotiated so old Agents never receive application probes.
pub const LATENCY_VERSION: &str = "1";
pub const LATENCY_HEADER: &str = "x-brocade-terminal-latency";
pub const MAX_SDP: usize = 32 * 1024;
pub const MAX_SIGNAL: usize = 64 * 1024;
pub const RTC_CONNECT_SECONDS: u64 = 8;
pub const LEASE_SECONDS: u64 = 5;
/// Two seconds of Console-side browser liveness plus three seconds at the
/// Agent bound a silent browser/Console partition to five seconds in total.
pub const CONTROL_FRESH_SECONDS: u64 = 2;
pub const AGENT_LEASE_SECONDS: u64 = LEASE_SECONDS - CONTROL_FRESH_SECONDS;
pub const DATA_CHANNEL_LABEL: &str = "brocade-terminal-v1";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RtcSettings {
    pub supported: bool,
    pub stun_servers: Vec<String>,
    #[serde(default)]
    pub trickle: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RtcOffer {
    pub sdp: String,
    pub stun_servers: Vec<String>,
    #[serde(default)]
    pub trickle: bool,
}

/// Signaling is scoped to one authenticated attachment and one ICE generation.
/// These frames never enter a terminal's Input stream.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IceSignal {
    IceCandidate {
        candidate: String,
        username_fragment: String,
    },
    IceComplete {
        username_fragment: String,
    },
}

pub fn ice_username_fragment(sdp: &str) -> Option<&str> {
    sdp.lines()
        .filter_map(|line| line.strip_prefix("a=ice-ufrag:"))
        .next_back()
        .filter(|s| {
            (4..=256).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+/".contains(&b))
        })
}

pub struct IceCandidates {
    username_fragment: String,
    seen: std::collections::BTreeSet<String>,
    complete: bool,
}

impl IceCandidates {
    pub fn from_sdp(sdp: &str) -> Result<Self, &'static str> {
        let username_fragment = ice_username_fragment(sdp)
            .ok_or("ICE generation is missing or invalid")?
            .to_owned();
        let seen: std::collections::BTreeSet<_> = sdp
            .lines()
            .filter_map(|line| {
                line.strip_prefix("a=")
                    .filter(|s| s.starts_with("candidate:"))
            })
            .map(str::to_owned)
            .collect();
        if seen.len() > MAX_ICE_CANDIDATES {
            return Err("too many ICE candidates");
        }
        Ok(Self {
            username_fragment,
            seen,
            complete: sdp.lines().any(|line| line == "a=end-of-candidates"),
        })
    }

    /// False is a duplicate. New candidates after completion and stale generations fail closed.
    pub fn accept(&mut self, signal: &IceSignal) -> Result<bool, &'static str> {
        let (username_fragment, candidate) = match signal {
            IceSignal::IceCandidate {
                candidate,
                username_fragment,
            } => (username_fragment, Some(candidate)),
            IceSignal::IceComplete { username_fragment } => (username_fragment, None),
        };
        if username_fragment != &self.username_fragment {
            return Err("stale ICE generation");
        }
        if let Some(candidate) = candidate {
            if !candidate.starts_with("candidate:")
                || candidate.len() > MAX_ICE_CANDIDATE_BYTES
                || !candidate.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
            {
                return Err("invalid ICE candidate");
            }
            if self.seen.contains(candidate) {
                return Ok(false);
            }
            if self.complete {
                return Err("ICE candidates are complete");
            }
            if self.seen.len() >= MAX_ICE_CANDIDATES {
                return Err("too many ICE candidates");
            }
            self.seen.insert(candidate.clone());
            Ok(true)
        } else {
            let changed = !self.complete;
            self.complete = true;
            Ok(changed)
        }
    }
}

/// The original Offer remains intact for existing execution backends and v2 clients.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionOffer {
    #[serde(flatten)]
    pub terminal: Offer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtc: Option<RtcOffer>,
}

impl std::ops::Deref for ConnectionOffer {
    type Target = Offer;
    fn deref(&self) -> &Offer {
        &self.terminal
    }
}

/// Only the authenticated Agent WebSocket may exchange these. Browser and
/// DataChannel terminal inputs must continue to deserialize as Input instead.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentControl {
    LeaseProbe { nonce: u64 },
    LeaseGrant { nonce: u64 },
    Revoke,
}

/// Browser authorization liveness stays on Console WSS, separate from input.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserControl {
    ControlProbe { nonce: u64 },
    ControlGrant { nonce: u64 },
}

/// Echoed by the Agent over the selected data transport, never sent to the Shell.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Probe {
    Ping { nonce: u32 },
    Pong { nonce: u32 },
}

/// Restrict configuration to UDP STUN discovery; no TURN credentials or arbitrary URLs.
pub fn stun_endpoint(value: &str) -> Result<(String, u16), &'static str> {
    let authority = value
        .strip_prefix("stun:")
        .ok_or("STUN URL must use stun:")?;
    if authority.is_empty()
        || authority.len() > 260
        || authority.bytes().any(|b| b.is_ascii_whitespace())
        || authority.contains(['/', '?', '#', '@', '\\'])
    {
        return Err("invalid STUN authority");
    }
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, rest) = rest.split_once(']').ok_or("invalid STUN IPv6 address")?;
        host.parse::<std::net::Ipv6Addr>()
            .map_err(|_| "invalid STUN IPv6 address")?;
        let port = if rest.is_empty() {
            "3478"
        } else {
            rest.strip_prefix(':').ok_or("invalid STUN port")?
        };
        (host, port)
    } else {
        let (host, port) = authority.split_once(':').unwrap_or((authority, "3478"));
        if host.is_empty()
            || host.len() > 253
            || !host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err("invalid STUN host");
        }
        (host, port)
    };
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|p| *p != 0)
        .ok_or("invalid STUN port")?;
    Ok((host.to_owned(), port))
}

pub fn valid_sdp(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SDP
        && value.starts_with("v=0")
        && value.lines().filter(|line| line.starts_with("m=")).count() == 1
        && value.lines().any(|line| line.starts_with("m=application "))
        && value
            .lines()
            .filter(|line| line.starts_with("a=candidate:"))
            .count()
            <= MAX_ICE_CANDIDATES
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub instance_id: String,
    pub runtime_id: String,
    pub restart_epoch: u64,
    pub reinstall_epoch: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Offer {
    pub session_id: String,
    pub target: Target,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub terminal_id: Option<String>,
    #[serde(default)]
    pub new_session: bool,
    #[serde(default)]
    pub policy_revision: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Input {
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Acknowledges bytes rendered by the browser, not merely received by a proxy.
    Ack {
        bytes: usize,
    },
    Close,
}

impl Input {
    pub fn valid(&self) -> bool {
        match self {
            Self::Resize { cols, rows } => (2..=500).contains(cols) && (1..=300).contains(rows),
            Self::Ack { bytes } => (1..=MAX_DATA).contains(bytes),
            Self::Close => true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Webrtc,
    Console,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Output {
    RtcAnswer {
        sdp: String,
    },
    Transport {
        kind: TransportKind,
        reason: Option<String>,
    },
    Ready,
    Session {
        id: String,
        sessions: Vec<SessionInfo>,
        cols: u16,
        rows: u16,
    },
    Replay {
        active: bool,
    },
    Exit {
        code: u32,
    },
    Error {
        message: String,
    },
}

pub fn valid_session(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trickled_candidates_are_bounded_and_scoped_to_one_generation() {
        let mut ice = IceCandidates::from_sdp(
            "v=0\r\na=ice-ufrag:abcd\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n",
        )
        .unwrap();
        let signal = |port| IceSignal::IceCandidate {
            candidate: format!("candidate:1 1 udp 2113929471 192.0.2.1 {port} typ host"),
            username_fragment: "abcd".into(),
        };
        assert!(ice
            .accept(&IceSignal::IceComplete {
                username_fragment: "old1".into()
            })
            .is_err());
        for candidate in [
            "candidate:1\r\na=ice-ufrag:evil".to_owned(),
            "candidate:".to_owned() + &"x".repeat(MAX_ICE_CANDIDATE_BYTES),
        ] {
            assert!(ice
                .accept(&IceSignal::IceCandidate {
                    candidate,
                    username_fragment: "abcd".into(),
                })
                .is_err());
        }
        for port in 30000..30032 {
            assert!(ice.accept(&signal(port)).unwrap());
        }
        assert!(!ice.accept(&signal(30000)).unwrap());
        assert!(ice.accept(&signal(30032)).is_err());
        let end = IceSignal::IceComplete {
            username_fragment: "abcd".into(),
        };
        assert!(ice.accept(&end).unwrap());
        assert!(!ice.accept(&end).unwrap());
        assert!(ice.accept(&signal(30033)).is_err());
        assert!(serde_json::from_value::<Input>(serde_json::to_value(end).unwrap()).is_err());
        assert!(IceCandidates::from_sdp("v=0\r\n").is_err());
    }
    #[test]
    fn terminal_bounds_reject_invalid_frames() {
        assert!(!Input::Resize { cols: 0, rows: 24 }.valid());
        assert!(!Input::Ack {
            bytes: MAX_DATA + 1
        }
        .valid());
        assert!(Input::Resize { cols: 80, rows: 24 }.valid());
        assert!(!valid_session("../runtime"));
        assert!(valid_session(&"a".repeat(64)));
        assert!(serde_json::from_str::<Input>(r#"{"type":"ping","nonce":1}"#).is_err());
        for nonce in ["-1", "4294967296", "1.5", "null"] {
            assert!(serde_json::from_str::<Probe>(&format!(
                r#"{{"type":"ping","nonce":{nonce}}}"#
            ))
            .is_err());
        }
    }
    #[test]
    fn only_bounded_udp_stun_configuration_is_accepted() {
        assert_eq!(
            stun_endpoint("stun:stun.example.com"),
            Ok(("stun.example.com".into(), 3478))
        );
        assert_eq!(
            stun_endpoint("stun:[2001:db8::1]:5349"),
            Ok(("2001:db8::1".into(), 5349))
        );
        for value in [
            "turn:example.com",
            "stuns:example.com",
            "stun://example.com",
            "stun:user@example.com",
            "stun:example.com?transport=tcp",
            "stun:example.com:0",
            "stun:-bad.example",
            "stun:example.com:65536",
            "stun:example.com/path",
        ] {
            assert!(stun_endpoint(value).is_err(), "{value}");
        }
    }
    #[test]
    fn rtc_signaling_preserves_legacy_offer_shape_and_is_not_terminal_input() {
        let offer = Offer {
            session_id: "a".repeat(64),
            target: Target {
                instance_id: "cvm-one".into(),
                runtime_id: "runtime-one".into(),
                restart_epoch: 2,
                reinstall_epoch: 3,
            },
            owner: "owner".into(),
            terminal_id: None,
            new_session: false,
            policy_revision: 4,
        };
        let legacy = serde_json::to_value(&offer).unwrap();
        let connection: ConnectionOffer = serde_json::from_value(legacy.clone()).unwrap();
        assert!(connection.rtc.is_none());
        assert_eq!(serde_json::to_value(connection).unwrap(), legacy);
        for control in [
            AgentControl::LeaseProbe { nonce: 1 },
            AgentControl::LeaseGrant { nonce: 1 },
            AgentControl::Revoke,
        ] {
            assert!(
                serde_json::from_value::<Input>(serde_json::to_value(control).unwrap()).is_err()
            );
        }
        let sdp = "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n";
        assert!(valid_sdp(sdp));
        assert!(!valid_sdp(
            &(sdp.to_owned() + "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n")
        ));
        assert!(!valid_sdp(
            &(sdp.to_owned() + &"a=candidate:x\r\n".repeat(33))
        ));
        assert!(!valid_sdp(&(sdp.to_owned() + &" ".repeat(MAX_SDP))));
    }
}
