//! Optional fixed-target web diagnostics, independent of convergence and accounting.
use crate::{
    http::{HttpClient, HttpResponse},
    options::Options,
};
use brocade_deployment::unlock_probe::{
    UnlockCode, UnlockPathError, UnlockProbeReport, UnlockProbeTarget, UnlockResult, UnlockService,
    UnlockStatus,
};
use std::{
    thread,
    time::{Duration, Instant},
};

mod chatgpt;
mod connections;
mod media;
mod youtube;
#[derive(Clone)]
struct ProbeContext {
    client: reqwest::blocking::Client,
    request_timeout: Duration,
    ipv6: bool,
    trace: Option<std::sync::Arc<connections::Source>>,
    trace_expected: bool,
    service: Option<UnlockService>,
    part: brocade_deployment::unlock_probe::UnlockPart,
    requests: std::sync::Arc<
        std::sync::Mutex<
            std::collections::BTreeMap<brocade_deployment::unlock_probe::UnlockPart, u32>,
        >,
    >,
}
impl ProbeContext {
    fn dial_failure(
        &self,
        domain: Option<&str>,
    ) -> Option<brocade_deployment::unlock_probe::UnlockDialFailureSummary> {
        let records = self.trace.as_ref()?.read(self.service?);
        let expected = self
            .requests
            .lock()
            .ok()?
            .get(&self.part)
            .copied()
            .unwrap_or(0);
        brocade_deployment::unlock_probe::observed_dial_failure(
            &records, self.part, expected, domain,
        )
    }

    fn can_retry_host(&self, host: &str) -> bool {
        self.dial_failure(Some(host))
            .is_none_or(|failure| failure.retry_allowed)
    }

    fn dial_error_code(
        &self,
        failure: brocade_deployment::unlock_probe::UnlockDialFailure,
    ) -> UnlockCode {
        use brocade_deployment::unlock_probe::UnlockDialFailure;
        match failure {
            UnlockDialFailure::NoIpv6Egress if self.ipv6 => UnlockCode::NoIpv6Egress,
            UnlockDialFailure::Timeout => UnlockCode::Timeout,
            _ => UnlockCode::NetworkError,
        }
    }

    fn fresh_service(&self) -> Self {
        Self {
            requests: Default::default(),
            ..self.clone()
        }
    }
    fn track_request(&self) {
        if self.trace_expected {
            if let Ok(mut counts) = self.requests.lock() {
                *counts.entry(self.part).or_default() += 1;
            }
        }
    }
    fn attach(&self, mut outcome: UnlockResult) -> UnlockResult {
        if self.trace_expected {
            outcome.connection_requests = self
                .requests
                .lock()
                .map(|counts| counts.clone())
                .unwrap_or_default();
        }
        if let (Some(trace), Some(service)) = (&self.trace, self.service) {
            outcome.connection_observations = trace.read(service);
            outcome.apply_absent_family_evidence(if self.ipv6 {
                brocade_deployment::unlock_probe::IpFamily::V6
            } else {
                brocade_deployment::unlock_probe::IpFamily::V4
            });
        }
        outcome
    }
}

const SERVICE_TIMEOUT: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
// Every chain needs room for relay/TLS and the complete HTTP body, including
// automatic paths through external proxies without connection evidence.
// Traced chains also wait for DNS evidence, which may use four seconds per resolver.
// The whole service still shares SERVICE_TIMEOUT, including every retry.
const CHAIN_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const REPORT_TTL: Duration = Duration::from_secs(60);
pub(crate) fn profile_supports_families(state_dir: &std::path::Path) -> bool {
    if crate::xray_process::serving(Some(state_dir)).is_none()
        || !crate::read_pending_config_convergence(state_dir).is_ok_and(|pending| pending.is_none())
    {
        return false;
    }
    std::fs::read(state_dir.join("xray.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|profile| {
            profile
                .get("brocadeUnlockIpFamilies")
                .and_then(serde_json::Value::as_u64)
                == Some(2)
        })
}
struct PendingReport {
    created: Instant,
    ttl: Duration,
    report: UnlockProbeReport,
}
fn measure(
    target: &UnlockProbeTarget,
    state_dir: &std::path::Path,
) -> (Vec<UnlockResult>, Option<UnlockPathError>) {
    let services = target.services();
    let Some(chain) = &target.chain else {
        let client = target.family.map(|family| {
            crate::http::diagnostic_family_client(match family {
                brocade_core::model::IpFamily::V4 => crate::http::AddressFamily::V4,
                brocade_core::model::IpFamily::V6 => crate::http::AddressFamily::V6,
            })
        });
        return match client {
            None => (detect(None, target.family, false, services), None),
            Some(Ok(client)) => {
                let context = ProbeContext {
                    client,
                    request_timeout: REQUEST_TIMEOUT,
                    ipv6: target.family == Some(brocade_core::model::IpFamily::V6),
                    trace: None,
                    trace_expected: false,
                    service: None,
                    part: brocade_deployment::unlock_probe::UnlockPart::Service,
                    requests: Default::default(),
                };
                (detect(Some(&context), target.family, false, services), None)
            }
            Some(Err(_)) => (
                services
                    .iter()
                    .copied()
                    .map(|service| {
                        result(
                            service,
                            UnlockStatus::Failed,
                            UnlockCode::NetworkError,
                            None,
                        )
                    })
                    .collect(),
                None,
            ),
        };
    };
    if target.family.is_some()
        && (target.family_ready != Some(true) || !profile_supports_families(state_dir))
    {
        return (
            services
                .iter()
                .copied()
                .map(|service| {
                    result(
                        service,
                        UnlockStatus::Failed,
                        UnlockCode::NetworkError,
                        None,
                    )
                })
                .collect(),
            Some(UnlockPathError::FamilyUnavailable),
        );
    }
    let options = brocade_probe::ProbeOptions::new(
        crate::options::xray_binary_path(),
        brocade_probe::ProbeCancellation::default(),
    );
    let mut chain = chain.clone();
    if let Some(family) = target.family {
        chain.uuid = brocade_core::model::probe_family_uuid(&chain.uuid, family);
    }
    let trace_groups = connections::groups(services);
    let session = if target.connection_trace {
        brocade_probe::start_traced_chain_client(&chain, &options, &trace_groups)
    } else {
        brocade_probe::start_chain_client(&chain, &options)
    };
    let client = session
        .as_ref()
        .ok()
        .and_then(|session| crate::http::diagnostic_socks_client(session.socks_port()).ok());
    match client {
        Some(client) => {
            let context = ProbeContext {
                client,
                request_timeout: CHAIN_REQUEST_TIMEOUT,
                ipv6: target.family == Some(brocade_core::model::IpFamily::V6),
                trace: session
                    .as_ref()
                    .ok()
                    .filter(|_| target.connection_trace)
                    .and_then(|session| {
                        connections::Source::new(session, &trace_groups)
                            .map(std::sync::Arc::new)
                            .map_err(|error| eprintln!("unlock connection evidence: {error}"))
                            .ok()
                    }),
                trace_expected: target.connection_trace,
                service: None,
                part: brocade_deployment::unlock_probe::UnlockPart::Service,
                requests: Default::default(),
            };
            (detect(Some(&context), target.family, true, services), None)
        }
        None => (
            services
                .iter()
                .copied()
                .map(|service| {
                    result(
                        service,
                        UnlockStatus::Failed,
                        UnlockCode::NetworkError,
                        None,
                    )
                })
                .collect(),
            Some(UnlockPathError::ClientUnavailable),
        ),
    }
    // The private Xray and its files stay alive until all requests have finished.
}

pub(crate) fn run(options: Options) {
    let mut pending = None;
    let mut failed = false;
    let mut wake =
        crate::wake::Cursor::new(brocade_deployment::agent_channel::WakeDomain::UnlockProbe);
    loop {
        let pause = match round(&options, &mut pending, |target| {
            measure(target, &options.state_dir)
        }) {
            Ok(supported) => {
                failed = false;
                if supported {
                    3
                } else {
                    300
                }
            }
            Err(error) => {
                if !failed {
                    eprintln!("unlock probe: {error}");
                }
                failed = true;
                15
            }
        };
        wake.wait_connected(
            Duration::from_secs(pause),
            Duration::from_secs(if pause == 3 { 60 } else { pause }),
        );
    }
}

fn round(
    options: &Options,
    pending: &mut Option<PendingReport>,
    measure: impl FnOnce(&UnlockProbeTarget) -> (Vec<UnlockResult>, Option<UnlockPathError>),
) -> Result<bool, String> {
    let client = HttpClient::new(&options.server)?;
    if pending
        .as_ref()
        .is_some_and(|report| report.created.elapsed() >= report.ttl)
    {
        *pending = None;
    }
    if pending.is_none() {
        let response =
            client.request("GET", "/agent/v1/unlock-probe/target", &options.token, None)?;
        if matches!(response.status, 404 | 405) {
            return Ok(false);
        }
        if response.status != 200 {
            return Err(format!("target HTTP {}", response.status));
        }
        let target: Option<UnlockProbeTarget> =
            serde_json::from_str(&response.body).map_err(|_| "invalid unlock target")?;
        let Some(target) = target else {
            return Ok(true);
        };
        if !target.valid_services() {
            return Err("invalid unlock service set".into());
        }
        if target.probe_id.len() != 32
            || !target.probe_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("invalid unlock probe ID".into());
        }
        let (results, path_error) = measure(&target);
        *pending = Some(PendingReport {
            created: Instant::now(),
            ttl: if target.family.is_some() || target.chain.is_some() {
                Duration::from_secs(360)
            } else {
                REPORT_TTL
            },
            report: UnlockProbeReport {
                probe_id: target.probe_id,
                family: target.family,
                results,
                path_error,
            },
        });
    }
    let body = serde_json::to_string(
        &pending
            .as_ref()
            .expect("pending report was filled above")
            .report,
    )
    .map_err(|_| "invalid unlock report")?;
    let response = client.request(
        "POST",
        "/agent/v1/unlock-probe/report",
        &options.token,
        Some(&body),
    )?;
    if (200..300).contains(&response.status)
        || matches!(response.status, 400 | 403 | 404 | 409 | 422)
    {
        *pending = None;
        return Ok(true);
    }
    Err(format!("report HTTP {}", response.status))
}

fn safe_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| {
            brocade_deployment::unlock_probe::V53_UNLOCK_SERVICES
                .iter()
                .any(|service| service.probe_hosts().contains(&host))
        })
}

struct Page {
    response: HttpResponse,
    url: reqwest::Url,
}
fn page_headers(url: &reqwest::Url) -> Vec<(&'static str, String)> {
    // A fresh EU/UK Premium request otherwise redirects to the consent interstitial.
    // This stateless preference contains no account or region and is sent only to YouTube;
    // availability and country must still come from the actual page and fresh response.
    if url.host_str() == Some("www.youtube.com") {
        vec![("Cookie", "SOCS=CAI".into())]
    } else if url.host_str() == Some("www.reddit.com") {
        // Reddit may strip X-Served-By; Fastly's public diagnostics expose the same
        // delivery cache in Fastly-Debug-Path without a second request or cookies.
        vec![("Fastly-Debug", "1".into())]
    } else {
        Vec::new()
    }
}

fn request_error_code(error: &str, proxy: Option<&ProbeContext>) -> UnlockCode {
    // A best-effort AAAA precheck can fail while another lookup is in progress.
    // The actual HTTP resolver can still conclusively establish address absence.
    if proxy.is_some_and(|context| context.ipv6)
        && error
            .split_ascii_whitespace()
            .any(|field| field == "kind=dns_no_ipv6")
    {
        UnlockCode::NoIpv6Dns
    } else if error.contains("timeout") {
        UnlockCode::Timeout
    } else {
        UnlockCode::NetworkError
    }
}

fn request_deadline(deadline: Instant, proxy: Option<&ProbeContext>) -> Instant {
    let timeout = proxy.map_or(REQUEST_TIMEOUT, |context| context.request_timeout);
    deadline.min(Instant::now() + timeout)
}

