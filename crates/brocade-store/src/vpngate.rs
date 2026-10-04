//! Durable VPN Gate catalogue and node-originated measurement history.
//!
//! The provider feed is mutable and profiles expire. This module therefore separates the fast
//! retained directory (`vpngate_servers`) from immutable sync observations and probe samples.
//! `current` records whether a server appears in the union of every selected collector's latest
//! complete snapshot. Bounded history remains audit evidence; serving reads a separate latest
//! projection so its cost does not grow with that history.

use std::collections::{BTreeMap, BTreeSet};

use brocade_core::model::{
    vpngate_runtime_peer, Action, ExternalOutboundProtocol, ModelSnapshot, VPNGATE_MAX_CANDIDATES,
    VPNGATE_RUNTIME_MAX_POOLS_PER_NODE, VPNGATE_RUNTIME_SOCKS_PORT,
};
use brocade_deployment::protocol::{
    evaluate_vpngate_admission, validate_vpngate_admission_policy, VpngateAdmissionDecision,
    VpngateAdmissionPolicy, VpngateCandidate, VpngateDesiredPool, VpngateDesiredState,
    VpngateIpIntelligenceReport, VpngateIpNetwork, VpngateIpProvider, VpngateIpScore,
    VpngateManualSwitchCommand, VpngateManualSwitchStatus, VpngatePoolReport,
    VpngateProbeAssignment, VpngateProbeMode, VpngateProbeReport, VpngateProbeSample,
    VpngateProbeStatus, VpngateReconcileReport, VpngateTransport, MIN_AGENT_PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, Result, StoreError};

const SYNC_LEASE_SECS: i32 = 300;
const MAX_ERROR_CODE_CHARS: usize = 64;
const MAX_ERROR_DETAIL_CHARS: usize = 2_000;
const MAX_CURRENT_CATALOG_DROP_FACTOR: u64 = 3;
// Keep a wider country-level shortlist than any one runtime pool. Individual outbounds still
// apply their own bounded `max_candidates`, while the directory can expose enough admitted
// alternatives for different pools and manual selection.
const MAX_ACTIVE_CANDIDATES_PER_COUNTRY: i64 = 32;
const DEFAULT_CATALOG_PROBE_WORKERS: u8 = 16;
const MAX_CATALOG_PROBE_WORKERS: usize = 128;
const MAX_CATALOG_PROBE_REPORT_SAMPLES: i64 = 128;
const REPUTATION_LEASE_SECS: i32 = 60;
const INTELLIGENCE_POLICY_MIN_HOURS: u32 = 1;
const INTELLIGENCE_POLICY_MAX_HOURS: u32 = 10 * 365 * 24;
const ACTIVE_MAX_CONNECT_MS: i32 = 15_000;
const ACTIVE_MIN_DOWNLOAD_BPS: i64 = 1_000_000;
// Successful performance evidence remains usable for at least five hours. The query extends this
// floor to the configured performance interval plus one connectivity interval, preventing a
// healthy candidate from falling out between its scheduled checks. A later failure still starts
// the much shorter review window below.
const ACTIVE_PROBE_MIN_MAX_AGE_SECS: i32 = 5 * 60 * 60;
const PROBE_SUCCESS_COOLDOWN_MIN_SECS: u32 = 60;
const PROBE_SUCCESS_COOLDOWN_MAX_SECS: u32 = 24 * 60 * 60;
const PROBE_PERFORMANCE_COOLDOWN_MIN_SECS: u32 = 60 * 60;
const PROBE_PERFORMANCE_COOLDOWN_MAX_SECS: u32 = 7 * 24 * 60 * 60;
const PROBE_SHARD_ROTATION_MIN_SECS: u32 = 60 * 60;
const PROBE_SHARD_ROTATION_MAX_SECS: u32 = 7 * 24 * 60 * 60;
const FAILED_CANDIDATE_GRACE_SECS: i32 = 20 * 60;
const MAX_CONSECUTIVE_PROBE_FAILURES: i32 = 3;
const HISTORY_PRUNE_BATCH_ROWS: i64 = 50_000;
const UNKNOWN_COUNTRY_CODE: &str = "ZZ";
const MANUAL_SWITCH_COOLDOWN_SECS: u32 = 10 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VpngateProbeRegion {
    Africa,
    Americas,
    Asia,
    Europe,
    Oceania,
}

/// Coarse regions are sufficient for catalogue placement. The GeoIP database available to the
/// Console intentionally exposes countries, not precise fleet coordinates; pretending that it
/// knows city-level distance would be less accurate than an explicit regional policy.
fn vpngate_probe_region(country_code: &str) -> Option<VpngateProbeRegion> {
    use VpngateProbeRegion::{Africa, Americas, Asia, Europe, Oceania};

    match country_code {
        "DZ" | "AO" | "BJ" | "BW" | "BF" | "BI" | "CV" | "CM" | "CF" | "TD" | "KM" | "CG"
        | "CD" | "CI" | "DJ" | "EG" | "GQ" | "ER" | "SZ" | "ET" | "GA" | "GM" | "GH" | "GN"
        | "GW" | "KE" | "LS" | "LR" | "LY" | "MG" | "MW" | "ML" | "MR" | "MU" | "YT" | "MA"
        | "MZ" | "NA" | "NE" | "NG" | "RE" | "RW" | "SH" | "ST" | "SN" | "SC" | "SL" | "SO"
        | "ZA" | "SS" | "SD" | "TZ" | "TG" | "TN" | "UG" | "EH" | "ZM" | "ZW" => Some(Africa),
        "AI" | "AG" | "AR" | "AW" | "BS" | "BB" | "BZ" | "BM" | "BO" | "BQ" | "BR" | "CA"
        | "KY" | "CL" | "CO" | "CR" | "CU" | "CW" | "DM" | "DO" | "EC" | "SV" | "FK" | "GF"
        | "GL" | "GD" | "GP" | "GT" | "GY" | "HT" | "HN" | "JM" | "MQ" | "MX" | "MS" | "NI"
        | "PA" | "PY" | "PE" | "PR" | "BL" | "KN" | "LC" | "MF" | "PM" | "VC" | "SX" | "SR"
        | "TT" | "TC" | "US" | "UY" | "VE" | "VG" | "VI" => Some(Americas),
        "AF" | "AM" | "AZ" | "BH" | "BD" | "BT" | "BN" | "KH" | "CN" | "CY" | "GE" | "HK"
        | "IN" | "ID" | "IR" | "IQ" | "IL" | "JP" | "JO" | "KZ" | "KP" | "KR" | "KW" | "KG"
        | "LA" | "LB" | "MO" | "MY" | "MV" | "MN" | "MM" | "NP" | "OM" | "PK" | "PS" | "PH"
        | "QA" | "SA" | "SG" | "LK" | "SY" | "TW" | "TJ" | "TH" | "TL" | "TM" | "AE" | "UZ"
        | "VN" | "YE" => Some(Asia),
        "AX" | "AL" | "AD" | "AT" | "BY" | "BE" | "BA" | "BG" | "HR" | "CZ" | "DK" | "EE"
        | "EU" | "FO" | "FI" | "FR" | "DE" | "GI" | "GR" | "GG" | "HU" | "IS" | "IE" | "IM"
        | "IT" | "JE" | "XK" | "LV" | "LI" | "LT" | "LU" | "MT" | "MD" | "MC" | "ME" | "NL"
        | "MK" | "NO" | "PL" | "PT" | "RO" | "RU" | "SM" | "RS" | "SK" | "SI" | "ES" | "SJ"
        | "SE" | "CH" | "TR" | "UA" | "GB" | "VA" => Some(Europe),
        "AS" | "AU" | "CK" | "FJ" | "PF" | "GU" | "KI" | "MH" | "FM" | "NR" | "NC" | "NZ"
        | "NU" | "NF" | "MP" | "PW" | "PG" | "PN" | "WS" | "SB" | "TK" | "TO" | "TV" | "UM"
        | "VU" | "WF" => Some(Oceania),
        _ => None,
    }
}

fn vpngate_probe_region_distance(target: VpngateProbeRegion, origin: VpngateProbeRegion) -> u8 {
    use VpngateProbeRegion::{Africa, Americas, Asia, Europe, Oceania};

    if target == origin {
        return 0;
    }
    match (target, origin) {
        (Asia, Oceania) | (Oceania, Asia) | (Africa, Europe) | (Europe, Africa) => 1,
        (Africa, Asia) | (Asia, Africa) | (Americas, Europe) | (Europe, Americas) => 2,
        (Americas, Oceania) | (Oceania, Americas) | (Asia, Europe) | (Europe, Asia) => 3,
        (Africa, Americas)
        | (Americas, Africa)
        | (Africa, Oceania)
        | (Oceania, Africa)
        | (Europe, Oceania)
        | (Oceania, Europe) => 4,
        (Americas, Asia) | (Asia, Americas) => 5,
        _ => unreachable!("equal regions returned above"),
    }
}

