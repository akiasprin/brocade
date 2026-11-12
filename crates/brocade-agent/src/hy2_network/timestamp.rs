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
// Keep the exact previous program as a compatibility reference, not an install
// candidate. Some kernels reject BPF_ST context stores; a filter already loaded
// on a kernel accepting it still clears tstamp and returns TC_ACT_OK correctly.
const LEGACY_INSTRUCTIONS: [Instruction; 3] = [
    Instruction {
        code: 0x7a, // *(u64 *)(r1 + 152) = 0
        registers: 1,
        offset: 152,
        immediate: 0,
    },
    Instruction {
        code: 0xb7, // r0 = TC_ACT_OK
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
static LEGACY_TAG: Mutex<Option<String>> = Mutex::new(None);
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
    // Use a register store: older verifiers (including Linux 6.1) reject BPF_ST
    // immediate stores to PTR_TO_CTX even when the context field is writable.
    let instructions = [
        Instruction {
            code: 0xb7, // r0 = 0 (also TC_ACT_OK on return)
            registers: 0,
            offset: 0,
            immediate: 0,
        },
        Instruction {
            code: 0x7b, // *(u64 *)(r1 + 152) = r0
            registers: 1,
            offset: 152,
            immediate: 0,
        },
        Instruction {
            code: 0x95,
            registers: 0,
            offset: 0,
            immediate: 0,
        },
    ];
    let reference = Arc::new(load_program(&instructions)?);
    *cached = Some(Arc::clone(&reference));
    Ok(reference)
}

fn load_program(instructions: &[Instruction]) -> Result<Reference, String> {
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
    // Kernels can rewrite instructions before computing the tag. Read each
    // reference's identity from this kernel rather than baking in a build tag.
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
    Ok(Reference { fd, tag })
}

pub(super) fn preflight() -> Result<(), String> {
    load().map(drop)
}

fn legacy_matches(tag: &str) -> Result<bool, String> {
    // Only attempt the old load for an existing filter in our ingress slot whose
    // tag differs. A fresh install on Linux 6.1 never needs this unsupported
    // program. Failed loads preserve the filter and can be retried later.
    let mut cached = LEGACY_TAG
        .lock()
        .map_err(|_| "HY2 旧版时间戳程序缓存锁失败")?;
    if cached.is_none() {
        let program = load_program(&LEGACY_INSTRUCTIONS).map_err(|error| {
            format!("HY2 时间戳过滤器的旧版身份无法确认，已保留现有规则：{error}")
        })?;
        *cached = Some(program.tag);
    }
    Ok(cached
        .as_deref()
        .is_some_and(|known| known.eq_ignore_ascii_case(tag)))
}

fn program_matches(tag: &str) -> Result<bool, String> {
    let current = load()
        .map_err(|error| format!("HY2 时间戳过滤器的程序身份无法确认，已保留现有规则：{error}"))?;
    if tag.eq_ignore_ascii_case(&current.tag) {
        return Ok(true);
    }
    legacy_matches(tag)
}

pub(super) fn matches(interface: &str) -> Result<bool, String> {
    let output = super::run("tc", &["-j", "filter", "show", "dev", interface, "ingress"])?;
    let filters: Vec<serde_json::Value> =
        serde_json::from_str(&output).map_err(|e| e.to_string())?;
    filters_match(&filters, program_matches)
}

fn tc_number(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        let value = value.as_str()?;
        match value.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(hex, 16).ok(),
            None => value.parse().ok(),
        }
    })
}

