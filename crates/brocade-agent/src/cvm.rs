//! CVM workers share one Rust Cube backend; execution and observation stay independent.
mod backend;
mod chain_egress;
pub(super) mod diagnostics;
mod entries;
pub(super) mod host;
mod network_quota;
pub(crate) mod nics;
mod observer;
mod reconcile;
mod terminal;

const IDLE_POLL_MIN_SECS: u64 = 45;
const IDLE_POLL_JITTER_SECS: u64 = 30;

/// Spread idle fallback polls across the fleet. The realtime desired-state wake remains the
/// primary path; this interval only bounds recovery when a wake is missed while connected.
fn idle_poll_interval(options: &crate::options::Options, domain: &str) -> std::time::Duration {
    crate::wake::stable_poll_interval(
        &options.token,
        &format!("cvm-{domain}"),
        IDLE_POLL_MIN_SECS,
        IDLE_POLL_JITTER_SECS,
    )
}

pub(crate) fn check(args: &[String]) -> Result<(), String> {
    if args == ["--baremetal-chain-contract"] {
        println!("brocade-host-chain-egress-v1");
        return Ok(());
    }
    if args == ["--network-contract"] {
        println!("cubevs-attachments-v2");
        return Ok(());
    }
    if args == ["--version"] {
        println!("brocade-cube-rust-v1");
        return Ok(());
    }
    if args == ["--maintenance-contract"] {
        println!("brocade-cvm-maintenance-v1");
        return Ok(());
    }
    if let [empty, flag, path] = args {
        if empty == "--empty" && flag == "--config" {
            let state = backend::state_path()?;
            brocade_cube::Backend::inspect_empty(std::path::Path::new(path), &state)?;
            println!("brocade-cvm-empty-v1");
            return Ok(());
        }
        if empty == "--empty" && flag == "--host-config" {
            host::Backend::inspect_empty(std::path::Path::new(path), &backend::host_state_path()?)?;
            println!("brocade-cvm-host-empty-v1");
            return Ok(());
        }
    }
    let (config, host) = match args {
        [] => (backend::config_path()?, false),
        [flag, path] if flag == "--config" => (std::path::PathBuf::from(path), false),
        [flag, path] if flag == "--host-config" => (std::path::PathBuf::from(path), true),
        _ => {
            return Err(
                "usage: brocade-agent cvm-check [--config /absolute/cube.json | --host-config /absolute/host.json | --version]".into(),
            )
        }
    };
    if host {
        let inventory = host::system_inventory(&config)?;
        println!(
            "{}",
            serde_json::to_string(
                &serde_json::json!({"executor":"brocade-host-system-v1","host_system":inventory})
            )
            .map_err(|_| "host system inventory serialization failed")?
        );
        return Ok(());
    }
    let capabilities = backend::inspect(&config)?;
    println!(
        "{}",
        serde_json::to_string(
            &serde_json::json!({"executor":"brocade-cube-rust-v1","capabilities":capabilities})
        )
        .map_err(|_| "CVM capabilities serialization failed")?
    );
    Ok(())
}

/// Pending host transitions fence only CVM; the other Agent domains keep running.
pub(super) fn maintenance() -> Result<bool, String> {
    maintenance_at(std::path::Path::new("/opt/brocade/cvm/host.json"))
}

fn maintenance_at(path: &std::path::Path) -> Result<bool, String> {
    let Some(bytes) = crate::fsutil::read_bounded(path, 64 * 1024)? else {
        return Ok(false);
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| "CVM virtualization transition state is invalid")?;
    if !value.is_object() {
        return Err("CVM virtualization transition state is invalid".into());
    }
    Ok(value
        .get("pending")
        .is_some_and(|pending| !pending.is_null()))
}

pub(crate) fn spawn(options: crate::options::Options) -> Result<(), String> {
    host::initialize(&options);
    nics::initialize(&options);
    network_quota::spawn(options.clone())?;
    nics::spawn(options.clone())?;
    entries::spawn(options.clone())?;
    reconcile::spawn(options.clone())?;
    observer::spawn(options.clone())?;
    terminal::spawn(options)
}

pub(crate) fn inspect_empty(
    config: &std::path::Path,
    state: &std::path::Path,
) -> Result<(), String> {
    brocade_cube::Backend::inspect_empty(config, state)
}

pub(crate) fn inspect_host_empty(
    config: &std::path::Path,
    state: &std::path::Path,
) -> Result<(), String> {
    host::Backend::inspect_empty(config, state)
}

pub(crate) fn native_chain_configured() -> Result<bool, String> {
    if backend::host_config_path()?.is_none() {
        return Ok(false);
    }
    host::chain_configured(&backend::host_state_path()?)
}

#[cfg(test)]
mod tests {
    use crate::options::{ApplyMode, Options};
    use std::time::Duration;

    fn options(token: &str) -> Options {
        Options {
            command: "run".into(),
            server: "http://127.0.0.1".into(),
            token: token.into(),
            state_dir: std::env::temp_dir(),
            apply_mode: ApplyMode::StateDir,
            vpngate_stats_window: Duration::from_secs(900),
        }
    }

