//! Short-lived on-demand TCP pings. Only Agents connect; Console never contacts the target.
use brocade_deployment::{
    forwarding_probe::{
        ForwardingProbeReport, ForwardingProbeTarget, TcpPingCode, TcpPingResult, TcpPingStatus,
    },
    protocol::FORWARDING_PROBE_PROTOCOL_VERSION,
};
use brocade_store::{
    AdminContext, ForwardingEntitlementView, ForwardingOverview, ForwardingRuleView, PgStore,
    StoreError,
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_JOBS: usize = 256;
const MAX_ACTIVE: usize = 32;
const MAX_USER_ACTIVE: usize = 4;
const TTL: Duration = Duration::from_secs(600);
const DEADLINE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Running,
    Complete,
    Obsolete,
    Expired,
}
impl State {
    fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub id: String,
    pub rule_id: i64,
    pub rule_updated_at: String,
    pub generation: i64,
    pub state: State,
    pub created_at: u64,
    pub result: Option<TcpPingResult>,
}

#[derive(Clone)]
struct Job {
    view: View,
    rule: ForwardingRuleView,
    entry: ForwardingEntitlementView,
    started: Instant,
    claimed: bool,
}

#[derive(Clone, Default)]
pub struct Service {
    jobs: Arc<Mutex<BTreeMap<String, Job>>>,
}

fn conflict(message: &str) -> StoreError {
    StoreError::Conflict(message.to_owned())
}

fn current<'a>(overview: &'a ForwardingOverview, job: &Job) -> Option<&'a ForwardingRuleView> {
    let rule = overview.rules.iter().find(|rule| rule.id == job.rule.id)?;
    let entry = overview
        .entitlements
        .iter()
        .find(|entry| entry.node_id == job.rule.node_id)?;
    (rule.updated_at == job.view.rule_updated_at
        && rule.node_id == job.rule.node_id
        && rule.target_host == job.rule.target_host
        && rule.target_port == job.rule.target_port
        && rule.enabled
        && entry.enabled
        && !entry.quota_suspended
        && !matches!(rule.runtime_status.as_str(), "service-paused" | "disabled")
        && entry.desired_generation == job.view.generation
        && entry.allow_private_targets == job.entry.allow_private_targets)
        .then_some(rule)
}

