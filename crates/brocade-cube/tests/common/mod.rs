#![allow(dead_code)]
use brocade_cube::Backend;
use brocade_deployment::cvm::*;
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tonic::{Request, Response, Status};

#[allow(clippy::all, dead_code)]
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/generated.rs"));
}
use proto::containerd::services::tasks::v1 as tasks;
pub use proto::cubelet::services::cubebox::v1 as cb;
use proto::cubelet::services::errorcode::v1::Ret;

pub fn message<T: Message + Default>(name: &str, value: Value) -> T {
    let pool =
        DescriptorPool::decode(include_bytes!(concat!(env!("OUT_DIR"), "/cube.bin")).as_ref())
            .unwrap();
    DynamicMessage::deserialize(pool.get_message_by_name(name).unwrap(), value)
        .unwrap()
        .transcode_to()
        .unwrap()
}

pub const METRICS: &str = "boot aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa\nuptime 42.5\ncpu 10 2 3 40 5 6 7 8\nmem_total 131072\nmem_available 65536\nroot 1048576 524288\nnet eth0 2 100 200\nnet_detail_eth0 1 2 0 0 0 0 1\nload 0.10 0.20 0.30\nprocesses 1 12\n";

#[derive(Default)]
pub struct State {
    pub storage_backend: Option<String>,
    pub boxes: Vec<Value>,
    pub creates: usize,
    pub root_deletes: usize,
    pub root_exists: bool,
    pub root_name: String,
    pub capacity: u64,
    pub cpu: u64,
    pub memory: u64,
    pub fail_create_reply: bool,
    pub block_create: bool,
    pub create_entered: bool,
    pub suppress_delete: bool,
    pub suppress_detach: bool,
    pub network_delay: bool,
    pub network_active: bool,
    pub network_overlap: bool,
    pub network_actions: Vec<String>,
    pub vcpu_request: Vec<Value>,
}

#[derive(Clone)]
struct Cube {
    state: Arc<Mutex<State>>,
}

