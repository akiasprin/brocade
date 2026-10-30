//! Keep independent access and country observations across bounded request retries.
use super::{get, parse, result, trace_region, ProbeContext, MAX_SERVICE_ATTEMPTS};
use brocade_deployment::unlock_probe::{
    UnlockCode as C, UnlockResult, UnlockService as S, UnlockStatus as U,
};
use std::{
    thread,
    time::{Duration, Instant},
};

pub(super) fn detect(attempts: usize, proxy: Option<&ProbeContext>) -> UnlockResult {
    measure(attempts, None, proxy)
}

pub(super) fn once(deadline: Instant, proxy: Option<&ProbeContext>) -> UnlockResult {
    measure(1, Some(deadline), proxy)
}

fn measure(attempts: usize, outer: Option<Instant>, proxy: Option<&ProbeContext>) -> UnlockResult {
    let deadline = outer.unwrap_or_else(|| Instant::now() + super::SERVICE_TIMEOUT);
    confirm_with(
        attempts,
        deadline,
        || {
            let outcome = get(
                "https://api.openai.com/compliance/cookie_requirements",
                deadline,
                proxy,
            )
            .map(|page| {
                parse(
                    S::Chatgpt,
                    page.response.status,
                    &page.response.body,
                    page.url.as_str(),
                )
            })
            .unwrap_or_else(|code| result(S::Chatgpt, U::Failed, code, None));
            AccessObservation {
                result: outcome,
                retry_allowed: proxy.is_none_or(|context| context.can_retry_host("api.openai.com")),
            }
        },
        || {
            let outcome = get("https://ios.chat.openai.com/", deadline, proxy)
                .map(|page| {
                    let lower = page.response.body.to_ascii_lowercase();
                    let (status, code) =
                        if lower.contains("request is not allowed. please try again later.") {
                            (U::Available, C::WebAvailable)
                        } else if lower.contains("unsupported_country") {
                            (U::UnsupportedRegion, C::RegionUnsupported)
                        } else if matches!(page.response.status, 403 | 451)
                            || lower.contains("disallowed isp")
                            || lower.contains("have been blocked")
                        {
                            (U::Restricted, C::AccessDenied)
                        } else {
                            (U::Failed, C::UnrecognizedResponse)
                        };
                    result(S::Chatgpt, status, code, None)
                })
                .unwrap_or_else(|code| result(S::Chatgpt, U::Failed, code, None));
            AccessObservation {
                result: outcome,
                retry_allowed: proxy
                    .is_none_or(|context| context.can_retry_host("ios.chat.openai.com")),
            }
        },
        || CountryObservation {
            region: trace_region("https://chatgpt.com/cdn-cgi/trace", deadline, proxy),
            retry_allowed: proxy.is_none_or(|context| context.can_retry_host("chatgpt.com")),
        },
        || {
            thread::sleep(
                Duration::from_millis(500).min(deadline.saturating_duration_since(Instant::now())),
            )
        },
    )
}

struct AccessObservation {
    result: UnlockResult,
    retry_allowed: bool,
}
impl From<UnlockResult> for AccessObservation {
    fn from(result: UnlockResult) -> Self {
        Self {
            result,
            retry_allowed: true,
        }
    }
}
struct CountryObservation {
    region: Option<String>,
    retry_allowed: bool,
}
impl From<Option<String>> for CountryObservation {
    fn from(region: Option<String>) -> Self {
        Self {
            region,
            retry_allowed: true,
        }
    }
}

fn pending(reading: &Option<AccessObservation>) -> bool {
    reading
        .as_ref()
        .is_none_or(|reading| reading.result.status == U::Failed && reading.retry_allowed)
}

fn confirm_with<
    W: Into<AccessObservation>,
    A: Into<AccessObservation>,
    R: Into<CountryObservation>,
