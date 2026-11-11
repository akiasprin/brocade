//! Persistent terminal I/O belongs to the instance's Shim, never to a browser or Agent.
use crate::{common::utils::Utils, container::exec::Tty, sandbox::sb::SandBox};
use async_trait::async_trait;
use containerd_shim::asynchronous::ExitSignal;
use cube_terminal::{
    process::{Process, State as ProcessState},
    server::{Admission, Command, Event, Guest, Manager},
};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::unix::AsyncFd,
    sync::{mpsc, Mutex},
};

pub(super) fn spawn(runtime: String, sandbox: Arc<Mutex<SandBox>>, exit: Arc<ExitSignal>) {
    if !cube_terminal::valid_runtime(&runtime) {
        return;
    }
    tokio::spawn(async move {
        let guest = ShimGuest {
            runtime: runtime.clone(),
            sandbox,
        };
        let manager = match Manager::new(PathBuf::from(cube_terminal::ROOT), guest) {
            Ok(manager) => manager,
            Err(error) => {
                eprintln!("persistent terminal initialization failed: {error}");
                return;
            }
        };
        tokio::select! {
            result = cube_terminal::server::serve(manager, &runtime) => {
                if let Err(error) = result { eprintln!("persistent terminal service failed: {error}"); }
            },
            _ = exit.wait() => {},
        }
    });
}

struct ShimGuest {
    runtime: String,
    sandbox: Arc<Mutex<SandBox>>,
}
struct PendingIo(Option<PathBuf>);
impl Drop for PendingIo {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            if let Err(error) = std::fs::remove_dir_all(path) {
                eprintln!("terminal startup I/O cleanup failed: {error}");
            }
        }
    }
}
struct OwnedExec {
    sandbox: SandBox,
    runtime: String,
    id: String,
    directory: PathBuf,
    active: bool,
    state: ProcessState,
    admission: Arc<Admission>,
}
impl OwnedExec {
    async fn cleanup(&mut self) -> Result<(), String> {
        if let Some(code) = cube_terminal::process::cleanup(self, self.state).await? {
            self.state = ProcessState::Exited(code);
        }
        match std::fs::remove_dir_all(&self.directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        self.active = false;
        Ok(())
    }
}
#[async_trait]
impl Process for OwnedExec {
    async fn kill(&mut self) -> Result<(), String> {
        self.sandbox
            .kill_container(&self.runtime, &self.id, libc::SIGKILL as u32)
            .await
            .map_err(|e| e.to_string())
    }
    async fn wait(&mut self) -> Result<u32, String> {
        self.sandbox
            .wait_container(&self.runtime, &self.id)
            .await
            .map(|(code, _)| code)
            .map_err(|e| e.to_string())
    }
    async fn close_io(&mut self) -> Result<(), String> {
        self.sandbox
            .close_io(&self.runtime, &self.id)
            .await
            .map_err(|e| e.to_string())
    }
    async fn delete(&mut self) -> Result<(), String> {
        self.sandbox
            .delete_exec(&self.runtime, &self.id)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

impl Drop for OwnedExec {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut cleanup = Self {
            sandbox: self.sandbox.clone(),
            runtime: self.runtime.clone(),
            id: self.id.clone(),
            directory: self.directory.clone(),
            active: false,
            state: self.state,
            admission: self.admission.clone(),
        };
        tokio::spawn(async move {
            if let Err(error) = cleanup.cleanup().await {
                eprintln!("persistent terminal cleanup failed; admission retained until instance shutdown: {error}");
                // At most three such guards can exist on a node. Unproven cleanup must
                // not admit a fourth live process. Instance shutdown drops the OS lock.
                std::mem::forget(cleanup.admission.clone());
            }
        });
    }
}

#[async_trait]
impl Guest for ShimGuest {
    async fn terminate(&self, id: &str, _: &mpsc::Sender<Command>) -> Result<(), String> {
        let sandbox = self.sandbox.lock().await.clone();
        tokio::time::timeout(
            Duration::from_secs(5),
            sandbox.kill_container(
                &self.runtime,
                &format!("brocade-persistent-{id}"),
                libc::SIGKILL as u32,
            ),
        )
        .await
        .map_err(|_| "terminal termination timed out")?
        .map_err(|e| e.to_string())
    }

