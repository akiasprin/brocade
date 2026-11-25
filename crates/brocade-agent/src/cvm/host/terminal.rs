//! A separate local service holds PTYs across Agent upgrades. It has no HTTP listener.
use super::*;
use cube_terminal::server::{Admission, Command as Input, Event, Guest, Manager};
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    process::{Command, Stdio},
};
use tokio::sync::mpsc;

pub(super) struct Account {
    uid: u32,
    gid: u32,
    home: String,
    shell: String,
    user: String,
}
pub(super) fn account(user: &str) -> Result<Account> {
    if user.is_empty()
        || user.len() > 32
        || !user
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    {
        return Err("invalid host terminal account".into());
    }
    let raw = command("getent", &["passwd", user])?;
    let fields = raw.trim_end().split(':').collect::<Vec<_>>();
    if fields.len() != 7
        || fields[0] != user
        || !fields[5].starts_with('/')
        || !fields[6].starts_with('/')
        || fields[6].ends_with("nologin")
        || fields[6].ends_with("/false")
    {
        return Err("host terminal account must exist and have a login shell".into());
    }
    Ok(Account {
        uid: fields[2].parse().map_err(|_| "invalid terminal UID")?,
        gid: fields[3].parse().map_err(|_| "invalid terminal GID")?,
        home: fields[5].into(),
        shell: fields[6].into(),
        user: user.into(),
    })
}
type Processes = Arc<Mutex<BTreeMap<String, Arc<std::sync::atomic::AtomicBool>>>>;
struct Local {
    config: Config,
    processes: Processes,
}
#[async_trait::async_trait]
impl Guest for Local {
    async fn start(
        &self,
        id: &str,
        cols: u16,
        rows: u16,
        output: mpsc::Sender<Event>,
        admission: Arc<Admission>,
    ) -> Result<mpsc::Sender<Input>> {
        let account = account(&self.config.terminal_user)?;
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut processes = self
                .processes
                .lock()
                .map_err(|_| "native terminal process state unavailable")?;
            processes.retain(|_, done| !done.load(std::sync::atomic::Ordering::Acquire));
            if processes.len() >= cube_terminal::MAX_SESSIONS {
                return Err("native terminal process capacity exceeded".into());
            }
            processes.insert(id.to_owned(), finished.clone());
        }

