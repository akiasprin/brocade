//! Delegate systemd listeners only to Console, without leaking them to PostgreSQL or tools.

use anyhow::{bail, Context, Result};
use std::{
    env,
    ffi::OsString,
    io,
    net::TcpListener,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::process::CommandExt,
    },
    path::Path,
};
use tokio::process::Command;

pub(crate) struct SocketActivation {
    _listeners: Vec<TcpListener>,
    child_sources: Vec<OwnedFd>,
}

impl SocketActivation {
    pub(crate) fn receive() -> Result<Option<Self>> {
        let Some(count) = activation_count(
            env::var_os("LISTEN_PID"),
            env::var_os("LISTEN_FDS"),
            std::process::id(),
        )?
        else {
            return Ok(None);
        };
        let mut listeners = Vec::with_capacity(count);
        for offset in 0..count {
            let raw = 3 + offset as RawFd;
            validate_listener(raw)?;
            // SAFETY: the matching LISTEN_PID transfers these descriptors to this process.
            // Each is owned once, and no child has been started by the launcher yet.
            let owned = unsafe { OwnedFd::from_raw_fd(raw) };
            listeners.push(TcpListener::from(owned));
        }
        Self::from_listeners(listeners).map(Some)
    }

    fn from_listeners(listeners: Vec<TcpListener>) -> Result<Self> {
        let mut child_sources = Vec::with_capacity(listeners.len());
        for listener in &listeners {
            let raw = listener.as_raw_fd();
            listener
                .local_addr()
                .context("systemd descriptor is not an IP socket")?;
            validate_listener(raw)?;
            // Keep every launcher child isolated. Only the Console child's pre_exec below
            // clears CLOEXEC on its dedicated copies, never on the parent's descriptors.
            // SAFETY: the listener owns this live descriptor; fcntl retains no pointer.
            let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
            if flags == -1
                || unsafe { libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1
            {
                return Err(io::Error::last_os_error().into());
            }
            // Sources cannot overlap destination descriptors 3/4 in the forked child.
            // SAFETY: F_DUPFD_CLOEXEC returns a distinct owned descriptor or -1.
            let duplicate = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 64) };
            if duplicate == -1 {
                return Err(io::Error::last_os_error().into());
            }
            // SAFETY: fcntl returned a new descriptor with exclusive ownership.
            child_sources.push(unsafe { OwnedFd::from_raw_fd(duplicate) });
        }
        Ok(Self {
            _listeners: listeners,
            child_sources,
        })
    }

    pub(crate) fn console_command(&self, program: &Path) -> Command {
        // Setting environment variables in pre_exec can allocate after fork. A fixed POSIX
        // exec shim sets the child's actual PID safely and preserves that PID across exec.
        // The executable is a separate argument, never interpolated into shell source.
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "LISTEN_PID=$$; export LISTEN_PID; exec \"$@\"",
            "brocade-console",
        ]);
        command
            .arg(program)
            .env("LISTEN_FDS", self.child_sources.len().to_string());
        let sources: Vec<_> = self.child_sources.iter().map(AsRawFd::as_raw_fd).collect();
        // SAFETY: the closure performs only async-signal-safe dup2 calls on preallocated
        // descriptor numbers. Parent-owned sources remain alive throughout spawn.
        unsafe {
            command.as_std_mut().pre_exec(move || {
                for (offset, source) in sources.iter().enumerate() {
                    if libc::dup2(*source, 3 + offset as RawFd) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        command
    }
}

fn validate_listener(fd: RawFd) -> Result<()> {
    if socket_option(fd, libc::SO_TYPE)? != libc::SOCK_STREAM
        || socket_option(fd, libc::SO_ACCEPTCONN)? != 1
    {
        bail!("systemd descriptor is not a listening TCP socket");
    }
    Ok(())
}

fn activation_count(
    pid: Option<OsString>,
    count: Option<OsString>,
    current_pid: u32,
) -> Result<Option<usize>> {
    if pid.is_none() && count.is_none() {
        return Ok(None);
    }
    let parse = |name: &str, value: Option<OsString>| -> Result<u32> {
        value
            .context(format!("{name} is missing"))?
            .to_str()
            .context(format!("{name} is not UTF-8"))?
            .parse()
            .with_context(|| format!("{name} is not an unsigned integer"))
    };
    let pid = parse("LISTEN_PID", pid)?;
    let count = parse("LISTEN_FDS", count)?;
    if pid != current_pid || count == 0 {
        return Ok(None);
    }
    if count > 2 {
        bail!("systemd passed more than the admin and agent listeners");
    }
    Ok(Some(count as usize))
}

fn socket_option(fd: RawFd, option: libc::c_int) -> io::Result<libc::c_int> {
    let mut value = 0;
    let mut length = std::mem::size_of_val(&value) as libc::socklen_t;
    // SAFETY: writable storage is sized for a SOL_SOCKET integer option.
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            (&mut value as *mut libc::c_int).cast(),
            &mut length,
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn pid_count_and_partial_activation_are_checked() {
        assert_eq!(activation_count(None, None, 5).unwrap(), None);
        assert_eq!(
            activation_count(Some("4".into()), Some("1".into()), 5).unwrap(),
            None
        );
        assert_eq!(
            activation_count(Some("5".into()), Some("2".into()), 5).unwrap(),
            Some(2)
        );
        assert!(activation_count(Some("5".into()), None, 5).is_err());
        assert!(activation_count(Some("5".into()), Some("3".into()), 5).is_err());
        assert!(activation_count(Some("bad".into()), Some("1".into()), 5).is_err());
    }

    #[test]
    fn non_listener_descriptors_are_rejected_before_taking_ownership() {
        let datagram = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        assert!(validate_listener(datagram.as_raw_fd()).is_err());
        assert!(validate_listener(-1).is_err());
    }

    #[test]
    #[ignore = "only executed as the Console subprocess by the delegation test"]
    fn socket_child() {
        assert_eq!(
            env::var("LISTEN_PID").unwrap(),
            std::process::id().to_string()
        );
        assert_eq!(env::var("LISTEN_FDS").unwrap(), "1");
        let activation = SocketActivation::receive().unwrap().unwrap();
        let listener = &activation._listeners[0];
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut bytes = [0; 512];
        assert!(stream.read(&mut bytes).unwrap() > 0);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .unwrap();
    }

    #[tokio::test]
    async fn listener_survives_console_replacement_and_is_not_leaked_to_other_children() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let activation = SocketActivation::from_listeners(vec![listener]).unwrap();
        for source in &activation.child_sources {
            let status = std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    "test ! -e /proc/self/fd/$1",
                    "check-fd",
                    &source.as_raw_fd().to_string(),
                ])
                .status()
                .unwrap();
            assert!(status.success());
        }
        // A connection queued before either child starts must be accepted after exec, on
        // the same listener. Repeat to cover a replacement without a port rebind.
        for _ in 0..2 {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: local\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut child = activation
                .console_command(&env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "socket_activation::tests::socket_child",
                    "--nocapture",
                ])
                .stdout(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let mut response = Vec::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                stream.read_to_end(&mut response),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(response.starts_with(b"HTTP/1.1 200 OK"));
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
                    .await
                    .unwrap()
                    .unwrap()
                    .success()
            );
        }
    }
}
