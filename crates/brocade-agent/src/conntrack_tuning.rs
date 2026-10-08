//! Persistent host-level conntrack sizing.
//!
//! The installer establishes this once for a new node. Agent startup repeats the
//! small reconciliation so machines enrolled before that installer change gain
//! the same floor without another service or an Xray restart.

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

const MIN_CONNTRACK_MAX: u64 = 32_768;
const TRANSPORT_COMMENT: &str = "# Brocade: TCP transport defaults.";
const CONFIG_COMMENT: &str = "# Brocade: conntrack for Hysteria 2 port hopping.";
const MAX_KEY: &str = "net.netfilter.nf_conntrack_max";
const UDP_TIMEOUT_KEY: &str = "net.netfilter.nf_conntrack_udp_timeout";
const UDP_STREAM_TIMEOUT_KEY: &str = "net.netfilter.nf_conntrack_udp_timeout_stream";

pub(crate) struct ReconcileResult {
    pub(crate) target_max: u64,
    pub(crate) changed: bool,
}

pub(crate) fn reconcile() -> Result<ReconcileResult, String> {
    const CONFIG_PATH: &str = "/etc/sysctl.d/99-brocade.conf";
    const MODULE_CONFIG_PATH: &str = "/etc/modules-load.d/brocade.conf";
    const MAX_PATH: &str = "/proc/sys/net/netfilter/nf_conntrack_max";
    const UDP_TIMEOUT_PATH: &str = "/proc/sys/net/netfilter/nf_conntrack_udp_timeout";
    const UDP_STREAM_TIMEOUT_PATH: &str = "/proc/sys/net/netfilter/nf_conntrack_udp_timeout_stream";

    if !Path::new(MAX_PATH).is_file() {
        crate::run_command("modprobe", &["nf_conntrack"])
            .map_err(|error| format!("加载 nf_conntrack 失败：{error}"))?;
    }

    let config_path = Path::new(CONFIG_PATH);
    let existing = read_optional_regular_file(config_path)?;
    let current = read_u64(Path::new(MAX_PATH))?;
    let configured = configured_conntrack_max(&existing).unwrap_or(0);
    let target = current.max(configured).max(MIN_CONNTRACK_MAX);
    let rendered = render_config(&existing, target);

    let mut changed = ensure_module_config(Path::new(MODULE_CONFIG_PATH))?;
    if existing.as_bytes() != rendered.as_bytes()
        || file_mode(config_path).is_some_and(|mode| mode != 0o644)
    {
        ensure_parent(config_path)?;
        crate::fsutil::atomic_write_with_mode(config_path, rendered.as_bytes(), 0o644)?;
        changed = true;
    }

    changed |= write_u64_if_different(Path::new(MAX_PATH), target)?;
    changed |= write_u64_if_different(Path::new(UDP_TIMEOUT_PATH), 30)?;
    changed |= write_u64_if_different(Path::new(UDP_STREAM_TIMEOUT_PATH), 60)?;

    Ok(ReconcileResult {
        target_max: target,
        changed,
    })
}

fn read_optional_regular_file(path: &Path) -> Result<String, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(format!("拒绝覆盖符号链接 {}", path.display()));
            }
            if !metadata.is_file() {
                return Err(format!("{} 不是普通文件", path.display()));
            }
            fs::read_to_string(path)
                .map_err(|error| format!("读取 {} 失败：{error}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(format!("检查 {} 失败：{error}", path.display())),
    }
}

fn ensure_module_config(path: &Path) -> Result<bool, String> {
    let existing = read_optional_regular_file(path)?;
    let mut lines = existing
        .lines()
        .filter(|line| line.trim() != "nf_conntrack")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines.push("nf_conntrack".to_owned());
    let rendered = lines.join("\n") + "\n";
    if existing == rendered && file_mode(path) == Some(0o644) {
        return Ok(false);
    }
    ensure_parent(path)?;
    crate::fsutil::atomic_write_with_mode(path, rendered.as_bytes(), 0o644)?;
    Ok(true)
}

fn ensure_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} 没有父目录", path.display()))?;
    if !parent.is_dir() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建 {} 失败：{error}", parent.display()))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("设置 {} 权限失败：{error}", parent.display()))?;
    }
    Ok(())
}

fn file_mode(path: &Path) -> Option<u32> {
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| metadata.permissions().mode() & 0o777)
}

fn read_u64(path: &Path) -> Result<u64, String> {
    let value = fs::read_to_string(path)
        .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    value
        .trim()
        .parse::<u64>()
        .map_err(|error| format!("解析 {} 失败：{error}", path.display()))
}

