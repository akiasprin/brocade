//! In-memory orchestration for operator-triggered user authorization probes.
//!
//! These jobs are deliberately not telemetry. They exist for one person pressing one button to
//! answer whether the exact credentials currently advertised by Serving can traverse the real
//! ingress and chain. A restart may forget the result; the periodic Agent E2E remains the durable
//! health signal.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use brocade_deployment::protocol::{E2eExitVerdict, E2eProbe, E2eProbeStatus};
use brocade_probe::{ProbeCancellation, ProbeOptions};
use brocade_store::{PgStore, StoreError, UserGrantProbePlan, UserGrantProbeTarget};
use serde::Serialize;
use tokio::sync::{broadcast, Semaphore};

const GLOBAL_CONCURRENCY: usize = 30;
const FINISHED_TTL: Duration = Duration::from_secs(10 * 60);

type RunProbe = dyn Fn(UserGrantProbeTarget, String, u64, ProbeOptions) -> E2eProbe + Send + Sync;

#[derive(Clone)]
pub(crate) struct GrantProbeService {
    inner: Arc<GrantProbeServiceInner>,
}

struct GrantProbeServiceInner {
    jobs: Mutex<HashMap<String, Arc<ProbeJob>>>,
    sequence: AtomicU64,
    semaphore: Arc<Semaphore>,
    xray_binary: PathBuf,
    runtime_dir: PathBuf,
    capability: ProbeCapability,
    run_probe: Arc<RunProbe>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProbeCapability {
    pub available: bool,
    pub version: Option<String>,
    pub reason: Option<String>,
    pub concurrency: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProbeJobSnapshot {
    pub id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub serving_revision: u64,
    pub serving_generation: u64,
    pub status: &'static str,
    pub message: Option<String>,
    pub created_at_unix_secs: u64,
    pub finished_at_unix_secs: Option<u64>,
    pub items: Vec<ProbeItemSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProbeItemSnapshot {
    pub id: String,
    pub name: String,
    pub app_id: String,
    pub app_name: String,
    pub chain_id: String,
    pub ingress_id: String,
    pub family: &'static str,
    pub protocol: &'static str,
    pub status: &'static str,
    pub ttfb_ms: Option<u32>,
    pub detail: Option<String>,
}

struct ProbeJob {
    cancellation: ProbeCancellation,
    snapshot: Mutex<ProbeJobSnapshot>,
    events: broadcast::Sender<ProbeJobSnapshot>,
}

impl GrantProbeService {
    pub(crate) fn from_env() -> Self {
        let binary = std::env::var("BROCADE_PROBE_XRAY_BIN")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/opt/brocade/libexec/xray"));
        let expected = std::env::var("BROCADE_XRAY_VERSION")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let runtime_dir = std::env::var("BROCADE_PROBE_RUNTIME_DIR")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if cfg!(debug_assertions) {
                    std::env::temp_dir().join("brocade-probes")
                } else {
                    PathBuf::from("/run/brocade/probes")
                }
            });
        let capability = inspect_runtime_dir(&runtime_dir)
            .err()
            .map(|reason| ProbeCapability {
                available: false,
                version: None,
                reason: Some(reason),
                concurrency: GLOBAL_CONCURRENCY,
            })
            .unwrap_or_else(|| inspect_xray(&binary, expected.as_deref()));
        Self::new(binary, runtime_dir, capability, Arc::new(default_run_probe))
    }

    fn new(
        binary: PathBuf,
        runtime_dir: PathBuf,
        capability: ProbeCapability,
        run_probe: Arc<RunProbe>,
    ) -> Self {
        Self {
            inner: Arc::new(GrantProbeServiceInner {
                jobs: Mutex::new(HashMap::new()),
                sequence: AtomicU64::new(0),
                semaphore: Arc::new(Semaphore::new(GLOBAL_CONCURRENCY)),
                xray_binary: binary,
                runtime_dir,
                capability,
                run_probe,
            }),
        }
    }

    pub(crate) fn capability(&self) -> ProbeCapability {
        self.inner.capability.clone()
    }

    pub(crate) async fn start_for_user(
        &self,
        store: PgStore,
        tenant_id: &str,
        user_id: &str,
        plan: UserGrantProbePlan,
        selected: &[String],
    ) -> Result<(ProbeJobSnapshot, bool), StoreError> {
        if !self.inner.capability.available {
            return Err(StoreError::Unavailable(
                self.inner
                    .capability
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Console 拨测组件不可用".to_owned()),
            ));
        }
        self.reap_finished();
        // Reject a second active round before consuming the plan. This is intentionally keyed by
        // the user, not the operator: two operators pressing at once should observe one test, not
        // double the user's measured traffic.
        if let Some(job) = self.find_active(tenant_id, user_id) {
            return Ok((snapshot(&job), true));
        }

        let selected_ids =
            validate_selection(selected, plan.items.iter().map(|item| item.id.as_str()))?;
        let targets = plan
            .items
            .iter()
            .filter(|item| selected_ids.is_empty() || selected_ids.contains(&item.id))
            .cloned()
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return Err(StoreError::InvalidData("没有可拨测的生效授权".to_owned()));
        }

        let now = unix_now();
        let id = format!(
            "p{:x}{:04x}",
            now,
            self.inner.sequence.fetch_add(1, Ordering::Relaxed) & 0xffff
        );
        let items = targets
            .iter()
            .map(|item| ProbeItemSnapshot {
                id: item.id.clone(),
                name: item.name.clone(),
                app_id: item.app_id.clone(),
                app_name: item.app_name.clone(),
                chain_id: item.chain_id.clone(),
                ingress_id: item.ingress_id.clone(),
                family: item.family,
                protocol: item.protocol,
                status: "waiting",
                ttfb_ms: None,
                detail: None,
            })
            .collect();
        let initial = ProbeJobSnapshot {
            id: id.clone(),
            tenant_id: tenant_id.to_owned(),
            user_id: user_id.to_owned(),
            serving_revision: plan.serving_revision,
            serving_generation: plan.serving_generation,
            status: "running",
            message: None,
            created_at_unix_secs: now,
            finished_at_unix_secs: None,
            items,
        };
        let (events, _) = broadcast::channel(32);
        let job = Arc::new(ProbeJob {
            cancellation: ProbeCancellation::default(),
            snapshot: Mutex::new(initial),
            events,
        });
        {
            let mut jobs = self.inner.jobs.lock().expect("grant probe jobs poisoned");
            if let Some(existing) = jobs
                .values()
                .find(|existing| {
                    let state = existing
                        .snapshot
                        .lock()
                        .expect("grant probe snapshot poisoned");
                    state.tenant_id == tenant_id
                        && state.user_id == user_id
                        && state.finished_at_unix_secs.is_none()
                })
                .cloned()
            {
                return Ok((snapshot(&existing), true));
            }
            jobs.insert(id, job.clone());
        }
        self.spawn_job(store, job.clone(), plan, targets);
        Ok((snapshot(&job), false))
    }