impl Service {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn enqueue(
        &self,
        overview: &ForwardingOverview,
        rule_id: i64,
        protocol: Option<i32>,
    ) -> Result<View, StoreError> {
        let rule = overview
            .rules
            .iter()
            .find(|rule| rule.id == rule_id)
            .ok_or_else(|| StoreError::NotFound("forwarding rule".to_owned()))?;
        let entry = overview
            .entitlements
            .iter()
            .find(|entry| entry.node_id == rule.node_id)
            .ok_or_else(|| conflict("转发入口授权不存在"))?;
        if !rule.enabled
            || !entry.enabled
            || entry.quota_suspended
            || matches!(rule.runtime_status.as_str(), "service-paused" | "disabled")
        {
            return Err(conflict(
                "转发已停用、额度暂停或不在有效期内，不能执行 TCP Ping",
            ));
        }
        let mut jobs = self.lock();
        jobs.retain(|_, job| job.started.elapsed() < TTL);
        for job in jobs.values_mut() {
            if job.view.state.active() && job.started.elapsed() > DEADLINE {
                job.view.state = State::Expired;
                job.view.result = None;
            }
        }
        if let Some(job) = jobs
            .values()
            .find(|job| job.rule.id == rule_id && current(overview, job).is_some())
        {
            if job.view.state.active() {
                return Ok(job.view.clone());
            }
            if job.started.elapsed() < Duration::from_secs(3) {
                return Err(conflict("请间隔 3 秒再执行 TCP Ping"));
            }
        }
        if jobs.values().filter(|job| job.view.state.active()).count() >= MAX_ACTIVE
            || jobs
                .values()
                .filter(|job| {
                    job.rule.tenant_id == rule.tenant_id
                        && job.rule.user_id == rule.user_id
                        && job.view.state.active()
                })
                .count()
                >= MAX_USER_ACTIVE
        {
            return Err(conflict("TCP Ping 请求较多，请稍后重试"));
        }
        if jobs.len() >= MAX_JOBS {
            if let Some(id) = jobs
                .values()
                .filter(|job| !job.view.state.active())
                .min_by_key(|job| job.started)
                .map(|job| job.view.id.clone())
            {
                jobs.remove(&id);
            } else {
                return Err(conflict("TCP Ping 请求较多，请稍后重试"));
            }
        }
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random)
            .map_err(|_| StoreError::Unavailable("TCP Ping ID 生成失败".to_owned()))?;
        let id = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut view = View {
            id,
            rule_id,
            rule_updated_at: rule.updated_at.clone(),
            generation: entry.desired_generation,
            state: State::Queued,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            result: None,
        };
        if protocol.is_none_or(|version| version < FORWARDING_PROBE_PROTOCOL_VERSION as i32) {
            view.state = State::Complete;
            view.result = Some(TcpPingResult::not_run(TcpPingCode::AgentUpgradeRequired));
        }
        jobs.retain(|_, job| job.rule.id != rule_id);
        jobs.insert(
            view.id.clone(),
            Job {
                view: view.clone(),
                rule: rule.clone(),
                entry: entry.clone(),
                started: Instant::now(),
                claimed: false,
            },
        );
        Ok(view)
    }

    pub fn list(&self, overview: &ForwardingOverview) -> Vec<View> {
        let mut jobs = self.lock();
        jobs.retain(|_, job| job.started.elapsed() < TTL);
        jobs.values_mut()
            .filter(|job| {
                job.rule.tenant_id == overview.tenant_id && job.rule.user_id == overview.user_id
            })
            .map(|job| {
                refresh(job, overview);
                job.view.clone()
            })
            .collect()
    }

    pub async fn claim(
        &self,
        store: &PgStore,
        node: &str,
    ) -> Result<Option<ForwardingProbeTarget>, StoreError> {
        let candidate = self
            .lock()
            .values()
            .filter(|job| job.rule.node_id == node && job.view.state.active() && !job.claimed)
            .min_by_key(|job| job.started)
            .cloned();
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        let overview = store
            .forwarding_overview(
                &AdminContext::system_admin("forwarding-tcp-ping"),
                &candidate.rule.tenant_id,
                &candidate.rule.user_id,
            )
            .await?;
        let mut jobs = self.lock();
        let Some(job) = jobs.get_mut(&candidate.view.id) else {
            return Ok(None);
        };
        refresh(job, &overview);
        if !job.view.state.active() || job.claimed {
            return Ok(None);
        }
        let Some(rule) = current(&overview, job) else {
            return Ok(None);
        };
        let target = ForwardingProbeTarget {
            probe_id: job.view.id.clone(),
            rule_id: rule.id,
            target_host: rule.target_host.clone(),
            target_port: rule.target_port,
            allow_private_targets: job.entry.allow_private_targets,
        };
        job.claimed = true;
        job.view.state = State::Running;
        Ok(Some(target))
    }

    pub async fn report(
        &self,
        store: &PgStore,
        node: &str,
        report: ForwardingProbeReport,
    ) -> Result<(), StoreError> {
        let candidate = self
            .lock()
            .get(&report.probe_id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound("TCP Ping expired".to_owned()))?;
        if candidate.rule.node_id != node || !candidate.claimed {
            return Err(StoreError::Forbidden(
                "TCP Ping belongs to another node or was not claimed".to_owned(),
            ));
        }
        if !valid_result(&report.result) {
            return Err(StoreError::InvalidData(
                "invalid TCP Ping result".to_owned(),
            ));
        }
        let overview = store
            .forwarding_overview(
                &AdminContext::system_admin("forwarding-tcp-ping"),
                &candidate.rule.tenant_id,
                &candidate.rule.user_id,
            )
            .await?;
        let mut jobs = self.lock();
        if let Some(job) = jobs.get_mut(&report.probe_id) {
            refresh(job, &overview);
            if job.view.state.active() {
                job.view.result = Some(report.result);
                job.view.state = State::Complete;
            }
        }
        Ok(())
    }
}

fn refresh(job: &mut Job, overview: &ForwardingOverview) {
    if current(overview, job).is_none() {
        job.view.state = State::Obsolete;
        job.view.result = None;
    } else if job.view.state.active() && job.started.elapsed() > DEADLINE {
        job.view.state = State::Expired;
        job.view.result = None;
    }
}