        let (tx, rx) = mpsc::channel(4);
        let (ready, started) = tokio::sync::oneshot::channel();
        let runtime = tokio::runtime::Handle::current();
        std::thread::Builder::new()
            .name("host-pty".into())
            .spawn(move || {
                let _admission = admission;
                if let Err(error) = pty(
                    account,
                    (cols, rows),
                    rx,
                    &output,
                    ready,
                    &runtime,
                    &finished,
                ) {
                    eprintln!("host PTY ended: {error}");
                }
                finished.store(true, std::sync::atomic::Ordering::Release);
            })
            .map_err(|_| "cannot start host PTY worker")?;
        tokio::time::timeout(Duration::from_secs(5), started)
            .await
            .map_err(|_| "host PTY startup timed out")?
            .map_err(|_| "host PTY failed to start")??;
        Ok(tx)
    }
    async fn terminate(&self, id: &str, commands: &mpsc::Sender<Input>) -> Result<()> {
        let done = self
            .processes
            .lock()
            .map_err(|_| "native terminal process state unavailable")?
            .get(id)
            .cloned();
        let Some(done) = done else {
            return Ok(());
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            if !done.load(std::sync::atomic::Ordering::Acquire) {
                let _ = commands.send(Input::Terminate).await;
            }
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "native shell exit has not been confirmed".into())
    }
}
fn resize(fd: i32, cols: u16, rows: u16) -> Result<()> {
    if !cube_terminal::valid_size(cols, rows) {
        return Err("invalid host terminal dimensions".into());
    }
    let size = libc::winsize {
        ws_col: cols,
        ws_row: rows,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: fd owns a PTY and size is a correctly initialized winsize.
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &size) } < 0 {
        return Err("host PTY resize failed".into());
    }
    Ok(())
}
fn allocate_pty() -> Result<(File, File)> {
    // Both endpoints must be close-on-exec at creation: another terminal thread
    // can spawn a login process before a later F_SETFD could protect these FDs.
    let flags = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
    // SAFETY: posix_openpt returns a newly owned descriptor or -1.
    let fd = unsafe { libc::posix_openpt(flags) };
    if fd < 0 {
        return Err("cannot allocate host PTY master".into());
    }
    // SAFETY: fd is a newly opened master, owned exactly once by this File.
    let master = unsafe { File::from_raw_fd(fd) };
    // SAFETY: the owned descriptor is a valid PTY master.
    if unsafe { libc::grantpt(master.as_raw_fd()) } < 0
        || unsafe { libc::unlockpt(master.as_raw_fd()) } < 0
    {
        return Err("cannot prepare host PTY slave".into());
    }
    // SAFETY: TIOCGPTPEER opens this master's slave with the supplied open flags.
    let fd = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGPTPEER, flags) };
    if fd < 0 {
        return Err("cannot allocate host PTY slave".into());
    }
    // SAFETY: the successful peer-open ioctl returns a newly owned descriptor.
    let slave = unsafe { File::from_raw_fd(fd) };
    Ok((master, slave))
}
fn pty(
    account: Account,
    size: (u16, u16),
    mut input: mpsc::Receiver<Input>,
    output: &mpsc::Sender<Event>,
    ready: tokio::sync::oneshot::Sender<Result<()>>,
    runtime: &tokio::runtime::Handle,
    finished: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    let (cols, rows) = size;
    let (mut master, slave) = allocate_pty()?;
    resize(master.as_raw_fd(), cols, rows)?;
    // SAFETY: fcntl changes flags only on the newly owned PTY master.
    if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return Err("cannot make host PTY nonblocking".into());
    }

    let user =
        std::ffi::CString::new(account.user.clone()).map_err(|_| "invalid terminal account")?;
    let parent = std::process::id() as libc::pid_t;
    let mut process = Command::new(&account.shell);
    process
        .arg("-l")
        .current_dir(&account.home)
        .env_clear()
        .env("HOME", &account.home)
        .env("USER", &account.user)
        .env("LOGNAME", &account.user)
        .env("TERM", "xterm-256color")
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .stdin(Stdio::from(
            slave.try_clone().map_err(|_| "cannot clone PTY")?,
        ))
        .stdout(Stdio::from(
            slave.try_clone().map_err(|_| "cannot clone PTY")?,
        ))
        .stderr(Stdio::from(slave));
    // SAFETY: child-only, allocation-free syscalls establish its controlling terminal,
    // supplementary groups and UID/GID before executing the preselected login shell.
    unsafe {
        process.pre_exec(move || {
            if libc::setsid() < 0
                || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0
                || ((libc::geteuid() != account.uid || libc::getegid() != account.gid)
                    && (libc::initgroups(user.as_ptr(), account.gid) < 0
                        || libc::setgid(account.gid) < 0
                        || libc::setuid(account.uid) < 0))
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGHUP, 0, 0, 0) < 0
                || libc::getppid() != parent
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = process
        .spawn()
        .map_err(|_| "host login shell could not start")?;
    let pid = child.id() as libc::pid_t;
    let _ = ready.send(Ok(()));
    let send = |event| {
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), output.send(event))
                .await
                .is_ok_and(|r| r.is_ok())
        })
    };
    let mut buffer = [0u8; 16 * 1024];
    let mut pending = std::collections::VecDeque::<u8>::new();
    let mut progress = std::time::Instant::now();
    let result = (|| -> Result<()> {
        loop {
            match input.try_recv() {
                Ok(Input::Input(bytes)) => {
                    if pending.len() + bytes.len() > 64 * 1024 {
                        return Err("host PTY input exceeds backpressure limit".into());
                    }
                    if pending.is_empty() {
                        progress = std::time::Instant::now();
                    }
                    pending.extend(bytes);
                }
                Ok(Input::Resize(c, r)) => resize(master.as_raw_fd(), c, r)?,
                Ok(Input::Terminate) | Err(mpsc::error::TryRecvError::Disconnected) => break,
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
            if !pending.is_empty() {
                match master.write(pending.as_slices().0) {
                    Ok(n) => {
                        pending.drain(..n);
                        progress = std::time::Instant::now();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => return Err("host PTY input failed".into()),
                }
                if progress.elapsed() > Duration::from_secs(5) {
                    return Err("host PTY input stalled".into());
                }
            }
            let mut poll = libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll owns a single initialized pollfd for the live master.
            let n = unsafe { libc::poll(&mut poll, 1, 50) };
            if n < 0 {
                return Err("host PTY polling failed".into());
            }
            if n > 0 {
                match master.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) if !send(Event::Data(buffer[..n].to_vec())) => break,
                    Ok(_) => {}
                    Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => return Err("host PTY read failed".into()),
                }
            }
            if child
                .try_wait()
                .map_err(|_| "cannot observe host shell")?
                .is_some()
                && n == 0
            {
                break;
            }
        }
        Ok(())
    })();
    // SAFETY: the child created a new session/process group with its own PID.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let status = child
        .wait()
        .map_err(|_| "host shell exit could not be confirmed")?;
    finished.store(true, std::sync::atomic::Ordering::Release);
    send(Event::Exit(status.code().unwrap_or(128) as u32));
    result
}
fn assigned(root: &Path) -> Result<Option<Record>> {
    if !root.exists() {
        return Ok(None);
    }
    let mut active = None;
    for (index, entry) in std::fs::read_dir(root)
        .map_err(|_| "native terminal state unavailable")?
        .enumerate()
    {
        if index >= 8192 {
            return Err("native terminal state exceeds limit".into());
        }
        let path = entry
            .map_err(|_| "native terminal state unavailable")?
            .path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let record: Record = serde_json::from_slice(&read(&path, 256 * 1024)?)
            .map_err(|_| "invalid native terminal assignment")?;
        if !record.deleted && record.desired.power == PowerState::Running {
            if active.is_some() {
                return Err("multiple native terminal owners".into());
            }
            active = Some(record);
        }
    }
    Ok(active)
}
pub(crate) fn run(args: &[String]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if !args.is_empty() {
        return Err("usage: brocade-agent cvm-host-terminal".into());
    }
    let config_path = super::super::backend::host_config_path()?
        .ok_or("host management configuration is unavailable")?;
    let config = load_config(&config_path)?;
    let root = super::super::backend::host_state_path()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "host terminal runtime unavailable")?;
    cube_terminal::server::private_directory(Path::new(cube_terminal::ROOT))?;
    runtime.block_on(async move {
        let ready_path=Path::new(cube_terminal::ROOT).join(format!("host-ready-{}.sock",boot_id()?));
        if ready_path.exists() {
            let metadata=std::fs::symlink_metadata(&ready_path).map_err(|_| "cannot inspect native terminal socket")?;
            use std::os::unix::fs::FileTypeExt;
            if !metadata.file_type().is_socket() || metadata.uid()!=unsafe {libc::geteuid()} || metadata.mode()&0o077!=0 {return Err("native terminal readiness endpoint has unsafe ownership".into());}
            if std::os::unix::net::UnixStream::connect(&ready_path).is_ok() {return Err("native terminal service is already active".into());}
            std::fs::remove_file(&ready_path).map_err(|_| "cannot remove stale native readiness socket")?;
        }
        let ready=tokio::net::UnixListener::bind(&ready_path).map_err(|_| "native terminal service already active")?;
        std::fs::set_permissions(&ready_path,std::fs::Permissions::from_mode(0o600)).map_err(|_| "cannot protect native terminal endpoint")?;
        type Serving = (String, Arc<Manager<Local>>, tokio::task::JoinHandle<Result<()>>);
        let mut current: Option<Serving> = None;
        loop {
            let bound=assigned(&root)?;
            let boot = boot_id()?;
            let target=bound.as_ref().map(|r|runtime_id(&r.desired.id,&boot));
            if current.as_ref().map(|(id,_,_)|id)!=target.as_ref() {
                if let Some((_,manager,task))=current.take() {
                    let response=manager.handle(cube_terminal::Request {version:cube_terminal::VERSION, owner:"0".repeat(64), incarnation:"0".repeat(64), policy_revision:0, operation:cube_terminal::Operation::Revoke {through_revision:u64::MAX}}).await;
                    if !matches!(response,cube_terminal::Response::Ok) {return Err("native terminal revocation was not confirmed".into());}
                    task.abort(); let _=task.await;
                }
                if let Some(target)=target {
                    let manager=Manager::new(PathBuf::from(cube_terminal::ROOT),Local {config:config.clone(), processes: Arc::new(Mutex::new(BTreeMap::new()))})?;
                    let serving=manager.clone(); let id=target.clone();
                    let task=tokio::spawn(async move {cube_terminal::server::serve(serving,&id).await});
                    current=Some((target,manager,task));
                }
            }
            if current.as_ref().is_some_and(|(_,_,task)|task.is_finished()) {return Err("native terminal endpoint stopped".into());}
            tokio::select! {
                accepted=ready.accept()=>{let _=accepted.map_err(|_| "native terminal readiness failed")?;},
                _=tokio::time::sleep(Duration::from_secs(1))=>{},
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_pty_uses_the_existing_filesystem_and_confirms_exit() {
        let root = std::env::temp_dir().join(format!("brocade-native-pty-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("shared.txt");
        let inherited = root.join("inherited-pty-fds.txt");
        let size = root.join("size.txt");
        // Keep another session's endpoints live in the same terminal service.
        let (_other_master, _other_slave) = allocate_pty().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let handle = runtime.handle().clone();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = finished.clone();
        let (input, rx) = mpsc::channel(4);
        let (output, mut events) = mpsc::channel(8);
        let (ready, start) = tokio::sync::oneshot::channel();
        // SAFETY: UID/GID queries have no preconditions. The test never changes identity.
        let account = Account {
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
            home: root.to_str().unwrap().into(),
            shell: "/bin/sh".into(),
            user: "fixture".into(),
        };
        let worker =
            std::thread::spawn(move || pty(account, (80, 24), rx, &output, ready, &handle, &done));
        runtime.block_on(async {
            start.await.unwrap().unwrap();
            input.send(Input::Resize(100, 30)).await.unwrap();
            input
                .send(Input::Input(
                    format!(
                        "test -t 0 && test -t 1 && test -t 2 || exit 7\n\
                         : > {inherited}\n\
                         for fd in /proc/$$/fd/*; do\n\
                           case \"${{fd##*/}}\" in 0|1|2) continue ;; esac\n\
                           target=$(readlink \"$fd\" 2>/dev/null) || continue\n\
                           case \"$target\" in /dev/ptmx|/dev/pts/*) printf '%s -> %s\\n' \"$fd\" \"$target\" >> {inherited} ;; esac\n\
                         done\n\
                         stty size > {size}\n\
                         printf 'host-visible' > {file}\nexit\n",
                        inherited = crate::shell_quote(inherited.to_str().unwrap()),
                        size = crate::shell_quote(size.to_str().unwrap()),
                        file = crate::shell_quote(file.to_str().unwrap()),
                    )
                    .into_bytes(),
                ))
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Some(Event::Exit(code)) = events.recv().await {
                        assert_eq!(code, 0);
                        break;
                    }
                }
            })
            .await
            .unwrap();
        });
        worker.join().unwrap().unwrap();
        assert!(finished.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(std::fs::read_to_string(file).unwrap(), "host-visible");
        assert_eq!(std::fs::read_to_string(size).unwrap().trim(), "30 100");
        assert_eq!(std::fs::read_to_string(inherited).unwrap(), "");
        std::fs::remove_dir_all(root).unwrap();
    }
}