    fn spawn_job(
        &self,
        store: PgStore,
        job: Arc<ProbeJob>,
        plan: UserGrantProbePlan,
        targets: Vec<UserGrantProbeTarget>,
    ) {
        let service = self.clone();
        tokio::spawn(async move {
            let mut joins = tokio::task::JoinSet::new();
            let timeout_secs = plan.timeout_secs;
            let serving_generation = plan.serving_generation;
            for target in targets {
                let service = service.clone();
                let store = store.clone();
                let job = job.clone();
                let endpoint = plan.endpoint_url.clone();
                joins.spawn(async move {
                    if job.cancellation.is_cancelled() {
                        service.finish_item(&job, &target.id, "canceled", None, None);
                        return;
                    }
                    let permit = match service.inner.semaphore.clone().acquire_owned().await {
                        Ok(permit) => permit,
                        Err(_) => {
                            service.finish_item(
                                &job,
                                &target.id,
                                "failed",
                                None,
                                Some("拨测调度器已关闭".to_owned()),
                            );
                            return;
                        }
                    };
                    if job.cancellation.is_cancelled() {
                        drop(permit);
                        service.finish_item(&job, &target.id, "canceled", None, None);
                        return;
                    }
                    service.set_item_running(&job, &target.id);
                    let options = ProbeOptions::new(
                        service.inner.xray_binary.clone(),
                        job.cancellation.clone(),
                    )
                    .with_runtime_dir(service.inner.runtime_dir.clone());
                    let runner = service.inner.run_probe.clone();
                    let target_for_runner = target.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        runner(target_for_runner, endpoint, timeout_secs, options)
                    })
                    .await;
                    drop(permit);
                    if job.cancellation.is_cancelled() {
                        service.finish_item(&job, &target.id, "canceled", None, None);
                        return;
                    }
                    match store
                        .user_grant_probe_generation_matches(serving_generation)
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) | Err(StoreError::Unavailable(_)) => {
                            service.supersede(&job);
                            return;
                        }
                        Err(error) => {
                            eprintln!("grant probe serving generation check failed: {error}");
                            service.finish_item(
                                &job,
                                &target.id,
                                "failed",
                                None,
                                Some("暂时无法确认 Serving 状态".to_owned()),
                            );
                            return;
                        }
                    }
                    match result {
                        Ok(result) => service.record_result(&job, &target, result),
                        Err(_) => service.finish_item(
                            &job,
                            &target.id,
                            "failed",
                            None,
                            Some("拨测执行线程异常结束".to_owned()),
                        ),
                    }
                });
            }
            while joins.join_next().await.is_some() {}
            service.finish_job(&job);
        });
    }

    fn get(&self, id: &str) -> Option<Arc<ProbeJob>> {
        self.inner
            .jobs
            .lock()
            .expect("grant probe jobs poisoned")
            .get(id)
            .cloned()
    }

    pub(crate) fn snapshot_for(&self, id: &str) -> Option<ProbeJobSnapshot> {
        self.get(id).map(|job| snapshot(&job))
    }

    pub(crate) fn subscribe(
        &self,
        id: &str,
    ) -> Option<(ProbeJobSnapshot, broadcast::Receiver<ProbeJobSnapshot>)> {
        self.get(id)
            .map(|job| (snapshot(&job), job.events.subscribe()))
    }

    pub(crate) fn cancel(&self, id: &str) -> Option<ProbeJobSnapshot> {
        let job = self.get(id)?;
        job.cancellation.cancel();
        let mut state = job.snapshot.lock().expect("grant probe snapshot poisoned");
        if state.finished_at_unix_secs.is_none() {
            state.status = "canceled";
            state.message = Some("已取消本轮拨测".to_owned());
            state.finished_at_unix_secs = Some(unix_now());
            for item in &mut state.items {
                if matches!(item.status, "waiting" | "running") {
                    item.status = "canceled";
                    item.detail = None;
                }
            }
        }
        let out = state.clone();
        drop(state);
        let _ = job.events.send(out.clone());
        Some(out)
    }

    fn find_active(&self, tenant: &str, user: &str) -> Option<Arc<ProbeJob>> {
        self.inner
            .jobs
            .lock()
            .expect("grant probe jobs poisoned")
            .values()
            .find(|job| {
                let state = job.snapshot.lock().expect("grant probe snapshot poisoned");
                state.tenant_id == tenant
                    && state.user_id == user
                    && state.finished_at_unix_secs.is_none()
            })
            .cloned()
    }

    fn set_item_running(&self, job: &ProbeJob, id: &str) {
        self.update(job, |state| {
            if let Some(item) = state.items.iter_mut().find(|item| item.id == id) {
                item.status = "running";
                item.detail = None;
            }
        });
    }

    fn finish_item(
        &self,
        job: &ProbeJob,
        id: &str,
        status: &'static str,
        ttfb_ms: Option<u32>,
        detail: Option<String>,
    ) {
        self.update(job, |state| {
            if let Some(item) = state.items.iter_mut().find(|item| item.id == id) {
                item.status = status;
                item.ttfb_ms = ttfb_ms;
                item.detail = detail;
            }
        });
    }

    fn record_result(&self, job: &ProbeJob, target: &UserGrantProbeTarget, result: E2eProbe) {
        let passed =
            result.status == E2eProbeStatus::Ok && result.exit_verdict != E2eExitVerdict::Mismatch;
        let detail = safe_result_detail(&result);
        self.finish_item(
            job,
            &target.id,
            if passed { "passed" } else { "failed" },
            result.ttfb_ms,
            detail,
        );
    }

    fn supersede(&self, job: &ProbeJob) {
        job.cancellation.cancel();
        self.update(job, |state| {
            state.status = "superseded";
            state.message = Some("Serving 状态已变化，本轮结果已作废，请重新拨测".to_owned());
            state.finished_at_unix_secs = Some(unix_now());
            for item in &mut state.items {
                if matches!(item.status, "waiting" | "running") {
                    item.status = "canceled";
                    item.detail = None;
                }
            }
        });
    }

    fn finish_job(&self, job: &ProbeJob) {
        self.update(job, |state| {
            if state.finished_at_unix_secs.is_some() {
                return;
            }
            state.status = "completed";
            state.finished_at_unix_secs = Some(unix_now());
            let failed = state
                .items
                .iter()
                .filter(|item| item.status == "failed")
                .count();
            state.message = Some(if failed == 0 {
                "网络拨测全部通过".to_owned()
            } else {
                format!("{failed} 项网络拨测失败")
            });
        });
    }

    fn update(&self, job: &ProbeJob, mutate: impl FnOnce(&mut ProbeJobSnapshot)) {
        let mut state = job.snapshot.lock().expect("grant probe snapshot poisoned");
        mutate(&mut state);
        let out = state.clone();
        drop(state);
        let _ = job.events.send(out);
    }

    fn reap_finished(&self) {
        let now = unix_now();
        self.inner
            .jobs
            .lock()
            .expect("grant probe jobs poisoned")
            .retain(|_, job| {
                snapshot(job)
                    .finished_at_unix_secs
                    .is_none_or(|finished| now.saturating_sub(finished) < FINISHED_TTL.as_secs())
            });
    }
}

