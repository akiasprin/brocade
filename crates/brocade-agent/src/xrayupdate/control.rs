//! Bounded local listener-FD export, including peers using the legacy single packet.
use super::*;

const FD_BATCH_SIZE: usize = 253; // Linux SCM_MAX_FD, not the process descriptor limit.

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    error: String,
    status: Option<HandoffRuntimeStatus>,
    fd_export: Option<FDExport>,
}

#[derive(Deserialize)]
struct FDExport {
    offset: usize,
    total: usize,
}

pub(super) fn preflight(status: &HandoffRuntimeStatus) -> Result<(), String> {
    let count = status
        .listeners
        .iter()
        .filter(|l| l.network == "tcp")
        .count();
    if count > MAX_HANDOFF_LISTENERS {
        return Err("运行代的 TCP 监听数量超过交接上限".into());
    }
    if count > FD_BATCH_SIZE && !status.fd_export_batches {
        return Err(format!(
            "运行代有 {count} 个 TCP 监听，超过单条消息的 {FD_BATCH_SIZE} 个 FD 上限且不支持分批交接；未暂停旧代"
        ));
    }
    Ok(())
}

pub(super) fn receive(
    socket: &OwnedFd,
    peer: i32,
    operation: &str,
    receive_fds: bool,
) -> Result<(HandoffRuntimeStatus, Vec<OwnedFd>), String> {
    let batched = operation == "export_batched";
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut descriptors = Vec::new();
    let mut status = None;
    let mut total = None;
    let mut bytes = 0;
    loop {
        let (response, mut fds, length) = packet(socket, deadline, receive_fds)
            .map_err(|error| {
                format!("代际控制 {operation} 等待响应失败（PID {peer}，超时上限 2 秒）：{error}")
            })?
            .ok_or_else(|| format!("代际控制 {operation} 响应提前关闭（PID {peer}）"))?;
        bytes += length;
        if bytes > MAX_HANDOFF_MESSAGE_BYTES {
            return Err("代际控制响应超出大小上限".into());
        }
        if !response.error.is_empty() {
            return Err(format!("代际控制端拒绝状态请求：{}", response.error));
        }
        if !batched {
            let status = response.status.ok_or("代际状态响应为空")?;
            if status.pid != peer {
                return Err("代际状态响应 PID 与控制端 peer 不一致".into());
            }
            if response.fd_export.is_some() {
                return Err("未协商分批的代际响应携带了 FD 批次".into());
            }
            if receive_fds && fds.is_empty() {
                return Err("代际 export 没有返回监听 FD".into());
            }
            return Ok((status, fds));
        }
        let batch = response.fd_export.ok_or("分批 export 缺少批次信息")?;
        if batch.total > MAX_HANDOFF_LISTENERS
            || batch.offset != descriptors.len()
            || batch.offset > batch.total
            || total.is_some_and(|total| total != batch.total)
            || fds.len() != FD_BATCH_SIZE.min(batch.total - batch.offset)
        {
            return Err("分批 export 的偏移、总数或 FD 数量无效".into());
        }
        if status.is_none() {
            let first = response.status.ok_or("分批 export 首包缺少状态")?;
            if first.pid != peer || !first.fd_export_batches {
                return Err("分批 export 的 PID 或能力不一致".into());
            }
            if first
                .listeners
                .iter()
                .filter(|l| l.network == "tcp")
                .count()
                != batch.total
            {
                return Err("分批 export 的总数与 TCP 监听清单不一致".into());
            }
            status = Some(first);
            total = Some(batch.total);
        } else if response.status.is_some() {
            return Err("分批 export 重复返回状态".into());
        }
        descriptors.append(&mut fds);
        if descriptors.len() == batch.total {
            return Ok((
                status.expect("first batch supplied the status"),
                descriptors,
            ));
        }
    }
}

