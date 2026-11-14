//! Real sockets in a private network namespace; no route to external services exists.
use super::*;
use std::{
    fs,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct FixtureDirectory(PathBuf);
impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn context() -> (Arc<Context>, FixtureDirectory) {
    let path = std::env::temp_dir().join(format!(
        "brocade-maintenance-netns-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&path).unwrap();
    let context = Arc::new(Context {
        directory: path.clone(),
        checkpoint: Arc::new(Mutex::new(super::super::Checkpoint::default())),
        last_sync: Arc::new(Mutex::new(None)),
        assets: Arc::new(Mutex::new(None)),
        stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        instance: "netns-fixture".into(),
        family: None,
    });
    (context, FixtureDirectory(path))
}
async fn headers(stream: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    loop {
        let byte = stream.read_u8().await.unwrap();
        bytes.push(byte);
        assert!(bytes.len() < 16384);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(bytes).unwrap()
}
fn options() -> FetchOptions<'static> {
    FetchOptions {
        ua: "Mozilla/5.0 fixture-device",
        jar: Arc::new(CookieStoreMutex::default()),
        referer: None,
        module: MaintenanceModule::Trust,
        timeout_secs: 5,
        youtube_probe: false,
    }
}

#[tokio::test]
#[ignore = "requires scripts/maintenance-http-netns.py and private loopback fixture addresses"]
async fn ip_maintenance_real_http_source_cookie_redirect_gzip_and_cancellation() {
    assert_eq!(
        std::env::var("BROCADE_MAINTENANCE_NETNS_FIXTURE").as_deref(),
        Ok("1")
    );
    for (source, target, dns) in [
        ("11.0.0.1", "11.0.0.2", false),
        ("2001:470:ffff:abcd::1", "2001:470:ffff:abcd::2", false),
        ("11.0.0.1", "11.0.0.2", true),
        ("2001:470:ffff:abcd::1", "2001:470:ffff:abcd::2", true),
    ] {
        let source: IpAddr = source.parse().unwrap();
        let target: IpAddr = target.parse().unwrap();
        let family = if source.is_ipv4() {
            MaintenanceFamily::V4
        } else {
            MaintenanceFamily::V6
        };
        let policy = IpMaintenancePolicy {
            source_ip: Some(source),
            family,
            ..Default::default()
        };
        let listener = TcpListener::bind((target, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = if dns {
            format!("http://dualstack.maintenance.test:{}", address.port())
        } else {
            format!("http://{address}")
        };
        let payload = b"fixture gzip page";
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, payload).unwrap();
        let compressed = encoder.finish().unwrap();
        let length = compressed.len();
        let server = tokio::spawn(async move {
            let (mut first, peer) = listener.accept().await.unwrap();
            assert_eq!(peer.ip(), source);
            let request = headers(&mut first).await.to_ascii_lowercase();
            assert!(request.contains("user-agent: mozilla/5.0 fixture-device"));
            assert!(request.contains("accept-encoding: gzip"));
            first.write_all(b"HTTP/1.1 302 Found\r\nLocation: /gzip\r\nSet-Cookie: visit=fixture-state; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            drop(first);
            let (mut second, peer) = listener.accept().await.unwrap();
            assert_eq!(peer.ip(), source);
            let request = headers(&mut second).await.to_ascii_lowercase();
            assert!(request.contains("cookie: visit=fixture-state"));
            assert!(request.contains("get /gzip http/1.1"));
            second.write_all(format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            second.write_all(&compressed).await.unwrap();
        });
        let (context, _directory) = context();
        let page = fetch(
            &context,
            &policy,
            Url::parse(&format!("{origin}/start")).unwrap(),
            options(),
        )
        .await
        .unwrap();
        assert_eq!(page.status, 200);
        assert_eq!(page.body, "fixture gzip page");
        assert_eq!(context.locked().unwrap().daily_requests, 2);
        assert_eq!(context.locked().unwrap().daily_bytes, length as u64);
        server.await.unwrap();
        if !dns {
            shared_youtube_headers_and_local_consent_failures(&policy, source, target).await;
        }
        if dns {
            // Selecting a family must not weaken public-only admission within
            // that family, or silently fall back when it has no usable address.
            for host in ["mixed.maintenance.test", "opposite-family.maintenance.test"] {
                if host == "opposite-family.maintenance.test" && source.is_ipv4() {
                    continue;
                }
                let result = fetch(
                    &context,
                    &policy,
                    Url::parse(&format!("http://{host}:{}/", address.port())).unwrap(),
                    options(),
                )
                .await;
                assert!(matches!(result, Err(ref error) if error == "non_public_target"));
            }
        }
    }
    let policy = IpMaintenancePolicy {
        source_ip: Some("11.0.0.1".parse().unwrap()),
        ..Default::default()
    };
    let private = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let private_address = private.local_addr().unwrap();
    let public = TcpListener::bind("11.0.0.2:0").await.unwrap();
    let public_address = public.local_addr().unwrap();
    let redirect = tokio::spawn(async move {
        let (mut stream, _) = public.accept().await.unwrap();
        headers(&mut stream).await;
        stream.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{private_address}/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    });
    let (context, _directory) = context();
    let result = fetch(
        &context,
        &policy,
        Url::parse(&format!("http://{public_address}/redirect")).unwrap(),
        options(),
    )
    .await;
    assert!(matches!(result,Err(ref error) if error=="non_public_target"));
    redirect.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), private.accept())
            .await
            .is_err()
    );
    let listener = TcpListener::bind("11.0.0.2:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, ready) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        headers(&mut stream).await;
        accepted.send(()).unwrap();
        let mut byte = [0];
        tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
            .await
            .unwrap()
            .unwrap()
    });
    {
        let request = fetch(
            &context,
            &policy,
            Url::parse(&format!("http://{address}/stalled")).unwrap(),
            options(),
        );
        tokio::pin!(request);
        tokio::select! {result=&mut request=>panic!("fixture completed before cancellation: {}",result.is_ok()),_ = ready=>{}}
        // Leaving this scope drops the actual HTTP future, as a policy change does.
    }
    assert_eq!(
        server.await.unwrap(),
        0,
        "canceled request retained a live connection"
    );
    dual_stack_real_requests_are_parallel_and_fail_independently().await;
    bounded_pages_retry_and_continue_to_the_next_action().await;
    javascript_prompt_is_an_ordinary_success().await;
    probe_requests_retry_twice_and_respect_rate_limits().await;
}

async fn javascript_prompt_is_an_ordinary_success() {
    let listener = TcpListener::bind("11.0.0.2:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = "<title>Google Search</title><a href='/httpservice/retry/enablejs'>Continue</a>";
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(headers(&mut stream)
            .await
            .starts_with("GET /search?q=fixture "));
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let (context, _directory) = context();
    let desired = IpMaintenanceDesired {
        policy: IpMaintenancePolicy {
            source_ip: Some("11.0.0.1".parse().unwrap()),
            ..Default::default()
        },
        ..Default::default()
    };
    context.apply(&desired).unwrap();
    context
        .begin_round(&desired, MaintenanceModule::Google, 1)
        .unwrap();
    let session = Session::open(&context.directory, "javascript-fixture", 14).unwrap();
    execute_actions(
        &context,
        &desired.policy,
        MaintenanceModule::Google,
        "fixture-device",
        &session,
        1,
        |_, _| {
            Ok(behavior::Action {
                kind: "google_search",
                url: Url::parse(&format!(
                    "http://www.google.com:{}/search?q=fixture",
                    address.port()
                ))
                .unwrap(),
                dwell_secs: 0,
            })
        },
    )
    .await
    .unwrap();
    server.await.unwrap();
    let state = context.locked().unwrap();
    let action = &state.current.as_ref().unwrap().actions[0];
    assert_eq!(action.status, Some(200));
    assert!(action.error.is_none());
    assert_eq!(state.daily_requests, 1);
}

async fn probe_requests_retry_twice_and_respect_rate_limits() {
    for statuses in [
        vec![500, 500, 200],
        vec![0, 0, 200],
        vec![503, 503, 503],
        vec![429],
    ] {
        let listener = TcpListener::bind("11.0.0.2:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let expected = statuses.len() as u32;
        let final_status = *statuses.last().unwrap();
        let server = tokio::spawn(async move {
            for status in statuses {
                let (mut stream, _) = listener.accept().await.unwrap();
                headers(&mut stream).await;
                if status == 0 {
                    continue;
                }
                stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").as_bytes()).await.unwrap();
            }
        });
        let (context, _directory) = context();
        let policy = IpMaintenancePolicy::default();
        let target = Url::parse(&format!("http://{address}/probe")).unwrap();
        let result = retry_request(|| fetch(&context, &policy, target.clone(), options())).await;
        if final_status == 429 {
            assert_eq!(result.err().as_deref(), Some("service_restricted"));
            assert!(context.locked().unwrap().cooldown_until > now().unwrap());
        } else {
            assert_eq!(result.unwrap().status, final_status);
        }
        server.await.unwrap();
        assert_eq!(context.locked().unwrap().daily_requests, expected);
    }
}

async fn shared_youtube_headers_and_local_consent_failures(
    policy: &IpMaintenancePolicy,
    source: IpAddr,
    target: IpAddr,
) {
    let listener = TcpListener::bind((target, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, peer) = listener.accept().await.unwrap();
        assert_eq!(peer.ip(), source);
        let request = headers(&mut stream).await.to_ascii_lowercase();
        assert!(request.starts_with("get /premium?hl=en http/1.1"));
        assert_eq!(request.matches("user-agent:").count(), 1);
        assert!(request.contains("chrome/130.0.0.0"));
        assert!(request.contains("accept-language: en-us,en;q=0.9"));
        assert!(request.contains("session=fixture"));
        assert_eq!(request.matches("socs=cai").count(), 1);
        let body = r#"{"desktopTopbarRenderer":{"countryCode":"US"}}"#;
        stream.write_all(format!("HTTP/1.1 200 OK\r\nSet-Cookie: VISITOR_PRIVACY_METADATA=CgJERQ==; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    let url = Url::parse(&format!(
        "http://www.youtube.com:{}/premium?hl=en",
        address.port()
    ))
    .unwrap();
    let jar = Arc::new(CookieStoreMutex::default());
    jar.set_cookies(
        &mut std::iter::once(&HeaderValue::from_static("session=fixture; Path=/")),
        &url,
    );
    let (context, _directory) = self::context();
    let mut probe = options();
    probe.module = MaintenanceModule::Google;
    probe.youtube_probe = true;
    probe.jar = jar;
    let page = fetch(&context, policy, url, probe).await.unwrap();
    let cookies = page
        .headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        crate::unlock_probe::youtube_response_country(&page.body, &cookies).as_deref(),
        Some("DE")
    );
    server.await.unwrap();

    // A consent-only 200 response is tried three times, then the following real
    // document still runs. Its traffic remains in the shared daily counters.
    let listener = TcpListener::bind((target, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for attempt in 0..4 {
            let (mut stream, peer) = listener.accept().await.unwrap();
            assert_eq!(peer.ip(), source);
            let request = headers(&mut stream).await;
            assert!(request.starts_with(if attempt < 3 {
                "GET /blocked "
            } else {
                "GET /content "
            }));
            let body = if attempt < 3 {
                "<html><title>Before you continue to Google</title></html>"
            } else {
                "<html><article>Actual regional content</article></html>"
            };
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let (context, _directory) = self::context();
    let desired = IpMaintenanceDesired {
        generation: 1,
        policy: policy.clone(),
        ..Default::default()
    };
    context.apply(&desired).unwrap();
    context
        .begin_round(&desired, MaintenanceModule::Trust, 2)
        .unwrap();
    let session = Session::open(&context.directory, "consent-fixture", 14).unwrap();
    execute_actions(
        &context,
        policy,
        MaintenanceModule::Trust,
        "fixture-ua",
        &session,
        2,
        |index, _| {
            Ok(behavior::Action {
                kind: "local_site",
                url: Url::parse(&format!(
                    "http://{address}/{}",
                    if index == 0 { "blocked" } else { "content" }
                ))
                .unwrap(),
                dwell_secs: 0,
            })
        },
    )
    .await
    .unwrap();
    server.await.unwrap();
    let state = context.locked().unwrap();
    let actions = &state.current.as_ref().unwrap().actions;
    assert_eq!(actions[0].error.as_deref(), Some("consent_required"));
    assert_eq!(actions[1].status, Some(200));
    assert_eq!(actions[1].error, None);
    assert_eq!(state.daily_requests, 4);
    assert!(state.daily_bytes > 0);
}

async fn bounded_pages_retry_and_continue_to_the_next_action() {
    for (size, gzip, failures, expected_requests, all_failed) in [
        (6 * 1024 * 1024, false, 0, 2, false),
        (16 * 1024 * 1024, false, 0, 2, false),
        (16 * 1024 * 1024 + 1, false, 0, 2, false),
        (16 * 1024 * 1024 + 1, true, 0, 2, false),
        (128, false, 2, 4, false),
        (128, false, 3, 4, false),
        (128, false, 6, 6, true),
    ] {
        let listener = TcpListener::bind("11.0.0.2:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let payload = vec![b'a'; size];
        let payload = if gzip {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            std::io::Write::write_all(&mut encoder, &payload).unwrap();
            encoder.finish().unwrap()
        } else {
            payload
        };
        let server = tokio::spawn(async move {
            for request in 0..expected_requests {
                let (mut stream, _) = listener.accept().await.unwrap();
                let headers = headers(&mut stream).await;
                assert!(
                    headers.starts_with(if request < expected_requests - 1 || all_failed {
                        "GET /"
                    } else {
                        "GET /1 "
                    })
                );
                let status = if request < failures { 500 } else { 200 };
                let body: &[u8] = if request == 0 { &payload } else { b"ok" };
                let encoding = if gzip && request == 0 {
                    "Content-Encoding: gzip\r\n"
                } else {
                    ""
                };
                stream.write_all(format!("HTTP/1.1 {status} Fixture\r\n{encoding}Content-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
                // A bounded client may close as soon as it crosses the page limit.
                let _ = stream.write_all(body).await;
            }
        });
        let (context, _directory) = context();
        let desired = IpMaintenanceDesired {
            generation: 1,
            policy: IpMaintenancePolicy {
                source_ip: Some("11.0.0.1".parse().unwrap()),
                ..Default::default()
            },
            ..Default::default()
        };
        context.apply(&desired).unwrap();
        context
            .begin_round(&desired, MaintenanceModule::Trust, 2)
            .unwrap();
        let session = Session::open(&context.directory, "size-retry-fixture", 14).unwrap();
        let result = execute_actions(
            &context,
            &desired.policy,
            MaintenanceModule::Trust,
            "fixture-device",
            &session,
            2,
            |index, _| {
                Ok(behavior::Action {
                    kind: "local_site",
                    url: Url::parse(&format!("http://{address}/{index}")).unwrap(),
                    dwell_secs: 0,
                })
            },
        )
        .await;
        if all_failed {
            assert_eq!(result.unwrap_err(), "all_actions_failed");
        } else {
            result.unwrap();
            context
                .emit(MaintenancePhase::Complete, None, None)
                .unwrap();
        }
        server.await.unwrap();
        let state = context.locked().unwrap();
        assert_eq!(state.daily_requests, expected_requests as u32);
        let report = state.current.as_ref().unwrap();
        assert_eq!(report.actions.len(), 2);
        assert_eq!(report.action_index, 2);
        if !all_failed {
            assert_eq!(report.phase, MaintenancePhase::Complete);
            assert_eq!(report.actions[1].status, Some(200));
        }
        if size > MAX_RESPONSE_BYTES {
            assert_eq!(
                report.actions[0].error.as_deref(),
                Some("response_size_skipped")
            );
        } else if failures >= 3 {
            assert_eq!(report.actions[0].status, Some(500));
        } else {
            assert_eq!(report.actions[0].status, Some(200));
            assert!(report.actions[0].error.is_none());
        }
        if failures > 0 {
            assert!(
                state.daily_bytes >= 128,
                "retry bytes must stay in the daily budget"
            );
        }
    }
}

async fn dual_stack_real_requests_are_parallel_and_fail_independently() {
    let (root, _directory) = context();
    let v4 = root.for_family(MaintenanceFamily::V4);
    let v6 = root.for_family(MaintenanceFamily::V6);
    let mut desired = IpMaintenanceDesired {
        generation: 1,
        ..Default::default()
    };
    desired.policy.family = MaintenanceFamily::Dual;
    desired.policy.source_ip = Some("11.0.0.1".parse().unwrap());
    desired.policy.source_ipv6 = Some("2001:470:ffff:abcd::1".parse().unwrap());
    root.apply(&desired).unwrap();
    for context in [&v4, &v6] {
        context
            .begin_round(&desired, MaintenanceModule::Trust, 1)
            .unwrap();
    }
    let a = TcpListener::bind("11.0.0.2:0").await.unwrap();
    let b = TcpListener::bind("[2001:470:ffff:abcd::2]:0")
        .await
        .unwrap();
    let aa = a.local_addr().unwrap();
    let bb = b.local_addr().unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let launch_server = |listener: TcpListener, expected: IpAddr, count: usize, status: u16| {
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            for index in 0..count {
                let (mut stream, peer) = listener.accept().await.unwrap();
                assert_eq!(peer.ip(), expected);
                headers(&mut stream).await;
                if index == 0 {
                    barrier.wait().await;
                }
                stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").as_bytes()).await.unwrap();
            }
        })
    };
    let sa = launch_server(a, desired.policy.source_ip.unwrap(), 3, 500);
    let sb = launch_server(b, desired.policy.source_ipv6.unwrap(), 1, 200);
    let execute = |context: Arc<Context>,
                   policy: IpMaintenancePolicy,
                   address: std::net::SocketAddr| async move {
        let session = Session::open(&context.directory, &address.to_string(), 14).unwrap();
        let outcome = execute_actions(
            &context,
            &policy,
            MaintenanceModule::Trust,
            "fixture-device",
            &session,
            1,
            |_, _| {
                Ok(behavior::Action {
                    kind: "local_site",
                    url: Url::parse(&format!("http://{address}/")).unwrap(),
                    dwell_secs: 0,
                })
            },
        )
        .await;
        context
            .emit(
                if outcome.is_ok() {
                    MaintenancePhase::Complete
                } else {
                    MaintenancePhase::Failed
                },
                outcome.as_ref().err().map(String::as_str),
                None,
            )
            .unwrap();
        outcome
    };
    let (ra, rb) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            execute(
                Arc::clone(&v4),
                desired.policy.for_family(MaintenanceFamily::V4).unwrap(),
                aa
            ),
            execute(
                Arc::clone(&v6),
                desired.policy.for_family(MaintenanceFamily::V6).unwrap(),
                bb
            )
        )
    })
    .await
    .expect("both address families must connect before either response is released");
    assert_eq!(ra.unwrap_err(), "all_actions_failed");
    rb.unwrap();
    sa.await.unwrap();
    sb.await.unwrap();
    assert_eq!(root.locked().unwrap().daily_requests, 4);
    assert_eq!(
        v4.locked().unwrap().current.as_ref().unwrap().phase,
        MaintenancePhase::Failed
    );
    assert_eq!(
        v6.locked().unwrap().current.as_ref().unwrap().phase,
        MaintenancePhase::Complete
    );
    assert_eq!(root.locked().unwrap().terminal.len(), 2);
}
