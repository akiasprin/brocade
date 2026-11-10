#![cfg(feature = "server")]
//! Kernel PTY regression. Every child and directory belongs to this test.
use async_trait::async_trait;
use cube_terminal::{
    server::{Admission, Command, Event, Guest, Manager},
    *,
};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{io::unix::AsyncFd, sync::mpsc};

struct PtyGuest;
#[async_trait]
impl Guest for PtyGuest {
    async fn start(
        &self,
        _: &str,
        cols: u16,
        rows: u16,
        output: mpsc::Sender<Event>,
        admission: Arc<Admission>,
    ) -> Result<mpsc::Sender<Command>, String> {
        let mut master = -1;
        let mut slave = -1;
        let size = libc::winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: both out-pointers and winsize are valid; null name/termios are supported.
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: openpty returned two fresh descriptors, each transferred exactly once.
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        // SAFETY: the descriptor remains owned by master; F_SETFL does not transfer ownership.
        if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-i")
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(slave.try_clone().map_err(|e| e.to_string())?))
            .stdout(Stdio::from(slave.try_clone().map_err(|e| e.to_string())?))
            .stderr(Stdio::from(slave));
        // SAFETY: the child hook only invokes async-signal-safe libc session/TTY operations.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let fd = AsyncFd::new(master).map_err(|e| e.to_string())?;
        let (tx, mut rx) = mpsc::channel(4);
        tokio::spawn(async move {
            let mut admission = Some(admission);
            let mut code = None;
            let mut eof = false;
            loop {
                tokio::select! {
                    status=child.wait(),if code.is_none()=>{code=Some(status.unwrap().code().unwrap_or(137) as u32);},
                    result=async {
                        loop {
                            let mut ready=fd.readable().await.unwrap();let mut data=vec![0;MAX_DATA];
                            match ready.try_io(|fd|fd.get_ref().read(&mut data)) {
                                Ok(Ok(n))=>{data.truncate(n);break Some(data);},
                                Ok(Err(error)) if error.raw_os_error()==Some(libc::EIO)=>break None,
                                Ok(Err(error))=>panic!("{error}"),Err(_)=>{},
                            }
                        }
                    },if !eof=>match result {
                        Some(data) if !data.is_empty()=>{output.send(Event::Data(data)).await.unwrap();},
                        _=>eof=true,
                    },
                    command=rx.recv()=>match command {
                        Some(Command::Input(data))=>{
                            let mut bytes=data.as_slice();while !bytes.is_empty(){
                                let mut ready=fd.writable().await.unwrap();
                                match ready.try_io(|fd|fd.get_ref().write(bytes)) {
                                    Ok(Ok(n))=>bytes=&bytes[n..],Ok(Err(error))=>panic!("{error}"),Err(_)=>{},
                                }
                            }
                        },
                        Some(Command::Resize(cols,rows))=>{
                            let size=libc::winsize{ws_col:cols,ws_row:rows,ws_xpixel:0,ws_ypixel:0};
                            // SAFETY: fd and winsize remain live for this ioctl.
                            assert_eq!(unsafe{libc::ioctl(fd.get_ref().as_raw_fd(),libc::TIOCSWINSZ,&size)},0);
                        },
                        Some(Command::Terminate)|None=>{child.start_kill().unwrap();},
                    }
                }
                if eof && code.is_some() {
                    break;
                }
            }
            drop(admission.take());
            output.send(Event::Exit(code.unwrap())).await.unwrap();
        });
        Ok(tx)
    }
}
fn id(n: usize) -> String {
    format!("{n:064x}")
}
fn req(operation: Operation) -> Request {
    Request {
        version: VERSION,
        owner: id(1),
        incarnation: id(99),
        policy_revision: 0,
        operation,
    }
}
async fn text_until(manager: &Arc<Manager<PtyGuest>>, attachment: usize, marker: &str) -> String {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut data = Vec::new();
        loop {
            if let Response::Data { data: part, .. } = manager
                .handle(req(Operation::Read {
                    id: id(10),
                    attachment: id(attachment),
                }))
                .await
            {
                data.extend(part);
                if String::from_utf8_lossy(&data).contains(marker) {
                    return String::from_utf8(data).unwrap();
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for PTY marker {marker}"))
}
#[tokio::test]
async fn same_shell_pid_variables_directory_resize_and_final_output_survive_detach() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let manager = Manager::new(root.path().into(), PtyGuest).unwrap();
    assert!(matches!(
        manager
            .handle(req(Operation::Open {
                id: id(10),
                fresh: true,
                exact: false,
                cols: 80,
                rows: 24
            }))
            .await,
        Response::Opened { .. }
    ));
    manager
        .handle(req(Operation::Attach {
            id: id(10),
            attachment: id(100),
        }))
        .await;
    let command=format!("stty -echo; PS1='BROCADE_PROMPT> '; KEEP_VALUE=persistent; cd '{}'; printf '\\137\\137BEFORE_%s_%s__\\n' \"$$\" \"$KEEP_VALUE\"\n",root.path().display());
    manager
        .handle(req(Operation::Input {
            id: id(10),
            attachment: id(100),
            data: command.into_bytes(),
        }))
        .await;
    let before = text_until(&manager, 100, "_persistent__").await;
    let pid = before
        .split("__BEFORE_")
        .last()
        .unwrap()
        .split('_')
        .next()
        .unwrap()
        .to_owned();
    assert!(pid.bytes().all(|c| c.is_ascii_digit()), "{before}");
    manager
        .handle(req(Operation::Detach {
            id: id(10),
            attachment: id(100),
        }))
        .await;
    assert!(matches!(
        manager
            .handle(req(Operation::Open {
                id: id(10),
                fresh: false,
                exact: true,
                cols: 80,
                rows: 24
            }))
            .await,
        Response::Opened { .. }
    ));
    manager
        .handle(req(Operation::Attach {
            id: id(10),
            attachment: id(101),
        }))
        .await;
    manager
        .handle(req(Operation::Resize {
            id: id(10),
            attachment: id(101),
            cols: 100,
            rows: 35,
        }))
        .await;
    let command=b"printf '__AFTER_%s_%s_%s__\n' \"$$\" \"$KEEP_VALUE\" \"$PWD\"; stty size; printf '__AFTER_DONE__\n'\n".to_vec();
    manager
        .handle(req(Operation::Input {
            id: id(10),
            attachment: id(101),
            data: command,
        }))
        .await;
    let after = text_until(&manager, 101, "__AFTER_DONE__").await;
    assert!(
        after.contains(&format!(
            "__AFTER_{pid}_persistent_{}__",
            root.path().display()
        )),
        "{after}"
    );
    assert!(after.contains("35 100"), "{after}");
    manager
        .handle(req(Operation::Input {
            id: id(10),
            attachment: id(101),
            data: b"/bin/sh -c 'printf \"__WAITING__\\n\"; exec sleep 30'\n".to_vec(),
        }))
        .await;
    text_until(&manager, 101, "__WAITING__").await;
    manager
        .handle(req(Operation::Input {
            id: id(10),
            attachment: id(101),
            data: vec![3],
        }))
        .await;
    text_until(&manager, 101, "BROCADE_PROMPT> ").await;
    manager
        .handle(req(Operation::Input {
            id: id(10),
            attachment: id(101),
            data: b"printf '__INTERRUPTED__\\n'\n".to_vec(),
        }))
        .await;
    text_until(&manager, 101, "__INTERRUPTED__").await;
    manager
        .handle(req(Operation::Input {
            id: id(10),
            attachment: id(101),
            data: b"printf '__FINAL_TAIL__\\n'; exit 7\n".to_vec(),
        }))
        .await;
    let (tail, code) = tokio::time::timeout(Duration::from_secs(3), async {
        let mut data = Vec::new();
        loop {
            if let Response::Data {
                data: part, exit, ..
            } = manager
                .handle(req(Operation::Read {
                    id: id(10),
                    attachment: id(101),
                }))
                .await
            {
                data.extend(part);
                if let Some(code) = exit {
                    break (data, code);
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(String::from_utf8(tail).unwrap().contains("__FINAL_TAIL__"));
    assert_eq!(code, 7);
}
