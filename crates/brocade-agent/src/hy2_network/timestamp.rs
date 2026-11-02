//! RX timestamps use wall time, while fq interprets a transmitted skb timestamp
//! as a monotonic delivery deadline. nf_dup does not scrub the RX timestamp.
//! Clear it on our private veth before reinjecting a reply through LOCAL_OUT;
//! never change an operator's public qdisc or its pacing policy.
use std::{
    ffi::CString,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const NAME: &str = "brocade_hy2_ts";
struct Reference {
    fd: OwnedFd,
    tag: String,
}
static REFERENCE: Mutex<Option<Arc<Reference>>> = Mutex::new(None);

#[repr(C)]
struct Instruction {
    code: u8,
    registers: u8,
    offset: i16,
    immediate: i32,
}

#[repr(C)]
struct Program {
    kind: u32,
    count: u32,
    instructions: u64,
    license: u64,
    log_level: u32,
    log_size: u32,
    log: u64,
    kernel_version: u32,
    flags: u32,
    name: [u8; 16],
}

fn load() -> Result<Arc<Reference>, String> {
    let mut cached = REFERENCE.lock().map_err(|_| "HY2 时间戳程序缓存锁失败")?;
    if let Some(program) = &*cached {
        return Ok(Arc::clone(program));
    }
    // __sk_buff.tstamp is the stable UAPI field at offset 152. A context store
    // is checked and translated by the verifier, not a write to struct sk_buff.
    let instructions = [
        Instruction {
            code: 0x7a,
            registers: 1,
            offset: 152,
            immediate: 0,
        },
        Instruction {
            code: 0xb7,
            registers: 0,
            offset: 0,
            immediate: 0,
        },
        Instruction {
            code: 0x95,
            registers: 0,
            offset: 0,
            immediate: 0,
        },
    ];
    let license = b"GPL\0";
    let mut log = [0_u8; 4096];
    let mut name = [0; 16];
    name[..NAME.len()].copy_from_slice(NAME.as_bytes());
    let program = Program {
        kind: 3, // BPF_PROG_TYPE_SCHED_CLS
        count: instructions.len() as u32,
        instructions: instructions.as_ptr() as u64,
        license: license.as_ptr() as u64,
        log_level: 1,
        log_size: log.len() as u32,
        log: log.as_mut_ptr() as u64,
        kernel_version: 0,
        flags: 0,
        name,
    };
    // SAFETY: the repr(C) attribute and all referenced buffers remain live for
    // the synchronous BPF_PROG_LOAD syscall; the verifier checks the program.
    let fd = unsafe { libc::syscall(libc::SYS_bpf, 5, &program, std::mem::size_of_val(&program)) };
    if fd < 0 {
        let detail =
            String::from_utf8_lossy(&log[..log.iter().position(|b| *b == 0).unwrap_or(log.len())]);
        return Err(format!(
            "HY2 时间戳程序加载失败：{} {detail}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: a successful load returns one newly owned program descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    // Kernels may rewrite context stores differently before computing the tag.
    // Compare with a reference loaded by this kernel, not a baked-in tag from
    // one build machine. Keep one descriptor per process, without pinning maps.
    #[repr(C)]
    struct Info {
        fd: u32,
        length: u32,
        pointer: u64,
    }
    let mut bytes = [0_u8; 64];
    let mut info = Info {
        fd: fd.as_raw_fd() as u32,
        length: bytes.len() as u32,
        pointer: bytes.as_mut_ptr() as u64,
    };
    // SAFETY: info and its writable buffer remain live through OBJ_GET_INFO_BY_FD.
    if unsafe { libc::syscall(libc::SYS_bpf, 15, &mut info, std::mem::size_of::<Info>()) } < 0 {
        return Err(format!(
            "HY2 时间戳程序信息读取失败：{}",
            io::Error::last_os_error()
        ));
    }
    let tag = bytes[8..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let reference = Arc::new(Reference { fd, tag });
    *cached = Some(Arc::clone(&reference));
    Ok(reference)
}

pub(super) fn preflight() -> Result<(), String> {
    load().map(drop)
}

pub(super) fn matches(interface: &str) -> Result<bool, String> {
    let reference = load()?;
    let output = super::run("tc", &["-j", "filter", "show", "dev", interface, "ingress"])?;
    let filters: Vec<serde_json::Value> =
        serde_json::from_str(&output).map_err(|e| e.to_string())?;
    let mut found = false;
    for filter in filters {
        if filter["pref"] != 1 || filter.get("options").is_none() {
            continue;
        }
        let options = &filter["options"];
        if filter["kind"] != "bpf"
            || options["handle"] != "0x1"
            || options["bpf_name"] != NAME
            || options["prog"]["tag"] != reference.tag
            || options["direct-action"] != true
        {
            return Err("HY2 私有接口的时间戳过滤器位置已被其他规则占用".into());
        }
        found = true;
    }
    Ok(found)
}

pub(super) fn ensure(interface: &str) -> Result<(), String> {
    if matches(interface)? {
        return Ok(());
    }
    let program = load()?;
    let qdiscs: Vec<serde_json::Value> = serde_json::from_str(&super::run(
        "tc",
        &["-j", "qdisc", "show", "dev", interface],
    )?)
    .map_err(|e| e.to_string())?;
    if !qdiscs.iter().any(|q| q["kind"] == "clsact") {
        super::run("tc", &["qdisc", "add", "dev", interface, "clsact"])?;
    }
    attach(interface, &program.fd)?;
    if !matches(interface)? {
        return Err("HY2 时间戳过滤器安装后未通过核验".into());
    }
    Ok(())
}

fn attribute(kind: u16, payload: &[u8], output: &mut Vec<u8>) {
    let length = 4 + payload.len();
    output.extend_from_slice(&(length as u16).to_ne_bytes());
    output.extend_from_slice(&kind.to_ne_bytes());
    output.extend_from_slice(payload);
    output.resize(output.len() + (4 - length % 4) % 4, 0);
}

fn attach(interface: &str, program: &OwnedFd) -> Result<(), String> {
    let interface = CString::new(interface).map_err(|_| "invalid HY2 interface")?;
    // SAFETY: interface is a live NUL-terminated name.
    let index = unsafe { libc::if_nametoindex(interface.as_ptr()) };
    if index == 0 {
        return Err(format!(
            "HY2 时间戳接口查找失败：{}",
            io::Error::last_os_error()
        ));
    }
    let mut options = Vec::new();
    attribute(6, &program.as_raw_fd().to_ne_bytes(), &mut options); // TCA_BPF_FD
    attribute(7, b"brocade_hy2_ts\0", &mut options);
    attribute(8, &1_u32.to_ne_bytes(), &mut options); // direct action
    let mut request = vec![0_u8; 36]; // nlmsghdr + tcmsg
    request[4..6].copy_from_slice(&44_u16.to_ne_bytes()); // RTM_NEWTFILTER
    request[6..8].copy_from_slice(&0x605_u16.to_ne_bytes()); // REQUEST|ACK|CREATE|EXCL
    request[8..12].copy_from_slice(&1_u32.to_ne_bytes());
    request[20..24].copy_from_slice(&index.to_ne_bytes());
    request[24..28].copy_from_slice(&1_u32.to_ne_bytes());
    request[28..32].copy_from_slice(&0xfffffff2_u32.to_ne_bytes()); // clsact ingress
    request[32..36].copy_from_slice(&(0x10000_u32 | u16::to_be(3) as u32).to_ne_bytes());
    attribute(1, b"bpf\0", &mut request);
    attribute(2, &options, &mut request);
    let length = request.len() as u32;
    request[..4].copy_from_slice(&length.to_ne_bytes());
    // SAFETY: these arguments create a fresh nonblocking route-netlink socket.
    let raw = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_ROUTE,
        )
    };
    if raw < 0 {
        return Err(format!(
            "HY2 时间戳 netlink socket：{}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: raw is a newly owned socket descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: zero initializes sockaddr_nl; pid 0 addresses the kernel.
    let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    // SAFETY: address is live and has the supplied size.
    if unsafe {
        libc::connect(
            fd.as_raw_fd(),
            std::ptr::addr_of!(address).cast(),
            std::mem::size_of_val(&address) as libc::socklen_t,
        )
    } < 0
    {
        return Err(format!(
            "HY2 时间戳 netlink connect：{}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: the request remains live for this synchronous send.
    if unsafe { libc::send(fd.as_raw_fd(), request.as_ptr().cast(), request.len(), 0) }
        != request.len() as isize
    {
        return Err(format!(
            "HY2 时间戳 netlink send：{}",
            io::Error::last_os_error()
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("HY2 时间戳 netlink 确认超时（3 秒）".into());
        }
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll describes one live socket for a bounded interval.
        let ready = unsafe { libc::poll(&mut poll, 1, remaining.as_millis().max(1) as i32) };
        if ready < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if ready < 0 || poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(format!(
                "HY2 时间戳 netlink poll：{}",
                io::Error::last_os_error()
            ));
        }
        if ready == 0 {
            continue;
        }
        let mut reply = [0_u8; 4096];
        // SAFETY: reply is writable for its full advertised size.
        let received = unsafe {
            libc::recv(
                fd.as_raw_fd(),
                reply.as_mut_ptr().cast(),
                reply.len(),
                libc::MSG_TRUNC,
            )
        };
        if received < 0
            && matches!(
                io::Error::last_os_error().kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            )
        {
            continue;
        }
        if received < 20
            || received as usize > reply.len()
            || u16::from_ne_bytes([reply[4], reply[5]]) != 2
            || u32::from_ne_bytes(
                reply[8..12]
                    .try_into()
                    .map_err(|_| "truncated HY2 netlink sequence")?,
            ) != 1
        {
            return Err("HY2 时间戳 netlink 确认不完整".into());
        }
        let code = i32::from_ne_bytes(
            reply[16..20]
                .try_into()
                .map_err(|_| "truncated HY2 netlink error")?,
        );
        return if code == 0 {
            Ok(())
        } else {
            Err(format!(
                "HY2 时间戳过滤器安装失败：{}",
                io::Error::from_raw_os_error(-code)
            ))
        };
    }
}
