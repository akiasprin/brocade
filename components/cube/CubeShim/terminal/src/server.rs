//! A Shim-owned service with node-wide advisory slots and bounded terminal state.
use crate::{screen::Screen, *};
use async_trait::async_trait;
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{mpsc, Mutex, Semaphore},
};

pub enum Command {
    Input(Vec<u8>),
    Resize(u16, u16),
    Terminate,
}
pub enum Event {
    Data(Vec<u8>),
    Exit(u32),
}

#[async_trait]
pub trait Guest: Send + Sync + 'static {
    /// Returns only after the Guest PTY is ready. Exit follows the final output and cleanup.
    async fn start(
        &self,
        id: &str,
        cols: u16,
        rows: u16,
        output: mpsc::Sender<Event>,
        admission: Arc<Admission>,
    ) -> Result<mpsc::Sender<Command>, String>;
    /// Termination remains available while a Guest stops reading stdin.
    async fn terminate(&self, _id: &str, commands: &mpsc::Sender<Command>) -> Result<(), String> {
        send(commands, Command::Terminate).await
    }
}

pub struct Admission(File);
impl Admission {
    fn acquire(root: &Path) -> Result<Self, String> {
        for index in 0..MAX_SESSIONS {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(root.join(format!("slot-{index}")))
                .map_err(|e| e.to_string())?;
            let metadata = file.metadata().map_err(|e| e.to_string())?;
            // SAFETY: geteuid has no preconditions.
            if !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
            {
                return Err("unsafe terminal slot".into());
            }
            // SAFETY: file owns a live descriptor. The lock lasts until this Slot is dropped.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err(error.to_string());
            }
        }
        Err("该节点最多保留 3 个终端会话，请先结束已有会话".into())
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        // SAFETY: this descriptor is owned by the Slot and still live during Drop.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

struct Viewer {
    id: String,
    replay: VecDeque<Vec<u8>>,
    live: VecDeque<Vec<u8>>,
    live_bytes: usize,
    resync: bool,
}
struct Session {
    id: String,
    owner: String,
    incarnation: String,
    policy_revision: u64,
    screen: Screen,
    command: mpsc::Sender<Command>,
    viewer: Option<Viewer>,
    exit: Option<u32>,
    faulted: bool,
    slot: Option<Arc<Admission>>,
    attached_at: std::time::Instant,
}
impl Session {
    fn release_completed(&mut self) {
        self.viewer = None;
        self.screen = Screen::new(2, 1);
        self.slot.take();
    }
    fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            running: self.exit.is_none(),
            history_bytes: self.screen.history_bytes(),
            history_truncated: self.screen.truncated(),
        }
    }
    fn viewer(&mut self, attachment: &str) -> Result<&mut Viewer, String> {
        self.viewer
            .as_mut()
            .filter(|v| v.id == attachment)
            .ok_or_else(|| "终端已在其他页面连接，请重新连接".into())
    }
}

