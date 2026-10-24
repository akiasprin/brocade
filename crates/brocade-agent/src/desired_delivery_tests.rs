use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
};

fn options(name: &str, server: String) -> Options {
    let state_dir = env::temp_dir().join(format!("brocade-delivery-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&state_dir);
    Options {
        command: "apply-once".to_owned(),
        server,
        token: "test-token".to_owned(),
        state_dir,
        apply_mode: ApplyMode::StateDir,
        vpngate_stats_window: Duration::from_secs(900),
    }
}

fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    let mut length = None;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length: ") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
        headers.push_str(&line);
    }
    let mut body = vec![0; length.unwrap()];
    reader.read_exact(&mut body).unwrap();
    (headers, body)
}

fn accept_request(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("test listener did not receive the expected request: {error}"),
        }
    }
}

#[test]
fn desired_delivery_recovers_lost_response_and_waits_for_observation_ack() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let options = options(
        "lost-response",
        format!("http://{}", listener.local_addr().unwrap()),
    );
    let desired = NodeDesiredDeployment {
        deployment_id: 638,
        claim_generation: 0,
        node_id: "n1".to_owned(),
        wave: 1,
        actions: vec![PlannedAction::ApplyWireGuard],
        xray_config_update_policy: None,
        usage_generation_id: None,
        desired: brocade_deployment::plan::NodeDesiredState {
            phantun: DesiredArtifact::Unmanaged {
                reason: "not managed by this test".to_owned(),
            },
            wireguard: DesiredArtifact::Present {
                sha256: sha256_hex(b"test wireguard"),
                content: "test wireguard".to_owned(),
            },
            xray: DesiredArtifact::Unmanaged {
                reason: "not managed by this test".to_owned(),
            },
            hy2_port_hop: DesiredArtifact::Unmanaged {
                reason: "not managed by this test".to_owned(),
            },
            grants: DesiredGrants::Unmanaged {
                reason: "not managed by this test".to_owned(),
            },
        },
        phantun_binary: None,
    };
    let expected = desired.clone();
    let server = thread::spawn(move || {
        let mut stream = accept_request(&listener);
        assert!(read_request(&mut stream)
            .0
            .starts_with("GET /agent/v1/desired "));
        // The Console has committed its claim, but the response is truncated in transit.
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{}")
            .unwrap();
        drop(stream);
        let mut stream = accept_request(&listener);
        assert!(read_request(&mut stream)
            .0
            .starts_with("GET /agent/v1/desired "));
        let body = serde_json::to_vec(&DesiredStateResponse::Deployment {
            deployment: desired,
            certificates: vec![],
        })
        .unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
    });
    let error = apply_once_inner(options.clone(), None, false).unwrap_err();
    assert!(error.contains("phase=body"), "{error}");
    assert!(!options.state_dir.join("wireguard.conf").exists());
    assert!(spool::spool_read(OBSERVATION_SPOOL, &options.state_dir)
        .unwrap()
        .is_empty());
    apply_once_inner(options.clone(), None, false).unwrap();
    server.join().unwrap();
    assert_eq!(
        fs::read_to_string(options.state_dir.join("wireguard.conf")).unwrap(),
        "test wireguard"
    );
    let receipts = spool::spool_read(OBSERVATION_SPOOL, &options.state_dir).unwrap();
    assert_eq!(receipts.len(), 1);
    let receipt: AgentObservationRequest = serde_json::from_str(&receipts[0]).unwrap();
    assert_eq!(receipt.deployment_id, expected.deployment_id);
    assert_eq!(receipt.claim_generation, expected.claim_generation);

    // A fresh executor has only on-disk state. The closed origin would fail any accidental GET.
    apply_once_inner(options.clone(), None, false).unwrap();
    assert_eq!(
        spool::spool_read(OBSERVATION_SPOOL, &options.state_dir).unwrap(),
        receipts
    );

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut recovered = options.clone();
    recovered.server = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let mut stream = accept_request(&listener);
        let (headers, body) = read_request(&mut stream);
        assert!(headers.starts_with("POST /agent/v1/observation "));
        let posted: AgentObservationRequest = serde_json::from_slice(&body).unwrap();
        assert_eq!(posted, receipt);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
        let mut stream = accept_request(&listener);
        assert!(read_request(&mut stream)
            .0
            .starts_with("GET /agent/v1/desired "));
        let body = serde_json::to_vec(&DesiredStateResponse::Certificates(vec![])).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        drop(stream);

        // The certificate response schedules a runtime report. Receiving its body proves the
        // preceding NIC sample and runtime snapshot have finished writing this state directory.
        let mut stream = accept_request(&listener);
        let (headers, body) = read_request(&mut stream);
        assert!(headers.starts_with("POST /agent/v1/runtime "));
        let report: brocade_deployment::protocol::NodeRuntimeReport =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(report.spool.observation, 0);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
    });
    spool_drain(OBSERVATION_SPOOL, &recovered).unwrap();
    apply_once_inner(recovered, None, false).unwrap();
    server.join().unwrap();
    assert!(spool::spool_read(OBSERVATION_SPOOL, &options.state_dir)
        .unwrap()
        .is_empty());
    fs::remove_dir_all(options.state_dir).unwrap();
}