fn filters_match(
    filters: &[serde_json::Value],
    program_matches: impl Fn(&str) -> Result<bool, String>,
) -> Result<bool, String> {
    let mut found = false;
    for filter in filters {
        // tc emits priority summaries without an attached classifier.
        if filter.get("options").is_none() {
            continue;
        }
        // Older tc versions may omit the default chain. Other chains are not
        // reached by this entry filter and must neither satisfy nor block it.
        let chain = match filter.get("chain") {
            None => 0,
            Some(value) => {
                tc_number(value).ok_or("HY2 时间戳过滤器的 chain 无法确认，已保留现有规则")?
            }
        };
        if chain != 0 {
            continue;
        }
        let pref =
            tc_number(&filter["pref"]).ok_or("HY2 时间戳过滤器的优先级无法确认，已保留现有规则")?;
        if pref != 1 {
            continue;
        }
        let options = &filter["options"];
        if filter["kind"] != "bpf" || options["direct-action"] != true {
            return Err("HY2 私有接口的时间戳过滤器入口存在规则冲突，已保留现有规则".into());
        }
        let protocol = filter
            .get("protocol")
            .ok_or("HY2 时间戳过滤器的协议范围无法确认，已保留现有规则")?;
        if protocol != "all" && tc_number(protocol) != Some(3) {
            return Err("HY2 时间戳过滤器未覆盖全部协议，已保留现有规则".into());
        }
        match options.get("skip_sw") {
            None | Some(serde_json::Value::Bool(false)) => (),
            Some(serde_json::Value::Bool(true)) => {
                return Err("HY2 时间戳过滤器未在软件路径执行，已保留现有规则".into());
            }
            Some(_) => {
                return Err("HY2 时间戳过滤器的软件执行状态无法确认，已保留现有规则".into());
            }
        }
        // Names and handles are labels, not program identity. Reuse a known
        // equivalent program without requiring the installer's exact labels.
        // iproute2 falls back to the classifier's tag when program-info lookup
        // is unavailable; both forms carry the same kernel program identity.
        let tag = options["prog"]["tag"]
            .as_str()
            .or_else(|| options["tag"].as_str());
        let tag = tag.ok_or("HY2 时间戳过滤器缺少程序指纹，身份无法确认，已保留现有规则")?;
        if !program_matches(tag)? {
            return Err("HY2 时间戳过滤器的程序尚未识别，已保留现有规则".into());
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

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT_TAG: &str = "05b92bdb90d27595";
    const LEGACY_TAG: &str = "672fb7bf1861cf79";

    fn known_program(tag: &str) -> Result<bool, String> {
        Ok(tag.eq_ignore_ascii_case(CURRENT_TAG) || tag.eq_ignore_ascii_case(LEGACY_TAG))
    }

    fn filter(tag: &str) -> serde_json::Value {
        serde_json::json!({
            "protocol": "all", "pref": 1, "kind": "bpf", "chain": 0,
            "options": {"handle": "0x1", "bpf_name": NAME,
                "direct-action": true, "prog": {"id": 42, "name": NAME, "tag": tag}}
        })
    }

    #[test]
    fn recognizes_current_and_equivalent_legacy_programs() {
        for tag in [CURRENT_TAG, LEGACY_TAG] {
            let entry = filter(tag);
            assert!(filters_match(std::slice::from_ref(&entry), known_program).unwrap());
            let summary = serde_json::json!({"pref": 1, "kind": "bpf"});
            assert!(filters_match(&[summary, entry], known_program).unwrap());
        }
    }

    #[test]
    fn recognizes_iproute2_classifier_tag_fallback() {
        let mut entry = filter(LEGACY_TAG);
        entry["options"].as_object_mut().unwrap().remove("prog");
        entry["options"].as_object_mut().unwrap().remove("bpf_name");
        entry["options"]["tag"] = LEGACY_TAG.into();
        assert!(filters_match(&[entry], known_program).unwrap());
    }

    #[test]
    fn known_program_does_not_depend_on_names_or_handle_labels() {
        for tag in [CURRENT_TAG, LEGACY_TAG] {
            let mut entry = filter(tag);
            entry["options"].as_object_mut().unwrap().remove("bpf_name");
            entry["options"]["prog"]
                .as_object_mut()
                .unwrap()
                .remove("name");
            assert!(filters_match(std::slice::from_ref(&entry), known_program).unwrap());
            entry["options"]["bpf_name"] = "another annotation".into();
            entry["options"]["prog"]["name"] = "another_name".into();
            entry["options"]["handle"] = "0x2".into();
            assert!(filters_match(&[entry], known_program).unwrap());
        }
    }

    #[test]
    fn unrelated_chains_can_coexist_in_either_dump_order() {
        let unrelated = serde_json::json!({"protocol": "ip", "pref": 1, "kind": "matchall",
            "chain": 1, "options": {"handle": "0x2", "actions": []}});
        for entries in [
            vec![filter(CURRENT_TAG), unrelated.clone()],
            vec![unrelated.clone(), filter(LEGACY_TAG)],
        ] {
            assert!(filters_match(&entries, known_program).unwrap());
        }
        assert!(!filters_match(&[unrelated], |_| panic!(
            "unreachable chain needs no program load"
        ))
        .unwrap());
        let mut unreachable = filter(CURRENT_TAG);
        unreachable["chain"] = 1.into();
        assert!(!filters_match(&[unreachable], known_program).unwrap());
    }

    #[test]
    fn default_chain_and_numeric_tc_representations_are_compatible() {
        let mut entry = filter(CURRENT_TAG);
        entry.as_object_mut().unwrap().remove("chain");
        assert!(filters_match(std::slice::from_ref(&entry), known_program).unwrap());
        entry["chain"] = "0".into();
        entry["pref"] = "1".into();
        for protocol in [
            serde_json::json!("all"),
            serde_json::json!(3),
            serde_json::json!("0x0003"),
            serde_json::json!("3"),
        ] {
            entry["protocol"] = protocol;
            assert!(filters_match(std::slice::from_ref(&entry), known_program).unwrap());
        }
    }

    #[test]
    fn rejects_partial_protocol_coverage_and_disabled_software_execution() {
        for protocol in ["ip", "ipv6", "0x0800", "0x86dd"] {
            let mut entry = filter(CURRENT_TAG);
            entry["protocol"] = protocol.into();
            assert!(filters_match(&[entry], known_program)
                .unwrap_err()
                .contains("未覆盖全部协议"));
        }
        let mut entry = filter(CURRENT_TAG);
        entry["options"]["skip_sw"] = true.into();
        assert!(filters_match(&[entry], known_program)
            .unwrap_err()
            .contains("未在软件路径执行"));
    }

    #[test]
    fn incomplete_observation_is_not_reported_as_missing_or_foreign() {
        for (pointer, value) in [
            ("/chain", serde_json::json!("unknown")),
            ("/pref", serde_json::Value::Null),
            ("/options/skip_sw", serde_json::json!("unknown")),
        ] {
            let mut entry = filter(CURRENT_TAG);
            entry["options"]["skip_sw"] = false.into();
            *entry.pointer_mut(pointer).unwrap() = value;
            assert!(filters_match(&[entry], known_program)
                .unwrap_err()
                .contains("无法确认"));
        }
        let mut entry = filter(CURRENT_TAG);
        entry.as_object_mut().unwrap().remove("protocol");
        assert!(filters_match(&[entry], known_program)
            .unwrap_err()
            .contains("协议范围无法确认"));
    }

    #[test]
    fn rejects_unknown_or_missing_program_fingerprint() {
        for tag in ["0123456789abcdef", ""] {
            assert!(filters_match(&[filter(tag)], known_program)
                .unwrap_err()
                .contains("尚未识别"));
        }
        let mut entry = filter(LEGACY_TAG);
        entry["options"].as_object_mut().unwrap().remove("prog");
        assert!(filters_match(&[entry], known_program)
            .unwrap_err()
            .contains("身份无法确认"));
    }

    #[test]
    fn known_fingerprint_does_not_bypass_classifier_execution_mode() {
        for (pointer, value) in [
            ("/kind", serde_json::json!("matchall")),
            ("/options/direct-action", serde_json::json!(false)),
        ] {
            let mut conflict = filter(LEGACY_TAG);
            *conflict.pointer_mut(pointer).unwrap() = value;
            assert!(
                filters_match(&[filter(CURRENT_TAG), conflict], known_program)
                    .unwrap_err()
                    .contains("规则冲突")
            );
        }
    }

    #[test]
    fn absent_filter_and_other_priorities_need_no_program_load() {
        let no_load = |_: &str| -> Result<bool, String> {
            panic!("irrelevant observations must not require BPF loads")
        };
        assert!(!filters_match(&[], no_load).unwrap());
        let mut other = filter("foreign");
        other["pref"] = 2.into();
        assert!(!filters_match(&[other], no_load).unwrap());
        let summary = serde_json::json!({"chain": "unknown", "pref": 1});
        assert!(!filters_match(&[summary], no_load).unwrap());
    }

    #[test]
    fn inability_to_load_a_reference_is_preserved_as_unconfirmed_identity() {
        let error = filters_match(&[filter(LEGACY_TAG)], |_| {
            Err("reference unavailable".into())
        })
        .unwrap_err();
        assert_eq!(error, "reference unavailable");
    }

    #[test]
    #[ignore = "requires Linux BPF privileges; run on the oldest supported kernel too"]
    fn kernel_program_clears_timestamp_and_preserves_packet() {
        assert_clears_timestamp(&load().unwrap());
    }

    fn assert_clears_timestamp(program: &Reference) {
        #[repr(C)]
        struct TestRun {
            fd: u32,
            return_value: u32,
            data_size_in: u32,
            data_size_out: u32,
            data_in: u64,
            data_out: u64,
            repeat: u32,
            duration: u32,
            context_size_in: u32,
            context_size_out: u32,
            context_in: u64,
            context_out: u64,
        }

        let mut packet = [0_u8; 64];
        packet[12..14].copy_from_slice(&0x0800_u16.to_be_bytes());
        packet[14] = 0x45; // Ethernet + IPv4 header for BPF_PROG_TEST_RUN.
        packet[16..18].copy_from_slice(&50_u16.to_be_bytes());
        for timestamp in [0_u64, 1, 1_700_000_000_123_456_789] {
            // Stable __sk_buff UAPI layout, through hwtstamp (Linux 5.16+).
            let mut context = [0_u8; 192];
            context[8..12].copy_from_slice(&42_u32.to_ne_bytes()); // mark
            context[32..36].copy_from_slice(&7_u32.to_ne_bytes()); // priority
            context[152..160].copy_from_slice(&timestamp.to_ne_bytes());
            let mut packet_out = [0_u8; 64];
            let mut context_out = [0_u8; 192];
            let mut run = TestRun {
                fd: program.fd.as_raw_fd() as u32,
                return_value: u32::MAX,
                data_size_in: packet.len() as u32,
                data_size_out: packet_out.len() as u32,
                data_in: packet.as_ptr() as u64,
                data_out: packet_out.as_mut_ptr() as u64,
                repeat: 1,
                duration: 0,
                context_size_in: context.len() as u32,
                context_size_out: context_out.len() as u32,
                context_in: context.as_ptr() as u64,
                context_out: context_out.as_mut_ptr() as u64,
            };
            // SAFETY: the repr(C) test attributes and all input/output buffers
            // remain live for the synchronous BPF_PROG_TEST_RUN syscall.
            let result = unsafe {
                libc::syscall(libc::SYS_bpf, 10, &mut run, std::mem::size_of::<TestRun>())
            };
            assert_eq!(
                result,
                0,
                "BPF_PROG_TEST_RUN: {}",
                io::Error::last_os_error()
            );
            assert_eq!(run.return_value, 0, "must return TC_ACT_OK");
            assert_eq!(run.data_size_out as usize, packet.len());
            assert_eq!(packet_out, packet);
            assert_eq!(&context_out[8..12], &context[8..12]);
            assert_eq!(&context_out[32..36], &context[32..36]);
            assert_eq!(&context_out[152..160], &0_u64.to_ne_bytes());
        }
    }

    #[test]
    #[ignore = "requires Linux BPF/network privileges and iproute2"]
    fn kernel_filter_reconciles_in_private_namespace() {
        // SAFETY: only this test thread enters a new network namespace. Its
        // interfaces are destroyed on exit; the caller's network is untouched.
        assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWNET) }, 0);
        let run = super::super::run;
        let interface = "hy2-ts-test";
        run("ip", &["link", "add", interface, "type", "dummy"]).unwrap();
        ensure(interface).unwrap();
        assert!(matches(interface).unwrap());
        run(
            "tc",
            &[
                "filter", "add", "dev", interface, "ingress", "chain", "1", "pref", "1",
                "matchall", "action", "pass",
            ],
        )
        .unwrap();
        let before = run("tc", &["-j", "filter", "show", "dev", interface, "ingress"]).unwrap();
        ensure(interface).unwrap();
        assert_eq!(
            before,
            run("tc", &["-j", "filter", "show", "dev", interface, "ingress"]).unwrap(),
            "reconcile must preserve the working entry and unrelated chain"
        );
        run(
            "tc",
            &[
                "filter", "delete", "dev", interface, "ingress", "chain", "0", "protocol", "all",
                "pref", "1", "handle", "0x1", "bpf",
            ],
        )
        .unwrap();
        assert!(!matches(interface).unwrap());
        ensure(interface).unwrap();
        let unrelated = |text: &str| {
            serde_json::from_str::<Vec<serde_json::Value>>(text)
                .unwrap()
                .into_iter()
                .filter(|filter| filter["chain"] == 1)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            unrelated(&before),
            unrelated(&run("tc", &["-j", "filter", "show", "dev", interface, "ingress"]).unwrap()),
            "repairing a missing chain-0 entry must leave chain 1 untouched"
        );
        run("tc", &["qdisc", "delete", "dev", interface, "clsact"]).unwrap();
        assert!(!matches(interface).unwrap());
        ensure(interface).unwrap();
        assert!(matches(interface).unwrap());
    }

    #[test]
    #[ignore = "requires a kernel accepting legacy BPF_ST context stores and network privileges"]
    fn kernel_legacy_filter_survives_agent_upgrade_without_replacement() {
        // SAFETY: this test thread owns an isolated namespace, never the host's.
        assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWNET) }, 0);
        let run = super::super::run;
        let interface = "hy2-ts-legacy";
        run("ip", &["link", "add", interface, "type", "dummy"]).unwrap();
        run("tc", &["qdisc", "add", "dev", interface, "clsact"]).unwrap();
        let legacy = load_program(&LEGACY_INSTRUCTIONS).unwrap();
        assert_clears_timestamp(&legacy);
        assert!(legacy_matches(&legacy.tag).unwrap());
        assert_ne!(legacy.tag, load().unwrap().tag);
        attach(interface, &legacy.fd).unwrap();
        let before = run("tc", &["-j", "filter", "show", "dev", interface, "ingress"]).unwrap();
        assert!(matches(interface).unwrap());
        ensure(interface).unwrap();
        ensure(interface).unwrap();
        assert_eq!(
            before,
            run("tc", &["-j", "filter", "show", "dev", interface, "ingress"]).unwrap(),
            "the working legacy program must stay attached with its original ID"
        );
    }

    #[test]
    #[ignore = "requires Linux BPF/network privileges and iproute2"]
    fn kernel_unknown_filter_is_preserved() {
        // SAFETY: this test thread owns an isolated namespace, never the host's.
        assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWNET) }, 0);
        let run = super::super::run;
        let interface = "hy2-ts-foreign";
        run("ip", &["link", "add", interface, "type", "dummy"]).unwrap();
        run("tc", &["qdisc", "add", "dev", interface, "clsact"]).unwrap();
        let foreign = load_program(&LEGACY_INSTRUCTIONS[1..]).unwrap();
        attach(interface, &foreign.fd).unwrap();
        let before = run("tc", &["-j", "filter", "show", "dev", interface, "ingress"]).unwrap();
        assert!(ensure(interface).unwrap_err().contains("已保留现有规则"));
        assert_eq!(
            before,
            run("tc", &["-j", "filter", "show", "dev", interface, "ingress"]).unwrap(),
            "a same-name program with unknown instructions must never be replaced"
        );
    }
}
