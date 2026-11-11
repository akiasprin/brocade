use crate::{backend::Inner, files, Result};
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, OpenOptionsExt},
        },
    },
    path::PathBuf,
    time::{Duration, Instant},
};

const HANDSHAKE: &str = r#"set -eu; stty -echo; printf 'BROCADE_READY:%s\n' "$1"; IFS= read -r gate; [ "$gate" = "$1" ]; token=$1; shift; set +e; "$@"; code=$?; set -e; printf '\nBROCADE_DONE:%s:%s\n' "$token" "$code"; IFS= read -r ack; [ "$ack" = "ACK:$token" ]; exit "$code""#;

pub(crate) struct GuestIo<'a> {
    pub(crate) inner: &'a Inner,
    pub(crate) runtime: String,
    pub(crate) id: String,
    pub(crate) input: File,
    pub(crate) output: File,
    pub(crate) hold: Option<File>,
    directory: PathBuf,
    cleaned: bool,
}

impl Inner {
    pub(crate) fn guest_exec(
        &self,
        runtime: &str,
        read_json: bool,
        args: &[&str],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let nonce = files::nonce()?;
        let mut io = GuestIo::new(self, runtime, format!("brocade-{nonce}"))?;
        let mut wrapped = vec!["/bin/sh", "-c", HANDSHAKE, "brocade-io", &nonce];
        wrapped.extend_from_slice(args);
        let process = json!({"terminal":true,"user":{"uid":0,"gid":0},"cwd":"/","env":["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"],"args":wrapped});
        io.start(process, deadline)?;
        if io.line(deadline)? != format!("BROCADE_READY:{nonce}") {
            return Err("Guest output readiness invalid".into());
        }
        io.write(format!("{nonce}\n").as_bytes(), deadline)?;
        let marker = format!("\nBROCADE_DONE:{nonce}:");
        let mut bytes = Vec::new();
        let result = loop {
            let mut buffer = [0; 4096];
            let count = io.read(&mut buffer, deadline)?;
            if count == 0 {
                return Err("Guest output closed before completion evidence".into());
            }
            if bytes.len() + count > 128 * 1024 {
                return Err("Guest output exceeds limit".into());
            }
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(begin) = bytes
                .windows(marker.len())
                .position(|v| v == marker.as_bytes())
            {
                if let Some(end) = bytes[begin + marker.len()..]
                    .iter()
                    .position(|&b| b == b'\n')
                {
                    let code = &bytes[begin + marker.len()..begin + marker.len() + end];
                    let code = std::str::from_utf8(code)
                        .map_err(|_| "Guest exit marker invalid")?
                        .trim();
                    let code: u32 = code.parse().map_err(|_| "Guest exit marker invalid")?;
                    bytes.truncate(begin);
                    break (bytes, code);
                }
            }
        };
        io.write(format!("ACK:{nonce}\n").as_bytes(), deadline)?;
        let receipt = self.task(
            "Wait",
            json!({"container_id":runtime,"exec_id":io.id}),
            remaining(deadline)?,
        )?;
        if crate::number(&receipt, "exit_status") != 0 || result.1 != 0 {
            return Err(format!("Guest command exited with status {}", result.1));
        }
        if read_json {
            serde_json::from_slice::<Value>(&result.0)
                .map_err(|_| "Guest observation JSON incomplete")?;
        }
        io.cleanup()?;
        Ok(result.0)
    }
}

