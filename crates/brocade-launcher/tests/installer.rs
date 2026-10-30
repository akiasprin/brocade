use std::{
    fs::{self, File},
    io::Write,
    os::{fd::FromRawFd, unix::fs::PermissionsExt},
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

const INSTALLER: &str = include_str!("../../../install.sh");

fn executable(path: &Path, source: &str) {
    fs::write(path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn address_command_works_without_starting_a_database() {
    for (ip, port, expected) in [
        ("192.0.2.10", None, "http://192.0.2.10:8080"),
        ("2001:db8::10", None, "http://[2001:db8::10]:8080"),
        ("192.0.2.10", Some("8008"), "http://192.0.2.10:8008"),
        ("2001:db8::10", Some("9000"), "http://[2001:db8::10]:9000"),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_brocade-launcher"));
        command.args(["address", "--public-ip", ip]);
        if let Some(port) = port {
            command.args(["--port", port]);
        }
        let output = command
            .env_remove("BROCADE_ADMIN_BIND")
            .env_remove("BROCADE_AGENT_PUBLIC_URL")
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    }
}

fn installer_section(start: &str, end: &str) -> &'static str {
    let start = INSTALLER.find(start).expect("installer section starts");
    let end = INSTALLER.find(end).expect("installer section ends");
    assert!(start < end, "installer section markers must be ordered");
    &INSTALLER[start..end]
}

#[test]
fn upgrade_failure_restores_old_files_only_before_the_migration_boundary() {
    let stop = installer_section("stop_runtime() {", "\ncleanup_staging() {");
    let cleanup = installer_section("cleanup_staging() {", "\ntrap cleanup_staging 0");
    for socket in [false, true] {
        for (published, migration_started) in [(false, false), (true, false), (true, true)] {
            let root = tempfile::tempdir().unwrap();
            let backup = root.path().join("backup");
            fs::create_dir_all(backup.join("database")).unwrap();
            if migration_started {
                fs::write(backup.join("database/migration-started"), "started").unwrap();
            }
            let script = root.path().join("cleanup.sh");
            fs::write(&script, format!(
            "#!/bin/sh\nset -eu\nupgrade_service_stopped=true\nupgrade_success=false\nupgrade_published={published}\nupgrade_backup=$1\n\
             launcher_stage=$1/launcher\nconsole_stage=$1/console\ncommand_stage=$1/command\nlisten_address_stage=$1/listen\n\
             public_url_stage=$1/public\ndatabase_type_stage=$1/type\ndatabase_url_stage=$1/url\nservice_stage=$1/service\nready_error=$1/ready\n\
             socket_unit=$1/socket\nsocket_dropin=$1/dropin\nsocket_stage=$1/socket-stage\nsocket_dropin_stage=$1/dropin-stage\n\
             socket_existed={socket}\nsocket_dropin_existed={socket}\nsocket_was_active={socket}\n\
             touch \"$socket_unit\" \"$socket_dropin\"\n\
             systemctl() {{ printf 'systemctl %s\\n' \"$*\"; }}\n\
             tar() {{ printf 'restore installation\\n'; }}\n\
             say() {{ printf '%s\\n' \"$*\"; }}\n{stop}\n{cleanup}\ncleanup_staging\n"
        )).unwrap();
            let output = Command::new("sh")
                .arg(script)
                .arg(&backup)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let output = String::from_utf8(output.stdout).unwrap();
            assert_eq!(
                output.contains("restore installation"),
                published && !migration_started
            );
            assert_eq!(
                output.contains("systemctl start brocade.service"),
                !migration_started
            );
            assert_eq!(
                output.contains("systemctl reset-failed brocade.service"),
                !migration_started
            );
            if !migration_started {
                let reset = output
                    .find("systemctl reset-failed brocade.service")
                    .unwrap();
                let start = output.find("systemctl start brocade.service").unwrap();
                assert!(
                    reset < start,
                    "recovery must clear the failed candidate's start limit"
                );
            }
            if migration_started {
                assert!(output.contains("systemctl stop brocade.service"));
                assert!(
                    output.find("systemctl stop brocade.socket").unwrap()
                        < output.find("systemctl stop brocade.service").unwrap()
                );
            } else if socket {
                assert!(
                    output.find("systemctl start brocade.socket").unwrap()
                        < output.find("systemctl start brocade.service").unwrap()
                );
            }
            assert_eq!(
                backup.join("socket").exists(),
                !published || migration_started || socket
            );
            assert_eq!(
                backup.join("dropin").exists(),
                !published || migration_started || socket
            );
        }
    }
}

#[test]
fn upgrade_retains_saved_entry_instead_of_running_new_install_detection() {
    let selection = installer_section(
        "if [ \"$upgrade\" = true ]; then\n    public_url=$saved_public_url",
        "\nrun_upgrade_database() {",
    );
    let output = Command::new("sh").args(["-c", &format!(
        "set -eu\nupgrade=true\nsaved_public_url=http://192.0.2.20:9001\nsaved_listen_address=0.0.0.0:9001\n{selection}\nprintf '%s\\n' \"$public_url\" \"$listen_address\""
    )]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "http://192.0.2.20:9001\n0.0.0.0:9001\n"
    );
}

#[test]
fn installer_accepts_static_musl_on_gnu_without_accepting_other_architectures() {
    let check = installer_section("case $(uname -m) in\n", "\nhas_ca_bundle() {");
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let script = root.path().join("check-platform.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nset -eu\nscript_dir=$1\n\
             die() {{ printf '%s\\n' \"$*\" >&2; exit 1; }}\n{check}\n"
        ),
    )
    .unwrap();
    for (machine_arch, machine_libc, bundle, accepted) in [
        ("x86_64", "gnu", "x86_64-musl", true),
        ("aarch64", "gnu", "aarch64-musl", true),
        ("x86_64", "gnu", "x86_64-gnu", true),
        ("x86_64", "musl", "x86_64-musl", true),
        ("x86_64", "musl", "x86_64-gnu", false),
        ("x86_64", "gnu", "aarch64-musl", false),
        ("aarch64", "gnu", "x86_64-gnu", false),
    ] {
        executable(
            &bin.join("uname"),
            &format!("#!/bin/sh\nprintf '{machine_arch}\\n'\n"),
        );
        let ldd = if machine_libc == "gnu" {
            "#!/bin/sh\nprintf 'ldd (GNU libc) 2.35\\n'\n"
        } else {
            "#!/bin/sh\nprintf 'musl libc Version 1.2.5\\n' >&2\nexit 1\n"
        };
        executable(&bin.join("ldd"), ldd);
        fs::write(root.path().join("BROCADE_PLATFORM"), format!("{bundle}\n")).unwrap();
        let output = Command::new("sh")
            .arg(&script)
            .arg(root.path())
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            accepted,
            "{machine_arch}-{machine_libc} {bundle}: {output:?}"
        );
    }
}