fn get(url: &str, deadline: Instant, proxy: Option<&ProbeContext>) -> Result<Page, UnlockCode> {
    // DNS/evidence, redirects, TLS and the complete body share the bounded request
    // budget. All chains get ten seconds; direct checks keep three seconds.
    let deadline = request_deadline(deadline, proxy);
    let mut url = reqwest::Url::parse(url).map_err(|_| UnlockCode::UnrecognizedResponse)?;
    for _ in 0..5 {
        if !safe_url(&url) {
            return Err(UnlockCode::UnrecognizedResponse);
        }
        // Keep failures local to this hostname/subitem. Another service hostname may
        // select a different exit, even within the same diagnostic.
        if let Some(failure) = proxy.and_then(|context| context.dial_failure(url.host_str())) {
            if !failure.retry_allowed {
                return Err(proxy
                    .zip(failure.failure)
                    .map_or(UnlockCode::NetworkError, |(context, failure)| {
                        context.dial_error_code(failure)
                    }));
            }
        }
        let budget = deadline.saturating_duration_since(Instant::now());
        if budget.is_zero() {
            return Err(UnlockCode::Timeout);
        }
        let transport = proxy
            .and_then(|context| {
                context.trace.as_ref().and_then(|trace| {
                    context
                        .service
                        .and_then(|service| trace.client(service, context.part))
                })
            })
            .or_else(|| proxy.map(|context| &context.client));
        if let Some(context) = proxy {
            context.track_request();
        }
        let client = HttpClient::new(url.origin().ascii_serialization().as_str())
            .map_err(|_| UnlockCode::NetworkError)?
            .with_diagnostic_proxy(transport);
        let mut path = url.path().to_owned();
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        let response = client
            .request_public_diagnostic(
                "GET",
                &path,
                None,
                "application/json",
                &page_headers(&url),
                budget,
            )
            .map_err(|error| {
                // HttpClient emits bounded phase/status counters and error categories, never
                // response material or credentials. Keep that evidence when the UI receives
                // only the stable network_error/timeout code, especially for chain TLS failures.
                eprintln!(
                    "unlock probe request origin={} {error}",
                    url.origin().ascii_serialization()
                );
                if let Some((context, failure)) = proxy.and_then(|context| {
                    context
                        .dial_failure(url.host_str())
                        .and_then(|summary| summary.failure)
                        .map(|failure| (context, failure))
                }) {
                    return context.dial_error_code(failure);
                }
                let no_address = proxy.is_some_and(|context| {
                    context.service.is_some_and(|service| {
                        context.trace.as_ref().is_some_and(|trace| {
                            let records = trace
                                .read(service)
                                .into_iter()
                                .filter(|record| record.part == context.part)
                                .collect::<Vec<_>>();
                            let expected = context
                                .requests
                                .lock()
                                .ok()
                                .and_then(|counts| counts.get(&context.part).copied())
                                .unwrap_or(0);
                            !records.is_empty()
                                && records.len() == expected as usize
                                && records.iter().all(|record| {
                                    record.connection.complete
                                        && record.connection.error.as_deref() == Some("dns_empty")
                                        && !record.connection.connected
                                })
                        })
                    })
                });
                if no_address {
                    if proxy.is_some_and(|context| context.ipv6) {
                        UnlockCode::NoIpv6Dns
                    } else {
                        UnlockCode::NoIpv4Dns
                    }
                } else {
                    request_error_code(&error, proxy)
                }
            })?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            let location = response
                .header("location")
                .ok_or(UnlockCode::UnrecognizedResponse)?;
            url = url
                .join(location)
                .map_err(|_| UnlockCode::UnrecognizedResponse)?;
            continue;
        }
        return Ok(Page { response, url });
    }
    Err(UnlockCode::UnrecognizedResponse)
}

const MAX_SERVICE_RETRIES: usize = 10;
const MAX_SERVICE_ATTEMPTS: usize = 1 + MAX_SERVICE_RETRIES;

fn detect(
    proxy: Option<&ProbeContext>,
    family: Option<brocade_core::model::IpFamily>,
    automatic_chain: bool,
    services: &[UnlockService],
) -> Vec<UnlockResult> {
    debug_assert!(services.len() <= brocade_deployment::unlock_probe::V53_UNLOCK_SERVICES.len());
    let run = |service| {
        let scoped = proxy.map(|parent| ProbeContext {
            client: parent
                .trace
                .as_ref()
                .and_then(|trace| {
                    trace.client(
                        service,
                        brocade_deployment::unlock_probe::UnlockPart::Service,
                    )
                })
                .cloned()
                .unwrap_or_else(|| parent.client.clone()),
            service: Some(service),
            ..parent.fresh_service()
        });
        let proxy = scoped.as_ref().or(proxy);
        let deadline = Instant::now() + SERVICE_TIMEOUT;
        let attempts = if family.is_some() || automatic_chain {
            MAX_SERVICE_ATTEMPTS
        } else {
            1
        };
        if service == UnlockService::Chatgpt {
            let outcome = chatgpt::detect(attempts, proxy);
            proxy.map_or(outcome.clone(), |context| context.attach(outcome))
        } else {
            confirm_with(
                service,
                attempts,
                deadline,
                || {
                    let outcome = check(service, deadline, proxy)
                        .unwrap_or_else(|code| result(service, UnlockStatus::Failed, code, None));
                    proxy.map_or(outcome.clone(), |context| context.attach(outcome))
                },
                || {
                    thread::sleep(
                        Duration::from_millis(500)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    )
                },
            )
        }
    };
    if family.is_none() && !automatic_chain {
        return services.iter().copied().map(run).collect();
    }
    // At most five workers for the negotiated service set, each with three services.
    // Each service, including all retries, has sixty seconds: at most 3 * 60
    // seconds per worker plus client startup,
    // below the fifteen-minute lease. Successful services stop retrying immediately.
    thread::scope(|scope| {
        let run = &run;
        let workers = services
            .chunks(3)
            .map(|chunk| scope.spawn(move || chunk.iter().copied().map(run).collect::<Vec<_>>()))
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .flat_map(|worker| match worker.join() {
                Ok(results) => results,
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect()
    })
}

fn needs_confirmation(outcome: &UnlockResult) -> bool {
    use brocade_deployment::unlock_probe::UnlockPart;
    if let Some(detail) = &outcome.youtube {
        [
            (UnlockPart::Webpage, &detail.webpage),
            (UnlockPart::Premium, &detail.premium),
        ]
        .iter()
        .any(|(part, page)| {
            page.status == UnlockStatus::Failed && outcome.dial_retry_allowed(*part, None)
        }) || detail.cdn.error.is_some_and(|error| {
            !error.is_family_skip() && outcome.dial_retry_allowed(UnlockPart::Cdn, None)
        })
    } else {
        outcome.status == UnlockStatus::Failed
            && outcome.dial_retry_allowed(UnlockPart::Service, None)
    }
}

fn merge_confirmation(mut previous: UnlockResult, current: UnlockResult) -> UnlockResult {
    if let (Some(before), Some(after)) = (&mut previous.youtube, current.youtube.as_ref()) {
        let merge_page =
            |before: &mut brocade_deployment::unlock_probe::YoutubePageResult,
             after: &brocade_deployment::unlock_probe::YoutubePageResult| {
                if before.status == UnlockStatus::Failed
                    || (before.region().is_none()
                        && after.status == before.status
                        && after.code == before.code
                        && after.region().is_some())
                {
                    *before = after.clone();
                    true
                } else {
                    false
                }
            };
        merge_page(&mut before.webpage, &after.webpage);
        merge_page(&mut before.premium, &after.premium);
        if before.cdn.node.is_none()
            || (before.cdn.airport.is_none() && after.cdn.airport.is_some())
        {
            before.cdn = after.cdn.clone();
        }
        previous.status = before.premium.status;
        previous.code = before.premium.code;
        previous.region = before.premium.region().map(str::to_owned);
        // Evidence describes every actual connection in this diagnostic run, including
        // retries. Keeping the complete per-part set avoids assigning late data by time.
        previous.connection_observations = current.connection_observations;
        previous.connection_requests = current.connection_requests;
        previous
    } else if previous.status == UnlockStatus::Failed
        || (previous.region.is_none()
            && current.status == previous.status
            && current.code == previous.code
            && current.region.is_some())
    {
        current
    } else {
        // Retrying unknown metadata must not erase an already observed access result.
        previous
    }
}

fn confirm_with(
    service: UnlockService,
    attempts: usize,
    deadline: Instant,
    mut check: impl FnMut() -> UnlockResult,
    mut pause: impl FnMut(),
) -> UnlockResult {
    if Instant::now() >= deadline {
        return result(service, UnlockStatus::Failed, UnlockCode::Timeout, None);
    }
    let mut outcome = check();
    if !needs_confirmation(&outcome) {
        return outcome;
    }
    let attempts = attempts.min(MAX_SERVICE_ATTEMPTS);
    for retry in 1..attempts {
        if Instant::now() >= deadline {
            break;
        }
        eprintln!(
            "unlock probe service={service:?} retry={retry}/{}",
            attempts - 1
        );
        pause();
        if Instant::now() >= deadline {
            break;
        }
        outcome = merge_confirmation(outcome, check());
        if !needs_confirmation(&outcome) {
            eprintln!(
                "unlock probe service={service:?} recovered_after={retry} status={:?} code={:?}",
                outcome.status, outcome.code
            );
            return outcome;
        }
    }
    eprintln!(
        "unlock probe service={service:?} attempts={attempts} status={:?} code={:?}",
        outcome.status, outcome.code
    );
    outcome
}

fn result(
    service: UnlockService,
    status: UnlockStatus,
    code: UnlockCode,
    region: Option<String>,
) -> UnlockResult {
    UnlockResult {
        service,
        status: if code.is_family_skip() {
            UnlockStatus::Skipped
        } else {
            status
        },
        code,
        region: if code.is_family_skip() { None } else { region },
        client_dns: false,
        youtube: None,
        dns_path: None,
        connection_observations: Vec::new(),
        connection_requests: Default::default(),
        dns_parts: Vec::new(),
    }
}
fn country(raw: &str) -> Option<String> {
    let raw = raw.trim().to_ascii_uppercase();
    if !(2..=3).contains(&raw.len()) || !raw.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    Some(
        match raw.as_str() {
            "HKG" => "HK",
            "MAC" => "MO",
            "CHN" => "CN",
            "TWN" => "TW",
            "USA" => "US",
            "JPN" => "JP",
            "KOR" => "KR",
            "SGP" => "SG",
            "GBR" => "GB",
            "DEU" => "DE",
            "FRA" => "FR",
            "CAN" => "CA",
            "AUS" => "AU",
            "IND" => "IN",
            _ => &raw,
        }
        .to_owned(),
    )
}
// Read an exact JSON field even when it lives inside an HTML script. Missing fields never
// imply success; challenge/login/changed pages need their own positive evidence.
fn field(body: &str, key: &str) -> Option<serde_json::Value> {
    let needle = format!("\"{key}\"");
    for (offset, _) in body.match_indices(&needle) {
        let Some(tail) = body[offset + needle.len()..].trim_start().strip_prefix(':') else {
            continue;
        };
        let tail = tail.trim_start();
        if let Some(Ok(value)) = serde_json::Deserializer::from_str(tail)
            .into_iter::<serde_json::Value>()
            .next()
        {
            return Some(value);
        }
    }
    None
}
fn region(body: &str, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| field(body, key).and_then(|value| value.as_str().and_then(country)))
}

fn trace_region(url: &str, deadline: Instant, proxy: Option<&ProbeContext>) -> Option<String> {
    let page = get(url, deadline, proxy).ok()?;
    if page.response.status != 200 {
        return None;
    }
    page.response
        .body
        .lines()
        .find_map(|line| line.strip_prefix("loc=").and_then(country))
}

fn tiktok(deadline: Instant, proxy: Option<&ProbeContext>) -> Result<UnlockResult, UnlockCode> {
    tiktok_with_check(|url| {
        let page = get(url, deadline, proxy)?;
        Ok(parse(
            UnlockService::Tiktok,
            page.response.status,
            &page.response.body,
            page.url.as_str(),
        ))
    })
}

fn tiktok_with_check(
    mut check: impl FnMut(&str) -> Result<UnlockResult, UnlockCode>,
) -> Result<UnlockResult, UnlockCode> {
    let home = check("https://www.tiktok.com/");
    if home.as_ref().is_ok_and(|result| {
        matches!(
            result.status,
            UnlockStatus::Available | UnlockStatus::UnsupportedRegion
        )
    }) {
        return home;
    }
    // Some IPs get an interstitial at / while the application at /explore is reachable.
    // Both requests use the same proxy and service deadline, including region/redirect reads.
    let explore = check("https://www.tiktok.com/explore");
    if explore.as_ref().is_ok_and(|result| {
        matches!(
            result.status,
            UnlockStatus::Available | UnlockStatus::UnsupportedRegion
        )
    }) {
        return explore;
    }
    if home
        .as_ref()
        .is_ok_and(|result| result.status == UnlockStatus::Restricted)
    {
        return home;
    }
    if explore
        .as_ref()
        .is_ok_and(|result| result.status == UnlockStatus::Restricted)
    {
        return explore;
    }
    // Keep a parsed result (and its service-attributed country) when the other request failed.
    match (home, explore) {
        (_, Ok(result)) | (Ok(result), _) => Ok(result),
        (Err(error), _) => Err(error),
    }
}

/// Exact opening tags only: an asset URL or a JavaScript component name is not rendered content.
fn html_tags<'a>(body: &'a str, tag: &'a str) -> impl Iterator<Item = &'a str> {
    body.match_indices(tag).filter_map(move |(offset, _)| {
        if offset == 0 || body.as_bytes()[offset - 1] != b'<' {
            return None;
        }
        let tail = &body[offset + tag.len()..];
        if !tail.starts_with(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/') {
            return None;
        }
        // Respect > inside quoted attributes when locating the tag boundary.
        let mut quote = None;
        for (index, byte) in tail.bytes().enumerate() {
            match (quote, byte) {
                (Some(expected), value) if value == expected => quote = None,
                (None, b'\'' | b'"') => quote = Some(byte),
                (None, b'>') => return Some(&tail[..index]),
                _ => {}
            }
        }
        None
    })
}

fn html_attribute<'a>(mut attributes: &'a str, name: &str) -> Option<&'a str> {
    while !attributes.is_empty() {
        attributes = attributes.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '/');
        let end = attributes
            .find(|c: char| c.is_ascii_whitespace() || c == '=')
            .unwrap_or(attributes.len());
        let key = &attributes[..end];
        attributes = attributes[end..].trim_start();
        if !attributes.starts_with('=') {
            continue;
        }
        attributes = attributes[1..].trim_start();
        let (value, rest) = match attributes.as_bytes().first().copied() {
            Some(quote @ (b'\'' | b'"')) => {
                let end = attributes[1..].find(char::from(quote))? + 1;
                (&attributes[1..end], &attributes[end + 1..])
            }
            Some(_) => {
                let end = attributes
                    .find(char::is_whitespace)
                    .unwrap_or(attributes.len());
                (&attributes[..end], &attributes[end..])
            }
            None => return None,
        };
        if key == name {
            return Some(value);
        }
        attributes = rest;
    }
    None
}

