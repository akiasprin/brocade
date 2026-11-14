mod google;

use super::{
    behavior::{self, Random},
    now,
    session::Session,
    Context,
};
use crate::http::{maintenance_client, AddressFamily};
use brocade_deployment::ip_maintenance::*;
use reqwest::{
    cookie::CookieStore as _,
    header::{
        HeaderMap, HeaderValue, ACCEPT, ACCEPT_ENCODING, ACCEPT_LANGUAGE, LOCATION, REFERER,
        USER_AGENT,
    },
    Url,
};
use reqwest_cookie_store::CookieStoreMutex;
use std::{collections::BTreeMap, io::Read, net::IpAddr, sync::Arc, time::Duration};
use tokio::{sync::watch, time::Instant};

struct Page {
    status: u16,
    headers: HeaderMap,
    body: String,
    url: Url,
}
fn family(policy: &IpMaintenancePolicy) -> Result<AddressFamily, String> {
    match policy.family {
        MaintenanceFamily::V4 => Ok(AddressFamily::V4),
        MaintenanceFamily::V6 => Ok(AddressFamily::V6),
        MaintenanceFamily::Dual => Err("address_family_unselected".into()),
    }
}
fn static_url(text: &str) -> Result<Url, String> {
    Url::parse(text).map_err(|_| "invalid_static_url".into())
}

struct FetchOptions<'a> {
    ua: &'a str,
    jar: Arc<CookieStoreMutex>,
    referer: Option<&'a str>,
    module: MaintenanceModule,
    timeout_secs: u32,
    youtube_probe: bool,
}
fn consent_cookie_header(url: &Url, jar: &CookieStoreMutex) -> Result<Option<HeaderValue>, String> {
    let Some(preference) = crate::http::anonymous_consent_cookie(url) else {
        return Ok(None);
    };
    let existing = jar.cookies(url);
    let cookies = existing
        .as_ref()
        .map(|cookies| cookies.to_str())
        .transpose()
        .map_err(|_| "consent_cookie_invalid")?
        .unwrap_or("");
    // An explicit Cookie header replaces reqwest's jar header. Preserve every
    // other anonymous session cookie and replace SOCS once, including domain copies.
    let cookies = cookies
        .split(';')
        .map(str::trim)
        .filter(|cookie| {
            !cookie.is_empty()
                && cookie
                    .split_once('=')
                    .is_none_or(|(name, _)| name != "SOCS")
        })
        .chain([preference])
        .collect::<Vec<_>>()
        .join("; ");
    HeaderValue::from_str(&cookies)
        .map(|mut header| {
            header.set_sensitive(true);
            Some(header)
        })
        .map_err(|_| "consent_cookie_invalid".into())
}
async fn fetch(
    context: &Context,
    policy: &IpMaintenancePolicy,
    mut url: Url,
    options: FetchOptions<'_>,
) -> Result<Page, String> {
    let FetchOptions {
        ua,
        jar,
        referer,
        module,
        timeout_secs,
        youtube_probe,
    } = options;
    let timeout = Duration::from_secs(u64::from(timeout_secs));
    let work =
        async {
            for _ in 0..=50 {
                let consent_cookie = consent_cookie_header(&url, &jar)?;
                let client = maintenance_client(
                    &url,
                    family(policy)?,
                    policy.source_ip,
                    Arc::clone(&jar),
                    timeout,
                    module == MaintenanceModule::Trust,
                )
                .await?;
                context.reserve_request(policy)?;
                let mut request = client.get(url.clone());
                if youtube_probe {
                    for (name, value) in crate::http::public_diagnostic_headers() {
                        request = request.header(name, value);
                    }
                } else {
                    request = request.header(USER_AGENT, ua).header(ACCEPT, "*/*");
                }
                if let Some(cookie) = consent_cookie {
                    request = request.header(reqwest::header::COOKIE, cookie);
                }
                if let Some(value) = referer {
                    request = request.header(REFERER, value);
                }
                if module == MaintenanceModule::Trust {
                    request = request
                        .header(
                            ACCEPT,
                            "text/html,application/xhtml+xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
                        )
                        .header(ACCEPT_ENCODING, "gzip")
                        .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9")
                        .header("sec-fetch-dest", "document")
                        .header("sec-fetch-mode", "navigate")
                        .header("upgrade-insecure-requests", "1");
                }
                let mut response = request.send().await.map_err(|error| {
                    if error.is_timeout() {
                        "network_timeout"
                    } else {
                        "network_request"
                    }
                })?;
                let status = response.status().as_u16();
                if response
                    .headers()
                    .iter()
                    .map(|(name, value)| name.as_str().len() + value.len() + 4)
                    .sum::<usize>()
                    > 64 * 1024
                {
                    return Err("response_header_limit".into());
                }
                let headers = response.headers().clone();
                if status == 429 {
                    let delay = headers
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(retry_after)
                        .unwrap_or(policy.cooldown_minutes * 60);
                    context.mutate(|state| {
                        state.cooldown_until =
                            state.cooldown_until.max(now()?.saturating_add(i64::from(
                                delay.max(policy.cooldown_minutes * 60),
                            )));
                        Ok(())
                    })?;
                    return Err("service_restricted".into());
                }
                let mut body = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(|_| "network_body")? {
                    context.received(chunk.len(), policy)?;
                    if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                        return Err("response_size_limit".into());
                    }
                    body.extend_from_slice(&chunk);
                }
                let body = if headers
                    .get("content-encoding")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.eq_ignore_ascii_case("gzip"))
                {
                    let mut decoded = Vec::new();
                    flate2::read::GzDecoder::new(body.as_slice())
                        .take(MAX_RESPONSE_BYTES as u64 + 1)
                        .read_to_end(&mut decoded)
                        .map_err(|_| "invalid_gzip_body")?;
                    if decoded.len() > MAX_RESPONSE_BYTES {
                        return Err("response_size_limit".into());
                    }
                    decoded
                } else {
                    body
                };
                let body = String::from_utf8_lossy(&body).into_owned();
                if crate::http::is_consent_page(&url, &body) {
                    return Err("consent_required".into());
                }
                let lower = body.to_ascii_lowercase();
                if lower.contains("our systems have detected unusual traffic")
                    || lower.contains("<title>just a moment")
                    || url.path().starts_with("/sorry/")
                {
                    return Err("verification_required".into());
                }
                if matches!(status, 301 | 302 | 303 | 307 | 308) {
                    let target = headers
                        .get(LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .ok_or("invalid_redirect")?;
                    url = url.join(target).map_err(|_| "invalid_redirect")?;
                    // The next hop repeats public-address validation and pins its own DNS result.
                    continue;
                }
                return Ok(Page {
                    status,
                    headers,
                    body,
                    url,
                });
            }
            Err("redirect_limit".into())
        };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| "network_timeout".to_owned())?
}