#[tonic::async_trait]
impl cb::cubebox_mgr_server::CubeboxMgr for Cube {
    async fn list(
        &self,
        request: Request<cb::ListCubeSandboxRequest>,
    ) -> Result<Response<cb::ListCubeSandboxResponse>, Status> {
        assert_eq!(
            request.metadata().get("containerd-namespace").unwrap(),
            "default"
        );
        let request = request.into_inner();
        let state = self.state.lock().unwrap();
        let items = state
            .boxes
            .iter()
            .filter(|item| request.id.as_ref().is_none_or(|id| item["id"] == *id))
            .map(|item| message("cubelet.services.cubebox.v1.CubeSandbox", item.clone()))
            .collect();
        Ok(Response::new(cb::ListCubeSandboxResponse { items }))
    }
    async fn update(
        &self,
        request: Request<cb::UpdateCubeSandboxRequest>,
    ) -> Result<Response<cb::UpdateCubeSandboxResponse>, Status> {
        let request = request.into_inner();
        let mutation = request
            .annotations
            .get("cube.network.attachments.action")
            .filter(|action| matches!(action.as_str(), "prepare" | "reconcile" | "quota"));
        if let Some(action) = mutation {
            let delay = {
                let mut state = self.state.lock().unwrap();
                state.network_actions.push(action.clone());
                if state.network_active {
                    state.network_overlap = true;
                    return Ok(Response::new(cb::UpdateCubeSandboxResponse {
                        ret: Some(Ret { ret_code: 130568,
                            ret_msg: "Cube network reconciliation is busy; retry the same desired version".into() }),
                        ..Default::default()
                    }));
                }
                state.network_active = true;
                state.network_delay
            };
            if delay {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            self.state.lock().unwrap().network_active = false;
        }
        let mut ext_info = HashMap::new();
        let action = request
            .annotations
            .get("brocade.cvm.action")
            .map(String::as_str);
        if action == Some("storage-contract-v1") {
            ext_info.insert(
                "brocade_storage_contract".into(),
                b"protected-root-v1".to_vec(),
            );
            if let Some(backend) = &self.state.lock().unwrap().storage_backend {
                ext_info.insert(
                    "brocade_storage_backend".into(),
                    backend.as_bytes().to_vec(),
                );
            }
        } else if let Some(action) = request.annotations.get("cube.network.attachments.action") {
            let payload: Value = request
                .annotations
                .get("cube.network.attachments.request")
                .map(|s| serde_json::from_str(s).unwrap())
                .unwrap_or(json!({}));
            let mut evidence = json!({"contract":"cubevs-attachments-v2","interfaces":[],"network":null,"detached_interfaces":[],"network_absent":false});
            match action.as_str() {
                "inventory" => {
                    evidence = json!({"contract":"cubevs-attachments-v2","max_interfaces":3,"sources":{},"managed_nics":true})
                }
                "prepare" => evidence["shim_version"] = json!("test-shim"),
                "quota" => evidence["reports"] = json!([]),
                "reconcile" => {
                    if payload["release"] == true {
                        evidence["network_absent"] = json!(true);
                        if !self.state.lock().unwrap().suppress_detach {
                            evidence["detached_interfaces"] = payload["detach"].clone();
                        }
                    } else {
                        let nics=payload["interfaces"].as_array().unwrap().iter().map(|nic|json!({"id":nic["id"],"device":"eth0","mac":"02:00:00:00:00:01","addresses":["192.0.2.10/32"],"attached":true})).collect::<Vec<_>>();
                        evidence["interfaces"] = json!(nics);
                        evidence["network"] = json!({"contact_route_preserved":true,"business_fallback_blocked":true,"default_ipv4_interface":null,"default_ipv6_interface":null});
                        let state = self.state.lock().unwrap();
                        evidence["actual_vcpus"] = json!(state.cpu);
                        evidence["actual_memory_bytes"] = json!(state.memory);
                    }
                }
                _ => return Err(Status::invalid_argument("unknown network test action")),
            }
            ext_info.insert(
                "cube.network.attachments.result".into(),
                serde_json::to_vec(&evidence).unwrap(),
            );
        }
        Ok(Response::new(cb::UpdateCubeSandboxResponse {
            ret: Some(Ret {
                ret_code: 200,
                ..Default::default()
            }),
            ext_info,
            ..Default::default()
        }))
    }
    async fn create(
        &self,
        request: Request<cb::RunCubeSandboxRequest>,
    ) -> Result<Response<cb::RunCubeSandboxResponse>, Status> {
        self.state.lock().unwrap().create_entered = true;
        loop {
            if !self.state.lock().unwrap().block_create {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let request = request.into_inner();
        let mut state = self.state.lock().unwrap();
        state.creates += 1;
        state.cpu = request.containers[0]
            .resources
            .as_ref()
            .unwrap()
            .cpu
            .trim_end_matches('m')
            .parse::<u64>()
            .unwrap()
            / 1000;
        state.memory = request.containers[0]
            .resources
            .as_ref()
            .unwrap()
            .mem
            .trim_end_matches("Mi")
            .parse::<u64>()
            .unwrap()
            * 1024
            * 1024;
        let root = request.volumes[0]
            .volume_source
            .as_ref()
            .unwrap()
            .empty_dir
            .as_ref()
            .unwrap();
        state.capacity = root.size_limit.parse().unwrap();
        state.root_name = "sb-cvm-test-root".into();
        state.root_exists = true;
        state.boxes = vec![
            json!({"id":"cvm-test","labels":request.labels,"containers":[{"id":"guest","state":1}]}),
        ];
        state
            .vcpu_request
            .push(json!({"annotations":request.annotations,"ports":request.exposed_ports}));
        if state.fail_create_reply {
            state.fail_create_reply = false;
            return Err(Status::unavailable("lost create receipt"));
        }
        Ok(Response::new(cb::RunCubeSandboxResponse {
            ret: Some(Ret {
                ret_code: 200,
                ..Default::default()
            }),
            sandbox_id: "cvm-test".into(),
            ..Default::default()
        }))
    }
    async fn destroy(
        &self,
        request: Request<cb::DestroyCubeSandboxRequest>,
    ) -> Result<Response<cb::DestroyCubeSandboxResponse>, Status> {
        let request = request.into_inner();
        let mut state = self.state.lock().unwrap();
        state.boxes.clear();
        if request
            .annotations
            .get("brocade.cvm.delete_root")
            .map(String::as_str)
            == Some("true")
        {
            state.root_deletes += 1;
            if !state.suppress_delete {
                state.root_exists = false;
            }
        }
        Ok(Response::new(cb::DestroyCubeSandboxResponse {
            ret: Some(Ret {
                ret_code: 200,
                ..Default::default()
            }),
            ..Default::default()
        }))
    }
    async fn inspect_storage_volumes(
        &self,
        _: Request<cb::InspectStorageVolumesRequest>,
    ) -> Result<Response<cb::InspectStorageVolumesResponse>, Status> {
        let state = self.state.lock().unwrap();
        let sandboxes = if state.boxes.is_empty() {
            vec![]
        } else {
            vec![message(
                "cubelet.services.cubebox.v1.SandboxStorageInfo",
                json!({"sandboxID":"cvm-test","volumes":[{"name":"root","volume_name":state.root_name,"kind":"volume"}]}),
            )]
        };
        Ok(Response::new(cb::InspectStorageVolumesResponse {
            ret: Some(Ret {
                ret_code: 200,
                ..Default::default()
            }),
            sandboxes,
            ..Default::default()
        }))
    }
    async fn list_sandbox_snapshots(
        &self,
        request: Request<cb::ListSandboxSnapshotsRequest>,
    ) -> Result<Response<cb::ListSandboxSnapshotsResponse>, Status> {
        let state = self.state.lock().unwrap();
        let objects = request
            .into_inner()
            .objects
            .into_iter()
            .map(|o| cb::CowObjectStatus {
                name: o.name,
                kind: o.kind,
                exists: state.root_exists,
                size_bytes: state.capacity,
                ..Default::default()
            })
            .collect();
        Ok(Response::new(cb::ListSandboxSnapshotsResponse {
            ret: Some(Ret {
                ret_code: 200,
                ..Default::default()
            }),
            objects,
            ..Default::default()
        }))
    }
}

struct Exec {
    done: AtomicBool,
    status: AtomicUsize,
    input: PathBuf,
    output: PathBuf,
    process: Value,
}
#[derive(Clone, Default)]
struct Tasks {
    execs: Arc<Mutex<HashMap<String, Arc<Exec>>>>,
    pub count: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
    close_io_failure: Arc<AtomicBool>,
    delete_exec_failure: Arc<AtomicBool>,
}

#[tonic::async_trait]
impl tasks::tasks_server::Tasks for Tasks {
    async fn exec(
        &self,
        request: Request<tasks::ExecProcessRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();
        assert!(!request.exec_id.is_empty());
        let spec = request.spec.unwrap();
        assert_eq!(
            spec.type_url,
            "types.containerd.io/opencontainers/runtime-spec/1/Process"
        );
        let process: Value = serde_json::from_slice(&spec.value).unwrap();
        let exec = Arc::new(Exec {
            done: AtomicBool::new(false),
            status: AtomicUsize::new(0),
            input: request.stdin.into(),
            output: request.stdout.into(),
            process,
        });
        self.execs
            .lock()
            .unwrap()
            .insert(request.exec_id, exec.clone());
        self.count.fetch_add(1, Ordering::Relaxed);
        self.live.fetch_add(1, Ordering::AcqRel);
        thread::spawn(move || simulate_exec(exec));
        Ok(Response::new(()))
    }
    async fn start(
        &self,
        _: Request<tasks::StartRequest>,
    ) -> Result<Response<tasks::StartResponse>, Status> {
        Ok(Response::new(Default::default()))
    }
    async fn wait(
        &self,
        request: Request<tasks::WaitRequest>,
    ) -> Result<Response<tasks::WaitResponse>, Status> {
        let id = request.into_inner().exec_id;
        let exec = self
            .execs
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| Status::not_found("exec"))?;
        while !exec.done.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok(Response::new(tasks::WaitResponse {
            exit_status: exec.status.load(Ordering::Acquire) as u32,
            ..Default::default()
        }))
    }
    async fn kill(&self, request: Request<tasks::KillRequest>) -> Result<Response<()>, Status> {
        let id = request.into_inner().exec_id;
        assert!(!id.is_empty());
        let exec = self
            .execs
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| Status::not_found("exec"))?;
        exec.done.store(true, Ordering::Release);
        Ok(Response::new(()))
    }
    async fn delete_process(
        &self,
        request: Request<tasks::DeleteProcessRequest>,
    ) -> Result<Response<tasks::DeleteResponse>, Status> {
        let id = request.into_inner().exec_id;
        assert!(!id.is_empty());
        if self.delete_exec_failure.load(Ordering::Acquire) {
            return Err(Status::unavailable("exec deletion not confirmed"));
        }
        if self.execs.lock().unwrap().remove(&id).is_some() {
            self.live.fetch_sub(1, Ordering::AcqRel);
        }
        Ok(Response::new(Default::default()))
    }
    async fn close_io(&self, _: Request<tasks::CloseIoRequest>) -> Result<Response<()>, Status> {
        if self.close_io_failure.load(Ordering::Acquire) {
            return Err(Status::unknown("stdin is already closed"));
        }
        Ok(Response::new(()))
    }
    async fn resize_pty(
        &self,
        _: Request<tasks::ResizePtyRequest>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented(
            "PTYs are exercised by the Shim terminal crate",
        ))
    }
}