fn packet(
    socket: &OwnedFd,
    deadline: Instant,
    receive_fds: bool,
) -> Result<Option<(Response, Vec<OwnedFd>, usize)>, String> {
    let mut response = vec![0_u8; MAX_HANDOFF_MESSAGE_BYTES + 1];
    let rights_bytes = std::mem::size_of::<i32>() * MAX_HANDOFF_LISTENERS;
    // SAFETY: CMSG_SPACE only calculates the space for the bounded ancillary payload.
    let mut control = vec![0_u8; unsafe { libc::CMSG_SPACE(rights_bytes as u32) } as usize];
    let mut iovec = libc::iovec {
        iov_base: response.as_mut_ptr().cast(),
        iov_len: response.len(),
    };
    // SAFETY: zeroed msghdr has no pointers until its live buffers are assigned below.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    // The field is usize on glibc and u32 on musl.
    #[allow(clippy::useless_conversion)]
    let capacity = control
        .len()
        .try_into()
        .map_err(|_| "辅助缓冲区长度超出平台上限")?;
    let received = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("control response deadline elapsed".into());
        }
        let timeout = libc::timeval {
            tv_sec: remaining.as_secs() as _,
            tv_usec: remaining.subsec_micros().max(1) as _,
        };
        // SAFETY: the owned socket and timeval remain live for this synchronous call.
        if unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                std::ptr::addr_of!(timeout).cast(),
                std::mem::size_of_val(&timeout) as libc::socklen_t,
            )
        } != 0
        {
            return Err(io::Error::last_os_error().to_string());
        }
        message.msg_controllen = capacity;
        message.msg_flags = 0;
        // SAFETY: all msghdr buffers remain live and writable for recvmsg.
        let received =
            unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
        if received >= 0 {
            break received as usize;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error.to_string());
        }
        // Resume this receive under the original deadline; never replay the request.
    };
    // Take ownership before checking truncation/JSON: recvmsg can already have
    // installed descriptors even when the packet is invalid. Every error drops them.
    let mut fds = Vec::new();
    let mut invalid = false;
    // SAFETY: recvmsg supplied a kernel-generated ancillary buffer of this capacity.
    let mut header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    while !header.is_null() {
        // SAFETY: CMSG_FIRSTHDR/NXTHDR checked the header lies inside control.
        let entry = unsafe { &*header };
        if entry.cmsg_level == libc::SOL_SOCKET && entry.cmsg_type == libc::SCM_RIGHTS {
            #[allow(clippy::useless_conversion)]
            let length = usize::try_from(entry.cmsg_len).map_err(|_| "SCM_RIGHTS 长度超限")?;
            // SAFETY: CMSG_LEN calculates the fixed ancillary header size.
            let header_bytes = unsafe { libc::CMSG_LEN(0) } as usize;
            if length < header_bytes
                || !(length - header_bytes).is_multiple_of(std::mem::size_of::<i32>())
            {
                return Err("SCM_RIGHTS 长度无效".into());
            }
            let count = (length - header_bytes) / std::mem::size_of::<i32>();
            // SAFETY: the kernel returned this SCM_RIGHTS payload and count.
            let values =
                unsafe { std::slice::from_raw_parts(libc::CMSG_DATA(header).cast::<i32>(), count) };
            for raw in values {
                if *raw >= 0 {
                    // SAFETY: each received SCM_RIGHTS descriptor is newly owned.
                    fds.push(unsafe { OwnedFd::from_raw_fd(*raw) });
                }
                invalid |= *raw < 3;
            }
        } else {
            invalid = true;
        }
        // SAFETY: advance within the same kernel-generated ancillary buffer.
        header = unsafe { libc::CMSG_NXTHDR(&message, header) };
    }
    if received == 0 {
        return Ok(None);
    }
    if received > MAX_HANDOFF_MESSAGE_BYTES {
        return Err("响应超出大小上限".into());
    }
    if message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Err("代际控制响应或监听 FD 被截断".into());
    }
    if invalid || fds.len() > FD_BATCH_SIZE || (!receive_fds && !fds.is_empty()) {
        return Err("代际控制端返回的监听 FD 或辅助数据无效".into());
    }
    let response = serde_json::from_slice(&response[..received])
        .map_err(|error| format!("解析代际状态失败：{error}"))?;
    Ok(Some((response, fds, received)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (OwnedFd, OwnedFd) {
        let mut fds = [-1; 2];
        // SAFETY: fds has space for the two newly owned socket descriptors.
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                    0,
                    fds.as_mut_ptr(),
                )
            },
            0
        );
        // SAFETY: successful socketpair returned two distinct owned descriptors.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [-1; 2];
        // SAFETY: fds has space for the newly owned read/write pipe descriptors.
        assert_eq!(
            unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
            0
        );
        // SAFETY: successful pipe2 returned two distinct owned descriptors.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    fn status(count: usize, batched: bool) -> serde_json::Value {
        serde_json::json!({
            "scope":"whole-generation", "retirement_supported":true,
            "fd_export_batches":batched, "generation_id":"test-generation-0001",
            "pid":std::process::id(), "boot_id":"test", "pid_start_ticks":"1",
            "ready":true, "accepting":false,
            "listeners":(0..count).map(|_| serde_json::json!({
                "address":"127.0.0.1:1234", "bind_address":"127.0.0.1:1234",
                "network":"tcp", "role":"public", "socket_inode":1,
                "socket_cookie":2, "protocol":6
            })).collect::<Vec<_>>()
        })
    }

    fn send(socket: &OwnedFd, body: &[u8], raw: i32, count: usize) {
        // SAFETY: CMSG_SPACE calculates space for this bounded SCM_RIGHTS payload.
        let mut control = vec![0_u8; unsafe { libc::CMSG_SPACE((count * 4) as u32) } as usize];
        let mut iovec = libc::iovec {
            iov_base: body.as_ptr().cast_mut().cast(),
            iov_len: body.len(),
        };
        // SAFETY: zeroed msghdr is populated with live buffers below.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iovec;
        message.msg_iovlen = 1;
        if count != 0 {
            message.msg_control = control.as_mut_ptr().cast();
            // The field is usize on glibc and u32 on musl.
            #[allow(clippy::useless_conversion)]
            {
                message.msg_controllen = control.len().try_into().unwrap();
            }
            // SAFETY: control has CMSG_SPACE bytes for this header and all descriptors.
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&message);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN((count * 4) as u32) as _;
                for i in 0..count {
                    libc::CMSG_DATA(header).cast::<i32>().add(i).write(raw);
                }
            }
        }
        // SAFETY: message references live body/control buffers for this synchronous sendmsg.
        assert_eq!(
            unsafe { libc::sendmsg(socket.as_raw_fd(), &message, libc::MSG_NOSIGNAL) },
            body.len() as isize,
            "{}",
            io::Error::last_os_error()
        );
    }

    fn assert_pipe_closed(reader: &OwnedFd) {
        let mut byte = 0_u8;
        // SAFETY: reader is an owned nonblocking pipe and byte is writable.
        assert_eq!(
            unsafe { libc::read(reader.as_raw_fd(), std::ptr::addr_of_mut!(byte).cast(), 1) },
            0,
            "a received write descriptor leaked"
        );
    }

    #[test]
    fn handoff_fd_export_checks_legacy_capacity_before_pause() {
        for (count, batched, allowed) in [
            (253, false, true),
            (254, false, false),
            (512, true, true),
            (513, true, false),
        ] {
            let status: HandoffRuntimeStatus =
                serde_json::from_value(status(count, batched)).unwrap();
            assert_eq!(preflight(&status).is_ok(), allowed);
        }
        let mut legacy = status(1, false);
        legacy.as_object_mut().unwrap().remove("fd_export_batches");
        assert!(
            !serde_json::from_value::<HandoffRuntimeStatus>(legacy)
                .unwrap()
                .fd_export_batches
        );
    }

    #[test]
    fn handoff_fd_export_receives_legacy_and_complete_batched_sets() {
        for (count, batched) in [
            (253, false),
            (0, true),
            (253, true),
            (254, true),
            (512, true),
        ] {
            let (client, server) = pair();
            let (reader, writer) = pipe();
            let worker = thread::spawn(move || {
                let mut offset = 0;
                loop {
                    let length = FD_BATCH_SIZE.min(count - offset);
                    let mut frame = serde_json::json!({});
                    if offset == 0 {
                        frame["status"] = status(count, batched);
                    }
                    if batched {
                        frame["fd_export"] = serde_json::json!({"offset":offset,"total":count});
                    }
                    send(
                        &server,
                        &serde_json::to_vec(&frame).unwrap(),
                        writer.as_raw_fd(),
                        length,
                    );
                    offset += length;
                    if offset == count {
                        break;
                    }
                }
            });
            let (_, descriptors) = receive(
                &client,
                std::process::id() as i32,
                if batched { "export_batched" } else { "export" },
                true,
            )
            .unwrap();
            assert_eq!(descriptors.len(), count);
            for fd in &descriptors {
                // SAFETY: F_GETFD reads flags on the live received descriptor.
                assert_ne!(
                    unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                    0
                );
            }
            worker.join().unwrap();
            drop(descriptors);
            assert_pipe_closed(&reader);
        }
    }

    #[test]
    fn handoff_fd_export_rejects_partial_or_invalid_batches_without_leaks() {
        for failure in [
            "offset",
            "total",
            "short",
            "status",
            "metadata",
            "json",
            "eof",
            "truncated",
            "too_many",
            "missing_first_status",
            "wrong_peer",
        ] {
            let (client, server) = pair();
            let (reader, writer) = pipe();
            let worker = thread::spawn(move || {
                let mut first = serde_json::json!({"status":status(254, true), "fd_export":{"offset":0,"total":254}});
                match failure {
                    "too_many" => first["fd_export"]["total"] = 513.into(),
                    "missing_first_status" => {
                        first.as_object_mut().unwrap().remove("status");
                    }
                    "wrong_peer" => first["status"]["pid"] = (std::process::id() + 1).into(),
                    _ => (),
                }
                send(
                    &server,
                    &serde_json::to_vec(&first).unwrap(),
                    writer.as_raw_fd(),
                    253,
                );
                if matches!(
                    failure,
                    "eof" | "too_many" | "missing_first_status" | "wrong_peer"
                ) {
                    return;
                }
                let mut next = serde_json::json!({"fd_export":{"offset":253,"total":254}});
                match failure {
                    "offset" => next["fd_export"]["offset"] = 0.into(),
                    "total" => next["fd_export"]["total"] = 255.into(),
                    "status" => next["status"] = status(254, true),
                    "metadata" => {
                        next.as_object_mut().unwrap().remove("fd_export");
                    }
                    _ => (),
                }
                let body = match failure {
                    "json" => b"{".to_vec(),
                    "truncated" => {
                        let size = (MAX_HANDOFF_MESSAGE_BYTES * 2) as i32;
                        // SAFETY: server and size are live for this bounded socket option.
                        assert_eq!(
                            unsafe {
                                libc::setsockopt(
                                    server.as_raw_fd(),
                                    libc::SOL_SOCKET,
                                    libc::SO_SNDBUF,
                                    std::ptr::addr_of!(size).cast(),
                                    std::mem::size_of_val(&size) as libc::socklen_t,
                                )
                            },
                            0
                        );
                        vec![b' '; MAX_HANDOFF_MESSAGE_BYTES + 2]
                    }
                    _ => serde_json::to_vec(&next).unwrap(),
                };
                send(
                    &server,
                    &body,
                    writer.as_raw_fd(),
                    usize::from(failure != "short"),
                );
            });
            let error =
                receive(&client, std::process::id() as i32, "export_batched", true).unwrap_err();
            assert!(!error.is_empty(), "{failure}");
            worker.join().unwrap();
            assert_pipe_closed(&reader);
        }
    }

    #[test]
    fn handoff_fd_export_closes_unexpected_rights_in_status_replies() {
        let (client, server) = pair();
        let (reader, writer) = pipe();
        send(
            &server,
            &serde_json::to_vec(&serde_json::json!({"status":status(0, false)})).unwrap(),
            writer.as_raw_fd(),
            1,
        );
        drop(writer);
        assert!(receive(&client, std::process::id() as i32, "status", false).is_err());
        assert_pipe_closed(&reader);
    }

    #[test]
    fn handoff_fd_batches_share_one_receive_deadline() {
        let (client, server) = pair();
        let (reader, writer) = pipe();
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_secs(1));
            let first = serde_json::json!({"status":status(254, true), "fd_export":{"offset":0,"total":254}});
            send(
                &server,
                &serde_json::to_vec(&first).unwrap(),
                writer.as_raw_fd(),
                253,
            );
            // Closing after the overall deadline distinguishes a shared timeout
            // from a fresh two-second timeout that would see EOF instead.
            thread::sleep(Duration::from_millis(1500));
        });
        let error =
            receive(&client, std::process::id() as i32, "export_batched", true).unwrap_err();
        assert!(error.contains("等待响应失败"), "{error}");
        worker.join().unwrap();
        drop(client);
        assert_pipe_closed(&reader);
    }
}