#[test]
fn launcher_probe_preserves_loader_errors_and_suppresses_success_output() {
    let check = installer_section("run_launcher --version", "\nresolve_origin() {");
    let root = tempfile::tempdir().unwrap();
    let launcher = root.path().join("launcher");
    let script = root.path().join("probe.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nset -eu\n\
             die() {{ printf '%s\\n' \"$*\" >&2; exit 1; }}\n\
             run_launcher() {{ \"$BROCADE_TEST_LAUNCHER\" \"$@\"; }}\n{check}\n"
        ),
    )
    .unwrap();
    for error in ["GLIBC_2.39 not found", "Permission denied"] {
        executable(
            &launcher,
            &format!("#!/bin/sh\nprintf '%s\\n' '{error}' >&2\nexit 127\n"),
        );
        let output = Command::new("sh")
            .arg(&script)
            .env("BROCADE_TEST_LAUNCHER", &launcher)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(error));
        assert!(stderr.contains("launcher 无法在当前系统执行"));
    }
    executable(&launcher, "#!/bin/sh\nprintf 'brocade 0.3.1\\n'\n");
    let output = Command::new("sh")
        .arg(&script)
        .env("BROCADE_TEST_LAUNCHER", &launcher)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

fn run_with_terminal_input(command: &mut Command, input: &str) -> Output {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: Both output pointers are valid and all optional openpty inputs are null.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
    // SAFETY: openpty succeeded, returning distinct owned file descriptors.
    let (mut master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
    let mut child = command
        .stdin(slave)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    master.write_all(input.as_bytes()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("installer terminal interaction timed out: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn interactive_port_selection_accepts_default_custom_and_explicit_ports() {
    // Execute the real argument parser and address selection against the real launcher, while
    // excluding privileged package/account/file installation from this terminal interaction test.
    let arguments = installer_section("public_ip=\n", "\n[ \"$(id -u)\" -eq 0 ]");
    let validation = installer_section("valid_port() {\n", "\nvalid_database_url() {");
    let selection = installer_section(
        "resolve_origin() {\n",
        "\n# The public command supplies system paths",
    );
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("select-port.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nset -eu\n\
             die() {{ printf '%s\\n' \"$*\" >&2; exit 1; }}\n\
             say() {{ printf '%s\\n' \"$*\"; }}\n\
             run_launcher() {{ \"$BROCADE_TEST_LAUNCHER\" \"$@\"; }}\n\
             {arguments}\n{validation}\n{selection}\n\
             printf '\\nRESULT=%s|%s\\n' \"$listen_address\" \"$public_url\"\n"
        ),
    )
    .unwrap();
    for (port_argument, input, expected_port) in [
        (None, "\n", "8080"),
        (None, "9000\n", "9000"),
        (None, "0\n65536\ninvalid\n\n", "8080"),
        (Some("9001"), "", "9001"),
    ] {
        let mut command = Command::new("sh");
        command
            .arg(&script)
            .args(["--public-ip", "192.0.2.10"])
            .env(
                "BROCADE_TEST_LAUNCHER",
                env!("CARGO_BIN_EXE_brocade-launcher"),
            )
            .env_remove("BROCADE_ADMIN_BIND")
            .env_remove("BROCADE_AGENT_PUBLIC_URL");
        if let Some(port) = port_argument {
            command.args(["--port", port]);
        }
        let output = run_with_terminal_input(&mut command, input);
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            stdout.contains("HTTP 端口 [8080]："),
            port_argument.is_none()
        );
        assert!(stdout.contains(&format!(
            "RESULT=0.0.0.0:{expected_port}|http://192.0.2.10:{expected_port}"
        )));
        if input.starts_with('0') {
            assert_eq!(
                stdout.matches("端口必须是 1 至 65535 之间的整数。").count(),
                3
            );
        }
    }
}

#[test]
fn explicit_port_overrides_installed_listener_and_origin() {
    let output = Command::new(env!("CARGO_BIN_EXE_brocade-launcher"))
        .args(["address", "--port", "9000"])
        .env("BROCADE_ADMIN_BIND", "0.0.0.0:8008")
        .env("BROCADE_AGENT_PUBLIC_URL", "http://192.0.2.10:8008")
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "http://192.0.2.10:9000"
    );
}

