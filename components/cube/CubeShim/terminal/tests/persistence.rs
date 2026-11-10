#![cfg(feature = "server")]
use async_trait::async_trait;
use cube_terminal::{
    server::{Admission, Command, Event, Guest, Manager},
    *,
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, Mutex};

#[derive(Clone, Default)]
struct FakeGuest {
    starts: Arc<AtomicUsize>,
    outputs: Arc<Mutex<BTreeMap<String, mpsc::Sender<Event>>>>,
}
#[async_trait]
impl Guest for FakeGuest {
    async fn start(
        &self,
        id: &str,
        _: u16,
        _: u16,
        output: mpsc::Sender<Event>,
        admission: Arc<Admission>,
    ) -> Result<mpsc::Sender<Command>, String> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.outputs.lock().await.insert(id.into(), output.clone());
        let (tx, mut rx) = mpsc::channel(4);
        tokio::spawn(async move {
            let mut admission = Some(admission);
            while let Some(command) = rx.recv().await {
                match command {
                    Command::Input(bytes) => {
                        output.send(Event::Data(bytes)).await.unwrap();
                    }
                    Command::Terminate => {
                        drop(admission.take());
                        output.send(Event::Exit(0)).await.unwrap();
                        break;
                    }
                    Command::Resize(..) => {}
                }
            }
        });
        Ok(tx)
    }
}
fn private_root() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}
fn id(n: usize) -> String {
    format!("{n:064x}")
}
fn req(owner: usize, operation: Operation) -> Request {
    Request {
        version: VERSION,
        owner: id(owner),
        incarnation: id(99),
        policy_revision: 0,
        operation,
    }
}
async fn open(manager: &Arc<Manager<FakeGuest>>, n: usize, owner: usize, fresh: bool) -> Response {
    manager
        .handle(req(
            owner,
            Operation::Open {
                id: id(n),
                fresh,
                exact: false,
                cols: 80,
                rows: 24,
            },
        ))
        .await
}
async fn attach(manager: &Arc<Manager<FakeGuest>>, n: usize, attachment: usize) -> Response {
    manager
        .handle(req(
            1,
            Operation::Attach {
                id: id(n),
                attachment: id(attachment),
            },
        ))
        .await
}
async fn read_all(manager: &Arc<Manager<FakeGuest>>, n: usize, attachment: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..1000 {
        match manager
            .handle(req(
                1,
                Operation::Read {
                    id: id(n),
                    attachment: id(attachment),
                },
            ))
            .await
        {
            Response::Data {
                data, replay_done, ..
            } => {
                bytes.extend(data);
                if replay_done {
                    return bytes;
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    panic!("replay did not finish")
}

#[tokio::test]
async fn three_sessions_are_shared_across_shims_and_owner_has_no_quota() {
    let root = private_root();
    let guest = FakeGuest::default();
    let a = Manager::new(root.path().into(), guest.clone()).unwrap();
    let b = Manager::new(root.path().into(), guest.clone()).unwrap();
    assert!(matches!(
        open(&a, 10, 1, true).await,
        Response::Opened { .. }
    ));
    assert!(matches!(
        open(&b, 11, 1, true).await,
        Response::Opened { .. }
    ));
    assert!(matches!(
        open(&a, 12, 1, true).await,
        Response::Opened { .. }
    ));
    assert!(matches!(
        open(&b, 13, 2, true).await,
        Response::Error { .. }
    ));
    assert_eq!(guest.starts.load(Ordering::SeqCst), 3);
    let other = private_root();
    let c = Manager::new(other.path().into(), guest.clone()).unwrap();
    assert!(matches!(
        open(&c, 14, 1, true).await,
        Response::Opened { .. }
    ));
    assert_eq!(guest.starts.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn retries_and_default_resume_never_create_a_second_shell() {
    let root = private_root();
    let guest = FakeGuest::default();
    let a = Manager::new(root.path().into(), guest.clone()).unwrap();
    open(&a, 10, 1, true).await;
    open(&a, 10, 1, true).await;
    match open(&a, 11, 1, false).await {
        Response::Opened { id: session, .. } => assert_eq!(session, id(10)),
        other => panic!("{other:?}"),
    }
    assert_eq!(guest.starts.load(Ordering::SeqCst), 1);
    assert!(matches!(
        a.handle(req(
            2,
            Operation::Open {
                id: id(10),
                fresh: true,
                exact: false,
                cols: 80,
                rows: 24
            }
        ))
        .await,
        Response::Error { .. }
    ));
    assert!(matches!(
        a.handle(req(
            1,
            Operation::Open {
                id: id(22),
                fresh: false,
                exact: true,
                cols: 80,
                rows: 24
            }
        ))
        .await,
        Response::Error { .. }
    ));
}

#[tokio::test]
async fn detach_keeps_process_and_output_and_new_attach_fences_old_writer() {
    let root = private_root();
    let guest = FakeGuest::default();
    let a = Manager::new(root.path().into(), guest.clone()).unwrap();
    open(&a, 10, 1, true).await;
    attach(&a, 10, 100).await;
    a.handle(req(
        1,
        Operation::Detach {
            id: id(10),
            attachment: id(100),
        },
    ))
    .await;
    guest.outputs.lock().await[&id(10)]
        .send(Event::Data("工作目录保持\r\n".as_bytes().to_vec()))
        .await
        .unwrap();
    tokio::task::yield_now().await;
    attach(&a, 10, 101).await;
    assert!(String::from_utf8(read_all(&a, 10, 101).await)
        .unwrap()
        .contains("工作目录保持"));
    assert!(matches!(
        a.handle(req(
            1,
            Operation::Input {
                id: id(10),
                attachment: id(100),
                data: b"unexpected\n".to_vec()
            }
        ))
        .await,
        Response::Error { .. }
    ));
    a.handle(req(
        1,
        Operation::Detach {
            id: id(10),
            attachment: id(100),
        },
    ))
    .await;
    assert!(matches!(
        a.handle(req(
            1,
            Operation::Input {
                id: id(10),
                attachment: id(101),
                data: b"expected\n".to_vec()
            }
        ))
        .await,
        Response::Ok
    ));
    assert_eq!(guest.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn exit_releases_slot_and_final_bytes_are_delivered_before_exit() {
    let root = private_root();
    let guest = FakeGuest::default();
    let a = Manager::new(root.path().into(), guest.clone()).unwrap();
    for n in 10..13 {
        open(&a, n, 1, true).await;
    }
    attach(&a, 10, 100).await;
    read_all(&a, 10, 100).await;
    let output = guest.outputs.lock().await[&id(10)].clone();
    output
        .send(Event::Data(b"FINAL-LINE\r\n".to_vec()))
        .await
        .unwrap();
    a.handle(req(
        1,
        Operation::Terminate {
            id: id(10),
            attachment: id(100),
        },
    ))
    .await;
    let (bytes, code) = tokio::time::timeout(Duration::from_secs(2), async {
        let mut bytes = Vec::new();
        loop {
            match a
                .handle(req(
                    1,
                    Operation::Read {
                        id: id(10),
                        attachment: id(100),
                    },
                ))
                .await
            {
                Response::Data { data, exit, .. } => {
                    bytes.extend(data);
                    if let Some(code) = exit {
                        break (bytes, code);
                    }
                }
                other => panic!("{other:?}"),
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(bytes.ends_with(b"FINAL-LINE\r\n"));
    assert_eq!(code, 0);
    assert!(matches!(
        open(&a, 13, 1, true).await,
        Response::Opened { .. }
    ));
}

#[tokio::test]
async fn exact_incarnation_and_owner_are_required_for_every_attachment() {
    let root = private_root();
    let a = Manager::new(root.path().into(), FakeGuest::default()).unwrap();
    open(&a, 10, 1, true).await;
    assert!(matches!(
        a.handle(req(
            2,
            Operation::Attach {
                id: id(10),
                attachment: id(100)
            }
        ))
        .await,
        Response::Error { .. }
    ));
    let mut request = req(
        1,
        Operation::Attach {
            id: id(10),
            attachment: id(100),
        },
    );
    request.incarnation = id(98);
    assert!(matches!(a.handle(request).await, Response::Error { .. }));
    let mut request = req(
        1,
        Operation::Attach {
            id: "../x".into(),
            attachment: id(100),
        },
    );
    request.version = 0;
    assert!(matches!(a.handle(request).await, Response::Error { .. }));
}

#[tokio::test]
async fn private_socket_survives_client_disconnect_and_replays_on_fresh_connection() {
    let root = private_root();
    let guest = FakeGuest::default();
    let manager = Manager::new(root.path().into(), guest.clone()).unwrap();
    let a = manager.clone();
    let server = tokio::spawn(async move { cube_terminal::server::serve(a, "runtime-one").await });
    let path = root.path().join(socket_name("runtime-one"));
    tokio::time::timeout(Duration::from_secs(2), async {
        while !path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let response = cube_terminal::server::call(
        &path,
        &req(
            1,
            Operation::Open {
                id: id(10),
                fresh: false,
                exact: false,
                cols: 80,
                rows: 24,
            },
        ),
    )
    .await
    .unwrap();
    assert!(matches!(response, Response::Opened { .. }));
    guest.outputs.lock().await[&id(10)]
        .send(Event::Data(b"alive across transports".to_vec()))
        .await
        .unwrap();
    tokio::task::yield_now().await;
    cube_terminal::server::call(
        &path,
        &req(
            1,
            Operation::Attach {
                id: id(10),
                attachment: id(100),
            },
        ),
    )
    .await
    .unwrap();
    assert!(String::from_utf8(read_all(&manager, 10, 100).await)
        .unwrap()
        .contains("alive across transports"));
    assert_eq!(guest.starts.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn saturated_buffers_remain_bounded_without_a_browser() {
    fn rss() -> usize {
        let text = std::fs::read_to_string("/proc/self/status").unwrap();
        text.lines()
            .find(|line| line.starts_with("VmRSS:"))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<usize>()
            .unwrap()
            * 1024
    }
    let root = private_root();
    let guest = FakeGuest::default();
    let a = Manager::new(root.path().into(), guest.clone()).unwrap();
    let before = rss();
    for n in 10..13 {
        open(&a, n, 1, true).await;
    }
    let data = format!("{}\r\n", "x".repeat(78)).repeat(200).into_bytes();
    for n in 10..13 {
        let output = guest.outputs.lock().await[&id(n)].clone();
        for _ in 0..350 {
            output.send(Event::Data(data.clone())).await.unwrap();
        }
    }
    let infos = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Response::Opened { sessions, .. } = open(&a, 10, 1, false).await {
                if sessions.iter().all(|session| session.history_truncated) {
                    break sessions;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(infos.len(), 3);
    assert!(infos
        .iter()
        .all(|session| session.history_bytes <= HISTORY_BYTES));
    println!(
        "three_shim_sessions_history_bytes={} broker_test_rss_delta_bytes={}",
        infos
            .iter()
            .map(|session| session.history_bytes)
            .sum::<usize>(),
        rss().saturating_sub(before)
    );
}

#[tokio::test]
async fn revocation_survives_reenable_and_rejects_late_old_offers() {
    let root = private_root();
    let guest = FakeGuest::default();
    let manager = Manager::new(root.path().into(), guest.clone()).unwrap();
    for (n, revision) in [(10, 1), (11, 3)] {
        let mut request = req(
            1,
            Operation::Open {
                id: id(n),
                fresh: true,
                exact: false,
                cols: 80,
                rows: 24,
            },
        );
        request.policy_revision = revision;
        assert!(matches!(
            manager.handle(request).await,
            Response::Opened { .. }
        ));
    }
    assert!(matches!(
        manager
            .handle(req(
                1,
                Operation::Revoke {
                    through_revision: 2
                }
            ))
            .await,
        Response::Ok
    ));
    let mut stale = req(
        1,
        Operation::Open {
            id: id(12),
            fresh: true,
            exact: false,
            cols: 80,
            rows: 24,
        },
    );
    stale.policy_revision = 1;
    assert!(matches!(
        manager.handle(stale).await,
        Response::Error { .. }
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        while !guest.outputs.lock().await[&id(10)].is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!guest.outputs.lock().await[&id(11)].is_closed());
    let mut current = req(
        1,
        Operation::Open {
            id: id(11),
            fresh: false,
            exact: true,
            cols: 80,
            rows: 24,
        },
    );
    current.policy_revision = 3;
    assert!(matches!(
        manager.handle(current).await,
        Response::Opened { .. }
    ));
}

#[tokio::test]
async fn socket_abort_and_runtime_reaping_remove_only_owned_namespaces() {
    use std::os::unix::fs::PermissionsExt;
    let root = private_root();
    let manager = Manager::new(root.path().into(), FakeGuest::default()).unwrap();
    let server = tokio::spawn(async move { cube_terminal::server::serve(manager, "owned").await });
    let socket = root.path().join(socket_name("owned"));
    tokio::time::timeout(Duration::from_secs(2), async {
        while !socket.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
    assert!(!socket.exists());
    let owned = root
        .path()
        .join(format!("io-{}-{}", socket_name("owned"), id(1)));
    let other = root
        .path()
        .join(format!("io-{}-{}", socket_name("other"), id(1)));
    std::fs::create_dir(&owned).unwrap();
    std::fs::set_permissions(&owned, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir(&other).unwrap();
    cube_terminal::server::cleanup_runtime(root.path(), "owned").unwrap();
    assert!(!owned.exists());
    assert!(other.exists());
    std::fs::create_dir(&owned).unwrap();
    std::fs::set_permissions(&owned, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(&other, owned.join("stdout")).unwrap();
    assert!(cube_terminal::server::cleanup_runtime(root.path(), "owned").is_err());
    assert!(other.exists());
}

#[tokio::test]
async fn termination_bypasses_a_saturated_guest_input_channel() {
    struct BlockedGuest(Arc<tokio::sync::Notify>);
    #[async_trait]
    impl Guest for BlockedGuest {
        async fn start(
            &self,
            _: &str,
            _: u16,
            _: u16,
            output: mpsc::Sender<Event>,
            admission: Arc<Admission>,
        ) -> Result<mpsc::Sender<Command>, String> {
            let (tx, rx) = mpsc::channel(1);
            let stop = self.0.clone();
            tokio::spawn(async move {
                let _rx = rx;
                stop.notified().await;
                drop(admission);
                output.send(Event::Exit(137)).await.unwrap();
            });
            Ok(tx)
        }
        async fn terminate(&self, _: &str, _: &mpsc::Sender<Command>) -> Result<(), String> {
            self.0.notify_one();
            Ok(())
        }
    }
    let root = private_root();
    let manager = Manager::new(
        root.path().into(),
        BlockedGuest(Arc::new(tokio::sync::Notify::new())),
    )
    .unwrap();
    assert!(matches!(
        manager
            .handle(req(
                1,
                Operation::Open {
                    id: id(10),
                    fresh: true,
                    exact: false,
                    cols: 80,
                    rows: 24
                }
            ))
            .await,
        Response::Opened { .. }
    ));
    manager
        .handle(req(
            1,
            Operation::Attach {
                id: id(10),
                attachment: id(100),
            },
        ))
        .await;
    assert!(matches!(
        manager
            .handle(req(
                1,
                Operation::Input {
                    id: id(10),
                    attachment: id(100),
                    data: vec![b'x'; MAX_DATA]
                }
            ))
            .await,
        Response::Ok
    ));
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        manager.handle(req(
            1,
            Operation::Terminate {
                id: id(10),
                attachment: id(100),
            },
        )),
    )
    .await
    .unwrap();
    assert!(matches!(response, Response::Ok));
}

#[tokio::test]
async fn unconfirmed_guest_loss_cannot_reconnect_or_release_admission() {
    struct LostGuest;
    #[async_trait]
    impl Guest for LostGuest {
        async fn start(
            &self,
            _: &str,
            _: u16,
            _: u16,
            _: mpsc::Sender<Event>,
            _: Arc<Admission>,
        ) -> Result<mpsc::Sender<Command>, String> {
            let (tx, _rx) = mpsc::channel(1);
            Ok(tx)
        }
    }
    let root = private_root();
    let manager = Manager::new(root.path().into(), LostGuest).unwrap();
    for n in 10..13 {
        assert!(matches!(
            manager
                .handle(req(
                    1,
                    Operation::Open {
                        id: id(n),
                        fresh: true,
                        exact: false,
                        cols: 80,
                        rows: 24
                    }
                ))
                .await,
            Response::Opened { .. }
        ));
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let result = manager
                .handle(req(
                    1,
                    Operation::Attach {
                        id: id(10),
                        attachment: id(100),
                    },
                ))
                .await;
            if matches!(result, Response::Error { .. }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        manager
            .handle(req(
                1,
                Operation::Open {
                    id: id(13),
                    fresh: true,
                    exact: false,
                    cols: 80,
                    rows: 24
                }
            ))
            .await,
        Response::Error { .. }
    ));
}

#[tokio::test]
async fn replay_completes_despite_continuous_output() {
    let root = private_root();
    let guest = FakeGuest::default();
    let manager = Manager::new(root.path().into(), guest.clone()).unwrap();
    open(&manager, 10, 1, true).await;
    let output = guest.outputs.lock().await[&id(10)].clone();
    let payload = format!("{}\r\n", "x".repeat(78)).repeat(204).into_bytes();
    for _ in 0..5 {
        output.send(Event::Data(payload.clone())).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    attach(&manager, 10, 100).await;
    let mut resets = 0;
    let mut finished = false;
    for _ in 0..20 {
        let before = match open(&manager, 10, 1, false).await {
            Response::Opened { sessions, .. } => sessions[0].history_bytes,
            _ => panic!(),
        };
        for _ in 0..5 {
            output.send(Event::Data(payload.clone())).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Response::Opened { sessions, .. } = open(&manager, 10, 1, false).await {
                    if sessions[0].history_bytes >= before + 80_000 {
                        break;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        match manager
            .handle(req(
                1,
                Operation::Read {
                    id: id(10),
                    attachment: id(100),
                },
            ))
            .await
        {
            Response::Data {
                data, replay_done, ..
            } => {
                resets += usize::from(data.starts_with(b"\x1bc"));
                finished |= replay_done;
            }
            other => panic!("{other:?}"),
        }
    }
    println!("review replay: {resets} resets in 20 reads, replay completed={finished}");
    assert!(
        finished,
        "replay is restarted on every read, so the browser never enables input"
    );
}

#[tokio::test]
async fn resize_roundtrip_retains_existing_visible_text() {
    let root = private_root();
    let guest = FakeGuest::default();
    let manager = Manager::new(root.path().into(), guest.clone()).unwrap();
    open(&manager, 10, 1, true).await;
    attach(&manager, 10, 100).await;
    read_all(&manager, 10, 100).await;
    let text = "BEGIN-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz-END";
    guest.outputs.lock().await[&id(10)]
        .send(Event::Data(text.as_bytes().to_vec()))
        .await
        .unwrap();
    // A marker in the live queue is also a barrier for consuming the Guest event.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Response::Data { data, .. } = manager
                .handle(req(
                    1,
                    Operation::Read {
                        id: id(10),
                        attachment: id(100),
                    },
                ))
                .await
            {
                if String::from_utf8(data).unwrap().contains(text) {
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for cols in [30, 80] {
        assert!(matches!(
            manager
                .handle(req(
                    1,
                    Operation::Resize {
                        id: id(10),
                        attachment: id(100),
                        cols,
                        rows: 24
                    }
                ))
                .await,
            Response::Ok
        ));
    }
    attach(&manager, 10, 101).await;
    let snapshot = read_all(&manager, 10, 101).await;
    let mut mirror = vt100::Parser::default();
    mirror.process(&snapshot);
    println!("review resized snapshot: {:?}", mirror.screen().contents());
    assert!(
        mirror.screen().contents().contains(text),
        "changing viewport width discarded unrecoverable text"
    );
}

#[tokio::test]
async fn revocation_attempts_every_matching_session() {
    struct FallibleGuest(FakeGuest);
    #[async_trait]
    impl Guest for FallibleGuest {
        async fn start(
            &self,
            id: &str,
            cols: u16,
            rows: u16,
            output: mpsc::Sender<Event>,
            admission: Arc<Admission>,
        ) -> Result<mpsc::Sender<Command>, String> {
            self.0.start(id, cols, rows, output, admission).await
        }
        async fn terminate(
            &self,
            target: &str,
            command: &mpsc::Sender<Command>,
        ) -> Result<(), String> {
            if target == id(10) {
                return Err("first exec cleanup unavailable".into());
            }
            command
                .send(Command::Terminate)
                .await
                .map_err(|e| e.to_string())
        }
    }
    let root = private_root();
    let guest = FakeGuest::default();
    let manager = Manager::new(root.path().into(), FallibleGuest(guest.clone())).unwrap();
    for n in 10..13 {
        assert!(matches!(
            manager
                .handle(req(
                    1,
                    Operation::Open {
                        id: id(n),
                        fresh: true,
                        exact: false,
                        cols: 80,
                        rows: 24
                    }
                ))
                .await,
            Response::Opened { .. }
        ));
    }
    assert!(matches!(
        manager
            .handle(req(
                1,
                Operation::Revoke {
                    through_revision: 2
                }
            ))
            .await,
        Response::Error { .. }
    ));
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    let still_live: Vec<_> = guest
        .outputs
        .lock()
        .await
        .iter()
        .filter(|(_, output)| !output.is_closed())
        .map(|(id, _)| id.clone())
        .collect();
    println!(
        "review revoke: {} live sessions after one kill failure",
        still_live.len()
    );
    assert_eq!(
        still_live,
        vec![id(10)],
        "one failed exec prevents every subsequent termination"
    );
}

#[tokio::test]
async fn delayed_guest_exit_keeps_the_node_slot_until_confirmation() {
    use cube_terminal::process::{self, Process, State};
    struct DelayedProcess {
        killed: Arc<tokio::sync::Notify>,
        exited: Arc<tokio::sync::Notify>,
        deleted: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Process for DelayedProcess {
        async fn kill(&mut self) -> Result<(), String> {
            self.killed.notify_one();
            Ok(())
        }
        async fn wait(&mut self) -> Result<u32, String> {
            self.exited.notified().await;
            Ok(137)
        }
        async fn close_io(&mut self) -> Result<(), String> {
            Ok(())
        }
        async fn delete(&mut self) -> Result<(), String> {
            self.deleted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    #[derive(Clone)]
    struct DelayedGuest {
        killed: Arc<tokio::sync::Notify>,
        exited: Arc<tokio::sync::Notify>,
        deleted: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Guest for DelayedGuest {
        async fn start(
            &self,
            _: &str,
            _: u16,
            _: u16,
            output: mpsc::Sender<Event>,
            admission: Arc<Admission>,
        ) -> Result<mpsc::Sender<Command>, String> {
            let (tx, mut rx) = mpsc::channel(4);
            let guest = self.clone();
            tokio::spawn(async move {
                let mut process = DelayedProcess {
                    killed: guest.killed,
                    exited: guest.exited,
                    deleted: guest.deleted,
                };
                while let Some(command) = rx.recv().await {
                    if matches!(command, Command::Terminate) {
                        let code = process::cleanup(&mut process, State::Started)
                            .await
                            .unwrap()
                            .unwrap();
                        drop(admission);
                        output.send(Event::Exit(code)).await.unwrap();
                        break;
                    }
                }
            });
            Ok(tx)
        }
    }
    let root = private_root();
    let guest = DelayedGuest {
        killed: Arc::new(tokio::sync::Notify::new()),
        exited: Arc::new(tokio::sync::Notify::new()),
        deleted: Arc::new(AtomicUsize::new(0)),
    };
    let manager = Manager::new(root.path().into(), guest.clone()).unwrap();
    let other = Manager::new(root.path().into(), FakeGuest::default()).unwrap();
    for n in 10..13 {
        assert!(matches!(
            manager
                .handle(req(
                    1,
                    Operation::Open {
                        id: id(n),
                        fresh: true,
                        exact: false,
                        cols: 80,
                        rows: 24
                    }
                ))
                .await,
            Response::Opened { .. }
        ));
    }
    manager
        .handle(req(
            1,
            Operation::Attach {
                id: id(10),
                attachment: id(100),
            },
        ))
        .await;
    manager
        .handle(req(
            1,
            Operation::Terminate {
                id: id(10),
                attachment: id(100),
            },
        ))
        .await;
    tokio::time::timeout(Duration::from_secs(1), guest.killed.notified())
        .await
        .unwrap();
    assert_eq!(guest.deleted.load(Ordering::SeqCst), 0);
    assert!(matches!(
        open(&other, 20, 2, true).await,
        Response::Error { .. }
    ));
    guest.exited.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Response::Data {
                exit: Some(137), ..
            } = manager
                .handle(req(
                    1,
                    Operation::Read {
                        id: id(10),
                        attachment: id(100),
                    },
                ))
                .await
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(guest.deleted.load(Ordering::SeqCst), 1);
    assert!(matches!(
        open(&other, 20, 2, true).await,
        Response::Opened { .. }
    ));
}