>(
    attempts: usize,
    deadline: Instant,
    mut webpage: impl FnMut() -> W,
    mut app: impl FnMut() -> A,
    mut country: impl FnMut() -> R,
    mut pause: impl FnMut(),
) -> UnlockResult {
    let mut web = None;
    let mut mobile = None;
    let mut region = None;
    let mut country_pending = true;
    for attempt in 0..attempts.clamp(1, MAX_SERVICE_ATTEMPTS) {
        if Instant::now() >= deadline {
            break;
        }
        if attempt > 0 {
            eprintln!("unlock probe service=Chatgpt retry={attempt} web_pending={} app_pending={} region_pending={}",
                pending(&web), pending(&mobile), region.is_none());
            pause();
        }
        if pending(&web) && Instant::now() < deadline {
            web = Some(webpage().into());
        }
        if pending(&mobile) && Instant::now() < deadline {
            mobile = Some(app().into());
        }
        // A failed country lookup neither erases access nor consumes another access attempt.
        // All requests retain the same family, proxy and exit resolver supplied by measure.
        if country_pending && Instant::now() < deadline {
            let observation = country().into();
            region = observation.region;
            country_pending = region.is_none() && observation.retry_allowed;
        }
        if !pending(&web) && !pending(&mobile) && !country_pending {
            break;
        }
    }
    let timeout = || result(S::Chatgpt, U::Failed, C::Timeout, None);
    let web = web
        .map(|observation| observation.result)
        .unwrap_or_else(timeout);
    let app = mobile
        .map(|observation| observation.result)
        .unwrap_or_else(timeout);
    let (status, code) = match (web.status, app.status) {
        (U::Available, U::Available) => (U::Available, C::WebAvailable),
        (U::Available, U::Restricted | U::UnsupportedRegion) => (U::Partial, C::WebOnly),
        (U::Restricted | U::UnsupportedRegion, U::Available) => (U::Partial, C::AppOnly),
        (U::Available, U::Skipped) => (U::Partial, C::WebOnly),
        (U::Skipped, U::Available) => (U::Partial, C::AppOnly),
        (U::UnsupportedRegion, _) => (U::UnsupportedRegion, web.code),
        (_, U::UnsupportedRegion) => (U::UnsupportedRegion, app.code),
        (U::Restricted, U::Restricted) => (U::Restricted, web.code),
        (U::Skipped, U::Skipped) => (U::Skipped, web.code),
        (U::Restricted, U::Skipped) => (U::Restricted, web.code),
        (U::Skipped, U::Restricted) => (U::Restricted, app.code),
        (U::Failed, _) => (U::Failed, web.code),
        (_, U::Failed) => (U::Failed, app.code),
        _ => (U::Failed, C::UnrecognizedResponse),
    };
    result(S::Chatgpt, status, code, region)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn available() -> UnlockResult {
        result(S::Chatgpt, U::Available, C::WebAvailable, None)
    }
    fn timeout() -> UnlockResult {
        result(S::Chatgpt, U::Failed, C::Timeout, None)
    }

    #[test]
    fn definitive_dial_failures_stop_access_and_country_without_extra_waits() {
        let (mut web, mut app, mut country) = (0, 0, 0);
        let outcome = confirm_with(
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + super::super::SERVICE_TIMEOUT,
            || {
                web += 1;
                AccessObservation {
                    result: result(S::Chatgpt, U::Skipped, C::NoIpv6Egress, None),
                    retry_allowed: false,
                }
            },
            || {
                app += 1;
                AccessObservation {
                    result: result(S::Chatgpt, U::Skipped, C::NoIpv6Egress, None),
                    retry_allowed: false,
                }
            },
            || {
                country += 1;
                CountryObservation {
                    region: None,
                    retry_allowed: false,
                }
            },
            || panic!("a completed structural failure must not wait for another retry"),
        );
        assert_eq!((web, app, country), (1, 1, 1));
        assert_eq!(
            (outcome.status, outcome.code),
            (U::Skipped, C::NoIpv6Egress)
        );
    }

    #[test]
    fn a_skipped_hostname_does_not_erase_another_exit_success_or_failure() {
        for (status, code, want_status, want_code) in [
            (U::Available, C::WebAvailable, U::Partial, C::AppOnly),
            (U::Failed, C::Timeout, U::Failed, C::Timeout),
            (
                U::Restricted,
                C::AccessDenied,
                U::Restricted,
                C::AccessDenied,
            ),
        ] {
            let outcome = confirm_with(
                1,
                Instant::now() + super::super::SERVICE_TIMEOUT,
                || result(S::Chatgpt, U::Skipped, C::NoIpv6Egress, None),
                || result(S::Chatgpt, status, code, None),
                || Some("HK".into()),
                || {},
            );
            assert_eq!((outcome.status, outcome.code), (want_status, want_code));
            assert!(outcome.valid());
        }
    }

    #[test]
    fn country_retries_do_not_repeat_or_erase_confirmed_access() {
        let (mut web, mut app, mut country, mut pauses) = (0, 0, 0, 0);
        let outcome = confirm_with(
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + super::super::SERVICE_TIMEOUT,
            || {
                web += 1;
                available()
            },
            || {
                app += 1;
                available()
            },
            || {
                country += 1;
                (country == 4).then(|| "JP".into())
            },
            || pauses += 1,
        );
        assert_eq!((web, app, country, pauses), (1, 1, 4, 3));
        assert_eq!(outcome.status, U::Available);
        assert_eq!(outcome.region.as_deref(), Some("JP"));
    }

    #[test]
    fn each_request_stops_at_its_first_success_and_has_at_most_ten_retries() {
        let (mut web, mut app, mut country) = (0, 0, 0);
        let outcome = confirm_with(
            usize::MAX,
            Instant::now() + super::super::SERVICE_TIMEOUT,
            || {
                web += 1;
                if web == 11 {
                    available()
                } else {
                    timeout()
                }
            },
            || {
                app += 1;
                if app == 2 {
                    available()
                } else {
                    timeout()
                }
            },
            || {
                country += 1;
                Some("US".into())
            },
            || {},
        );
        assert_eq!((web, app, country), (11, 2, 1));
        assert_eq!(outcome.status, U::Available);
        assert_eq!(outcome.region.as_deref(), Some("US"));
    }

    #[test]
    fn exhausted_country_lookup_preserves_access_without_guessing_region() {
        let mut country = 0;
        let outcome = confirm_with(
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + super::super::SERVICE_TIMEOUT,
            available,
            available,
            || {
                country += 1;
                None
            },
            || {},
        );
        assert_eq!(country, 11);
        assert_eq!(outcome.status, U::Available);
        assert!(outcome.region.is_none());
    }

    #[test]
    fn country_success_cannot_turn_an_access_timeout_into_available() {
        let mut web = 0;
        let outcome = confirm_with(
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + super::super::SERVICE_TIMEOUT,
            || {
                web += 1;
                timeout()
            },
            available,
            || Some("JP".into()),
            || {},
        );
        assert_eq!(web, 11);
        assert_eq!((outcome.status, outcome.code), (U::Failed, C::Timeout));
    }

    #[test]
    fn expired_service_budget_never_starts_another_request() {
        let mut calls = 0;
        let outcome = confirm_with(
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + Duration::from_millis(30),
            || {
                calls += 1;
                thread::sleep(Duration::from_millis(40));
                timeout()
            },
            || -> UnlockResult {
                panic!("the app request must not start after the common deadline")
            },
            || -> Option<String> {
                panic!("country lookup must not start after the common deadline")
            },
            || panic!("the exhausted service budget must not trigger retry waits"),
        );
        assert_eq!(calls, 1);
        assert_eq!((outcome.status, outcome.code), (U::Failed, C::Timeout));
    }

    #[test]
    fn incomplete_response_times_out_at_three_seconds_and_the_next_attempt_recovers() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for attempt in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(6)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                if attempt == 0 {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nloc",
                        )
                        .unwrap();
                    // The client must time out despite the valid status and partial body.
                    assert_eq!(socket.read(&mut [0]).unwrap(), 0);
                } else {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nloc=JP\n").unwrap();
                }
            }
        });
        let mut calls = 0;
        let started = Instant::now();
        let outcome = confirm_with(
            MAX_SERVICE_ATTEMPTS,
            Instant::now() + super::super::SERVICE_TIMEOUT,
            || {
                calls += 1;
                let deadline =
                    super::super::request_deadline(Instant::now() + Duration::from_secs(20), None);
                match super::super::HttpClient::new(&origin)
                    .unwrap()
                    .request_public_diagnostic(
                        "GET",
                        "/fixture",
                        None,
                        "application/json",
                        &[],
                        deadline.saturating_duration_since(Instant::now()),
                    ) {
                    Ok(response) => {
                        assert_eq!(response.body, "loc=JP\n");
                        available()
                    }
                    Err(error) => {
                        assert_eq!(
                            super::super::request_error_code(&error, None),
                            C::Timeout,
                            "{error}"
                        );
                        timeout()
                    }
                }
            },
            available,
            || Some("JP".into()),
            || {},
        );
        assert_eq!(calls, 2);
        assert_eq!(outcome.status, U::Available);
        assert!(started.elapsed() >= Duration::from_millis(2800));
        assert!(started.elapsed() < Duration::from_secs(6));
        server.join().unwrap();
    }

    #[test]
    fn unsupported_region_and_ipv6_skips_are_definitive_and_keep_their_meaning() {
        for (status, code) in [
            (U::UnsupportedRegion, C::RegionUnsupported),
            (U::Skipped, C::NoIpv6Dns),
            (U::Skipped, C::NoIpv6Egress),
        ] {
            let mut calls = 0;
            let outcome = confirm_with(
                MAX_SERVICE_ATTEMPTS,
                Instant::now() + super::super::SERVICE_TIMEOUT,
                || {
                    calls += 1;
                    result(S::Chatgpt, status, code, None)
                },
                || result(S::Chatgpt, status, code, None),
                || Some("HK".into()),
                || {},
            );
            assert_eq!(calls, 1);
            assert_eq!((outcome.status, outcome.code), (status, code));
            assert!(outcome.valid());
        }
    }
}