    async fn start(
        &self,
        id: &str,
        cols: u16,
        rows: u16,
        output: mpsc::Sender<Event>,
        admission: Arc<Admission>,
    ) -> Result<mpsc::Sender<Command>, String> {
        let directory = Path::new(cube_terminal::ROOT).join(format!(
            "io-{}-{id}",
            cube_terminal::socket_name(&self.runtime)
        ));
        // Refuse a pre-existing namespace: it may belong to an uncertain old exec.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|e| e.to_string())?;
        let mut pending = PendingIo(Some(directory.clone()));
        let input_path = directory.join("stdin");
        let output_path = directory.join("stdout");
        let input = fifo(&input_path, true)?;
        let reader = fifo(&output_path, false)?;
        let hold = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&output_path)
            .map_err(|e| e.to_string())?;
        let id = format!("brocade-persistent-{id}");
        let sandbox = self.sandbox.lock().await.clone();
        let mut owned = OwnedExec {
            sandbox,
            runtime: self.runtime.clone(),
            id: id.clone(),
            directory,
            active: true,
            state: ProcessState::Reserved,
            admission,
        };
        pending.0 = None;
        let process = Utils::get_oci_proc(&serde_json::to_vec(&serde_json::json!({
            "terminal":true,"cwd":"/root","user":{"uid":0,"gid":0},
            "env":["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin","HOME=/root","TERM=xterm-256color","LANG=C.UTF-8"],
            "args":["/bin/sh","-c","set -eu; stty -echo; printf 'BROCADE_PERSISTENT_READY\\n'; IFS= read -r gate; [ \"$gate\" = SIZE1 ]; printf 'SIZE:%s\\n' \"$(stty size)\"; IFS= read -r gate; [ \"$gate\" = SIZE2 ]; printf 'SIZE:%s\\n' \"$(stty size)\"; IFS= read -r gate; [ \"$gate\" = GO ]; stty echo; exec /bin/sh -i"]
        })).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let tty = Tty {
            terminal: true,
            stdin: input_path.to_string_lossy().into_owned(),
            stdout: output_path.to_string_lossy().into_owned(),
            ..Default::default()
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            owned
                .sandbox
                .exec_container(&owned.runtime, &id, tty, process)
                .await
                .map_err(|e| e.to_string())?;
            owned.state = ProcessState::Started;
            owned
                .sandbox
                .start_exec(&owned.runtime, &id)
                .await
                .map_err(|e| e.to_string())?;
            if line(&reader).await? != b"BROCADE_PERSISTENT_READY\r\n" {
                return Err("persistent terminal readiness failed".into());
            }
            owned
                .sandbox
                .resize_pty(&owned.runtime, &id, 83, 31)
                .await
                .map_err(|e| e.to_string())?;
            write(&input, b"SIZE1\n").await?;
            if line(&reader).await? != b"SIZE:31 83\r\n" {
                return Err("persistent terminal actual resize unavailable".into());
            }
            owned
                .sandbox
                .resize_pty(&owned.runtime, &id, u32::from(cols), u32::from(rows))
                .await
                .map_err(|e| e.to_string())?;
            write(&input, b"SIZE2\n").await?;
            if line(&reader).await? != format!("SIZE:{rows} {cols}\r\n").as_bytes() {
                return Err("persistent terminal initial size mismatch".into());
            }
            write(&input, b"GO\n").await?;
            Ok::<(), String>(())
        })
        .await
        .map_err(|_| "persistent terminal readiness timed out")??;
        let (tx, mut commands) = mpsc::channel(4);
        tokio::spawn(async move {
            let waiting = owned.sandbox.clone();
            let runtime = owned.runtime.clone();
            let exec = owned.id.clone();
            let wait = waiting.wait_container(&runtime, &exec);
            tokio::pin!(wait);
            let mut hold = Some(hold);
            let mut exit = None;
            let mut terminating = false;
            let mut pending_input: VecDeque<Vec<u8>> = VecDeque::new();
            let mut input_bytes = 0;
            let mut input_offset = 0;
            let drain = tokio::time::sleep(Duration::from_secs(5));
            tokio::pin!(drain);
            let result = async {
                loop {
                    tokio::select! {
                        data = read(&reader) => {
                            let data = data?;
                            if data.is_empty() && exit.is_some() { return Ok::<(), String>(()); }
                            if !data.is_empty() { output.send(Event::Data(data)).await.map_err(|_| "terminal consumer closed")?; }
                        }
                        written = write_some(&input, pending_input.front().map(|data| &data[input_offset..]).unwrap_or(&[])), if !pending_input.is_empty() => {
                            let written = written?;
                            input_bytes -= written;
                            input_offset += written;
                            if input_offset == pending_input.front().expect("input queue is nonempty in guarded write").len() {
                                pending_input.pop_front(); input_offset = 0;
                            }
                        },
                        command = commands.recv(), if !terminating && input_bytes <= 48 * 1024 => match command {
                            Some(Command::Input(data)) => { input_bytes += data.len(); pending_input.push_back(data); },
                            Some(Command::Resize(cols,rows)) => {
                                tokio::time::timeout(Duration::from_secs(5), owned.sandbox.resize_pty(&owned.runtime,&owned.id,u32::from(cols),u32::from(rows))).await.map_err(|_| "terminal resize timed out")?.map_err(|e| e.to_string())?;
                            }
                            Some(Command::Terminate) | None => {
                                owned.sandbox.kill_container(&owned.runtime,&owned.id,libc::SIGKILL as u32).await.map_err(|e| e.to_string())?;
                                terminating = true;
                            }
                        },
                        code = &mut wait, if exit.is_none() => {
                            exit = Some(code.map_err(|e| e.to_string())?.0); hold.take();
                            drain.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(5));
                        }
                        _ = &mut drain, if exit.is_some() => {
                            return Err("persistent terminal final output did not finish".into());
                        }
                    }
                }
            }.await;
            if let Err(error) = &result {
                eprintln!("persistent terminal I/O failed: {error}");
            }
            if let Some(code) = exit {
                owned.state = ProcessState::Exited(code);
            }
            let cleanup = owned.cleanup().await;
            let confirmed_exit = match owned.state {
                ProcessState::Exited(code) => Some(code),
                _ => None,
            };
            drop(owned);
            match cleanup {
                Ok(()) => {
                    if let Some(code) = confirmed_exit {
                        let _ = output.send(Event::Exit(code)).await;
                    }
                }
                Err(error) => eprintln!("persistent terminal cleanup not confirmed: {error}"),
            }
        });
        Ok(tx)
    }
}

