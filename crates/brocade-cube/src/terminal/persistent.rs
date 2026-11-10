use super::*;
use cube_terminal::{Operation, Request, Response};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

impl Backend {
    pub fn persistent_terminal(&self, offer: wire::Offer) -> Result<TerminalSession> {
        if !wire::valid_session(&offer.session_id)
            || !wire::valid_session(&offer.owner)
            || offer
                .terminal_id
                .as_deref()
                .is_some_and(|id| !wire::valid_session(id))
        {
            return Err("invalid persistent terminal identity".into());
        }
        let inner = self.inner().clone();
        inner.terminal_identity(&offer.target, true)?;
        let (input, commands) = tokio::sync::mpsc::channel(4);
        let (events, output) = tokio::sync::mpsc::channel(2);
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        let worker = inner.clone();
        inner.runtime.spawn(async move {
            if let Err(error) = run(worker, offer, commands, &events, stop).await {
                let _ = events.try_send(TerminalEvent::Output(wire::Output::Error {
                    message: error,
                }));
            }
        });
        Ok(TerminalSession {
            input,
            output,
            cancelled,
        })
    }

    /// Revokes only Shim-managed terminals; unrelated Guest programs are not selected.
    pub fn revoke_persistent_terminals(&self, through_revision: u64) -> Result<()> {
        let inner = self.inner();
        let root = Path::new(cube_terminal::ROOT);
        if !root.exists() {
            return Ok(());
        }
        checked(root, false)?;
        let runtimes = inner
            .state()?
            .records
            .values()
            .filter(|r| !r.deleted)
            .map(|r| r.runtime.clone())
            .collect::<Vec<_>>();
        inner.runtime.block_on(async {
            let mut failures = 0;
            for runtime in runtimes {
                let path = root.join(cube_terminal::socket_name(&runtime));
                if !path.exists() {
                    continue;
                }
                let result = call(
                    &path,
                    &Request {
                        version: cube_terminal::VERSION,
                        owner: "0".repeat(64),
                        incarnation: "0".repeat(64),
                        policy_revision: 0,
                        operation: Operation::Revoke { through_revision },
                    },
                )
                .await
                .and_then(accepted);
                if result.is_err() {
                    failures += 1;
                }
            }
            if failures == 0 {
                Ok(())
            } else {
                Err(format!(
                    "terminal revocation unconfirmed for {failures} runtimes"
                ))
            }
        })
    }
}