fn valid_catalog_probe_workers(workers: u8) -> bool {
    (1..=MAX_CATALOG_PROBE_WORKERS).contains(&usize::from(workers))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpngateServerInput {
    pub id: String,
    pub hostname: String,
    pub ip: String,
    pub country_code: String,
    pub country_name: String,
    pub score: u64,
    pub ping_ms: Option<u32>,
    pub speed_bps: u64,
    pub vpn_sessions: u32,
    pub uptime_millis: u64,
    pub total_users: u64,
    pub total_traffic_bytes: u64,
    pub log_type: String,
    pub operator_name: String,
    pub message: String,
    pub profile_sha256: String,
    pub remote_address: String,
    pub remote_port: u16,
    pub transport: VpngateTransport,
    pub openvpn_config: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpngateSyncBatch {
    pub content_sha256: String,
    pub fetched_rows: u32,
    pub rejected_rows: u32,
    pub servers: Vec<VpngateServerInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpngateSyncClaim {
    pub run_id: u64,
    pub lease_generation: u64,
    pub source_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VpngateSyncLeaseKind {
    Global,
    Collector,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateCatalogStatus {
    pub enabled: bool,
    pub interval_secs: u32,
    pub probe_success_cooldown_secs: u32,
    pub probe_performance_cooldown_secs: u32,
    pub probe_shard_rotation_secs: u32,
    pub source_url: String,
    pub next_sync_at_unix_secs: i64,
    pub syncing: bool,
    pub last_success_run_id: Option<u64>,
    pub last_error_code: Option<String>,
    pub last_error_detail: Option<String>,
    pub current_servers: u64,
    pub retained_servers: u64,
    pub retained_observations: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateSyncHistoryPoint {
    pub finished_at_unix_secs: i64,
    pub current_servers: u64,
    pub first_seen_servers: u64,
    pub accepted_rows: u64,
    pub rejected_rows: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateCountrySummary {
    pub country_code: String,
    pub country_name: String,
    pub current_servers: u64,
    pub retained_servers: u64,
    pub measured_successful: u64,
    pub candidate_servers: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateOverview {
    /// Lets a newer frontend avoid sending manual intent to an older Console that would ignore it.
    #[serde(default)]
    pub manual_pools_supported: bool,
    pub status: VpngateCatalogStatus,
    pub admission_policy: VpngateAdmissionPolicy,
    pub intelligence_policy: VpngateIntelligencePolicy,
    pub intelligence_credentials: VpngateIntelligenceCredentials,
    #[serde(default)]
    pub sync_history: Vec<VpngateSyncHistoryPoint>,
    pub countries: Vec<VpngateCountrySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateServerView {
    pub id: String,
    pub hostname: String,
    pub ip: String,
    pub country_code: String,
    pub country_name: String,
    pub ping_ms: Option<u32>,
    pub catalog_speed_bps: u64,
    pub vpn_sessions: u32,
    pub last_seen_at_unix_secs: i64,
    pub seen_in_latest_sync: bool,
    pub active: bool,
    /// One-based position in the complete admitted country ranking. `None` means the server did
    /// not pass the current admission policy and therefore has no candidate ordering semantics.
    pub candidate_rank: Option<u32>,
    /// Provider-local Pareto frontier. Lower layers dominate higher layers; candidates inside one
    /// layer are ordered only by the fleet's latest-per-node performance aggregate.
    pub pareto_layer: Option<u32>,
    pub global_download_bps: Option<u64>,
    pub global_connect_ms: Option<u32>,
    pub measured_nodes: u64,
    pub successful_samples: u64,
    pub latest_probe_status: Option<String>,
    pub consecutive_probe_failures: u32,
    pub probe_eligible_until_unix_secs: Option<i64>,
    pub latest_exit_ip: Option<String>,
    pub latest_exit_country_code: Option<String>,
    pub latest_connect_ms: Option<u32>,
    pub latest_download_bps: Option<u64>,
    pub latest_ip_scores: Vec<VpngateIpScore>,
    pub latest_ip_networks: Vec<VpngateIpNetwork>,
    pub latest_error_code: Option<String>,
    pub latest_probed_at_unix_secs: Option<i64>,
    pub latest_successful_probed_at_unix_secs: Option<i64>,
    pub intelligence_verified_at_unix_secs: Option<i64>,
    pub intelligence_stale: bool,
}

pub const VPNGATE_DIRECTORY_PAGE_SIZE: u32 = 100;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateDirectoryFilter {
    #[default]
    All,
    Candidate,
    Successful,
    Failed,
    Reviewing,
    Suspended,
    Pending,
    Current,
    Retained,
}

impl VpngateDirectoryFilter {
    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Candidate => "candidate",
            Self::Successful => "successful",
            Self::Failed => "failed",
            Self::Reviewing => "reviewing",
            Self::Suspended => "suspended",
            Self::Pending => "pending",
            Self::Current => "current",
            Self::Retained => "retained",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateDirectorySort {
    #[default]
    Candidate,
    Download,
    Connect,
    Catalog,
    Samples,
    Recent,
    Hostname,
}

impl VpngateDirectorySort {
    fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Download => "download",
            Self::Connect => "connect",
            Self::Catalog => "catalog",
            Self::Samples => "samples",
            Self::Recent => "recent",
            Self::Hostname => "hostname",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VpngateServerPageRequest {
    #[serde(default = "default_vpngate_directory_page")]
    pub page: u32,
    #[serde(default = "default_vpngate_directory_page_size")]
    pub page_size: u32,
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub filter: VpngateDirectoryFilter,
    #[serde(default)]
    pub sort: VpngateDirectorySort,
}

fn default_vpngate_directory_page() -> u32 {
    1
}

fn default_vpngate_directory_page_size() -> u32 {
    VPNGATE_DIRECTORY_PAGE_SIZE
}

impl Default for VpngateServerPageRequest {
    fn default() -> Self {
        Self {
            page: default_vpngate_directory_page(),
            page_size: default_vpngate_directory_page_size(),
            search: String::new(),
            filter: VpngateDirectoryFilter::default(),
            sort: VpngateDirectorySort::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateServerPage {
    pub items: Vec<VpngateServerView>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
}

#[derive(Clone)]
struct VpngateQualifiedCandidate {
    candidate: VpngateCandidateEvaluation,
    global_download_bps: u64,
    global_connect_ms: u32,
}

#[derive(Clone)]
struct VpngateRankedCandidate {
    candidate: VpngateCandidateEvaluation,
    pareto_layer: u32,
    global_download_bps: u64,
    global_connect_ms: u32,
}

#[derive(Clone)]
struct VpngateCandidateEvaluation {
    server_id: String,
    country_code: String,
    profile_sha256: String,
    verified_exit_ip: Option<String>,
    verified_exit_country_code: Option<String>,
    verified_ip_scores: Vec<VpngateIpScore>,
    verified_ip_networks: Vec<VpngateIpNetwork>,
}

struct VpngateCandidateProfile {
    server_id: String,
    hostname: String,
    country_code: String,
    remote_address: String,
    remote_port: u16,
    transport: VpngateTransport,
    profile_sha256: String,
    openvpn_config: String,
}

struct VpngateDirectoryRanking {
    server_ids: Vec<String>,
    pareto_layers: Vec<i32>,
    global_downloads: Vec<i64>,
    global_connects: Vec<i32>,
    active_limit: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateRuntimeView {
    pub node_id: String,
    pub node_name: String,
    pub tenant_id: String,
    pub outbound_id: String,
    pub outbound_name: String,
    pub country_code: String,
    pub automatic_pool: bool,
    pub runtime_status: String,
    pub selected_server_id: Option<String>,
    pub selected_hostname: Option<String>,
    pub reported_at_unix_secs: i64,
    pub latest_probe_status: Option<String>,
    pub latest_exit_ip: Option<String>,
    pub latest_exit_country_code: Option<String>,
    pub latest_connect_ms: Option<u32>,
    pub latest_download_bps: Option<u64>,
    pub latest_ip_scores: Vec<VpngateIpScore>,
    pub latest_ip_networks: Vec<VpngateIpNetwork>,
    pub latest_error_code: Option<String>,
    pub latest_error_detail: Option<String>,
    pub latest_probed_at_unix_secs: Option<i64>,
    pub latest_successful_probed_at_unix_secs: Option<i64>,
    pub intelligence_verified_at_unix_secs: Option<i64>,
    pub intelligence_stale: bool,
    pub switch_request_id: Option<u64>,
    pub switch_status: Option<String>,
    pub switch_previous_server_id: Option<String>,
    pub switch_previous_hostname: Option<String>,
    pub switch_selected_server_id: Option<String>,
    pub switch_cooldown_until_unix_secs: Option<i64>,
    pub switch_error_detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestVpngatePoolSwitch {
    pub expected_server_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngatePoolSwitchRequestView {
    pub request_id: u64,
    pub node_id: String,
    pub outbound_id: String,
    pub previous_server_id: String,
    pub status: String,
    pub selected_server_id: Option<String>,
    pub cooldown_until_unix_secs: Option<i64>,
    pub error_detail: Option<String>,
    pub requested_at_unix_secs: i64,
    pub completed_at_unix_secs: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateVpngateCatalogSettings {
    pub enabled: bool,
    pub interval_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateVpngateProbeSettings {
    pub success_cooldown_secs: u32,
    pub performance_cooldown_secs: u32,
    pub shard_rotation_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateIntelligenceCredentials {
    pub proxycheck_api_key_configured: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateVpngateIntelligenceCredentials {
    pub proxycheck_api_keys: Vec<String>,
    #[serde(default)]
    pub mode: VpngateIntelligenceCredentialUpdateMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateIntelligenceCredentialUpdateMode {
    Append,
    #[default]
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateIntelligenceRefreshMode {
    OnChange,
    Periodic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateStaleIntelligencePolicy {
    Retain,
    Mark,
    Reject,
}

/// Operational policy for deciding which exact exit IPs receive provider queries and how old
/// successful evidence participates in admission. It is deliberately independent from model
/// revisions and from provider-local risk thresholds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VpngateIntelligencePolicy {
    pub refresh_mode: VpngateIntelligenceRefreshMode,
    pub refresh_interval_hours: u32,
    pub active_window_hours: u32,
    pub stale_policy: VpngateStaleIntelligencePolicy,
    pub stale_after_hours: u32,
}

impl Default for VpngateIntelligencePolicy {
    fn default() -> Self {
        Self {
            refresh_mode: VpngateIntelligenceRefreshMode::OnChange,
            refresh_interval_hours: 7 * 24,
            active_window_hours: 72,
            stale_policy: VpngateStaleIntelligencePolicy::Retain,
            stale_after_hours: 7 * 24,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateIntelligenceRefreshResult {
    pub queued: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateReportReceipt {
    pub accepted_samples: u32,
    pub current_state_updated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateVpngateProbeNode {
    pub enabled: bool,
    #[serde(default)]
    pub workers: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateProbeNodeSelection {
    pub node_id: String,
    pub enabled: bool,
    pub workers: Option<u8>,
    pub selected_at_unix_secs: Option<i64>,
}

/// A selected, currently usable catalogue-probe machine before its public address is decorated
/// with a country by the Console's local GeoIP database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpngateProbeNodeAddress {
    pub node_id: String,
    pub public_ipv4: String,
}

/// The country observed for one currently usable catalogue-probe machine. `ZZ` means that the
/// Console's local GeoIP database could not resolve this address; retaining that machine in the
/// input lets the complete fallback queue remain sharded during a GeoIP cold start. This stays an
/// input to assignment rather than durable model state: address geolocation is operational
/// evidence and a GeoIP database refresh must not create a model revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpngateProbeNodeOrigin {
    pub node_id: String,
    pub country_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateVpngateIntelligenceNode {
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateIntelligenceNodeSelection {
    pub node_id: String,
    pub enabled: bool,
    pub selected_at_unix_secs: Option<i64>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct VpngateIpIntelligenceClaim {
    pub exit_ip: String,
    pub lease_generation: u64,
    pub proxycheck_api_key: Option<String>,
}

pub async fn catalog_status(pool: &PgPool) -> Result<VpngateCatalogStatus> {
    let row = sqlx::query(
        "SELECT state.enabled, state.interval_secs,
                state.probe_success_cooldown_secs, state.probe_performance_cooldown_secs,
                state.probe_shard_rotation_secs,
                state.source_url,
                EXTRACT(EPOCH FROM COALESCE(
                    (SELECT MIN(catalogue_next_sync_at) FROM vpngate_intelligence_nodes),
                    state.next_sync_at
                ))::BIGINT AS next_sync_at_unix_secs,
                (state.lease_until IS NOT NULL AND state.lease_until > now())
                    OR EXISTS (
                        SELECT 1 FROM vpngate_intelligence_nodes
                         WHERE catalogue_lease_until > now()
                    ) AS syncing,
                state.last_success_run_id,
                CASE WHEN EXISTS (
                    SELECT 1 FROM vpngate_intelligence_nodes collector
                     WHERE collector.catalogue_last_error_code = state.last_error_code
                       AND collector.catalogue_last_error_detail IS NOT DISTINCT FROM state.last_error_detail
                ) THEN NULL::TEXT ELSE state.last_error_code END AS last_error_code,
                CASE WHEN EXISTS (
                    SELECT 1 FROM vpngate_intelligence_nodes collector
                     WHERE collector.catalogue_last_error_code = state.last_error_code
                       AND collector.catalogue_last_error_detail IS NOT DISTINCT FROM state.last_error_detail
                ) THEN NULL::TEXT ELSE state.last_error_detail END AS last_error_detail,
                (SELECT COUNT(*) FROM vpngate_servers
                  WHERE current AND country_code <> $1) AS current_servers,
                (SELECT COUNT(*) FROM vpngate_servers
                  WHERE country_code <> $1) AS retained_servers,
                state.retained_observation_count AS retained_observations
           FROM vpngate_catalog_state state
          WHERE state.id = TRUE",
    )
    .bind(UNKNOWN_COUNTRY_CODE)
    .fetch_one(pool)
    .await?;
    Ok(VpngateCatalogStatus {
        enabled: row.try_get("enabled")?,
        interval_secs: i32_to_u32("interval_secs", row.try_get("interval_secs")?)?,
        probe_success_cooldown_secs: i32_to_u32(
            "probe_success_cooldown_secs",
            row.try_get("probe_success_cooldown_secs")?,
        )?,
        probe_performance_cooldown_secs: i32_to_u32(
            "probe_performance_cooldown_secs",
            row.try_get("probe_performance_cooldown_secs")?,
        )?,
        probe_shard_rotation_secs: i32_to_u32(
            "probe_shard_rotation_secs",
            row.try_get("probe_shard_rotation_secs")?,
        )?,
        source_url: row.try_get("source_url")?,
        next_sync_at_unix_secs: row.try_get("next_sync_at_unix_secs")?,
        syncing: row.try_get("syncing")?,
        last_success_run_id: optional_i64_to_u64(
            "last_success_run_id",
            row.try_get("last_success_run_id")?,
        )?,
        last_error_code: row.try_get("last_error_code")?,
        last_error_detail: row.try_get("last_error_detail")?,
        current_servers: i64_to_u64("current_servers", row.try_get("current_servers")?)?,
        retained_servers: i64_to_u64("retained_servers", row.try_get("retained_servers")?)?,
        retained_observations: i64_to_u64(
            "retained_observations",
            row.try_get("retained_observations")?,
        )?,
    })
}

pub async fn overview(pool: &PgPool) -> Result<VpngateOverview> {
    let policy_and_counts = async {
        let (admission_policy, intelligence_policy) =
            tokio::try_join!(admission_policy(pool), intelligence_policy(pool))?;
        let candidate_counts =
            active_candidate_counts(pool, &admission_policy, &intelligence_policy).await?;
        Ok::<_, StoreError>((admission_policy, intelligence_policy, candidate_counts))
    };
    let (status, policy_and_counts, intelligence_credentials, sync_history, mut countries) = tokio::try_join!(
        catalog_status(pool),
        policy_and_counts,
        intelligence_credentials(pool),
        catalog_sync_history(pool),
        catalog_country_summaries(pool),
    )?;
    let (admission_policy, intelligence_policy, candidate_counts) = policy_and_counts;
    for country in &mut countries {
        country.candidate_servers = candidate_counts
            .get(&country.country_code)
            .copied()
            .unwrap_or(0);
    }
    Ok(VpngateOverview {
        manual_pools_supported: true,
        status,
        admission_policy,
        intelligence_policy,
        intelligence_credentials,
        sync_history,
        countries,
    })
}

async fn catalog_country_summaries(pool: &PgPool) -> Result<Vec<VpngateCountrySummary>> {
    // Availability is a fleet fact: one current node-local success is sufficient. Aggregating the
    // bounded latest projection once is both more accurate and cheaper than choosing one arbitrary
    // probing node through thousands of per-server lateral index lookups.
    let rows = sqlx::query(
        "WITH measured AS MATERIALIZED (
             SELECT sample.server_id, sample.profile_sha256,
                    BOOL_OR(sample.status = 'succeeded') AS successful
               FROM vpngate_candidate_probe_latest sample
              GROUP BY sample.server_id, sample.profile_sha256
         )
         SELECT server.country_code,
                MIN(server.country_name) AS country_name,
                COUNT(*) FILTER (WHERE server.current) AS current_servers,
                COUNT(*) AS retained_servers,
                COUNT(*) FILTER (WHERE measured.successful) AS measured_successful
           FROM vpngate_servers server
           LEFT JOIN measured
             ON measured.server_id = server.id
            AND measured.profile_sha256 = server.profile_sha256
          WHERE server.country_code <> $1
          GROUP BY server.country_code
          ORDER BY server.country_code",
    )
    .bind(UNKNOWN_COUNTRY_CODE)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let country_code: String = row.try_get("country_code")?;
            Ok(VpngateCountrySummary {
                candidate_servers: 0,
                country_code,
                country_name: row.try_get("country_name")?,
                current_servers: i64_to_u64("current_servers", row.try_get("current_servers")?)?,
                retained_servers: i64_to_u64("retained_servers", row.try_get("retained_servers")?)?,
                measured_successful: i64_to_u64(
                    "measured_successful",
                    row.try_get("measured_successful")?,
                )?,
            })
        })
        .collect()
}

async fn catalog_sync_history(pool: &PgPool) -> Result<Vec<VpngateSyncHistoryPoint>> {
    let rows = sqlx::query(
        "WITH recent_events AS (
                SELECT id, finished_at,
                       EXTRACT(EPOCH FROM finished_at)::BIGINT AS finished_at_unix_secs,
                       accepted_rows, rejected_rows
                  FROM vpngate_sync_runs
                 WHERE status = 'succeeded'
                 ORDER BY finished_at DESC, id DESC
                 LIMIT 24
           ), history AS (
                SELECT event.id, event.finished_at_unix_secs,
                       CASE WHEN EXISTS (
                                SELECT 1
                                  FROM vpngate_intelligence_nodes selected
                                  JOIN vpngate_sync_runs run ON run.worker_id = selected.node_id
                                 WHERE run.status = 'succeeded'
                                   AND run.finished_at <= event.finished_at
                            )
                            THEN (
                                SELECT COUNT(DISTINCT observation.server_id)
                                  FROM vpngate_intelligence_nodes selected
                                  JOIN LATERAL (
                                       SELECT run.id
                                         FROM vpngate_sync_runs run
                                        WHERE run.worker_id = selected.node_id
                                          AND run.status = 'succeeded'
                                          AND run.finished_at <= event.finished_at
                                        ORDER BY run.finished_at DESC, run.id DESC
                                        LIMIT 1
                                  ) latest ON TRUE
                                  JOIN vpngate_server_observations observation
                                    ON observation.sync_run_id = latest.id
                                 WHERE observation.country_code <> $1
                            )
                            ELSE (
                                SELECT COUNT(*)
                                  FROM vpngate_server_observations observation
                                 WHERE observation.sync_run_id = event.id
                                   AND observation.country_code <> $1
                            )
                       END AS current_servers,
                       (
                           SELECT COUNT(*)
                             FROM vpngate_servers catalog_server
                            WHERE catalog_server.first_seen_run_id = event.id
                              AND catalog_server.country_code <> $1
                       ) AS first_seen_servers,
                       event.accepted_rows, event.rejected_rows
                  FROM recent_events event
           )
           SELECT history.finished_at_unix_secs, history.current_servers,
                  history.first_seen_servers,
                  history.accepted_rows, history.rejected_rows
             FROM history
            ORDER BY history.finished_at_unix_secs, history.id",
    )
    .bind(UNKNOWN_COUNTRY_CODE)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(VpngateSyncHistoryPoint {
                finished_at_unix_secs: row.try_get("finished_at_unix_secs")?,
                current_servers: i64_to_u64("current_servers", row.try_get("current_servers")?)?,
                first_seen_servers: i64_to_u64(
                    "first_seen_servers",
                    row.try_get("first_seen_servers")?,
                )?,
                accepted_rows: u64::from(i32_to_u32(
                    "accepted_rows",
                    row.try_get("accepted_rows")?,
                )?),
                rejected_rows: u64::from(i32_to_u32(
                    "rejected_rows",
                    row.try_get("rejected_rows")?,
                )?),
            })
        })
        .collect()
}

pub async fn admission_policy(pool: &PgPool) -> Result<VpngateAdmissionPolicy> {
    let value = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT admission_policy FROM vpngate_catalog_state WHERE id = TRUE",
    )
    .fetch_one(pool)
    .await?;
    let policy = decode_json("admission_policy", value)?;
    if !validate_vpngate_admission_policy(&policy) {
        return Err(StoreError::InvalidData(
            "stored VPN Gate admission policy is invalid".to_owned(),
        ));
    }
    Ok(policy)
}

pub async fn update_admission_policy(
    pool: &PgPool,
    actor: &AdminContext,
    policy: VpngateAdmissionPolicy,
) -> Result<VpngateAdmissionPolicy> {
    require_system_admin(actor, "configure VPN Gate admission")?;
    if !validate_vpngate_admission_policy(&policy) {
        return Err(StoreError::InvalidData(
            "VPN Gate admission policy must contain one 0–100 rule per provider and require 1–3 sources"
                .to_owned(),
        ));
    }
    let value = serde_json::to_value(&policy)
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
    sqlx::query(
        "UPDATE vpngate_catalog_state
            SET admission_policy = $1, updated_at = now()
          WHERE id = TRUE",
    )
    .bind(value)
    .execute(pool)
    .await?;
    Ok(policy)
}

fn validate_intelligence_policy(policy: &VpngateIntelligencePolicy) -> bool {
    let valid_hours = |hours: u32| {
        (INTELLIGENCE_POLICY_MIN_HOURS..=INTELLIGENCE_POLICY_MAX_HOURS).contains(&hours)
    };
    valid_hours(policy.refresh_interval_hours)
        && valid_hours(policy.active_window_hours)
        && valid_hours(policy.stale_after_hours)
}

fn intelligence_hours_to_secs(field: &str, hours: u32) -> Result<i32> {
    let seconds = hours.checked_mul(60 * 60).ok_or_else(|| {
        StoreError::InvalidData(format!("VPN Gate {field} is outside the supported range"))
    })?;
    i32::try_from(seconds).map_err(|_| {
        StoreError::InvalidData(format!("VPN Gate {field} is outside the supported range"))
    })
}

fn decode_intelligence_policy(value: serde_json::Value) -> Result<VpngateIntelligencePolicy> {
    let policy = decode_json("intelligence_policy", value)?;
    if !validate_intelligence_policy(&policy) {
        return Err(StoreError::InvalidData(
            "stored VPN Gate intelligence policy is invalid".to_owned(),
        ));
    }
    Ok(policy)
}

pub async fn intelligence_policy(pool: &PgPool) -> Result<VpngateIntelligencePolicy> {
    let value = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT intelligence_policy FROM vpngate_catalog_state WHERE id = TRUE",
    )
    .fetch_one(pool)
    .await?;
    decode_intelligence_policy(value)
}

pub async fn intelligence_credentials(pool: &PgPool) -> Result<VpngateIntelligenceCredentials> {
    let proxycheck_api_key_configured = sqlx::query_scalar::<_, bool>(
        "SELECT proxycheck_api_key_sealed IS NOT NULL
           FROM vpngate_catalog_state
          WHERE id = TRUE",
    )
    .fetch_one(pool)
    .await?;
    Ok(VpngateIntelligenceCredentials {
        proxycheck_api_key_configured,
    })
}

fn valid_proxycheck_api_key(value: &str) -> bool {
    let mut groups = value.split('-');
    (0..4).all(|_| {
        groups.next().is_some_and(|group| {
            group.len() == 6 && group.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    }) && groups.next().is_none()
}

const MAX_PROXYCHECK_API_KEYS: usize = 32;

#[derive(Serialize, Deserialize)]
struct StoredProxycheckCredentials {
    keys: Vec<String>,
    next_key_index: usize,
}

fn proxycheck_start_index(entropy: [u8; 8], key_count: usize) -> usize {
    debug_assert!(key_count > 0);
    let key_count = u64::try_from(key_count).expect("ProxyCheck key count is bounded to 32");
    usize::try_from(u64::from_le_bytes(entropy) % key_count)
        .expect("ProxyCheck key index is smaller than the bounded key count")
}

fn random_proxycheck_start_index(key_count: usize) -> Result<usize> {
    let mut entropy = [0_u8; 8];
    getrandom::fill(&mut entropy)?;
    Ok(proxycheck_start_index(entropy, key_count))
}

fn validate_proxycheck_api_keys(keys: &[String]) -> Result<()> {
    if keys.is_empty() || keys.len() > MAX_PROXYCHECK_API_KEYS {
        return Err(StoreError::InvalidData(format!(
            "ProxyCheck API key list must contain between 1 and {MAX_PROXYCHECK_API_KEYS} keys"
        )));
    }
    if keys.iter().any(|key| !valid_proxycheck_api_key(key)) {
        return Err(StoreError::InvalidData(
            "each ProxyCheck API key must contain four six-character groups".to_owned(),
        ));
    }
    let unique = keys.iter().collect::<std::collections::BTreeSet<_>>();
    if unique.len() != keys.len() {
        return Err(StoreError::InvalidData(
            "ProxyCheck API key list contains duplicates".to_owned(),
        ));
    }
    Ok(())
}

fn decode_proxycheck_credentials(sealed: &str) -> Result<StoredProxycheckCredentials> {
    let plaintext = crate::secrets::open(crate::secrets::CTX_PROXYCHECK_API_KEY, sealed)?;
    let credentials = match serde_json::from_str::<StoredProxycheckCredentials>(&plaintext) {
        Ok(credentials) => credentials,
        Err(_) => match serde_json::from_str::<Vec<String>>(&plaintext) {
            Ok(keys) => {
                validate_proxycheck_api_keys(&keys)?;
                let next_key_index = random_proxycheck_start_index(keys.len())?;
                StoredProxycheckCredentials {
                    keys,
                    next_key_index,
                }
            }
            Err(_) if valid_proxycheck_api_key(&plaintext) => StoredProxycheckCredentials {
                keys: vec![plaintext],
                next_key_index: 0,
            },
            Err(_) => {
                return Err(StoreError::InvalidData(
                    "stored ProxyCheck API key pool is invalid".to_owned(),
                ));
            }
        },
    };
    validate_proxycheck_api_keys(&credentials.keys)?;
    Ok(credentials)
}

fn seal_proxycheck_credentials(credentials: &StoredProxycheckCredentials) -> Result<String> {
    let plaintext = serde_json::to_string(credentials)
        .map_err(|_| StoreError::InvalidData("cannot encode ProxyCheck API key pool".to_owned()))?;
    crate::secrets::seal(crate::secrets::CTX_PROXYCHECK_API_KEY, &plaintext)
}

pub async fn update_intelligence_credentials(
    pool: &PgPool,
    actor: &AdminContext,
    request: UpdateVpngateIntelligenceCredentials,
) -> Result<VpngateIntelligenceCredentials> {
    require_system_admin(actor, "configure VPN Gate intelligence credentials")?;
    validate_proxycheck_api_keys(&request.proxycheck_api_keys)?;
    let mut tx = pool.begin().await?;
    let sealed_current = sqlx::query_scalar::<_, Option<String>>(
        "SELECT proxycheck_api_key_sealed
           FROM vpngate_catalog_state
          WHERE id = TRUE
          FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await?;
    let keys = match request.mode {
        VpngateIntelligenceCredentialUpdateMode::Replace => request.proxycheck_api_keys,
        VpngateIntelligenceCredentialUpdateMode::Append => {
            let mut keys = sealed_current
                .as_deref()
                .map(decode_proxycheck_credentials)
                .transpose()?
                .map(|credentials| credentials.keys)
                .unwrap_or_default();
            for key in request.proxycheck_api_keys {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
            validate_proxycheck_api_keys(&keys)?;
            keys
        }
    };
    // An updated pool must not always spend the first listed key first. Choose one random starting
    // position, then retain the cursor so subsequent claims enumerate the complete pool fairly.
    let next_key_index = random_proxycheck_start_index(keys.len())?;
    let sealed = seal_proxycheck_credentials(&StoredProxycheckCredentials {
        keys,
        next_key_index,
    })?;
    sqlx::query(
        "UPDATE vpngate_catalog_state
            SET proxycheck_api_key_sealed = $1,
                updated_at = now()
          WHERE id = TRUE",
    )
    .bind(sealed)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(VpngateIntelligenceCredentials {
        proxycheck_api_key_configured: true,
    })
}

pub async fn update_intelligence_policy(
    pool: &PgPool,
    actor: &AdminContext,
    policy: VpngateIntelligencePolicy,
) -> Result<VpngateIntelligencePolicy> {
    require_system_admin(actor, "configure VPN Gate intelligence")?;
    if !validate_intelligence_policy(&policy) {
        return Err(StoreError::InvalidData(format!(
            "VPN Gate intelligence policy hours must be between {INTELLIGENCE_POLICY_MIN_HOURS} and {INTELLIGENCE_POLICY_MAX_HOURS}"
        )));
    }
    let refresh_secs = intelligence_hours_to_secs(
        "intelligence refresh interval",
        policy.refresh_interval_hours,
    )?;
    let value = serde_json::to_value(&policy)
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE vpngate_catalog_state
            SET intelligence_policy = $1, updated_at = now()
          WHERE id = TRUE",
    )
    .bind(value)
    .execute(&mut *tx)
    .await?;
    match policy.refresh_mode {
        VpngateIntelligenceRefreshMode::OnChange => {
            sqlx::query(
                "UPDATE vpngate_exit_reputations
                    SET next_check_at = 'infinity'::timestamptz, updated_at = now()
                  WHERE verified_at IS NOT NULL AND lease_until IS NULL",
            )
            .execute(&mut *tx)
            .await?;
        }
        VpngateIntelligenceRefreshMode::Periodic => {
            sqlx::query(
                "UPDATE vpngate_exit_reputations
                    SET next_check_at = LEAST(
                            next_check_at,
                            COALESCE(verified_at + make_interval(secs => $1), now())
                        ),
                        updated_at = now()
                  WHERE lease_until IS NULL",
            )
            .bind(refresh_secs)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(policy)
}

pub async fn request_intelligence_refresh(
    pool: &PgPool,
    actor: &AdminContext,
) -> Result<VpngateIntelligenceRefreshResult> {
    require_system_admin(actor, "refresh VPN Gate intelligence")?;
    let policy = intelligence_policy(pool).await?;
    let active_secs =
        intelligence_hours_to_secs("intelligence active window", policy.active_window_hours)?;
    let affected = sqlx::query(
        "UPDATE vpngate_exit_reputations
            SET next_check_at = now(), updated_at = now()
          WHERE last_seen_at >= now() - make_interval(secs => $1)
            AND (lease_until IS NULL OR lease_until <= now())",
    )
    .bind(active_secs)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(VpngateIntelligenceRefreshResult { queued: affected })
}

pub async fn country_server_page(
    pool: &PgPool,
    country_code: &str,
    request: VpngateServerPageRequest,
) -> Result<VpngateServerPage> {
    country_server_page_inner(pool, country_code, request, true).await
}

pub async fn country_servers(pool: &PgPool, country_code: &str) -> Result<Vec<VpngateServerView>> {
    let page = country_server_page_inner(
        pool,
        country_code,
        VpngateServerPageRequest {
            page_size: 100_000,
            ..VpngateServerPageRequest::default()
        },
        false,
    )
    .await?;
    Ok(page.items)
}

async fn country_server_page_inner(
    pool: &PgPool,
    country_code: &str,
    request: VpngateServerPageRequest,
    enforce_page_limit: bool,
) -> Result<VpngateServerPage> {
    let country_code = normalize_country_code(country_code)?;
    if request.page == 0
        || request.page_size == 0
        || (enforce_page_limit && request.page_size > VPNGATE_DIRECTORY_PAGE_SIZE)
        || request.search.chars().count() > 128
    {
        return Err(StoreError::InvalidData(format!(
            "VPN Gate directory pages must use page >= 1, page_size 1–{VPNGATE_DIRECTORY_PAGE_SIZE}, and search terms no longer than 128 characters"
        )));
    }
    if country_code == UNKNOWN_COUNTRY_CODE {
        return Ok(VpngateServerPage {
            items: Vec::new(),
            total: 0,
            page: request.page,
            page_size: request.page_size,
        });
    }
    let (policy, intelligence_policy) =
        tokio::try_join!(admission_policy(pool), intelligence_policy(pool))?;
    let ranked = ranked_candidates(pool, &country_code, &policy, &intelligence_policy).await?;
    let active_limit = usize::try_from(MAX_ACTIVE_CANDIDATES_PER_COUNTRY)
        .expect("active VPN Gate candidate limit fits usize");
    let ranking = VpngateDirectoryRanking {
        server_ids: ranked
            .iter()
            .map(|candidate| candidate.candidate.server_id.clone())
            .collect(),
        pareto_layers: ranked
            .iter()
            .map(|candidate| i32::try_from(candidate.pareto_layer).unwrap_or(i32::MAX))
            .collect(),
        global_downloads: ranked
            .iter()
            .map(|candidate| u64_to_i64("global_download_bps", candidate.global_download_bps))
            .collect::<Result<Vec<_>>>()?,
        global_connects: ranked
            .iter()
            .map(|candidate| u32_to_i32("global_connect_ms", candidate.global_connect_ms))
            .collect::<Result<Vec<_>>>()?,
        active_limit: i64::try_from(active_limit).unwrap_or(i64::MAX),
    };
    let page = i64::from(request.page);
    let page_size = i64::from(request.page_size);
    let offset = page
        .checked_sub(1)
        .and_then(|value| value.checked_mul(page_size))
        .ok_or_else(|| {
            StoreError::InvalidData("VPN Gate directory page offset is too large".to_owned())
        })?;
    let search = request.search.trim();
    let mark_stale = !matches!(
        intelligence_policy.stale_policy,
        VpngateStaleIntelligencePolicy::Retain
    );
    let stale_after_secs = intelligence_hours_to_secs(
        "intelligence stale age",
        intelligence_policy.stale_after_hours,
    )?;
    // The directory and automatic pools share this exact order: Pareto frontier first, followed
    // by fleet download and connect quality computed from one fresh sample per probing machine.
    // Observation counts never affect the order, so a long-running collector cannot accumulate
    // rank merely by producing more samples.
    let rows = if search.is_empty()
        && request.filter == VpngateDirectoryFilter::All
        && request.sort == VpngateDirectorySort::Candidate
    {
        default_candidate_directory_page_rows(
            pool,
            &country_code,
            &ranking,
            mark_stale,
            stale_after_secs,
            page_size,
            offset,
        )
        .await?
    } else {
        sqlx::query(
        "WITH ranked AS (
             SELECT candidate.server_id, candidate.pareto_layer,
                    candidate.global_download_bps, candidate.global_connect_ms,
                    candidate.candidate_rank
               FROM unnest($2::TEXT[], $3::INTEGER[], $4::BIGINT[], $5::INTEGER[])
                    WITH ORDINALITY AS candidate(
                        server_id, pareto_layer, global_download_bps,
                        global_connect_ms, candidate_rank
                    )
         ), country_servers AS (
             SELECT id, profile_sha256
               FROM vpngate_servers
              WHERE country_code = $1
         ), measured AS (
             SELECT sample.server_id, sample.profile_sha256,
                    COUNT(DISTINCT sample.node_id) AS measured_nodes,
                    COUNT(*) FILTER (WHERE sample.last_success_probed_at IS NOT NULL)
                        AS successful_samples,
                    (array_agg(sample.status ORDER BY sample.probed_at DESC,
                        sample.received_at DESC, sample.node_id))[1]
                        AS latest_probe_status,
                    (array_agg(sample.error_code ORDER BY sample.probed_at DESC,
                        sample.received_at DESC, sample.node_id))[1]
                        AS latest_error_code,
                    MAX(sample.probed_at) AS latest_probed_at,
                    (array_agg(sample.last_success_exit_ip
                        ORDER BY sample.last_success_probed_at DESC, sample.node_id)
                        FILTER (WHERE sample.last_success_probed_at IS NOT NULL))[1]
                        AS last_success_exit_ip,
                    (array_agg(sample.last_success_connect_ms
                        ORDER BY sample.last_success_probed_at DESC, sample.node_id)
                        FILTER (WHERE sample.last_success_probed_at IS NOT NULL))[1]
                        AS last_success_connect_ms,
                    (array_agg(sample.last_success_download_bps
                        ORDER BY sample.last_success_probed_at DESC, sample.node_id)
                        FILTER (WHERE sample.last_success_probed_at IS NOT NULL))[1]
                        AS last_success_download_bps,
                    MAX(sample.last_success_probed_at) AS last_success_probed_at,
                    MAX(sample.last_success_received_at) AS last_success_received_at
               FROM country_servers server
               JOIN vpngate_candidate_probe_latest sample
                 ON sample.server_id = server.id
                AND sample.profile_sha256 = server.profile_sha256
              GROUP BY sample.server_id, sample.profile_sha256
         ), directory AS (
             SELECT server.id, server.hostname, host(server.ip) AS ip, server.country_code,
                server.country_name, server.ping_ms,
                server.speed_bps AS catalog_speed_bps, server.vpn_sessions,
                EXTRACT(EPOCH FROM server.last_seen_at)::BIGINT AS last_seen_at_unix_secs,
                server.current AS seen_in_latest_sync,
                ranked.candidate_rank IS NOT NULL
                    AND ranked.candidate_rank <= $6 AS active,
                ranked.candidate_rank,
                ranked.pareto_layer,
                ranked.global_download_bps,
                ranked.global_connect_ms,
                COALESCE(measured.measured_nodes, 0) AS measured_nodes,
                COALESCE(measured.successful_samples, 0) AS successful_samples,
                measured.latest_probe_status,
                COALESCE(probe_state.consecutive_failures, 0)
                    AS consecutive_probe_failures,
                EXTRACT(EPOCH FROM probe_state.failure_streak_started_at
                    + make_interval(secs => $14))::BIGINT
                    AS probe_eligible_until_unix_secs,
                host(measured.last_success_exit_ip) AS latest_exit_ip,
                reputation.country_code AS latest_exit_country_code,
                NULLIF(measured.last_success_connect_ms, 0) AS latest_connect_ms,
                measured.last_success_download_bps AS latest_download_bps,
                COALESCE(reputation.ip_scores, '[]'::jsonb) AS latest_ip_scores,
                COALESCE(reputation.ip_networks, '[]'::jsonb) AS latest_ip_networks,
                measured.latest_error_code,
                EXTRACT(EPOCH FROM measured.latest_probed_at)::BIGINT
                    AS latest_probed_at_unix_secs,
                EXTRACT(EPOCH FROM measured.last_success_probed_at)::BIGINT
                    AS latest_successful_probed_at_unix_secs,
                EXTRACT(EPOCH FROM reputation.verified_at)::BIGINT
                    AS intelligence_verified_at_unix_secs,
                ($7 AND reputation.verified_at < now() - make_interval(secs => $8))
                    AS intelligence_stale
           FROM vpngate_servers server
           LEFT JOIN ranked ON ranked.server_id = server.id
           LEFT JOIN measured
             ON measured.server_id = server.id
            AND measured.profile_sha256 = server.profile_sha256
           LEFT JOIN vpngate_candidate_probe_state probe_state
             ON probe_state.server_id = server.id
            AND probe_state.profile_sha256 = server.profile_sha256
           LEFT JOIN vpngate_exit_reputations reputation
             ON reputation.exit_ip = measured.last_success_exit_ip
          WHERE server.country_code = $1
         ), filtered AS (
             SELECT directory.*
               FROM directory
              WHERE CASE $10::TEXT
                        WHEN 'candidate' THEN directory.active
                        WHEN 'successful' THEN directory.successful_samples > 0
                        WHEN 'failed' THEN directory.latest_probe_status = 'failed'
                        WHEN 'reviewing' THEN
                            directory.consecutive_probe_failures BETWEEN 1 AND 2
                            AND directory.probe_eligible_until_unix_secs
                                >= EXTRACT(EPOCH FROM now())::BIGINT
                        WHEN 'suspended' THEN
                            directory.consecutive_probe_failures >= 3
                            OR (directory.consecutive_probe_failures BETWEEN 1 AND 2
                                AND directory.probe_eligible_until_unix_secs
                                    < EXTRACT(EPOCH FROM now())::BIGINT)
                        WHEN 'pending' THEN directory.latest_probe_status IS NULL
                        WHEN 'current' THEN directory.seen_in_latest_sync
                        WHEN 'retained' THEN NOT directory.seen_in_latest_sync
                        ELSE TRUE
                    END
                AND ($9::TEXT = '' OR
                     LOWER(directory.id) LIKE '%' || LOWER($9) || '%' OR
                     LOWER(directory.hostname) LIKE '%' || LOWER($9) || '%' OR
                     LOWER(directory.ip) LIKE '%' || LOWER($9) || '%' OR
                     LOWER(COALESCE(directory.latest_exit_ip, '')) LIKE '%' || LOWER($9) || '%' OR
                     LOWER(COALESCE(directory.latest_exit_country_code, '')) LIKE '%' || LOWER($9) || '%' OR
                     LOWER(COALESCE(directory.latest_error_code, '')) LIKE '%' || LOWER($9) || '%' OR
                     LOWER(directory.latest_ip_scores::TEXT) LIKE '%' || LOWER($9) || '%' OR
                     LOWER(directory.latest_ip_networks::TEXT) LIKE '%' || LOWER($9) || '%')
         ), counted AS (
             SELECT COUNT(*) AS filtered_total FROM filtered
         )
         SELECT page.*, counted.filtered_total
           FROM counted
           LEFT JOIN LATERAL (
                SELECT * FROM filtered
                 ORDER BY
                    CASE WHEN $11 = 'candidate' THEN candidate_rank END ASC NULLS LAST,
                    CASE WHEN $11 = 'download' THEN global_download_bps END DESC NULLS LAST,
                    CASE WHEN $11 = 'connect' THEN global_connect_ms END ASC NULLS LAST,
                    CASE WHEN $11 = 'catalog' THEN catalog_speed_bps END DESC NULLS LAST,
                    CASE WHEN $11 = 'catalog' THEN ping_ms END ASC NULLS LAST,
                    CASE WHEN $11 = 'samples' THEN successful_samples END DESC NULLS LAST,
                    CASE WHEN $11 = 'recent' THEN latest_probed_at_unix_secs END DESC NULLS LAST,
                    CASE WHEN $11 = 'hostname' THEN hostname END ASC NULLS LAST,
                    candidate_rank ASC NULLS LAST,
                    id
                 LIMIT $12 OFFSET $13
           ) page ON TRUE",
        )
        .bind(&country_code)
        .bind(&ranking.server_ids)
        .bind(&ranking.pareto_layers)
        .bind(&ranking.global_downloads)
        .bind(&ranking.global_connects)
        .bind(ranking.active_limit)
        .bind(mark_stale)
        .bind(stale_after_secs)
        .bind(search)
        .bind(request.filter.as_str())
        .bind(request.sort.as_str())
        .bind(page_size)
        .bind(offset)
        .bind(FAILED_CANDIDATE_GRACE_SECS)
        .fetch_all(pool)
        .await?
    };
    let total = rows
        .first()
        .map(|row| i64_to_u64("filtered_total", row.try_get("filtered_total")?))
        .transpose()?
        .unwrap_or(0);
    let items = rows
        .iter()
        .map(|row| {
            let Some(id) = row.try_get::<Option<String>, _>("id")? else {
                return Ok(None);
            };
            Ok(Some(VpngateServerView {
                id,
                hostname: row.try_get("hostname")?,
                ip: row.try_get("ip")?,
                country_code: row.try_get("country_code")?,
                country_name: row.try_get("country_name")?,
                ping_ms: optional_i32_to_u32("ping_ms", row.try_get("ping_ms")?)?,
                catalog_speed_bps: i64_to_u64(
                    "catalog_speed_bps",
                    row.try_get("catalog_speed_bps")?,
                )?,
                vpn_sessions: i32_to_u32("vpn_sessions", row.try_get("vpn_sessions")?)?,
                last_seen_at_unix_secs: row.try_get("last_seen_at_unix_secs")?,
                seen_in_latest_sync: row.try_get("seen_in_latest_sync")?,
                active: row.try_get("active")?,
                candidate_rank: optional_i64_to_u64(
                    "candidate_rank",
                    row.try_get("candidate_rank")?,
                )?
                .map(|value| u32::try_from(value).unwrap_or(u32::MAX)),
                pareto_layer: optional_i32_to_u32("pareto_layer", row.try_get("pareto_layer")?)?,
                global_download_bps: optional_i64_to_u64(
                    "global_download_bps",
                    row.try_get("global_download_bps")?,
                )?,
                global_connect_ms: optional_i32_to_u32(
                    "global_connect_ms",
                    row.try_get("global_connect_ms")?,
                )?,
                measured_nodes: i64_to_u64("measured_nodes", row.try_get("measured_nodes")?)?,
                successful_samples: i64_to_u64(
                    "successful_samples",
                    row.try_get("successful_samples")?,
                )?,
                latest_probe_status: row.try_get("latest_probe_status")?,
                consecutive_probe_failures: optional_i32_to_u32(
                    "consecutive_probe_failures",
                    row.try_get("consecutive_probe_failures")?,
                )?
                .unwrap_or(0),
                probe_eligible_until_unix_secs: row.try_get("probe_eligible_until_unix_secs")?,
                latest_exit_ip: row.try_get("latest_exit_ip")?,
                latest_exit_country_code: row.try_get("latest_exit_country_code")?,
                latest_connect_ms: optional_i32_to_u32(
                    "latest_connect_ms",
                    row.try_get("latest_connect_ms")?,
                )?,
                latest_download_bps: optional_i64_to_u64(
                    "latest_download_bps",
                    row.try_get("latest_download_bps")?,
                )?,
                latest_ip_scores: decode_json(
                    "latest_ip_scores",
                    row.try_get("latest_ip_scores")?,
                )?,
                latest_ip_networks: decode_json(
                    "latest_ip_networks",
                    row.try_get("latest_ip_networks")?,
                )?,
                latest_error_code: row.try_get("latest_error_code")?,
                latest_probed_at_unix_secs: row.try_get("latest_probed_at_unix_secs")?,
                latest_successful_probed_at_unix_secs: row
                    .try_get("latest_successful_probed_at_unix_secs")?,
                intelligence_verified_at_unix_secs: row
                    .try_get("intelligence_verified_at_unix_secs")?,
                intelligence_stale: row
                    .try_get::<Option<bool>, _>("intelligence_stale")?
                    .unwrap_or(false),
            }))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    Ok(VpngateServerPage {
        items,
        total,
        page: request.page,
        page_size: request.page_size,
    })
}

async fn default_candidate_directory_page_rows(
    pool: &PgPool,
    country_code: &str,
    ranking: &VpngateDirectoryRanking,
    mark_stale: bool,
    stale_after_secs: i32,
    page_size: i64,
    offset: i64,
) -> Result<Vec<sqlx::postgres::PgRow>> {
    // Candidate order depends only on the lightweight ranking projection. Select the requested
    // page first, then aggregate probe and reputation details for at most one page of servers.
    // Search and observation-dependent filters deliberately use the complete query instead.
    Ok(sqlx::query(
        "WITH ranked AS (
             SELECT candidate.server_id, candidate.pareto_layer,
                    candidate.global_download_bps, candidate.global_connect_ms,
                    candidate.candidate_rank
               FROM unnest($2::TEXT[], $3::INTEGER[], $4::BIGINT[], $5::INTEGER[])
                    WITH ORDINALITY AS candidate(
                        server_id, pareto_layer, global_download_bps,
                        global_connect_ms, candidate_rank
                    )
         ), country_servers AS MATERIALIZED (
             SELECT server.id, server.hostname, server.ip, server.country_code,
                    server.country_name, server.ping_ms, server.speed_bps,
                    server.vpn_sessions, server.last_seen_at, server.current,
                    server.profile_sha256,
                    ranked.candidate_rank IS NOT NULL
                        AND ranked.candidate_rank <= $6 AS active,
                    ranked.candidate_rank, ranked.pareto_layer,
                    ranked.global_download_bps, ranked.global_connect_ms
               FROM vpngate_servers server
               LEFT JOIN ranked ON ranked.server_id = server.id
              WHERE server.country_code = $1
         ), page_servers AS MATERIALIZED (
             SELECT *
               FROM country_servers
              ORDER BY candidate_rank ASC NULLS LAST, id
              LIMIT $9 OFFSET $10
         ), measured AS MATERIALIZED (
             SELECT sample.server_id, sample.profile_sha256,
                    COUNT(DISTINCT sample.node_id) AS measured_nodes,
                    COUNT(*) FILTER (WHERE sample.last_success_probed_at IS NOT NULL)
                        AS successful_samples,
                    (array_agg(sample.status ORDER BY sample.probed_at DESC,
                        sample.received_at DESC, sample.node_id))[1]
                        AS latest_probe_status,
                    (array_agg(sample.error_code ORDER BY sample.probed_at DESC,
                        sample.received_at DESC, sample.node_id))[1]
                        AS latest_error_code,
                    MAX(sample.probed_at) AS latest_probed_at,
                    (array_agg(sample.last_success_exit_ip
                        ORDER BY sample.last_success_probed_at DESC, sample.node_id)
                        FILTER (WHERE sample.last_success_probed_at IS NOT NULL))[1]
                        AS last_success_exit_ip,
                    (array_agg(sample.last_success_connect_ms
                        ORDER BY sample.last_success_probed_at DESC, sample.node_id)
                        FILTER (WHERE sample.last_success_probed_at IS NOT NULL))[1]
                        AS last_success_connect_ms,
                    (array_agg(sample.last_success_download_bps
                        ORDER BY sample.last_success_probed_at DESC, sample.node_id)
                        FILTER (WHERE sample.last_success_probed_at IS NOT NULL))[1]
                        AS last_success_download_bps,
                    MAX(sample.last_success_probed_at) AS last_success_probed_at,
                    MAX(sample.last_success_received_at) AS last_success_received_at
               FROM page_servers server
               JOIN vpngate_candidate_probe_latest sample
                 ON sample.server_id = server.id
                AND sample.profile_sha256 = server.profile_sha256
              GROUP BY sample.server_id, sample.profile_sha256
         ), directory AS (
             SELECT server.id, server.hostname, host(server.ip) AS ip,
                    server.country_code, server.country_name, server.ping_ms,
                    server.speed_bps AS catalog_speed_bps, server.vpn_sessions,
                    EXTRACT(EPOCH FROM server.last_seen_at)::BIGINT
                        AS last_seen_at_unix_secs,
                    server.current AS seen_in_latest_sync,
                    server.active, server.candidate_rank, server.pareto_layer,
                    server.global_download_bps, server.global_connect_ms,
                    COALESCE(measured.measured_nodes, 0) AS measured_nodes,
                    COALESCE(measured.successful_samples, 0) AS successful_samples,
                    measured.latest_probe_status,
                    COALESCE(probe_state.consecutive_failures, 0)
                        AS consecutive_probe_failures,
                    EXTRACT(EPOCH FROM probe_state.failure_streak_started_at
                        + make_interval(secs => $11))::BIGINT
                        AS probe_eligible_until_unix_secs,
                    host(measured.last_success_exit_ip) AS latest_exit_ip,
                    reputation.country_code AS latest_exit_country_code,
                    NULLIF(measured.last_success_connect_ms, 0) AS latest_connect_ms,
                    measured.last_success_download_bps AS latest_download_bps,
                    COALESCE(reputation.ip_scores, '[]'::jsonb) AS latest_ip_scores,
                    COALESCE(reputation.ip_networks, '[]'::jsonb) AS latest_ip_networks,
                    measured.latest_error_code,
                    EXTRACT(EPOCH FROM measured.latest_probed_at)::BIGINT
                        AS latest_probed_at_unix_secs,
                    EXTRACT(EPOCH FROM measured.last_success_probed_at)::BIGINT
                        AS latest_successful_probed_at_unix_secs,
                    EXTRACT(EPOCH FROM reputation.verified_at)::BIGINT
                        AS intelligence_verified_at_unix_secs,
                    ($7 AND reputation.verified_at < now() - make_interval(secs => $8))
                        AS intelligence_stale
               FROM page_servers server
               LEFT JOIN measured
                 ON measured.server_id = server.id
                AND measured.profile_sha256 = server.profile_sha256
               LEFT JOIN vpngate_candidate_probe_state probe_state
                 ON probe_state.server_id = server.id
                AND probe_state.profile_sha256 = server.profile_sha256
               LEFT JOIN vpngate_exit_reputations reputation
                 ON reputation.exit_ip = measured.last_success_exit_ip
         ), counted AS (
             SELECT COUNT(*) AS filtered_total FROM country_servers
         )
         SELECT page.*, counted.filtered_total
           FROM counted
           LEFT JOIN LATERAL (
                SELECT * FROM directory
                 ORDER BY candidate_rank ASC NULLS LAST, id
           ) page ON TRUE",
    )
    .bind(country_code)
    .bind(&ranking.server_ids)
    .bind(&ranking.pareto_layers)
    .bind(&ranking.global_downloads)
    .bind(&ranking.global_connects)
    .bind(ranking.active_limit)
    .bind(mark_stale)
    .bind(stale_after_secs)
    .bind(page_size)
    .bind(offset)
    .bind(FAILED_CANDIDATE_GRACE_SECS)
    .fetch_all(pool)
    .await?)
}

/// A fresh Agent observation supplied by the Console, never by the browser. A missing selected
/// server means the supervisor currently has no active backend, not a fallback to stored state.
#[derive(Debug, Clone, Serialize)]
pub struct VpngateRuntimeSelection {
    pub node_id: String,
    pub outbound_id: String,
    pub selected_server_id: Option<String>,
}

pub async fn runtime_views(
    pool: &PgPool,
    actor: &AdminContext,
    observed: &[VpngateRuntimeSelection],
) -> Result<Vec<VpngateRuntimeView>> {
    let intelligence_policy = intelligence_policy(pool).await?;
    let rows = sqlx::query(
        "WITH runtime_state AS (
            SELECT stored.*,
                   CASE WHEN live.node_id IS NOT NULL THEN live.selected_server_id
                        ELSE stored.selected_server_id END AS active_server_id
              FROM vpngate_node_pool_state stored
              LEFT JOIN jsonb_to_recordset($6::jsonb)
                   AS live(node_id TEXT, outbound_id TEXT, selected_server_id TEXT)
                ON live.node_id = stored.node_id AND live.outbound_id = stored.outbound_id
         )
         SELECT state.node_id, node.name AS node_name, node.tenant_id,
                state.outbound_id, outbound.name AS outbound_name,
                outbound.protocol_options ->> 'country_code' AS country_code,
                (NULLIF(outbound.protocol_options ->> 'server_id', '') IS NULL
                    AND COALESCE(outbound.protocol_options -> 'server_ids', '[]'::jsonb) = '[]'::jsonb)
                    AS automatic_pool,
                state.runtime_status, state.active_server_id AS selected_server_id,
                server.hostname AS selected_hostname,
                EXTRACT(EPOCH FROM state.reported_at)::BIGINT AS reported_at_unix_secs,
                latest.status AS latest_probe_status,
                host(last_success.exit_ip) AS latest_exit_ip,
                COALESCE(reputation.country_code, last_success.exit_country_code)
                    AS latest_exit_country_code,
                last_connect.connect_ms AS latest_connect_ms,
                last_success.download_bps AS latest_download_bps,
                COALESCE(reputation.ip_scores, last_success.ip_scores, '[]'::jsonb)
                    AS latest_ip_scores,
                COALESCE(reputation.ip_networks, last_success.ip_networks, '[]'::jsonb)
                    AS latest_ip_networks,
                latest.error_code AS latest_error_code,
                latest.error_detail AS latest_error_detail,
                EXTRACT(EPOCH FROM latest.probed_at)::BIGINT AS latest_probed_at_unix_secs,
                EXTRACT(EPOCH FROM last_success.probed_at)::BIGINT
                    AS latest_successful_probed_at_unix_secs,
                EXTRACT(EPOCH FROM reputation.verified_at)::BIGINT
                    AS intelligence_verified_at_unix_secs,
                ($4 AND reputation.verified_at < now() - make_interval(secs => $5))
                    AS intelligence_stale,
                switch_request.id AS switch_request_id,
                switch_request.status AS switch_status,
                switch_request.previous_server_id AS switch_previous_server_id,
                previous_server.hostname AS switch_previous_hostname,
                switch_request.selected_server_id AS switch_selected_server_id,
                EXTRACT(EPOCH FROM switch_request.cooldown_until)::BIGINT
                    AS switch_cooldown_until_unix_secs,
                switch_request.error_detail AS switch_error_detail
           FROM runtime_state state
           JOIN nodes node ON node.id = state.node_id
           JOIN external_outbounds outbound
             ON outbound.id = state.outbound_id AND outbound.protocol = 'vpngate'
           LEFT JOIN vpngate_servers server ON server.id = state.active_server_id
           LEFT JOIN LATERAL (
                SELECT sample.status, sample.exit_ip, sample.exit_country_code,
                       sample.connect_ms, sample.download_bps, sample.ip_scores,
                       sample.ip_networks,
                       sample.error_code, sample.error_detail,
                       sample.probed_at
                  FROM vpngate_probe_samples sample
                 WHERE sample.node_id = state.node_id
                   AND sample.outbound_id = state.outbound_id
                   AND sample.server_id = state.active_server_id
                 ORDER BY sample.probed_at DESC, sample.id DESC
                 LIMIT 1
           ) latest ON TRUE
           LEFT JOIN LATERAL (
                SELECT sample.exit_ip, sample.exit_country_code,
                       sample.connect_ms, sample.download_bps,
                       sample.ip_scores, sample.ip_networks, sample.probed_at
                  FROM vpngate_probe_samples sample
                 WHERE sample.node_id = state.node_id
                   AND sample.outbound_id = state.outbound_id
                   AND sample.server_id = state.active_server_id
                   AND sample.status = 'succeeded'
                 ORDER BY sample.probed_at DESC, sample.id DESC
                 LIMIT 1
           ) last_success ON TRUE
           LEFT JOIN LATERAL (
                SELECT sample.connect_ms
                  FROM vpngate_probe_samples sample
                 WHERE sample.node_id = state.node_id
                   AND sample.outbound_id = state.outbound_id
                   AND sample.server_id = state.active_server_id
                   AND sample.status = 'succeeded'
                   AND sample.connect_ms > 0
                 ORDER BY sample.probed_at DESC, sample.id DESC
                 LIMIT 1
           ) last_connect ON TRUE
           LEFT JOIN vpngate_exit_reputations reputation
             ON reputation.exit_ip = last_success.exit_ip
           LEFT JOIN LATERAL (
                SELECT request.id, request.status, request.previous_server_id,
                       request.selected_server_id, request.cooldown_until,
                       request.error_detail
                  FROM vpngate_pool_switch_requests request
                 WHERE request.node_id = state.node_id
                   AND request.outbound_id = state.outbound_id
                 ORDER BY request.id DESC
                 LIMIT 1
           ) switch_request ON TRUE
           LEFT JOIN vpngate_servers previous_server
             ON previous_server.id = switch_request.previous_server_id
          WHERE ($1::TEXT IS NULL OR node.tenant_id = $1 OR node.tenant_id LIKE $2 ESCAPE '\\')
            AND outbound.protocol_options ->> 'country_code' <> $3
          ORDER BY node.tenant_id, node.name, state.outbound_id
          LIMIT 500",
    )
    .bind(actor.tenant_scope())
    .bind(actor.tenant_scope_like_pattern())
    .bind(UNKNOWN_COUNTRY_CODE)
    .bind(!matches!(
        intelligence_policy.stale_policy,
        VpngateStaleIntelligencePolicy::Retain
    ))
    .bind(intelligence_hours_to_secs(
        "intelligence stale age",
        intelligence_policy.stale_after_hours,
    )?)
    .bind(serde_json::to_value(observed)?)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(VpngateRuntimeView {
                node_id: row.try_get("node_id")?,
                node_name: row.try_get("node_name")?,
                tenant_id: row.try_get("tenant_id")?,
                outbound_id: row.try_get("outbound_id")?,
                outbound_name: row.try_get("outbound_name")?,
                country_code: row.try_get("country_code")?,
                automatic_pool: row.try_get("automatic_pool")?,
                runtime_status: row.try_get("runtime_status")?,
                selected_server_id: row.try_get("selected_server_id")?,
                selected_hostname: row.try_get("selected_hostname")?,
                reported_at_unix_secs: row.try_get("reported_at_unix_secs")?,
                latest_probe_status: row.try_get("latest_probe_status")?,
                latest_exit_ip: row.try_get("latest_exit_ip")?,
                latest_exit_country_code: row.try_get("latest_exit_country_code")?,
                latest_connect_ms: optional_i32_to_u32(
                    "latest_connect_ms",
                    row.try_get("latest_connect_ms")?,
                )?,
                latest_download_bps: optional_i64_to_u64(
                    "latest_download_bps",
                    row.try_get("latest_download_bps")?,
                )?,
                latest_ip_scores: decode_json(
                    "latest_ip_scores",
                    row.try_get("latest_ip_scores")?,
                )?,
                latest_ip_networks: decode_json(
                    "latest_ip_networks",
                    row.try_get("latest_ip_networks")?,
                )?,
                latest_error_code: row.try_get("latest_error_code")?,
                latest_error_detail: row.try_get("latest_error_detail")?,
                latest_probed_at_unix_secs: row.try_get("latest_probed_at_unix_secs")?,
                latest_successful_probed_at_unix_secs: row
                    .try_get("latest_successful_probed_at_unix_secs")?,
                intelligence_verified_at_unix_secs: row
                    .try_get("intelligence_verified_at_unix_secs")?,
                intelligence_stale: row
                    .try_get::<Option<bool>, _>("intelligence_stale")?
                    .unwrap_or(false),
                switch_request_id: optional_i64_to_u64(
                    "switch_request_id",
                    row.try_get("switch_request_id")?,
                )?,
                switch_status: row.try_get("switch_status")?,
                switch_previous_server_id: row.try_get("switch_previous_server_id")?,
                switch_previous_hostname: row.try_get("switch_previous_hostname")?,
                switch_selected_server_id: row.try_get("switch_selected_server_id")?,
                switch_cooldown_until_unix_secs: row.try_get("switch_cooldown_until_unix_secs")?,
                switch_error_detail: row.try_get("switch_error_detail")?,
            })
        })
        .collect()
}

fn switch_request_view(row: &sqlx::postgres::PgRow) -> Result<VpngatePoolSwitchRequestView> {
    Ok(VpngatePoolSwitchRequestView {
        request_id: i64_to_u64("switch request id", row.try_get("id")?)?,
        node_id: row.try_get("node_id")?,
        outbound_id: row.try_get("outbound_id")?,
        previous_server_id: row.try_get("previous_server_id")?,
        status: row.try_get("status")?,
        selected_server_id: row.try_get("selected_server_id")?,
        cooldown_until_unix_secs: row.try_get("cooldown_until_unix_secs")?,
        error_detail: row.try_get("error_detail")?,
        requested_at_unix_secs: row.try_get("requested_at_unix_secs")?,
        completed_at_unix_secs: row.try_get("completed_at_unix_secs")?,
    })
}

pub async fn request_pool_switch(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    outbound_id: &str,
    request: RequestVpngatePoolSwitch,
    observed: Option<&VpngateRuntimeSelection>,
) -> Result<VpngatePoolSwitchRequestView> {
    require_system_admin(actor, "switch a VPN Gate automatic pool")?;
    let mut tx = pool.begin().await?;
    let minimum_protocol = i32::try_from(MIN_AGENT_PROTOCOL_VERSION)
        .expect("Agent protocol version is deliberately within PostgreSQL INTEGER");
    let runtime = sqlx::query(
        "SELECT state.topology_revision, state.selected_server_id,
                COALESCE(agent.runtime_reported_at >= now() - interval '2 minutes', FALSE)
                    AS agent_fresh,
                COALESCE(agent.agent_protocol_version >= $3, FALSE) AS protocol_compatible
           FROM vpngate_node_pool_state state
           JOIN nodes node ON node.id = state.node_id
           LEFT JOIN node_agent_state agent ON agent.node_id = state.node_id
          WHERE state.node_id = $1 AND state.outbound_id = $2
            AND node.retired_at IS NULL
          FOR UPDATE OF state",
    )
    .bind(node_id)
    .bind(outbound_id)
    .bind(minimum_protocol)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::NotFound("VPN Gate runtime was not found".to_owned()))?;
    if !runtime.try_get::<bool, _>("protocol_compatible")? {
        return Err(StoreError::Conflict(format!(
            "VPN Gate Agent protocol is incompatible; upgrade to v{MIN_AGENT_PROTOCOL_VERSION} or newer"
        )));
    }
    if !runtime.try_get::<bool, _>("agent_fresh")? {
        return Err(StoreError::Conflict(
            "VPN Gate runtime report is stale; wait for the Agent to reconnect".to_owned(),
        ));
    }
    let selected_server_id = if let Some(observed) = observed {
        if observed.node_id != node_id || observed.outbound_id != outbound_id {
            return Err(StoreError::InvalidData(
                "VPN Gate observation does not match the requested runtime".to_owned(),
            ));
        }
        observed.selected_server_id.clone()
    } else {
        runtime.try_get::<Option<String>, _>("selected_server_id")?
    }
    .ok_or_else(|| StoreError::Conflict("VPN Gate runtime has no active server".to_owned()))?;
    if request.expected_server_id != selected_server_id {
        return Err(StoreError::Conflict(format!(
            "VPN Gate active server changed from {} to {selected_server_id}",
            request.expected_server_id
        )));
    }
    let topology_revision = i64_to_u64(
        "topology_revision",
        runtime.try_get::<i64, _>("topology_revision")?,
    )?;
    let snapshot =
        crate::materialize::load_immutable_snapshot_tx(&mut tx, topology_revision).await?;
    if !referenced_vpngate_pool_ids(&snapshot, node_id).contains(outbound_id) {
        return Err(StoreError::Conflict(
            "VPN Gate runtime is no longer referenced by this machine".to_owned(),
        ));
    }
    let automatic_pool = snapshot.external_outbounds.iter().any(|outbound| {
        outbound.id == outbound_id
            && matches!(
                &outbound.protocol,
                ExternalOutboundProtocol::Vpngate {
                    server_id: None,
                    server_ids,
                    max_candidates,
                    ..
                } if server_ids.is_empty() && *max_candidates >= 2
            )
    });
    if !automatic_pool {
        return Err(StoreError::Conflict(
            "only an automatic VPN Gate pool with at least two candidates can be switched"
                .to_owned(),
        ));
    }

    if let Some(row) = sqlx::query(
        "SELECT id, node_id, outbound_id, previous_server_id, status,
                selected_server_id,
                EXTRACT(EPOCH FROM cooldown_until)::BIGINT AS cooldown_until_unix_secs,
                error_detail,
                EXTRACT(EPOCH FROM requested_at)::BIGINT AS requested_at_unix_secs,
                EXTRACT(EPOCH FROM completed_at)::BIGINT AS completed_at_unix_secs
           FROM vpngate_pool_switch_requests
          WHERE node_id = $1 AND outbound_id = $2 AND status = 'pending'
          FOR UPDATE",
    )
    .bind(node_id)
    .bind(outbound_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        let existing = switch_request_view(&row)?;
        if existing.previous_server_id == selected_server_id {
            tx.commit().await?;
            return Ok(existing);
        }
        return Err(StoreError::Conflict(
            "another VPN Gate switch is already pending".to_owned(),
        ));
    }

    let row = sqlx::query(
        "INSERT INTO vpngate_pool_switch_requests
                (node_id, outbound_id, previous_server_id, requested_by)
         VALUES ($1, $2, $3, $4)
         RETURNING id, node_id, outbound_id, previous_server_id, status,
                   selected_server_id,
                   EXTRACT(EPOCH FROM cooldown_until)::BIGINT AS cooldown_until_unix_secs,
                   error_detail,
                   EXTRACT(EPOCH FROM requested_at)::BIGINT AS requested_at_unix_secs,
                   EXTRACT(EPOCH FROM completed_at)::BIGINT AS completed_at_unix_secs",
    )
    .bind(node_id)
    .bind(outbound_id)
    .bind(&selected_server_id)
    .bind(actor.operator_id())
    .fetch_one(&mut *tx)
    .await?;
    let result = switch_request_view(&row)?;
    tx.commit().await?;
    Ok(result)
}

pub async fn update_settings(
    pool: &PgPool,
    actor: &AdminContext,
    request: UpdateVpngateCatalogSettings,
) -> Result<VpngateCatalogStatus> {
    require_system_admin(actor, "configure VPN Gate collection")?;
    if !(60..=86_400).contains(&request.interval_secs) {
        return Err(StoreError::InvalidData(
            "VPN Gate sync interval must be between 60 and 86400 seconds".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE vpngate_catalog_state
            SET enabled = $1,
                interval_secs = $2,
                next_sync_at = CASE WHEN $1 THEN LEAST(next_sync_at, now()) ELSE next_sync_at END,
                updated_at = now()
          WHERE id = TRUE",
    )
    .bind(request.enabled)
    .bind(u32_to_i32("interval_secs", request.interval_secs)?)
    .execute(&mut *tx)
    .await?;
    if request.enabled {
        sqlx::query(
            "UPDATE vpngate_intelligence_nodes
                SET catalogue_next_sync_at = LEAST(catalogue_next_sync_at, now())",
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    catalog_status(pool).await
}

pub async fn update_probe_settings(
    pool: &PgPool,
    actor: &AdminContext,
    request: UpdateVpngateProbeSettings,
) -> Result<VpngateCatalogStatus> {
    require_system_admin(actor, "configure VPN Gate catalogue probing")?;
    if !(PROBE_SUCCESS_COOLDOWN_MIN_SECS..=PROBE_SUCCESS_COOLDOWN_MAX_SECS)
        .contains(&request.success_cooldown_secs)
        || !request.success_cooldown_secs.is_multiple_of(60)
    {
        return Err(StoreError::InvalidData(format!(
            "VPN Gate successful probe cooldown must be a whole number of minutes between {PROBE_SUCCESS_COOLDOWN_MIN_SECS} and {PROBE_SUCCESS_COOLDOWN_MAX_SECS} seconds"
        )));
    }
    if !(PROBE_PERFORMANCE_COOLDOWN_MIN_SECS..=PROBE_PERFORMANCE_COOLDOWN_MAX_SECS)
        .contains(&request.performance_cooldown_secs)
        || !request.performance_cooldown_secs.is_multiple_of(60 * 60)
    {
        return Err(StoreError::InvalidData(format!(
            "VPN Gate performance probe cooldown must be a whole number of hours between {PROBE_PERFORMANCE_COOLDOWN_MIN_SECS} and {PROBE_PERFORMANCE_COOLDOWN_MAX_SECS} seconds"
        )));
    }
    if !(PROBE_SHARD_ROTATION_MIN_SECS..=PROBE_SHARD_ROTATION_MAX_SECS)
        .contains(&request.shard_rotation_secs)
        || !request.shard_rotation_secs.is_multiple_of(60 * 60)
    {
        return Err(StoreError::InvalidData(format!(
            "VPN Gate probe shard rotation must be a whole number of hours between {PROBE_SHARD_ROTATION_MIN_SECS} and {PROBE_SHARD_ROTATION_MAX_SECS} seconds"
        )));
    }
    sqlx::query(
        "UPDATE vpngate_catalog_state
            SET probe_success_cooldown_secs = $1,
                probe_performance_cooldown_secs = $2,
                probe_shard_rotation_secs = $3,
                updated_at = now()
          WHERE id = TRUE",
    )
    .bind(u32_to_i32(
        "success_cooldown_secs",
        request.success_cooldown_secs,
    )?)
    .bind(u32_to_i32(
        "performance_cooldown_secs",
        request.performance_cooldown_secs,
    )?)
    .bind(u32_to_i32(
        "shard_rotation_secs",
        request.shard_rotation_secs,
    )?)
    .execute(pool)
    .await?;
    catalog_status(pool).await
}

pub async fn request_sync(pool: &PgPool, actor: &AdminContext) -> Result<()> {
    require_system_admin(actor, "start a VPN Gate sync")?;
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE vpngate_catalog_state
            SET enabled = TRUE, next_sync_at = now(), updated_at = now()
          WHERE id = TRUE",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE vpngate_intelligence_nodes
            SET catalogue_next_sync_at = now()",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn update_probe_node(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    request: UpdateVpngateProbeNode,
) -> Result<VpngateProbeNodeSelection> {
    require_system_admin(actor, "choose VPN Gate probe nodes")?;
    if node_id.trim().is_empty() || node_id.chars().count() > 128 {
        return Err(StoreError::InvalidData(
            "VPN Gate probe node id is invalid".to_owned(),
        ));
    }
    let workers = request.workers.unwrap_or(DEFAULT_CATALOG_PROBE_WORKERS);
    if request.enabled && !valid_catalog_probe_workers(workers) {
        return Err(StoreError::InvalidData(
            "VPN Gate probe workers must be between 1 and 128".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT n.retired_at IS NULL
                    AND COALESCE(l.phase, 'active') = 'active' AS lifecycle_active,
                oi.node_id IS NOT NULL AS isolated,
                s.agent_protocol_version,
                COALESCE(s.runtime_reported_at >= now() - interval '2 minutes', FALSE)
                    AS runtime_fresh,
                NULLIF(BTRIM(s.runtime_versions->>'openvpn'), '') AS openvpn
           FROM nodes n
           LEFT JOIN node_lifecycle_state l ON l.node_id = n.id
           LEFT JOIN node_operational_isolations oi ON oi.node_id = n.id
           LEFT JOIN node_agent_state s ON s.node_id = n.id
          WHERE n.id = $1
          FOR UPDATE OF n",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
    if request.enabled {
        let minimum_protocol = i32::try_from(MIN_AGENT_PROTOCOL_VERSION)
            .expect("Agent protocol version is deliberately within PostgreSQL INTEGER");
        let reason = if !row.try_get::<bool, _>("lifecycle_active")? {
            Some("machine is not in the active lifecycle")
        } else if row.try_get::<bool, _>("isolated")? {
            Some("machine is operationally isolated")
        } else if row
            .try_get::<Option<i32>, _>("agent_protocol_version")?
            .is_none_or(|version| version < minimum_protocol)
        {
            Some("machine does not report a compatible Agent protocol")
        } else if !row.try_get::<bool, _>("runtime_fresh")? {
            Some("machine has no fresh runtime report")
        } else if row.try_get::<Option<String>, _>("openvpn")?.is_none() {
            Some("machine does not report an executable OpenVPN")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(StoreError::Conflict(format!(
                "cannot enable VPN Gate catalogue probing: {reason}"
            )));
        }
        sqlx::query(
            "INSERT INTO vpngate_probe_nodes (node_id, selected_by, selected_at, workers)
             VALUES ($1, $2, now(), $3)
             ON CONFLICT (node_id) DO UPDATE SET
                selected_by = EXCLUDED.selected_by,
                selected_at = now(),
                workers = EXCLUDED.workers",
        )
        .bind(node_id)
        .bind(actor.operator_id())
        .bind(i32::from(workers))
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query("DELETE FROM vpngate_probe_nodes WHERE node_id = $1")
            .bind(node_id)
            .execute(&mut *tx)
            .await?;
    }
    let selected = sqlx::query(
        "SELECT workers, EXTRACT(EPOCH FROM selected_at)::BIGINT AS selected_at_unix_secs
           FROM vpngate_probe_nodes
          WHERE node_id = $1",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (workers, selected_at_unix_secs) = match selected {
        Some(row) => {
            let workers = u8::try_from(row.try_get::<i32, _>("workers")?).map_err(|_| {
                StoreError::InvalidData("stored VPN Gate probe workers are invalid".to_owned())
            })?;
            (Some(workers), Some(row.try_get("selected_at_unix_secs")?))
        }
        None => (None, None),
    };
    tx.commit().await?;
    Ok(VpngateProbeNodeSelection {
        node_id: node_id.to_owned(),
        enabled: workers.is_some(),
        workers,
        selected_at_unix_secs,
    })
}

pub async fn update_intelligence_node(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    request: UpdateVpngateIntelligenceNode,
) -> Result<VpngateIntelligenceNodeSelection> {
    require_system_admin(
        actor,
        "choose VPN Gate catalogue and IP intelligence workers",
    )?;
    if node_id.trim().is_empty() || node_id.chars().count() > 128 {
        return Err(StoreError::InvalidData(
            "VPN Gate intelligence node id is invalid".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT n.retired_at IS NULL
                    AND COALESCE(l.phase, 'active') = 'active' AS lifecycle_active,
                oi.node_id IS NOT NULL AS isolated,
                s.agent_protocol_version,
                COALESCE(s.runtime_reported_at >= now() - interval '2 minutes', FALSE)
                    AS runtime_fresh
           FROM nodes n
           LEFT JOIN node_lifecycle_state l ON l.node_id = n.id
           LEFT JOIN node_operational_isolations oi ON oi.node_id = n.id
           LEFT JOIN node_agent_state s ON s.node_id = n.id
          WHERE n.id = $1
          FOR UPDATE OF n",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
    if request.enabled {
        let minimum_protocol = i32::try_from(MIN_AGENT_PROTOCOL_VERSION)
            .expect("Agent protocol version is deliberately within PostgreSQL INTEGER");
        let reason = if !row.try_get::<bool, _>("lifecycle_active")? {
            Some("machine is not in the active lifecycle")
        } else if row.try_get::<bool, _>("isolated")? {
            Some("machine is operationally isolated")
        } else if row
            .try_get::<Option<i32>, _>("agent_protocol_version")?
            .is_none_or(|version| version < minimum_protocol)
        {
            Some("machine does not report a compatible Agent protocol")
        } else if !row.try_get::<bool, _>("runtime_fresh")? {
            Some("machine has no fresh runtime report")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(StoreError::Conflict(format!(
                "cannot enable VPN Gate IP intelligence: {reason}"
            )));
        }
        sqlx::query(
            "INSERT INTO vpngate_intelligence_nodes (node_id, selected_by, selected_at)
             VALUES ($1, $2, now())
             ON CONFLICT (node_id) DO UPDATE SET
                selected_by = EXCLUDED.selected_by,
                selected_at = now()",
        )
        .bind(node_id)
        .bind(actor.operator_id())
        .execute(&mut *tx)
        .await?;
    } else {
        lock_catalog_publication(&mut tx).await?;
        sqlx::query(
            "UPDATE vpngate_sync_runs run
                SET status = 'failed', error_code = 'collector-disabled',
                    error_detail = 'collector was removed while this lease was active',
                    finished_at = now()
               FROM vpngate_intelligence_nodes selected
              WHERE selected.node_id = $1
                AND run.id = selected.catalogue_active_run_id
                AND run.status = 'running'",
        )
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM vpngate_intelligence_nodes WHERE node_id = $1")
            .bind(node_id)
            .execute(&mut *tx)
            .await?;
        refresh_current_catalog_union(&mut tx).await?;
    }
    let selected_at_unix_secs = sqlx::query_scalar::<_, i64>(
        "SELECT EXTRACT(EPOCH FROM selected_at)::BIGINT
           FROM vpngate_intelligence_nodes
          WHERE node_id = $1",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(VpngateIntelligenceNodeSelection {
        node_id: node_id.to_owned(),
        enabled: selected_at_unix_secs.is_some(),
        selected_at_unix_secs,
    })
}

pub async fn claim_exit_intelligence(
    pool: &PgPool,
    node_id: &str,
) -> Result<Option<VpngateIpIntelligenceClaim>> {
    if node_id.trim().is_empty() || node_id.chars().count() > 128 {
        return Err(StoreError::InvalidData(
            "VPN Gate intelligence node id is empty or too long".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    let minimum_protocol = i32::try_from(MIN_AGENT_PROTOCOL_VERSION)
        .expect("Agent protocol version is deliberately within PostgreSQL INTEGER");
    let eligible = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
            SELECT 1
              FROM vpngate_intelligence_nodes selected
              JOIN nodes node ON node.id = selected.node_id
              JOIN node_agent_state state ON state.node_id = selected.node_id
              LEFT JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = selected.node_id
              LEFT JOIN node_operational_isolations isolation ON isolation.node_id = selected.node_id
             WHERE selected.node_id = $1
               AND node.retired_at IS NULL
               AND COALESCE(lifecycle.phase, 'active') = 'active'
               AND isolation.node_id IS NULL
               AND state.agent_protocol_version >= $2
               AND state.runtime_reported_at >= now() - interval '2 minutes'
        )",
    )
    .bind(node_id)
    .bind(minimum_protocol)
    .fetch_one(&mut *tx)
    .await?;
    if !eligible {
        tx.commit().await?;
        return Ok(None);
    }
    let catalog = sqlx::query(
        "SELECT intelligence_policy, proxycheck_api_key_sealed
           FROM vpngate_catalog_state
          WHERE id = TRUE
          FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await?;
    let policy = decode_intelligence_policy(catalog.try_get("intelligence_policy")?)?;
    let mut proxycheck_credentials = catalog
        .try_get::<Option<String>, _>("proxycheck_api_key_sealed")?
        .map(|sealed| decode_proxycheck_credentials(&sealed))
        .transpose()?;
    let active_secs =
        intelligence_hours_to_secs("intelligence active window", policy.active_window_hours)?;
    let row = sqlx::query(
        "SELECT host(exit_ip) AS exit_ip, lease_generation
           FROM vpngate_exit_reputations
          WHERE next_check_at <= now()
            AND (lease_until IS NULL OR lease_until <= now())
            AND last_seen_at >= now() - make_interval(secs => $1)
          ORDER BY next_check_at, exit_ip
          FOR UPDATE SKIP LOCKED
          LIMIT 1",
    )
    .bind(active_secs)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    let exit_ip: String = row.try_get("exit_ip")?;
    let lease_generation = row
        .try_get::<i64, _>("lease_generation")?
        .checked_add(1)
        .ok_or_else(|| {
            StoreError::InvalidData("VPN Gate reputation lease generation overflow".to_owned())
        })?;
    let proxycheck_api_key = if let Some(credentials) = &mut proxycheck_credentials {
        let index = credentials.next_key_index % credentials.keys.len();
        let key = credentials.keys[index].clone();
        credentials.next_key_index = (index + 1) % credentials.keys.len();
        let sealed = seal_proxycheck_credentials(credentials)?;
        sqlx::query(
            "UPDATE vpngate_catalog_state
                SET proxycheck_api_key_sealed = $1, updated_at = now()
              WHERE id = TRUE",
        )
        .bind(sealed)
        .execute(&mut *tx)
        .await?;
        Some(key)
    } else {
        None
    };
    sqlx::query(
        "UPDATE vpngate_exit_reputations
            SET lease_owner = $2,
                lease_generation = $3,
                lease_until = now() + make_interval(secs => $4),
                updated_at = now()
          WHERE exit_ip = $1::inet",
    )
    .bind(&exit_ip)
    .bind(node_id)
    .bind(lease_generation)
    .bind(REPUTATION_LEASE_SECS)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(VpngateIpIntelligenceClaim {
        exit_ip,
        lease_generation: i64_to_u64("lease_generation", lease_generation)?,
        proxycheck_api_key,
    }))
}

/// Claim this selected Agent's own catalogue collection.
///
/// VPN Gate's response varies by network origin, so each selected Agent keeps an independent
/// schedule and lease. The Console later publishes the union of their latest successful snapshots
/// instead of allowing whichever Agent reports last to replace the other origins.
pub async fn claim_agent_catalog_sync(
    pool: &PgPool,
    node_id: &str,
) -> Result<Option<VpngateSyncClaim>> {
    if node_id.trim().is_empty() || node_id.chars().count() > 128 {
        return Err(StoreError::InvalidData(
            "VPN Gate catalogue collector node id is empty or too long".to_owned(),
        ));
    }
    let minimum_protocol = i32::try_from(MIN_AGENT_PROTOCOL_VERSION)
        .expect("Agent protocol version is deliberately within PostgreSQL INTEGER");
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT catalog.enabled, catalog.source_url,
                selected.catalogue_lease_generation,
                selected.catalogue_active_run_id,
                selected.catalogue_next_sync_at <= now() AS due,
                selected.catalogue_lease_until IS NULL
                    OR selected.catalogue_lease_until <= now() AS lease_available
           FROM vpngate_intelligence_nodes selected
           JOIN nodes node ON node.id = selected.node_id
           JOIN node_agent_state state ON state.node_id = selected.node_id
           LEFT JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = selected.node_id
           LEFT JOIN node_operational_isolations isolation ON isolation.node_id = selected.node_id
          CROSS JOIN vpngate_catalog_state catalog
          WHERE selected.node_id = $1
            AND node.retired_at IS NULL
            AND COALESCE(lifecycle.phase, 'active') = 'active'
            AND isolation.node_id IS NULL
            AND state.agent_protocol_version >= $2
            AND state.runtime_reported_at >= now() - interval '2 minutes'
            AND catalog.id = TRUE
          FOR UPDATE OF selected",
    )
    .bind(node_id)
    .bind(minimum_protocol)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    if !row.try_get::<bool, _>("enabled")?
        || !row.try_get::<bool, _>("due")?
        || !row.try_get::<bool, _>("lease_available")?
    {
        tx.commit().await?;
        return Ok(None);
    }
    let source_url: String = row.try_get("source_url")?;
    if let Some(expired_run_id) = row.try_get::<Option<i64>, _>("catalogue_active_run_id")? {
        sqlx::query(
            "UPDATE vpngate_sync_runs
                SET status = 'failed', error_code = 'lease-expired',
                    error_detail = 'collector did not report before its lease expired',
                    finished_at = now()
              WHERE id = $1 AND status = 'running'",
        )
        .bind(expired_run_id)
        .execute(&mut *tx)
        .await?;
    }
    let lease_generation = row
        .try_get::<i64, _>("catalogue_lease_generation")?
        .checked_add(1)
        .ok_or_else(|| {
            StoreError::InvalidData("VPN Gate collector lease generation overflow".to_owned())
        })?;
    let run_id: i64 = sqlx::query_scalar(
        "INSERT INTO vpngate_sync_runs (source_url, worker_id, trigger)
         VALUES ($1, $2, 'scheduled')
         RETURNING id",
    )
    .bind(&source_url)
    .bind(node_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE vpngate_intelligence_nodes
            SET catalogue_lease_generation = $2,
                catalogue_lease_until = now() + make_interval(secs => $3),
                catalogue_active_run_id = $4
          WHERE node_id = $1",
    )
    .bind(node_id)
    .bind(lease_generation)
    .bind(SYNC_LEASE_SECS)
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(VpngateSyncClaim {
        run_id: i64_to_u64("run_id", run_id)?,
        lease_generation: i64_to_u64("lease_generation", lease_generation)?,
        source_url,
    }))
}

pub async fn record_ip_intelligence_report(
    pool: &PgPool,
    node_id: &str,
    report: &VpngateIpIntelligenceReport,
) -> Result<()> {
    if report.exit_ip.parse::<std::net::IpAddr>().is_err() || report.lease_generation == 0 {
        return Err(StoreError::InvalidData(
            "VPN Gate IP intelligence report identity is invalid".to_owned(),
        ));
    }
    let expected = BTreeSet::from([
        VpngateIpProvider::Proxycheck,
        VpngateIpProvider::Ffraud,
        VpngateIpProvider::Iplogs,
    ]);
    let mut reported = BTreeSet::new();
    for observation in &report.observations {
        normalize_country_code(&observation.country_code)?;
        if observation.score > 100
            || observation
                .isp
                .as_deref()
                .is_some_and(|value| value.trim().is_empty() || value.chars().count() > 160)
            || !reported.insert(observation.provider)
        {
            return Err(StoreError::InvalidData(
                "VPN Gate IP intelligence observation is invalid or duplicated".to_owned(),
            ));
        }
    }
    for failure in &report.failures {
        if failure.code.is_empty()
            || failure.code.chars().count() > MAX_ERROR_CODE_CHARS
            || !failure
                .code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || !reported.insert(failure.provider)
        {
            return Err(StoreError::InvalidData(
                "VPN Gate IP intelligence failure is invalid or duplicated".to_owned(),
            ));
        }
    }
    if reported != expected {
        return Err(StoreError::InvalidData(
            "VPN Gate IP intelligence report must contain all three providers".to_owned(),
        ));
    }
    let mut observations = report.observations.clone();
    for observation in &mut observations {
        observation.country_code = normalize_country_code(&observation.country_code)?;
    }
    observations.sort_by_key(|observation| observation.provider);
    let country_codes = observations
        .iter()
        .map(|observation| observation.country_code.as_str())
        .collect::<BTreeSet<_>>();
    // One successful provider is usable evidence. Country disagreement is retained on each score
    // and evaluated by the configured country policy; it is not a failed intelligence refresh.
    let verified = !observations.is_empty();
    let complete = report.failures.is_empty();
    let country_code = (country_codes.len() == 1).then(|| observations[0].country_code.clone());
    let scores = observations
        .iter()
        .map(|observation| VpngateIpScore {
            provider: observation.provider,
            score: observation.score,
            country_code: observation.country_code.clone(),
        })
        .collect::<Vec<_>>();
    let networks = observations
        .iter()
        .map(|observation| VpngateIpNetwork {
            provider: observation.provider,
            isp: observation.isp.clone(),
            network_type: observation.network_type,
        })
        .collect::<Vec<_>>();
    let (error_code, error_detail) = if report.failures.is_empty() {
        (None, None)
    } else {
        let mut failures = report.failures.clone();
        failures.sort_by_key(|failure| failure.provider);
        (
            Some("provider-partial".to_owned()),
            Some(
                failures
                    .iter()
                    .map(|failure| format!("{}:{}", provider_name(failure.provider), failure.code))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        )
    };
    let policy = intelligence_policy(pool).await?;
    let periodic = matches!(
        policy.refresh_mode,
        VpngateIntelligenceRefreshMode::Periodic
    );
    let refresh_secs = intelligence_hours_to_secs(
        "intelligence refresh interval",
        policy.refresh_interval_hours,
    )?;
    let affected = sqlx::query(
        "UPDATE vpngate_exit_reputations
            SET country_code = CASE WHEN $6 THEN $4 ELSE country_code END,
                ip_scores = CASE WHEN $6 OR verified_at IS NULL THEN $5 ELSE ip_scores END,
                ip_networks = CASE WHEN $6 OR verified_at IS NULL THEN $7 ELSE ip_networks END,
                attempt_count = CASE WHEN $11 THEN 0 ELSE attempt_count + 1 END,
                next_check_at = CASE
                    WHEN $11 AND $12 THEN now() + make_interval(secs => $8)
                    WHEN $11 THEN 'infinity'::timestamptz
                    ELSE now() + make_interval(secs => LEAST(3600, 300 * (attempt_count + 1)))
                END,
                lease_owner = NULL,
                lease_until = NULL,
                last_error_code = $9,
                last_error_detail = $10,
                verified_at = CASE WHEN $6 THEN now() ELSE verified_at END,
                updated_at = now()
          WHERE exit_ip = $1::inet
            AND lease_owner = $2
            AND lease_generation = $3",
    )
    .bind(&report.exit_ip)
    .bind(node_id)
    .bind(u64_to_i64("lease_generation", report.lease_generation)?)
    .bind(country_code)
    .bind(serde_json::to_value(scores).map_err(|error| StoreError::InvalidData(error.to_string()))?)
    .bind(verified)
    .bind(
        serde_json::to_value(networks)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?,
    )
    .bind(refresh_secs)
    .bind(error_code)
    .bind(error_detail)
    .bind(complete)
    .bind(periodic)
    .execute(pool)
    .await?
    .rows_affected();
    if affected == 1 {
        Ok(())
    } else {
        Err(StoreError::Conflict(
            "VPN Gate IP intelligence lease was replaced by a newer worker".to_owned(),
        ))
    }
}

pub async fn claim_sync(
    pool: &PgPool,
    worker_id: &str,
    trigger: &str,
) -> Result<Option<VpngateSyncClaim>> {
    if worker_id.trim().is_empty() || worker_id.chars().count() > 128 {
        return Err(StoreError::InvalidData(
            "VPN Gate sync worker id is empty or too long".to_owned(),
        ));
    }
    if !matches!(trigger, "scheduled" | "manual" | "startup") {
        return Err(StoreError::InvalidData(
            "unknown VPN Gate sync trigger".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT enabled, source_url, lease_generation,
                next_sync_at <= now() AS due,
                lease_until IS NULL OR lease_until <= now() AS lease_available
           FROM vpngate_catalog_state
          WHERE id = TRUE
          FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await?;
    if !row.try_get::<bool, _>("enabled")?
        || !row.try_get::<bool, _>("due")?
        || !row.try_get::<bool, _>("lease_available")?
    {
        tx.commit().await?;
        return Ok(None);
    }
    let source_url: String = row.try_get("source_url")?;
    let lease_generation = row
        .try_get::<i64, _>("lease_generation")?
        .checked_add(1)
        .ok_or_else(|| StoreError::InvalidData("VPN Gate lease generation overflow".to_owned()))?;
    let run_id: i64 = sqlx::query_scalar(
        "INSERT INTO vpngate_sync_runs (source_url, worker_id, trigger)
         VALUES ($1, $2, $3)
         RETURNING id",
    )
    .bind(&source_url)
    .bind(worker_id)
    .bind(trigger)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE vpngate_catalog_state
            SET lease_owner = $1,
                lease_generation = $2,
                lease_until = now() + make_interval(secs => $3),
                active_run_id = $4,
                updated_at = now()
          WHERE id = TRUE",
    )
    .bind(worker_id)
    .bind(lease_generation)
    .bind(SYNC_LEASE_SECS)
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(VpngateSyncClaim {
        run_id: i64_to_u64("run_id", run_id)?,
        lease_generation: i64_to_u64("lease_generation", lease_generation)?,
        source_url,
    }))
}

pub async fn complete_sync(
    pool: &PgPool,
    worker_id: &str,
    claim: &VpngateSyncClaim,
    batch: VpngateSyncBatch,
) -> Result<()> {
    validate_batch(&batch)?;
    let mut tx = pool.begin().await?;
    lock_catalog_publication(&mut tx).await?;
    let lease_kind = lock_claim(&mut tx, worker_id, claim).await?;
    let run_id = u64_to_i64("run_id", claim.run_id)?;
    let reference_servers = match lease_kind {
        VpngateSyncLeaseKind::Global => i64_to_u64(
            "current VPN Gate server count",
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM vpngate_servers WHERE current AND country_code <> $1",
            )
            .bind(UNKNOWN_COUNTRY_CODE)
            .fetch_one(&mut *tx)
            .await?,
        )?,
        VpngateSyncLeaseKind::Collector => sqlx::query_scalar::<_, i32>(
            "SELECT accepted_rows
               FROM vpngate_sync_runs
              WHERE worker_id = $1 AND status = 'succeeded' AND id <> $2
              ORDER BY finished_at DESC, id DESC
              LIMIT 1",
        )
        .bind(worker_id)
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(|count| i32_to_u32("accepted_rows", count).map(u64::from))
        .transpose()?
        .unwrap_or(0),
    };
    validate_catalog_replacement(reference_servers, batch.servers.len())?;
    for server in &batch.servers {
        upsert_profile(&mut tx, run_id, server).await?;
    }
    if lease_kind == VpngateSyncLeaseKind::Global {
        // The retained global path is used only by storage fixtures and old single-source
        // deployments. A real Agent collection uses the multi-origin union below.
        sqlx::query("UPDATE vpngate_servers SET current = FALSE WHERE current")
            .execute(&mut *tx)
            .await?;
    }
    for server in &batch.servers {
        upsert_server(&mut tx, run_id, server).await?;
        insert_observation(&mut tx, run_id, server).await?;
    }
    let accepted_rows = u32::try_from(batch.servers.len())
        .map_err(|_| StoreError::InvalidData("too many accepted VPN Gate rows".to_owned()))?;
    let retained_observation_rows = batch
        .servers
        .iter()
        .filter(|server| server.country_code != UNKNOWN_COUNTRY_CODE)
        .count();
    sqlx::query(
        "UPDATE vpngate_catalog_state
            SET retained_observation_count = retained_observation_count + $1
          WHERE id = TRUE",
    )
    .bind(i64::try_from(retained_observation_rows).map_err(|_| {
        StoreError::InvalidData("too many retained VPN Gate observations".to_owned())
    })?)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE vpngate_sync_runs
            SET status = 'succeeded', content_sha256 = $2, fetched_rows = $3,
                accepted_rows = $4, rejected_rows = $5, finished_at = now()
          WHERE id = $1 AND status = 'running'",
    )
    .bind(run_id)
    .bind(&batch.content_sha256)
    .bind(u32_to_i32("fetched_rows", batch.fetched_rows)?)
    .bind(u32_to_i32("accepted_rows", accepted_rows)?)
    .bind(u32_to_i32("rejected_rows", batch.rejected_rows)?)
    .execute(&mut *tx)
    .await?;
    match lease_kind {
        VpngateSyncLeaseKind::Global => {
            sqlx::query(
                "UPDATE vpngate_catalog_state
                    SET next_sync_at = now() + make_interval(secs => interval_secs),
                        lease_owner = NULL, lease_until = NULL, active_run_id = NULL,
                        last_success_run_id = $1, last_error_code = NULL,
                        last_error_detail = NULL, updated_at = now()
                  WHERE id = TRUE",
            )
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        }
        VpngateSyncLeaseKind::Collector => {
            sqlx::query(
                "UPDATE vpngate_intelligence_nodes
                    SET catalogue_next_sync_at = now() + make_interval(secs => (
                            SELECT interval_secs FROM vpngate_catalog_state WHERE id = TRUE
                        )),
                        catalogue_lease_until = NULL,
                        catalogue_active_run_id = NULL,
                        catalogue_last_success_run_id = $2,
                        catalogue_last_error_code = NULL,
                        catalogue_last_error_detail = NULL
                  WHERE node_id = $1",
            )
            .bind(worker_id)
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
            refresh_current_catalog_union(&mut tx).await?;
            sqlx::query(
                "UPDATE vpngate_catalog_state
                    SET next_sync_at = COALESCE(
                            (SELECT MIN(catalogue_next_sync_at)
                               FROM vpngate_intelligence_nodes),
                            next_sync_at
                        ),
                        last_success_run_id = $1,
                        last_error_code = NULL,
                        last_error_detail = NULL,
                        updated_at = now()
                  WHERE id = TRUE",
            )
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

fn validate_catalog_replacement(current_servers: u64, accepted_servers: usize) -> Result<()> {
    let accepted_servers = u64::try_from(accepted_servers)
        .map_err(|_| StoreError::InvalidData("too many accepted VPN Gate rows".to_owned()))?;
    if current_servers >= 10
        && accepted_servers.saturating_mul(MAX_CURRENT_CATALOG_DROP_FACTOR) < current_servers
    {
        return Err(StoreError::InvalidData(format!(
            "VPN Gate catalogue dropped from {current_servers} to {accepted_servers} servers in one sync; preserving the last complete snapshot"
        )));
    }
    Ok(())
}

pub async fn fail_sync(
    pool: &PgPool,
    worker_id: &str,
    claim: &VpngateSyncClaim,
    error_code: &str,
    error_detail: &str,
) -> Result<()> {
    let error_code = bounded(error_code, MAX_ERROR_CODE_CHARS);
    let error_detail = bounded(error_detail, MAX_ERROR_DETAIL_CHARS);
    if error_code.is_empty() {
        return Err(StoreError::InvalidData(
            "VPN Gate sync error code must not be empty".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    let lease_kind = lock_claim(&mut tx, worker_id, claim).await?;
    let run_id = u64_to_i64("run_id", claim.run_id)?;
    sqlx::query(
        "UPDATE vpngate_sync_runs
            SET status = 'failed', error_code = $2, error_detail = $3, finished_at = now()
          WHERE id = $1 AND status = 'running'",
    )
    .bind(run_id)
    .bind(&error_code)
    .bind(&error_detail)
    .execute(&mut *tx)
    .await?;
    match lease_kind {
        VpngateSyncLeaseKind::Global => {
            sqlx::query(
                "UPDATE vpngate_catalog_state
                    SET next_sync_at = now() + make_interval(
                            secs => GREATEST(60, LEAST(interval_secs, 300))
                        ),
                        lease_owner = NULL, lease_until = NULL, active_run_id = NULL,
                        last_error_code = $1, last_error_detail = $2, updated_at = now()
                  WHERE id = TRUE",
            )
            .bind(&error_code)
            .bind(&error_detail)
            .execute(&mut *tx)
            .await?;
        }
        VpngateSyncLeaseKind::Collector => {
            sqlx::query(
                "UPDATE vpngate_intelligence_nodes
                    SET catalogue_next_sync_at = now() + make_interval(secs => (
                            SELECT GREATEST(60, LEAST(interval_secs, 300))
                              FROM vpngate_catalog_state WHERE id = TRUE
                        )),
                        catalogue_lease_until = NULL,
                        catalogue_active_run_id = NULL,
                        catalogue_last_error_code = $2,
                        catalogue_last_error_detail = $3
                  WHERE node_id = $1",
            )
            .bind(worker_id)
            .bind(&error_code)
            .bind(&error_detail)
            .execute(&mut *tx)
            .await?;
            // One origin can receive an interstitial, truncated response, or a transient upstream
            // failure while the other collectors remain healthy. Keep that evidence on the run
            // and collector, but do not turn a partial-origin failure into a catalogue-wide error.
            sqlx::query(
                "UPDATE vpngate_catalog_state
                    SET next_sync_at = COALESCE(
                            (SELECT MIN(catalogue_next_sync_at)
                               FROM vpngate_intelligence_nodes),
                            next_sync_at
                        ),
                        updated_at = now()
                  WHERE id = TRUE",
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

fn referenced_vpngate_pool_ids(snapshot: &ModelSnapshot, node_id: &str) -> BTreeSet<String> {
    let vpngate_ids = snapshot
        .external_outbounds
        .iter()
        .filter_map(|outbound| {
            matches!(
                &outbound.protocol,
                ExternalOutboundProtocol::Vpngate { country_code, .. }
                    if country_code != UNKNOWN_COUNTRY_CODE
            )
            .then_some(outbound.id.as_str())
        })
        .collect::<BTreeSet<_>>();
    snapshot
        .apps
        .iter()
        .flat_map(|app| &app.steps)
        .filter(|step| step.node == node_id)
        .flat_map(|step| &step.rules)
        .filter_map(|rule| match &rule.action {
            Action::Proxy { outbound } if vpngate_ids.contains(outbound.as_str()) => {
                Some(outbound.clone())
            }
            _ => None,
        })
        .collect()
}

pub async fn usable_probe_node_addresses(pool: &PgPool) -> Result<Vec<VpngateProbeNodeAddress>> {
    let minimum_protocol = i32::try_from(MIN_AGENT_PROTOCOL_VERSION)
        .expect("Agent protocol version is deliberately within PostgreSQL INTEGER");
    let rows = sqlx::query(
        "SELECT selected.node_id, node.public_ipv4
           FROM vpngate_probe_nodes selected
           JOIN nodes node ON node.id = selected.node_id
           JOIN node_agent_state state ON state.node_id = selected.node_id
           LEFT JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = selected.node_id
           LEFT JOIN node_operational_isolations isolation ON isolation.node_id = selected.node_id
          WHERE node.retired_at IS NULL
            AND node.public_ipv4 IS NOT NULL
            AND COALESCE(lifecycle.phase, 'active') = 'active'
            AND isolation.node_id IS NULL
            AND state.agent_protocol_version >= $1
            AND state.runtime_reported_at >= now() - interval '2 minutes'
            AND NULLIF(BTRIM(state.runtime_versions->>'openvpn'), '') IS NOT NULL
          ORDER BY selected.node_id",
    )
    .bind(minimum_protocol)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(VpngateProbeNodeAddress {
                node_id: row.try_get("node_id")?,
                public_ipv4: row.try_get("public_ipv4")?,
            })
        })
        .collect()
}

pub async fn agent_desired(pool: &PgPool, node_id: &str) -> Result<Option<VpngateDesiredState>> {
    agent_desired_with_probe_origins(pool, node_id, &[]).await
}

pub async fn agent_desired_with_probe_origins(
    pool: &PgPool,
    node_id: &str,
    probe_origins: &[VpngateProbeNodeOrigin],
) -> Result<Option<VpngateDesiredState>> {
    let row = sqlx::query(
        "SELECT topology_revision_id, generation, isolated_node_ids
           FROM subscription_serving_state
          WHERE id = TRUE",
    )
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let topology_revision =
        i64_to_u64("topology_revision_id", row.try_get("topology_revision_id")?)?;
    let isolated = row.try_get::<serde_json::Value, _>("isolated_node_ids")?;
    let isolated = isolated.as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item.as_str().is_some_and(|candidate| candidate == node_id))
    });
    // Missing OpenVPN is a supported Agent shape, not a convergence error. An updated Agent
    // reports the first line of `openvpn --version`; until that fact exists, return a complete
    // empty desired state so the node tears down any old pools and never receives provider
    // profiles it cannot run.
    let openvpn_available = sqlx::query_scalar::<_, Option<String>>(
        "SELECT NULLIF(BTRIM(runtime_versions->>'openvpn'), '')
           FROM node_agent_state
          WHERE node_id = $1",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await?
    .flatten()
    .is_some();
    let snapshot = crate::materialize::load_immutable_snapshot(pool, topology_revision).await?;
    let configured_countries = snapshot
        .external_outbounds
        .iter()
        .filter_map(|outbound| match &outbound.protocol {
            ExternalOutboundProtocol::Vpngate { country_code, .. }
                if country_code != UNKNOWN_COUNTRY_CODE =>
            {
                Some(country_code.clone())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let referenced = if !isolated && openvpn_available {
        referenced_vpngate_pool_ids(&snapshot, node_id)
    } else {
        BTreeSet::new()
    };
    let by_id = snapshot
        .external_outbounds
        .iter()
        .map(|outbound| (outbound.id.as_str(), outbound))
        .collect::<BTreeMap<_, _>>();
    let pools = referenced
        .iter()
        .filter_map(|id| {
            let outbound = by_id.get(id.as_str())?;
            matches!(
                &outbound.protocol,
                ExternalOutboundProtocol::Vpngate { country_code, .. }
                    if country_code != UNKNOWN_COUNTRY_CODE
            )
            .then_some(*outbound)
        })
        .collect::<Vec<_>>();
    let catalog_row = sqlx::query(
        "SELECT last_success_run_id, admission_policy, intelligence_policy,
                probe_success_cooldown_secs, probe_performance_cooldown_secs,
                probe_shard_rotation_secs
           FROM vpngate_catalog_state WHERE id = TRUE",
    )
    .fetch_one(pool)
    .await?;
    let catalog_generation = catalog_row
        .try_get::<Option<i64>, _>("last_success_run_id")?
        .map(|value| i64_to_u64("catalog_generation", value))
        .transpose()?
        .unwrap_or(0);
    let admission_policy: VpngateAdmissionPolicy =
        decode_json("admission_policy", catalog_row.try_get("admission_policy")?)?;
    if !validate_vpngate_admission_policy(&admission_policy) {
        return Err(StoreError::InvalidData(
            "stored VPN Gate admission policy is invalid".to_owned(),
        ));
    }
    let intelligence_policy =
        decode_intelligence_policy(catalog_row.try_get("intelligence_policy")?)?;
    let probe_success_cooldown_secs: i32 = catalog_row.try_get("probe_success_cooldown_secs")?;
    let probe_performance_cooldown_secs: i32 =
        catalog_row.try_get("probe_performance_cooldown_secs")?;
    let probe_shard_rotation_secs =
        i64::from(catalog_row.try_get::<i32, _>("probe_shard_rotation_secs")?);
    let probe_workers = probe_node_workers(pool, node_id).await?;
    let mut active_by_country = BTreeMap::<String, Vec<VpngateCandidate>>::new();
    // Candidate selection is needed only by automatic pools on this node. `configured_countries`
    // is fleet-wide and also feeds catalogue probe assignment below; using it here made every
    // Agent rescan the complete candidate history for every configured country every ten seconds,
    // including nodes without OpenVPN and pools pinned to explicit servers.
    for country_code in automatic_pool_countries(pools.iter().map(|outbound| &outbound.protocol)) {
        let candidates =
            active_candidates(pool, &country_code, &admission_policy, &intelligence_policy).await?;
        active_by_country.insert(country_code, candidates);
    }

    let mut desired_pools = Vec::with_capacity(pools.len());
    for (slot, outbound) in pools.into_iter().enumerate() {
        let ExternalOutboundProtocol::Vpngate {
            country_code,
            server_id,
            server_ids,
            max_connect_ms,
            min_download_bps,
            max_candidates,
        } = &outbound.protocol
        else {
            continue;
        };
        let peer = vpngate_runtime_peer(slot).ok_or_else(|| {
            StoreError::InvalidData("VPN Gate runtime slot exceeds compiler limit".to_owned())
        })?;
        let mut host_octets = peer.octets();
        host_octets[3] = host_octets[3].checked_sub(1).ok_or_else(|| {
            StoreError::InvalidData("VPN Gate runtime peer has no host pair".to_owned())
        })?;
        let candidates = if !server_ids.is_empty() {
            // Only the explicitly selected relays may enter this pool. Missing or retired IDs
            // stay unavailable; never refill from the automatic country shortlist.
            let mut selected = Vec::new();
            let mut ids = server_ids.iter().collect::<Vec<_>>();
            ids.sort();
            for id in ids {
                if let Some(candidate) =
                    candidate_by_id(pool, country_code, id, node_id, &intelligence_policy).await?
                {
                    selected.push(candidate);
                }
            }
            selected
        } else if let Some(server_id) = server_id {
            candidate_by_id(pool, country_code, server_id, node_id, &intelligence_policy)
                .await?
                .into_iter()
                .collect()
        } else {
            active_by_country
                .get(country_code)
                .into_iter()
                .flatten()
                .filter(|candidate| {
                    evaluate_vpngate_admission(
                        &admission_policy,
                        country_code,
                        &candidate.verified_ip_scores,
                    ) == VpngateAdmissionDecision::Admitted
                })
                .take(usize::from(*max_candidates))
                .cloned()
                .collect()
        };
        let manual_switch = if server_id.is_none() && server_ids.is_empty() {
            sqlx::query(
                "SELECT id, previous_server_id
                   FROM vpngate_pool_switch_requests
                  WHERE node_id = $1 AND outbound_id = $2 AND status = 'pending'
                  ORDER BY id DESC
                  LIMIT 1",
            )
            .bind(node_id)
            .bind(&outbound.id)
            .fetch_optional(pool)
            .await?
            .map(|row| -> Result<VpngateManualSwitchCommand> {
                Ok(VpngateManualSwitchCommand {
                    request_id: i64_to_u64("switch request id", row.try_get("id")?)?,
                    previous_server_id: row.try_get("previous_server_id")?,
                    cooldown_secs: MANUAL_SWITCH_COOLDOWN_SECS,
                })
            })
            .transpose()?
        } else {
            None
        };
        desired_pools.push(VpngateDesiredPool {
            outbound_id: outbound.id.clone(),
            country_code: country_code.clone(),
            max_connect_ms: *max_connect_ms,
            min_download_bps: *min_download_bps,
            max_candidates: *max_candidates,
            runtime_slot: u16::try_from(slot).map_err(|_| {
                StoreError::InvalidData("VPN Gate runtime slot exceeds u16".to_owned())
            })?,
            host_address: std::net::Ipv4Addr::from(host_octets).to_string(),
            peer_address: peer.to_string(),
            prefix_len: 30,
            socks_port: VPNGATE_RUNTIME_SOCKS_PORT,
            candidates,
            manual_switch,
        });
    }
    let probe_assignments = if !isolated && openvpn_available {
        match probe_workers {
            Some(workers) => {
                next_retained_probe_assignments(
                    pool,
                    node_id,
                    &configured_countries,
                    workers,
                    probe_origins,
                    VpngateProbeSchedule {
                        connectivity_cooldown_secs: probe_success_cooldown_secs,
                        performance_cooldown_secs: probe_performance_cooldown_secs,
                        shard_rotation_secs: probe_shard_rotation_secs,
                    },
                )
                .await?
            }
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    Ok(Some(VpngateDesiredState {
        topology_revision,
        catalog_generation,
        admission_policy,
        pools: desired_pools,
        probe_assignments,
    }))
}

async fn probe_node_workers(pool: &PgPool, node_id: &str) -> Result<Option<usize>> {
    let minimum_protocol = i32::try_from(MIN_AGENT_PROTOCOL_VERSION)
        .expect("Agent protocol version is deliberately within PostgreSQL INTEGER");
    let Some(row) = sqlx::query(
        "SELECT selected.workers,
                state.runtime_versions->>'vpngate_catalog_probe_workers' AS reported_workers
           FROM vpngate_probe_nodes selected
           JOIN nodes node ON node.id = selected.node_id
           JOIN node_agent_state state ON state.node_id = selected.node_id
           LEFT JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = selected.node_id
           LEFT JOIN node_operational_isolations isolation ON isolation.node_id = selected.node_id
          WHERE selected.node_id = $1
            AND node.retired_at IS NULL
            AND COALESCE(lifecycle.phase, 'active') = 'active'
            AND isolation.node_id IS NULL
            AND state.agent_protocol_version >= $2
            AND state.runtime_reported_at >= now() - interval '2 minutes'
            AND NULLIF(BTRIM(state.runtime_versions->>'openvpn'), '') IS NOT NULL",
    )
    .bind(node_id)
    .bind(minimum_protocol)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let configured = usize::try_from(row.try_get::<i32, _>("workers")?).map_err(|_| {
        StoreError::InvalidData("stored VPN Gate probe workers are invalid".to_owned())
    })?;
    if !(1..=MAX_CATALOG_PROBE_WORKERS).contains(&configured) {
        return Err(StoreError::InvalidData(
            "stored VPN Gate probe workers are invalid".to_owned(),
        ));
    }
    let reported = row.try_get::<Option<String>, _>("reported_workers")?;
    let available = reported
        .as_deref()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, MAX_CATALOG_PROBE_WORKERS);
    Ok(Some(configured.min(available)))
}

async fn active_candidates(
    pool: &PgPool,
    country_code: &str,
    admission_policy: &VpngateAdmissionPolicy,
    intelligence_policy: &VpngateIntelligencePolicy,
) -> Result<Vec<VpngateCandidate>> {
    let ranked = ranked_candidates(pool, country_code, admission_policy, intelligence_policy)
        .await?
        .into_iter()
        .take(
            usize::try_from(MAX_ACTIVE_CANDIDATES_PER_COUNTRY)
                .expect("active VPN Gate candidate limit fits usize"),
        )
        .collect();
    hydrate_ranked_candidates(pool, ranked).await
}

async fn ranked_candidates(
    pool: &PgPool,
    country_code: &str,
    admission_policy: &VpngateAdmissionPolicy,
    intelligence_policy: &VpngateIntelligencePolicy,
) -> Result<Vec<VpngateRankedCandidate>> {
    if country_code == UNKNOWN_COUNTRY_CODE {
        return Ok(Vec::new());
    }
    let candidates = qualified_candidates(pool, Some(country_code), intelligence_policy).await?;
    let admitted = candidates
        .into_iter()
        .filter(|candidate| {
            evaluate_vpngate_admission(
                admission_policy,
                country_code,
                &candidate.candidate.verified_ip_scores,
            ) == VpngateAdmissionDecision::Admitted
        })
        .collect::<Vec<_>>();
    Ok(rank_candidates_by_risk(admitted))
}

fn automatic_pool_countries<'a>(
    protocols: impl IntoIterator<Item = &'a ExternalOutboundProtocol>,
) -> BTreeSet<String> {
    protocols
        .into_iter()
        .filter_map(|protocol| match protocol {
            ExternalOutboundProtocol::Vpngate {
                country_code,
                server_id: None,
                server_ids,
                ..
            } if country_code != UNKNOWN_COUNTRY_CODE && server_ids.is_empty() => {
                Some(country_code.clone())
            }
            _ => None,
        })
        .collect()
}

/// Order admitted automatic-pool candidates without manufacturing a cross-provider risk score.
///
/// Within the same provider set, a candidate dominates another only when every provider-local
/// score is no higher and at least one is lower. Missing provider sets remain incomparable rather
/// than being rewarded or penalized by their length. Repeatedly removing the non-dominated
/// frontier produces Pareto layers; fleet download and connect quality order candidates inside a
/// layer. Counts of providers, probing machines, and historical samples deliberately do not rank.
fn rank_candidates_by_risk(
    candidates: Vec<VpngateQualifiedCandidate>,
) -> Vec<VpngateRankedCandidate> {
    let layers = pareto_risk_layers(&candidates);
    let mut indexed = candidates.into_iter().enumerate().collect::<Vec<_>>();
    indexed.sort_by(|(left_index, left), (right_index, right)| {
        layers[*left_index]
            .cmp(&layers[*right_index])
            .then_with(|| right.global_download_bps.cmp(&left.global_download_bps))
            .then_with(|| left.global_connect_ms.cmp(&right.global_connect_ms))
            .then_with(|| left.candidate.server_id.cmp(&right.candidate.server_id))
    });
    indexed
        .into_iter()
        .map(|(index, candidate)| VpngateRankedCandidate {
            candidate: candidate.candidate,
            pareto_layer: u32::try_from(layers[index]).unwrap_or(u32::MAX),
            global_download_bps: candidate.global_download_bps,
            global_connect_ms: candidate.global_connect_ms,
        })
        .collect()
}

fn pareto_risk_layers(candidates: &[VpngateQualifiedCandidate]) -> Vec<usize> {
    let signatures = candidates
        .iter()
        .map(|candidate| {
            let mut signature = candidate
                .candidate
                .verified_ip_scores
                .iter()
                .map(|score| (score.provider, score.score))
                .collect::<Vec<_>>();
            signature.sort();
            signature
        })
        .collect::<Vec<_>>();
    let mut evaluation_order = (0..candidates.len()).collect::<Vec<_>>();
    evaluation_order.sort_by(|left, right| {
        signatures[*left]
            .cmp(&signatures[*right])
            .then_with(|| left.cmp(right))
    });

    let mut layers = vec![1_usize; candidates.len()];
    for (position, candidate_index) in evaluation_order.iter().copied().enumerate() {
        for better_index in evaluation_order[..position].iter().copied() {
            if risk_signature_dominates(&signatures[better_index], &signatures[candidate_index]) {
                layers[candidate_index] = layers[candidate_index].max(layers[better_index] + 1);
            }
        }
    }
    layers
}

fn risk_signature_dominates(
    left: &[(VpngateIpProvider, u8)],
    right: &[(VpngateIpProvider, u8)],
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut strictly_lower = false;
    for ((left_provider, left_score), (right_provider, right_score)) in
        left.iter().zip(right.iter())
    {
        if left_provider != right_provider || left_score > right_score {
            return false;
        }
        strictly_lower |= left_score < right_score;
    }
    strictly_lower
}

async fn active_candidate_counts(
    pool: &PgPool,
    admission_policy: &VpngateAdmissionPolicy,
    intelligence_policy: &VpngateIntelligencePolicy,
) -> Result<BTreeMap<String, u64>> {
    let candidates = qualified_candidates(pool, None, intelligence_policy).await?;
    let limit = u64::try_from(MAX_ACTIVE_CANDIDATES_PER_COUNTRY)
        .expect("active VPN Gate candidate limit fits u64");
    let mut counts = BTreeMap::<String, u64>::new();
    for candidate in candidates {
        if evaluate_vpngate_admission(
            admission_policy,
            &candidate.candidate.country_code,
            &candidate.candidate.verified_ip_scores,
        ) != VpngateAdmissionDecision::Admitted
        {
            continue;
        }
        let count = counts.entry(candidate.candidate.country_code).or_default();
        if *count < limit {
            *count += 1;
        }
    }
    Ok(counts)
}

fn candidate_networks_have_excluded_isp(networks: &[VpngateIpNetwork]) -> bool {
    // Provider claims are kept separately; one identified excluded ISP is enough to keep the
    // relay out of serving, even when another provider reports a different network name.
    networks
        .iter()
        .filter_map(|network| network.isp.as_deref())
        .any(|isp| {
            let normalized = isp.trim().to_ascii_lowercase();
            normalized.contains("optage") || normalized.contains("chubu telecommunications company")
        })
}

async fn qualified_candidates(
    pool: &PgPool,
    country_code: Option<&str>,
    intelligence_policy: &VpngateIntelligencePolicy,
) -> Result<Vec<VpngateQualifiedCandidate>> {
    let maximum_age_secs = match intelligence_policy.stale_policy {
        VpngateStaleIntelligencePolicy::Reject => Some(intelligence_hours_to_secs(
            "intelligence stale age",
            intelligence_policy.stale_after_hours,
        )?),
        VpngateStaleIntelligencePolicy::Retain | VpngateStaleIntelligencePolicy::Mark => None,
    };
    let rows = sqlx::query(
        "WITH eligible AS MATERIALIZED (
            SELECT sample.node_id, sample.server_id, sample.profile_sha256,
                   sample.last_success_exit_ip AS exit_ip,
                   sample.last_success_connect_ms AS connect_ms,
                   sample.last_success_download_bps AS download_bps,
                   sample.last_success_probed_at AS probed_at
              FROM vpngate_candidate_probe_latest sample
             CROSS JOIN vpngate_catalog_state schedule
              JOIN vpngate_exit_reputations reputation
                ON reputation.exit_ip = sample.last_success_exit_ip
              LEFT JOIN vpngate_candidate_probe_state probe_state
                ON probe_state.server_id = sample.server_id
               AND probe_state.profile_sha256 = sample.profile_sha256
             WHERE sample.last_success_received_at >= now() - make_interval(
                       secs => GREATEST(
                           $5,
                           schedule.probe_performance_cooldown_secs
                               + schedule.probe_success_cooldown_secs
                       )
                   )
               AND ($1::TEXT IS NULL OR sample.country_code = $1)
               AND COALESCE(probe_state.consecutive_failures, 0) < $7
               AND (COALESCE(probe_state.consecutive_failures, 0) = 0
                    OR probe_state.failure_streak_started_at
                        >= now() - make_interval(secs => $8))
               AND ($2::INTEGER IS NULL
                    OR reputation.verified_at >= now() - make_interval(secs => $2))
               AND jsonb_array_length(reputation.ip_scores) >= 1
        ), qualified AS MATERIALIZED (
            SELECT sample.server_id, sample.profile_sha256,
                   AVG(sample.download_bps) AS average_download,
                   AVG(sample.connect_ms) AS average_connect
              FROM eligible sample
             WHERE sample.connect_ms > 0
               AND sample.connect_ms <= $3
               AND sample.download_bps >= $4
             GROUP BY sample.server_id, sample.profile_sha256
        ), evidence AS MATERIALIZED (
            SELECT DISTINCT ON (sample.server_id, sample.profile_sha256)
                   sample.server_id, sample.profile_sha256, sample.exit_ip
              FROM eligible sample
             ORDER BY sample.server_id, sample.profile_sha256,
                      sample.probed_at DESC, sample.node_id, sample.exit_ip
        )
        SELECT server.id, server.country_code, server.profile_sha256,
               host(evidence.exit_ip) AS verified_exit_ip,
               reputation.country_code AS verified_exit_country_code,
               reputation.ip_scores AS verified_ip_scores,
               reputation.ip_networks AS verified_ip_networks,
               ROUND(qualified.average_download)::BIGINT AS global_download_bps,
               ROUND(qualified.average_connect)::INTEGER AS global_connect_ms
          FROM qualified
          JOIN vpngate_servers server
            ON server.id = qualified.server_id
           AND server.profile_sha256 = qualified.profile_sha256
          JOIN evidence
            ON evidence.server_id = qualified.server_id
           AND evidence.profile_sha256 = qualified.profile_sha256
          JOIN vpngate_exit_reputations reputation ON reputation.exit_ip = evidence.exit_ip
         WHERE server.country_code <> $6
           AND ($1::TEXT IS NULL OR server.country_code = $1)
         ORDER BY server.country_code,
                  qualified.average_download DESC,
                  qualified.average_connect ASC,
                  server.id",
    )
    .bind(country_code)
    .bind(maximum_age_secs)
    .bind(ACTIVE_MAX_CONNECT_MS)
    .bind(ACTIVE_MIN_DOWNLOAD_BPS)
    .bind(ACTIVE_PROBE_MIN_MAX_AGE_SECS)
    .bind(UNKNOWN_COUNTRY_CODE)
    .bind(MAX_CONSECUTIVE_PROBE_FAILURES)
    .bind(FAILED_CANDIDATE_GRACE_SECS)
    .fetch_all(pool)
    .await?;
    let candidates = rows
        .iter()
        .map(qualified_candidate_from_row)
        .collect::<Result<Vec<_>>>()?;
    Ok(candidates
        .into_iter()
        .filter(|candidate| {
            !candidate_networks_have_excluded_isp(&candidate.candidate.verified_ip_networks)
        })
        .collect())
}

fn qualified_candidate_from_row(row: &sqlx::postgres::PgRow) -> Result<VpngateQualifiedCandidate> {
    Ok(VpngateQualifiedCandidate {
        candidate: VpngateCandidateEvaluation {
            server_id: row.try_get("id")?,
            country_code: row.try_get("country_code")?,
            profile_sha256: row.try_get("profile_sha256")?,
            verified_exit_ip: row.try_get("verified_exit_ip")?,
            verified_exit_country_code: row.try_get("verified_exit_country_code")?,
            verified_ip_scores: decode_json(
                "verified_ip_scores",
                row.try_get("verified_ip_scores")?,
            )?,
            verified_ip_networks: decode_json(
                "verified_ip_networks",
                row.try_get("verified_ip_networks")?,
            )?,
        },
        global_download_bps: i64_to_u64(
            "global_download_bps",
            row.try_get("global_download_bps")?,
        )?,
        global_connect_ms: i32_to_u32("global_connect_ms", row.try_get("global_connect_ms")?)?,
    })
}

async fn hydrate_ranked_candidates(
    pool: &PgPool,
    ranked: Vec<VpngateRankedCandidate>,
) -> Result<Vec<VpngateCandidate>> {
    if ranked.is_empty() {
        return Ok(Vec::new());
    }
    let server_ids = ranked
        .iter()
        .map(|candidate| candidate.candidate.server_id.clone())
        .collect::<Vec<_>>();
    let profile_sha256s = ranked
        .iter()
        .map(|candidate| candidate.candidate.profile_sha256.clone())
        .collect::<Vec<_>>();
    let rows = sqlx::query(
        "WITH selected AS (
             SELECT server_id, profile_sha256
               FROM unnest($1::TEXT[], $2::TEXT[]) AS item(server_id, profile_sha256)
         )
         SELECT server.id, server.hostname, server.country_code,
                host(profile.remote_address) AS remote_address, profile.remote_port,
                profile.transport, profile.sha256 AS profile_sha256, profile.openvpn_config
           FROM selected
           JOIN vpngate_servers server
             ON server.id = selected.server_id
            AND server.profile_sha256 = selected.profile_sha256
           JOIN vpngate_profiles profile ON profile.sha256 = selected.profile_sha256",
    )
    .bind(&server_ids)
    .bind(&profile_sha256s)
    .fetch_all(pool)
    .await?;
    let mut profiles = rows
        .iter()
        .map(candidate_profile_from_row)
        .map(|profile| {
            profile.map(|profile| {
                (
                    (profile.server_id.clone(), profile.profile_sha256.clone()),
                    profile,
                )
            })
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(ranked
        .into_iter()
        .filter_map(|ranked| {
            let evaluation = ranked.candidate;
            let profile = profiles.remove(&(
                evaluation.server_id.clone(),
                evaluation.profile_sha256.clone(),
            ))?;
            Some(VpngateCandidate {
                server_id: profile.server_id,
                hostname: profile.hostname,
                country_code: profile.country_code,
                remote_address: profile.remote_address,
                remote_port: profile.remote_port,
                transport: profile.transport,
                profile_sha256: profile.profile_sha256,
                openvpn_config: profile.openvpn_config,
                probe_mode: VpngateProbeMode::Performance,
                last_observed_exit_ip: evaluation.verified_exit_ip.clone(),
                verified_exit_ip: evaluation.verified_exit_ip,
                verified_exit_country_code: evaluation.verified_exit_country_code,
                verified_ip_scores: evaluation.verified_ip_scores,
                verified_ip_networks: evaluation.verified_ip_networks,
            })
        })
        .collect())
}

fn candidate_profile_from_row(row: &sqlx::postgres::PgRow) -> Result<VpngateCandidateProfile> {
    let transport = match row.try_get::<String, _>("transport")?.as_str() {
        "udp" => VpngateTransport::Udp,
        "tcp" => VpngateTransport::Tcp,
        value => {
            return Err(StoreError::InvalidData(format!(
                "unknown stored VPN Gate transport {value}"
            )))
        }
    };
    Ok(VpngateCandidateProfile {
        server_id: row.try_get("id")?,
        hostname: row.try_get("hostname")?,
        country_code: row.try_get("country_code")?,
        remote_address: row.try_get("remote_address")?,
        remote_port: u16::try_from(row.try_get::<i32, _>("remote_port")?).map_err(|_| {
            StoreError::InvalidData("VPN Gate remote_port is outside u16".to_owned())
        })?,
        transport,
        profile_sha256: row.try_get("profile_sha256")?,
        openvpn_config: row.try_get("openvpn_config")?,
    })
}

async fn candidate_by_id(
    pool: &PgPool,
    country_code: &str,
    server_id: &str,
    node_id: &str,
    intelligence_policy: &VpngateIntelligencePolicy,
) -> Result<Option<VpngateCandidate>> {
    if country_code == UNKNOWN_COUNTRY_CODE {
        return Ok(None);
    }
    let maximum_age_secs = match intelligence_policy.stale_policy {
        VpngateStaleIntelligencePolicy::Reject => Some(intelligence_hours_to_secs(
            "intelligence stale age",
            intelligence_policy.stale_after_hours,
        )?),
        VpngateStaleIntelligencePolicy::Retain | VpngateStaleIntelligencePolicy::Mark => None,
    };
    let row = sqlx::query(
        "SELECT server.id, server.hostname, server.country_code,
                host(profile.remote_address) AS remote_address, profile.remote_port,
                profile.transport, profile.sha256 AS profile_sha256, profile.openvpn_config,
                host(verified.exit_ip) AS verified_exit_ip,
                verified.country_code AS verified_exit_country_code,
                COALESCE(verified.ip_scores, '[]'::jsonb) AS verified_ip_scores,
                COALESCE(verified.ip_networks, '[]'::jsonb) AS verified_ip_networks
           FROM vpngate_servers server
           JOIN vpngate_profiles profile ON profile.sha256 = server.profile_sha256
           LEFT JOIN LATERAL (
                SELECT evidence.exit_ip, reputation.country_code,
                       reputation.ip_scores, reputation.ip_networks
                  FROM (
                       SELECT sample.node_id, sample.last_success_exit_ip AS exit_ip,
                              sample.last_success_probed_at AS probed_at
                         FROM vpngate_candidate_probe_latest sample
                        WHERE sample.server_id = server.id
                          AND sample.profile_sha256 = profile.sha256
                          AND sample.last_success_probed_at IS NOT NULL
                       UNION ALL
                       SELECT sample.node_id, sample.exit_ip, sample.probed_at
                         FROM vpngate_probe_samples sample
                        WHERE sample.server_id = server.id
                          AND sample.profile_sha256 = profile.sha256
                          AND sample.status = 'succeeded'
                  ) evidence
                  JOIN vpngate_exit_reputations reputation
                    ON reputation.exit_ip = evidence.exit_ip
                 WHERE ($4::INTEGER IS NULL
                        OR reputation.verified_at >= now() - make_interval(secs => $4))
                   AND jsonb_array_length(reputation.ip_scores) >= 1
                 ORDER BY (evidence.node_id = $3) DESC, evidence.probed_at DESC
                 LIMIT 1
           ) verified ON TRUE
          WHERE server.country_code = $1 AND server.id = $2",
    )
    .bind(country_code)
    .bind(server_id)
    .bind(node_id)
    .bind(maximum_age_secs)
    .fetch_optional(pool)
    .await?;
    let candidate = row.as_ref().map(candidate_from_row).transpose()?;
    Ok(candidate
        .filter(|candidate| !candidate_networks_have_excluded_isp(&candidate.verified_ip_networks)))
}

fn eligible_probe_countries(
    node_id: &str,
    country_codes: &[String],
    origins: &[VpngateProbeNodeOrigin],
) -> Vec<String> {
    let origins = origins
        .iter()
        .filter_map(|origin| {
            let country_code = origin.country_code.trim().to_ascii_uppercase();
            vpngate_probe_region(&country_code).map(|region| (origin.node_id.clone(), region))
        })
        .collect::<BTreeMap<_, _>>();

    // GeoIP is a best-effort local database. During cold start, preserve the old complete queue
    // rather than making every selected Agent idle; the Console switches to regional placement as
    // soon as at least one usable selected machine has a country.
    if origins.is_empty() {
        return country_codes.to_vec();
    }

    country_codes
        .iter()
        .filter_map(|country_code| {
            let target = vpngate_probe_region(country_code);
            let minimum_distance = target.and_then(|target| {
                origins
                    .values()
                    .map(|origin| vpngate_probe_region_distance(target, *origin))
                    .min()
            });
            origins
                .iter()
                .any(|(origin_node_id, origin)| {
                    origin_node_id == node_id
                        && match (target, minimum_distance) {
                            (Some(target), Some(distance)) => {
                                vpngate_probe_region_distance(target, *origin) == distance
                            }
                            // A future or private country code must still make progress. Every
                            // known origin participates until the region table is extended.
                            _ => true,
                        }
                })
                .then_some(country_code.clone())
        })
        .collect()
}

/// Peers which share this machine's regional work are also its catalogue shard group. The query
/// below gives every profile to exactly one peer for the configured rotation window, then advances
/// ownership by one sorted peer so a machine does not permanently measure the same subset. Tied
/// nearest regions deliberately keep separate shard groups because another region is useful
/// independent evidence.
fn probe_shard_peers(node_id: &str, origins: &[VpngateProbeNodeOrigin]) -> Vec<String> {
    let regions = origins
        .iter()
        .filter_map(|origin| {
            let country_code = origin.country_code.trim().to_ascii_uppercase();
            vpngate_probe_region(&country_code).map(|region| (origin.node_id.as_str(), region))
        })
        .collect::<BTreeMap<_, _>>();
    let mut peers = match regions.get(node_id) {
        Some(node_region) => regions
            .iter()
            .filter_map(|(peer_id, peer_region)| {
                (peer_region == node_region).then_some((*peer_id).to_owned())
            })
            .collect::<Vec<_>>(),
        None if regions.is_empty() => origins
            .iter()
            .map(|origin| origin.node_id.clone())
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    peers.sort();
    peers.dedup();
    if peers.is_empty() {
        peers.push(node_id.to_owned());
    }
    peers
}

/// Build one work-conserving regional batch across country boundaries, capped by this selected
/// machine's configured worker count and its reported Agent capability. Machines in the same
/// region partition one complete regional queue; tied nearest regions retain separate queues so
/// genuinely distinct origins still produce independent evidence. No task lease is needed because
/// rotating ownership is recomputed from the currently usable peer set on every desired read.
#[derive(Clone, Copy)]
struct VpngateProbeSchedule {
    connectivity_cooldown_secs: i32,
    performance_cooldown_secs: i32,
    shard_rotation_secs: i64,
}

async fn next_retained_probe_assignments(
    pool: &PgPool,
    node_id: &str,
    configured_countries: &BTreeSet<String>,
    workers: usize,
    origins: &[VpngateProbeNodeOrigin],
    schedule: VpngateProbeSchedule,
) -> Result<Vec<VpngateProbeAssignment>> {
    let configured_countries = configured_countries.iter().cloned().collect::<Vec<_>>();
    let country_codes = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT country_code
           FROM vpngate_servers
          WHERE country_code <> $1
          ORDER BY country_code",
    )
    .bind(UNKNOWN_COUNTRY_CODE)
    .fetch_all(pool)
    .await?;
    let scoped_countries = eligible_probe_countries(node_id, &country_codes, origins);
    if scoped_countries.is_empty() {
        return Ok(Vec::new());
    }
    let shard_peers = probe_shard_peers(node_id, origins);
    let rows = sqlx::query(
        "SELECT server.id, server.hostname, server.country_code,
                host(profile.remote_address) AS remote_address, profile.remote_port,
                profile.transport, profile.sha256 AS profile_sha256,
                profile.openvpn_config,
                COALESCE(
                    probe_state.consecutive_failures BETWEEN 1 AND 2
                    AND probe_state.failure_streak_started_at
                        >= now() - make_interval(secs => $6),
                    FALSE
                )
                    AS review_priority,
                server.current
                    AND server.country_code = ANY($2)
                    AND latest.received_at IS NULL AS serving_priority,
                latest.received_at AS last_received_at,
                server.current,
                performance.last_observed_exit_ip,
                (
                    probe_state.last_outcome_status IS DISTINCT FROM 'succeeded'
                    OR performance.last_performance_received_at IS NULL
                    OR performance.last_performance_received_at
                        <= now() - make_interval(secs => $9)
                ) AS performance_due,
                NULL::TEXT AS verified_exit_ip,
                NULL::TEXT AS verified_exit_country_code,
                '[]'::jsonb AS verified_ip_scores,
                '[]'::jsonb AS verified_ip_networks
           FROM vpngate_servers server
           JOIN vpngate_profiles profile ON profile.sha256 = server.profile_sha256
           LEFT JOIN vpngate_candidate_probe_latest latest
             ON latest.node_id = $1
            AND latest.server_id = server.id
            AND latest.profile_sha256 = profile.sha256
           LEFT JOIN vpngate_candidate_probe_state probe_state
             ON probe_state.server_id = server.id
            AND probe_state.profile_sha256 = profile.sha256
           LEFT JOIN LATERAL (
                SELECT host(sample.last_success_exit_ip) AS last_observed_exit_ip,
                       sample.last_success_received_at AS last_performance_received_at
                  FROM vpngate_candidate_probe_latest sample
                 WHERE sample.server_id = server.id
                   AND sample.profile_sha256 = profile.sha256
                   AND sample.last_success_received_at IS NOT NULL
                 ORDER BY sample.last_success_received_at DESC, sample.node_id
                 LIMIT 1
           ) performance ON TRUE
          WHERE server.country_code = ANY($5)
            AND server.country_code <> $4
            AND (
                probe_state.last_outcome_status IS DISTINCT FROM 'succeeded'
                OR probe_state.last_outcome_received_at
                    <= now() - make_interval(secs => $8)
            )
            AND $1 = (
                SELECT peer.node_id
                  FROM (
                       SELECT candidate.node_id,
                              row_number() OVER (ORDER BY candidate.node_id) - 1 AS peer_index,
                              count(*) OVER () AS peer_count
                         FROM UNNEST($7::TEXT[]) AS candidate(node_id)
                  ) peer
                 WHERE peer.peer_index = mod(
                           mod(
                               (('x' || substr(
                                   md5(server.id || ':' || profile.sha256),
                                   1,
                                   15
                               ))::bit(60)::bigint),
                               peer.peer_count
                           ) + mod(
                               floor(EXTRACT(EPOCH FROM now()) / $10)::bigint,
                               peer.peer_count
                           ),
                           peer.peer_count
                       )
            )
          ORDER BY review_priority DESC,
                   serving_priority DESC,
                   last_received_at ASC NULLS FIRST,
                   server.current DESC,
                   server.country_code,
                   server.id
          LIMIT $3",
    )
    .bind(node_id)
    .bind(configured_countries)
    .bind(i64::try_from(workers).map_err(|_| {
        StoreError::InvalidData("VPN Gate probe workers exceed PostgreSQL BIGINT".to_owned())
    })?)
    .bind(UNKNOWN_COUNTRY_CODE)
    .bind(scoped_countries)
    .bind(FAILED_CANDIDATE_GRACE_SECS)
    .bind(shard_peers)
    .bind(schedule.connectivity_cooldown_secs)
    .bind(schedule.performance_cooldown_secs)
    .bind(schedule.shard_rotation_secs)
    .fetch_all(pool)
    .await?;
    let mut assignments = Vec::<VpngateProbeAssignment>::new();
    for row in &rows {
        let mut candidate = candidate_from_row(row)?;
        candidate.probe_mode = if row.try_get("performance_due")? {
            VpngateProbeMode::Performance
        } else {
            VpngateProbeMode::Connectivity
        };
        candidate.last_observed_exit_ip = row.try_get("last_observed_exit_ip")?;
        if let Some(assignment) = assignments
            .iter_mut()
            .find(|assignment| assignment.country_code == candidate.country_code)
        {
            assignment.candidates.push(candidate);
        } else {
            assignments.push(VpngateProbeAssignment {
                country_code: candidate.country_code.clone(),
                candidates: vec![candidate],
            });
        }
    }
    Ok(assignments)
}

fn candidate_from_row(row: &sqlx::postgres::PgRow) -> Result<VpngateCandidate> {
    let transport = match row.try_get::<String, _>("transport")?.as_str() {
        "udp" => VpngateTransport::Udp,
        "tcp" => VpngateTransport::Tcp,
        value => {
            return Err(StoreError::InvalidData(format!(
                "unknown stored VPN Gate transport {value}"
            )))
        }
    };
    Ok(VpngateCandidate {
        server_id: row.try_get("id")?,
        hostname: row.try_get("hostname")?,
        country_code: row.try_get("country_code")?,
        remote_address: row.try_get("remote_address")?,
        remote_port: u16::try_from(row.try_get::<i32, _>("remote_port")?).map_err(|_| {
            StoreError::InvalidData("VPN Gate remote_port is outside u16".to_owned())
        })?,
        transport,
        profile_sha256: row.try_get("profile_sha256")?,
        openvpn_config: row.try_get("openvpn_config")?,
        probe_mode: VpngateProbeMode::Performance,
        last_observed_exit_ip: None,
        verified_exit_ip: row.try_get("verified_exit_ip")?,
        verified_exit_country_code: row.try_get("verified_exit_country_code")?,
        verified_ip_scores: decode_json("verified_ip_scores", row.try_get("verified_ip_scores")?)?,
        verified_ip_networks: decode_json(
            "verified_ip_networks",
            row.try_get("verified_ip_networks")?,
        )?,
    })
}

pub async fn record_agent_report(
    pool: &PgPool,
    node_id: &str,
    report: VpngatePoolReport,
) -> Result<VpngateReportReceipt> {
    validate_report(&report)?;
    let mut tx = pool.begin().await?;
    let expected = current_reconcile_pool_ids_tx(
        &mut tx,
        node_id,
        report.topology_revision,
        report.catalog_generation,
    )
    .await?;
    let mut exit_reputations = Vec::new();
    let accepted_samples =
        record_report_samples_tx(&mut tx, node_id, &report, &mut exit_reputations).await?;
    queue_exit_reputations(&mut tx, &exit_reputations).await?;
    let current_state_updated = expected
        .as_ref()
        .is_some_and(|ids| ids.contains(&report.outbound_id));
    if current_state_updated {
        validate_selected_profile_tx(&mut tx, &report).await?;
        record_manual_switch_result_tx(&mut tx, node_id, &report).await?;
        upsert_pool_state_tx(&mut tx, node_id, &report).await?;
    }
    tx.commit().await?;
    Ok(VpngateReportReceipt {
        accepted_samples,
        current_state_updated,
    })
}

/// Store one complete post-reconcile pool set and remove every replaceable state row omitted by
/// the Agent. Samples remain immutable history even when their generation is stale; state
/// replacement and deletion happen only when both echoed generations and the complete expected
/// pool identity set match the current desired state.
pub async fn record_reconcile_report(
    pool: &PgPool,
    node_id: &str,
    report: VpngateReconcileReport,
) -> Result<VpngateReportReceipt> {
    validate_reconcile_report(&report)?;
    let mut tx = pool.begin().await?;
    let expected = current_reconcile_pool_ids_tx(
        &mut tx,
        node_id,
        report.topology_revision,
        report.catalog_generation,
    )
    .await?;
    let mut accepted_samples = 0_u32;
    let mut exit_reputations = Vec::new();
    for pool_report in &report.pools {
        accepted_samples = accepted_samples.saturating_add(
            record_report_samples_tx(&mut tx, node_id, pool_report, &mut exit_reputations).await?,
        );
    }
    queue_exit_reputations(&mut tx, &exit_reputations).await?;
    let reported_ids = report
        .pools
        .iter()
        .map(|pool| pool.outbound_id.clone())
        .collect::<BTreeSet<_>>();
    let current_state_updated = expected.as_ref() == Some(&reported_ids);
    if current_state_updated {
        for pool_report in &report.pools {
            validate_selected_profile_tx(&mut tx, pool_report).await?;
            record_manual_switch_result_tx(&mut tx, node_id, pool_report).await?;
            upsert_pool_state_tx(&mut tx, node_id, pool_report).await?;
        }
        let retained_ids = reported_ids.into_iter().collect::<Vec<_>>();
        sqlx::query(
            "DELETE FROM vpngate_node_pool_state
              WHERE node_id = $1
                AND NOT (outbound_id = ANY($2::text[]))",
        )
        .bind(node_id)
        .bind(&retained_ids)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(VpngateReportReceipt {
        accepted_samples,
        current_state_updated,
    })
}

async fn record_report_samples_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    report: &VpngatePoolReport,
    exit_reputations: &mut Vec<(String, i64)>,
) -> Result<u32> {
    let mut accepted_samples = 0_u32;
    for sample in &report.samples {
        let identity_matches = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
                SELECT 1 FROM vpngate_profiles
                 WHERE sha256 = $1 AND server_id = $2
            )",
        )
        .bind(&sample.profile_sha256)
        .bind(&sample.server_id)
        .fetch_one(&mut **tx)
        .await?;
        if !identity_matches {
            return Err(StoreError::InvalidData(format!(
                "VPN Gate report profile does not belong to server {}",
                sample.server_id
            )));
        }
        let connect_ms = if matches!(sample.status, VpngateProbeStatus::Succeeded)
            && sample.connect_ms == Some(0)
        {
            // Agent versions before runtime metadata carried the original dial measurement used
            // zero when periodically rechecking an already connected backend. Preserve that
            // successful route observation without turning the sentinel into an impossible 0 ms
            // dial: reuse the latest real measurement for this exact backend. If none exists,
            // omit only this sample; replaceable runtime state is still accepted below.
            sqlx::query_scalar::<_, i32>(
                "SELECT connect_ms
                   FROM vpngate_probe_samples
                  WHERE node_id = $1 AND outbound_id = $2
                    AND server_id = $3 AND profile_sha256 = $4
                    AND status = 'succeeded' AND connect_ms > 0
                  ORDER BY probed_at DESC, id DESC
                  LIMIT 1",
            )
            .bind(node_id)
            .bind(&report.outbound_id)
            .bind(&sample.server_id)
            .bind(&sample.profile_sha256)
            .fetch_optional(&mut **tx)
            .await?
            .map(|value| i32_to_u32("connect_ms", value))
            .transpose()?
        } else {
            sample.connect_ms
        };
        if matches!(sample.status, VpngateProbeStatus::Succeeded) && connect_ms.is_none() {
            continue;
        }
        let status = match sample.status {
            VpngateProbeStatus::Succeeded => "succeeded",
            VpngateProbeStatus::Failed => "failed",
        };
        if matches!(sample.status, VpngateProbeStatus::Succeeded) {
            let exit_ip = sample
                .exit_ip
                .as_deref()
                .expect("validated successful VPN Gate samples have an exit IP");
            exit_reputations.push((exit_ip.to_owned(), sample.probed_at_unix_secs));
        }
        let (exit_country_code, ip_scores, ip_networks) = match sample.exit_ip.as_deref() {
            Some(exit_ip) => {
                verified_reputation(tx, exit_ip)
                    .await?
                    .unwrap_or((None, Vec::new(), Vec::new()))
            }
            None => (None, Vec::new(), Vec::new()),
        };
        let affected = sqlx::query(
            "INSERT INTO vpngate_probe_samples
                (node_id, outbound_id, topology_revision, catalog_generation, server_id,
                 profile_sha256, status, exit_ip, exit_country_code, connect_ms, download_bps,
                 ip_scores, ip_networks, error_code, error_detail, probed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::inet, $9, $10, $11, $12, $13,
                     $14, $15, to_timestamp($16::double precision))
             ON CONFLICT (node_id, outbound_id, server_id, profile_sha256, probed_at) DO NOTHING",
        )
        .bind(node_id)
        .bind(&report.outbound_id)
        .bind(u64_to_i64("topology_revision", report.topology_revision)?)
        .bind(u64_to_i64("catalog_generation", report.catalog_generation)?)
        .bind(&sample.server_id)
        .bind(&sample.profile_sha256)
        .bind(status)
        .bind(sample.exit_ip.as_deref())
        .bind(exit_country_code.as_deref())
        .bind(optional_u32_to_i32("connect_ms", connect_ms)?)
        .bind(
            sample
                .download_bps
                .map(|value| u64_to_i64("download_bps", value))
                .transpose()?,
        )
        .bind(
            serde_json::to_value(ip_scores)
                .map_err(|error| StoreError::InvalidData(error.to_string()))?,
        )
        .bind(
            serde_json::to_value(ip_networks)
                .map_err(|error| StoreError::InvalidData(error.to_string()))?,
        )
        .bind(sample.error_code.as_deref().map(|value| bounded(value, 64)))
        .bind(
            sample
                .error_detail
                .as_deref()
                .map(|value| bounded(value, MAX_ERROR_DETAIL_CHARS)),
        )
        .bind(sample.probed_at_unix_secs)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        accepted_samples = accepted_samples.saturating_add(u32::try_from(affected).unwrap_or(1));
    }
    Ok(accepted_samples)
}

async fn current_reconcile_pool_ids_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    topology_revision: u64,
    catalog_generation: u64,
) -> Result<Option<BTreeSet<String>>> {
    // Keep the generation check and omission deletion in one publication epoch. Callers acquire
    // these publication locks before writing exit-IP reputation rows, preserving the catalog ->
    // reputation lock order used by policy updates. Otherwise an old report could pass the check,
    // race a new publication, and delete rows already written for that newer desired state.
    let current = sqlx::query(
        "SELECT serving.topology_revision_id,
                COALESCE(catalog.last_success_run_id, 0) AS catalog_generation,
                serving.isolated_node_ids,
                NULLIF(BTRIM(agent.runtime_versions->>'openvpn'), '') IS NOT NULL
                    AS openvpn_available
           FROM subscription_serving_state serving
           CROSS JOIN vpngate_catalog_state catalog
           LEFT JOIN node_agent_state agent ON agent.node_id = $1
          WHERE serving.id = TRUE AND catalog.id = TRUE
          FOR SHARE OF serving, catalog",
    )
    .bind(node_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    if current.try_get::<i64, _>("topology_revision_id")?
        != u64_to_i64("topology_revision", topology_revision)?
        || current.try_get::<i64, _>("catalog_generation")?
            != u64_to_i64("catalog_generation", catalog_generation)?
    {
        return Ok(None);
    }
    let isolated = current
        .try_get::<serde_json::Value, _>("isolated_node_ids")?
        .as_array()
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.as_str().is_some_and(|candidate| candidate == node_id))
        });
    if isolated || !current.try_get::<bool, _>("openvpn_available")? {
        return Ok(Some(BTreeSet::new()));
    }
    let snapshot = crate::materialize::load_immutable_snapshot_tx(tx, topology_revision).await?;
    Ok(Some(referenced_vpngate_pool_ids(&snapshot, node_id)))
}

async fn validate_selected_profile_tx(
    tx: &mut Transaction<'_, Postgres>,
    report: &VpngatePoolReport,
) -> Result<()> {
    let Some((server_id, profile_sha256)) = report
        .selected_server_id
        .as_deref()
        .zip(report.applied_profile_sha256.as_deref())
    else {
        return Ok(());
    };
    let selected_matches = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
            SELECT 1 FROM vpngate_servers server
            JOIN vpngate_profiles profile ON profile.sha256 = server.profile_sha256
            WHERE server.id = $1 AND profile.sha256 = $2
        )",
    )
    .bind(server_id)
    .bind(profile_sha256)
    .fetch_one(&mut **tx)
    .await?;
    if !selected_matches {
        return Err(StoreError::InvalidData(
            "selected VPN Gate server/profile is not in the retained directory".to_owned(),
        ));
    }
    Ok(())
}

async fn record_manual_switch_result_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    report: &VpngatePoolReport,
) -> Result<()> {
    let Some(result) = &report.manual_switch_result else {
        return Ok(());
    };
    let request_id = u64_to_i64("switch request id", result.request_id)?;
    let rows_affected = match result.status {
        // The first acknowledgement must describe the backend that is active in this same
        // snapshot. Once the request is terminal, later complete snapshots deliberately keep
        // replaying the historical result even if an automatic failover has since selected a
        // different backend; the exact stored-result check below makes that replay idempotent.
        VpngateManualSwitchStatus::Applied
            if result.selected_server_id == report.selected_server_id =>
        {
            let selected_server_id = result
                .selected_server_id
                .as_deref()
                .expect("validated applied switch result has a selected server");
            let cooldown_until = result
                .cooldown_until_unix_secs
                .expect("validated applied switch result has a cooldown deadline");
            sqlx::query(
                "UPDATE vpngate_pool_switch_requests
                    SET status = 'applied', selected_server_id = $5,
                        cooldown_until = to_timestamp($6::double precision),
                        error_detail = NULL, completed_at = now()
                  WHERE id = $1 AND node_id = $2 AND outbound_id = $3
                    AND previous_server_id = $4 AND status = 'pending'",
            )
            .bind(request_id)
            .bind(node_id)
            .bind(&report.outbound_id)
            .bind(&result.previous_server_id)
            .bind(selected_server_id)
            .bind(cooldown_until)
            .execute(&mut **tx)
            .await?
            .rows_affected()
        }
        VpngateManualSwitchStatus::Applied => 0,
        VpngateManualSwitchStatus::Failed => {
            let error_detail = result
                .error_detail
                .as_deref()
                .expect("validated failed switch result has an error detail");
            sqlx::query(
                "UPDATE vpngate_pool_switch_requests
                    SET status = 'failed', selected_server_id = NULL,
                        cooldown_until = NULL, error_detail = $5, completed_at = now()
                  WHERE id = $1 AND node_id = $2 AND outbound_id = $3
                    AND previous_server_id = $4 AND status = 'pending'",
            )
            .bind(request_id)
            .bind(node_id)
            .bind(&report.outbound_id)
            .bind(&result.previous_server_id)
            .bind(bounded(error_detail, MAX_ERROR_DETAIL_CHARS))
            .execute(&mut **tx)
            .await?
            .rows_affected()
        }
    };
    if rows_affected == 1 {
        return Ok(());
    }

    // The Agent includes its last terminal result in every complete runtime report. Accept the
    // exact stored result again, but reject an unrelated or contradictory request identity.
    let stored = sqlx::query(
        "SELECT node_id, outbound_id, previous_server_id, status, selected_server_id,
                EXTRACT(EPOCH FROM cooldown_until)::BIGINT AS cooldown_until_unix_secs,
                error_detail
           FROM vpngate_pool_switch_requests
          WHERE id = $1",
    )
    .bind(request_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(stored) = stored else {
        return Err(StoreError::InvalidData(
            "VPN Gate switch result references an unknown request".to_owned(),
        ));
    };
    let stored_matches = stored.try_get::<String, _>("node_id")? == node_id
        && stored.try_get::<String, _>("outbound_id")? == report.outbound_id
        && stored.try_get::<String, _>("previous_server_id")? == result.previous_server_id
        && stored.try_get::<String, _>("status")?
            == match result.status {
                VpngateManualSwitchStatus::Applied => "applied",
                VpngateManualSwitchStatus::Failed => "failed",
            }
        && stored.try_get::<Option<String>, _>("selected_server_id")? == result.selected_server_id
        && stored.try_get::<Option<i64>, _>("cooldown_until_unix_secs")?
            == result.cooldown_until_unix_secs
        && stored.try_get::<Option<String>, _>("error_detail")?
            == result
                .error_detail
                .as_deref()
                .map(|value| bounded(value, MAX_ERROR_DETAIL_CHARS));
    if !stored_matches {
        return Err(StoreError::InvalidData(
            "VPN Gate switch result contradicts the stored request".to_owned(),
        ));
    }
    Ok(())
}

async fn upsert_pool_state_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    report: &VpngatePoolReport,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO vpngate_node_pool_state
                (node_id, outbound_id, topology_revision, catalog_generation, runtime_status,
                 selected_server_id, applied_profile_sha256, reported_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, now())
             ON CONFLICT (node_id, outbound_id) DO UPDATE SET
                topology_revision = EXCLUDED.topology_revision,
                catalog_generation = EXCLUDED.catalog_generation,
                runtime_status = EXCLUDED.runtime_status,
                selected_server_id = EXCLUDED.selected_server_id,
                applied_profile_sha256 = EXCLUDED.applied_profile_sha256,
                reported_at = now()",
    )
    .bind(node_id)
    .bind(&report.outbound_id)
    .bind(u64_to_i64("topology_revision", report.topology_revision)?)
    .bind(u64_to_i64("catalog_generation", report.catalog_generation)?)
    .bind(&report.runtime_status)
    .bind(report.selected_server_id.as_deref())
    .bind(report.applied_profile_sha256.as_deref())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn record_probe_report(
    pool: &PgPool,
    node_id: &str,
    report: VpngateProbeReport,
) -> Result<VpngateReportReceipt> {
    validate_probe_report(&report)?;
    let mut tx = pool.begin().await?;
    let mut accepted_samples = 0_u32;
    let mut exit_reputations = Vec::new();
    for sample in &report.samples {
        // Catalogue collection runs independently from multi-minute OpenVPN probes. The retained
        // directory is authoritative for its current identity; immutable observations cover an
        // in-flight assignment when that identity changes before its report arrives. History is
        // bounded, so requiring an observation alone would permanently reject a retained server
        // once its last observation aged out and block the Agent's durable report queue.
        let identity_matches = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
                SELECT 1
                  FROM vpngate_servers server
                 WHERE server.profile_sha256 = $1
                   AND server.id = $2
                   AND server.country_code = $3
                UNION ALL
                SELECT 1
                  FROM vpngate_server_observations observation
                 WHERE observation.profile_sha256 = $1
                   AND observation.server_id = $2
                   AND observation.country_code = $3
            )",
        )
        .bind(&sample.profile_sha256)
        .bind(&sample.server_id)
        .bind(&report.country_code)
        .fetch_one(&mut *tx)
        .await?;
        if !identity_matches {
            return Err(StoreError::InvalidData(format!(
                "VPN Gate catalogue sample profile does not belong to {} in {}",
                sample.server_id, report.country_code
            )));
        }
        if matches!(sample.status, VpngateProbeStatus::Succeeded) {
            let exit_ip = sample
                .exit_ip
                .as_deref()
                .expect("validated successful VPN Gate samples have an exit IP");
            exit_reputations.push((exit_ip.to_owned(), sample.probed_at_unix_secs));
        }
        let status = match sample.status {
            VpngateProbeStatus::Succeeded => "succeeded",
            VpngateProbeStatus::Failed => "failed",
        };
        let affected = sqlx::query(
            "INSERT INTO vpngate_candidate_probe_samples
                (node_id, catalog_generation, country_code, server_id, profile_sha256,
                 status, exit_ip, connect_ms, download_bps, error_code, error_detail, probed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7::inet, $8, $9, $10, $11,
                     to_timestamp($12::double precision))
             ON CONFLICT (node_id, server_id, profile_sha256, probed_at) DO NOTHING",
        )
        .bind(node_id)
        .bind(u64_to_i64("catalog_generation", report.catalog_generation)?)
        .bind(&report.country_code)
        .bind(&sample.server_id)
        .bind(&sample.profile_sha256)
        .bind(status)
        .bind(sample.exit_ip.as_deref())
        .bind(optional_u32_to_i32("connect_ms", sample.connect_ms)?)
        .bind(
            sample
                .download_bps
                .map(|value| u64_to_i64("download_bps", value))
                .transpose()?,
        )
        .bind(sample.error_code.as_deref().map(|value| bounded(value, 64)))
        .bind(
            sample
                .error_detail
                .as_deref()
                .map(|value| bounded(value, MAX_ERROR_DETAIL_CHARS)),
        )
        .bind(sample.probed_at_unix_secs)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if affected == 1 {
            upsert_candidate_latest(
                &mut tx,
                node_id,
                report.catalog_generation,
                &report.country_code,
                sample,
            )
            .await?;
            update_candidate_probe_state(&mut tx, node_id, sample).await?;
        }
        accepted_samples = accepted_samples.saturating_add(u32::try_from(affected).unwrap_or(1));
    }
    queue_exit_reputations(&mut tx, &exit_reputations).await?;
    tx.commit().await?;
    Ok(VpngateReportReceipt {
        accepted_samples,
        current_state_updated: false,
    })
}

/// Remove bounded chunks of expired VPN Gate evidence.
///
/// The maintenance loop calls this hourly. Limiting each table to one chunk prevents a fresh
/// deployment over an existing unbounded history from generating a single huge DELETE/WAL burst;
/// the steady-state expiry rate is comfortably below the batch size.
pub async fn prune_history(
    pool: &PgPool,
    candidate_retain_days: u32,
    observation_retain_days: u32,
) -> Result<u64> {
    let candidate_retain_days =
        i32::try_from(candidate_retain_days.clamp(1, 365)).expect("retention fits i32");
    let observation_retain_days =
        i32::try_from(observation_retain_days.clamp(1, 365)).expect("retention fits i32");
    let mut tx = pool.begin().await?;
    let candidate_rows = sqlx::query_scalar::<_, i64>(
        "WITH expired AS MATERIALIZED (
             SELECT ctid
               FROM vpngate_candidate_probe_samples
              WHERE received_at < now() - make_interval(days => $1)
              LIMIT $2
         ), deleted AS (
             DELETE FROM vpngate_candidate_probe_samples history
              USING expired
              WHERE history.ctid = expired.ctid
              RETURNING 1
         )
         SELECT count(*) FROM deleted",
    )
    .bind(candidate_retain_days)
    .bind(HISTORY_PRUNE_BATCH_ROWS)
    .fetch_one(&mut *tx)
    .await?;
    let (observation_rows, retained_observation_rows) = sqlx::query_as::<_, (i64, i64)>(
        "WITH expired AS MATERIALIZED (
             SELECT ctid
               FROM vpngate_server_observations
              WHERE observed_at < now() - make_interval(days => $1)
              LIMIT $2
         ), deleted AS (
             DELETE FROM vpngate_server_observations history
              USING expired
              WHERE history.ctid = expired.ctid
              RETURNING history.country_code
         )
         SELECT count(*), count(*) FILTER (WHERE country_code <> $3) FROM deleted",
    )
    .bind(observation_retain_days)
    .bind(HISTORY_PRUNE_BATCH_ROWS)
    .bind(UNKNOWN_COUNTRY_CODE)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE vpngate_catalog_state
            SET retained_observation_count = GREATEST(
                    retained_observation_count - $1,
                    0
                )
          WHERE id = TRUE",
    )
    .bind(retained_observation_rows)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let removed = candidate_rows
        .checked_add(observation_rows)
        .ok_or_else(|| StoreError::InvalidData("VPN Gate prune count overflow".to_owned()))?;
    u64::try_from(removed)
        .map_err(|_| StoreError::InvalidData("negative VPN Gate prune count".to_owned()))
}

async fn upsert_candidate_latest(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    catalog_generation: u64,
    country_code: &str,
    sample: &VpngateProbeSample,
) -> Result<()> {
    let status = match sample.status {
        VpngateProbeStatus::Succeeded => "succeeded",
        VpngateProbeStatus::Failed => "failed",
    };
    sqlx::query(
        "INSERT INTO vpngate_candidate_probe_latest
            (node_id, catalog_generation, country_code, server_id, profile_sha256,
             status, exit_ip, connect_ms, download_bps, error_code, error_detail, probed_at,
             received_at, last_success_exit_ip, last_success_connect_ms,
             last_success_download_bps, last_success_probed_at, last_success_received_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7::inet, $8, $9, $10, $11,
                 to_timestamp($12::double precision), now(),
                 CASE WHEN $6 = 'succeeded' AND $9 IS NOT NULL THEN $7::inet END,
                 CASE WHEN $6 = 'succeeded' AND $9 IS NOT NULL THEN $8 END,
                 CASE WHEN $6 = 'succeeded' AND $9 IS NOT NULL THEN $9 END,
                 CASE WHEN $6 = 'succeeded' AND $9 IS NOT NULL
                      THEN to_timestamp($12::double precision) END,
                 CASE WHEN $6 = 'succeeded' AND $9 IS NOT NULL THEN now() END)
         ON CONFLICT (node_id, server_id, profile_sha256) DO UPDATE SET
            catalog_generation = CASE WHEN EXCLUDED.probed_at > vpngate_candidate_probe_latest.probed_at
                THEN EXCLUDED.catalog_generation ELSE vpngate_candidate_probe_latest.catalog_generation END,
            country_code = CASE WHEN EXCLUDED.probed_at > vpngate_candidate_probe_latest.probed_at
                THEN EXCLUDED.country_code ELSE vpngate_candidate_probe_latest.country_code END,
            status = CASE WHEN EXCLUDED.probed_at > vpngate_candidate_probe_latest.probed_at
                THEN EXCLUDED.status ELSE vpngate_candidate_probe_latest.status END,
            exit_ip = CASE WHEN EXCLUDED.probed_at > vpngate_candidate_probe_latest.probed_at
                THEN EXCLUDED.exit_ip ELSE vpngate_candidate_probe_latest.exit_ip END,
            connect_ms = CASE WHEN EXCLUDED.probed_at > vpngate_candidate_probe_latest.probed_at
                THEN EXCLUDED.connect_ms ELSE vpngate_candidate_probe_latest.connect_ms END,
            download_bps = CASE WHEN EXCLUDED.probed_at > vpngate_candidate_probe_latest.probed_at
                THEN EXCLUDED.download_bps ELSE vpngate_candidate_probe_latest.download_bps END,
            error_code = CASE WHEN EXCLUDED.probed_at > vpngate_candidate_probe_latest.probed_at
                THEN EXCLUDED.error_code ELSE vpngate_candidate_probe_latest.error_code END,
            error_detail = CASE WHEN EXCLUDED.probed_at > vpngate_candidate_probe_latest.probed_at
                THEN EXCLUDED.error_detail ELSE vpngate_candidate_probe_latest.error_detail END,
            probed_at = GREATEST(EXCLUDED.probed_at, vpngate_candidate_probe_latest.probed_at),
            received_at = CASE WHEN EXCLUDED.probed_at > vpngate_candidate_probe_latest.probed_at
                THEN EXCLUDED.received_at ELSE vpngate_candidate_probe_latest.received_at END,
            last_success_exit_ip = CASE
                WHEN EXCLUDED.last_success_probed_at >
                    vpngate_candidate_probe_latest.last_success_probed_at
                    OR vpngate_candidate_probe_latest.last_success_probed_at IS NULL
                THEN EXCLUDED.last_success_exit_ip
                ELSE vpngate_candidate_probe_latest.last_success_exit_ip END,
            last_success_connect_ms = CASE
                WHEN EXCLUDED.last_success_probed_at >
                    vpngate_candidate_probe_latest.last_success_probed_at
                    OR vpngate_candidate_probe_latest.last_success_probed_at IS NULL
                THEN EXCLUDED.last_success_connect_ms
                ELSE vpngate_candidate_probe_latest.last_success_connect_ms END,
            last_success_download_bps = CASE
                WHEN EXCLUDED.last_success_probed_at >
                    vpngate_candidate_probe_latest.last_success_probed_at
                    OR vpngate_candidate_probe_latest.last_success_probed_at IS NULL
                THEN EXCLUDED.last_success_download_bps
                ELSE vpngate_candidate_probe_latest.last_success_download_bps END,
            last_success_probed_at = GREATEST(
                EXCLUDED.last_success_probed_at,
                vpngate_candidate_probe_latest.last_success_probed_at
            ),
            last_success_received_at = CASE
                WHEN EXCLUDED.last_success_probed_at >
                    vpngate_candidate_probe_latest.last_success_probed_at
                    OR vpngate_candidate_probe_latest.last_success_probed_at IS NULL
                THEN EXCLUDED.last_success_received_at
                ELSE vpngate_candidate_probe_latest.last_success_received_at END",
    )
    .bind(node_id)
    .bind(u64_to_i64("catalog_generation", catalog_generation)?)
    .bind(country_code)
    .bind(&sample.server_id)
    .bind(&sample.profile_sha256)
    .bind(status)
    .bind(sample.exit_ip.as_deref())
    .bind(optional_u32_to_i32("connect_ms", sample.connect_ms)?)
    .bind(
        sample
            .download_bps
            .map(|value| u64_to_i64("download_bps", value))
            .transpose()?,
    )
    .bind(sample.error_code.as_deref().map(|value| bounded(value, 64)))
    .bind(
        sample
            .error_detail
            .as_deref()
            .map(|value| bounded(value, MAX_ERROR_DETAIL_CHARS)),
    )
    .bind(sample.probed_at_unix_secs)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn update_candidate_probe_state(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    sample: &VpngateProbeSample,
) -> Result<()> {
    let status = match sample.status {
        VpngateProbeStatus::Succeeded => "succeeded",
        VpngateProbeStatus::Failed => "failed",
    };
    let initial_failures = i32::from(matches!(sample.status, VpngateProbeStatus::Failed));
    sqlx::query(
        "INSERT INTO vpngate_candidate_probe_state
            (server_id, profile_sha256, last_outcome_status, last_outcome_node_id,
             last_outcome_at, last_outcome_received_at, failure_streak_started_at,
             consecutive_failures)
         VALUES ($1, $2, $3, $4, to_timestamp($5::double precision), now(),
                 CASE WHEN $3 = 'failed' THEN now() ELSE NULL END, $6)
         ON CONFLICT (server_id, profile_sha256) DO UPDATE SET
            last_outcome_status = EXCLUDED.last_outcome_status,
            last_outcome_node_id = EXCLUDED.last_outcome_node_id,
            last_outcome_at = EXCLUDED.last_outcome_at,
            last_outcome_received_at = EXCLUDED.last_outcome_received_at,
            failure_streak_started_at = CASE
                WHEN EXCLUDED.last_outcome_status = 'succeeded' THEN NULL
                WHEN vpngate_candidate_probe_state.last_outcome_status = 'succeeded'
                    THEN EXCLUDED.failure_streak_started_at
                ELSE vpngate_candidate_probe_state.failure_streak_started_at
            END,
            consecutive_failures = CASE
                WHEN EXCLUDED.last_outcome_status = 'succeeded' THEN 0
                WHEN vpngate_candidate_probe_state.last_outcome_status = 'succeeded' THEN 1
                ELSE LEAST(
                    vpngate_candidate_probe_state.consecutive_failures + 1,
                    $7
                )
            END
         WHERE EXCLUDED.last_outcome_at >= vpngate_candidate_probe_state.last_outcome_at",
    )
    .bind(&sample.server_id)
    .bind(&sample.profile_sha256)
    .bind(status)
    .bind(node_id)
    .bind(sample.probed_at_unix_secs)
    .bind(initial_failures)
    .bind(MAX_CONSECUTIVE_PROBE_FAILURES)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn queue_exit_reputations(
    tx: &mut Transaction<'_, Postgres>,
    observations: &[(String, i64)],
) -> Result<()> {
    if observations.is_empty() {
        return Ok(());
    }
    let (exit_ips, probed_at_unix_secs): (Vec<_>, Vec<_>) =
        deduplicate_exit_reputations(observations)?
            .into_iter()
            .unzip();
    sqlx::query(
        "INSERT INTO vpngate_exit_reputations (exit_ip, last_seen_at)
         SELECT input.exit_ip::inet,
                LEAST(to_timestamp(input.probed_at::double precision), now())
           FROM UNNEST($1::text[], $2::bigint[]) AS input(exit_ip, probed_at)
          ORDER BY input.exit_ip::inet
         ON CONFLICT (exit_ip) DO UPDATE SET
            last_seen_at = GREATEST(
                vpngate_exit_reputations.last_seen_at,
                EXCLUDED.last_seen_at
            ),
            updated_at = now()
         WHERE vpngate_exit_reputations.last_seen_at < EXCLUDED.last_seen_at",
    )
    .bind(&exit_ips)
    .bind(&probed_at_unix_secs)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn deduplicate_exit_reputations(observations: &[(String, i64)]) -> Result<Vec<(String, i64)>> {
    let mut latest_by_exit_ip = BTreeMap::<std::net::IpAddr, i64>::new();
    for (exit_ip, probed_at) in observations {
        let exit_ip = exit_ip.parse::<std::net::IpAddr>().map_err(|_| {
            StoreError::InvalidData("exit reputation observation has an invalid IP".to_owned())
        })?;
        latest_by_exit_ip
            .entry(exit_ip)
            .and_modify(|stored| *stored = (*stored).max(*probed_at))
            .or_insert(*probed_at);
    }
    Ok(latest_by_exit_ip
        .into_iter()
        .map(|(exit_ip, probed_at)| (exit_ip.to_string(), probed_at))
        .collect())
}

async fn verified_reputation(
    tx: &mut Transaction<'_, Postgres>,
    exit_ip: &str,
) -> Result<Option<(Option<String>, Vec<VpngateIpScore>, Vec<VpngateIpNetwork>)>> {
    let row = sqlx::query(
        "SELECT country_code, ip_scores, ip_networks
           FROM vpngate_exit_reputations
          WHERE exit_ip = $1::inet
            AND verified_at IS NOT NULL",
    )
    .bind(exit_ip)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|row| {
        Ok((
            row.try_get("country_code")?,
            decode_json("ip_scores", row.try_get("ip_scores")?)?,
            decode_json("ip_networks", row.try_get("ip_networks")?)?,
        ))
    })
    .transpose()
}

fn validate_report(report: &VpngatePoolReport) -> Result<()> {
    if report.outbound_id.trim().is_empty() || report.outbound_id.chars().count() > 128 {
        return Err(StoreError::InvalidData(
            "VPN Gate report outbound id is invalid".to_owned(),
        ));
    }
    if report.samples.len() > usize::from(VPNGATE_MAX_CANDIDATES) {
        return Err(StoreError::InvalidData(format!(
            "VPN Gate report contains more than {VPNGATE_MAX_CANDIDATES} samples"
        )));
    }
    if !matches!(
        report.runtime_status.as_str(),
        "pending" | "probing" | "running" | "degraded" | "failed" | "disabled"
    ) {
        return Err(StoreError::InvalidData(
            "VPN Gate runtime status is unknown".to_owned(),
        ));
    }
    if report.selected_server_id.is_some() != report.applied_profile_sha256.is_some() {
        return Err(StoreError::InvalidData(
            "VPN Gate selected server and applied profile must be reported together".to_owned(),
        ));
    }
    if let Some(result) = &report.manual_switch_result {
        if result.request_id == 0
            || result.previous_server_id.trim().is_empty()
            || result.previous_server_id.chars().count() > 128
        {
            return Err(StoreError::InvalidData(
                "VPN Gate switch result identity is invalid".to_owned(),
            ));
        }
        match result.status {
            VpngateManualSwitchStatus::Applied
                if result.selected_server_id.is_none()
                    || result.selected_server_id == Some(result.previous_server_id.clone())
                    || result.cooldown_until_unix_secs.is_none()
                    || result.error_detail.is_some() =>
            {
                return Err(StoreError::InvalidData(
                    "applied VPN Gate switch result is incomplete".to_owned(),
                ));
            }
            VpngateManualSwitchStatus::Failed
                if result.selected_server_id.is_some()
                    || result.cooldown_until_unix_secs.is_some()
                    || result.error_detail.as_deref().is_none_or(str::is_empty)
                    || result
                        .error_detail
                        .as_deref()
                        .is_some_and(|value| value.chars().count() > MAX_ERROR_DETAIL_CHARS) =>
            {
                return Err(StoreError::InvalidData(
                    "failed VPN Gate switch result is incomplete".to_owned(),
                ));
            }
            _ => {}
        }
    }
    for sample in &report.samples {
        validate_sample(sample, false)?;
    }
    Ok(())
}

fn validate_reconcile_report(report: &VpngateReconcileReport) -> Result<()> {
    if report.pools.len() > VPNGATE_RUNTIME_MAX_POOLS_PER_NODE {
        return Err(StoreError::InvalidData(format!(
            "VPN Gate reconcile report contains more than {VPNGATE_RUNTIME_MAX_POOLS_PER_NODE} pools"
        )));
    }
    let mut outbound_ids = BTreeSet::new();
    for pool in &report.pools {
        validate_report(pool)?;
        if pool.topology_revision != report.topology_revision
            || pool.catalog_generation != report.catalog_generation
        {
            return Err(StoreError::InvalidData(
                "VPN Gate reconcile report pool generations do not match the envelope".to_owned(),
            ));
        }
        if !outbound_ids.insert(pool.outbound_id.as_str()) {
            return Err(StoreError::InvalidData(
                "VPN Gate reconcile report contains duplicate pools".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_probe_report(report: &VpngateProbeReport) -> Result<()> {
    normalize_country_code(&report.country_code)?;
    let maximum = usize::try_from(MAX_CATALOG_PROBE_REPORT_SAMPLES)
        .expect("VPN Gate probe batch bound fits usize");
    if report.samples.len() > maximum {
        return Err(StoreError::InvalidData(format!(
            "VPN Gate catalogue report contains more than {maximum} samples"
        )));
    }
    let mut identities = BTreeSet::new();
    for sample in &report.samples {
        validate_sample(sample, true)?;
        if !identities.insert((&sample.server_id, &sample.profile_sha256)) {
            return Err(StoreError::InvalidData(
                "VPN Gate catalogue report contains duplicate candidates".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_sample(sample: &VpngateProbeSample, allow_connectivity_only: bool) -> Result<()> {
    if !valid_sha256(&sample.profile_sha256)
        || sample.server_id.trim().is_empty()
        || sample.server_id.chars().count() > 128
        || sample.probed_at_unix_secs <= 0
        || (sample.ip_scores.is_empty() != sample.ip_networks.is_empty())
        || (!sample.ip_scores.is_empty()
            && !valid_ip_intelligence(&sample.ip_scores, &sample.ip_networks))
        || sample
            .exit_country_code
            .as_deref()
            .is_some_and(|value| normalize_country_code(value).is_err())
    {
        return Err(StoreError::InvalidData(
            "VPN Gate sample identity, reputation, or timestamp is invalid".to_owned(),
        ));
    }
    match sample.status {
        VpngateProbeStatus::Succeeded => {
            if sample
                .exit_ip
                .as_deref()
                .is_none_or(|value| value.parse::<std::net::IpAddr>().is_err())
                || sample.connect_ms.is_none()
                || (!allow_connectivity_only && sample.download_bps.is_none())
                || sample.error_code.is_some()
            {
                return Err(StoreError::InvalidData(
                    "successful VPN Gate sample lacks route measurements".to_owned(),
                ));
            }
        }
        VpngateProbeStatus::Failed if sample.error_code.as_deref().is_none_or(str::is_empty) => {
            return Err(StoreError::InvalidData(
                "failed VPN Gate sample lacks an error code".to_owned(),
            ));
        }
        VpngateProbeStatus::Failed => {}
    }
    Ok(())
}

async fn lock_claim(
    tx: &mut Transaction<'_, Postgres>,
    worker_id: &str,
    claim: &VpngateSyncClaim,
) -> Result<VpngateSyncLeaseKind> {
    let generation = u64_to_i64("lease_generation", claim.lease_generation)?;
    let run_id = u64_to_i64("run_id", claim.run_id)?;
    let collector_matched = sqlx::query_scalar::<_, bool>(
        "SELECT COALESCE(
                    catalogue_lease_generation = $2 AND catalogue_active_run_id = $3,
                    FALSE
                )
           FROM vpngate_intelligence_nodes
          WHERE node_id = $1
          FOR UPDATE",
    )
    .bind(worker_id)
    .bind(generation)
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await?;
    if collector_matched == Some(true) {
        return Ok(VpngateSyncLeaseKind::Collector);
    }
    let global_matched = sqlx::query_scalar::<_, bool>(
        "SELECT COALESCE(
                    lease_owner = $1 AND lease_generation = $2 AND active_run_id = $3,
                    FALSE
                )
           FROM vpngate_catalog_state
          WHERE id = TRUE
          FOR UPDATE",
    )
    .bind(worker_id)
    .bind(generation)
    .bind(run_id)
    .fetch_one(&mut **tx)
    .await?;
    if global_matched {
        Ok(VpngateSyncLeaseKind::Global)
    } else {
        Err(StoreError::Conflict(
            "VPN Gate sync lease was replaced by a newer worker".to_owned(),
        ))
    }
}

/// Serializes publication across collectors without turning collection itself back into a global
/// lease. Each fetch runs concurrently; only the short transaction that merges snapshots waits.
async fn lock_catalog_publication(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(711001)")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn refresh_current_catalog_union(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    sqlx::query(
        "WITH latest_per_collector AS (
             SELECT DISTINCT ON (run.worker_id) run.id
               FROM vpngate_sync_runs run
               JOIN vpngate_intelligence_nodes selected
                 ON selected.node_id = run.worker_id
              WHERE run.status = 'succeeded'
              ORDER BY run.worker_id, run.finished_at DESC, run.id DESC
         ), visible AS (
             SELECT DISTINCT observation.server_id
               FROM vpngate_server_observations observation
               JOIN latest_per_collector latest ON latest.id = observation.sync_run_id
         ), desired AS (
             SELECT server.id, visible.server_id IS NOT NULL AS current
               FROM vpngate_servers server
               LEFT JOIN visible ON visible.server_id = server.id
         )
         UPDATE vpngate_servers server
            SET current = desired.current
           FROM desired
          WHERE server.id = desired.id
            AND server.current IS DISTINCT FROM desired.current
            AND EXISTS (SELECT 1 FROM vpngate_intelligence_nodes)",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_profile(
    tx: &mut Transaction<'_, Postgres>,
    run_id: i64,
    server: &VpngateServerInput,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO vpngate_profiles
            (sha256, server_id, remote_address, remote_port, transport, openvpn_config,
             first_seen_run_id, last_seen_run_id)
         VALUES ($1, $2, $3::inet, $4, $5, $6, $7, $7)
         ON CONFLICT (sha256) DO UPDATE SET
            last_seen_run_id = EXCLUDED.last_seen_run_id,
            last_seen_at = now()
         WHERE (vpngate_profiles.server_id, vpngate_profiles.remote_address,
                vpngate_profiles.remote_port, vpngate_profiles.transport,
                vpngate_profiles.openvpn_config)
               = (EXCLUDED.server_id, EXCLUDED.remote_address,
                  EXCLUDED.remote_port, EXCLUDED.transport, EXCLUDED.openvpn_config)",
    )
    .bind(&server.profile_sha256)
    .bind(&server.id)
    .bind(&server.remote_address)
    .bind(i32::from(server.remote_port))
    .bind(transport_name(server.transport))
    .bind(&server.openvpn_config)
    .bind(run_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_server(
    tx: &mut Transaction<'_, Postgres>,
    run_id: i64,
    server: &VpngateServerInput,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO vpngate_servers
            (id, hostname, ip, country_code, country_name, score, ping_ms, speed_bps,
             vpn_sessions, uptime_millis, total_users, total_traffic_bytes, log_type,
             operator_name, message, profile_sha256, current, first_seen_run_id, last_seen_run_id)
         VALUES ($1, $2, $3::inet, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                 $13, $14, $15, $16, TRUE, $17, $17)
         ON CONFLICT (id) DO UPDATE SET
            hostname = EXCLUDED.hostname, ip = EXCLUDED.ip,
            country_code = EXCLUDED.country_code, country_name = EXCLUDED.country_name,
            score = EXCLUDED.score, ping_ms = EXCLUDED.ping_ms,
            speed_bps = EXCLUDED.speed_bps, vpn_sessions = EXCLUDED.vpn_sessions,
            uptime_millis = EXCLUDED.uptime_millis, total_users = EXCLUDED.total_users,
            total_traffic_bytes = EXCLUDED.total_traffic_bytes, log_type = EXCLUDED.log_type,
            operator_name = EXCLUDED.operator_name, message = EXCLUDED.message,
            profile_sha256 = EXCLUDED.profile_sha256, current = TRUE,
            last_seen_run_id = EXCLUDED.last_seen_run_id, last_seen_at = now()",
    )
    .bind(&server.id)
    .bind(&server.hostname)
    .bind(&server.ip)
    .bind(&server.country_code)
    .bind(&server.country_name)
    .bind(u64_to_i64("score", server.score)?)
    .bind(optional_u32_to_i32("ping_ms", server.ping_ms)?)
    .bind(u64_to_i64("speed_bps", server.speed_bps)?)
    .bind(u32_to_i32("vpn_sessions", server.vpn_sessions)?)
    .bind(u64_to_i64("uptime_millis", server.uptime_millis)?)
    .bind(u64_to_i64("total_users", server.total_users)?)
    .bind(u64_to_i64(
        "total_traffic_bytes",
        server.total_traffic_bytes,
    )?)
    .bind(&server.log_type)
    .bind(&server.operator_name)
    .bind(&server.message)
    .bind(&server.profile_sha256)
    .bind(run_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_observation(
    tx: &mut Transaction<'_, Postgres>,
    run_id: i64,
    server: &VpngateServerInput,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO vpngate_server_observations
            (sync_run_id, server_id, hostname, ip, country_code, country_name, score,
             ping_ms, speed_bps, vpn_sessions, uptime_millis, total_users,
             total_traffic_bytes, log_type, operator_name, message, profile_sha256)
         VALUES ($1, $2, $3, $4::inet, $5, $6, $7, $8, $9, $10, $11, $12,
                 $13, $14, $15, $16, $17)",
    )
    .bind(run_id)
    .bind(&server.id)
    .bind(&server.hostname)
    .bind(&server.ip)
    .bind(&server.country_code)
    .bind(&server.country_name)
    .bind(u64_to_i64("score", server.score)?)
    .bind(optional_u32_to_i32("ping_ms", server.ping_ms)?)
    .bind(u64_to_i64("speed_bps", server.speed_bps)?)
    .bind(u32_to_i32("vpn_sessions", server.vpn_sessions)?)
    .bind(u64_to_i64("uptime_millis", server.uptime_millis)?)
    .bind(u64_to_i64("total_users", server.total_users)?)
    .bind(u64_to_i64(
        "total_traffic_bytes",
        server.total_traffic_bytes,
    )?)
    .bind(&server.log_type)
    .bind(&server.operator_name)
    .bind(&server.message)
    .bind(&server.profile_sha256)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn validate_batch(batch: &VpngateSyncBatch) -> Result<()> {
    if !valid_sha256(&batch.content_sha256) {
        return Err(StoreError::InvalidData(
            "VPN Gate response sha256 is invalid".to_owned(),
        ));
    }
    let accepted = u32::try_from(batch.servers.len())
        .map_err(|_| StoreError::InvalidData("too many VPN Gate rows".to_owned()))?;
    if accepted
        .checked_add(batch.rejected_rows)
        .is_none_or(|total| total != batch.fetched_rows)
    {
        return Err(StoreError::InvalidData(
            "VPN Gate accepted and rejected counts do not match fetched rows".to_owned(),
        ));
    }
    if batch.servers.is_empty() {
        return Err(StoreError::InvalidData(
            "VPN Gate sync refused an empty accepted catalogue".to_owned(),
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for server in &batch.servers {
        if !ids.insert(server.id.as_str()) {
            return Err(StoreError::InvalidData(format!(
                "VPN Gate batch contains duplicate server {}",
                server.id
            )));
        }
        validate_server(server)?;
    }
    Ok(())
}

fn validate_server(server: &VpngateServerInput) -> Result<()> {
    if server.id.trim().is_empty()
        || server.id.chars().count() > 128
        || server.hostname.trim().is_empty()
        || server.hostname.chars().count() > 255
    {
        return Err(StoreError::InvalidData(
            "VPN Gate server id or hostname is invalid".to_owned(),
        ));
    }
    normalize_country_code(&server.country_code)?;
    if server.ip.parse::<std::net::IpAddr>().is_err()
        || server.remote_address.parse::<std::net::IpAddr>().is_err()
        || server.remote_address != server.ip
    {
        return Err(StoreError::InvalidData(format!(
            "VPN Gate server {} profile remote does not match its catalogue IP",
            server.id
        )));
    }
    if !valid_sha256(&server.profile_sha256)
        || brocade_core::hash::sha256_hex(server.openvpn_config.as_bytes()) != server.profile_sha256
        || server.openvpn_config.is_empty()
        || server.openvpn_config.len() > 65_536
    {
        return Err(StoreError::InvalidData(format!(
            "VPN Gate server {} profile is invalid",
            server.id
        )));
    }
    Ok(())
}

fn normalize_country_code(value: &str) -> Result<String> {
    let value = value.trim().to_ascii_uppercase();
    if value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_uppercase()) {
        Ok(value)
    } else {
        Err(StoreError::InvalidData(
            "VPN Gate country code must be two ASCII letters".to_owned(),
        ))
    }
}

fn transport_name(value: VpngateTransport) -> &'static str {
    match value {
        VpngateTransport::Udp => "udp",
        VpngateTransport::Tcp => "tcp",
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn bounded(value: &str, max_chars: usize) -> String {
    value.trim().chars().take(max_chars).collect()
}

fn provider_name(provider: VpngateIpProvider) -> &'static str {
    match provider {
        VpngateIpProvider::Proxycheck => "proxycheck",
        VpngateIpProvider::Ffraud => "ffraud",
        VpngateIpProvider::Iplogs => "iplogs",
    }
}

fn decode_json<T: serde::de::DeserializeOwned>(field: &str, value: serde_json::Value) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|error| StoreError::InvalidData(format!("{field} contains invalid JSON: {error}")))
}

fn valid_ip_intelligence(scores: &[VpngateIpScore], networks: &[VpngateIpNetwork]) -> bool {
    let score_providers = scores
        .iter()
        .map(|score| score.provider)
        .collect::<BTreeSet<_>>();
    let network_providers = networks
        .iter()
        .map(|network| network.provider)
        .collect::<BTreeSet<_>>();
    (1..=3).contains(&scores.len())
        && scores.len() == score_providers.len()
        && scores.len() == networks.len()
        && score_providers == network_providers
        && scores.iter().all(|score| {
            score.score <= 100
                && score.country_code.len() == 2
                && score
                    .country_code
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase())
        })
        && networks.iter().all(|network| {
            network
                .isp
                .as_deref()
                .is_none_or(|isp| !isp.trim().is_empty() && isp.chars().count() <= 160)
        })
}

fn require_system_admin(actor: &AdminContext, action: &str) -> Result<()> {
    if actor.is_system_admin() {
        Ok(())
    } else {
        Err(StoreError::Forbidden(format!(
            "only system-admin can {action}"
        )))
    }
}

fn u64_to_i64(field: &str, value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{field} is outside PostgreSQL BIGINT")))
}

fn i64_to_u64(field: &str, value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{field} is negative: {value}")))
}

fn optional_i64_to_u64(field: &str, value: Option<i64>) -> Result<Option<u64>> {
    value.map(|value| i64_to_u64(field, value)).transpose()
}

fn u32_to_i32(field: &str, value: u32) -> Result<i32> {
    i32::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{field} is outside PostgreSQL INTEGER")))
}

fn optional_u32_to_i32(field: &str, value: Option<u32>) -> Result<Option<i32>> {
    value.map(|value| u32_to_i32(field, value)).transpose()
}

fn i32_to_u32(field: &str, value: i32) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{field} is negative: {value}")))
}

fn optional_i32_to_u32(field: &str, value: Option<i32>) -> Result<Option<u32>> {
    value.map(|value| i32_to_u32(field, value)).transpose()
}

impl crate::PgStore {
    pub async fn vpngate_overview(&self) -> Result<VpngateOverview> {
        overview(self.pool()).await
    }

    pub async fn vpngate_country_servers(
        &self,
        country_code: &str,
    ) -> Result<Vec<VpngateServerView>> {
        country_servers(self.pool(), country_code).await
    }

    pub async fn vpngate_country_server_page(
        &self,
        country_code: &str,
        request: VpngateServerPageRequest,
    ) -> Result<VpngateServerPage> {
        country_server_page(self.pool(), country_code, request).await
    }

    pub async fn vpngate_runtime_views(
        &self,
        actor: &AdminContext,
    ) -> Result<Vec<VpngateRuntimeView>> {
        runtime_views(self.pool(), actor, &[]).await
    }

    pub async fn vpngate_runtime_views_observed(
        &self,
        actor: &AdminContext,
        observed: &[VpngateRuntimeSelection],
    ) -> Result<Vec<VpngateRuntimeView>> {
        runtime_views(self.pool(), actor, observed).await
    }

    pub async fn request_vpngate_pool_switch(
        &self,
        actor: &AdminContext,
        node_id: &str,
        outbound_id: &str,
        request: RequestVpngatePoolSwitch,
        observed: Option<&VpngateRuntimeSelection>,
    ) -> Result<VpngatePoolSwitchRequestView> {
        request_pool_switch(self.pool(), actor, node_id, outbound_id, request, observed).await
    }

    pub async fn update_vpngate_catalog_settings(
        &self,
        actor: &AdminContext,
        request: UpdateVpngateCatalogSettings,
    ) -> Result<VpngateCatalogStatus> {
        update_settings(self.pool(), actor, request).await
    }

    pub async fn update_vpngate_probe_settings(
        &self,
        actor: &AdminContext,
        request: UpdateVpngateProbeSettings,
    ) -> Result<VpngateCatalogStatus> {
        update_probe_settings(self.pool(), actor, request).await
    }

    pub async fn update_vpngate_admission_policy(
        &self,
        actor: &AdminContext,
        policy: VpngateAdmissionPolicy,
    ) -> Result<VpngateAdmissionPolicy> {
        update_admission_policy(self.pool(), actor, policy).await
    }

    pub async fn update_vpngate_intelligence_policy(
        &self,
        actor: &AdminContext,
        policy: VpngateIntelligencePolicy,
    ) -> Result<VpngateIntelligencePolicy> {
        update_intelligence_policy(self.pool(), actor, policy).await
    }

    pub async fn update_vpngate_intelligence_credentials(
        &self,
        actor: &AdminContext,
        request: UpdateVpngateIntelligenceCredentials,
    ) -> Result<VpngateIntelligenceCredentials> {
        update_intelligence_credentials(self.pool(), actor, request).await
    }

    pub async fn request_vpngate_intelligence_refresh(
        &self,
        actor: &AdminContext,
    ) -> Result<VpngateIntelligenceRefreshResult> {
        request_intelligence_refresh(self.pool(), actor).await
    }

    pub async fn request_vpngate_sync(&self, actor: &AdminContext) -> Result<()> {
        request_sync(self.pool(), actor).await
    }

    pub async fn prune_vpngate_history(
        &self,
        candidate_retain_days: u32,
        observation_retain_days: u32,
    ) -> Result<u64> {
        prune_history(self.pool(), candidate_retain_days, observation_retain_days).await
    }

    pub async fn update_vpngate_probe_node(
        &self,
        actor: &AdminContext,
        node_id: &str,
        request: UpdateVpngateProbeNode,
    ) -> Result<VpngateProbeNodeSelection> {
        update_probe_node(self.pool(), actor, node_id, request).await
    }

    pub async fn update_vpngate_intelligence_node(
        &self,
        actor: &AdminContext,
        node_id: &str,
        request: UpdateVpngateIntelligenceNode,
    ) -> Result<VpngateIntelligenceNodeSelection> {
        update_intelligence_node(self.pool(), actor, node_id, request).await
    }

    pub async fn claim_vpngate_sync(
        &self,
        worker_id: &str,
        trigger: &str,
    ) -> Result<Option<VpngateSyncClaim>> {
        claim_sync(self.pool(), worker_id, trigger).await
    }

    pub async fn complete_vpngate_sync(
        &self,
        worker_id: &str,
        claim: &VpngateSyncClaim,
        batch: VpngateSyncBatch,
    ) -> Result<()> {
        complete_sync(self.pool(), worker_id, claim, batch).await
    }

    pub async fn fail_vpngate_sync(
        &self,
        worker_id: &str,
        claim: &VpngateSyncClaim,
        error_code: &str,
        error_detail: &str,
    ) -> Result<()> {
        fail_sync(self.pool(), worker_id, claim, error_code, error_detail).await
    }

    pub async fn claim_vpngate_exit_intelligence(
        &self,
        node_id: &str,
    ) -> Result<Option<VpngateIpIntelligenceClaim>> {
        claim_exit_intelligence(self.pool(), node_id).await
    }

    pub async fn claim_vpngate_catalog_sync(
        &self,
        node_id: &str,
    ) -> Result<Option<VpngateSyncClaim>> {
        claim_agent_catalog_sync(self.pool(), node_id).await
    }

    pub async fn record_vpngate_ip_intelligence_report(
        &self,
        node_id: &str,
        report: &VpngateIpIntelligenceReport,
    ) -> Result<()> {
        record_ip_intelligence_report(self.pool(), node_id, report).await
    }

    pub async fn vpngate_agent_desired(
        &self,
        node_id: &str,
    ) -> Result<Option<VpngateDesiredState>> {
        agent_desired(self.pool(), node_id).await
    }

    pub async fn vpngate_usable_probe_node_addresses(
        &self,
    ) -> Result<Vec<VpngateProbeNodeAddress>> {
        usable_probe_node_addresses(self.pool()).await
    }

    pub async fn vpngate_agent_desired_with_probe_origins(
        &self,
        node_id: &str,
        origins: &[VpngateProbeNodeOrigin],
    ) -> Result<Option<VpngateDesiredState>> {
        agent_desired_with_probe_origins(self.pool(), node_id, origins).await
    }

    pub async fn record_vpngate_agent_report(
        &self,
        node_id: &str,
        report: VpngatePoolReport,
    ) -> Result<VpngateReportReceipt> {
        record_agent_report(self.pool(), node_id, report).await
    }

    pub async fn record_vpngate_reconcile_report(
        &self,
        node_id: &str,
        report: VpngateReconcileReport,
    ) -> Result<VpngateReportReceipt> {
        record_reconcile_report(self.pool(), node_id, report).await
    }

    pub async fn record_vpngate_probe_report(
        &self,
        node_id: &str,
        report: VpngateProbeReport,
    ) -> Result<VpngateReportReceipt> {
        record_probe_report(self.pool(), node_id, report).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxycheck_key_pool_maps_entropy_to_its_enumeration_start() {
        assert_eq!(proxycheck_start_index(0_u64.to_le_bytes(), 3), 0);
        assert_eq!(proxycheck_start_index(1_u64.to_le_bytes(), 3), 1);
        assert_eq!(proxycheck_start_index(5_u64.to_le_bytes(), 3), 2);
        assert_eq!(proxycheck_start_index(u64::MAX.to_le_bytes(), 1), 0);
    }

    fn vpngate_protocol(
        country_code: &str,
        server_id: Option<&str>,
        server_ids: &[&str],
    ) -> ExternalOutboundProtocol {
        ExternalOutboundProtocol::Vpngate {
            country_code: country_code.to_owned(),
            server_id: server_id.map(str::to_owned),
            server_ids: server_ids.iter().map(|id| (*id).to_owned()).collect(),
            max_connect_ms: 15_000,
            min_download_bps: 1_000_000,
            max_candidates: 16,
        }
    }

    #[test]
    fn candidate_queries_cover_only_distinct_automatic_pool_countries() {
        let automatic_jp = vpngate_protocol("JP", None, &[]);
        let duplicate_jp = vpngate_protocol("JP", None, &[]);
        let pinned_kr = vpngate_protocol("KR", Some("vpn-kr"), &[]);
        let manual_us = vpngate_protocol("US", None, &["vpn-us"]);
        let unknown = vpngate_protocol("ZZ", None, &[]);
        let unrelated = ExternalOutboundProtocol::Anytls {
            credential: "secret".to_owned(),
        };

        assert_eq!(
            automatic_pool_countries([
                &automatic_jp,
                &duplicate_jp,
                &pinned_kr,
                &manual_us,
                &unknown,
                &unrelated,
            ]),
            BTreeSet::from(["JP".to_owned()])
        );
        assert!(automatic_pool_countries(std::iter::empty()).is_empty());
    }

    #[test]
    fn batch_counts_and_profile_identity_are_checked_before_a_transaction() {
        let openvpn_config = "client\nremote 192.0.2.10 1194\n".to_owned();
        let profile_sha256 = brocade_core::hash::sha256_hex(openvpn_config.as_bytes());
        let mut batch = VpngateSyncBatch {
            content_sha256: "a".repeat(64),
            fetched_rows: 1,
            rejected_rows: 0,
            servers: vec![VpngateServerInput {
                id: "vpn1".to_owned(),
                hostname: "public-vpn-1".to_owned(),
                ip: "192.0.2.10".to_owned(),
                country_code: "JP".to_owned(),
                country_name: "Japan".to_owned(),
                score: 10,
                ping_ms: Some(20),
                speed_bps: 1_000_000,
                vpn_sessions: 1,
                uptime_millis: 1,
                total_users: 1,
                total_traffic_bytes: 1,
                log_type: "2weeks".to_owned(),
                operator_name: "volunteer".to_owned(),
                message: String::new(),
                profile_sha256,
                remote_address: "192.0.2.10".to_owned(),
                remote_port: 1194,
                transport: VpngateTransport::Udp,
                openvpn_config,
            }],
        };
        assert!(validate_batch(&batch).is_ok());
        batch.servers[0].remote_address = "192.0.2.11".to_owned();
        assert!(validate_batch(&batch).is_err());
        batch.servers[0].remote_address = "192.0.2.10".to_owned();
        batch.rejected_rows = 1;
        assert!(validate_batch(&batch).is_err());
    }

    #[test]
    fn bounded_error_text_is_unicode_safe() {
        assert_eq!(bounded("  节点连接失败  ", 4), "节点连接");
    }

    #[test]
    fn catalogue_probe_worker_limit_is_independent_from_pool_candidates() {
        assert!(valid_catalog_probe_workers(1));
        assert!(valid_catalog_probe_workers(128));
        assert!(!valid_catalog_probe_workers(0));
        assert!(!valid_catalog_probe_workers(129));
    }

    #[test]
    fn catalogue_probe_regions_share_one_scope_then_use_nearest_fallback() {
        let origins = vec![
            VpngateProbeNodeOrigin {
                node_id: "jp-a".to_owned(),
                country_code: "JP".to_owned(),
            },
            VpngateProbeNodeOrigin {
                node_id: "jp-b".to_owned(),
                country_code: "JP".to_owned(),
            },
            VpngateProbeNodeOrigin {
                node_id: "us-a".to_owned(),
                country_code: "US".to_owned(),
            },
        ];
        let countries = ["JP", "KR", "US", "CA", "GB", "ZA", "AU"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();

        let japan_a = eligible_probe_countries("jp-a", &countries, &origins);
        let japan_b = eligible_probe_countries("jp-b", &countries, &origins);
        let america = eligible_probe_countries("us-a", &countries, &origins);

        assert_eq!(japan_a, japan_b);
        assert!(japan_a.iter().any(|country| country == "JP"));
        assert!(japan_a.iter().any(|country| country == "KR"));
        assert!(japan_a.iter().any(|country| country == "ZA"));
        assert!(japan_a.iter().any(|country| country == "AU"));
        assert!(!japan_a.iter().any(|country| country == "GB"));
        assert!(!america.iter().any(|country| country == "JP"));
        assert!(america.iter().any(|country| country == "US"));
        assert!(america.iter().any(|country| country == "CA"));
        assert!(america.iter().any(|country| country == "GB"));
        assert!(!america.iter().any(|country| country == "ZA"));
    }

    #[test]
    fn catalogue_probe_regions_fall_back_to_the_complete_queue_without_geoip() {
        let countries = vec!["JP".to_owned(), "US".to_owned()];
        let origins = [
            VpngateProbeNodeOrigin {
                node_id: "edge-a".to_owned(),
                country_code: "ZZ".to_owned(),
            },
            VpngateProbeNodeOrigin {
                node_id: "edge-b".to_owned(),
                country_code: "ZZ".to_owned(),
            },
        ];
        let scopes = eligible_probe_countries("edge-a", &countries, &origins);
        assert_eq!(scopes, vec!["JP".to_owned(), "US".to_owned()]);
        assert_eq!(
            probe_shard_peers("edge-a", &origins),
            vec!["edge-a".to_owned(), "edge-b".to_owned()]
        );
    }

    #[test]
    fn catalogue_probe_shards_only_within_the_same_origin_region() {
        let origins = [
            VpngateProbeNodeOrigin {
                node_id: "jp-b".to_owned(),
                country_code: "JP".to_owned(),
            },
            VpngateProbeNodeOrigin {
                node_id: "us-a".to_owned(),
                country_code: "US".to_owned(),
            },
            VpngateProbeNodeOrigin {
                node_id: "jp-a".to_owned(),
                country_code: "JP".to_owned(),
            },
        ];
        assert_eq!(
            probe_shard_peers("jp-a", &origins),
            vec!["jp-a".to_owned(), "jp-b".to_owned()]
        );
        assert_eq!(probe_shard_peers("us-a", &origins), vec!["us-a".to_owned()]);
    }

    fn candidate_with_risk(
        server_id: &str,
        scores: &[(VpngateIpProvider, u8)],
    ) -> VpngateCandidate {
        VpngateCandidate {
            server_id: server_id.to_owned(),
            hostname: format!("{server_id}.example"),
            country_code: "JP".to_owned(),
            remote_address: "192.0.2.1".to_owned(),
            remote_port: 1194,
            transport: VpngateTransport::Udp,
            profile_sha256: "a".repeat(64),
            openvpn_config: "client".to_owned(),
            probe_mode: VpngateProbeMode::Performance,
            last_observed_exit_ip: Some("198.51.100.1".to_owned()),
            verified_exit_ip: Some("198.51.100.1".to_owned()),
            verified_exit_country_code: Some("JP".to_owned()),
            verified_ip_scores: scores
                .iter()
                .map(|(provider, score)| VpngateIpScore {
                    provider: *provider,
                    score: *score,
                    country_code: "JP".to_owned(),
                })
                .collect(),
            verified_ip_networks: Vec::new(),
        }
    }

    fn candidate_evaluation(candidate: VpngateCandidate) -> VpngateCandidateEvaluation {
        VpngateCandidateEvaluation {
            server_id: candidate.server_id,
            country_code: candidate.country_code,
            profile_sha256: candidate.profile_sha256,
            verified_exit_ip: candidate.verified_exit_ip,
            verified_exit_country_code: candidate.verified_exit_country_code,
            verified_ip_scores: candidate.verified_ip_scores,
            verified_ip_networks: candidate.verified_ip_networks,
        }
    }

    fn ranked_ids(candidates: Vec<VpngateCandidate>) -> Vec<String> {
        let count = candidates.len();
        let candidates = candidates
            .into_iter()
            .enumerate()
            .map(|(index, candidate)| VpngateQualifiedCandidate {
                candidate: candidate_evaluation(candidate),
                // Unit-test input order represents the fleet-quality order established by SQL.
                global_download_bps: u64::try_from(count - index).unwrap() * 1_000_000,
                global_connect_ms: u32::try_from(index + 1).unwrap() * 100,
            })
            .collect();
        rank_candidates_by_risk(candidates)
            .into_iter()
            .map(|candidate| candidate.candidate.server_id)
            .collect()
    }

    #[test]
    fn candidate_isp_exclusions_use_provider_evidence_not_hostname() {
        use brocade_deployment::protocol::VpngateNetworkType;

        let mut candidate = candidate_with_risk("ordinary", &[]);
        candidate.hostname = "OPTAGE.example".to_owned();
        assert!(!candidate_networks_have_excluded_isp(
            &candidate.verified_ip_networks
        ));

        for (provider, isp) in [
            (VpngateIpProvider::Proxycheck, " OPTAGE Inc. "),
            (
                VpngateIpProvider::Ffraud,
                "chubu telecommunications company, inc.",
            ),
        ] {
            candidate.verified_ip_networks = vec![VpngateIpNetwork {
                provider,
                isp: Some(isp.to_owned()),
                network_type: VpngateNetworkType::Business,
            }];
            assert!(
                candidate_networks_have_excluded_isp(&candidate.verified_ip_networks),
                "{isp}"
            );
        }
        candidate.verified_ip_networks[0].isp = Some("Example ISP".to_owned());
        assert!(!candidate_networks_have_excluded_isp(
            &candidate.verified_ip_networks
        ));
    }

    #[test]
    fn automatic_candidates_use_pareto_layers_then_global_quality() {
        use VpngateIpProvider::{Ffraud, Iplogs, Proxycheck};

        let candidates = vec![
            candidate_with_risk("incomplete-low", &[(Proxycheck, 1), (Ffraud, 1)]),
            candidate_with_risk(
                "dominated-fast",
                &[(Proxycheck, 30), (Ffraud, 50), (Iplogs, 30)],
            ),
            candidate_with_risk("balanced", &[(Proxycheck, 20), (Ffraud, 20), (Iplogs, 20)]),
            candidate_with_risk("tradeoff", &[(Proxycheck, 10), (Ffraud, 60), (Iplogs, 10)]),
        ];

        assert_eq!(
            ranked_ids(candidates),
            ["incomplete-low", "balanced", "tradeoff", "dominated-fast"]
        );
    }

    #[test]
    fn provider_and_probe_counts_do_not_rank_candidates() {
        use VpngateIpProvider::{Ffraud, Iplogs, Proxycheck};

        let candidates = vec![
            VpngateQualifiedCandidate {
                candidate: candidate_evaluation(candidate_with_risk(
                    "two-sources-fast",
                    &[(Proxycheck, 5), (Ffraud, 5)],
                )),
                global_download_bps: 80_000_000,
                global_connect_ms: 400,
            },
            VpngateQualifiedCandidate {
                candidate: candidate_evaluation(candidate_with_risk(
                    "three-sources-slow",
                    &[(Proxycheck, 5), (Ffraud, 5), (Iplogs, 5)],
                )),
                global_download_bps: 20_000_000,
                global_connect_ms: 800,
            },
        ];

        let ranked = rank_candidates_by_risk(candidates);
        assert_eq!(ranked[0].candidate.server_id, "two-sources-fast");
        assert_eq!(ranked[0].pareto_layer, 1);
        assert_eq!(ranked[1].pareto_layer, 1);
    }

    #[test]
    fn pareto_ranking_never_sums_scores_from_different_providers() {
        use VpngateIpProvider::{Ffraud, Iplogs, Proxycheck};

        // The tradeoff candidate has the lower arithmetic sum, but neither candidate dominates
        // the other. Their existing evidence/performance order must therefore remain unchanged.
        let candidates = vec![
            candidate_with_risk(
                "balanced-first",
                &[(Proxycheck, 40), (Ffraud, 40), (Iplogs, 40)],
            ),
            candidate_with_risk(
                "lower-sum-tradeoff",
                &[(Proxycheck, 0), (Ffraud, 90), (Iplogs, 0)],
            ),
        ];

        assert_eq!(
            ranked_ids(candidates),
            ["balanced-first", "lower-sum-tradeoff"]
        );
    }

    #[test]
    fn missing_evidence_sets_are_risk_incomparable() {
        use VpngateIpProvider::{Ffraud, Iplogs, Proxycheck};

        let candidates = vec![
            candidate_with_risk("proxycheck-iplogs", &[(Proxycheck, 20), (Iplogs, 20)]),
            candidate_with_risk("proxycheck-ffraud", &[(Proxycheck, 10), (Ffraud, 10)]),
        ];

        assert_eq!(
            ranked_ids(candidates),
            ["proxycheck-iplogs", "proxycheck-ffraud"]
        );
    }

    #[test]
    fn risk_ranking_happens_before_the_country_candidate_limit() {
        use VpngateIpProvider::{Ffraud, Iplogs, Proxycheck};

        let mut candidates = (0..MAX_ACTIVE_CANDIDATES_PER_COUNTRY)
            .map(|index| {
                candidate_with_risk(
                    &format!("faster-{index:02}"),
                    &[(Proxycheck, 70), (Ffraud, 70), (Iplogs, 70)],
                )
            })
            .collect::<Vec<_>>();
        candidates.push(candidate_with_risk(
            "safer-but-slower",
            &[(Proxycheck, 10), (Ffraud, 10), (Iplogs, 10)],
        ));

        let selected = ranked_ids(candidates)
            .into_iter()
            .take(
                usize::try_from(MAX_ACTIVE_CANDIDATES_PER_COUNTRY)
                    .expect("country candidate limit fits usize"),
            )
            .collect::<Vec<_>>();
        assert_eq!(selected.len(), 32);
        assert_eq!(selected[0], "safer-but-slower");
        assert!(!selected.iter().any(|id| id == "faster-31"));
    }

    #[test]
    fn catastrophic_catalogue_drop_is_rejected_but_normal_churn_is_allowed() {
        assert!(validate_catalog_replacement(99, 80).is_ok());
        assert!(validate_catalog_replacement(99, 33).is_ok());
        assert!(validate_catalog_replacement(99, 32).is_err());
        assert!(validate_catalog_replacement(0, 1).is_ok());
    }

    #[test]
    fn intelligence_policy_defaults_to_change_only_and_a_72_hour_active_window() {
        let policy = VpngateIntelligencePolicy::default();
        assert_eq!(
            policy.refresh_mode,
            VpngateIntelligenceRefreshMode::OnChange
        );
        assert_eq!(policy.active_window_hours, 72);
        assert_eq!(policy.stale_policy, VpngateStaleIntelligencePolicy::Retain);
        assert!(validate_intelligence_policy(&policy));

        let mut invalid = policy;
        invalid.active_window_hours = 0;
        assert!(!validate_intelligence_policy(&invalid));
    }

    #[test]
    fn catalogue_probe_accepts_connectivity_only_facts_and_rejects_duplicate_candidates() {
        let mut sample = VpngateProbeSample {
            server_id: "vpn1".to_owned(),
            profile_sha256: "a".repeat(64),
            status: VpngateProbeStatus::Succeeded,
            exit_ip: Some("198.51.100.20".to_owned()),
            exit_country_code: None,
            connect_ms: Some(800),
            download_bps: Some(20_000_000),
            ip_scores: Vec::new(),
            ip_networks: Vec::new(),
            error_code: None,
            error_detail: None,
            probed_at_unix_secs: 1,
        };
        let mut report = VpngateProbeReport {
            catalog_generation: 7,
            country_code: "JP".to_owned(),
            samples: vec![sample.clone()],
        };
        assert!(validate_probe_report(&report).is_ok());
        sample.download_bps = None;
        report.samples = vec![sample.clone()];
        assert!(validate_probe_report(&report).is_ok());
        assert!(validate_sample(&sample, false).is_err());
        report.samples.push(sample);
        assert!(validate_probe_report(&report).is_err());
    }

    #[test]
    fn catalogue_report_accepts_one_parallel_country_batch_and_rejects_more() {
        let sample = |index: usize| VpngateProbeSample {
            server_id: format!("vpn{index}"),
            profile_sha256: format!("{index:064x}"),
            status: VpngateProbeStatus::Failed,
            exit_ip: None,
            exit_country_code: None,
            connect_ms: None,
            download_bps: None,
            ip_scores: Vec::new(),
            ip_networks: Vec::new(),
            error_code: Some("catalogue-probe-failed".to_owned()),
            error_detail: None,
            probed_at_unix_secs: 1,
        };
        let mut report = VpngateProbeReport {
            catalog_generation: 7,
            country_code: "JP".to_owned(),
            samples: (0..128).map(sample).collect(),
        };
        assert!(validate_probe_report(&report).is_ok());
        report.samples.push(sample(128));
        assert!(validate_probe_report(&report).is_err());
    }

    #[test]
    fn exit_reputation_batch_is_canonical_sorted_and_keeps_latest_observation() {
        let observations = vec![
            ("2001:0db8::1".to_owned(), 10),
            ("8.8.8.8".to_owned(), 20),
            ("2001:db8::1".to_owned(), 30),
            ("1.1.1.1".to_owned(), 15),
        ];

        assert_eq!(
            deduplicate_exit_reputations(&observations).unwrap(),
            [
                ("1.1.1.1".to_owned(), 15),
                ("8.8.8.8".to_owned(), 20),
                ("2001:db8::1".to_owned(), 30),
            ]
        );
        assert!(deduplicate_exit_reputations(&[("not-an-ip".to_owned(), 1)]).is_err());
    }
}