fn disney_post(
    path: &str,
    body: &str,
    content_type: &str,
    key: &str,
    deadline: Instant,
    proxy: Option<&ProbeContext>,
) -> Result<HttpResponse, UnlockCode> {
    let deadline = request_deadline(deadline, proxy);
    if let Some(context) = proxy {
        context.track_request();
    }
    let budget = deadline.saturating_duration_since(Instant::now());
    if budget.is_zero() {
        return Err(UnlockCode::Timeout);
    }
    HttpClient::new("https://disney.api.edge.bamgrid.com")
        .map_err(|_| UnlockCode::NetworkError)?
        .with_diagnostic_proxy(proxy.map(|context| &context.client))
        .request_public_diagnostic(
            "POST",
            path,
            Some(body),
            content_type,
            &[("Authorization", format!("Bearer {key}"))],
            budget,
        )
        .map_err(|error| request_error_code(&error, proxy))
}
fn disney(deadline: Instant, proxy: Option<&ProbeContext>) -> Result<UnlockResult, UnlockCode> {
    use UnlockCode as C;
    use UnlockStatus as U;
    let service = UnlockService::DisneyPlus;
    let page = get("https://www.disneyplus.com/", deadline, proxy)?;
    let preliminary = parse(
        service,
        page.response.status,
        &page.response.body,
        page.url.as_str(),
    );
    if matches!(preliminary.status, U::Restricted | U::UnsupportedRegion) {
        return Ok(preliminary);
    }
    // Discover the anonymous browser client identifier from Disney's public configuration.
    // Neither it nor short-lived device tokens are logged, persisted, or sent to Console.
    let body = page.response.body.replace("\\\"", "\"");
    let key = field(&body, "clientApiKey")
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or(C::UnrecognizedResponse)?;
    if key.len() > 512
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(C::UnrecognizedResponse);
    }
    let device = disney_post(
        "/devices",
        r#"{"deviceFamily":"browser","applicationRuntime":"chrome","deviceProfile":"linux","attributes":{}}"#,
        "application/json",
        &key,
        deadline,
        proxy,
    )?;
    if matches!(device.status, 403 | 451) {
        return Ok(result(service, U::Restricted, C::AccessDenied, None));
    }
    let assertion = serde_json::from_str::<serde_json::Value>(&device.body)
        .ok()
        .and_then(|v| v["assertion"].as_str().map(str::to_owned))
        .ok_or(C::UnrecognizedResponse)?;
    let mut form =
        reqwest::Url::parse("https://unused.invalid/").map_err(|_| C::UnrecognizedResponse)?;
    form.query_pairs_mut()
        .append_pair(
            "grant_type",
            "urn:ietf:params:oauth:grant-type:token-exchange",
        )
        .append_pair("platform", "browser")
        .append_pair("subject_token", &assertion)
        .append_pair(
            "subject_token_type",
            "urn:bamtech:params:oauth:token-type:device",
        );
    let token = disney_post(
        "/token",
        form.query().ok_or(C::UnrecognizedResponse)?,
        "application/x-www-form-urlencoded",
        &key,
        deadline,
        proxy,
    )?;
    if token.body.contains("forbidden-location") {
        return Ok(result(
            service,
            U::UnsupportedRegion,
            C::RegionUnsupported,
            None,
        ));
    }
    if matches!(token.status, 403 | 451) {
        return Ok(result(service, U::Restricted, C::AccessDenied, None));
    }
    let refresh_token = serde_json::from_str::<serde_json::Value>(&token.body)
        .ok()
        .and_then(|v| v["refresh_token"].as_str().map(str::to_owned))
        .ok_or(C::UnrecognizedResponse)?;
    let payload = serde_json::json!({
        "query": "mutation refreshToken($input: RefreshTokenInput!) { refreshToken(refreshToken: $input) { activeSession { sessionId } } }",
        "variables": {"input": {"refreshToken": refresh_token}}
    }).to_string();
    let session = disney_post(
        "/graph/v1/device/graphql",
        &payload,
        "application/json",
        &key,
        deadline,
        proxy,
    )?;
    let mut outcome = parse(service, session.status, &session.body, page.url.as_str());
    if field(&session.body, "inSupportedLocation").and_then(|v| v.as_bool()) == Some(false) {
        outcome.status = U::UnsupportedRegion;
        outcome.code = C::RegionUnsupported;
    }
    // A successful HTTP response without the session's country and availability is unknown.
    if outcome.status == U::Available && outcome.region.is_none() {
        return Err(C::UnrecognizedResponse);
    }
    Ok(outcome)
}

fn check(
    service: UnlockService,
    deadline: Instant,
    proxy: Option<&ProbeContext>,
) -> Result<UnlockResult, UnlockCode> {
    if service == UnlockService::YoutubePremium {
        return Ok(youtube::detect(deadline, proxy));
    }
    if service == UnlockService::Netflix {
        return netflix(deadline, proxy);
    }
    if service == UnlockService::DisneyPlus {
        return disney(deadline, proxy);
    }
    if service == UnlockService::Chatgpt {
        return Ok(chatgpt::once(deadline, proxy));
    }
    if service == UnlockService::Tiktok {
        return tiktok(deadline, proxy);
    }
    if let Some(outcome) = media::detect(service, deadline, proxy) {
        return outcome;
    }
    let url = match service {
        UnlockService::Claude => "https://claude.ai/",
        UnlockService::Gemini => "https://gemini.google.com/",
        UnlockService::DisneyPlus => "https://www.disneyplus.com/",
        UnlockService::Tiktok => "https://www.tiktok.com/",
        UnlockService::PrimeVideo => "https://www.primevideo.com/",
        UnlockService::Reddit => "https://www.reddit.com/",
        UnlockService::Chatgpt
        | UnlockService::Netflix
        | UnlockService::YoutubePremium
        | UnlockService::Spotify
        | UnlockService::HboMax
        | UnlockService::Dazn
        | UnlockService::BbcIplayer
        | UnlockService::Crunchyroll
        | UnlockService::ParamountPlus => unreachable!(),
    };
    let page = get(url, deadline, proxy)?;
    let mut outcome = if service == UnlockService::Reddit {
        reddit(&page)
    } else {
        parse(
            service,
            page.response.status,
            &page.response.body,
            page.url.as_str(),
        )
    };
    let trace = match service {
        UnlockService::Claude => Some("https://claude.ai/cdn-cgi/trace"),
        _ => None,
    };
    if let Some(trace) = trace {
        outcome.region = trace_region(trace, deadline, proxy);
    }
    Ok(outcome)
}

fn reddit(page: &Page) -> UnlockResult {
    let mut outcome = parse(
        UnlockService::Reddit,
        page.response.status,
        &page.response.body,
        page.url.as_str(),
    );
    // Fastly lists the shield first and the delivery cache last. Use the final POP
    // code as the requested Reddit region reference, without guessing from DNS/IP.
    let delivery = page
        .response
        .header_values("x-served-by")
        .flat_map(|value| value.split(','))
        .last()
        .map(str::trim);
    // Unlike X-Served-By, Debug-Path starts with the edge delivery cache. Later
    // entries may describe an old cache fill, so never use them as a fallback.
    let debug = page
        .response
        .header_values("fastly-debug-path")
        .next()
        .and_then(|path| path.trim().strip_prefix("(D "))
        .and_then(|path| path.split_once(')'))
        .and_then(|(entry, _)| {
            let mut parts = entry.split_whitespace();
            let node = parts.next()?;
            let stamp = parts.next()?;
            (parts.next().is_none()
                && stamp.len() <= 20
                && stamp.bytes().all(|byte| byte.is_ascii_digit()))
            .then_some(node)
        });
    outcome.region = delivery
        .and_then(fastly_pop)
        .or_else(|| debug.and_then(fastly_pop))
        .or(outcome.region);
    outcome
}

fn fastly_pop(node: &str) -> Option<String> {
    if !(10..=128).contains(&node.len())
        || !node.starts_with("cache-")
        || !node
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }
    let (node, code) = node.rsplit_once('-')?;
    (node.len() > 6 && code.len() == 3 && code.bytes().all(|byte| byte.is_ascii_alphabetic()))
        .then(|| code.to_ascii_uppercase())
}