    #[test]
    fn idle_fallback_is_stable_and_bounded_per_worker() {
        let options = options("fixture-token");
        for domain in ["nics", "entries", "reconcile", "observer"] {
            let interval = super::idle_poll_interval(&options, domain);
            assert_eq!(interval, super::idle_poll_interval(&options, domain));
            assert!(interval >= Duration::from_secs(45));
            assert!(interval <= Duration::from_secs(75));
        }
    }

    #[test]
    fn host_transition_fences_cvm_until_commit_and_rejects_corrupt_state() {
        let root =
            std::env::temp_dir().join(format!("brocade-cvm-maintenance-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("host.json");
        assert!(!super::maintenance_at(&path).unwrap());
        std::fs::write(&path, br#"{"pending":{"phase":"awaiting_reboot"}}"#).unwrap();
        assert!(super::maintenance_at(&path).unwrap());
        std::fs::write(&path, br#"{"effective":"pvm"}"#).unwrap();
        assert!(!super::maintenance_at(&path).unwrap());
        std::fs::write(&path, b"invalid").unwrap();
        assert!(super::maintenance_at(&path).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires an isolated Console, assigned KVM Guest and explicit private stop marker"]
    fn kvm_terminal_service() {
        assert_eq!(std::env::var("BROCADE_RUN_CVM_KVM").as_deref(), Ok("1"));
        let options = crate::options::Options::parse(vec!["run".into()]).unwrap();
        super::reconcile::round(&options, &mut 0).unwrap();
        let backend = super::backend::get().unwrap();
        backend
            .wait_idle(std::time::Duration::from_secs(300))
            .unwrap();
        super::reconcile::round(&options, &mut 0).unwrap();
        backend
            .wait_idle(std::time::Duration::from_secs(300))
            .unwrap();
        super::terminal::spawn(options.clone()).unwrap();
        let stop = options.state_dir.join("terminal-service.stop");
        assert!(!stop.exists(), "acceptance stop marker must be fresh");
        let started = std::time::Instant::now();
        while !stop.exists() {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(180),
                "terminal acceptance did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let state = std::path::PathBuf::from(std::env::var_os("BROCADE_CVM_STATE_DIR").unwrap());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            backend.inventory().unwrap();
            let pending = std::fs::read_dir(&state).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".guest-io-")
            });
            if !pending {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Guest terminal exec cleanup incomplete"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    #[test]
    #[ignore = "requires an isolated Docker Console and real KVM/Cube execution host"]
    fn kvm_convergence_round() {
        assert_eq!(std::env::var("BROCADE_RUN_CVM_KVM").as_deref(), Ok("1"));
        let options = crate::options::Options::parse(vec!["run".into()])
            .expect("external acceptance Agent options must be configured");
        assert_eq!(options.apply_mode, crate::options::ApplyMode::Linux);
        super::nics::initialize(&options);
        super::reconcile::round(&options, &mut 0)
            .expect("real CVM convergence round must complete");
        if let Ok(backend) = super::backend::get() {
            backend
                .wait_idle(std::time::Duration::from_secs(300))
                .expect("real Cube intent must reach a durable boundary");
            super::reconcile::round(&options, &mut 0).expect("completed CVM receipt must upload");
            backend
                .wait_idle(std::time::Duration::from_secs(300))
                .expect("receipt recheck must finish");
        }
        let observed = super::observer::round(&options);
        if std::env::var("BROCADE_EXPECT_CVM_OBSERVER_UNAVAILABLE").as_deref() == Ok("1") {
            assert!(observed.is_err());
        } else {
            observed.expect("independent CVM observation round must complete");
            if let Ok(backend) = super::backend::get() {
                backend
                    .collect_observations()
                    .expect("independent Guest sampling must complete");
                super::observer::round(&options).expect("actual Guest samples must upload");
            }
        }
    }

    #[test]
    #[ignore = "requires explicitly assigned targets on an isolated KVM/Cube execution host"]
    fn kvm_observation_cycle() {
        assert_eq!(std::env::var("BROCADE_RUN_CVM_KVM").as_deref(), Ok("1"));
        let root = std::path::PathBuf::from(
            std::env::var_os("BROCADE_CVM_OBSERVATION_INPUT").expect("private input path required"),
        );
        let bytes = std::fs::read(root).expect("assigned observation request must be readable");
        assert!(bytes.len() <= 2 * 1024 * 1024);
        #[derive(serde::Deserialize)]
        struct Input {
            targets: Vec<brocade_deployment::cvm::ObservationTarget>,
            active_ids: Vec<String>,
        }
        let request: Input =
            serde_json::from_slice(&bytes).expect("valid observation assignment required");
        let backend = super::backend::get().expect("real Cube backend required");
        backend
            .observe(&request.targets, &request.active_ids)
            .expect("valid observation assignment required");
        backend
            .collect_observations()
            .expect("actual Guest probes must complete");
        let observations = backend
            .observe(&request.targets, &request.active_ids)
            .expect("actual observations must be available");
        let output = std::path::PathBuf::from(
            std::env::var_os("BROCADE_CVM_OBSERVATION_OUTPUT")
                .expect("private output path required"),
        );
        crate::fsutil::atomic_write_private(&output, &serde_json::to_vec(&observations).unwrap())
            .expect("observation output must persist privately");
    }
}