fn simulate_exec(exec: Arc<Exec>) {
    let result = (|| -> std::io::Result<()> {
        let nonce = exec.process["args"][4].as_str().unwrap();
        let mut writer = OpenOptions::new().write(true).open(&exec.output)?;
        let mut reader = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&exec.input)?;
        writeln!(writer, "BROCADE_READY:{nonce}")?;
        writer.flush()?;
        assert_eq!(read_line(&mut reader, &exec)?, nonce);
        if exec.process["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().is_some_and(|s| s.contains("printf 'boot '")))
        {
            writer.write_all(METRICS.as_bytes())?;
        }
        writeln!(writer, "\nBROCADE_DONE:{nonce}:0")?;
        writer.flush()?;
        assert_eq!(read_line(&mut reader, &exec)?, format!("ACK:{nonce}"));
        Ok(())
    })();
    if result.is_err() {
        exec.status.store(255, Ordering::Release);
    }
    exec.done.store(true, Ordering::Release);
}

fn read_line(file: &mut File, exec: &Exec) -> std::io::Result<String> {
    let start = Instant::now();
    let mut out = vec![];
    while start.elapsed() < Duration::from_secs(5) && !exec.done.load(Ordering::Acquire) {
        let mut b = [0];
        match file.read(&mut b) {
            Ok(1) if b[0] == b'\n' => return Ok(String::from_utf8(out).unwrap()),
            Ok(1) => out.push(b[0]),
            Err(e) if e.kind() != std::io::ErrorKind::WouldBlock => return Err(e),
            _ => thread::sleep(Duration::from_millis(1)),
        }
    }
    Err(std::io::Error::other("test Guest handshake timed out"))
}