#[test]
fn desired_delivery_serializes_executors_and_releases_the_lock_on_drop() {
    let options = options("executor-lock", "http://127.0.0.1:1".to_owned());
    let lock = lock_convergence(&options.state_dir).unwrap();
    assert!(lock_convergence(&options.state_dir).is_err());
    assert!(apply_once_inner(options.clone(), None, false)
        .unwrap_err()
        .contains("独占收敛目录"));
    drop(lock);
    drop(lock_convergence(&options.state_dir).unwrap());
    assert!(options.state_dir.join("convergence.lock").is_file());
    fs::remove_dir_all(options.state_dir).unwrap();
}

#[test]
fn desired_delivery_rejects_symlinked_locks_and_unreadable_observations() {
    let options = options("invalid-state", "http://127.0.0.1:1".to_owned());
    create_private_dir(&options.state_dir).unwrap();
    let outside = options.state_dir.join("unrelated");
    fs::write(&outside, "preserve").unwrap();
    std::os::unix::fs::symlink(&outside, options.state_dir.join("convergence.lock")).unwrap();
    assert!(lock_convergence(&options.state_dir).is_err());
    assert_eq!(fs::read_to_string(outside).unwrap(), "preserve");
    fs::remove_file(options.state_dir.join("convergence.lock")).unwrap();
    fs::create_dir(options.state_dir.join(OBSERVATION_SPOOL.file)).unwrap();
    assert!(apply_once_inner(options.clone(), None, false).is_err());
    fs::remove_dir_all(options.state_dir).unwrap();
}

