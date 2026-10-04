//! Persistent, live reconciliation of per-NIC software IRQ coalescing.
//!
//! The Console sends the fleet policy on every desired-state response. The Agent stores the last
//! accepted policy so reboot-time convergence does not depend on control-plane availability, then
//! applies it to the physical interface carrying the default route. This is sysfs state shared by
//! all sockets; no Xray restart or separate service is needed.

use std::{fs, io, path::Path};

use brocade_deployment::protocol::{
    HostNetworkTuning, MAX_NIC_GRO_FLUSH_TIMEOUT_NS, MAX_NIC_NAPI_DEFER_HARD_IRQS,
};

const POLICY_FILE: &str = "host-network-tuning.json";
type Attribute = (&'static str, fn(HostNetworkTuning) -> u32);
const ATTRIBUTES: [Attribute; 2] = [
    ("gro_flush_timeout", |settings| {
        settings.gro_flush_timeout_ns
    }),
    ("napi_defer_hard_irqs", |settings| {
        settings.napi_defer_hard_irqs
    }),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplyResult {
    pub(crate) interface: String,
    pub(crate) policy_changed: bool,
    pub(crate) runtime_changed: bool,
}

fn validate(settings: HostNetworkTuning) -> Result<HostNetworkTuning, String> {
    if settings.gro_flush_timeout_ns > MAX_NIC_GRO_FLUSH_TIMEOUT_NS {
        return Err(format!(
            "gro_flush_timeout_ns={} exceeds {MAX_NIC_GRO_FLUSH_TIMEOUT_NS}",
            settings.gro_flush_timeout_ns
        ));
    }
    if settings.napi_defer_hard_irqs > MAX_NIC_NAPI_DEFER_HARD_IRQS {
        return Err(format!(
            "napi_defer_hard_irqs={} exceeds {MAX_NIC_NAPI_DEFER_HARD_IRQS}",
            settings.napi_defer_hard_irqs
        ));
    }
    Ok(settings)
}

fn policy_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join(POLICY_FILE)
}

fn read_saved_or_default(state_dir: &Path) -> Result<HostNetworkTuning, String> {
    match fs::read(policy_path(state_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("decode saved host network tuning: {error}"))
            .and_then(validate),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(HostNetworkTuning::default()),
        Err(error) => Err(format!("read saved host network tuning: {error}")),
    }
}

fn persist(state_dir: &Path, settings: HostNetworkTuning) -> Result<bool, String> {
    let path = policy_path(state_dir);
    let mut bytes = serde_json::to_vec(&settings)
        .map_err(|error| format!("encode host network tuning: {error}"))?;
    bytes.push(b'\n');
    if fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(false);
    }
    crate::fsutil::atomic_write_private(&path, &bytes)?;
    Ok(true)
}

pub(crate) fn reconcile_saved(state_dir: &Path) -> Result<ApplyResult, String> {
    apply(read_saved_or_default(state_dir)?, false)
}

pub(crate) fn apply_control_plane(
    state_dir: &Path,
    settings: HostNetworkTuning,
) -> Result<ApplyResult, String> {
    let settings = validate(settings)?;
    let policy_changed = persist(state_dir, settings)?;
    apply(settings, policy_changed)
}

fn apply(settings: HostNetworkTuning, policy_changed: bool) -> Result<ApplyResult, String> {
    let interface = crate::load::main_interface().ok_or("default-route interface not found")?;
    let runtime_changed = apply_at(Path::new("/sys/class/net"), &interface, settings)?;
    Ok(ApplyResult {
        interface,
        policy_changed,
        runtime_changed,
    })
}

fn apply_at(
    sys_class_net: &Path,
    interface: &str,
    settings: HostNetworkTuning,
) -> Result<bool, String> {
    if interface.is_empty() || interface.contains('/') || matches!(interface, "." | "..") {
        return Err(format!("invalid default-route interface {interface:?}"));
    }

    let interface_dir = sys_class_net.join(interface);
    let mut changed = false;
    let mut failures = Vec::new();
    for (attribute, value) in ATTRIBUTES {
        let wanted = value(settings).to_string();
        let path = interface_dir.join(attribute);
        if !path.is_file() {
            failures.push(format!("{} is unavailable", path.display()));
            continue;
        }

        let current = match fs::read_to_string(&path) {
            Ok(current) => current,
            Err(error) => {
                failures.push(format!("read {}: {error}", path.display()));
                continue;
            }
        };
        if current.trim() != wanted {
            if let Err(error) = fs::write(&path, format!("{wanted}\n")) {
                failures.push(format!("write {}={wanted}: {error}", path.display()));
                continue;
            }
            changed = true;
        }

        match fs::read_to_string(&path) {
            Ok(actual) if actual.trim() == wanted => {}
            Ok(actual) => failures.push(format!(
                "verify {}: got {:?}, want {wanted:?}",
                path.display(),
                actual.trim()
            )),
            Err(error) => failures.push(format!("verify {}: {error}", path.display())),
        }
    }

    if failures.is_empty() {
        Ok(changed)
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "brocade-host-tuning-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_and_verifies_both_requested_eth0_values() {
        let root = temp_dir("values");
        let eth0 = root.join("eth0");
        fs::create_dir_all(&eth0).unwrap();
        fs::write(eth0.join("gro_flush_timeout"), b"0\n").unwrap();
        fs::write(eth0.join("napi_defer_hard_irqs"), b"0\n").unwrap();

        assert!(apply_at(&root, "eth0", HostNetworkTuning::default()).unwrap());
        assert_eq!(
            fs::read_to_string(eth0.join("gro_flush_timeout")).unwrap(),
            "20000\n"
        );
        assert_eq!(
            fs::read_to_string(eth0.join("napi_defer_hard_irqs")).unwrap(),
            "2\n"
        );
        assert!(!apply_at(&root, "eth0", HostNetworkTuning::default()).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsupported_driver_does_not_create_fake_sysfs_attributes() {
        let root = temp_dir("missing");
        let eth0 = root.join("eth0");
        fs::create_dir_all(&eth0).unwrap();
        fs::write(eth0.join("napi_defer_hard_irqs"), b"0\n").unwrap();

        let error = apply_at(&root, "eth0", HostNetworkTuning::default()).unwrap_err();
        assert!(error.contains("gro_flush_timeout"));
        assert!(!eth0.join("gro_flush_timeout").exists());
        assert_eq!(
            fs::read_to_string(eth0.join("napi_defer_hard_irqs")).unwrap(),
            "2\n",
            "independent supported knobs still converge"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn control_plane_policy_is_persisted_for_the_next_start() {
        let state = temp_dir("persist");
        let settings = HostNetworkTuning {
            gro_flush_timeout_ns: 40_000,
            napi_defer_hard_irqs: 4,
        };
        assert!(persist(&state, settings).unwrap());
        assert_eq!(read_saved_or_default(&state).unwrap(), settings);
        assert!(!persist(&state, settings).unwrap());
        fs::remove_dir_all(state).unwrap();
    }
}
