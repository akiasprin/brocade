//! A namespace listener does not occupy the main namespace's UDP port allocator.
//! Reserve its public ranges there so ordinary outbound replies cannot enter HY2.
use super::*;
use std::io::{ErrorKind, Read};

const CHECKPOINT: &str = "hy2-port-reservation.json";
const SYSCTL: &str = "/proc/sys/net/ipv4/ip_local_reserved_ports";
const MAXIMUM_SYSCTL_BYTES: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Reservation {
    version: u32,
    boot_id: String,
    owner: String,
    original: String,
    applied: String,
}

fn parse(text: &str) -> Result<BTreeSet<u16>, String> {
    let mut ports = BTreeSet::new();
    if text.trim().is_empty() {
        return Ok(ports);
    }
    for item in text.trim().split(',') {
        let (start, end) = item.split_once('-').unwrap_or((item, item));
        let start = start.parse::<u16>().map_err(|_| "保留端口格式错误")?;
        let end = end.parse::<u16>().map_err(|_| "保留端口格式错误")?;
        if start > end {
            return Err("保留端口范围倒置".into());
        }
        ports.extend(start..=end);
    }
    Ok(ports)
}

fn render(ports: &BTreeSet<u16>) -> String {
    let mut ranges = Vec::new();
    let mut iter = ports.iter().copied().peekable();
    while let Some(start) = iter.next() {
        let mut end = start;
        while iter.peek().is_some_and(|p| end.checked_add(1) == Some(*p)) {
            end = iter
                .next()
                .expect("peeked reservation port remains in the same iterator");
        }
        ranges.push(if start == end {
            start.to_string()
        } else {
            format!("{start}-{end}")
        });
    }
    ranges.join(",")
}

fn read_value(mut reader: impl Read) -> Result<String, String> {
    // proc_dobitmap advances to EOF after its first read, even when the caller's
    // buffer truncates the value. read_to_end starts with a small buffer, so it
    // can silently turn a valid port list into a partial number or range.
    let mut bytes = vec![0; MAXIMUM_SYSCTL_BYTES + 1];
    let length = loop {
        match reader.read(&mut bytes) {
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            result => break result.map_err(|e| format!("读取系统保留端口失败：{e}"))?,
        }
    };
    if length > MAXIMUM_SYSCTL_BYTES {
        return Err("系统保留端口内容超限".into());
    }
    if length != 0 && bytes[length - 1] != b'\n' {
        return Err("系统保留端口读取不完整".into());
    }
    let text = std::str::from_utf8(&bytes[..length]).map_err(|_| "系统保留端口编码错误")?;
    Ok(render(&parse(text)?))
}

fn current() -> Result<String, String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(SYSCTL)
        .map_err(|e| format!("读取系统保留端口失败：{e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("系统保留端口不是普通 sysctl 文件".into());
    }
    read_value(file)
}

fn checkpoint(state_dir: &Path) -> Result<Option<Reservation>, String> {
    let Some(text) = optional_file(&state_dir.join(CHECKPOINT))? else {
        return Ok(None);
    };
    let saved: Reservation =
        serde_json::from_str(&text).map_err(|e| format!("HY2 保留端口检查点损坏：{e}"))?;
    if saved.version != 1 || saved.owner != identity(state_dir)?.2 {
        return Err("HY2 保留端口检查点归属不匹配".into());
    }
    parse(&saved.original)?;
    parse(&saved.applied)?;
    Ok(Some(saved))
}