fn default_run_probe(
    target: UserGrantProbeTarget,
    endpoint: String,
    timeout: u64,
    options: ProbeOptions,
) -> E2eProbe {
    brocade_probe::probe_one_with_options(&target.target, &endpoint, timeout, &options)
}

fn snapshot(job: &ProbeJob) -> ProbeJobSnapshot {
    job.snapshot
        .lock()
        .expect("grant probe snapshot poisoned")
        .clone()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn validate_selection<'a>(
    selected: &[String],
    all: impl IntoIterator<Item = &'a str>,
) -> Result<HashSet<String>, StoreError> {
    let selected_ids = selected.iter().cloned().collect::<HashSet<_>>();
    if selected_ids.len() != selected.len() {
        return Err(StoreError::InvalidData("拨测项目不能重复".to_owned()));
    }
    let all_ids = all.into_iter().collect::<HashSet<_>>();
    if !selected_ids.is_empty()
        && selected_ids
            .iter()
            .any(|selected| !all_ids.contains(selected.as_str()))
    {
        return Err(StoreError::Conflict(
            "拨测项目已随 Serving 变化，请刷新用户页面后重试".to_owned(),
        ));
    }
    Ok(selected_ids)
}

fn inspect_xray(path: &Path, expected: Option<&str>) -> ProbeCapability {
    let output = match std::process::Command::new(path).arg("version").output() {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            return ProbeCapability {
                available: false,
                version: None,
                reason: Some(format!(
                    "Console 拨测 Xray 无法运行（退出状态 {}）",
                    output.status
                )),
                concurrency: GLOBAL_CONCURRENCY,
            }
        }
        Err(error) => {
            eprintln!("grant probe xray inspection failed: {error}");
            return ProbeCapability {
                available: false,
                version: None,
                reason: Some("Console 未安装或无法执行拨测 Xray".to_owned()),
                concurrency: GLOBAL_CONCURRENCY,
            };
        }
    };
    let first = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    let actual = first.split_whitespace().nth(1).map(str::to_owned);
    if let (Some(expected), Some(actual)) = (expected, actual.as_deref()) {
        if expected.trim_start_matches('v') != actual.trim_start_matches('v') {
            return ProbeCapability {
                available: false,
                version: Some(actual.to_owned()),
                reason: Some(format!(
                    "Console 拨测 Xray 版本为 {actual}，要求 {expected}"
                )),
                concurrency: GLOBAL_CONCURRENCY,
            };
        }
    }
    ProbeCapability {
        available: true,
        version: actual,
        reason: None,
        concurrency: GLOBAL_CONCURRENCY,
    }
}

