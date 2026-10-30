//! Fixed-target service availability diagnostics for machines and chains.
use serde::{Deserialize, Serialize};

mod dial;
pub use dial::{observed_dial_failure, UnlockDialFailure, UnlockDialFailureSummary};

pub const UNLOCK_DUAL_STACK_PROTOCOL_VERSION: u32 = 50;
pub const UNLOCK_AUTOMATIC_CHAIN_PROTOCOL_VERSION: u32 = 52;
pub const UNLOCK_EXTENDED_SERVICES_PROTOCOL_VERSION: u32 = 53;
pub const UNLOCK_REDUCED_SERVICES_PROTOCOL_VERSION: u32 = 54;
pub const UNLOCK_EGRESS_CAPABILITY_PROTOCOL_VERSION: u32 = 55;
pub const UNLOCK_CONNECTION_TRACE_PROTOCOL_VERSION: u32 = 57;
pub const PROBE_NODE_ID_HEADER: &str = "x-brocade-node-id";
pub const PROBE_IDENTITY_ENV: &str = "XRAY_BROCADE_PROBE_IDENTITY_FILE";
pub const PROBE_PROFILE_ENV: &str = "XRAY_BROCADE_PROBE_PROFILE";
pub const PROBE_FILE_ENV: &str = "XRAY_BROCADE_PROBE_FILE";
pub use brocade_core::model::IpFamily;

pub const UNLOCK_PROBE_PROTOCOL_VERSION: u32 = 46;
pub const CHAIN_UNLOCK_PROBE_PROTOCOL_VERSION: u32 = 47;
pub const YOUTUBE_UNLOCK_DETAILS_PROTOCOL_VERSION: u32 = 49;
pub const UNLOCK_PROBE_INTERVAL_SECS: u64 = 86_400;