fn validate_action_page(action: &behavior::Action, page: &Page) -> Result<(), String> {
    if (300..400).contains(&page.status) {
        return Err("unexpected_response_status".into());
    }
    if !(200..300).contains(&page.status) {
        return Ok(());
    }
    let connectivity = action.kind == "platform_page"
        && page.status == 204
        && page.url.host_str() == Some("connectivitycheck.gstatic.com")
        && page.url.path() == "/generate_204";
    if page.body.trim().is_empty() && !connectivity {
        return Err("empty_response".into());
    }
    if action.kind == "local_site"
        && behavior::is_news_redirect(&page.url)
        && page.body.contains("data-n-a-sg")
    {
        return Err("article_redirect_unresolved".into());
    }
    Ok(())
}

async fn retry_request<F, Fut>(mut request: F) -> Result<Page, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Page, String>>,
{
    let mut attempt = 0;
    loop {
        attempt += 1;
        let result = request().await;
        let retryable = match &result {
            Ok(page) => !(200..400).contains(&page.status),
            Err(error) => !matches!(
                error.as_str(),
                "response_size_limit"
                    | "daily_budget"
                    | "round_download_limit"
                    | "service_restricted"
                    | "non_public_target"
                    | "no_ipv4_address"
                    | "no_ipv6_address"
                    | "verification_required"
                    | "article_redirect_unresolved"
            ),
        };
        if !retryable || attempt == 3 {
            return result;
        }
        tokio::time::sleep(Duration::from_secs(attempt)).await;
    }
}

