use std::{
    fs,
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

use brocade_core::model::{
    ExternalOutbound, ExternalOutboundProtocol, ExternalOutboundSecurity, ExternalVlessTransport,
};
use brocade_deployment::protocol::{E2eProbeSecurity, E2eProbeStatus, E2eProbeTarget, E2eProbeTls};
use brocade_probe::{
    probe_chained_external_with_options, probe_chained_with_options, ProbeCancellation,
    ProbeOptions,
};
use sha2::{Digest, Sha256};

const MEMBER_PORT: u16 = 10_001;
const TARGET_PORT: u16 = 10_002;
const MEMBER_UUID: &str = "8cbf9a54-8698-4dca-b1c4-7fc41a896149";
const TARGET_UUID: &str = "35ac62da-574c-4092-a29b-ba2f2f18656c";

struct DockerFixture {
    network: String,
    containers: Vec<String>,
    directory: PathBuf,
}

impl DockerFixture {
    fn new() -> Self {
        let suffix = format!("{}-", std::process::id());
        let directory = std::env::temp_dir().join(format!("brocade-chained-docker-{suffix}"));
        fs::create_dir(&directory).expect("create docker probe directory");
        let network = format!("brocade-chained-probe-{suffix}");
        docker_ok(["network", "create", &network]);
        Self {
            network,
            containers: Vec::new(),
            directory,
        }
    }

    fn name(&self, role: &str) -> String {
        format!("{}-{role}", self.network)
    }

    fn remember(&mut self, name: String) -> String {
        self.containers.push(name.clone());
        name
    }
}

impl Drop for DockerFixture {
    fn drop(&mut self) {
        for name in self.containers.iter().rev() {
            let _ = docker(["rm", "-f", name]);
        }
        let _ = docker(["network", "rm", &self.network]);
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
#[ignore = "requires BROCADE_RUN_DOCKER_PROBE_TESTS=1, Docker, OpenSSL and .tools/xray"]
fn chained_probe_crosses_two_real_xray_servers_and_obeys_member_routing() {
    if std::env::var("BROCADE_RUN_DOCKER_PROBE_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping without BROCADE_RUN_DOCKER_PROBE_TESTS=1");
        return;
    }
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let xray = workspace.join(".tools/xray");
    assert!(
        xray.is_file(),
        "build or install the pinned Xray at {}",
        xray.display()
    );
    docker_ok(["version", "--format", "{{.Server.Version}}"]);

    let mut fixture = DockerFixture::new();
    let echo = fixture.name("echo");
    docker_ok([
        "run",
        "-d",
        "--name",
        &echo,
        "--network",
        &fixture.network,
        "python:3.12-alpine",
        "python3",
        "-m",
        "http.server",
        "80",
    ]);
    fixture.remember(echo.clone());

    let cert = fixture.directory.join("target.pem");
    let key = fixture.directory.join("target-key.pem");
    openssl_certificate(&cert, &key);
    let der = fixture.directory.join("target.der");
    command_ok(
        Command::new("openssl")
            .args(["x509", "-in"])
            .arg(&cert)
            .args(["-outform", "DER", "-out"])
            .arg(&der),
        "export target certificate",
    );
    let pin = Sha256::digest(fs::read(&der).expect("read certificate DER"))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    let target_name = fixture.name("target");
    let target_config = fixture.directory.join("target.json");
    fs::write(
        &target_config,
        serde_json::to_vec_pretty(&serde_json::json!({
            "log": { "loglevel": "info" },
            "inbounds": [{
                "tag": "target-in",
                "listen": "0.0.0.0",
                "port": TARGET_PORT,
                "protocol": "vless",
                "settings": {
                    "clients": [{ "id": TARGET_UUID, "email": "target@test" }],
                    "decryption": "none"
                },
                "streamSettings": {
                    "network": "tcp",
                    "security": "tls",
                    "tlsSettings": {
                        "certificates": [{
                            "certificateFile": "/run/target.pem",
                            "keyFile": "/run/target-key.pem"
                        }]
                    }
                },
                "sniffing": {
                    "enabled": true,
                    "destOverride": ["http", "tls", "quic"],
                    "routeOnly": true
                }
            }],
            "outbounds": [{
                "tag": "direct",
                "protocol": "freedom",
                "settings": { "ipsBlocked": [] }
            }]
        }))
        .expect("serialize target config"),
    )
    .expect("write target config");
    run_xray_container(
        &mut fixture,
        &target_name,
        &xray,
        &target_config,
        &[(&cert, "/run/target.pem"), (&key, "/run/target-key.pem")],
        None,
    );

    let host_port = free_port();
    let member_name = fixture.name("member");
    let member_config = fixture.directory.join("member.json");
    write_member_config(&member_config, false);
    run_xray_container(
        &mut fixture,
        &member_name,
        &xray,
        &member_config,
        &[],
        Some(host_port),
    );
    wait_for_tcp(host_port);

    let member = E2eProbeTarget {
        app_id: Some("app".to_owned()),
        chain_id: "member-chain".to_owned(),
        chain_name: "Member chain".to_owned(),
        ingress_id: "member".to_owned(),
        dial_host: Ipv4Addr::LOCALHOST.to_string(),
        port: host_port,
        uuid: MEMBER_UUID.to_owned(),
        security: E2eProbeSecurity::VlessEncryption {
            encryption: "none".to_owned(),
        },
        xhttp: None,
        expected_exit_ips: Vec::new(),
    };
    let target = E2eProbeTarget {
        app_id: Some("app".to_owned()),
        chain_id: "target-chain".to_owned(),
        chain_name: "Target chain".to_owned(),
        ingress_id: "target".to_owned(),
        // This name exists only in Docker DNS. A host process cannot reach the target directly;
        // it can be resolved only after the member Xray receives the nested request.
        dial_host: target_name.clone(),
        port: TARGET_PORT,
        uuid: TARGET_UUID.to_owned(),
        security: E2eProbeSecurity::Tls(E2eProbeTls {
            server_name: "target.invalid".to_owned(),
            pinned_peer_cert_sha256: Some(pin),
            flow: None,
        }),
        xhttp: None,
        expected_exit_ips: Vec::new(),
    };
    let options = ProbeOptions::new(&xray, ProbeCancellation::default())
        .with_runtime_dir(&fixture.directory)
        .without_warm_up();
    let endpoint = format!("http://{echo}/");
    let passed = probe_chained_with_options(&member, &target, &endpoint, 4, &options);
    assert_eq!(
        passed.status,
        E2eProbeStatus::Ok,
        "real two-server path failed: {:?}",
        passed.detail
    );

    let external_member = ExternalOutbound {
        id: "custom-1111-1111".to_owned(),
        tenant: "platform.acme".to_owned(),
        name: "External member".to_owned(),
        address: Ipv4Addr::LOCALHOST.to_string(),
        port: host_port,
        protocol: ExternalOutboundProtocol::Vless {
            credential: MEMBER_UUID.to_owned(),
            encryption: "none".to_owned(),
            flow: None,
            transport: ExternalVlessTransport::Raw,
        },
        security: ExternalOutboundSecurity::None,
        bindings: Vec::new(),
    };
    let external_passed =
        probe_chained_external_with_options(&external_member, &target, &endpoint, 4, &options);
    assert_eq!(
        external_passed.status,
        E2eProbeStatus::Ok,
        "real external-member path failed: {:?}",
        external_passed.detail
    );

    docker_ok(["rm", "-f", &member_name]);
    fixture.containers.retain(|name| name != &member_name);
    write_member_config(&member_config, true);
    run_xray_container(
        &mut fixture,
        &member_name,
        &xray,
        &member_config,
        &[],
        Some(host_port),
    );
    wait_for_tcp(host_port);
    let blocked = probe_chained_with_options(&member, &target, &endpoint, 2, &options);
    assert_ne!(
        blocked.status,
        E2eProbeStatus::Ok,
        "member route blocked target.invalid but the nested path still passed"
    );
}

fn write_member_config(path: &Path, block_target: bool) {
    let route = block_target.then(|| {
        serde_json::json!({
            "domainStrategy": "AsIs",
            "rules": [{
                "type": "field",
                "domain": ["full:target.invalid"],
                "outboundTag": "block"
            }]
        })
    });
    fs::write(
        path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "log": { "loglevel": "info" },
            "inbounds": [{
                "tag": "member-in",
                "listen": "0.0.0.0",
                "port": MEMBER_PORT,
                "protocol": "vless",
                "settings": {
                    "clients": [{ "id": MEMBER_UUID, "email": "member@test" }],
                    "decryption": "none"
                },
                "streamSettings": { "network": "tcp", "security": "none" },
                "sniffing": {
                    "enabled": true,
                    "destOverride": ["http", "tls", "quic"],
                    "routeOnly": true
                }
            }],
            "outbounds": [
                { "tag": "direct", "protocol": "freedom", "settings": { "ipsBlocked": [] } },
                { "tag": "block", "protocol": "blackhole" }
            ],
            "routing": route
        }))
        .expect("serialize member config"),
    )
    .expect("write member config");
}

