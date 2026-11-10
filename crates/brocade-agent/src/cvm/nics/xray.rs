//! Authenticated IP framing over a private Xray reverse stream; no application proxy conversion.
use super::*;
use ring::hmac;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io,
    os::fd::AsRawFd,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::{
    io::{unix::AsyncFd, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const TLS_NAME: &str = "cvm-nic.brocade.internal";
const MAGIC: &[u8] = b"BCNIC\0\0\x01";
const DATA_PORT: u16 = 47172;
const DIAL_PORT: u16 = 47171;
#[derive(Serialize, Deserialize)]
struct Identity {
    certificate: String,
    key: String,
}
#[derive(Deserialize)]
struct Peer {
    packet_key: String,
    certificate_pem: String,
}
fn identity(root: &Path) -> Result<Identity> {
    let path = root.join("cvm-nic-identity.json");
    if let Some(bytes) = read_bounded(&path, 32768)? {
        return serde_json::from_slice(&bytes)
            .map_err(|_| "invalid private NIC transport identity".into());
    }
    let generated = rcgen::generate_simple_self_signed(vec![TLS_NAME.into()])
        .map_err(|_| "cannot generate NIC transport identity")?;
    let value = Identity {
        certificate: generated.cert.pem(),
        key: generated.signing_key.serialize_pem(),
    };
    atomic_write_private(
        &path,
        &serde_json::to_vec(&value).map_err(|_| "cannot encode NIC identity")?,
    )?;
    Ok(value)
}
pub(super) fn certificate(root: &Path) -> Result<String> {
    let executable = crate::options::xray_binary_path();
    let checked = capture_command_with_timeout(
        executable.to_str().ok_or("invalid Xray executable path")?,
        &["version"],
        Duration::from_secs(5),
    )
    .map_err(|_| "Brocade Xray is unavailable")?;
    if !checked.status.success() || !Path::new("/dev/net/tun").exists() {
        return Err("Xray IP transport requires its executable and TUN".into());
    }
    Ok(identity(root)?.certificate)
}
#[derive(Default)]
struct Status {
    prepared: bool,
    connected: bool,
    error: Option<String>,
}
struct Worker {
    fingerprint: String,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    join: Option<std::thread::JoinHandle<()>>,
    started: std::time::Instant,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
static WORKERS: OnceLock<Mutex<BTreeMap<String, Worker>>> = OnceLock::new();
fn worker_id(b: &NicBinding) -> String {
    format!(
        "{}-{}",
        b.id,
        if b.side == BindingSide::Consumer {
            "c"
        } else {
            "p"
        }
    )
}
fn fingerprint(b: &NicBinding) -> Result<String> {
    Ok(brocade_core::hash::sha256_hex(
        &serde_json::to_vec(&(
            &b.id,
            b.side,
            &b.credential,
            &b.peer_credential,
            &b.offer,
            b.port,
            b.nic.ipv4,
            b.nic.ipv6,
            b.nic.mtu,
        ))
        .map_err(|_| "invalid NIC Xray configuration")?,
    ))
}
pub(super) fn apply(root: &Path, b: &NicBinding) -> Result<LinkObservation> {
    let key = worker_id(b);
    let fingerprint = fingerprint(b)?;
    let mut workers = WORKERS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| "NIC Xray worker registry unavailable")?;
    let mut previous_error = None;
    if workers.get(&key).is_some_and(|worker| {
        worker.fingerprint != fingerprint
            || worker.join.as_ref().is_some_and(|join| join.is_finished())
    }) {
        previous_error = workers.get(&key).and_then(|worker| {
            worker
                .status
                .lock()
                .ok()
                .and_then(|state| state.error.clone())
        });
        workers.remove(&key);
    }
    if !workers.contains_key(&key) {
        if workers.len() >= NODE_BINDINGS * 4 {
            return Err("NIC Xray worker capacity exceeded".into());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(Status::default()));
        let owned_binding = b.clone();
        let owned_root = root.to_owned();
        let signal = stop.clone();
        let observed = status.clone();
        let join = std::thread::Builder::new()
            .name(format!(
                "nic-xray-{}",
                &b.id[b.id.len().saturating_sub(8)..]
            ))
            .spawn(move || {
                let outcome = enter(&owned_binding)
                    .and_then(|_| {
                        tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .map_err(|_| "cannot create NIC packet runtime".into())
                    })
                    .and_then(|runtime| {
                        runtime.block_on(worker(&owned_root, &owned_binding, &signal, &observed))
                    });
                if let Err(error) = outcome {
                    if let Ok(mut state) = observed.lock() {
                        state.prepared = false;
                        state.connected = false;
                        state.error = Some(error);
                    }
                }
            })
            .map_err(|_| "cannot start NIC Xray packet worker")?;
        workers.insert(
            key.clone(),
            Worker {
                fingerprint,
                stop,
                status,
                join: Some(join),
                started: std::time::Instant::now(),
            },
        );
    }
    let worker = workers.get(&key).ok_or("NIC Xray worker was not created")?;
    if let Some(error) = previous_error {
        return Err(error);
    }
    let state = worker
        .status
        .lock()
        .map_err(|_| "NIC Xray packet state unavailable")?;
    if let Some(error) = &state.error {
        return Err(error.clone());
    }
    if !state.prepared {
        return Ok(LinkObservation {
            reachable: Some(false),
            ..Default::default()
        });
    }
    if !state.connected && worker.started.elapsed() > Duration::from_secs(120) {
        return Err("Xray IP 承载连接尚未建立，保留原网卡与分配".into());
    }
    let (rx, tx) = host::counters(&host::namespace(b), &host::tunnel(b))?;
    Ok(LinkObservation {
        configured: true,
        reachable: Some(state.connected),
        rx,
        tx,
    })
}
pub(super) fn stop(b: &NicBinding) -> Result<()> {
    if let Some(workers) = WORKERS.get() {
        workers
            .lock()
            .map_err(|_| "NIC Xray worker registry unavailable")?
            .remove(&worker_id(b));
    }
    Ok(())
}
fn enter(b: &NicBinding) -> Result<()> {
    let file = File::open(Path::new("/run/netns").join(host::namespace(b)))
        .map_err(|_| "NIC namespace is unavailable")?;
    // SAFETY: the ownership-checked namespace descriptor stays open for setns.
    // This dedicated worker thread has no runtime before entering its namespace.
    if unsafe { libc::setns(file.as_raw_fd(), libc::CLONE_NEWNET) } < 0 {
        return Err("cannot enter NIC transport namespace".into());
    }
    Ok(())
}
fn adapter(b: &NicBinding) -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::new(127, 64, (b.port >> 8) as u8, b.port as u8)
}
fn config(b: &NicBinding, directory: &Path) -> serde_json::Value {
    use serde_json::json;
    let address = adapter(b).to_string();
    let reverse = format!("nic-reverse-{}", b.id);
    if b.side
        == b.offer
            .listen_side
            .resolved()
            .expect("validated binding listener")
    {
        let tls = json!({"minVersion":"1.3","certificates":[{"certificateFile":directory.join("certificate.pem"),"keyFile":directory.join("key.pem")}]});
        json!({"log":{"loglevel":"none"},"inbounds":[{"listen":if b.offer.endpoint.is_ipv4(){"0.0.0.0"}else{"::"},"port":b.port,"protocol":"vless","tag":"nic-control","settings":{"decryption":"none","clients":[{"id":b.credential,"reverse":{"tag":reverse}}]},"streamSettings":{"network":"tcp","sockopt":{"mark":186},"security":"tls","tlsSettings":tls}},{"listen":address,"port":DIAL_PORT,"protocol":"dokodemo-door","tag":"nic-frames","settings":{"address":address,"port":DATA_PORT,"network":"tcp"}}],"outbounds":[{"protocol":"blackhole","tag":"deny"}],"routing":{"rules":[{"type":"field","inboundTag":["nic-frames"],"outboundTag":reverse}]},"policy":{"levels":{"0":{"connIdle":120}}}})
    } else {
        json!({"log":{"loglevel":"none"},"outbounds":[{"protocol":"freedom","tag":"local-adapter"},{"protocol":"vless","tag":"nic-dial","settings":{"address":b.offer.endpoint,"port":b.port,"id":b.credential,"encryption":"none","reverse":{"tag":reverse}},"streamSettings":{"network":"tcp","sockopt":{"mark":186},"security":"tls","tlsSettings":{"serverName":TLS_NAME,"minVersion":"1.3","disableSystemRoot":true,"certificates":[{"certificateFile":directory.join("certificate.pem"),"usage":"verify"}]}}},{"protocol":"blackhole","tag":"deny"}],"routing":{"rules":[{"type":"field","inboundTag":[reverse],"ip":[format!("{address}/32")],"port":DATA_PORT.to_string(),"outboundTag":"local-adapter"},{"type":"field","inboundTag":[reverse],"outboundTag":"deny"}]},"policy":{"levels":{"0":{"connIdle":120}}}})
    }
}
fn open_tun(b: &NicBinding) -> Result<AsyncFd<File>> {
    let name = host::tunnel(b);
    #[repr(C)]
    struct Request {
        name: [u8; 16],
        flags: i16,
        padding: [u8; 22],
    }
    let mut request = Request {
        name: [0; 16],
        flags: (0x0001u16 | 0x1000 | 0x8000) as i16,
        padding: [0; 22],
    };
    request.name[..name.len()].copy_from_slice(name.as_bytes());
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open("/dev/net/tun")
        .map_err(|_| "cannot open allocated NIC TUN")?;
    // SAFETY: Request has Linux ifreq layout and is writable for the ioctl.
    if unsafe { libc::ioctl(file.as_raw_fd(), 0x400454ca as _, &mut request) } < 0 {
        return Err("cannot create owned NIC TUN; an existing device is never adopted".into());
    }
    run_command(
        "ip",
        &[
            "link",
            "set",
            "dev",
            &name,
            "alias",
            &format!("brocade-binding:{}", b.id),
            "mtu",
            &b.nic.mtu.to_string(),
            "up",
        ],
    )
    .map_err(|_| "cannot activate NIC TUN")?;
    // SAFETY: AsyncFd owns the stable nonblocking File until it is dropped.
    unsafe { AsyncFd::register(file) }.map_err(|_| "cannot register NIC packet descriptor".into())
}
struct Process(Child);
impl Process {
    fn stop(&mut self) -> Result<()> {
        if self
            .0
            .try_wait()
            .map_err(|_| "cannot observe NIC Xray process")?
            .is_some()
        {
            return Ok(());
        }
        self.0.kill().map_err(|_| "cannot stop NIC Xray process")?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if self
                .0
                .try_wait()
                .map_err(|_| "cannot reap NIC Xray process")?
                .is_some()
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err("NIC Xray cleanup is unconfirmed".into())
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
fn start(path: &Path) -> Result<Process> {
    let mut command = Command::new(crate::options::xray_binary_path());
    command
        .arg("run")
        .arg("-config")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: only async-signal-safe libc operations run between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() == 1 {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Agent exited before NIC transport startup",
                ));
            }
            Ok(())
        });
    }
    command
        .spawn()
        .map(Process)
        .map_err(|_| "cannot start private NIC Xray process".into())
}
async fn stopped(stop: &AtomicBool) {
    while !stop.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn worker(
    root: &Path,
    b: &NicBinding,
    stop: &AtomicBool,
    status: &Mutex<Status>,
) -> Result<()> {
    let peer: Peer =
        serde_json::from_str(&b.peer_credential).map_err(|_| "invalid NIC Xray peer identity")?;
    if peer.packet_key.len() != 64
        || !peer.packet_key.bytes().all(|c| c.is_ascii_hexdigit())
        || !peer.certificate_pem.contains("-----BEGIN CERTIFICATE-----")
    {
        return Err("invalid NIC packet authentication identity".into());
    }
    let directory = root.join("cvm-nics").join(worker_id(b));
    private_dir(&directory)?;
    if b.side == b.offer.listen_side.resolved()? {
        let identity = identity(root)?;
        if identity.certificate != peer.certificate_pem {
            return Err("NIC transport certificate changed; prepare a new binding".into());
        }
        atomic_write_private(&directory.join("key.pem"), identity.key.as_bytes())?;
    }
    atomic_write_private(
        &directory.join("certificate.pem"),
        peer.certificate_pem.as_bytes(),
    )?;
    let path = directory.join("xray.json");
    atomic_write_private(
        &path,
        &serde_json::to_vec(&config(b, &directory))
            .map_err(|_| "cannot encode NIC transport configuration")?,
    )?;
    let binary = crate::options::xray_binary_path();
    let checked = capture_command_with_timeout(
        binary.to_str().ok_or("invalid Xray path")?,
        &[
            "run",
            "-test",
            "-config",
            path.to_str().ok_or("invalid NIC Xray configuration path")?,
        ],
        Duration::from_secs(10),
    )
    .map_err(|_| "NIC Xray configuration validation failed")?;
    if !checked.status.success() {
        return Err("NIC Xray configuration rejected".into());
    }
    let tun = open_tun(b)?;
    host::limits(b)?;
    let listener = if b.side != b.offer.listen_side.resolved()? {
        Some(
            TcpListener::bind((adapter(b), DATA_PORT))
                .await
                .map_err(|_| "NIC packet adapter address is occupied")?,
        )
    } else {
        None
    };
    let mut process = start(&path)?;
    if let Ok(mut state) = status.lock() {
        state.prepared = true;
    }
    let key_bytes = (0..64)
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&peer.packet_key[i..i + 2], 16).map_err(|_| "invalid NIC packet key")
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let key = hmac::Key::new(hmac::HMAC_SHA256, &key_bytes);
    let mut delay = 1;
    let outcome=async{loop{
        let connect=async{if let Some(listener)=&listener{listener.accept().await.map(|(stream,_)|stream)}else{TcpStream::connect((adapter(b),DIAL_PORT)).await}};
        let connection=tokio::select!{_=stopped(stop)=>return Ok(()),connection=tokio::time::timeout(Duration::from_secs(5),connect)=>connection};
        if let Ok(Ok(mut stream))=connection{
            let session=async{stream.set_nodelay(true).map_err(|_|"NIC stream setup failed")?;tokio::time::timeout(Duration::from_secs(10),authenticate(&mut stream,b,&key)).await.map_err(|_|"NIC packet authentication timed out")??;if let Ok(mut state)=status.lock(){state.connected=true;state.error=None;}pump(&tun,stream,b).await};
            tokio::select!{_=stopped(stop)=>return Ok(()),_=session=>{}};
            if let Ok(mut state)=status.lock(){state.connected=false;}delay=1;
        }
        if process.0.try_wait().map_err(|_|"cannot observe NIC Xray process")?.is_some(){return Err("NIC Xray process exited".into());}
        tokio::select!{_=stopped(stop)=>return Ok(()),_=tokio::time::sleep(Duration::from_secs(delay))=>{}};delay=(delay*2).min(30);
    }}.await;
    outcome.and(process.stop())
}
async fn authenticate(stream: &mut TcpStream, b: &NicBinding, key: &hmac::Key) -> Result<()> {
    let mut challenge = vec![0; MAGIC.len() + 32];
    challenge[..MAGIC.len()].copy_from_slice(MAGIC);
    let consumer = b.side == BindingSide::Consumer;
    if consumer {
        getrandom::fill(&mut challenge[MAGIC.len()..])
            .map_err(|_| "NIC authentication nonce unavailable")?;
        stream
            .write_all(&challenge)
            .await
            .map_err(|_| "NIC challenge write failed")?;
    } else {
        stream
            .read_exact(&mut challenge)
            .await
            .map_err(|_| "NIC challenge read failed")?;
        if &challenge[..MAGIC.len()] != MAGIC {
            return Err("unsupported NIC packet framing".into());
        }
    }
    challenge.extend_from_slice(b.id.as_bytes());
    let mut client = challenge.clone();
    client.extend_from_slice(b"client");
    let mut server = challenge;
    server.extend_from_slice(b"server");
    let mut proof = [0; 32];
    if consumer {
        stream
            .read_exact(&mut proof)
            .await
            .map_err(|_| "NIC authentication read failed")?;
        hmac::verify(key, &client, &proof).map_err(|_| "NIC authentication rejected")?;
        stream
            .write_all(hmac::sign(key, &server).as_ref())
            .await
            .map_err(|_| "NIC authentication write failed")?;
    } else {
        stream
            .write_all(hmac::sign(key, &client).as_ref())
            .await
            .map_err(|_| "NIC authentication write failed")?;
        stream
            .read_exact(&mut proof)
            .await
            .map_err(|_| "NIC authentication read failed")?;
        hmac::verify(key, &server, &proof).map_err(|_| "NIC authentication rejected")?;
    }
    Ok(())
}
fn scoped(packet: &[u8], b: &NicBinding, send: bool) -> bool {
    let source = (b.side == BindingSide::Consumer) == send;
    match packet.first().map(|byte| byte >> 4) {
        Some(4)
            if packet.len() >= 20
                && packet[0] & 15 >= 5
                && usize::from(packet[0] & 15) * 4 <= packet.len()
                && usize::from(u16::from_be_bytes([packet[2], packet[3]])) == packet.len() =>
        {
            let offset = if source { 12 } else { 16 };
            b.nic.ipv4.is_some_and(
                |ip| matches!(ip,IpAddr::V4(ip) if packet[offset..offset+4]==ip.octets()),
            )
        }
        Some(6)
            if packet.len() >= 40
                && usize::from(u16::from_be_bytes([packet[4], packet[5]])) + 40 == packet.len() =>
        {
            let offset = if source { 8 } else { 24 };
            b.nic.ipv6.is_some_and(
                |ip| matches!(ip,IpAddr::V6(ip) if packet[offset..offset+16]==ip.octets()),
            )
        }
        _ => false,
    }
}
async fn read_packet(tun: &AsyncFd<File>, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        let mut ready = tun.readable().await?;
        match ready.try_io(|fd| {
            // SAFETY: the mutable buffer is valid for its advertised length.
            let read =
                unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            if read < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(read as usize)
            }
        }) {
            Ok(value) => return value,
            Err(_) => continue,
        }
    }
}
async fn write_packet(tun: &AsyncFd<File>, packet: &[u8]) -> io::Result<()> {
    loop {
        let mut ready = tun.writable().await?;
        match ready.try_io(|fd| {
            // SAFETY: the immutable packet is valid for its advertised length.
            let written =
                unsafe { libc::write(fd.as_raw_fd(), packet.as_ptr().cast(), packet.len()) };
            if written < 0 {
                Err(io::Error::last_os_error())
            } else if written as usize != packet.len() {
                Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "partial NIC TUN packet",
                ))
            } else {
                Ok(())
            }
        }) {
            Ok(value) => return value,
            Err(_) => continue,
        }
    }
}
async fn pump(tun: &AsyncFd<File>, stream: TcpStream, b: &NicBinding) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let send = async {
        let mut packet = vec![0; 65535];
        let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
        loop {
            let length = tokio::select! {result=read_packet(tun,&mut packet)=>{let length=result.map_err(|_|"NIC TUN read failed")?;if !scoped(&packet[..length],b,true){continue}length},_=heartbeat.tick()=>0};
            tokio::time::timeout(Duration::from_secs(5), async {
                writer.write_all(&(length as u16).to_be_bytes()).await?;
                writer.write_all(&packet[..length]).await
            })
            .await
            .map_err(|_| "NIC packet writer stalled")?
            .map_err(|_| "NIC packet channel closed")?;
        }
        #[allow(unreachable_code)]
        Ok::<(), String>(())
    };
    let receive = async {
        let mut packet = vec![0; 65535];
        loop {
            let length = tokio::time::timeout(Duration::from_secs(65), reader.read_u16())
                .await
                .map_err(|_| "NIC packet heartbeat expired")?
                .map_err(|_| "NIC packet reader closed")? as usize;
            if length == 0 {
                continue;
            }
            tokio::time::timeout(
                Duration::from_secs(5),
                reader.read_exact(&mut packet[..length]),
            )
            .await
            .map_err(|_| "NIC partial packet deadline expired")?
            .map_err(|_| "NIC partial packet channel closed")?;
            if !scoped(&packet[..length], b, false) {
                return Err("NIC packet escaped its allocated identity".into());
            }
            tokio::time::timeout(Duration::from_secs(5), write_packet(tun, &packet[..length]))
                .await
                .map_err(|_| "NIC TUN writer stalled")?
                .map_err(|_| "NIC TUN write failed")?;
        }
        #[allow(unreachable_code)]
        Ok::<(), String>(())
    };
    tokio::select! {result=send=>result,result=receive=>result}
}