pub struct Manager<G: Guest> {
    root: PathBuf,
    guest: Arc<G>,
    sessions: Mutex<BTreeMap<String, Arc<Mutex<Session>>>>,
    creation: Mutex<Option<u64>>,
}
impl<G: Guest> Manager<G> {
    pub fn new(root: PathBuf, guest: G) -> Result<Arc<Self>, String> {
        private_directory(&root)?;
        Ok(Arc::new(Self {
            root,
            guest: Arc::new(guest),
            sessions: Mutex::new(BTreeMap::new()),
            creation: Mutex::new(None),
        }))
    }
    pub async fn handle(self: &Arc<Self>, request: Request) -> Response {
        match self.request(request).await {
            Ok(value) => value,
            Err(message) => Response::Error { message },
        }
    }
    async fn request(self: &Arc<Self>, request: Request) -> Result<Response, String> {
        if request.version != VERSION
            || !valid_id(&request.owner)
            || !valid_id(&request.incarnation)
        {
            return Err("invalid terminal identity or version".into());
        }
        let Request {
            owner,
            incarnation,
            policy_revision,
            operation,
            ..
        } = request;
        if let Operation::Revoke { through_revision } = operation {
            let mut revoked = self.creation.lock().await;
            *revoked = Some(revoked.map_or(through_revision, |old| old.max(through_revision)));
            let sessions: Vec<_> = self.sessions.lock().await.values().cloned().collect();
            let mut failures = 0;
            for session in sessions {
                let s = session.lock().await;
                if s.exit.is_none() && s.policy_revision <= through_revision {
                    if let Err(error) = self.guest.terminate(&s.id, &s.command).await {
                        eprintln!("persistent terminal revocation failed: {error}");
                        failures += 1;
                    }
                }
            }
            return if failures == 0 {
                Ok(Response::Ok)
            } else {
                Err(format!(
                    "terminal revocation unconfirmed for {failures} sessions"
                ))
            };
        }
        if let Operation::Open {
            id,
            fresh,
            exact,
            cols,
            rows,
        } = operation
        {
            if !valid_id(&id) || !valid_size(cols, rows) {
                return Err("invalid terminal creation".into());
            }
            // Serialize create/retry and reserve the node-wide slot before starting any process.
            let revoked = self.creation.lock().await;
            if revoked.is_some_and(|through| policy_revision <= through) {
                return Err("终端访问策略已变更，请重新连接".into());
            }
            let mut sessions = self.sessions.lock().await;
            let mut list = Vec::new();
            let mut resume: Option<(std::time::Instant, String)> = None;
            let mut ended = Vec::new();
            for (key, session) in sessions.iter() {
                let s = session.lock().await;
                if s.exit.is_some() && s.slot.is_none() {
                    ended.push(key.clone());
                }
                if s.owner == owner
                    && s.incarnation == incarnation
                    && s.policy_revision == policy_revision
                {
                    if s.exit.is_none() && resume.as_ref().is_none_or(|(at, _)| *at < s.attached_at)
                    {
                        resume = Some((s.attached_at, s.id.clone()));
                    }
                    list.push(s.info());
                }
            }
            if let Some(existing) = sessions.get(&id) {
                let s = existing.lock().await;
                if s.faulted {
                    return Err("终端清理未确认，请重启实例后重试".into());
                }
                if s.owner != owner
                    || s.incarnation != incarnation
                    || s.policy_revision != policy_revision
                {
                    return Err("terminal identity mismatch".into());
                }
                return Ok(Response::Opened { id, sessions: list });
            }
            if exact {
                return Err("终端会话已结束或实例已重启，请新建会话".into());
            }
            if !fresh {
                if let Some((_, id)) = resume {
                    return Ok(Response::Opened { id, sessions: list });
                }
            }
            // Only completed entries are evicted. A disconnected running session keeps its slot.
            for key in ended {
                sessions.remove(&key);
            }
            drop(sessions);
            let slot = Arc::new(Admission::acquire(&self.root)?);
            let (tx, mut rx) = mpsc::channel(4);
            let command = self.guest.start(&id, cols, rows, tx, slot.clone()).await?;
            let session = Arc::new(Mutex::new(Session {
                id: id.clone(),
                owner,
                incarnation,
                policy_revision,
                screen: Screen::new(cols, rows),
                command,
                viewer: None,
                exit: None,
                faulted: false,
                slot: Some(slot),
                attached_at: std::time::Instant::now(),
            }));
            list.retain(|s| s.running);
            list.push(session.lock().await.info());
            self.sessions
                .lock()
                .await
                .insert(id.clone(), session.clone());
            tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    let mut s = session.lock().await;
                    match event {
                        Event::Data(data) => {
                            s.screen.process(&data);
                            if let Some(v) = &mut s.viewer {
                                if v.live_bytes + data.len() > 64 * 1024 {
                                    v.live.clear();
                                    v.live_bytes = 0;
                                    v.resync = true;
                                } else if !v.resync {
                                    v.live_bytes += data.len();
                                    v.live.push_back(data);
                                }
                            }
                        }
                        Event::Exit(code) => {
                            s.exit = Some(code);
                            if s.viewer.is_none() {
                                s.release_completed();
                            }
                            break;
                        }
                    }
                }
                let mut s = session.lock().await;
                // A missing exit receipt must not free a live process's admission slot.
                if s.exit.is_none() {
                    s.faulted = true;
                    s.viewer = None;
                    return;
                }
                drop(s);
                drop(rx);
                // An abandoned reader must not retain an exited session's history.
                // Keep its slot until final delivery (or this bounded drain grace).
                tokio::time::sleep(Duration::from_secs(30)).await;
                session.lock().await.release_completed();
            });
            return Ok(Response::Opened { id, sessions: list });
        }
        let (id, attachment) = match &operation {
            Operation::Attach { id, attachment }
            | Operation::Read { id, attachment }
            | Operation::Input { id, attachment, .. }
            | Operation::Resize { id, attachment, .. }
            | Operation::Detach { id, attachment }
            | Operation::Terminate { id, attachment } => (id, attachment),
            _ => return Err("invalid terminal operation".into()),
        };
        if !valid_id(id) || !valid_id(attachment) {
            return Err("invalid terminal attachment".into());
        }
        let session = self
            .sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or("终端会话已结束或实例已重启")?;
        let mut s = session.lock().await;
        if s.faulted {
            return Err("终端清理未确认，请重启实例后重试".into());
        }
        if s.owner != owner || s.incarnation != incarnation || s.policy_revision != policy_revision
        {
            return Err("terminal identity mismatch".into());
        }
        match operation {
            Operation::Attach { attachment, .. } => {
                s.attached_at = std::time::Instant::now();
                s.viewer = Some(Viewer {
                    id: attachment,
                    replay: s.screen.snapshot(),
                    live: VecDeque::new(),
                    live_bytes: 0,
                    resync: false,
                });
                let (cols, rows) = s.screen.size();
                Ok(Response::Attached { cols, rows })
            }
            Operation::Read { attachment, .. } => {
                let viewer = s.viewer(&attachment)?;
                // Complete the finite replay before replacing it. Continuous output
                // may overflow live data, but must never rewind an in-flight replay.
                if viewer.resync && viewer.replay.is_empty() {
                    let snapshot = s.screen.snapshot_current();
                    let v = s.viewer(&attachment)?;
                    v.replay = snapshot;
                    v.live.clear();
                    v.live_bytes = 0;
                    v.resync = false;
                }
                let v = s.viewer(&attachment)?;
                let replay = !v.replay.is_empty();
                let mut data = if replay {
                    v.replay.pop_front().unwrap_or_default()
                } else {
                    let data = v.live.pop_front().unwrap_or_default();
                    v.live_bytes -= data.len();
                    data
                };
                if data.len() > MAX_DATA {
                    let rest = data.split_off(MAX_DATA);
                    if replay {
                        v.replay.push_front(rest);
                    } else {
                        v.live_bytes += rest.len();
                        v.live.push_front(rest);
                    }
                }
                while data.len() < MAX_DATA {
                    let queue = if replay { &mut v.replay } else { &mut v.live };
                    let Some(mut next) = queue.pop_front() else {
                        break;
                    };
                    if !replay {
                        v.live_bytes -= next.len();
                    }
                    let available = MAX_DATA - data.len();
                    if next.len() > available {
                        let rest = next.split_off(available);
                        if !replay {
                            v.live_bytes += rest.len();
                        }
                        queue.push_front(rest);
                    }
                    data.extend(next);
                }
                let replay_done = replay && v.replay.is_empty();
                let drained = v.replay.is_empty() && v.live.is_empty() && !v.resync;
                let exit = if drained { s.exit } else { None };
                if exit.is_some() {
                    s.release_completed();
                }
                Ok(Response::Data {
                    data,
                    replay,
                    replay_done,
                    exit,
                })
            }
            Operation::Input {
                attachment, data, ..
            } => {
                s.viewer(&attachment)?;
                if data.len() > MAX_DATA || s.exit.is_some() {
                    return Err("invalid terminal input".into());
                }
                send(&s.command, Command::Input(data)).await?;
                Ok(Response::Ok)
            }
            Operation::Resize {
                attachment,
                cols,
                rows,
                ..
            } => {
                s.viewer(&attachment)?;
                if !valid_size(cols, rows) {
                    return Err("invalid terminal size".into());
                }
                if s.exit.is_none() {
                    send(&s.command, Command::Resize(cols, rows)).await?;
                }
                s.screen.resize(cols, rows);
                Ok(Response::Ok)
            }
            Operation::Detach { attachment, .. } => {
                // Stale disconnects cannot detach a newer connection.
                if s.viewer.as_ref().is_some_and(|v| v.id == attachment) {
                    s.viewer = None;
                    if s.exit.is_some() {
                        s.release_completed();
                    }
                }
                Ok(Response::Ok)
            }
            Operation::Terminate { attachment, .. } => {
                s.viewer(&attachment)?;
                if s.exit.is_none() {
                    self.guest.terminate(&s.id, &s.command).await?;
                }
                Ok(Response::Ok)
            }
            _ => Err("invalid terminal operation".into()),
        }
    }
}
async fn send(tx: &mpsc::Sender<Command>, command: Command) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(5), tx.send(command))
        .await
        .map_err(|_| "terminal command timed out")?
        .map_err(|_| "terminal process unavailable".into())
}

