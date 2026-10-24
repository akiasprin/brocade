use super::*;
use std::{
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

struct Server {
    address: SocketAddr,
    calls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn start(handler: impl Fn(usize, TcpStream) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_stop = Arc::clone(&stop);
        let worker_calls = Arc::clone(&calls);
        let worker = thread::spawn(move || {
            thread::scope(|scope| {
                while !worker_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let handler = &handler;
                            let calls = &worker_calls;
                            scope.spawn(move || {
                                stream
                                    .set_read_timeout(Some(Duration::from_secs(1)))
                                    .unwrap();
                                stream
                                    .set_write_timeout(Some(Duration::from_secs(1)))
                                    .unwrap();
                                let mut preface = [0; 24];
                                stream.read_exact(&mut preface).unwrap();
                                assert_eq!(&preface, CLIENT_PREFACE);
                                loop {
                                    let frame = read_frame(&mut stream).unwrap();
                                    if frame.kind == 0 && frame.flags & END_STREAM != 0 {
                                        break;
                                    }
                                }
                                let index = calls.fetch_add(1, Ordering::Relaxed);
                                assert!(index < 16, "test server received unbounded requests");
                                handler(index, stream);
                            });
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1))
                        }
                        Err(error) => panic!("accept test connection: {error}"),
                    }
                }
            });
        });
        Self {
            address,
            calls,
            stop,
            worker: Some(worker),
        }
    }

    fn count(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn policy() -> Policy {
    Policy {
        io_timeout: Duration::from_millis(80),
        read_budget: Duration::from_millis(500),
        retry_delay: Duration::from_millis(5),
        max_attempts: 3,
    }
}

fn reply(mut stream: TcpStream) {
    write_frame(&mut stream, 1, END_HEADERS, 1, &[0x88]).unwrap();
    write_frame(&mut stream, 0, END_STREAM, 1, &[0; 5]).unwrap();
}

#[test]
fn retries_a_temporary_read_timeout_on_a_fresh_connection() {
    let server = Server::start(|index, stream| {
        if index == 0 {
            thread::sleep(Duration::from_millis(250));
        } else {
            reply(stream);
        }
    });
    let result = unary_with_policy(server.address, LIST_INBOUNDS_PATH, &[], None, policy());
    assert_eq!(result.unwrap(), Vec::<u8>::new());
    assert_eq!(server.count(), 2);
}

#[test]
fn discards_a_partial_frame_before_retrying() {
    let server = Server::start(|index, mut stream| {
        if index == 0 {
            stream.write_all(&[0, 0]).unwrap();
            thread::sleep(Duration::from_millis(250));
        } else {
            reply(stream);
        }
    });
    assert!(
        unary_with_policy(server.address, GET_INBOUND_USERS_PATH, &[], None, policy())
            .unwrap()
            .is_empty()
    );
    assert_eq!(server.count(), 2);
}

#[test]
fn persistent_timeouts_exhaust_the_attempt_limit_without_logging_payloads() {
    let server = Server::start(|_, _stream| thread::sleep(Duration::from_millis(350)));
    let started = Instant::now();
    let mut request = Vec::new();
    bytes_field(&mut request, 1, b"request-payload-sentinel");
    let error =
        unary_with_policy(server.address, QUERY_STATS_PATH, &request, None, policy()).unwrap_err();
    assert_eq!(server.count(), 3);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(error.contains("XRAY_API_READ_TIMEOUT"), "{error}");
    assert!(error.contains("attempts=3"), "{error}");
    assert!(error.contains("first=stage=Read"), "{error}");
    assert!(error.contains("last=stage=Read"), "{error}");
    assert!(!error.contains("request-payload-sentinel"));
}

#[test]
fn counter_reset_and_malformed_requests_are_not_read_only() {
    for path in [QUERY_STATS_PATH, GET_USERS_STATS_PATH] {
        assert!(read_only(path, &[]));
        let mut request = Vec::new();
        // Both reset fields are field 2 in app/stats/command/command.proto.
        varint_field(&mut request, 2, 0);
        assert!(read_only(path, &request));
        varint_field(&mut request, 2, 1);
        assert!(!read_only(path, &request));
        assert!(!read_only(path, &[0xff]));
        let server = Server::start(|_, _stream| thread::sleep(Duration::from_millis(250)));
        let error = unary_with_policy(server.address, path, &request, None, policy()).unwrap_err();
        assert!(error.contains("attempts=1"), "{error}");
        assert_eq!(server.count(), 1);
    }
    // GetUsersStats field 1 enables traffic in the response; it does not reset it.
    let mut request = Vec::new();
    varint_field(&mut request, 1, 1);
    assert!(read_only(GET_USERS_STATS_PATH, &request));
}

#[test]
#[ignore = "uses the production 5-second idle timeout; run explicitly"]
fn production_idle_timeout_recovers_without_a_manual_retry() {
    let server = Server::start(|index, stream| {
        if index == 0 {
            thread::sleep(Duration::from_secs(6));
        } else {
            reply(stream);
        }
    });
    let started = Instant::now();
    let result = super::super::inbound_tags(server.address);
    assert!(result.unwrap().is_empty());
    assert_eq!(server.count(), 2);
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn never_replays_a_mutation_that_may_already_have_been_applied() {
    for path in [
        ALTER_INBOUND_PATH,
        ADD_INBOUND_PATH,
        REMOVE_INBOUND_PATH,
        "/new/UnknownMethod",
    ] {
        let applied = Arc::new(AtomicUsize::new(0));
        let server_applied = Arc::clone(&applied);
        let server = Server::start(move |index, stream| {
            if index == 0 {
                server_applied.fetch_add(1, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(250));
            } else {
                reply(stream);
            }
        });
        let error = unary_with_policy(server.address, path, &[], None, policy()).unwrap_err();
        assert!(error.contains("attempts=1"), "{error}");
        assert_eq!(server.count(), 1);
        assert_eq!(applied.load(Ordering::Relaxed), 1);
        // The state-based owner may now query the actual state instead of sending
        // another raw mutation. This read uses the same internal retry policy.
        assert!(unary_with_policy(server.address, LIST_INBOUNDS_PATH, &[], None, policy()).is_ok());
        assert_eq!(server.count(), 2);
        assert_eq!(applied.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn does_not_retry_protocol_errors_or_an_eof() {
    for compressed in [false, true] {
        let server = Server::start(move |_, mut stream| {
            if compressed {
                write_frame(&mut stream, 0, END_STREAM, 1, &[1, 0, 0, 0, 0]).unwrap();
            }
        });
        let error = unary_with_policy(server.address, GET_USERS_STATS_PATH, &[], None, policy())
            .unwrap_err();
        assert!(error.contains("attempts=1"), "{error}");
        assert!(!error.contains("XRAY_API_READ_TIMEOUT"), "{error}");
        assert_eq!(server.count(), 1);
    }
}

#[test]
fn classifies_io_errors_by_stage_not_by_display_text() {
    let would_block = || io::Error::from_raw_os_error(libc::EAGAIN);
    assert!(Failure::Io(Stage::Read, would_block()).retryable_read());
    assert!(!Failure::Io(Stage::Write, would_block()).retryable_read());
    assert!(!Failure::Io(Stage::Connect, would_block()).retryable_read());
    assert!(!Failure::Io(Stage::Read, io::Error::from_raw_os_error(libc::EACCES)).retryable_read());
    assert!(
        !Failure::Protocol("Resource temporarily unavailable (os error 11)".to_owned())
            .retryable_read()
    );
}

#[test]
fn outer_deadline_limits_all_attempts_and_forbids_a_new_connection_after_expiry() {
    let server = Server::start(|_, _stream| thread::sleep(Duration::from_millis(250)));
    let started = Instant::now();
    let error = unary_with_policy(
        server.address,
        LIST_INBOUNDS_PATH,
        &[],
        Some(started + Duration::from_millis(120)),
        policy(),
    )
    .unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(350));
    assert!(error.contains("XRAY_API_READ_TIMEOUT"), "{error}");
    assert!(server.count() <= 2);

    let calls = server.count();
    let error = unary_with_policy(
        server.address,
        LIST_INBOUNDS_PATH,
        &[],
        Some(Instant::now()),
        policy(),
    )
    .unwrap_err();
    assert!(error.contains("deadline exceeded"), "{error}");
    assert_eq!(server.count(), calls);
}

#[test]
fn ping_frames_cannot_refresh_the_overall_deadline() {
    let server = Server::start(|_, mut stream| {
        for _ in 0..100 {
            if write_frame(&mut stream, 6, 0, 0, b"12345678").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    });
    let started = Instant::now();
    let error = unary_with_policy(
        server.address,
        LIST_INBOUNDS_PATH,
        &[],
        Some(started + Duration::from_millis(120)),
        policy(),
    )
    .unwrap_err();
    assert!(error.contains("XRAY_API_READ_TIMEOUT"), "{error}");
    assert!(started.elapsed() < Duration::from_millis(350));
    assert_eq!(server.count(), 1);
}

#[test]
fn scoped_runtime_deadline_covers_multiple_rpcs_and_is_restored_after_unwind() {
    let server = Server::start(|_, _stream| thread::sleep(Duration::from_millis(150)));
    let started = Instant::now();
    {
        let _deadline = deadline_after(Some(Duration::from_millis(80)));
        let error = unary(server.address, LIST_INBOUNDS_PATH, &[], None).unwrap_err();
        assert!(error.contains("XRAY_API_READ_TIMEOUT"), "{error}");
        let calls = server.count();
        let _nested = deadline_after(Some(Duration::from_secs(20)));
        let error = unary(server.address, LIST_INBOUNDS_PATH, &[], None).unwrap_err();
        assert!(error.contains("deadline exceeded"), "{error}");
        assert_eq!(
            server.count(),
            calls,
            "a second RPC escaped the shared budget"
        );
    }
    assert!(started.elapsed() < Duration::from_millis(400));
    let _ = std::panic::catch_unwind(|| {
        let _deadline = deadline_after(Some(Duration::ZERO));
        panic!("test unwind");
    });
    let healthy = Server::start(|_, stream| reply(stream));
    assert!(unary(healthy.address, LIST_INBOUNDS_PATH, &[], None).is_ok());
}