fn write_u64_if_different(path: &Path, value: u64) -> Result<bool, String> {
    if read_u64(path)? == value {
        return Ok(false);
    }
    fs::write(path, format!("{value}\n"))
        .map_err(|error| format!("写入 {} 失败：{error}", path.display()))?;
    if read_u64(path)? != value {
        return Err(format!("写入 {} 后回读值不一致", path.display()));
    }
    Ok(true)
}

fn configured_conntrack_max(contents: &str) -> Option<u64> {
    contents.lines().filter_map(parse_conntrack_max).max()
}

fn parse_conntrack_max(line: &str) -> Option<u64> {
    let (key, value) = line.split_once('=')?;
    (key.trim() == MAX_KEY)
        .then(|| value.trim().parse::<u64>().ok())
        .flatten()
}

fn render_config(existing: &str, target_max: u64) -> String {
    let had_transport_comment = existing.lines().any(is_transport_comment);
    let mut lines = existing
        .lines()
        .filter(|line| !is_managed_line(line) && !is_transport_comment(line))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    if had_transport_comment {
        lines.insert(0, TRANSPORT_COMMENT.to_owned());
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.extend([
        CONFIG_COMMENT.to_owned(),
        format!("{MAX_KEY} = {target_max}"),
        format!("{UDP_TIMEOUT_KEY} = 30"),
        format!("{UDP_STREAM_TIMEOUT_KEY} = 60"),
    ]);
    lines.join("\n") + "\n"
}

fn is_transport_comment(line: &str) -> bool {
    matches!(
        line.trim(),
        TRANSPORT_COMMENT
            | "# 由 brocade 安装脚本写入。"
            | "# 由 brocade 安装脚本写入。控制台靠这个文件在不在，区分「本来就是 bbr」和"
            | "# 「我们设成了 bbr，后来被人改回去了」——两句话对应的处理完全不同。"
    )
}

fn is_managed_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed == CONFIG_COMMENT
        || trimmed.starts_with("# 连接跟踪。hy2 端口跳转")
        || trimmed.starts_with("# 机器上是三四千条。")
    {
        return true;
    }
    let Some((key, _)) = trimmed.split_once('=') else {
        return false;
    };
    matches!(
        key.trim(),
        MAX_KEY | UDP_TIMEOUT_KEY | UDP_STREAM_TIMEOUT_KEY
    )
}

#[cfg(test)]
mod tests {
    use std::{env, fs, os::unix::fs::PermissionsExt};

    use super::{configured_conntrack_max, ensure_module_config, render_config};

    #[test]
    fn render_replaces_legacy_block_and_preserves_other_settings() {
        let existing =
            "# 由 brocade 安装脚本写入。控制台靠这个文件在不在，区分「本来就是 bbr」和\n\
# 「我们设成了 bbr，后来被人改回去了」——两句话对应的处理完全不同。\n\
net.core.default_qdisc = fq\n\n\
# 连接跟踪。hy2 端口跳转每换一个目的端口就是一条新表项，而内核按内存推出来的默认值在小内存\n\
# 机器上是三四千条。表满之后丢的是新流，已建立的照常走。\n\
net.netfilter.nf_conntrack_max = 4096\n\
net.netfilter.nf_conntrack_udp_timeout = 120\n\
net.netfilter.nf_conntrack_udp_timeout_stream = 120\n\
net.netfilter.nf_conntrack_tcp_timeout_established = 1800\n";

        let rendered = render_config(existing, 32_768);
        assert_eq!(
            rendered,
            "# Brocade: TCP transport defaults.\n\
net.core.default_qdisc = fq\n\n\
net.netfilter.nf_conntrack_tcp_timeout_established = 1800\n\n\
# Brocade: conntrack for Hysteria 2 port hopping.\n\
net.netfilter.nf_conntrack_max = 32768\n\
net.netfilter.nf_conntrack_udp_timeout = 30\n\
net.netfilter.nf_conntrack_udp_timeout_stream = 60\n"
        );
        assert_eq!(render_config(&rendered, 32_768), rendered);
    }

    #[test]
    fn configured_max_keeps_the_highest_existing_value() {
        assert_eq!(
            configured_conntrack_max(
                "net.netfilter.nf_conntrack_max = 65536\n\
                 net.netfilter.nf_conntrack_max=262144\n"
            ),
            Some(262_144)
        );
        assert_eq!(configured_conntrack_max("# none\n"), None);
    }

    #[test]
    fn module_config_preserves_other_modules_and_is_idempotent() {
        let dir = env::temp_dir().join(format!("brocade-conntrack-module-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("brocade.conf");
        fs::write(&path, b"wireguard\nnf_conntrack\nnf_conntrack\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(ensure_module_config(&path).unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"wireguard\nnf_conntrack\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert!(!ensure_module_config(&path).unwrap());

        let _ = fs::remove_dir_all(dir);
    }
}
