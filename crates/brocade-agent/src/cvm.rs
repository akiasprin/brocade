//! CVM workers share one Rust Cube backend; execution and observation stay independent.
mod backend;
mod entries;
mod network_quota;
pub(crate) mod nics;
mod observer;
mod reconcile;
mod terminal;

pub(crate) fn check(args: &[String]) -> Result<(), String> {
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
            let state = std::env::var_os("BROCADE_CVM_STATE_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("/var/lib/brocade/cvm-cube"));
            brocade_cube::Backend::inspect_empty(std::path::Path::new(path), &state)?;
            println!("brocade-cvm-empty-v1");
            return Ok(());
        }
    }
    let config = match args {
        [] => std::env::var_os("BROCADE_CVM_CONFIG")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/etc/brocade/cvm-cube.json")),
        [flag, path] if flag == "--config" => std::path::PathBuf::from(path),
        _ => {
            return Err(
                "usage: brocade-agent cvm-check [--config /absolute/config.json | --version]"
                    .into(),
            )
        }
    };
    let capabilities = brocade_cube::Backend::inspect(&config)?;
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
    network_quota::spawn(options.clone())?;
    nics::spawn(options.clone())?;
    entries::spawn(options.clone())?;
    reconcile::spawn(options.clone())?;
    observer::spawn(options.clone())?;
    terminal::spawn(options)
}

#[cfg(test)]
mod tests {
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