#[test]
fn installed_command_keeps_ip_port_and_database_selection_across_restarts() {
    let marker = "cat >\"$command_stage\" <<'EOF'\n";
    let start = INSTALLER
        .find(marker)
        .expect("installer writes the public command")
        + marker.len();
    let end = INSTALLER[start..]
        .find("\nEOF\n")
        .expect("public command heredoc ends")
        + start;
    assert!(start < end);
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let config = root.path().join("config");
    fs::create_dir(&config).unwrap();
    fs::write(config.join("agent-public-url"), "http://192.0.2.10:9000\n").unwrap();
    fs::write(config.join("listen-address"), "0.0.0.0:9000\n").unwrap();
    fs::write(config.join("database-type"), "managed\n").unwrap();
    let runtime = bin.join("runtime");
    executable(
        &runtime,
        "#!/bin/sh\nprintf '%s\\n' \"$BROCADE_AGENT_PUBLIC_URL\" \"$BROCADE_ADMIN_BIND\" \"$*\"\n",
    );
    // Exercise the installed wrapper as its service account without requiring root on the test host.
    executable(&bin.join("id"), "#!/bin/sh\nprintf '1000\\n'\n");
    let script = INSTALLER[start..end]
        .replace(
            "runtime=/opt/brocade/brocade-launcher",
            &format!("runtime='{}'", runtime.display()),
        )
        .replace("/etc/brocade/", &format!("{}/", config.display()));
    let wrapper = bin.join("brocade");
    executable(&wrapper, &script);
    for kind in ["managed", "managed-socket-v1"] {
        fs::write(config.join("database-type"), format!("{kind}\n")).unwrap();
        for (command, args) in [("up", "up --managed-db"), ("url", "url")] {
            let output = Command::new("sh")
                .arg(&wrapper)
                .arg(command)
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env_remove("BROCADE_AGENT_PUBLIC_URL")
                .env_remove("BROCADE_ADMIN_BIND")
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output);
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("http://192.0.2.10:9000\n0.0.0.0:9000\n{args}\n")
            );
        }
    }
}