pub struct Fixture {
    pub backend: Option<Backend>,
    pub state: Arc<Mutex<State>>,
    pub directory: tempfile::TempDir,
    pub config: PathBuf,
    pub records: PathBuf,
    pub guest_calls: Arc<AtomicUsize>,
    pub live_execs: Arc<AtomicUsize>,
    pub close_io_failure: Arc<AtomicBool>,
    pub delete_exec_failure: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    server: Option<thread::JoinHandle<()>>,
    inspector: Option<thread::JoinHandle<()>>,
}

impl Fixture {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let socket = directory.path().join("cubelet.sock");
        let state = Arc::new(Mutex::new(State {
            storage_backend: Some("plain".into()),
            ..State::default()
        }));
        let tasks = Tasks::default();
        let guest_calls = tasks.count.clone();
        let live_execs = tasks.live.clone();
        let close_io_failure = tasks.close_io_failure.clone();
        let delete_exec_failure = tasks.delete_exec_failure.clone();
        let cube = Cube {
            state: state.clone(),
        };
        let (shutdown, stop_rx) = tokio::sync::oneshot::channel();
        let (bound_rx, bound_tx) = std::sync::mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::UnixListener::bind(&socket).unwrap();
                fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
                bound_rx.send(()).unwrap();
                tonic::transport::Server::builder()
                    .add_service(cb::cubebox_mgr_server::CubeboxMgrServer::new(cube))
                    .add_service(tasks::tasks_server::TasksServer::new(tasks))
                    .serve_with_incoming_shutdown(
                        tokio_stream::wrappers::UnixListenerStream::new(listener),
                        async {
                            let _ = stop_rx.await;
                        },
                    )
                    .await
                    .unwrap();
            });
        });
        bound_tx.recv().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let observed = state.clone();
        let inspector = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut headers = BufReader::new(stream.try_clone().unwrap());
                        let mut line = String::new();
                        loop {
                            line.clear();
                            if headers.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                                break;
                            }
                        }
                        let present = !observed.lock().unwrap().boxes.is_empty();
                        let bytes=serde_json::to_vec(&json!({"taps":if present{json!([{"sandboxID":"cvm-test","sandboxIP":"192.0.2.10"}])}else{json!([])}})).unwrap();
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        )
                        .unwrap();
                        stream.write_all(&bytes).unwrap();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        let image = json!({"volumes":[{"name":"root","volume_source":{"empty_dir":{}}}],"containers":[{"volume_mounts":[{"name":"root","container_path":"/"}]}]});
        let image_bytes = serde_json::to_vec(&image).unwrap();
        let image_path = directory.path().join("system.json");
        fs::write(&image_path, &image_bytes).unwrap();
        fs::set_permissions(&image_path, fs::Permissions::from_mode(0o600)).unwrap();
        let config = directory.path().join("config.json");
        let capabilities = capabilities(brocade_core::hash::sha256_hex(&image_bytes));
        fs::write(&config,serde_json::to_vec(&json!({"cubelet_socket":directory.path().join("cubelet.sock"),"network_inspection_url":format!("http://{address}/v1/network/taps"),"storage_backend":"plain","capabilities":capabilities,"image_files":{"debian":image_path}})).unwrap()).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let records = directory.path().join("records");
        let backend = Some(Backend::open(&config, &records).unwrap());
        Self {
            backend,
            state,
            directory,
            config,
            records,
            guest_calls,
            live_execs,
            close_io_failure,
            delete_exec_failure,
            stop,
            shutdown: Some(shutdown),
            server: Some(server),
            inspector: Some(inspector),
        }
    }
    pub fn backend(&self) -> &Backend {
        self.backend.as_ref().unwrap()
    }
    pub fn desired(&self) -> InstanceDesired {
        let config: Value = serde_json::from_slice(&fs::read(&self.config).unwrap()).unwrap();
        let spec = InstanceSpec {
            routing: ContainerRoutes::default(),
            name: "Test".into(),
            image: serde_json::from_value(config["capabilities"]["images"][0].clone()).unwrap(),
            resources: Resources {
                vcpus: 1,
                memory_bytes: 128 * 1024 * 1024,
                root_disk_bytes: 3 * 1024 * 1024 * 1024,
            },
            ssh_public_key: public_key(),
            interfaces: vec![InterfaceSpec {
                allocated: None,
                id: "nic-test".into(),
                connection: InterfaceRequest {
                    source_id: "contact".into(),
                    purpose: InterfacePurpose::Contact,
                    default_ipv4: false,
                    default_ipv6: false,
                },
            }],
        };
        InstanceDesired {
            detached_interfaces: Vec::new(),
            id: "cvm-test".into(),
            operation_id: "op-1".into(),
            generation: 1,
            spec_sha256: brocade_core::hash::sha256_hex(&serde_json::to_vec(&spec).unwrap()),
            spec,
            power: PowerState::Running,
            delete_data: false,
            restart_epoch: 0,
            reinstall_epoch: 0,
        }
    }
    pub fn report(&self, generation: u64) -> Value {
        // The receipt file becomes visible before the executor publishes its record.
        // Retrying and observing require the same completed-attempt boundary.
        self.backend()
            .wait_idle(Duration::from_secs(10))
            .expect("fixture execution must settle before reading its receipt");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(bytes) = fs::read(self.records.join("cvm-test.json")) {
                let record: Value = serde_json::from_slice(&bytes).unwrap();
                if record["generation"] == generation && !record["report"].is_null() {
                    return record["report"].clone();
                }
            }
            assert!(
                Instant::now() < deadline,
                "execution receipt missing; state={:?}",
                self.state.lock().unwrap().creates
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.backend.take();
        self.state.lock().unwrap().block_create = false;
        self.stop.store(true, Ordering::Release);
        if let Some(stop) = self.shutdown.take() {
            let _ = stop.send(());
        }
        if let Some(server) = self.server.take() {
            server.join().unwrap();
        }
        if let Some(inspector) = self.inspector.take() {
            inspector.join().unwrap();
        }
    }
}

