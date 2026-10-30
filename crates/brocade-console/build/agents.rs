//! One Cargo graph for both Agent architectures, independent of Console OUT_DIR/profile.
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

pub fn target_dir(workspace: &Path) -> PathBuf {
    workspace.join("target/brocade-agent-build")
}

pub fn command(workspace: &Path, cache: &Path, triples: &[&str]) -> Command {
    assert!(
        !triples.is_empty(),
        "only missing Agent targets are passed to Cargo"
    );
    let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.args(["build", "--release", "--locked", "-p", "brocade-agent"]);
    for triple in triples {
        command.args(["--target", triple]);
    }
    command
        .arg("--manifest-path")
        .arg(workspace.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(cache)
        .current_dir(workspace)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET");
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn both_targets_share_one_graph_and_keep_per_target_compiler_settings() {
        let root = Path::new("/workspace");
        let cache = target_dir(root);
        let command = command(
            root,
            &cache,
            &["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"],
        );
        let args = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let targets = args
            .windows(2)
            .filter(|a| a[0] == "--target")
            .map(|a| a[1].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            targets,
            ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"]
        );
        assert!(args.iter().any(|a| a == "--locked"));
        assert_eq!(command.get_current_dir(), Some(root));
        assert_eq!(cache, root.join("target/brocade-agent-build"));
        for key in [
            "CARGO_TARGET_DIR",
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_BUILD_TARGET",
        ] {
            assert!(command.get_envs().any(|(k, v)| k == key && v.is_none()));
        }
    }

    #[test]
    fn real_cargo_runs_both_target_build_scripts_concurrently() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "brocade-agent-graph-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"brocade-agent\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[workspace]\n").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("build.rs"), r#"
            fn main() {
                let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
                let ready = root.join("ready");
                std::fs::create_dir_all(&ready).unwrap();
                std::fs::write(ready.join(std::env::var("TARGET").unwrap()), b"ready").unwrap();
                let started = std::time::Instant::now();
                while std::fs::read_dir(&ready).unwrap().count() < 2 {
                    assert!(started.elapsed().as_secs() < 15, "target build scripts were serialized");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        "#).unwrap();
        let sysroot = Command::new("rustc")
            .args(["--print", "sysroot"])
            .output()
            .unwrap();
        assert!(sysroot.status.success());
        let sysroot = PathBuf::from(String::from_utf8(sysroot.stdout).unwrap().trim());
        let mut command = command(
            &root,
            &target_dir(&root),
            &["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"],
        );
        command
            .args(["--offline", "--jobs", "4"])
            .env("RUSTUP_TOOLCHAIN", sysroot.file_name().unwrap())
            .env(
                "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS",
                "-C linker=rust-lld",
            );
        // A lockfile is an explicit release prerequisite, including for the synthetic fixture.
        let lock = Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(&root)
            .env("RUSTUP_TOOLCHAIN", sysroot.file_name().unwrap())
            .output()
            .unwrap();
        assert!(lock.status.success());
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for (triple, machine) in [
            ("x86_64-unknown-linux-musl", 62u16),
            ("aarch64-unknown-linux-musl", 183u16),
        ] {
            let binary =
                fs::read(target_dir(&root).join(triple).join("release/brocade-agent")).unwrap();
            assert_eq!(&binary[..6], b"\x7fELF\x02\x01");
            assert_eq!(&binary[18..20], &machine.to_le_bytes());
        }
    }
}