#[test]
fn desired_delivery_recovers_completed_journal_before_or_after_spool_ack_without_applying() {
    for crash_position in [
        "journal-before-spool",
        "journal-queued",
        "journal-after-ack",
    ] {
        let options = options(crash_position, "http://127.0.0.1:1".to_owned());
        let after = ReportedNodeState {
            phantun: AppliedArtifactState::Unmanaged,
            wireguard: AppliedArtifactState::Unmanaged,
            xray: AppliedArtifactState::Disabled,
            hy2_port_hop: AppliedArtifactState::Unmanaged,
            grants: AppliedGrantsState::Unmanaged,
        };
        let report = AgentObservationRequest {
            deployment_id: 638,
            claim_generation: 7,
            result: TargetApplyResult::Applied,
            observed_before: after.clone(),
            observed_after: after.clone(),
            error: None,
            route: RouteIpReport {
                ipv4: None,
                ipv6: None,
            },
            usage_activated_at_unix_secs: Some(100),
        };
        let pending = PendingConfigConvergence {
            desired: NodeDesiredDeployment {
                deployment_id: 638,
                claim_generation: 7,
                node_id: "n1".to_owned(),
                wave: 1,
                actions: vec![PlannedAction::ApplyXray],
                xray_config_update_policy: None,
                usage_generation_id: None,
                phantun_binary: None,
                desired: brocade_deployment::plan::NodeDesiredState {
                    phantun: DesiredArtifact::Unmanaged {
                        reason: "test".to_owned(),
                    },
                    wireguard: DesiredArtifact::Unmanaged {
                        reason: "test".to_owned(),
                    },
                    xray: DesiredArtifact::Disabled {
                        reason: "must not execute again".to_owned(),
                    },
                    hy2_port_hop: DesiredArtifact::Unmanaged {
                        reason: "test".to_owned(),
                    },
                    grants: DesiredGrants::Unmanaged {
                        reason: "test".to_owned(),
                    },
                },
            },
            observed_before: after,
            usage_activated_at_unix_secs: Some(100),
            certificate_received: false,
            completed_observation: Some(report.clone()),
            observation_queued: false,
        };
        create_private_dir(&options.state_dir).unwrap();
        let mut legacy = serde_json::to_value(&pending).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("completed_observation");
        fsutil::atomic_write_private(
            &pending_config_convergence_path(&options.state_dir),
            &serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        assert!(read_pending_config_convergence(&options.state_dir)
            .unwrap()
            .unwrap()
            .completed_observation
            .is_none());
        let mut foreign = pending.clone();
        foreign
            .completed_observation
            .as_mut()
            .unwrap()
            .claim_generation += 1;
        assert!(write_pending_config_convergence(&options.state_dir, &foreign).is_err());
        fsutil::atomic_write_private(
            &pending_config_convergence_path(&options.state_dir),
            &serde_json::to_vec(&foreign).unwrap(),
        )
        .unwrap();
        assert!(read_pending_config_convergence(&options.state_dir).is_err());
        write_pending_config_convergence(&options.state_dir, &pending).unwrap();
        // Crashes can occur before appending, with a queued receipt, or after an independent
        // reporter delivered and removed it. All positions retain the completed journal.
        if crash_position == "journal-queued" {
            spool_push_unique(OBSERVATION_SPOOL, &options.state_dir, &report).unwrap();
        } else if crash_position == "journal-after-ack" {
            fs::write(options.state_dir.join(OBSERVATION_SPOOL.file), "").unwrap();
        }
        apply_once_inner(options.clone(), None, false).unwrap();
        assert!(
            !options.state_dir.join("xray.disabled").exists(),
            "completed changes must not run again"
        );
        assert!(!options.state_dir.join(CONFIG_CONVERGENCE_FILE).exists());
        let receipts = spool::spool_read(OBSERVATION_SPOOL, &options.state_dir).unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(
            serde_json::from_str::<AgentObservationRequest>(&receipts[0]).unwrap(),
            report
        );
        fs::remove_dir_all(options.state_dir).unwrap();
    }
}

#[test]
fn completed_convergence_unlocks_even_while_a_forked_child_has_not_execed() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let options = options("forked-lock", "http://127.0.0.1:1".to_owned());
    let lock = lock_convergence(&options.state_dir).unwrap();
    let mut descriptors = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let read = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let write = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    // The child executes only async-signal-safe syscalls and _exit, never Rust cleanup.
    // Hold it before exec to reproduce the inherited open-description lock deterministically.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        let mut signal = 0_u8;
        unsafe {
            libc::close(write.as_raw_fd());
            libc::read(read.as_raw_fd(), (&mut signal as *mut u8).cast(), 1);
            libc::_exit(0);
        }
    }
    drop(lock);
    let replacement = lock_convergence(&options.state_dir);
    let signal = 1_u8;
    assert_eq!(
        unsafe { libc::write(write.as_raw_fd(), (&signal as *const u8).cast(), 1) },
        1
    );
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(
        replacement.is_ok(),
        "the pre-exec child retained a completed convergence lock"
    );
    drop(replacement);
    fs::remove_dir_all(options.state_dir).unwrap();
}