fn run_xray_container(
    fixture: &mut DockerFixture,
    name: &str,
    xray: &Path,
    config: &Path,
    mounts: &[(&Path, &str)],
    published_port: Option<u16>,
) {
    let mut command = Command::new("docker");
    command.args(["run", "-d", "--name", name, "--network", &fixture.network]);
    if let Some(port) = published_port {
        command.args(["-p", &format!("127.0.0.1:{port}:{MEMBER_PORT}")]);
    }
    command.args(["-v", &format!("{}:/usr/local/bin/xray:ro", xray.display())]);
    command.args(["-v", &format!("{}:/run/config.json:ro", config.display())]);
    for (source, destination) in mounts {
        command.args(["-v", &format!("{}:{destination}:ro", source.display())]);
    }
    command.args([
        "alpine:3.22",
        "/usr/local/bin/xray",
        "run",
        "-config",
        "/run/config.json",
    ]);
    command_ok(&mut command, &format!("start {name}"));
    fixture.remember(name.to_owned());
    let inspect = docker_ok(["inspect", "--format", "{{.State.Running}}", name]);
    assert_eq!(String::from_utf8_lossy(&inspect.stdout).trim(), "true");
}

fn openssl_certificate(cert: &Path, key: &Path) {
    command_ok(
        Command::new("openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
            ])
            .args([
                "-subj",
                "/CN=target.invalid",
                "-addext",
                "subjectAltName=DNS:target.invalid",
            ])
            .arg("-keyout")
            .arg(key)
            .arg("-out")
            .arg(cert),
        "create target certificate",
    );
}

fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind temporary port")
        .local_addr()
        .expect("read temporary port")
        .port()
}

fn wait_for_tcp(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("member Xray did not listen on {port}");
}

fn docker<const N: usize>(args: [&str; N]) -> std::io::Result<Output> {
    Command::new("docker").args(args).output()
}

fn docker_ok<const N: usize>(args: [&str; N]) -> Output {
    let shown = args.join(" ");
    let output = docker(args).unwrap_or_else(|error| panic!("run docker {shown}: {error}"));
    assert!(
        output.status.success(),
        "docker {shown} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn command_ok(command: &mut Command, description: &str) -> Output {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{description}: {error}"));
    assert!(
        output.status.success(),
        "{description} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