fn fifo(path: &Path, input: bool) -> Result<AsyncFd<File>, String> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: name is a live, NUL-terminated path in this Shim's private directory.
    if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(input)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    AsyncFd::new(file).map_err(|e| e.to_string())
}
async fn line(fd: &AsyncFd<File>) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    loop {
        let data = read(fd).await?;
        if data.is_empty() {
            return Err("persistent terminal readiness closed".into());
        }
        line.extend(data);
        if line.len() > 128 {
            return Err("persistent terminal readiness exceeds limit".into());
        }
        if line.ends_with(b"\n") {
            return Ok(line);
        }
    }
}
async fn read(fd: &AsyncFd<File>) -> Result<Vec<u8>, String> {
    loop {
        let mut ready = fd.readable().await.map_err(|e| e.to_string())?;
        let mut data = vec![0; cube_terminal::MAX_DATA];
        match ready.try_io(|fd| (&*fd.get_ref()).read(&mut data)) {
            Ok(Ok(n)) => {
                data.truncate(n);
                return Ok(data);
            }
            Ok(Err(e)) => return Err(e.to_string()),
            Err(_) => {}
        }
    }
}
async fn write(fd: &AsyncFd<File>, mut data: &[u8]) -> Result<(), String> {
    while !data.is_empty() {
        let written = write_some(fd, data).await?;
        data = &data[written..];
    }
    Ok(())
}
async fn write_some(fd: &AsyncFd<File>, data: &[u8]) -> Result<usize, String> {
    loop {
        let mut ready = fd.writable().await.map_err(|e| e.to_string())?;
        match ready.try_io(|fd| (&*fd.get_ref()).write(data)) {
            Ok(Ok(0)) => return Err("persistent terminal input closed".into()),
            Ok(Ok(n)) => return Ok(n),
            Ok(Err(e)) => return Err(e.to_string()),
            Err(_) => {}
        }
    }
}