fn parse(service: UnlockService, status: u16, body: &str, url: &str) -> UnlockResult {
    use UnlockCode as C;
    use UnlockService as S;
    use UnlockStatus as U;
    let original_body = body;
    let body = body.replace("\\\"", "\"").replace("&quot;", "\"");
    let lower = body.to_ascii_lowercase();
    // JSON string unescaping above is useful for script fields, but changes the quoting of
    // HTML attribute values. Parse Reddit's actual tags before that transformation.
    let reddit_html = if service == S::Reddit {
        original_body.to_ascii_lowercase()
    } else {
        String::new()
    };
    let mut region = match service {
        S::DisneyPlus => region(&body, &["countryCode", "region"]),
        S::YoutubePremium => youtube::topbar_region(original_body),
        // Other scripts contain CDN datacenter regions, and video authors have their own
        // region. Only the web application's viewer context describes this request's egress.
        // Parse the original JSON: unescaping quotes in descriptions/configuration first
        // invalidates the enclosing object even though its viewer region is present.
        S::Tiktok => field(original_body, "__DEFAULT_SCOPE__")
            .and_then(|scope| {
                scope
                    .get("webapp.app-context")?
                    .get("region")?
                    .as_str()
                    .and_then(country)
            })
            .or_else(|| {
                field(original_body, "AppContext").and_then(|context| {
                    context
                        .get("appContext")?
                        .get("region")?
                        .as_str()
                        .and_then(country)
                })
            }),
        S::Reddit => html_tags(&reddit_html, "shreddit-app")
            .find_map(|attributes| html_attribute(attributes, "country").and_then(country)),
        S::PrimeVideo => region(&body, &["currentTerritory"]),
        _ => None,
    };
    if service == S::Gemini {
        region = body
            .split_once(",2,1,200,\"")
            .and_then(|(_, tail)| tail.split_once('"'))
            .and_then(|(code, _)| country(code));
    }
    let tiktok_hk_restriction = service == S::Tiktok
        && reqwest::Url::parse(url).is_ok_and(|url| {
            url.host_str() == Some("www.tiktok.com")
                && url.path().trim_end_matches('/') == "/hk/notfound"
        });
    if tiktok_hk_restriction {
        region = Some("HK".into());
    }
    let unsupported = lower.contains("unsupported_country")
        || lower.contains("not available in your country")
        || lower.contains("not available in your region")
        || lower.contains("forbidden-location")
        || url.contains("app-unavailable-in-region")
        || (service == S::DisneyPlus && (url.contains("/unavailable") || url.contains("/preview")))
        || (service == S::Gemini
            && (body.contains("45631641,null,false")
                || field(&body, "45631641").and_then(|v| v.as_bool()) == Some(false)))
        || (service == S::PrimeVideo
            && field(&body, "isServiceRestricted").and_then(|v| v.as_bool()) == Some(true));
    // A redirect from the application homepage to its company site is not access to
    // the application. Do not infer a country or a geographic cause from CDN metadata.
    let tiktok_company_redirect = service == S::Tiktok
        && lower.contains("<title>about | tiktok</title>")
        && reqwest::Url::parse(url).is_ok_and(|url| {
            url.host_str() == Some("www.tiktok.com") && url.path().trim_end_matches('/') == "/about"
        });
    // Reddit's anonymous HTTP 200 continuation runs JavaScript automatically in a browser.
    // It is distinct from the explicit block page requiring login/developer credentials.
    let reddit_js_continuation = service == S::Reddit
        && html_tags(&reddit_html, "input")
            .any(|attributes| html_attribute(attributes, "name") == Some("js_challenge"));
    let reddit_login_block = service == S::Reddit
        && (lower.contains("you've been blocked")
            || lower.contains("your request has been blocked")
            || lower.contains("whoa there, pardner")
            || (lower.contains("to continue")
                && lower.contains("log in")
                && lower.contains("developer token")));
    let (state, code) = if unsupported || tiktok_hk_restriction {
        (U::UnsupportedRegion, C::RegionUnsupported)
    } else if matches!(status, 403 | 451)
        || lower.contains("access denied")
        || lower.contains("blocked by network security")
        // These are actual interstitials, including HTTP 200 responses. Merely loading a
        // CAPTCHA library in a normal service page does not indicate a challenge.
        || lower.contains("<title>just a moment")
        || lower.contains("window._cf_chl_opt")
        || (service == S::Tiktok
            && field(&body, "slardarClient")
                .is_some_and(|v| v.as_str() == Some("SlardarWAF")))
        || reddit_login_block
        || tiktok_company_redirect
    {
        (U::Restricted, C::AccessDenied)
    } else if !(200..300).contains(&status) {
        (U::Failed, C::UnrecognizedResponse)
    } else {
        let available = match service {
            S::Chatgpt => serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .is_some_and(|v| {
                    [
                        "cookie_consent_required",
                        "is_gdpr_region",
                        "requires_cookie_consent",
                    ]
                    .iter()
                    .any(|key| v.get(key).and_then(|v| v.as_bool()).is_some())
                }),
            S::Claude => {
                lower.contains("claude")
                    && (lower.contains("log in")
                        || lower.contains("login")
                        || lower.contains("continue with"))
            }
            S::Gemini => {
                body.contains("45631641,null,true")
                    || field(&body, "45631641").and_then(|v| v.as_bool()) == Some(true)
            }
            S::DisneyPlus => {
                field(&body, "inSupportedLocation").and_then(|v| v.as_bool()) == Some(true)
            }
            S::YoutubePremium => body.contains("SPunlimited") || lower.contains("ad-free"),
            S::Tiktok => {
                region.is_some()
                    && (body.contains("__UNIVERSAL_DATA_FOR_REHYDRATION__")
                        || body.contains("SIGI_STATE"))
            }
            S::PrimeVideo => region.is_some(),
            S::Reddit => {
                lower.contains("reddit")
                    && (html_tags(&reddit_html, "shreddit-app").next().is_some()
                        || reddit_js_continuation)
            }
            S::Netflix => false,
            S::Spotify
            | S::HboMax
            | S::Dazn
            | S::BbcIplayer
            | S::Crunchyroll
            | S::ParamountPlus => false,
        };
        if available {
            (U::Available, C::WebAvailable)
        } else {
            (U::Failed, C::UnrecognizedResponse)
        }
    };
    result(service, state, code, region)
}

fn netflix(deadline: Instant, proxy: Option<&ProbeContext>) -> Result<UnlockResult, UnlockCode> {
    netflix_with_check(|url| get(url, deadline, proxy))
}

fn netflix_with_check(
    mut check: impl FnMut(&str) -> Result<Page, UnlockCode>,
) -> Result<UnlockResult, UnlockCode> {
    use UnlockCode as C;
    use UnlockStatus as U;
    let service = UnlockService::Netflix;
    // Each title is independent evidence. A connection failure for one title must not
    // discard an actual title page from the other. Both requests keep the same deadline.
    let first = check("https://www.netflix.com/title/81280792");
    let second = check("https://www.netflix.com/title/70143836");
    let pages = [first.as_ref().ok(), second.as_ref().ok()];
    let region = pages
        .into_iter()
        .flatten()
        .map(|page| page.url.path())
        .find_map(|path| {
            let segment = path.trim_start_matches('/').split('/').next()?;
            country(segment.split('-').next()?)
        });
    let statuses = pages.map(|page| page.map(|page| page.response.status));
    let (status, code) = if matches!(
        statuses,
        [Some(403 | 451), Some(_)] | [Some(_), Some(403 | 451)]
    ) {
        (U::Restricted, C::AccessDenied)
    } else if pages.into_iter().flatten().any(|page| {
        page.response.status == 200
            && page.response.body.contains("netflix")
            && (page.response.body.contains("og:video")
                || page.response.body.contains("data-uia=\"title-info")
                || page.response.body.contains("\"titleId\""))
            && !page.response.body.contains("cf-chl-")
    }) {
        (U::Available, C::WebAvailable)
    } else {
        // Without positive title evidence, retain transport failures. One missing title
        // plus a failed request cannot establish an originals-only catalogue.
        first?;
        second?;
        if statuses == [Some(404), Some(404)] {
            (U::Partial, C::OriginalsOnly)
        } else {
            (U::Failed, C::UnrecognizedResponse)
        }
    };
    Ok(result(service, status, code, region))
}

#[cfg(test)]
mod tests {
    use super::*;
    use brocade_deployment::unlock_probe::UNLOCK_SERVICES;
    fn netflix_page(status: u16, body: &str) -> Page {
        let wire = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        Page {
            response: crate::http::tests::response_from_wire(wire.as_bytes()).unwrap(),
            url: reqwest::Url::parse("https://www.netflix.com/hk-en/title/81280792").unwrap(),
        }
    }