pub fn public_key() -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let mut key = vec![];
    key.extend_from_slice(&11u32.to_be_bytes());
    key.extend_from_slice(b"ssh-ed25519");
    key.extend_from_slice(&32u32.to_be_bytes());
    key.extend_from_slice(&[7; 32]);
    format!("ssh-ed25519 {}", STANDARD.encode(key))
}

fn capabilities(digest: String) -> Value {
    json!({
        "create": true, "start": true, "stop": true, "delete": true,
        "stable_storage": true, "retain_on_delete": false,
        "restart": true, "reinstall": true, "configure": true,
        "hot_attach": true, "hot_detach": true, "max_interfaces": 3,
        "allocatable_vcpus": 8,
        "allocatable_memory_bytes": 8u64 * 1024 * 1024 * 1024,
        "allocatable_disk_bytes": 64u64 * 1024 * 1024 * 1024,
        "images": [{"id":"debian", "name":"Debian", "version":"13",
            "architecture":std::env::consts::ARCH, "sha256":digest, "boot_mode":"cube-oci"}],
        "resource_limits": {
            "min_vcpus":1, "max_vcpus":8,
            "min_memory_bytes":128u64 * 1024 * 1024,
            "max_memory_bytes":4u64 * 1024 * 1024 * 1024,
            "memory_step_bytes":64u64 * 1024 * 1024,
            "min_root_disk_bytes":3u64 * 1024 * 1024 * 1024,
            "max_root_disk_bytes":32u64 * 1024 * 1024 * 1024,
            "root_disk_step_bytes":1024u64 * 1024 * 1024
        },
        "network_sources":[{"id":"contact", "name":"Primary", "kind":"public", "description":""}]
    })
}