async fn run(
    inner: Arc<Inner>,
    offer: wire::Offer,
    mut commands: tokio::sync::mpsc::Receiver<TerminalCommand>,
    events: &tokio::sync::mpsc::Sender<TerminalEvent>,
    cancelled: Arc<AtomicBool>,
) -> Result<()> {
    let incarnation = brocade_core::hash::sha256_hex(
        &serde_json::to_vec(&offer.target).map_err(|_| "invalid terminal target")?,
    );
    let path =
        Path::new(cube_terminal::ROOT).join(cube_terminal::socket_name(&offer.target.runtime_id));
    let request = |operation| Request {
        version: cube_terminal::VERSION,
        owner: offer.owner.clone(),
        incarnation: incarnation.clone(),
        policy_revision: offer.policy_revision,
        operation,
    };
    let requested = offer
        .terminal_id
        .clone()
        .unwrap_or_else(|| offer.session_id.clone());
    let opened = call(
        &path,
        &request(Operation::Open {
            id: requested,
            fresh: offer.new_session,
            exact: offer.terminal_id.is_some() && !offer.new_session,
            cols: 80,
            rows: 24,
        }),
    )
    .await?;
    let (id, sessions) = match opened {
        Response::Opened { id, sessions } => (id, sessions),
        response => {
            accepted(response)?;
            return Err("invalid terminal creation receipt".into());
        }
    };
    let attached = call(
        &path,
        &request(Operation::Attach {
            id: id.clone(),
            attachment: offer.session_id.clone(),
        }),
    )
    .await?;
    let (cols, rows) = match attached {
        Response::Attached { cols, rows } => (cols, rows),
        response => {
            accepted(response)?;
            return Err("invalid terminal attach receipt".into());
        }
    };
    let result = async {
        emit(events, TerminalEvent::Output(wire::Output::Session { id: id.clone(), sessions, cols, rows })).await?;
        emit(events, TerminalEvent::Output(wire::Output::Ready)).await?;
        let mut pending = 0;
        let mut replaying = false;
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        let mut validate = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if cancelled.load(Ordering::Acquire) || events.is_closed() { return Ok(()); }
                    if pending != 0 { continue; }
                    match call(&path, &request(Operation::Read { id: id.clone(), attachment: offer.session_id.clone() })).await? {
                        Response::Data { data, replay, replay_done, exit } => {
                            if replay && !replaying {
                                emit(events, TerminalEvent::Output(wire::Output::Replay { active: true })).await?;
                                replaying = true;
                            }
                            if !data.is_empty() {
                                pending = data.len();
                                emit(events, TerminalEvent::Data(data)).await?;
                            }
                            if replay_done {
                                emit(events, TerminalEvent::Output(wire::Output::Replay { active: false })).await?;
                                replaying = false;
                            }
                            if let Some(code) = exit {
                                emit(events, TerminalEvent::Output(wire::Output::Exit { code })).await?;
                                return Ok(());
                            }
                        }
                        response => { accepted(response)?; return Err("invalid terminal output receipt".into()); }
                    }
                }
                _ = validate.tick() => {
                    let worker = inner.clone(); let target = offer.target.clone();
                    tokio::task::spawn_blocking(move || {
                        let launch = worker.terminal_identity(&target, false)?;
                        worker.terminal_runtime(&target, &launch)
                    }).await.map_err(|_| "terminal validation task failed")??;
                }
                command = commands.recv() => {
                    let operation = match command {
                        Some(TerminalCommand::Data(data)) if data.len() <= wire::MAX_DATA =>
                            Operation::Input { id: id.clone(), attachment: offer.session_id.clone(), data },
                        Some(TerminalCommand::Control(wire::Input::Ack { bytes })) => {
                            if pending == 0 || pending != bytes { return Err("terminal output ACK mismatch".into()); }
                            pending = 0; tick.reset_immediately(); continue;
                        }
                        Some(TerminalCommand::Control(wire::Input::Resize { cols, rows })) =>
                            Operation::Resize { id: id.clone(), attachment: offer.session_id.clone(), cols, rows },
                        Some(TerminalCommand::Control(wire::Input::Close)) =>
                            Operation::Terminate { id: id.clone(), attachment: offer.session_id.clone() },
                        None => return Ok(()),
                        _ => return Err("invalid terminal input".into()),
                    };
                    accepted(call(&path, &request(operation)).await?)?;
                }
            }
        }
    }.await;
    let detached = call(
        &path,
        &request(Operation::Detach {
            id,
            attachment: offer.session_id,
        }),
    )
    .await;
    if let Err(error) = detached {
        eprintln!("persistent terminal detach not confirmed: {error}");
    }
    result
}
async fn emit(
    events: &tokio::sync::mpsc::Sender<TerminalEvent>,
    event: TerminalEvent,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), events.send(event))
        .await
        .map_err(|_| "terminal client is too slow")?
        .map_err(|_| "terminal client disconnected".into())
}
fn accepted(response: Response) -> Result<()> {
    match response {
        Response::Ok => Ok(()),
        Response::Error { message } => Err(message),
        _ => Err("invalid terminal receipt".into()),
    }
}
fn checked(path: &Path, socket: bool) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| "实例运行栈尚不支持持久终端，请更新 CubeShim 并切换实例运行栈")?;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
        || if socket {
            !metadata.file_type().is_socket()
        } else {
            !metadata.is_dir()
        }
    {
        return Err("persistent terminal endpoint ownership is unsafe".into());
    }
    Ok(())
}
async fn call(path: &Path, request: &Request) -> Result<Response> {
    checked(Path::new(cube_terminal::ROOT), false)?;
    checked(path, true)?;
    tokio::time::timeout(Duration::from_secs(12), async {
        let mut socket = UnixStream::connect(path)
            .await
            .map_err(|_| "persistent terminal endpoint unavailable")?;
        // SAFETY: geteuid has no preconditions.
        if socket
            .peer_cred()
            .map_err(|_| "terminal peer unavailable")?
            .uid()
            != unsafe { libc::geteuid() }
        {
            return Err("persistent terminal peer is not trusted".into());
        }
        let bytes = serde_json::to_vec(request).map_err(|_| "invalid terminal request")?;
        if bytes.len() > cube_terminal::MAX_FRAME {
            return Err("terminal request exceeds limit".into());
        }
        socket
            .write_all(&bytes)
            .await
            .map_err(|_| "terminal request failed")?;
        socket
            .shutdown()
            .await
            .map_err(|_| "terminal request failed")?;
        let mut bytes = Vec::new();
        socket
            .take(cube_terminal::MAX_FRAME as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "terminal response failed")?;
        if bytes.len() > cube_terminal::MAX_FRAME {
            return Err("terminal response exceeds limit".into());
        }
        serde_json::from_slice(&bytes).map_err(|_| "invalid terminal response".into())
    })
    .await
    .map_err(|_| "persistent terminal request timed out")?
}