/// Digest the loaded DNS and Freedom configuration, excluding credentials and unrelated
/// certificate/runtime details. Agent and Console share this exact canonical projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnlockProbeProfile {
    pub fingerprint: String,
    pub strategies: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub bindings: std::collections::BTreeMap<String, IpFamily>,
}
impl UnlockProbeProfile {
    pub fn from_config(config: &serde_json::Value) -> Option<Self> {
        let outbounds = config
            .get("outbounds")?
            .as_array()?
            .iter()
            .filter(|outbound| outbound.get("protocol").and_then(|v| v.as_str()) == Some("freedom"))
            .cloned()
            .collect::<Vec<_>>();
        let strategies = outbounds
            .iter()
            .filter_map(|outbound| {
                let tag = outbound.get("tag")?.as_str()?;
                if tag.starts_with("probe:") || brocade_core::model::is_probe_family_tag(tag) {
                    return None;
                }
                let strategy = outbound
                    .pointer("/settings/domainStrategy")
                    .and_then(|v| v.as_str())
                    .unwrap_or("AsIs");
                Some((tag.to_owned(), strategy.to_owned()))
            })
            .collect();
        let bindings = outbounds
            .iter()
            .filter_map(|outbound| {
                let tag = outbound.get("tag")?.as_str()?;
                if tag.starts_with("probe:") || brocade_core::model::is_probe_family_tag(tag) {
                    return None;
                }
                let source = outbound
                    .get("sendThrough")?
                    .as_str()?
                    .parse::<std::net::IpAddr>()
                    .ok()?;
                Some((
                    tag.to_owned(),
                    if source.is_ipv4() {
                        IpFamily::V4
                    } else {
                        IpFamily::V6
                    },
                ))
            })
            .collect();
        let projection = serde_json::json!({"dns":config.get("dns"),"outbounds":outbounds});
        let bytes = serde_json::to_vec(&projection).ok()?;
        Some(Self {
            fingerprint: brocade_core::hash::sha256_hex(&bytes),
            strategies,
            bindings,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockService {
    Chatgpt,
    Claude,
    Gemini,
    Netflix,
    DisneyPlus,
    YoutubePremium,
    Tiktok,
    PrimeVideo,
    Reddit,
    Spotify,
    HboMax,
    Dazn,
    BbcIplayer,
    Crunchyroll,
    ParamountPlus,
}
pub const LEGACY_UNLOCK_SERVICES: [UnlockService; 9] = [
    UnlockService::Chatgpt,
    UnlockService::Claude,
    UnlockService::Gemini,
    UnlockService::Netflix,
    UnlockService::DisneyPlus,
    UnlockService::YoutubePremium,
    UnlockService::Tiktok,
    UnlockService::PrimeVideo,
    UnlockService::Reddit,
];
pub const UNLOCK_SERVICES: [UnlockService; 12] = [
    UnlockService::Chatgpt,
    UnlockService::Claude,
    UnlockService::Gemini,
    UnlockService::Netflix,
    UnlockService::DisneyPlus,
    UnlockService::YoutubePremium,
    UnlockService::Tiktok,
    UnlockService::PrimeVideo,
    UnlockService::Reddit,
    UnlockService::Spotify,
    UnlockService::HboMax,
    UnlockService::Dazn,
];
/// Retained for v53 in-flight reports and Console rollback. New jobs never select
/// BBC iPlayer, Crunchyroll or Paramount+.
pub const V53_UNLOCK_SERVICES: [UnlockService; 15] = [
    UnlockService::Chatgpt,
    UnlockService::Claude,
    UnlockService::Gemini,
    UnlockService::Netflix,
    UnlockService::DisneyPlus,
    UnlockService::YoutubePremium,
    UnlockService::Tiktok,
    UnlockService::PrimeVideo,
    UnlockService::Reddit,
    UnlockService::Spotify,
    UnlockService::HboMax,
    UnlockService::Dazn,
    UnlockService::BbcIplayer,
    UnlockService::Crunchyroll,
    UnlockService::ParamountPlus,
];

impl UnlockService {
    /// All fixed request and redirect hosts used by this diagnostic. DNS attribution
    /// requires agreement across them, rather than guessing a service from its name.
    pub fn probe_hosts(self) -> &'static [&'static str] {
        match self {
            Self::Chatgpt => &[
                "chat.openai.com",
                "chatgpt.com",
                "api.openai.com",
                "ios.chat.openai.com",
            ],
            Self::Claude => &["claude.ai", "claude.com", "www.anthropic.com"],
            Self::Gemini => &["gemini.google.com"],
            Self::Netflix => &["www.netflix.com"],
            Self::DisneyPlus => &[
                "www.disneyplus.com",
                "disneyplus.com",
                "disney.api.edge.bamgrid.com",
            ],
            Self::YoutubePremium => &["www.youtube.com", "redirector.googlevideo.com"],
            Self::Tiktok => &["www.tiktok.com"],
            Self::PrimeVideo => &["www.primevideo.com"],
            Self::Reddit => &["www.reddit.com"],
            Self::Spotify => &["open.spotify.com"],
            Self::HboMax => &["www.max.com", "www.hbomax.com"],
            Self::Dazn => &["startup.core.indazn.com"],
            Self::BbcIplayer => &["open.live.bbc.co.uk"],
            Self::Crunchyroll => &["www.crunchyroll.com"],
            Self::ParamountPlus => &["www.paramountplus.com"],
        }
    }
}

/// Console explanation frozen with actual resolver evidence from the exit Xray.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockDnsPath {
    pub exit: String,
    pub normal_family: Option<IpFamily>,
    pub fallback: brocade_core::model::EgressDnsFallback,
    pub rules: Vec<UnlockDnsRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_from: Option<UnlockDnsSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<UnlockDnsReason>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockDnsReason {
    NoAddress,
    ResolverError,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockDnsSource {
    Machine,
    Policy,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockDnsRule {
    pub selector: brocade_core::model::DestMatch,
    pub resolution: brocade_core::model::EgressDnsResolution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_member: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<UnlockDnsMember>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockPart {
    Service,
    Webpage,
    Premium,
    Cdn,
}
impl UnlockPart {
    pub fn key(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Webpage => "webpage",
            Self::Premium => "premium",
            Self::Cdn => "cdn",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockConnectionObservation {
    pub part: UnlockPart,
    #[serde(flatten)]
    pub connection: UnlockConnectionTrace,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockConnectionTrace {
    pub version: u32,
    pub id: String,
    pub domain: String,
    pub exit: String,
    pub outbound: String,
    pub profile: String,
    pub normal_strategy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Connected peer family on success; attempted destination family on failure.
    pub family: Option<IpFamily>,
    pub connected: bool,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub lookups: Vec<UnlockDnsTrace>,
}
impl UnlockConnectionTrace {
    pub fn valid(&self) -> bool {
        let identifier = |value: &str, max: usize| {
            value.len() <= max
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b":._-".contains(&b))
        };
        self.version == 2
            && self.id.len() == 32
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && V53_UNLOCK_SERVICES
                .iter()
                .any(|s| s.probe_hosts().contains(&self.domain.as_str()))
            && identifier(&self.exit, 128)
            && identifier(&self.outbound, 256)
            && (self.profile.is_empty()
                || self.profile.len() == 64 && self.profile.bytes().all(|b| b.is_ascii_hexdigit()))
            && self.normal_strategy.len() <= 32
            && (!self.connected || self.family.is_some())
            && self.error.as_ref().is_none_or(|s| identifier(s, 32))
            && self.lookups.len() <= 8
            && self.lookups.iter().all(UnlockDnsTrace::valid)
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    #[test]
    fn profile_digest_fences_dns_and_outbound_semantics_without_credentials() {
        let mut config = serde_json::json!({"dns":{"servers":["1.1.1.1"]},"inbounds":[{"settings":{"password":"synthetic-test-key"}}],"outbounds":[{"tag":"out:forward","protocol":"freedom","settings":{"domainStrategy":"UseIP"}}]});
        let before = UnlockProbeProfile::from_config(&config).unwrap();
        config["inbounds"][0]["settings"]["password"] = serde_json::json!("other-synthetic-key");
        assert_eq!(
            before.fingerprint,
            UnlockProbeProfile::from_config(&config)
                .unwrap()
                .fingerprint
        );
        config["dns"]["servers"] = serde_json::json!(["9.9.9.9"]);
        assert_ne!(
            before.fingerprint,
            UnlockProbeProfile::from_config(&config)
                .unwrap()
                .fingerprint
        );
        config["dns"]["servers"] = serde_json::json!(["1.1.1.1"]);
        config["outbounds"][0]["settings"]["domainStrategy"] = serde_json::json!("UseIPv6");
        assert_ne!(
            before.fingerprint,
            UnlockProbeProfile::from_config(&config)
                .unwrap()
                .fingerprint
        );
    }
    #[test]
    fn actual_connection_format_is_versioned_bounded_and_has_no_response_material() {
        let mut trace:UnlockConnectionTrace=serde_json::from_value(serde_json::json!({"version":2,"id":"00000000000000000000000000000001","domain":"www.youtube.com","exit":"mo","outbound":"out:forward:probe-v4","profile":"a".repeat(64),"normal_strategy":"UseIP","family":"v4","connected":true,"complete":true,"lookups":[]})).unwrap();
        assert!(trace.valid());
        trace.version = 1;
        assert!(!trace.valid());
        trace.version = 2;
        trace.domain = "www.youtube.com?private=value".into();
        assert!(!trace.valid());
        trace.domain = "www.youtube.com".into();
        trace.id = "g".repeat(32);
        assert!(!trace.valid());
        let skipped:UnlockResult=serde_json::from_value(serde_json::json!({"service":"youtube_premium","status":"skipped","code":"no_ipv4_dns","region":null})).unwrap();
        assert!(skipped.valid());
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockDnsKind {
    Direct,
    Fallback,
    Partial,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockDnsPartPath {
    pub part: UnlockPart,
    pub kind: UnlockDnsKind,
    pub paths: Vec<UnlockDnsPath>,
    pub exits: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockDnsTrace {
    pub version: u32,
    pub complete: bool,
    pub steps: Vec<UnlockDnsStep>,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub strategy: String,
}
impl UnlockDnsTrace {
    pub fn valid(&self) -> bool {
        self.version == 1
            && self.domain.len() <= 253
            && self
                .domain
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
            && self.strategy.len() <= 32
            && self.steps.len() <= 8
            && self.steps.iter().all(|step| {
                step.tag.len() <= 64
                    && step
                        .tag
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b":._-".contains(&b))
                    && step.members.len() <= 16
                    && step.members.iter().all(|member| {
                        (1..=16).contains(&member.index)
                            && member.attempts <= 5
                            && member.rcode <= 4095
                            && matches!(member.cache.as_str(), "" | "hit" | "stale" | "miss")
                    })
                    && step
                        .members
                        .iter()
                        .map(|member| member.index)
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == step.members.len()
                    && step.winner.is_none_or(|winner| {
                        step.outcome == UnlockDnsOutcome::Answer
                            && step.members.iter().any(|member| {
                                member.index == winner && member.outcome == UnlockDnsOutcome::Answer
                            })
                    })
            })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockDnsStep {
    pub tag: String,
    pub scoped: bool,
    pub outcome: UnlockDnsOutcome,
    pub other: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<UnlockDnsMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockDnsMember {
    pub index: u32,
    pub outcome: UnlockDnsOutcome,
    pub other: bool,
    pub attempts: u32,
    pub elapsed_ms: u32,
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub rcode: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cache: String,
}

fn is_zero_u16(value: &u16) -> bool {
    *value == 0
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockDnsOutcome {
    Answer,
    Empty,
    Error,
}

fn valid_service_set(services: impl IntoIterator<Item = UnlockService>) -> bool {
    let services: Vec<_> = services.into_iter().collect();
    let expected: &[UnlockService] = if services.len() == LEGACY_UNLOCK_SERVICES.len() {
        &LEGACY_UNLOCK_SERVICES
    } else if services.len() == UNLOCK_SERVICES.len() {
        &UNLOCK_SERVICES
    } else if services.len() == V53_UNLOCK_SERVICES.len() {
        &V53_UNLOCK_SERVICES
    } else {
        return false;
    };
    services
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        == expected.iter().copied().collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockStatus {
    Available,
    UnsupportedRegion,
    Restricted,
    Partial,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockCode {
    WebAvailable,
    RegionUnsupported,
    AccessDenied,
    OriginalsOnly,
    WebOnly,
    AppOnly,
    Timeout,
    NetworkError,
    NoIpv6Dns,
    NoIpv4Dns,
    NoIpv6Egress,
    UnrecognizedResponse,
}
impl UnlockCode {
    pub fn is_family_skip(self) -> bool {
        matches!(self, Self::NoIpv6Dns | Self::NoIpv4Dns | Self::NoIpv6Egress)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YoutubePageResult {
    pub status: UnlockStatus,
    pub code: UnlockCode,
    pub http_status: Option<u16>,
    /// GL is page configuration, kept separately from service-attributed country evidence.
    pub page_region: Option<String>,
    pub topbar_region: Option<String>,
    /// Only the country from a fresh privacy response cookie; never its raw contents.
    pub privacy_region: Option<String>,
}
impl YoutubePageResult {
    pub fn region(&self) -> Option<&str> {
        self.privacy_region
            .as_deref()
            .or(self.topbar_region.as_deref())
    }
    pub fn valid(&self) -> bool {
        self.status != UnlockStatus::Partial
            && (self.status != UnlockStatus::Skipped
                || (self.http_status.is_none()
                    && self.page_region.is_none()
                    && self.topbar_region.is_none()
                    && self.privacy_region.is_none()))
            && status_code_valid(self.status, self.code)
            && valid_http_status(self.http_status)
            && (self.status != UnlockStatus::Available
                || self
                    .http_status
                    .is_some_and(|status| (200..300).contains(&status)))
            && [&self.page_region, &self.topbar_region, &self.privacy_region]
                .into_iter()
                .all(|region| valid_region(region.as_deref()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YoutubeCdnResult {
    pub http_status: Option<u16>,
    /// Sanitized mapping node only, without the client IP/prefix or raw response.
    pub node: Option<String>,
    pub airport: Option<String>,
    pub country: Option<String>,
    pub error: Option<UnlockCode>,
    /// Complete request duration through the measured path; not an exit-to-CDN RTT.
    pub elapsed_ms: u32,
}
impl YoutubeCdnResult {
    pub fn valid(&self) -> bool {
        valid_http_status(self.http_status)
            && match (&self.node, self.error) {
                (Some(node), None) => {
                    !node.is_empty()
                        && node.len() <= 96
                        && node
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
                        && self
                            .http_status
                            .is_some_and(|status| (200..300).contains(&status))
                }
                (
                    None,
                    Some(
                        UnlockCode::Timeout
                        | UnlockCode::NetworkError
                        | UnlockCode::UnrecognizedResponse
                        | UnlockCode::AccessDenied
                        | UnlockCode::NoIpv6Dns
                        | UnlockCode::NoIpv4Dns
                        | UnlockCode::NoIpv6Egress,
                    ),
                ) => self.airport.is_none() && self.country.is_none(),
                _ => false,
            }
            && self.airport.as_ref().is_none_or(|airport| {
                airport.len() == 3 && airport.bytes().all(|byte| byte.is_ascii_uppercase())
            })
            && valid_region(self.country.as_deref())
            && (self.country.is_none() || self.airport.is_some())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YoutubeDiagnostics {
    pub webpage: YoutubePageResult,
    pub premium: YoutubePageResult,
    pub cdn: YoutubeCdnResult,
}
impl YoutubeDiagnostics {
    pub fn valid(&self) -> bool {
        self.webpage.valid() && self.premium.valid() && self.cdn.valid()
    }
}

fn valid_http_status(status: Option<u16>) -> bool {
    status.is_none_or(|status| (100..=599).contains(&status))
}
fn valid_region(region: Option<&str>) -> bool {
    region.is_none_or(|region| {
        (2..=3).contains(&region.len()) && region.bytes().all(|byte| byte.is_ascii_uppercase())
    })
}
fn status_code_valid(status: UnlockStatus, code: UnlockCode) -> bool {
    matches!(
        (status, code),
        (
            UnlockStatus::Skipped,
            UnlockCode::NoIpv6Dns | UnlockCode::NoIpv4Dns | UnlockCode::NoIpv6Egress
        ) | (UnlockStatus::Available, UnlockCode::WebAvailable)
            | (
                UnlockStatus::UnsupportedRegion,
                UnlockCode::RegionUnsupported
            )
            | (UnlockStatus::Restricted, UnlockCode::AccessDenied)
            | (
                UnlockStatus::Partial,
                UnlockCode::OriginalsOnly | UnlockCode::WebOnly | UnlockCode::AppOnly
            )
            | (
                UnlockStatus::Failed,
                UnlockCode::Timeout | UnlockCode::NetworkError | UnlockCode::UnrecognizedResponse
            )
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockResult {
    pub service: UnlockService,
    pub status: UnlockStatus,
    pub code: UnlockCode,
    /// Service-returned region code; Reddit also uses the Fastly delivery POP.
    /// Never substituted with the machine's configured/GeoIP country.
    pub region: Option<String>,
    /// Historical v48 result condition. New split diagnostics always measure the original
    /// DNS path; retain this field so archived/rolling-upgrade results remain readable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub client_dns: bool,
    /// Additive v49 details. Legacy fields above mirror the Premium result for older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub youtube: Option<YoutubeDiagnostics>,
    /// Added by Console after authenticating and fencing the job; never trusted
    /// from an Agent. Old reports and old peers omit this optional explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_path: Option<UnlockDnsPath>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connection_observations: Vec<UnlockConnectionObservation>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub connection_requests: std::collections::BTreeMap<UnlockPart, u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dns_parts: Vec<UnlockDnsPartPath>,
}
impl UnlockResult {
    pub fn dial_retry_allowed(&self, part: UnlockPart, domain: Option<&str>) -> bool {
        observed_dial_failure(
            &self.connection_observations,
            part,
            self.connection_requests.get(&part).copied().unwrap_or(0),
            domain,
        )
        .is_none_or(|failure| failure.retry_allowed)
    }

    /// Premium landing-page markers also appear in countries without subscriptions.
    /// Apply the service-attributed country after parsing, without changing independent
    /// webpage/CDN observations or replacing a real network/access failure.
    pub fn apply_youtube_premium_region_policy(&mut self) {
        if self.service != UnlockService::YoutubePremium || self.status != UnlockStatus::Available {
            return;
        }
        let region = match &self.youtube {
            Some(detail) => detail.premium.region(),
            None => self.region.as_deref(),
        };
        if !matches!(region, Some("CN" | "MO")) {
            return;
        }
        self.status = UnlockStatus::UnsupportedRegion;
        self.code = UnlockCode::RegionUnsupported;
        self.client_dns = false;
        if let Some(detail) = &mut self.youtube {
            detail.premium.status = self.status;
            detail.premium.code = self.code;
        }
    }

    pub fn valid(&self) -> bool {
        let status_matches = status_code_valid(self.status, self.code);
        status_matches
            && self.connection_observations.len() <= 64
            && self
                .connection_observations
                .iter()
                .all(|observation| observation.connection.valid())
            && self.connection_requests.len() <= 4
            && self.connection_requests.values().all(|count| *count <= 64)
            && self
                .connection_observations
                .iter()
                .map(|observation| observation.part)
                .chain(self.connection_requests.keys().copied())
                .all(|part| {
                    if self.service == UnlockService::YoutubePremium {
                        part != UnlockPart::Service
                    } else {
                        part == UnlockPart::Service
                    }
                })
            && self
                .connection_observations
                .iter()
                .map(|o| &o.connection.id)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == self.connection_observations.len()
            && (self.status != UnlockStatus::Skipped || (self.region.is_none() && !self.client_dns))
            && (!self.client_dns
                || (self.service == UnlockService::YoutubePremium
                    && self.status == UnlockStatus::Available))
            && match self.code {
                UnlockCode::OriginalsOnly => self.service == UnlockService::Netflix,
                UnlockCode::WebOnly | UnlockCode::AppOnly => self.service == UnlockService::Chatgpt,
                _ => true,
            }
            && valid_region(self.region.as_deref())
            && self.youtube.as_ref().is_none_or(|youtube| {
                self.service == UnlockService::YoutubePremium
                    && !self.client_dns
                    && youtube.valid()
                    && youtube.premium.status == self.status
                    && youtube.premium.code == self.code
                    && youtube.premium.region() == self.region.as_deref()
            })
    }
}

#[cfg(test)]
mod youtube_region_policy_tests {
    use super::*;

    #[test]
    fn premium_country_policy_is_bounded_and_idempotent() {
        for region in ["CN", "MO", "HK", "TW", "US"] {
            let mut result: UnlockResult = serde_json::from_value(serde_json::json!({
                "service":"youtube_premium","status":"available","code":"web_available",
                "region":region,"client_dns":true
            }))
            .unwrap();
            result.apply_youtube_premium_region_policy();
            assert!(result.valid());
            let unsupported = matches!(region, "CN" | "MO");
            assert_eq!(
                result.status == UnlockStatus::UnsupportedRegion,
                unsupported
            );
            assert_eq!(result.client_dns, !unsupported);
            let once = result.clone();
            result.apply_youtube_premium_region_policy();
            assert_eq!(result, once);
        }
    }

    #[test]
    fn premium_country_policy_preserves_failures_restrictions_and_other_services() {
        for (service, status, code, region) in [
            (
                UnlockService::YoutubePremium,
                UnlockStatus::Failed,
                UnlockCode::NetworkError,
                Some("CN"),
            ),
            (
                UnlockService::YoutubePremium,
                UnlockStatus::Failed,
                UnlockCode::Timeout,
                Some("MO"),
            ),
            (
                UnlockService::YoutubePremium,
                UnlockStatus::Restricted,
                UnlockCode::AccessDenied,
                Some("CN"),
            ),
            (
                UnlockService::YoutubePremium,
                UnlockStatus::Skipped,
                UnlockCode::NoIpv6Dns,
                None,
            ),
            (
                UnlockService::YoutubePremium,
                UnlockStatus::Available,
                UnlockCode::WebAvailable,
                None,
            ),
            (
                UnlockService::Netflix,
                UnlockStatus::Available,
                UnlockCode::WebAvailable,
                Some("CN"),
            ),
        ] {
            let mut result: UnlockResult = serde_json::from_value(serde_json::json!({
                "service":service,"status":status,"code":code,"region":region
            }))
            .unwrap();
            assert!(result.valid());
            let before = result.clone();
            result.apply_youtube_premium_region_policy();
            assert_eq!(result, before);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockProbeTarget {
    pub probe_id: String,
    /// Missing on older Consoles: a newer Agent must still report the original nine services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<UnlockService>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<IpFamily>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// The selected path can propagate the forced-family identity. Runtime freshness and
    /// host address observations do not establish network reachability.
    pub family_ready: Option<bool>,
    /// Legacy v55 negotiation. Retained for decoding older peers; new Agents use actual
    /// requests and never infer a remote exit from the local machine environment.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ipv6_egress_check: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<crate::protocol::E2eProbeTarget>,
    /// v56 opt-in. Missing on an older Console preserves its original DNS work.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub connection_trace: bool,
}
impl UnlockProbeTarget {
    pub fn services(&self) -> &[UnlockService] {
        self.services.as_deref().unwrap_or(&LEGACY_UNLOCK_SERVICES)
    }
    pub fn valid_services(&self) -> bool {
        valid_service_set(self.services().iter().copied())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockPathError {
    ClientUnavailable,
    ConfigurationChanged,
    FamilyUnavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockProbeReport {
    pub probe_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<IpFamily>,
    pub results: Vec<UnlockResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_error: Option<UnlockPathError>,
}
impl UnlockProbeReport {
    pub fn valid(&self) -> bool {
        valid_service_set(self.results.iter().map(|result| result.service))
            && (self.path_error.is_none()
                || self
                    .results
                    .iter()
                    .all(|result| result.status == UnlockStatus::Failed))
            && self.results.iter().all(UnlockResult::valid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn address_families_are_explicit_and_legacy_targets_remain_unclassified() {
        let legacy: UnlockProbeTarget = serde_json::from_value(serde_json::json!({
            "probe_id": "legacy"
        }))
        .unwrap();
        assert_eq!(legacy.family, None);
        assert_eq!(legacy.family_ready, None);
        assert!(!legacy.connection_trace);
        assert_eq!(legacy.services(), LEGACY_UNLOCK_SERVICES);
        assert!(legacy.valid_services());
        let encoded = serde_json::to_value(legacy).unwrap();
        assert!(encoded.get("family").is_none());
        assert!(encoded.get("family_ready").is_none());
        assert!(encoded.get("connection_trace").is_none());
        for (family, code) in [(IpFamily::V4, "v4"), (IpFamily::V6, "v6")] {
            let value = serde_json::json!({"probe_id":"split", "family":code});
            let target: UnlockProbeTarget = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(target.family, Some(family));
            assert_eq!(serde_json::to_value(target).unwrap(), value);
            let report: UnlockProbeReport = serde_json::from_value(serde_json::json!({
                "probe_id":"split", "family":code, "results":[]
            }))
            .unwrap();
            assert_eq!(report.family, Some(family));
            assert_eq!(serde_json::to_value(report).unwrap()["family"], code);
        }
        assert!(
            serde_json::from_value::<UnlockProbeTarget>(serde_json::json!({
                "probe_id":"split", "family":"auto"
            }))
            .is_err()
        );
    }

    #[test]
    fn extended_services_are_negotiated_and_legacy_reports_remain_valid() {
        let mut target: UnlockProbeTarget = serde_json::from_value(serde_json::json!({
            "probe_id":"extended", "services":UNLOCK_SERVICES
        }))
        .unwrap();
        assert!(target.valid_services());
        assert_eq!(target.services().len(), 12);
        let mut report = UnlockProbeReport {
            probe_id: "legacy".into(),
            family: None,
            path_error: None,
            results: LEGACY_UNLOCK_SERVICES
                .into_iter()
                .map(|service| UnlockResult {
                    service,
                    status: UnlockStatus::Available,
                    code: UnlockCode::WebAvailable,
                    region: Some("HK".into()),
                    client_dns: false,
                    youtube: None,
                    dns_path: None,
                    connection_observations: Vec::new(),
                    connection_requests: Default::default(),
                    dns_parts: Vec::new(),
                })
                .collect(),
        };
        assert!(
            report.valid(),
            "old Agents continue to report exactly nine services"
        );
        for service in UNLOCK_SERVICES
            .into_iter()
            .skip(LEGACY_UNLOCK_SERVICES.len())
        {
            report.results.push(UnlockResult {
                service,
                status: UnlockStatus::Available,
                code: UnlockCode::WebAvailable,
                region: Some("HK".into()),
                client_dns: false,
                youtube: None,
                dns_path: None,
                connection_observations: Vec::new(),
                connection_requests: Default::default(),
                dns_parts: Vec::new(),
            });
        }
        assert!(report.valid());
        assert_eq!(
            serde_json::from_str::<UnlockProbeReport>(&serde_json::to_string(&report).unwrap())
                .unwrap(),
            report
        );
        report.results.pop();
        assert!(
            !report.valid(),
            "a partial new set cannot masquerade as a completed result"
        );
        target.services.as_mut().unwrap().pop();
        assert!(!target.valid_services());
    }

    #[test]
    fn retired_services_are_only_retained_for_v53_compatibility() {
        for service in [
            UnlockService::BbcIplayer,
            UnlockService::Crunchyroll,
            UnlockService::ParamountPlus,
        ] {
            assert!(!UNLOCK_SERVICES.contains(&service));
            assert!(V53_UNLOCK_SERVICES.contains(&service));
        }
        assert!(valid_service_set(V53_UNLOCK_SERVICES));
        let historical = UnlockProbeReport {
            probe_id: "v53".into(),
            family: None,
            path_error: None,
            results: V53_UNLOCK_SERVICES
                .into_iter()
                .map(|service| UnlockResult {
                    service,
                    status: UnlockStatus::Available,
                    code: UnlockCode::WebAvailable,
                    region: Some("HK".into()),
                    client_dns: false,
                    youtube: None,
                    dns_path: None,
                    connection_observations: Vec::new(),
                    connection_requests: Default::default(),
                    dns_parts: Vec::new(),
                })
                .collect(),
        };
        assert!(historical.valid());
        assert_eq!(
            serde_json::from_str::<UnlockProbeReport>(&serde_json::to_string(&historical).unwrap())
                .unwrap(),
            historical
        );
    }

    #[test]
    fn split_youtube_results_are_additive_bounded_and_match_the_legacy_projection() {
        let value = serde_json::json!({
            "service":"youtube_premium","status":"unsupported_region","code":"region_unsupported","region":"MO",
            "youtube":{
                "webpage":{"status":"available","code":"web_available","http_status":200,"page_region":"US","topbar_region":null,"privacy_region":"MO"},
                "premium":{"status":"unsupported_region","code":"region_unsupported","http_status":200,"page_region":"US","topbar_region":null,"privacy_region":"MO"},
                "cdn":{"http_status":200,"node":"hkg12s15","airport":"HKG","country":"HK","error":null,"elapsed_ms":1100}
            }
        });
        let original: UnlockResult = serde_json::from_value(value).unwrap();
        assert!(original.valid());
        assert_eq!(
            serde_json::from_str::<UnlockResult>(&serde_json::to_string(&original).unwrap())
                .unwrap(),
            original
        );
        let mut changed = original.clone();
        changed.region = Some("US".into());
        assert!(!changed.valid());
        let mut changed = original.clone();
        changed.service = UnlockService::Netflix;
        assert!(!changed.valid());
        let mut changed = original.clone();
        changed.youtube.as_mut().unwrap().cdn.node = Some("<script>".into());
        assert!(!changed.valid());
        let mut changed = original.clone();
        changed.youtube.as_mut().unwrap().cdn.node = Some("x".repeat(97));
        assert!(!changed.valid());
        let mut changed = original.clone();
        changed.youtube.as_mut().unwrap().webpage.http_status = None;
        assert!(!changed.valid());
        let mut changed = original;
        let cdn = &mut changed.youtube.as_mut().unwrap().cdn;
        cdn.node = None;
        cdn.airport = None;
        cdn.country = None;
        cdn.error = Some(UnlockCode::Timeout);
        cdn.http_status = None;
        assert!(changed.valid());
    }
    #[test]
    fn client_dns_is_optional_and_only_describes_confirmed_youtube_access() {
        let old = r#"{"service":"youtube_premium","status":"available","code":"web_available","region":"HK"}"#;
        let mut result: UnlockResult = serde_json::from_str(old).unwrap();
        assert!(!result.client_dns);
        assert!(result.valid());
        assert!(serde_json::to_value(&result)
            .unwrap()
            .get("client_dns")
            .is_none());
        result.client_dns = true;
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["client_dns"], true);
        assert_eq!(
            serde_json::from_value::<UnlockResult>(value).unwrap(),
            result
        );
        assert!(result.valid());
        result.service = UnlockService::Netflix;
        assert!(!result.valid());
        result.service = UnlockService::YoutubePremium;
        result.status = UnlockStatus::Failed;
        result.code = UnlockCode::NetworkError;
        assert!(!result.valid());
    }
    #[test]
    fn no_ipv6_dns_is_a_known_skipped_result_without_a_region() {
        let mut result: UnlockResult = serde_json::from_value(serde_json::json!({
            "service":"reddit","status":"skipped","code":"no_ipv6_dns","region":null
        }))
        .unwrap();
        assert!(result.valid());
        result.code = UnlockCode::NetworkError;
        assert!(!result.valid());
        result.code = UnlockCode::NoIpv6Egress;
        assert!(result.valid());
        result.code = UnlockCode::NoIpv6Dns;
        result.region = Some("HK".into());
        assert!(!result.valid());
    }

    #[test]
    fn report_requires_every_service_once_and_consistent_evidence() {
        let mut report = UnlockProbeReport {
            family: None,
            probe_id: "probe".into(),
            path_error: None,
            results: UNLOCK_SERVICES
                .into_iter()
                .map(|service| UnlockResult {
                    service,
                    status: UnlockStatus::Failed,
                    code: UnlockCode::Timeout,
                    region: None,
                    client_dns: false,
                    youtube: None,
                    dns_path: None,
                    connection_observations: Vec::new(),
                    connection_requests: Default::default(),
                    dns_parts: Vec::new(),
                })
                .collect(),
        };
        assert!(report.valid());
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["results"][0]["service"], "chatgpt");
        report.results[1].service = UnlockService::Chatgpt;
        assert!(!report.valid());
        report.results[1].service = UnlockService::Claude;
        report.results[0].status = UnlockStatus::Available;
        assert!(!report.valid());
        report.results[0].code = UnlockCode::WebAvailable;
        report.results[0].region = Some("HK".into());
        assert!(report.valid());
        report.results[0].region = Some("<script>".into());
        assert!(!report.valid());
    }
}