fn inspect_runtime_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(path).map_err(|_| "Console 无法创建私有拨测运行目录".to_owned())?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "Console 无法检查私有拨测运行目录".to_owned())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("Console 拨测运行路径不是私有目录".to_owned());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| "Console 无法收紧拨测运行目录权限".to_owned())?;
    Ok(())
}

/// Never send Xray's raw log to the browser. It may contain an address or account material, and
/// exposing it turns a diagnostic convenience into a second artifact endpoint. The status enum is
/// enough to tell an operator where to look next.
fn safe_result_detail(result: &E2eProbe) -> Option<String> {
    match result.status {
        E2eProbeStatus::Ok => match result.exit_verdict {
            E2eExitVerdict::Mismatch => Some("链路可达，但出口与 Serving 预期不一致".to_owned()),
            E2eExitVerdict::Unknown => Some("链路可达；该出口无法核对公网地址".to_owned()),
            E2eExitVerdict::Match => None,
        },
        E2eProbeStatus::HandshakeFailed => Some("入口握手或用户认证失败".to_owned()),
        E2eProbeStatus::Timeout => Some("完整链路在时限内没有返回".to_owned()),
        E2eProbeStatus::ChainBroken => Some("握手后未能完成出口请求".to_owned()),
        E2eProbeStatus::Unsupported => Some("Console 拨测执行器无法完成该项目".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_probe_detail_is_never_returned() {
        let result = E2eProbe {
            app_id: None,
            chain_id: "chain".to_owned(),
            status: E2eProbeStatus::HandshakeFailed,
            ttfb_ms: None,
            exit_ip: Some("203.0.113.8".to_owned()),
            exit_loc: Some("ZZ".to_owned()),
            exit_verdict: E2eExitVerdict::Unknown,
            detail: Some("uuid secret at 203.0.113.8 vendor-name".to_owned()),
        };
        let safe = safe_result_detail(&result).unwrap();
        assert_eq!(safe, "入口握手或用户认证失败");
        assert!(!safe.contains("203.0.113.8"));
        assert!(!safe.contains("secret"));
        assert!(!safe.contains("vendor-name"));
    }

    #[test]
    fn a_wrong_exit_is_not_an_authorization_pass() {
        let result = E2eProbe {
            app_id: None,
            chain_id: "chain".to_owned(),
            status: E2eProbeStatus::Ok,
            ttfb_ms: Some(42),
            exit_ip: None,
            exit_loc: None,
            exit_verdict: E2eExitVerdict::Mismatch,
            detail: None,
        };
        assert_eq!(
            safe_result_detail(&result).as_deref(),
            Some("链路可达，但出口与 Serving 预期不一致")
        );
    }

    #[test]
    fn browser_selection_must_be_unique_and_belong_to_the_frozen_plan() {
        let all = ["a", "b"];
        assert_eq!(
            validate_selection(&["b".to_owned()], all).unwrap(),
            HashSet::from(["b".to_owned()])
        );
        assert!(matches!(
            validate_selection(&["a".to_owned(), "a".to_owned()], all),
            Err(StoreError::InvalidData(_))
        ));
        assert!(matches!(
            validate_selection(&["not-a-target".to_owned()], all),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn runtime_directory_is_created_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "brocade-console-probe-test-{}-{}",
            std::process::id(),
            unix_now()
        ));
        inspect_runtime_dir(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_dir(dir).unwrap();
    }
}