impl<'a> GuestIo<'a> {
    pub(crate) fn new(inner: &'a Inner, runtime: &str, id: String) -> Result<Self> {
        let mut active = inner
            .guest_io
            .lock()
            .map_err(|_| "active Guest execution state unavailable")?;
        let mut count = 0;
        for entry in
            std::fs::read_dir(&inner.root).map_err(|_| "Guest execution history unavailable")?
        {
            let entry = entry.map_err(|_| "Guest execution history entry unavailable")?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".guest-io-")
            {
                count += 1;
                if count >= crate::guest_cleanup::MAX_IO {
                    return Err("Guest execution history is full; awaiting owned cleanup".into());
                }
            }
        }
        let directory = inner.root.join(format!(".guest-io-{}", files::nonce()?));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| "Guest output directory unavailable")?;
        let result = (|| {
            files::atomic_json(
                &directory.join("exec.json"),
                &crate::guest_cleanup::Intent {
                    runtime: runtime.into(),
                    exec_id: id.clone(),
                },
            )?;
            let input = fifo(&directory.join("stdin"), true)?;
            let output = fifo(&directory.join("stdout"), false)?;
            let hold = OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(directory.join("stdout"))
                .map_err(|_| "Guest FIFO readiness hold unavailable")?;
            Ok(Self {
                inner,
                runtime: runtime.into(),
                id,
                input,
                output,
                hold: Some(hold),
                directory: directory.clone(),
                cleaned: false,
            })
        })();
        if result.is_err() {
            std::fs::remove_dir_all(&directory)
                .map_err(|_| "Guest setup directory cleanup failed")?;
        }
        if result.is_ok() {
            active.insert(directory);
        }
        result
    }

    pub(crate) fn start(&self, process: Value, deadline: Instant) -> Result<()> {
        self.inner.runtime.block_on(async {
            tokio::time::timeout(
                remaining(deadline)?,
                self.inner.rpc.exec(
                    &self.runtime,
                    &self.id,
                    &self.directory.join("stdin"),
                    &self.directory.join("stdout"),
                    process,
                ),
            )
            .await
            .map_err(|_| "Guest execution setup deadline exceeded")?
        })?;
        self.inner.task(
            "Start",
            json!({"container_id":self.runtime,"exec_id":self.id}),
            remaining(deadline)?,
        )?;
        Ok(())
    }

    pub(crate) fn line(&mut self, deadline: Instant) -> Result<String> {
        let mut bytes = vec![];
        while bytes.len() < 1024 {
            let mut byte = [0];
            if self.read(&mut byte, deadline)? == 0 {
                return Err("Guest handshake output ended early".into());
            }
            if byte[0] == b'\n' {
                return String::from_utf8(bytes)
                    .map(|s| s.trim().to_owned())
                    .map_err(|_| "Guest handshake encoding invalid".into());
            }
            bytes.push(byte[0]);
        }
        Err("Guest handshake exceeds limit".into())
    }

    pub(crate) fn read(&mut self, bytes: &mut [u8], deadline: Instant) -> Result<usize> {
        loop {
            match self.output.read(bytes) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    poll(&self.output, libc::POLLIN, deadline)?
                }
                Err(_) => return Err("Guest output read failed".into()),
            }
        }
    }

    pub(crate) fn write(&mut self, mut bytes: &[u8], deadline: Instant) -> Result<()> {
        while !bytes.is_empty() {
            match self.input.write(bytes) {
                Ok(0) => return Err("Guest input closed".into()),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    poll(&self.input, libc::POLLOUT, deadline)?
                }
                Err(_) => return Err("Guest input write failed".into()),
            }
        }
        Ok(())
    }

    pub(crate) fn cleanup(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        crate::guest_cleanup::cleanup_exec(
            self.inner,
            &self.runtime,
            &self.id,
            Instant::now() + Duration::from_secs(5),
        )?;
        self.hold.take();
        std::fs::remove_dir_all(&self.directory)
            .map_err(|_| "Guest FIFO directory cleanup failed")?;
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for GuestIo<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("CVM Guest cleanup failed: {error}");
        }
        match self.inner.guest_io.lock() {
            Ok(mut active) => {
                active.remove(&self.directory);
            }
            Err(_) => eprintln!("CVM active Guest cleanup state unavailable"),
        }
    }
}

fn fifo(path: &std::path::Path, input: bool) -> Result<File> {
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "Guest FIFO path invalid")?;
    // SAFETY: path_c is a live NUL-terminated pathname; mkfifo does not retain it.
    if unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) } != 0 {
        return Err("Guest FIFO creation failed".into());
    }
    OpenOptions::new()
        .read(true)
        .write(input)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "Guest FIFO open failed".into())
}

pub(crate) fn remaining(deadline: Instant) -> Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("Guest I/O deadline exceeded".into());
    }
    Ok(remaining)
}

fn poll(file: &File, events: i16, deadline: Instant) -> Result<()> {
    let timeout = remaining(deadline)?.as_millis().min(100) as i32;
    let mut fd = libc::pollfd {
        fd: file.as_raw_fd(),
        events,
        revents: 0,
    };
    // SAFETY: fd points to one live pollfd, and the File remains open for the call.
    if unsafe { libc::poll(&mut fd, 1, timeout) } < 0
        && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
    {
        return Err("Guest FIFO readiness failed".into());
    }
    Ok(())
}