    #[test]
    fn netflix_keeps_a_confirmed_title_when_either_request_fails() {
        for failure in [UnlockCode::NetworkError, UnlockCode::Timeout] {
            for failed_call in [0, 1] {
                let mut calls = 0;
                let outcome = netflix_with_check(|_| {
                    let call = calls;
                    calls += 1;
                    if call == failed_call {
                        Err(failure)
                    } else {
                        Ok(netflix_page(200, r#"netflix <meta property="og:video">"#))
                    }
                })
                .unwrap();
                assert_eq!(calls, 2);
                assert_eq!(outcome.status, UnlockStatus::Available);
                assert_eq!(outcome.code, UnlockCode::WebAvailable);
                assert_eq!(outcome.region.as_deref(), Some("HK"));
                assert!(outcome.valid());
            }
        }
    }

    #[test]
    fn netflix_does_not_turn_missing_titles_or_challenges_into_positive_evidence() {
        for (status, body) in [
            (404, "netflix title missing"),
            (200, "netflix sign in"),
            (
                200,
                r#"netflix og:video <script src="cf-chl-script"></script>"#,
            ),
        ] {
            let mut calls = 0;
            let outcome = netflix_with_check(|_| {
                calls += 1;
                if calls == 1 {
                    Ok(netflix_page(status, body))
                } else {
                    Err(UnlockCode::NetworkError)
                }
            });
            assert_eq!(outcome.unwrap_err(), UnlockCode::NetworkError);
        }
        assert_eq!(
            netflix_with_check(|_| Err(UnlockCode::Timeout)).unwrap_err(),
            UnlockCode::Timeout
        );
        let originals =
            netflix_with_check(|_| Ok(netflix_page(404, "netflix title missing"))).unwrap();
        assert_eq!(originals.status, UnlockStatus::Partial);
        assert_eq!(originals.code, UnlockCode::OriginalsOnly);
    }

    #[test]
    fn netflix_preserves_explicit_access_denial_when_both_pages_answer() {
        let mut calls = 0;
        let outcome = netflix_with_check(|_| {
            calls += 1;
            Ok(if calls == 1 {
                netflix_page(403, "Access denied")
            } else {
                netflix_page(200, "netflix og:video")
            })
        })
        .unwrap();
        assert_eq!(outcome.status, UnlockStatus::Restricted);
    }

    #[test]
    #[ignore = "contacts Netflix through an explicitly selected diagnostic SOCKS proxy"]
    fn live_netflix_transport() {
        let port: u16 = std::env::var("BROCADE_DIAGNOSTIC_SOCKS_PORT")
            .expect("select an explicit diagnostic SOCKS proxy")
            .parse()
            .unwrap();
        assert_ne!(port, 0);
        let context = ProbeContext {
            client: crate::http::diagnostic_socks_client(port).unwrap(),
            request_timeout: CHAIN_REQUEST_TIMEOUT,
            ipv6: false,
            trace: None,
            trace_expected: false,
            service: None,
            part: brocade_deployment::unlock_probe::UnlockPart::Service,
            requests: Default::default(),
        };
        let outcome = confirm_with(
            UnlockService::Netflix,
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + SERVICE_TIMEOUT,
            || {
                netflix(Instant::now() + SERVICE_TIMEOUT, Some(&context)).unwrap_or_else(|code| {
                    result(UnlockService::Netflix, UnlockStatus::Failed, code, None)
                })
            },
            || thread::sleep(Duration::from_millis(500)),
        );
        assert!(outcome.valid());
        assert_ne!(outcome.status, UnlockStatus::Failed);
        println!("{}", serde_json::to_string(&outcome).unwrap());
    }

    #[test]
    fn tiktok_explore_can_confirm_access_after_a_homepage_challenge() {
        let mut requested = Vec::new();
        let result = tiktok_with_check(|url| {
            requested.push(url.to_owned());
            let body = if url.ends_with("/explore") {
                r#"<script id="__UNIVERSAL_DATA_FOR_REHYDRATION__">{"__DEFAULT_SCOPE__":{"webapp.app-context":{"region":"MY"}}}</script>"#
            } else {
                r#"<script>{"slardarClient":"SlardarWAF"}</script>"#
            };
            Ok(parse(UnlockService::Tiktok, 200, body, url))
        }).unwrap();
        assert_eq!(
            requested,
            ["https://www.tiktok.com/", "https://www.tiktok.com/explore"]
        );
        assert_eq!(result.status, UnlockStatus::Available);
        assert_eq!(result.region.as_deref(), Some("MY"));
    }

    #[test]
    fn tiktok_fallback_retains_restrictions_and_stops_on_explicit_country_denial() {
        let result = tiktok_with_check(|url| {
            if url.ends_with("/explore") {
                return Err(UnlockCode::Timeout);
            }
            Ok(parse(UnlockService::Tiktok, 403, "Access denied", url))
        })
        .unwrap();
        assert_eq!(result.status, UnlockStatus::Restricted);
        let mut calls = 0;
        let denied = tiktok_with_check(|_| {
            calls += 1;
            Ok(parse(
                UnlockService::Tiktok,
                404,
                "Not found",
                "https://www.tiktok.com/hk/notfound/",
            ))
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(denied.status, UnlockStatus::UnsupportedRegion);
        assert_eq!(denied.region.as_deref(), Some("HK"));
        assert!(tiktok_with_check(|_| Err(UnlockCode::NetworkError)).is_err());
    }

    #[test]
    fn tiktok_legacy_sigi_uses_viewer_region_and_ignores_author_or_datacenter_regions() {
        let body = r#"<script id="SIGI_STATE">{"region":"ALISG","AppContext":{"appContext":{"region":"MY","description":"Watch \"videos\""}},"ItemModule":{"video":{"region":"US"}}}</script>"#;
        let outcome = parse(
            UnlockService::Tiktok,
            200,
            body,
            "https://www.tiktok.com/explore",
        );
        assert_eq!(outcome.status, UnlockStatus::Available);
        assert_eq!(outcome.region.as_deref(), Some("MY"));
        let author_only =
            r#"<script id="SIGI_STATE">{"ItemModule":{"video":{"region":"US"}}}</script>"#;
        assert!(parse(UnlockService::Tiktok, 200, author_only, "")
            .region
            .is_none());
        let mut calls = 0;
        let result = tiktok_with_check(|_| {
            calls += 1;
            Ok(outcome.clone())
        })
        .unwrap();
        assert_eq!(calls, 1, "a confirmed application response needs no retry");
        assert_eq!(result.region.as_deref(), Some("MY"));
    }

    #[test]
    fn reddit_checks_actual_content_and_reads_country_only_from_application_attributes() {
        let page = r#"<script>{"country":"US"}</script><shreddit-app data-country="FR" aria-label="a > b" country = 'JP'><shreddit-post></shreddit-post></shreddit-app>"#;
        let outcome = parse(UnlockService::Reddit, 200, page, "https://www.reddit.com/");
        assert_eq!(outcome.status, UnlockStatus::Available);
        assert_eq!(outcome.region.as_deref(), Some("JP"));
        let quoted = r#"<shreddit-app aria-label="country=&quot;US&quot; > examples" country="JP"></shreddit-app>"#;
        assert_eq!(
            parse(UnlockService::Reddit, 200, quoted, "")
                .region
                .as_deref(),
            Some("JP")
        );
        let assets_only = "<title>Reddit</title><script src='/shreddit-app.js'></script>";
        assert_eq!(
            parse(UnlockService::Reddit, 200, assets_only, "").status,
            UnlockStatus::Failed
        );
        for name in [
            "name = 'js_challenge'",
            "name=js_challenge",
            "name=\"js_challenge\"",
        ] {
            let challenge = format!(
                "<title>Reddit</title><input name=solution value='x'><input {name} value='x'><shreddit-app country=JP></shreddit-app>"
            );
            let outcome = parse(UnlockService::Reddit, 200, &challenge, "");
            assert_eq!(outcome.status, UnlockStatus::Available);
            assert_eq!(outcome.code, UnlockCode::WebAvailable);
            assert_eq!(outcome.region.as_deref(), Some("JP"));
        }
        assert!(parse(
            UnlockService::Reddit,
            200,
            "<shreddit-app></shreddit-app>",
            ""
        )
        .region
        .is_none());
    }
    #[test]
    fn reddit_region_uses_the_delivery_cache_after_shields_and_repeated_headers() {
        let body = r#"<title>Reddit</title><shreddit-app country="MO"></shreddit-app>"#;
        for headers in [
            "X-Served-By: cache-iad-kcgs7200156-IAD, cache-hkg-etou8220071-HKG\r\n",
            "X-Served-By: cache-iad2120-IAD\r\nx-served-by: cache-hkg123-HKG\r\n",
            "X-Served-By: cache-hkg123-hkg\r\n",
        ] {
            let wire = format!(
                "HTTP/1.1 200 OK\r\n{headers}Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let page = Page {
                response: crate::http::tests::response_from_wire(wire.as_bytes()).unwrap(),
                url: reqwest::Url::parse("https://www.reddit.com/").unwrap(),
            };
            let outcome = reddit(&page);
            assert_eq!(outcome.region.as_deref(), Some("HKG"));
            assert_eq!(outcome.status, UnlockStatus::Available);
            assert!(outcome.valid());
        }
        for header in [
            "",
            "X-Served-By: cache-hkg123-HKG, unknown\r\n",
            "X-Served-By: cache-HKG\r\n",
            "X-Served-By: cache-good-<script>\r\n",
            "X-Served-By: cache-good-HK2\r\n",
            "X-Served-By: cache-good-HKG,\r\n",
        ] {
            let wire = format!(
                "HTTP/1.1 200 OK\r\n{header}Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let page = Page {
                response: crate::http::tests::response_from_wire(wire.as_bytes()).unwrap(),
                url: reqwest::Url::parse("https://www.reddit.com/").unwrap(),
            };
            assert_eq!(reddit(&page).region.as_deref(), Some("MO"));
        }
    }
    #[test]
    fn reddit_debug_path_uses_only_the_first_delivery_cache_and_served_by_takes_priority() {
        assert_eq!(
            page_headers(&reqwest::Url::parse("https://www.reddit.com/").unwrap()),
            vec![("Fastly-Debug", "1".into())]
        );
        for (headers, expected) in [
            ("Fastly-Debug-Path: (D cache-hkg123-HKG 1700000000) (F cache-iad123-IAD 1600000000)\r\n", "HKG"),
            ("X-Served-By: cache-lhr123-LHR\r\nFastly-Debug-Path: (D cache-hkg123-HKG 1700000000)\r\n", "LHR"),
            ("Fastly-Debug-Path: (F cache-hkg123-HKG 1700000000) (D cache-iad123-IAD 1600000000)\r\n", "MO"),
            ("Fastly-Debug-Path: (D unknown 1700000000) (D cache-iad123-IAD 1600000000)\r\n", "MO"),
            ("Fastly-Debug-Path: (D cache-hkg123-HKG not-a-timestamp)\r\n", "MO"),
        ] {
            let body = r#"<title>Reddit</title><shreddit-app country="MO"></shreddit-app>"#;
            let wire = format!("HTTP/1.1 200 OK\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len());
            let page = Page {
                response: crate::http::tests::response_from_wire(wire.as_bytes()).unwrap(),
                url: reqwest::Url::parse("https://www.reddit.com/").unwrap(),
            };
            assert_eq!(reddit(&page).region.as_deref(), Some(expected));
        }
    }
    #[test]
    #[ignore = "contacts Reddit using the Agent HTTP transport; metadata only"]
    fn live_reddit_region_transport() {
        let outcome = check(
            UnlockService::Reddit,
            Instant::now() + SERVICE_TIMEOUT,
            None,
        )
        .unwrap();
        println!("{}", serde_json::to_string(&outcome).unwrap());
        assert!(outcome.valid());
    }
    #[test]
    fn reddit_cache_region_does_not_change_availability_or_turn_blocks_into_success() {
        for (status, body, expected) in [
            (
                200,
                r#"<title>Reddit</title><input name="js_challenge">"#,
                UnlockStatus::Available,
            ),
            (
                403,
                "You've been blocked by network security.",
                UnlockStatus::Restricted,
            ),
            (200, "unrecognized page", UnlockStatus::Failed),
        ] {
            let wire = format!(
                "HTTP/1.1 {status} Fixture\r\nX-Served-By: cache-hkg123-HKG\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let page = Page {
                response: crate::http::tests::response_from_wire(wire.as_bytes()).unwrap(),
                url: reqwest::Url::parse("https://www.reddit.com/").unwrap(),
            };
            let outcome = reddit(&page);
            assert_eq!(outcome.status, expected);
            assert_eq!(outcome.region.as_deref(), Some("HKG"));
            assert!(outcome.valid());
        }
    }
    #[test]
    fn reddit_anonymous_continuation_is_available_but_login_blocks_remain_restricted() {
        let continuation = r#"<title>Reddit</title><form><input type="hidden" name="js_challenge" value="1"/></form>"#;
        let outcome = parse(UnlockService::Reddit, 200, continuation, "");
        assert_eq!(outcome.status, UnlockStatus::Available);
        assert!(
            outcome.region.is_none(),
            "a continuation supplies no visitor country"
        );
        for status in [403, 451] {
            assert_eq!(
                parse(UnlockService::Reddit, status, continuation, "").status,
                UnlockStatus::Restricted
            );
        }
        for block in [
            "You've been blocked by network security. To continue, log in to your Reddit account or use your developer token.",
            "Your request has been blocked. Please try to login with your Reddit account.",
            "Whoa there, pardner! Log in to continue.",
            "To continue, log in to your Reddit account or use your developer token.",
        ] {
            let body = format!("<title>Reddit</title><main>{block}</main>{continuation}");
            let outcome = parse(UnlockService::Reddit, 200, &body, "");
            assert_eq!(outcome.status, UnlockStatus::Restricted, "{block}");
            assert_eq!(outcome.code, UnlockCode::AccessDenied);
        }
        let anonymous_page = r#"<title>Reddit</title><shreddit-app country="MO"><a href="/login/">Log in</a><shreddit-post></shreddit-post></shreddit-app>"#;
        let outcome = parse(UnlockService::Reddit, 200, anonymous_page, "");
        assert_eq!(
            outcome.status,
            UnlockStatus::Available,
            "an optional login button is not a gate"
        );
        assert_eq!(outcome.region.as_deref(), Some("MO"));
    }
    #[test]
    #[ignore = "contacts YouTube Premium using the Agent HTTP transport; metadata only"]
    fn live_youtube_transport() {
        let target = std::env::var_os("BROCADE_DIAGNOSTIC_CHAIN_TARGET").map(|path| {
            serde_json::from_slice::<brocade_deployment::protocol::E2eProbeTarget>(
                &std::fs::read(path).unwrap(),
            )
            .unwrap()
        });
        let session = target.as_ref().map(|target| {
            brocade_probe::start_chain_client(
                target,
                &brocade_probe::ProbeOptions::new(
                    crate::options::xray_binary_path(),
                    brocade_probe::ProbeCancellation::default(),
                ),
            )
            .unwrap()
        });
        let external_port = std::env::var("BROCADE_DIAGNOSTIC_SOCKS_PORT")
            .ok()
            .map(|port| {
                let port: u16 = port.parse().expect("diagnostic SOCKS port must be a u16");
                assert_ne!(port, 0);
                port
            });
        assert!(
            !(external_port.is_some() && target.is_some()),
            "select one diagnostic proxy source"
        );
        let clients = external_port
            .or_else(|| session.as_ref().map(|session| session.socks_port()))
            .map(|port| crate::http::diagnostic_socks_client(port).unwrap());
        let context = clients.map(|client| ProbeContext {
            client,
            request_timeout: CHAIN_REQUEST_TIMEOUT,
            ipv6: false,
            trace: None,
            trace_expected: false,
            service: None,
            part: brocade_deployment::unlock_probe::UnlockPart::Service,
            requests: Default::default(),
        });
        let service = UnlockService::YoutubePremium;
        let outcome = check(service, Instant::now() + SERVICE_TIMEOUT, context.as_ref())
            .unwrap_or_else(|code| result(service, UnlockStatus::Failed, code, None));
        assert!(outcome.valid());
        println!("{}", serde_json::to_string(&outcome).unwrap());
    }

    #[test]
    #[ignore = "contacts TikTok and Reddit through the production detection flow"]
    fn live_tiktok_reddit_checks() {
        for service in [UnlockService::Tiktok, UnlockService::Reddit] {
            let outcome = check(service, Instant::now() + SERVICE_TIMEOUT, None)
                .unwrap_or_else(|code| result(service, UnlockStatus::Failed, code, None));
            assert!(outcome.valid());
            println!("{}", serde_json::to_string(&outcome).unwrap());
        }
    }
    #[test]
    fn current_claude_region_redirect_stays_within_fixed_service_hosts() {
        let url = "https://claude.com/app-unavailable-in-region";
        assert!(safe_url(&reqwest::Url::parse(url).unwrap()));
        assert_eq!(
            parse(UnlockService::Claude, 200, "App unavailable in region", url).status,
            UnlockStatus::UnsupportedRegion
        );
        assert!(!safe_url(
            &reqwest::Url::parse("https://claude.com.evil.test/app-unavailable-in-region").unwrap()
        ));
    }

    #[test]
    fn actual_browser_challenges_are_access_restrictions_even_with_http_200() {
        for (service, body) in [
            (
                UnlockService::Claude,
                "<title>Just a moment...</title><script src=\"/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1\"></script>",
            ),
            (
                UnlockService::Tiktok,
                r#"<script id="slardar-config" type="application/json">{"slardarClient":"SlardarWAF"}</script><body>Please wait...</body>"#,
            ),
        ] {
            let outcome = parse(service, 200, body, "");
            assert_eq!(outcome.status, UnlockStatus::Restricted, "{service:?}");
            assert_eq!(outcome.code, UnlockCode::AccessDenied);
            assert!(outcome.region.is_none());
        }
    }

    #[test]
    fn positive_evidence_and_unknown_pages_are_distinct() {
        for service in UNLOCK_SERVICES {
            assert_eq!(
                parse(
                    service,
                    200,
                    "<html>Sign in</html>",
                    "https://example.test/"
                )
                .status,
                UnlockStatus::Failed
            );
            assert_eq!(
                parse(service, 429, "rate limited", "https://example.test/").status,
                UnlockStatus::Failed
            );
        }
        let prime = parse(
            UnlockService::PrimeVideo,
            200,
            r#"{"currentTerritory":"HK","isServiceRestricted":false}"#,
            "",
        );
        assert_eq!(prime.status, UnlockStatus::Available);
        assert_eq!(prime.region.as_deref(), Some("HK"));
        assert_eq!(
            parse(
                UnlockService::PrimeVideo,
                200,
                r#"{"currentTerritory":"HK","isServiceRestricted":true}"#,
                ""
            )
            .status,
            UnlockStatus::UnsupportedRegion
        );
        assert_eq!(
            parse(
                UnlockService::Chatgpt,
                403,
                r#"{"error":{"code":"unsupported_country_region_territory"}}"#,
                ""
            )
            .status,
            UnlockStatus::UnsupportedRegion
        );
        assert_eq!(
            parse(
                UnlockService::Claude,
                200,
                "Claude",
                "https://www.anthropic.com/app-unavailable-in-region"
            )
            .status,
            UnlockStatus::UnsupportedRegion
        );
        let gemini = parse(
            UnlockService::Gemini,
            200,
            r#",2,1,200,"HKG" 45631641,null,false"#,
            "",
        );
        assert_eq!(gemini.region.as_deref(), Some("HK"));
        assert_eq!(gemini.status, UnlockStatus::UnsupportedRegion);
        assert_eq!(
            parse(
                UnlockService::YoutubePremium,
                200,
                r#"{"GL":"HK","browseId":"SPunlimited"}"#,
                ""
            )
            .status,
            UnlockStatus::Available
        );
        assert_eq!(
            parse(
                UnlockService::Tiktok,
                200,
                r#"__UNIVERSAL_DATA_FOR_REHYDRATION__ {"__DEFAULT_SCOPE__":{"webapp.app-context":{"region":"SG"}}}"#,
                ""
            )
            .status,
            UnlockStatus::Available
        );
        assert_eq!(
            parse(
                UnlockService::DisneyPlus,
                200,
                r#"{"countryCode":"HK","inSupportedLocation":true}"#,
                ""
            )
            .status,
            UnlockStatus::Available
        );
        assert_eq!(
            parse(
                UnlockService::Reddit,
                403,
                "blocked by network security",
                ""
            )
            .status,
            UnlockStatus::Restricted
        );
    }
    #[test]
    fn sanitized_live_response_shapes_do_not_depend_on_obsolete_fields_or_captcha_libraries() {
        // Reduced from 2026-10-25 public responses; no visitor IDs, tokens or page content.
        assert_eq!(
            parse(
                UnlockService::Chatgpt,
                200,
                r#"{"cookie_consent_required":false}"#,
                ""
            )
            .status,
            UnlockStatus::Available
        );
        let gemini = parse(
            UnlockService::Gemini,
            200,
            r#"[2,1,200,"TWN"] {\"45631641\":true}"#,
            "",
        );
        assert_eq!(gemini.status, UnlockStatus::Available);
        let youtube = parse(
            UnlockService::YoutubePremium,
            200,
            r#"{"GL":"JP","desktopTopbarRenderer":{"countryCode":"JP"},"browseId":"SPunlimited","RECAPTCHA_V3_SITEKEY":"PUBLIC_PLACEHOLDER"}"#,
            "",
        );
        assert_eq!(youtube.status, UnlockStatus::Available);
        assert_eq!(youtube.region.as_deref(), Some("JP"));
        let tiktok = parse(
            UnlockService::Tiktok,
            200,
            r#"__UNIVERSAL_DATA_FOR_REHYDRATION__ {"region":"ALISG"}"#,
            "",
        );
        assert_eq!(tiktok.status, UnlockStatus::Failed);
        assert!(tiktok.region.is_none());
        assert_eq!(
            parse(
                UnlockService::DisneyPlus,
                200,
                r#"{"region":"JP"} Subscribe now"#,
                ""
            )
            .status,
            UnlockStatus::Failed
        );
        assert_eq!(
            parse(
                UnlockService::YoutubePremium,
                200,
                "<title>Just a moment</title> SPunlimited",
                ""
            )
            .status,
            UnlockStatus::Restricted
        );
    }

    #[test]
    fn tiktok_region_comes_from_viewer_context_not_datacenter_or_video_author() {
        let page = r#"<script id="tiktok-environment">{"region":"ALISG"}</script>
            <script id="__UNIVERSAL_DATA_FOR_REHYDRATION__">
            {"__DEFAULT_SCOPE__":{"webapp.video-detail":{"author":{"region":"JP"}},
                "webapp.app-context":{"region":"US"}}}</script>"#;
        let outcome = parse(UnlockService::Tiktok, 200, page, "");
        assert_eq!(outcome.status, UnlockStatus::Available);
        assert_eq!(outcome.region.as_deref(), Some("US"));

        let shell = r#"<script id="tiktok-environment">{"region":"SG"}</script>
            <script id="__UNIVERSAL_DATA_FOR_REHYDRATION__">
            {"__DEFAULT_SCOPE__":{"webapp.video-detail":{"author":{"region":"JP"}}}}</script>"#;
        let outcome = parse(UnlockService::Tiktok, 200, shell, "");
        assert_eq!(outcome.status, UnlockStatus::Failed);
        assert!(outcome.region.is_none());
    }

    #[test]
    fn tiktok_viewer_context_keeps_json_string_escaping_intact() {
        let page = r#"<script id="__UNIVERSAL_DATA_FOR_REHYDRATION__">
            {"__DEFAULT_SCOPE__":{"webapp.app-context":{"region":"US",
                "description":"Watch \"example\" videos", "config":"{\"nested\":true}"}}}</script>"#;
        let outcome = parse(UnlockService::Tiktok, 200, page, "");
        assert_eq!(outcome.status, UnlockStatus::Available);
        assert_eq!(outcome.region.as_deref(), Some("US"));
    }

    #[test]
    fn tiktok_company_redirect_does_not_claim_application_access_or_cdn_country() {
        let page = r#"<title>About | TikTok</title><script id="tiktok-environment">{"region":"SG"}</script>"#;
        let outcome = parse(
            UnlockService::Tiktok,
            200,
            page,
            "https://www.tiktok.com/about",
        );
        assert_eq!(outcome.status, UnlockStatus::Restricted);
        assert_eq!(outcome.code, UnlockCode::AccessDenied);
        assert!(outcome.region.is_none());
        assert_eq!(
            parse(UnlockService::Tiktok, 200, page, "https://www.tiktok.com/").status,
            UnlockStatus::Failed
        );
    }

    #[test]
    fn passive_cloudflare_javascript_does_not_reject_a_real_login_page() {
        let page = r#"<title>Claude</title>Log in to Claude. Continue with email.
            <script src="/cdn-cgi/challenge-platform/scripts/jsd/main.js"></script>"#;
        assert_eq!(
            parse(UnlockService::Claude, 200, page, "https://claude.ai/login").status,
            UnlockStatus::Available
        );
        let challenge = r#"<script>window._cf_chl_opt={};</script>Claude login"#;
        assert_eq!(
            parse(
                UnlockService::Claude,
                200,
                challenge,
                "https://claude.ai/login"
            )
            .status,
            UnlockStatus::Restricted
        );
    }

    #[test]
    #[ignore = "contacts public services; prints only sanitized response metadata"]
    fn live_response_metadata_for_diagnostic_failures() {
        for (service, url) in [
            (UnlockService::Claude, "https://claude.ai/"),
            (UnlockService::Tiktok, "https://www.tiktok.com/"),
            (UnlockService::Tiktok, "https://www.tiktok.com/foryou"),
            (UnlockService::Tiktok, "https://www.tiktok.com/explore"),
            (UnlockService::Reddit, "https://www.reddit.com/"),
        ] {
            let deadline = Instant::now() + SERVICE_TIMEOUT;
            match get(url, deadline, None) {
                Ok(page) => {
                    let raw_scope = field(&page.response.body, "__DEFAULT_SCOPE__");
                    let outcome = parse(
                        service,
                        page.response.status,
                        &page.response.body,
                        page.url.as_str(),
                    );
                    println!(
                        "{}",
                        serde_json::json!({
                            "service":service,"status":page.response.status,"host":page.url.host_str(),
                            "bytes":page.response.body.len(),"result":outcome,
                            "raw_viewer_region":raw_scope.and_then(|v| v.get("webapp.app-context")?.get("region")?.as_str().and_then(country)),
                            "slardar_waf":field(&page.response.body,"slardarClient").is_some_and(|v|v.as_str()==Some("SlardarWAF")),
                            "cloudflare_passive_js":page.response.body.contains("/scripts/jsd/"),
                            "cloudflare_active_challenge":page.response.body.contains("window._cf_chl_opt") || page.response.body.to_ascii_lowercase().contains("<title>just a moment"),
                            "claude_login_evidence":service==UnlockService::Claude && page.response.body.to_ascii_lowercase().contains("claude") && (page.response.body.to_ascii_lowercase().contains("login") || page.response.body.to_ascii_lowercase().contains("continue with")),
                            "has_waf_challenge":page.response.body.contains("_wafchallengeid"),
                            "has_universal_data":page.response.body.contains("__UNIVERSAL_DATA_FOR_REHYDRATION__"),
                            "has_sigi_state":page.response.body.contains("SIGI_STATE"),
                            "legacy_viewer_region":field(&page.response.body,"AppContext").and_then(|v| v.get("appContext")?.get("region")?.as_str().and_then(country)),
                            "has_unavailable_text":page.response.body.to_ascii_lowercase().contains("not available"),
                            "has_captcha_interstitial":page.response.body.contains("secsdk-captcha"),
                            "requested_feed":url.ends_with("/foryou"),
                            "redirected_to_about":page.url.path()=="/about"
                        })
                    );
                    if url == "https://www.tiktok.com/" {
                        if let Some(path) = std::env::var_os("BROCADE_DIAGNOSTIC_PRIVATE_BODY") {
                            use std::io::Write;
                            use std::os::unix::fs::OpenOptionsExt;
                            let mut file = std::fs::OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .mode(0o600)
                                .open(path)
                                .unwrap();
                            file.write_all(page.response.body.as_bytes()).unwrap();
                        }
                    }
                }
                Err(code) => println!("{}", serde_json::json!({"service":service,"code":code})),
            }
        }
    }
    #[test]
    fn upload_retry_reuses_results_and_never_repeats_service_requests() {
        for (family, services) in [
            (None, None),
            (Some(brocade_core::model::IpFamily::V4), None),
            (
                Some(brocade_core::model::IpFamily::V4),
                Some(UNLOCK_SERVICES.to_vec()),
            ),
        ] {
            use std::{
                io::{Read, Write},
                net::TcpListener,
            };
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let mut reports = Vec::new();
                for step in 0..3 {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(30)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut byte = [0];
                    while !bytes.ends_with(b"\r\n\r\n") {
                        socket.read_exact(&mut byte).unwrap();
                        bytes.push(byte[0]);
                    }
                    let headers = String::from_utf8(bytes).unwrap();
                    assert!(headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer test-only"));
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    let mut body = vec![0; length];
                    socket.read_exact(&mut body).unwrap();
                    let (status, response) = if step == 0 {
                        assert!(headers.starts_with("GET /agent/v1/unlock-probe/target "));
                        (200, serde_json::json!({"probe_id":"0123456789abcdef0123456789abcdef", "family":family, "services":services}).to_string())
                    } else {
                        assert!(headers.starts_with("POST /agent/v1/unlock-probe/report "));
                        reports.push(serde_json::from_slice::<UnlockProbeReport>(&body).unwrap());
                        (if step == 1 { 503 } else { 204 }, String::new())
                    };
                    write!(socket, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
                }
                assert_eq!(reports[0], reports[1]);
                assert!(reports[0].valid());
                assert_eq!(
                    reports[0].results.len(),
                    services.as_ref().map_or(9, Vec::len)
                );
            });
            let options = Options {
                command: "run".into(),
                server: format!("http://{address}"),
                token: "test-only".into(),
                state_dir: "/nonexistent-unlock-state".into(),
                apply_mode: crate::options::ApplyMode::StateDir,
                vpngate_stats_window: Duration::from_secs(900),
            };
            let mut pending = None;
            assert!(round(&options, &mut pending, |target| (
                target
                    .services()
                    .iter()
                    .copied()
                    .map(|service| result(service, UnlockStatus::Failed, UnlockCode::Timeout, None))
                    .collect(),
                None
            ))
            .is_err());
            assert!(pending.is_some());
            if family.is_some() {
                pending.as_mut().unwrap().created = Instant::now() - Duration::from_secs(120);
            }
            assert!(round(&options, &mut pending, |_| panic!(
                "upload retry must not repeat detection"
            ))
            .unwrap());
            assert!(pending.is_none());
            server.join().unwrap();
        }
    }
    #[test]
    #[ignore = "contacts real public services; run explicitly for current-egress evidence"]
    fn live_services_report_current_egress_without_logging_response_bodies() {
        let results = detect(None, None, false, &UNLOCK_SERVICES);
        assert_eq!(results.len(), UNLOCK_SERVICES.len());
        assert!(results.iter().all(UnlockResult::valid));
        // This checks transport and records observations, not universal unlock correctness.
        for result in results {
            println!("{}", serde_json::to_string(&result).unwrap());
        }
    }

    #[test]
    fn redirects_cannot_reach_credentials_private_hosts_or_plaintext() {
        for url in [
            "http://www.netflix.com/",
            "https://127.0.0.1/",
            "https://www.netflix.com.evil.test/",
            "https://secret@claude.ai/",
            "https://claude.ai:8443/",
        ] {
            assert!(!safe_url(&reqwest::Url::parse(url).unwrap()));
        }
        assert!(safe_url(
            &reqwest::Url::parse("https://claude.ai/login").unwrap()
        ));
        assert_eq!(country("unknown"), None);
        assert_eq!(region(r#"{"region":"not-a-country"}"#, &["region"]), None);
    }
}

#[cfg(test)]
mod proxy_tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    #[test]
    fn traced_diagnostic_keeps_chain_resolution_and_waits_for_dns_evidence() {
        delayed_chain_request(true);
    }

    #[test]
    fn untraced_chain_waits_for_the_complete_slow_response_body() {
        delayed_chain_request(false);
    }

    #[test]
    fn direct_family_requests_keep_the_short_budget_and_service_deadline() {
        for ipv6 in [false, true] {
            let context = ProbeContext {
                client: reqwest::blocking::Client::new(),
                request_timeout: REQUEST_TIMEOUT,
                ipv6,
                trace: None,
                trace_expected: false,
                service: None,
                part: brocade_deployment::unlock_probe::UnlockPart::Service,
                requests: Default::default(),
            };
            let now = Instant::now();
            let deadline = request_deadline(now + SERVICE_TIMEOUT, Some(&context));
            assert!(deadline >= now + Duration::from_secs(3));
            assert!(deadline < now + Duration::from_secs(4));
            let service_deadline = now + Duration::from_millis(100);
            assert_eq!(
                request_deadline(service_deadline, Some(&context)),
                service_deadline
            );
            assert_eq!(request_deadline(now, Some(&context)), now);
        }
    }

    fn delayed_chain_request(trace_expected: bool) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut greeting = [0; 2];
            socket.read_exact(&mut greeting).unwrap();
            assert_eq!(greeting[0], 5);
            let mut methods = vec![0; greeting[1] as usize];
            socket.read_exact(&mut methods).unwrap();
            socket.write_all(&[5, 0]).unwrap();
            let mut request = [0; 5];
            socket.read_exact(&mut request).unwrap();
            assert_eq!(&request[..4], &[5, 1, 0, 3]);
            let mut host = vec![0; request[4] as usize];
            socket.read_exact(&mut host).unwrap();
            assert_eq!(host, b"only-via-chain.invalid");
            let mut remote_port = [0; 2];
            socket.read_exact(&mut remote_port).unwrap();
            assert_eq!(remote_port, 80u16.to_be_bytes());
            if trace_expected {
                // Remote DNS/evidence may exceed the direct request budget.
                thread::sleep(Duration::from_millis(3200));
            }
            socket
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
                .unwrap();
            let mut bytes = Vec::new();
            let mut byte = [0];
            while !bytes.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
            }
            let request = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
            assert!(request.starts_with("get /probe "));
            assert!(!request.contains("authorization:"));
            assert!(!request.contains("x-brocade-protocol"));
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nloc=")
                .unwrap();
            if !trace_expected {
                // An automatic chain can return HTTP 200 before its body is complete.
                thread::sleep(Duration::from_millis(3200));
            }
            socket.write_all(b"JP\n").unwrap();
        });
        let context = ProbeContext {
            client: crate::http::diagnostic_socks_client(port).unwrap(),
            request_timeout: CHAIN_REQUEST_TIMEOUT,
            ipv6: false,
            trace: None,
            trace_expected,
            service: None,
            part: brocade_deployment::unlock_probe::UnlockPart::Service,
            requests: Default::default(),
        };
        let service_deadline = Instant::now() + SERVICE_TIMEOUT;
        let deadline = request_deadline(service_deadline, Some(&context));
        assert!(deadline > Instant::now() + Duration::from_secs(4));
        let short_service_deadline = Instant::now() + Duration::from_millis(100);
        assert_eq!(
            request_deadline(short_service_deadline, Some(&context)),
            short_service_deadline
        );
        assert!(request_deadline(service_deadline, None) <= Instant::now() + REQUEST_TIMEOUT);
        let response = HttpClient::new("http://only-via-chain.invalid")
            .unwrap()
            .with_diagnostic_proxy(Some(&context.client))
            .request_public_diagnostic(
                "GET",
                "/probe",
                None,
                "application/json",
                &[],
                deadline.saturating_duration_since(Instant::now()),
            )
            .unwrap();
        assert_eq!(response.body, "loc=JP\n");
        server.join().unwrap();
    }
    #[test]
    fn a_failed_proxy_never_falls_back_to_direct_even_for_loopback() {
        let socks = TcpListener::bind("127.0.0.1:0").unwrap();
        let socks_port = socks.local_addr().unwrap().port();
        drop(socks);
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = target.local_addr().unwrap();
        target.set_nonblocking(true).unwrap();
        let proxy = crate::http::diagnostic_socks_client(socks_port).unwrap();
        assert!(HttpClient::new(&format!("http://{address}"))
            .unwrap()
            .with_diagnostic_proxy(Some(&proxy))
            .request_public_diagnostic(
                "GET",
                "/",
                None,
                "application/json",
                &[],
                Duration::from_secs(1)
            )
            .is_err());
        assert!(
            target.accept().is_err(),
            "a direct connection would return the machine's unlock status"
        );
    }
}

#[cfg(test)]
mod retry_confirmation_tests {
    use super::*;
    #[test]
    fn unknown_results_get_initial_attempt_and_ten_retries() {
        let mut calls = 0;
        let mut waits = 0;
        let outcome = confirm_with(
            UnlockService::Reddit,
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + SERVICE_TIMEOUT,
            || {
                calls += 1;
                result(
                    UnlockService::Reddit,
                    UnlockStatus::Failed,
                    UnlockCode::Timeout,
                    None,
                )
            },
            || waits += 1,
        );
        assert_eq!(calls, MAX_SERVICE_ATTEMPTS);
        assert_eq!(waits, MAX_SERVICE_RETRIES);
        assert_eq!(outcome.status, UnlockStatus::Failed);
    }
    #[test]
    fn a_recovered_family_stops_immediately_without_borrowing_another_family() {
        let mut calls = 0;
        let outcome = confirm_with(
            UnlockService::Netflix,
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + SERVICE_TIMEOUT,
            || {
                calls += 1;
                if calls < 3 {
                    result(
                        UnlockService::Netflix,
                        UnlockStatus::Failed,
                        UnlockCode::NetworkError,
                        None,
                    )
                } else {
                    result(
                        UnlockService::Netflix,
                        UnlockStatus::Available,
                        UnlockCode::WebAvailable,
                        Some("JP".into()),
                    )
                }
            },
            || {},
        );
        assert_eq!(calls, 3);
        assert_eq!(outcome.region.as_deref(), Some("JP"));
        let mut other_calls = 0;
        let other = confirm_with(
            UnlockService::Netflix,
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + SERVICE_TIMEOUT,
            || {
                other_calls += 1;
                result(
                    UnlockService::Netflix,
                    UnlockStatus::Failed,
                    UnlockCode::NetworkError,
                    None,
                )
            },
            || {},
        );
        assert_eq!(other_calls, MAX_SERVICE_ATTEMPTS);
        assert!(other.region.is_none());
    }
    #[test]
    fn youtube_confirmation_retains_known_pages_when_only_cdn_was_unknown() {
        let mut first: UnlockResult = serde_json::from_value(serde_json::json!({
            "service":"youtube_premium", "status":"unsupported_region", "code":"region_unsupported", "region":"MO",
            "youtube": {
                "webpage":{"status":"available","code":"web_available","http_status":200,"page_region":"US","topbar_region":null,"privacy_region":"MO"},
                "premium":{"status":"unsupported_region","code":"region_unsupported","http_status":200,"page_region":"US","topbar_region":null,"privacy_region":"MO"},
                "cdn":{"http_status":null,"node":null,"airport":null,"country":null,"error":"timeout","elapsed_ms":0}
            }
        })).unwrap();
        assert!(first.valid());
        let mut calls = 0;
        let outcome = confirm_with(
            UnlockService::YoutubePremium,
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + SERVICE_TIMEOUT,
            || {
                calls += 1;
                if calls == 1 {
                    return first.clone();
                }
                let detail = first.youtube.as_mut().unwrap();
                detail.webpage.status = UnlockStatus::Failed;
                detail.webpage.code = UnlockCode::Timeout;
                detail.premium.status = UnlockStatus::Failed;
                detail.premium.code = UnlockCode::Timeout;
                first.status = UnlockStatus::Failed;
                first.code = UnlockCode::Timeout;
                if calls == 2 {
                    detail.cdn.node = Some("hkg12s15".into());
                    detail.cdn.airport = Some("HKG".into());
                    detail.cdn.country = Some("HK".into());
                    detail.cdn.http_status = Some(200);
                    detail.cdn.error = None;
                }
                first.clone()
            },
            || {},
        );
        assert_eq!(calls, 2);
        assert!(outcome.valid());
        assert_eq!(outcome.status, UnlockStatus::UnsupportedRegion);
        let detail = outcome.youtube.unwrap();
        assert_eq!(detail.webpage.status, UnlockStatus::Available);
        assert_eq!(detail.cdn.airport.as_deref(), Some("HKG"));
    }

    #[test]
    fn confirmed_anonymous_access_returns_immediately_even_without_country() {
        let mut calls = 0;
        let outcome = confirm_with(
            UnlockService::Reddit,
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + SERVICE_TIMEOUT,
            || {
                calls += 1;
                if calls == 1 {
                    result(
                        UnlockService::Reddit,
                        UnlockStatus::Available,
                        UnlockCode::WebAvailable,
                        None,
                    )
                } else {
                    result(
                        UnlockService::Reddit,
                        UnlockStatus::Failed,
                        UnlockCode::Timeout,
                        None,
                    )
                }
            },
            || {},
        );
        assert_eq!(calls, 1);
        assert_eq!(outcome.status, UnlockStatus::Available);
        assert!(outcome.region.is_none());
        let mut known = 0;
        confirm_with(
            UnlockService::Reddit,
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + SERVICE_TIMEOUT,
            || {
                known += 1;
                result(
                    UnlockService::Reddit,
                    UnlockStatus::Available,
                    UnlockCode::WebAvailable,
                    Some("HK".into()),
                )
            },
            || {},
        );
        assert_eq!(known, 1);
    }

    #[test]
    fn a_late_success_stops_at_the_first_recovered_attempt() {
        let mut calls = 0;
        let mut pauses = 0;
        let outcome = confirm_with(
            UnlockService::Netflix,
            usize::MAX,
            Instant::now() + SERVICE_TIMEOUT,
            || {
                calls += 1;
                assert!(
                    calls <= 10,
                    "a successful result must stop further requests"
                );
                if calls < 10 {
                    result(
                        UnlockService::Netflix,
                        UnlockStatus::Failed,
                        UnlockCode::NetworkError,
                        None,
                    )
                } else {
                    result(
                        UnlockService::Netflix,
                        UnlockStatus::Available,
                        UnlockCode::WebAvailable,
                        Some("HK".into()),
                    )
                }
            },
            || pauses += 1,
        );
        assert_eq!(calls, 10);
        assert_eq!(pauses, 9);
        assert_eq!(outcome.status, UnlockStatus::Available);
        assert_eq!(outcome.region.as_deref(), Some("HK"));
    }
}

#[cfg(test)]
mod ipv6_skip_tests {
    use super::*;
    #[test]
    fn final_http_dns_evidence_skips_only_a_confirmed_missing_ipv6_family() {
        let mut context = ProbeContext {
            client: reqwest::blocking::Client::new(),
            request_timeout: REQUEST_TIMEOUT,
            ipv6: true,
            trace: None,
            trace_expected: false,
            service: None,
            part: brocade_deployment::unlock_probe::UnlockPart::Service,
            requests: Default::default(),
        };
        let absent = "HTTP phase=send kind=dns_no_ipv6 status=unknown received_bytes=0";
        assert_eq!(
            request_error_code(absent, Some(&context)),
            UnlockCode::NoIpv6Dns
        );
        for error in [
            "HTTP phase=send kind=connect status=unknown",
            "HTTP phase=send kind=capacity status=unknown",
            "HTTP phase=send kind=dns_no_ipv6_other status=unknown",
        ] {
            assert_eq!(
                request_error_code(error, Some(&context)),
                UnlockCode::NetworkError
            );
        }
        assert_eq!(
            request_error_code("HTTP phase=send kind=timeout", Some(&context)),
            UnlockCode::Timeout
        );
        context.ipv6 = false;
        assert_eq!(
            request_error_code(absent, Some(&context)),
            UnlockCode::NetworkError
        );
        assert_eq!(request_error_code(absent, None), UnlockCode::NetworkError);
    }

    #[test]
    fn confirmed_no_aaaa_is_skipped_without_unknown_result_retries() {
        let mut calls = 0;
        let outcome = confirm_with(
            UnlockService::Reddit,
            4,
            Instant::now() + SERVICE_TIMEOUT,
            || {
                calls += 1;
                result(
                    UnlockService::Reddit,
                    UnlockStatus::Failed,
                    UnlockCode::NoIpv6Dns,
                    None,
                )
            },
            || panic!("known no-AAAA result must not retry"),
        );
        assert_eq!(calls, 1);
        assert_eq!(outcome.status, UnlockStatus::Skipped);
        assert!(outcome.valid());
    }
}

#[cfg(test)]
mod youtube_consent_tests {
    use super::*;
    #[test]
    fn anonymous_consent_is_youtube_only_and_never_sets_a_region_or_account() {
        for url in [
            "https://www.youtube.com/",
            "https://www.youtube.com/premium?hl=en",
        ] {
            assert_eq!(
                page_headers(&reqwest::Url::parse(url).unwrap()),
                vec![("Cookie", "SOCS=CAI".into())]
            );
        }
        for host in [
            "redirector.googlevideo.com",
            "www.netflix.com",
            "claude.ai",
            "consent.youtube.com",
        ] {
            assert!(
                page_headers(&reqwest::Url::parse(&format!("https://{host}/")).unwrap()).is_empty()
            );
        }
    }
}

#[cfg(test)]
mod dial_failure_tests {
    use super::*;
    use brocade_deployment::unlock_probe::*;

    #[test]
    #[ignore = "requires the isolated Docker actual-Xray egress fixture"]
    fn actual_xray_dial_policy_in_docker() {
        assert!(std::path::Path::new("/.dockerenv").exists());
        let path =
            std::env::var("BROCADE_PROBE_EGRESS_TEST").expect("explicit Docker fixture descriptor");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let ports: std::collections::BTreeMap<String, u16> =
            serde_json::from_value(value["ports"].clone()).unwrap();
        let trace = std::sync::Arc::new(connections::Source::fixture_with_ports(
            value["trace"].as_str().unwrap().into(),
            &ports,
        ));
        let context = ProbeContext {
            client: crate::http::diagnostic_socks_client(ports["webpage"]).unwrap(),
            request_timeout: CHAIN_REQUEST_TIMEOUT,
            ipv6: true,
            trace: Some(trace),
            trace_expected: true,
            service: None,
            part: UnlockPart::Service,
            requests: Default::default(),
        };
        let readings = detect(
            Some(&context),
            Some(IpFamily::V6),
            false,
            &[UnlockService::YoutubePremium],
        );
        let reading = &readings[0];
        assert!(reading.valid());
        let (status, code, count, error) = if value["mode"] == "no-address" {
            (
                UnlockStatus::Skipped,
                UnlockCode::NoIpv6Egress,
                1,
                "ipv6_no_address",
            )
        } else {
            (
                UnlockStatus::Failed,
                UnlockCode::NetworkError,
                2,
                "network_unreachable",
            )
        };
        assert_eq!((reading.status, reading.code), (status, code));
        let detail = reading.youtube.as_ref().unwrap();
        assert_eq!((detail.webpage.status, detail.webpage.code), (status, code));
        assert_eq!((detail.premium.status, detail.premium.code), (status, code));
        assert_eq!(detail.cdn.error, Some(code));
        for part in [UnlockPart::Webpage, UnlockPart::Premium, UnlockPart::Cdn] {
            assert_eq!(reading.connection_requests[&part], count);
            let records = reading
                .connection_observations
                .iter()
                .filter(|record| record.part == part)
                .collect::<Vec<_>>();
            assert_eq!(records.len(), count as usize);
            assert!(records
                .iter()
                .all(|record| record.connection.exit == "docker-exit"
                    && record.connection.family == Some(IpFamily::V6)
                    && record.connection.complete
                    && !record.connection.connected
                    && record.connection.error.as_deref() == Some(error)));
        }
        let output = value["output"].as_str().unwrap();
        std::fs::write(output, serde_json::to_vec(reading).unwrap()).unwrap();
    }
    fn records(code: &str, count: u32) -> Vec<UnlockConnectionObservation> {
        (1..=count).map(|id|serde_json::from_value(serde_json::json!({
            "part":"service","version":2,"id":format!("{id:032x}"),"domain":"gemini.google.com",
            "exit":"actual-exit","outbound":"out:egress:probe-v6","profile":"a".repeat(64),"normal_strategy":"UseIP",
            "family":"v6","connected":false,"complete":true,"error":code,"lookups":[]
        })).unwrap()).collect()
    }
    #[test]
    fn structured_unreachable_stops_after_one_confirmation_but_timeouts_keep_retrying() {
        for (code, want) in [
            ("network_unreachable", 2),
            ("host_unreachable", 2),
            ("connection_refused", 2),
            ("source_unavailable", 1),
            ("connect_timeout", 11),
            ("connect_error", 11),
        ] {
            let mut calls = 0;
            let outcome = confirm_with(
                UnlockService::Gemini,
                MAX_SERVICE_ATTEMPTS,
                Instant::now() + SERVICE_TIMEOUT,
                || {
                    calls += 1;
                    let mut outcome = result(
                        UnlockService::Gemini,
                        UnlockStatus::Failed,
                        UnlockCode::NetworkError,
                        None,
                    );
                    outcome.connection_observations = records(code, calls);
                    outcome
                        .connection_requests
                        .insert(UnlockPart::Service, calls);
                    outcome
                },
                || {},
            );
            assert_eq!(calls, want, "{code}");
            assert_eq!(outcome.status, UnlockStatus::Failed);
        }
    }
    #[test]
    fn exhausted_host_is_not_requested_again_and_does_not_disable_other_hosts() {
        let path = std::env::temp_dir().join(format!("brocade-dial-cache-{}", std::process::id()));
        let observation = &records("ipv6_no_address", 1)[0];
        let mut value = serde_json::to_value(observation).unwrap();
        value["group"] = serde_json::json!("gemini.service");
        let mut started = value.clone();
        started["started"] = serde_json::json!(true);
        started["complete"] = serde_json::json!(false);
        std::fs::write(&path, format!("{started}\n{value}\n")).unwrap();
        let context = ProbeContext {
            client: crate::http::diagnostic_socks_client(1).unwrap(),
            request_timeout: CHAIN_REQUEST_TIMEOUT,
            ipv6: true,
            trace: Some(std::sync::Arc::new(connections::Source::fixture_source(
                path.clone(),
            ))),
            trace_expected: true,
            service: Some(UnlockService::Gemini),
            part: UnlockPart::Service,
            requests: Default::default(),
        };
        context
            .requests
            .lock()
            .unwrap()
            .insert(UnlockPart::Service, 1);
        assert!(!context.can_retry_host("gemini.google.com"));
        assert!(context.can_retry_host("claude.ai"));
        assert!(matches!(
            get(
                "https://gemini.google.com/",
                Instant::now() + Duration::from_secs(1),
                Some(&context)
            ),
            Err(UnlockCode::NoIpv6Egress)
        ));
        assert_eq!(context.requests.lock().unwrap()[&UnlockPart::Service], 1);
        std::fs::remove_file(path).unwrap();
    }
}