pub fn private_directory(path: &Path) -> Result<(), String> {
    match std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
    {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.to_string()),
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    // SAFETY: geteuid has no preconditions.
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err("unsafe terminal directory".into());
    }
    Ok(())
}

pub async fn serve<G: Guest>(manager: Arc<Manager<G>>, runtime: &str) -> Result<(), String> {
    if !valid_runtime(runtime) {
        return Err("invalid terminal runtime".into());
    }
    let path = manager.root.join(socket_name(runtime));
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        // SAFETY: geteuid has no preconditions.
        if !metadata.file_type().is_socket()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err("unsafe existing terminal socket".into());
        }
        match UnixStream::connect(&path).await {
            Ok(_) => return Err("terminal socket already active".into()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                std::fs::remove_file(&path).map_err(|error| error.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
    let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    let _socket = SocketGuard {
        path: path.clone(),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    let capacity = Arc::new(Semaphore::new(16));
    loop {
        let permit = capacity
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| e.to_string())?;
        let (mut socket, _) = listener.accept().await.map_err(|e| e.to_string())?;
        // SAFETY: geteuid has no preconditions.
        if socket.peer_cred().map_err(|e| e.to_string())?.uid() != unsafe { libc::geteuid() } {
            continue;
        }
        let manager = manager.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result = tokio::time::timeout(Duration::from_secs(15), async {
                let mut data = Vec::new();
                (&mut socket)
                    .take(MAX_FRAME as u64 + 1)
                    .read_to_end(&mut data)
                    .await
                    .map_err(|e| e.to_string())?;
                if data.len() > MAX_FRAME {
                    return Err("terminal request exceeds limit".to_string());
                }
                let request = serde_json::from_slice(&data)
                    .map_err(|_| "invalid terminal request".to_string())?;
                let response = manager.handle(request).await;
                let data = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
                if data.len() > MAX_FRAME {
                    return Err("terminal response exceeds limit".to_string());
                }
                socket.write_all(&data).await.map_err(|e| e.to_string())?;
                Ok::<(), String>(())
            })
            .await;
            if let Ok(Err(error)) = result {
                eprintln!("terminal request failed: {error}");
            }
        });
    }
}

struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path) {
            if metadata.dev() == self.device && metadata.ino() == self.inode {
                if let Err(error) = std::fs::remove_file(&self.path) {
                    eprintln!("terminal socket cleanup failed: {error}");
                }
            }
        }
    }
}

/// Called by the runtime's existing sandbox destruction path, including crash reaping.
/// Only the exact runtime's private socket and known FIFO directories are removed.
pub fn cleanup_runtime(root: &Path, runtime: &str) -> Result<(), String> {
    if !valid_runtime(runtime) {
        return Err("invalid terminal runtime".into());
    }
    match std::fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
        Ok(_) => private_directory(root)?,
    }
    let name = socket_name(runtime);
    let prefix = format!("io-{name}-");
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if file_name != name && !file_name.strip_prefix(&prefix).is_some_and(valid_id) {
            continue;
        }
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        // SAFETY: geteuid has no preconditions.
        if file_name == name {
            if !metadata.file_type().is_socket()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
            {
                return Err("unsafe terminal socket cleanup".into());
            }
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        } else if file_name.strip_prefix(&prefix).is_some_and(valid_id) {
            private_directory(&path)?;
            for fifo in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
                let fifo = fifo.map_err(|e| e.to_string())?;
                let metadata = std::fs::symlink_metadata(fifo.path()).map_err(|e| e.to_string())?;
                if !matches!(fifo.file_name().to_str(), Some("stdin" | "stdout"))
                    || !metadata.file_type().is_fifo()
                    || metadata.uid() != unsafe { libc::geteuid() }
                    || metadata.mode() & 0o077 != 0
                {
                    return Err("unsafe terminal FIFO cleanup".into());
                }
                std::fs::remove_file(fifo.path()).map_err(|e| e.to_string())?;
            }
            std::fs::remove_dir(path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Clients use a half-close to delimit each bounded request.
pub async fn call(path: &Path, request: &Request) -> Result<Response, String> {
    let mut socket = UnixStream::connect(path).await.map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    socket.write_all(&bytes).await.map_err(|e| e.to_string())?;
    socket.shutdown().await.map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    socket
        .take(MAX_FRAME as u64 + 1)
        .read_to_end(&mut data)
        .await
        .map_err(|e| e.to_string())?;
    if data.len() > MAX_FRAME {
        return Err("terminal response exceeds limit".into());
    }
    serde_json::from_slice(&data).map_err(|_| "invalid terminal response".into())
}