fn valid_result(result: &TcpPingResult) -> bool {
    let facts = match result.status {
        TcpPingStatus::Reachable => {
            result.code == TcpPingCode::Connected
                && result.connect_ms.is_some()
                && result.resolved_ip.is_some()
        }
        TcpPingStatus::Failed => {
            matches!(
                result.code,
                TcpPingCode::DnsFailed
                    | TcpPingCode::DnsBusy
                    | TcpPingCode::DnsEmpty
                    | TcpPingCode::ConnectFailed
                    | TcpPingCode::Timeout
            ) && result.connect_ms.is_none()
        }
        TcpPingStatus::NotRun => {
            matches!(
                result.code,
                TcpPingCode::InvalidTarget | TcpPingCode::AddressBlocked
            ) && result.connect_ms.is_none()
        }
    };
    facts
        && [result.dns_ms, result.connect_ms]
            .into_iter()
            .flatten()
            .all(|value| value <= 10_000)
        && result
            .resolved_ip
            .as_ref()
            .is_none_or(|ip| ip.parse::<std::net::IpAddr>().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn overview(user: &str, id: i64) -> ForwardingOverview {
        serde_json::from_value(json!({
            "tenant_id":"t", "user_id":user,
            "entitlements":[{"node_id":"n1","node_name":"入口","public_ipv4":null,"public_ipv6":null,
                "enabled":true,"tcp_allowed":true,"udp_allowed":true,"listen_port_start":30000,"listen_port_end":59999,
                "max_rules":100,"allow_private_targets":false,"rule_count":1,"desired_generation":3,"applied_generation":3,
                "reconcile_status":"converged","last_error":null,"cycle_limit_bytes":null,"cycle_used_bytes":0,
                "quota_suspended":false,"quota_job_id":null,"quota_job_status":null,"quota_job_error":null}],
            "rules":[{"id":id,"tenant_id":"t","user_id":user,"node_id":"n1","name":"rule","protocol":"tcp",
                "listen_port":30001,"target_host":"1.1.1.1","target_port":443,"enabled":true,"runtime_status":"active",
                "uplink_bytes":5,"downlink_bytes":10,"created_at":"one","updated_at":"one"}]
        })).unwrap()
    }

    #[test]
    fn only_explicit_enqueue_creates_one_idempotent_request() {
        let service = Service::default();
        let overview = overview("alice", 1);
        assert!(service.list(&overview).is_empty());
        let first = service.enqueue(&overview, 1, Some(36)).unwrap();
        let second = service.enqueue(&overview, 1, Some(36)).unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(service.lock().len(), 1);
        assert_eq!(first.state, State::Queued);
        assert!(first.result.is_none());
    }

    #[test]
    fn tcp_ping_does_not_wait_for_xray_listener_convergence() {
        let service = Service::default();
        let mut overview = overview("alice", 1);
        overview.rules[0].runtime_status = "pending".to_owned();
        assert_eq!(
            service.enqueue(&overview, 1, Some(36)).unwrap().state,
            State::Queued
        );
    }

    #[test]
    fn unknown_or_old_agent_is_not_reported_as_connection_failure() {
        for protocol in [None, Some(35)] {
            let service = Service::default();
            let result = service.enqueue(&overview("alice", 1), 1, protocol).unwrap();
            assert_eq!(result.state, State::Complete);
            assert_eq!(result.result.unwrap().status, TcpPingStatus::NotRun);
        }
    }

    #[test]
    fn editing_revocation_and_quota_invalidate_only_the_affected_users_result() {
        for change in 0..5 {
            let service = Service::default();
            let mut alice = overview("alice", 1);
            let bob = overview("bob", 2);
            let view = service.enqueue(&alice, 1, Some(36)).unwrap();
            service.enqueue(&bob, 2, Some(36)).unwrap();
            {
                let mut jobs = service.lock();
                let job = jobs.get_mut(&view.id).unwrap();
                job.view.state = State::Complete;
                job.view.result = Some(TcpPingResult {
                    status: TcpPingStatus::Reachable,
                    code: TcpPingCode::Connected,
                    dns_ms: None,
                    connect_ms: Some(12),
                    resolved_ip: Some("1.1.1.1".to_owned()),
                });
            }
            match change {
                0 => alice.rules[0].updated_at = "two".to_owned(),
                1 => alice.entitlements[0].enabled = false,
                2 => alice.entitlements[0].quota_suspended = true,
                3 => alice.rules[0].runtime_status = "service-paused".to_owned(),
                _ => alice.rules.clear(),
            }
            let alice_result = service.list(&alice);
            assert_eq!(alice_result[0].state, State::Obsolete);
            assert!(alice_result[0].result.is_none());
            let bob_result = service.list(&bob);
            assert_eq!(bob_result.len(), 1);
            assert_eq!(bob_result[0].state, State::Queued);
            assert_eq!(bob.rules[0].uplink_bytes, 5);
            assert_eq!(bob.rules[0].downlink_bytes, 10);
        }
    }

    #[test]
    fn service_paused_rules_cannot_enqueue_diagnostic_traffic() {
        let mut overview = overview("alice", 1);
        overview.rules[0].runtime_status = "service-paused".to_owned();
        let service = Service::default();
        assert!(matches!(
            service.enqueue(&overview, 1, Some(36)),
            Err(StoreError::Conflict(_))
        ));
        assert!(service.lock().is_empty());
    }

    #[test]
    fn requests_are_bounded_and_expire_without_automatic_retry() {
        let service = Service::default();
        let mut overview = overview("alice", 1);
        for id in 1..=4 {
            overview.rules[0].id = id;
            service.enqueue(&overview, id, Some(36)).unwrap();
        }
        overview.rules[0].id = 5;
        assert!(service.enqueue(&overview, 5, Some(36)).is_err());
        for job in service.lock().values_mut() {
            job.started = Instant::now() - DEADLINE - Duration::from_secs(1);
        }
        assert!(service.enqueue(&overview, 5, Some(36)).is_ok());
        assert_eq!(
            service
                .lock()
                .values()
                .filter(|job| job.view.state.active())
                .count(),
            1
        );
    }

    #[test]
    fn fabricated_timings_and_arbitrary_remote_text_are_rejected() {
        let mut result = TcpPingResult {
            status: TcpPingStatus::Reachable,
            code: TcpPingCode::Connected,
            dns_ms: Some(3),
            connect_ms: Some(12),
            resolved_ip: Some("1.1.1.1".to_owned()),
        };
        assert!(valid_result(&result));
        result.connect_ms = None;
        assert!(!valid_result(&result));
        result.connect_ms = Some(12);
        result.resolved_ip = Some("secret injected text".to_owned());
        assert!(!valid_result(&result));
    }
}