// Action failures stay local; shared budgets and service cooldown still stop the round.
async fn execute_actions(
    context: &Context,
    p: &IpMaintenancePolicy,
    module: MaintenanceModule,
    ua: &str,
    session: &Session,
    total: u32,
    mut action_for: impl FnMut(u32, &mut Random) -> Result<behavior::Action, String>,
) -> Result<(), String> {
    let mut random = Random;
    let mut referers = BTreeMap::<&str, String>::new();
    let platform = behavior::platform(ua);
    let mut succeeded = 0;
    let mut skipped = 0;
    for index in 0..total {
        let action = action_for(index, &mut random)?;
        let tracks_referer = module == MaintenanceModule::Google
            && (action.kind != "platform_page" || !matches!(platform, "android" | "ios" | "macos"));
        let referer = if tracks_referer && random.draw(100)? < u32::from(p.behavior.referer_percent)
        {
            referers.get(action.kind).cloned()
        } else {
            None
        };
        context.mutate(|state| {
            if let Some(r) = &mut state.current {
                r.action_index = index + 1;
                r.actions.push(MaintenanceAction {
                    index: index + 1,
                    kind: action.kind.into(),
                    host: action.url.host_str().ok_or("invalid_action_host")?.into(),
                    status: None,
                    error: None,
                    response_bytes: 0,
                });
            }
            Ok(())
        })?;
        context.emit(MaintenancePhase::Requesting, None, None)?;
        let before = context.locked()?.daily_bytes;
        let page = retry_request(|| async {
            let result = fetch(
                context,
                p,
                action.url.clone(),
                FetchOptions {
                    ua,
                    jar: Arc::clone(&session.jar),
                    referer: referer.as_deref(),
                    module,
                    timeout_secs: p.behavior.request_timeout_secs,
                    youtube_probe: false,
                },
            )
            .await
            .and_then(|page| validate_action_page(&action, &page).map(|()| page));
            session.save()?;
            result
        })
        .await;
        let oversized = matches!(&page, Err(error) if error == "response_size_limit");
        let (status, error) = match &page {
            Ok(page) => (Some(page.status), None),
            Err(error) => (
                None,
                Some(if oversized {
                    "response_size_skipped".into()
                } else {
                    error.clone()
                }),
            ),
        };
        context.mutate(|state| {
            let bytes = state.daily_bytes.saturating_sub(before);
            if let Some(r) = &mut state.current {
                let action = r.actions.last_mut().ok_or("missing_action_checkpoint")?;
                action.status = status;
                action.error = error;
                action.response_bytes = bytes;
            }
            Ok(())
        })?;
        if oversized
            || matches!(&page, Err(error) if matches!(error.as_str(), "no_ipv4_address" | "no_ipv6_address"))
        {
            skipped += 1;
            referers.remove(action.kind);
            continue;
        }
        match page {
            Ok(page) if (200..300).contains(&page.status) => {
                succeeded += 1;
                if tracks_referer {
                    referers.insert(action.kind, action.url.to_string());
                }
            }
            Ok(_) => {
                referers.remove(action.kind);
                continue;
            }
            Err(error) => {
                referers.remove(action.kind);
                if matches!(
                    error.as_str(),
                    "daily_budget" | "service_restricted" | "round_download_limit"
                ) {
                    return Err(error);
                }
                continue;
            }
        }
        if action.dwell_secs > 0 {
            let until = now()?.saturating_add(i64::from(action.dwell_secs));
            context.emit(MaintenancePhase::Dwelling, None, Some(until))?;
            tokio::time::sleep(Duration::from_secs(u64::from(action.dwell_secs))).await;
        }
    }
    if succeeded == 0 && skipped < total {
        return Err("all_actions_failed".into());
    }
    Ok(())
}

fn record_exit(
    context: &Context,
    policy: &IpMaintenancePolicy,
    exit: IpAddr,
) -> Result<(), String> {
    context.mutate(|state| {
        if let Some(report) = &mut state.current {
            report.actual_exit_ip = Some(exit);
        }
        Ok(())
    })?;
    if !policy.family.accepts(exit)
        || policy
            .expected_public_ip
            .is_some_and(|expected| expected != exit)
    {
        return Err("expected_exit_changed".into());
    }
    Ok(())
}