fn plan(state_dir: &Path, spec: &Spec) -> Result<(String, Reservation), String> {
    let current = current()?;
    let boot = boot_id()?;
    let saved = checkpoint(state_dir)?;
    // External edits make old shared-setting ownership ambiguous. Preserve the
    // complete current value and claim only the additions made from this point.
    let original = saved
        .filter(|s| s.boot_id == boot && s.applied == current)
        .map_or_else(|| current.clone(), |s| s.original);
    let mut ports = parse(&current)?;
    for range in &spec.redirects {
        ports.extend(range.start..=range.end);
    }
    let ephemeral =
        fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range").map_err(|e| e.to_string())?;
    let values = ephemeral
        .split_whitespace()
        .map(str::parse::<u16>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "临时端口范围格式错误")?;
    if values.len() != 2
        || values[0] > values[1]
        || (values[0]..=values[1]).all(|p| ports.contains(&p))
    {
        return Err("HY2 端口段将耗尽主空间临时端口，保留当前服务".into());
    }
    Ok((
        current,
        Reservation {
            version: 1,
            boot_id: boot,
            owner: identity(state_dir)?.2,
            original,
            applied: render(&ports),
        },
    ))
}

pub(super) fn preflight(state_dir: &Path, spec: &Spec) -> Result<String, String> {
    let (current, desired) = plan(state_dir, spec)?;
    if current != desired.applied {
        // Opening a proc sysctl without truncation does not change its value.
        // Detect read-only mounts before stopping the healthy serving process.
        fs::OpenOptions::new()
            .write(true)
            .open(SYSCTL)
            .map_err(|e| format!("HY2 无法保留服务端口，未停止当前服务：{e}"))?;
    }
    Ok(desired.applied)
}

pub(super) fn reserve(state_dir: &Path, spec: &Spec) -> Result<(), String> {
    let (current, desired) = plan(state_dir, spec)?;
    if current == desired.applied {
        return Ok(());
    }
    write_json(state_dir, CHECKPOINT, &desired)?;
    fs::write(SYSCTL, format!("{}\n", desired.applied))
        .map_err(|e| format!("保留 HY2 服务端口失败：{e}"))?;
    if self::current()? != desired.applied {
        return Err("HY2 保留端口写入后发生变化".into());
    }
    Ok(())
}

pub(super) fn release(state_dir: &Path) -> Result<(), String> {
    let Some(saved) = checkpoint(state_dir)? else {
        return Ok(());
    };
    if saved.boot_id == boot_id()? && current()? == saved.applied {
        fs::write(SYSCTL, format!("{}\n", saved.original))
            .map_err(|e| format!("恢复原有保留端口失败：{e}"))?;
    }
    // If another administrator changed the value, do not remove shared ranges
    // whose ownership can no longer be demonstrated. A reboot also resets it.
    fs::remove_file(state_dir.join(CHECKPOINT)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OneRead<'a>(&'a [u8]);

    impl Read for OneRead<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let length = buffer.len().min(self.0.len());
            buffer[..length].copy_from_slice(&self.0[..length]);
            self.0 = &[];
            Ok(length)
        }
    }

    #[test]
    fn bitmap_sysctl_lists_are_read_completely_in_one_read() {
        let ranges = "18100-18499,19200-19299,30000-30099\n";
        assert!(ranges.len() > 32);
        assert_eq!(
            read_value(OneRead(ranges.as_bytes())).unwrap(),
            ranges.trim()
        );
        let long = (1024..=65535)
            .step_by(2)
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert!(long.len() > 4096);
        assert_eq!(
            read_value(OneRead(format!("{long}\n").as_bytes())).unwrap(),
            long
        );
        assert!(read_value(OneRead(b"30000-30")).is_err());
        assert!(read_value(OneRead(&vec![b'1'; MAXIMUM_SYSCTL_BYTES + 2])).is_err());
    }
    #[test]
    fn reservations_preserve_existing_ranges_and_canonicalize_without_overflow() {
        let ports = parse("12,2-4,3-6,65534-65535").unwrap();
        assert_eq!(render(&ports), "2-6,12,65534-65535");
        assert_eq!(parse(&render(&ports)).unwrap(), ports);
        assert!(parse("8-3").is_err());
        assert!(parse("65536").is_err());
        assert!(parse("wrong").is_err());
        assert!(parse("1,,2").is_err());
    }
}