async fn execute(context: Arc<Context>, desired: IpMaintenanceDesired) -> Result<(), String> {
    let selected = context
        .family
        .map(|family| desired.policy.for_family(family))
        .transpose()?;
    let p = selected.as_ref().unwrap_or(&desired.policy);
    let cached = context
        .assets
        .lock()
        .map_err(|_| "maintenance_data_lock")?
        .clone();
    let bundle = match cached.filter(|data| data.region.id == p.region) {
        Some(data) => data,
        None => Arc::new(data::MaintenanceData::seed(&p.region).ok_or("unknown_region")?),
    };
    let region = &bundle.region;
    let mut random = Random;
    let module = if p.google && (!p.trust || random.draw(100)? < u32::from(p.google_percent)) {
        MaintenanceModule::Google
    } else {
        MaintenanceModule::Trust
    };
    let total = random.between(if module == MaintenanceModule::Google {
        &p.behavior.google_actions
    } else {
        &p.behavior.trust_actions
    })?;
    context.begin_round(&desired, module, total)?;
    context.mutate(|state| {
        if let Some(report) = &mut state.current {
            report.data_digest = Some(bundle.digest.clone());
        }
        Ok(())
    })?;
    if let Some(source) = p.source_ip {
        std::net::UdpSocket::bind((source, 0)).map_err(|_| "source_unavailable")?;
    }
    let clean = Arc::new(CookieStoreMutex::default());
    let trace_url = static_url("https://www.cloudflare.com/cdn-cgi/trace")?;
    let trace = retry_request(|| {
        fetch(
            &context,
            p,
            trace_url.clone(),
            FetchOptions {
                ua: "brocade-maintenance",
                jar: Arc::clone(&clean),
                referer: None,
                module: MaintenanceModule::Google,
                timeout_secs: p.behavior.request_timeout_secs,
                youtube_probe: false,
            },
        )
    })
    .await?;
    if !(200..300).contains(&trace.status) {
        return Err("exit_observation_failed".into());
    }
    let exit = trace
        .body
        .lines()
        .find_map(|line| line.strip_prefix("ip="))
        .and_then(|ip| ip.trim().parse::<IpAddr>().ok())
        .ok_or("exit_observation_invalid")?;
    record_exit(&context, p, exit)?;
    let agents = &bundle.user_agents;
    let seed_identity = if exit.is_ipv6() {
        format!("[{exit}]")
    } else {
        exit.to_string()
    };
    let indexes = behavior::persona_indexes(&seed_identity, agents.len());
    let ua = &agents[indexes[random.draw(3)? as usize]];
    let platform = behavior::platform(ua);
    let latitude = region.latitude + (f64::from(random.draw(540)?) - 270.0) / 10000.0;
    let longitude = region.longitude + (f64::from(random.draw(540)?) - 270.0) / 10000.0;
    let identity = format!(
        "{exit}|{}|{module:?}|{}",
        p.region, desired.session_generation
    );
    let session = Session::open(&context.directory, &identity, p.behavior.cookie_idle_days)?;
    let device = behavior::DeviceSession {
        region,
        behavior: &p.behavior,
        device: platform,
        latitude,
        longitude,
    };
    execute_actions(&context, p, module, ua, &session, total, |index, random| {
        behavior::action(module, &device, index + 1 == total, random)
    })
    .await?;
    if module == MaintenanceModule::Google {
        context.emit(MaintenancePhase::Postcheck, None, None)?;
        // Retain the legacy source key so stored reports and older Consoles remain readable.
        for (source, target, timeout) in [
            ("google_jump", google::TARGET, 10),
            ("youtube_premium", crate::unlock_probe::PREMIUM_TARGET, 12),
            ("youtube_music", crate::unlock_probe::MUSIC_TARGET, 12),
        ] {
            let target = static_url(target)?;
            let page = retry_request(|| {
                fetch(
                    &context,
                    p,
                    target.clone(),
                    FetchOptions {
                        ua: google::USER_AGENT,
                        // A region cookie from the activity session must not choose the probe country.
                        jar: Arc::new(CookieStoreMutex::default()),
                        referer: None,
                        module,
                        timeout_secs: timeout,
                        youtube_probe: source != "google_jump",
                    },
                )
            })
            .await;
            session.save()?;
            let (status, country) = match page {
                Ok(page) => {
                    let country = if source == "google_jump" {
                        google::response_country(page.status, &page.body)
                    } else {
                        let cookies = page
                            .headers
                            .get_all("set-cookie")
                            .iter()
                            .filter_map(|value| value.to_str().ok())
                            .collect::<Vec<_>>();
                        crate::unlock_probe::youtube_response_country(&page.body, &cookies)
                    };
                    (Some(page.status), country.filter(|c| c.len() == 2))
                }
                Err(error) if matches!(error.as_str(), "daily_budget" | "service_restricted") => {
                    return Err(error)
                }
                Err(_) => (None, None),
            };
            context.mutate(|state| {
                if let Some(r) = &mut state.current {
                    r.evidence.push(MaintenanceEvidence {
                        source: source.into(),
                        country,
                        status,
                    });
                }
                Ok(())
            })?;
        }
        context.mutate(|state| {
            if let Some(report) = &mut state.current {
                report.evidence_observed_at_unix_secs = Some(now()?);
                state.last_evidence = Some(report.clone());
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn retry_after(value: &str) -> Option<u32> {
    value
        .parse::<u32>()
        .ok()
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()?
                .duration_since(std::time::SystemTime::now())
                .ok()
                .and_then(|d| u32::try_from(d.as_secs()).ok())
        })
        .map(|delay| delay.min(7 * 86400))
}
fn should_cancel(
    old: &IpMaintenanceDesired,
    next: &IpMaintenanceDesired,
    round: Option<&str>,
) -> bool {
    old.generation != next.generation
        || old.session_generation != next.session_generation
        || old.policy != next.policy
        || (next.stop_sequence > old.stop_sequence
            && next.target_round_id.as_deref() == round
            && round.is_some())
}
pub(super) fn schedule(
    now: i64,
    policy: &IpMaintenancePolicy,
    last_started_at: i64,
    random: &mut Random,
) -> Result<i64, String> {
    let interval = i64::from(policy.interval_minutes) * 60;
    let slot = now
        .div_euclid(interval)
        .max(last_started_at.div_euclid(interval))
        .checked_add(1)
        .ok_or("schedule_overflow")?;
    slot.checked_mul(interval)
        .and_then(|base| {
            base.checked_add(i64::from(
                random.draw(policy.behavior.timer_jitter_secs + 1).ok()?,
            ))
        })
        .and_then(|base| {
            base.checked_add(i64::from(
                random.draw(policy.behavior.runner_jitter_secs + 1).ok()?,
            ))
        })
        .ok_or_else(|| "schedule_overflow".into())
}

fn policy_channel_error(context: &Context) -> &'static str {
    if super::terminating(&context.stop) {
        "agent_stopping"
    } else {
        "policy_channel_closed"
    }
}

async fn controlled_round(
    context: &Arc<Context>,
    desired: &IpMaintenanceDesired,
    receiver: &mut watch::Receiver<Option<IpMaintenanceDesired>>,
    current: &mut Option<IpMaintenanceDesired>,
    operation: impl std::future::Future<Output = Result<(), String>>,
) -> Result<(), String> {
    let mut heartbeat = Instant::now();
    let round = tokio::time::timeout(Duration::from_secs(MAX_ROUND_SECS), operation);
    tokio::pin!(round);
    loop {
        tokio::select! {
            result = &mut round => return result.map_err(|_| "round_deadline".to_owned()).and_then(|value|value),
            changed = receiver.changed() => {
                changed.map_err(|_| policy_channel_error(context.as_ref()))?;
                let next = receiver.borrow_and_update().clone();
                let round_id = context.locked()?.current.as_ref().and_then(|r|r.round_id.clone());
                if next.as_ref().is_none_or(|next| should_cancel(desired,next,round_id.as_deref())) {
                    let stop = next.as_ref().is_some_and(|next| next.generation==desired.generation && next.session_generation==desired.session_generation && next.policy==desired.policy);
                    *current=next;
                    return Err(if stop {"stop_requested"}else{"policy_changed"}.into());
                }
                if let Some(next)=&next {
                    context.apply(next)?;
                    context.mutate(|state| {state.manual_sequence=state.manual_sequence.max(next.manual_sequence);Ok(())})?;
                    let report=context.locked()?.current.clone();
                    if let Some(report)=report {context.emit(report.phase,report.reason.as_deref(),report.phase_deadline_unix_secs)?;}
                }
                *current=next;
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if super::terminating(&context.stop) {return Err("agent_stopping".into());}
                if heartbeat.elapsed()>=Duration::from_secs(30) {
                    let (phase,deadline)={
                        let state=context.locked()?;
                        state.current.as_ref().map(|r|(r.phase,r.phase_deadline_unix_secs)).unwrap_or((MaintenancePhase::Preflight,None))
                    };
                    context.emit(phase,None,deadline)?;
                    heartbeat=Instant::now();
                }
                if !desired.policy.continue_offline && context.lease_expired() {
                    return Err("policy_lease_expired".into());
                }
            }
        }
    }
}

pub(super) async fn run(
    context: Arc<Context>,
    mut receiver: watch::Receiver<Option<IpMaintenanceDesired>>,
) -> Result<(), String> {
    // A new healthy worker publishes its actual idle/running state. The failed
    // round remains in the durable terminal queue; recovery does not reset usage.
    if !super::terminating(&context.stop)
        && context.locked()?.current.as_ref().is_some_and(|report| {
            report.phase == MaintenancePhase::Failed
                && report.reason.as_deref() == Some("worker_recovering")
        })
    {
        context.clear_round()?;
    }
    let mut current = receiver.borrow().clone();
    let mut heartbeat = Instant::now();
    loop {
        if super::terminating(&context.stop) {
            return Ok(());
        }
        let Some(desired) = current.clone() else {
            tokio::select! {
                changed = receiver.changed() => { changed.map_err(|_| policy_channel_error(context.as_ref()))?; current = receiver.borrow_and_update().clone(); },
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            continue;
        };
        desired.policy.validate()?;
        if super::terminating(&context.stop) {
            return Ok(());
        }
        let applied_changed = context.locked()?.current.as_ref().is_some_and(|r| {
            r.generation != desired.generation
                || r.applied_control_sequence != desired.control_sequence
                || r.applied_stop_sequence != desired.stop_sequence
                || r.applied_session_generation != desired.session_generation
        });
        context.apply(&desired)?;
        if context.locked()?.desired.as_ref() != Some(&desired) {
            current = receiver.borrow_and_update().clone();
            tokio::task::yield_now().await;
            continue;
        }
        if context
            .family
            .is_some_and(|family| !desired.policy.family.includes(family))
        {
            tokio::select! {
                changed = receiver.changed() => { changed.map_err(|_| policy_channel_error(context.as_ref()))?; current = receiver.borrow_and_update().clone(); },
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            continue;
        }
        if applied_changed {
            let report = context.locked()?.current.clone();
            if let Some(report) = report {
                context.emit(
                    report.phase,
                    report.reason.as_deref(),
                    report.phase_deadline_unix_secs,
                )?;
            }
        }
        let stamp = now()?;
        context.schedule_families(&desired, stamp)?;
        let manual = {
            let state = context.locked()?;
            desired.manual_sequence > state.manual_sequence
                && desired
                    .manual_expires_at
                    .is_some_and(|until| until >= stamp)
        };
        let due = {
            let state = context.locked()?;
            if context.family.is_some() {
                state.scheduled
            } else {
                state.next_scheduled_at <= stamp
            }
        };
        let offline = !desired.policy.continue_offline && context.lease_expired();
        let blocked = { context.locked()?.terminal.len() >= 64 };
        let runnable = desired.policy.google || desired.policy.trust;
        let cooling = context.locked()?.cooldown_until > stamp;
        if runnable
            && (manual || (desired.policy.enabled && due))
            && !offline
            && !blocked
            && !cooling
        {
            context.mutate(|state| {
                state.manual_sequence = state.manual_sequence.max(desired.manual_sequence);
                state.scheduled = false;
                Ok(())
            })?;
            let result = controlled_round(
                &context,
                &desired,
                &mut receiver,
                &mut current,
                execute(Arc::clone(&context), desired.clone()),
            )
            .await;
            if context.family.is_none() {
                let mut random = Random;
                let next = schedule(
                    now()?,
                    &desired.policy,
                    context.locked()?.last_started_at,
                    &mut random,
                )?;
                context.mutate(|state| {
                    state.next_scheduled_at = state.next_scheduled_at.max(next);
                    Ok(())
                })?;
            }
            let stopping = matches!(&result,Err(error) if error=="agent_stopping");
            match result {
                Ok(()) => context.emit(MaintenancePhase::Complete, None, None)?,
                Err(error) => {
                    let phase = match error.as_str() {
                        "daily_budget" => MaintenancePhase::BudgetWait,
                        "source_unavailable" | "expected_exit_changed" => {
                            MaintenancePhase::BlockedEgress
                        }
                        "service_restricted" | "consecutive_network_failures" => {
                            MaintenancePhase::Cooldown
                        }
                        "policy_changed"
                        | "stop_requested"
                        | "policy_lease_expired"
                        | "agent_stopping" => MaintenancePhase::Interrupted,
                        _ => MaintenancePhase::Failed,
                    };
                    if phase == MaintenancePhase::Cooldown {
                        context.mutate(|state| {
                            state.cooldown_until =
                                state.cooldown_until.max(now()?.saturating_add(
                                    i64::from(desired.policy.cooldown_minutes) * 60,
                                ));
                            Ok(())
                        })?;
                    }
                    context.emit(phase, Some(&error), None)?;
                }
            }
            if stopping {
                return Ok(());
            }
        } else {
            let phase = if cooling {
                MaintenancePhase::Cooldown
            } else if !desired.policy.enabled || !runnable {
                MaintenancePhase::Disabled
            } else if blocked || offline {
                MaintenancePhase::Failed
            } else {
                MaintenancePhase::Waiting
            };
            let reason = if blocked {
                Some("maintenance_receipt_buffer_full")
            } else if offline {
                Some("policy_lease_expired")
            } else {
                None
            };
            let update = context.locked()?.current.as_ref().is_none_or(|r| {
                let preserve = r.phase.terminal()
                    && r.reason.as_deref() != Some("policy_lease_expired")
                    && (r.phase != MaintenancePhase::Cooldown || cooling)
                    && !blocked
                    && !offline;
                r.generation != desired.generation
                    || (!preserve && (r.phase != phase || r.reason.as_deref() != reason))
            });
            if update {
                context.clear_round()?;
                context.emit(phase, reason, None)?;
            }
        }
        if heartbeat.elapsed() > Duration::from_secs(30) {
            let (phase, reason, deadline) = {
                let state = context.locked()?;
                match &state.current {
                    Some(r) => (r.phase, r.reason.clone(), r.phase_deadline_unix_secs),
                    None => (MaintenancePhase::Waiting, None, None),
                }
            };
            context.emit(phase, reason.as_deref(), deadline)?;
            heartbeat = Instant::now();
        }
        tokio::select! {
            _=tokio::time::sleep(Duration::from_secs(1))=>{},
            changed=receiver.changed()=>{changed.map_err(|_|policy_channel_error(context.as_ref()))?;current=receiver.borrow_and_update().clone();}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_address_family_does_not_retry() {
        for error in ["no_ipv4_address", "no_ipv6_address"] {
            let mut attempts = 0;
            let result = retry_request(|| {
                attempts += 1;
                std::future::ready(Err(error.to_owned()))
            })
            .await;
            assert!(matches!(result, Err(ref actual) if actual == error));
            assert_eq!(attempts, 1);
        }
    }

    #[test]
    fn consent_preference_preserves_session_cookies_and_cannot_escape_the_service() {
        let jar = CookieStoreMutex::default();
        let url = Url::parse("https://www.youtube.com/premium?hl=en").unwrap();
        let cookies = [
            HeaderValue::from_static("session=fixture; Domain=.youtube.com; Path=/; Secure"),
            HeaderValue::from_static("SOCS=old-domain; Domain=.youtube.com; Path=/; Secure"),
            HeaderValue::from_static("SOCS=old-host; Path=/; Secure"),
        ];
        jar.set_cookies(&mut cookies.iter(), &url);
        let header = consent_cookie_header(&url, &jar).unwrap().unwrap();
        let header = header.to_str().unwrap();
        assert!(header.contains("session=fixture"));
        assert_eq!(header.matches("SOCS=").count(), 1);
        assert!(header.contains("SOCS=CAI"));
        assert!(!header.contains("old-domain") && !header.contains("old-host"));
        assert!(consent_cookie_header(
            &Url::parse("https://www.youtube.com.example.org/").unwrap(),
            &jar
        )
        .unwrap()
        .is_none());
        assert!(
            consent_cookie_header(&Url::parse("https://www.spiegel.de/").unwrap(), &jar)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn consent_interstitial_is_not_a_successful_page_or_an_ordinary_banner() {
        for host in ["consent.google.com", "consent.youtube.com"] {
            assert!(crate::http::is_consent_page(
                &Url::parse(&format!("https://{host}/m")).unwrap(),
                ""
            ));
        }
        let url = Url::parse("https://www.example.org/").unwrap();
        for title in [
            "Before you continue to Google",
            "Before you continue to YouTube",
            "Bevor Sie zu Google weitergehen",
            "Privacy consent required",
        ] {
            assert!(crate::http::is_consent_page(
                &url,
                &format!("<html><title>{title}</title></html>")
            ));
        }
        assert!(!crate::http::is_consent_page(&url, "<html><title>News article</title><article>Actual news</article><aside>Cookie consent required</aside></html>"));
        assert!(!crate::http::is_consent_page(
            &Url::parse("https://consent.google.com.example.org/").unwrap(),
            "<title>News</title>"
        ));
    }

    #[test]
    fn empty_and_unresolved_news_pages_are_not_successful_content_visits() {
        let action = behavior::Action {
            kind: "local_site",
            url: Url::parse("https://www.amazon.de/").unwrap(),
            dwell_secs: 0,
        };
        let mut page = Page {
            status: 202,
            headers: HeaderMap::new(),
            body: String::new(),
            url: action.url.clone(),
        };
        assert_eq!(
            validate_action_page(&action, &page).unwrap_err(),
            "empty_response"
        );
        page.status = 200;
        page.url = Url::parse("https://news.google.com/rss/articles/fixture").unwrap();
        page.body = "<title>Google News</title><c-wiz data-n-a-sg='fixture'></c-wiz>".into();
        assert_eq!(
            validate_action_page(&action, &page).unwrap_err(),
            "article_redirect_unresolved"
        );
        page.url = action.url.clone();
        page.body = "<title>Regional website</title><article>Actual content</article>".into();
        assert!(validate_action_page(&action, &page).is_ok());
        let probe = behavior::Action {
            kind: "platform_page",
            url: Url::parse("https://connectivitycheck.gstatic.com/generate_204").unwrap(),
            dwell_secs: 0,
        };
        page.url = probe.url.clone();
        page.status = 204;
        page.body.clear();
        assert!(validate_action_page(&probe, &page).is_ok());
        assert_eq!(
            validate_action_page(&action, &page).unwrap_err(),
            "empty_response"
        );
    }

    #[tokio::test]
    async fn closing_policy_channel_during_shutdown_cancels_the_round_normally() {
        for stopping in [false, true] {
            let fixture = super::super::tests::Fixture::new();
            let desired = IpMaintenanceDesired {
                generation: 1,
                ..Default::default()
            };
            let (sender, mut receiver) = watch::channel(Some(desired.clone()));
            let mut current = Some(desired.clone());
            fixture
                .0
                .stop
                .store(stopping, std::sync::atomic::Ordering::Release);
            drop(sender);
            let result = controlled_round(
                &fixture.0,
                &desired,
                &mut receiver,
                &mut current,
                std::future::pending(),
            )
            .await;
            assert_eq!(
                result,
                Err(if stopping {
                    "agent_stopping"
                } else {
                    "policy_channel_closed"
                }
                .into())
            );
        }
    }
    #[tokio::test(start_paused = true)]
    async fn dual_rounds_run_concurrently_and_stopping_one_does_not_cancel_the_other() {
        let fixture = super::super::tests::Fixture::new();
        fixture.0.synced().unwrap();
        let mut desired = IpMaintenanceDesired {
            generation: 1,
            ..Default::default()
        };
        desired.policy.family = MaintenanceFamily::Dual;
        fixture.0.apply(&desired).unwrap();
        let v4 = fixture.0.for_family(MaintenanceFamily::V4);
        let v6 = fixture.0.for_family(MaintenanceFamily::V6);
        for context in [&v4, &v6] {
            context
                .begin_round(&desired, MaintenanceModule::Google, 1)
                .unwrap();
        }
        let target = v4
            .locked()
            .unwrap()
            .current
            .as_ref()
            .unwrap()
            .round_id
            .clone();
        let (send, receive) = watch::channel(Some(desired.clone()));
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let launch = |context: Arc<Context>| {
            let mut receiver = receive.clone();
            let desired = desired.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                let mut current = Some(desired.clone());
                controlled_round(&context, &desired, &mut receiver, &mut current, async {
                    barrier.wait().await;
                    tokio::time::sleep(Duration::from_secs(600)).await;
                    Ok(())
                })
                .await
            })
        };
        let a = launch(v4);
        let b = launch(v6);
        barrier.wait().await;
        desired.control_sequence = 1;
        desired.stop_sequence = 1;
        desired.target_round_id = target;
        send.send_replace(Some(desired));
        assert_eq!(a.await.unwrap().unwrap_err(), "stop_requested");
        assert!(
            !b.is_finished(),
            "the other family remains active after a targeted stop"
        );
        fixture
            .0
            .stop
            .store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(b.await.unwrap().unwrap_err(), "agent_stopping");
    }
    #[tokio::test(start_paused = true)]
    async fn stop_cancels_a_dwell_and_dwell_continues_reporting_worker_health() {
        let fixture = super::super::tests::Fixture::new();
        fixture.0.synced().unwrap();
        let old = IpMaintenanceDesired {
            generation: 1,
            ..Default::default()
        };
        fixture.0.apply(&old).unwrap();
        fixture
            .0
            .begin_round(&old, MaintenanceModule::Google, 5)
            .unwrap();
        fixture
            .0
            .emit(MaintenancePhase::Dwelling, None, Some(now().unwrap() + 600))
            .unwrap();
        let round = fixture
            .0
            .locked()
            .unwrap()
            .current
            .as_ref()
            .unwrap()
            .round_id
            .clone();
        let before = fixture.0.locked().unwrap().sequence;
        let (sender, mut receiver) = watch::channel(Some(old.clone()));
        let context = Arc::clone(&fixture.0);
        let desired = old.clone();
        let task = tokio::spawn(async move {
            let mut current = Some(desired.clone());
            controlled_round(&context, &desired, &mut receiver, &mut current, async {
                tokio::time::sleep(Duration::from_secs(600)).await;
                Ok(())
            })
            .await
        });
        tokio::task::yield_now().await;
        for _ in 0..31 {
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
        }
        assert!(
            fixture.0.locked().unwrap().sequence > before,
            "a long dwell must report worker health"
        );
        let mut next = old;
        next.control_sequence = 1;
        next.stop_sequence = 1;
        next.target_round_id = round;
        sender.send_replace(Some(next));
        tokio::task::yield_now().await;
        assert_eq!(task.await.unwrap().unwrap_err(), "stop_requested");
        assert!(
            !fixture
                .0
                .locked()
                .unwrap()
                .desired
                .as_ref()
                .unwrap()
                .policy
                .enabled
        );
    }
    #[tokio::test(start_paused = true)]
    async fn a_disconnected_policy_expires_on_monotonic_time_without_a_wall_clock_change() {
        let fixture = super::super::tests::Fixture::new();
        fixture.0.synced().unwrap();
        let desired = IpMaintenanceDesired {
            generation: 1,
            ..Default::default()
        };
        fixture.0.apply(&desired).unwrap();
        fixture
            .0
            .begin_round(&desired, MaintenanceModule::Google, 5)
            .unwrap();
        let (_sender, mut receiver) = watch::channel(Some(desired.clone()));
        let context = Arc::clone(&fixture.0);
        let task = tokio::spawn(async move {
            let mut current = Some(desired.clone());
            controlled_round(
                &context,
                &desired,
                &mut receiver,
                &mut current,
                std::future::pending(),
            )
            .await
        });
        tokio::task::yield_now().await;
        for _ in 0..121 {
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(task.await.unwrap().unwrap_err(), "policy_lease_expired");
    }
    #[test]
    fn blocked_exit_still_preserves_the_observed_address() {
        let fixture = super::super::tests::Fixture::new();
        let desired = IpMaintenanceDesired {
            generation: 1,
            policy: IpMaintenancePolicy {
                expected_public_ip: Some("1.1.1.1".parse().unwrap()),
                ..Default::default()
            },
            ..Default::default()
        };
        fixture.0.apply(&desired).unwrap();
        fixture
            .0
            .begin_round(&desired, MaintenanceModule::Google, 5)
            .unwrap();
        assert_eq!(
            record_exit(&fixture.0, &desired.policy, "8.8.8.8".parse().unwrap()).unwrap_err(),
            "expected_exit_changed"
        );
        let disk: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.0.directory.join("checkpoint.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(disk["current"]["actual_exit_ip"], "8.8.8.8");
        assert_eq!(disk["daily_requests"], 0);
    }
    #[test]
    fn stop_targets_only_its_round_and_preserves_the_auto_flag() {
        let old = IpMaintenanceDesired {
            generation: 1,
            ..Default::default()
        };
        let mut next = old.clone();
        next.stop_sequence = 1;
        next.target_round_id = Some("a".into());
        assert!(should_cancel(&old, &next, Some("a")));
        assert!(!should_cancel(&old, &next, Some("b")));
        assert!(!should_cancel(&old, &next, None));
        assert_eq!(old.policy.enabled, next.policy.enabled);
    }
    #[test]
    fn retry_after_supports_seconds_and_http_dates() {
        assert_eq!(retry_after("120"), Some(120));
        let date = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(180));
        assert!((178..=180).contains(&retry_after(&date).unwrap()));
        assert_eq!(retry_after("garbage"), None);
    }
    #[test]
    fn scheduling_skips_old_slots_and_never_replays_a_missed_cycle() {
        let mut p = IpMaintenancePolicy {
            interval_minutes: 20,
            ..IpMaintenancePolicy::default()
        };
        p.behavior.timer_jitter_secs = 0;
        p.behavior.runner_jitter_secs = 0;
        assert_eq!(schedule(1260, &p, 0, &mut Random).unwrap(), 2400);
        assert_eq!(schedule(10, &p, 3600, &mut Random).unwrap(), 4800);
    }
}
#[cfg(test)]
mod netns_tests;
