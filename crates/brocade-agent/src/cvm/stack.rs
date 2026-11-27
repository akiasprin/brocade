//! Node-local cold stack update. The executing systemd unit is separate from
//! brocade-agent.service, which the installer is allowed to stop and restart.
use crate::{
    fsutil::{atomic_write_private, read_bounded},
    http::HttpClient,
    options::{ApplyMode, Options},
};
use brocade_deployment::cvm::{stack::*, valid_digest};
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    process::{Command, Stdio},
    sync::{Mutex, MutexGuard},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ROOT: &str = "/opt/brocade/cvm";
const MARKER: &str = "/opt/brocade/cvm/stack-maintenance";
const KEY: &str = "/etc/brocade/cvm-stack-update.pub";
const MANAGER: &str = "/usr/local/lib/brocade/cvm-stack.py";
const ARCHIVE: &str = "/opt/brocade/cvm/stack-candidate.tar.gz";
const AGENT: &str = "/usr/local/bin/brocade-agent";
const CONFIG: &str = "/opt/brocade/cvm/current/generated/cube.json";
pub(super) const EXECUTOR_STATE: &str = "/var/lib/brocade/cvm-cube";
const API: &str = "/agent/v1/cvm/stack-releases";
const SCRIPT: &[u8] = include_bytes!("../../../../scripts/cvm_stack.py");
const JOB_MAX: usize = 16 * 1024;
const MANAGER_DIRECTORIES: &[&str] = &[
    "/opt",
    "/opt/brocade",
    ROOT,
    "/usr",
    "/usr/local",
    "/usr/local/lib",
    "/usr/local/lib/brocade",
];

static SWITCH: Mutex<()> = Mutex::new(());

/// The self-update rename guard and marker publication share an Agent-process lock.
pub(crate) fn switch_guard() -> Result<MutexGuard<'static, ()>, String> {
    SWITCH
        .lock()
        .map_err(|_| "Cube stack maintenance lock unavailable".into())
}

pub(crate) fn fenced() -> Result<bool, String> {
    match fs::symlink_metadata(MARKER) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("cannot inspect Cube stack maintenance marker".into()),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Claiming,
    Claimed,
    Started,
    Recovering,
    Done(StackReport),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Job {
    offer: StackOffer,
    roots_sha256: String,
    phase: Phase,
}

impl Job {
    fn read() -> Result<Option<Self>, String> {
        let path = Path::new(MARKER);
        if !fenced()? {
            return Ok(None);
        }
        trusted_file(path, 0o077)?;
        read_json(path, JOB_MAX)?
            .ok_or_else(|| "maintenance marker disappeared".into())
            .map(Some)
    }

    fn save(&self) -> Result<(), String> {
        if self.offer.id <= 0 || !valid_digest(&self.roots_sha256) {
            return Err("invalid Cube stack job identity".into());
        }
        let bytes = serde_json::to_vec(self).map_err(|_| "cannot encode Cube stack job")?;
        atomic_write_private(Path::new(MARKER), &bytes)
    }

    fn finish(&mut self, status: StackReleaseStatus, error: Option<&str>) -> Result<(), String> {
        self.phase = Phase::Done(StackReport {
            id: self.offer.id,
            status,
            roots_sha256: Some(self.roots_sha256.clone()),
            error: error.map(str::to_owned),
        });
        self.save()?;
        start_agent()
    }
}

fn trusted_directory(path: &Path) -> Result<(), String> {
    let meta =
        fs::symlink_metadata(path).map_err(|_| "Cube installation directory is unavailable")?;
    if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err("unsafe Cube installation directory".into());
    }
    Ok(())
}

fn absent(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(_) => Err("cannot inspect Cube installation path".into()),
    }
}

fn trusted_file(path: &Path, prohibited_mode: u32) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|_| "Cube stack file unavailable")?;
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & prohibited_mode != 0 {
        return Err("unsafe Cube stack file".into());
    }
    Ok(())
}

fn lock_file(path: &str) -> Result<File, String> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "cannot open Cube stack maintenance lock")?;
    let metadata = lock
        .metadata()
        .map_err(|_| "cannot inspect Cube stack maintenance lock")?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err("unsafe Cube stack maintenance lock".into());
    }
    lock.try_lock()
        .map_err(|_| "Cube stack maintenance lock is busy or unavailable")?;
    Ok(lock)
}

fn read_json<T: DeserializeOwned>(path: &Path, cap: usize) -> Result<Option<T>, String> {
    read_bounded(path, cap)?
        .map(|bytes| {
            serde_json::from_slice(&bytes).map_err(|_| "invalid Cube stack checkpoint".into())
        })
        .transpose()
}

fn clear_job() -> Result<(), String> {
    fs::remove_file(MARKER)
        .and_then(|()| File::open(ROOT)?.sync_all())
        .map_err(|_| "cannot clear Cube stack maintenance fence".into())
}

fn timestamp() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|_| "node clock cannot validate Cube maintenance window".into())
}

fn key() -> Result<Option<[u8; 32]>, String> {
    let path = Path::new(KEY);
    let Some(bytes) = read_bounded(path, 256)? else {
        return Ok(None);
    };
    for directory in ["/etc", "/etc/brocade"] {
        trusted_directory(Path::new(directory))?;
    }
    trusted_file(path, 0o022)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "invalid Cube stack public key")?;
    stack_hex(text.trim())
        .map(Some)
        .ok_or_else(|| "invalid Cube stack public key".into())
}

fn verified(offer: &StackOffer, key: [u8; 32]) -> Result<StackPair, String> {
    let key = UnparsedPublicKey::new(&ED25519, key);
    let pair = offer
        .artifact
        .checked(|payload, signature| key.verify(payload, signature).is_ok())?;
    if offer.id <= 0
        || offer.expires_at > pair.expires_at
        || pair.architecture != crate::identity::self_arch()
    {
        return Err("Cube stack offer does not match the node or signed window".into());
    }
    Ok(pair)
}

fn manager() -> Result<(), String> {
    for directory in MANAGER_DIRECTORIES {
        trusted_directory(Path::new(directory))?;
    }
    let path = Path::new(MANAGER);
    trusted_file(path, 0o022)?;
    if read_bounded(path, 512 * 1024)?.as_deref() != Some(SCRIPT) {
        return Err(
            "install the matching managed Cube stack installer before enabling releases".into(),
        );
    }
    Ok(())
}

fn fresh_config() -> Result<bool, String> {
    Ok(!super::backend::configured()? && super::backend::host_config_path()?.is_none())
}

fn prepare_manager() -> Result<(), String> {
    if !absent(Path::new(ROOT))? && !absent(Path::new(MANAGER))? {
        return manager();
    }
    if !fresh_config()?
        || !absent(Path::new(ROOT).join("installation.json").as_path())?
        || !absent(Path::new(ROOT).join("current").as_path())?
    {
        return Err("install the matching Cube stack installer before enabling releases".into());
    }
    for name in MANAGER_DIRECTORIES {
        let path = Path::new(name);
        if let Err(error) = fs::create_dir(path) {
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err("cannot prepare managed Cube installation directories".into());
            }
        }
        trusted_directory(path)?;
    }
    let _lock = lock_file("/opt/brocade/cvm/install.lock")?;
    let path = Path::new(MANAGER);
    if absent(path)? {
        atomic_write_private(path, SCRIPT)?;
    }
    manager()
}

fn run_manager(args: &[&str], seconds: u32) -> Result<Vec<u8>, String> {
    let output = Command::new("/usr/bin/timeout")
        .args([
            "--kill-after=10s",
            &format!("{seconds}s"),
            "/usr/bin/python3",
            "-I",
            MANAGER,
        ])
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|_| "cannot run Cube stack installer")?;
    if !output.status.success() || output.stdout.len() > 16 * 1024 {
        return Err("Cube stack installer did not verify the requested state".into());
    }
    Ok(output.stdout)
}

fn installed() -> Result<StackInstalled, String> {
    let bytes = run_manager(&["stack-inspect"], 45)?;
    let installed: StackInstalled =
        serde_json::from_slice(&bytes).map_err(|_| "invalid Cube stack inventory")?;
    if !installed.valid() {
        return Err("invalid installed Cube stack identity".into());
    }
    Ok(installed)
}

fn cold() -> Result<String, String> {
    brocade_cube::Backend::inspect_stack_cold(Path::new(CONFIG), Path::new(EXECUTOR_STATE), false)
}

fn target_args(pair: &StackPair) -> [&str; 8] {
    [
        "--target",
        &pair.target_sha256,
        "--storage",
        pair.storage_backend.as_str(),
        "--target-cubelet",
        &pair.target_cubelet_sha256,
        "--target-shim",
        &pair.target_shim_sha256,
    ]
}

fn source_args(source: &StackInstalled) -> [&str; 6] {
    [
        "--source",
        &source.bundle_sha256,
        "--source-cubelet",
        &source.cubelet_sha256,
        "--source-shim",
        &source.shim_sha256,
    ]
}

fn fresh_storage() -> Result<StackStorage, String> {
    if !fresh_config()? {
        return Err("Agent already has a configured CVM backend".into());
    }
    let output = run_manager(&["stack-install-inspect"], 45)?;
    serde_json::from_slice(&output).map_err(|_| "invalid empty Cube host observation".into())
}

#[derive(Clone, Default)]
struct Observation {
    installed: Option<StackInstalled>,
    installable: Option<StackStorage>,
}

impl Observation {
    fn read() -> Result<Self, String> {
        let mut result = Self::default();
        if super::backend::config_path()? == Path::new(CONFIG) {
            result.installed = installed().ok();
        } else if fresh_config()? {
            // Absence of a healthy Cubelet is not positive evidence of an empty host.
            result.installable = fresh_storage().ok();
        }
        Ok(result)
    }

    fn matches_source(&self, pair: &StackPair) -> bool {
        match pair.source() {
            Some(source) => self.installed.as_ref() == Some(&source),
            None => self.installable == Some(pair.storage_backend),
        }
    }
}

fn source_roots(pair: &StackPair) -> Result<String, String> {
    if pair.is_install() {
        if fresh_storage()? != pair.storage_backend {
            return Err("empty Cube host storage changed".into());
        }
        Ok(pair.storage_backend.empty_roots_sha256())
    } else {
        cold()
    }
}

fn post<T: Serialize, R: DeserializeOwned>(
    options: &Options,
    path: &str,
    body: &T,
) -> Result<R, String> {
    let body = serde_json::to_string(body).map_err(|_| "cannot encode Cube stack request")?;
    let response =
        HttpClient::new(&options.server)?.request("POST", path, &options.token, Some(&body))?;
    if response.status != 200 {
        return Err(format!(
            "Cube stack request rejected (HTTP {})",
            response.status
        ));
    }
    serde_json::from_str(&response.body).map_err(|_| "invalid Cube stack API response".into())
}

fn poll(
    options: &Options,
    observed: Observation,
    report: Option<StackReport>,
) -> Result<Option<StackOffer>, String> {
    post(
        options,
        &format!("{API}/poll"),
        &StackPoll {
            installed: observed.installed,
            installable: observed.installable,
            report,
        },
    )
}

fn download(options: &Options, pair: &StackPair) -> Result<(), String> {
    let mut url = reqwest::Url::parse(&options.server).map_err(|_| "invalid Console origin")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("invalid Console origin for Cube stack archive".into());
    }
    let route = format!(
        "{}/agent/v1/cvm/stack-bundles/{}",
        url.path().trim_end_matches('/'),
        pair.target_sha256
    );
    url.set_path(&route);
    let tls = (*crate::http::tls_config()?).clone();
    let client = reqwest::blocking::Client::builder()
        .use_preconfigured_tls(tls)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .no_gzip()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(900))
        .build()
        .map_err(|_| "cannot initialize Cube stack archive download")?;
    let mut response = client
        .get(url)
        .bearer_auth(&options.token)
        .header(
            "X-Brocade-Protocol-Version",
            brocade_deployment::protocol::AGENT_PROTOCOL_VERSION,
        )
        .send()
        .map_err(|_| "cannot download Cube stack archive")?;
    if response.status() != reqwest::StatusCode::OK
        || response.content_length() != Some(pair.target_bytes)
    {
        return Err("Cube stack archive has unexpected response or length".into());
    }
    let path = Path::new(ARCHIVE);
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "cannot write private Cube stack archive")?;
    if !file
        .metadata()
        .is_ok_and(|meta| meta.is_file() && meta.uid() == 0 && meta.mode() & 0o077 == 0)
    {
        return Err("unsafe existing Cube stack archive".into());
    }
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut remaining = pair.target_bytes;
    let mut buffer = [0; 64 * 1024];
    while remaining > 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let length = response
            .read(&mut buffer[..limit])
            .map_err(|_| "Cube stack archive stream interrupted")?;
        if length == 0 {
            return Err("Cube stack archive truncated".into());
        }
        digest.update(&buffer[..length]);
        file.write_all(&buffer[..length])
            .map_err(|_| "cannot stage Cube stack archive; check free space")?;
        remaining -= length as u64;
    }
    if response
        .read(&mut buffer[..1])
        .map_err(|_| "Cube stack archive read failed")?
        != 0
        || brocade_core::hash::hex_lower(digest.finish().as_ref()) != pair.target_sha256
    {
        return Err("Cube stack archive digest does not match signed metadata".into());
    }
    file.sync_all()
        .map_err(|_| "cannot sync Cube stack archive".into())
}

fn stage(options: &Options, pair: &StackPair, observed: &Observation) -> Result<String, String> {
    if !observed.matches_source(pair) {
        return Err("Cube host no longer matches the signed starting state".into());
    }
    // Fail closed before a large download if the operator has not completed the cold window.
    let before = source_roots(pair)?;
    let result = (|| {
        download(options, pair)?;
        let source = pair.source();
        let command = if source.is_some() {
            "stack-stage"
        } else {
            "stack-install-stage"
        };
        let mut arguments = vec![command, "--bundle", ARCHIVE];
        arguments.extend(target_args(pair));
        if let Some(source) = &source {
            arguments.extend(source_args(source));
        }
        run_manager(&arguments, 900)?;
        if source_roots(pair)? != before {
            return Err("Cube root evidence changed during preflight".into());
        }
        Ok(before)
    })();
    match fs::remove_file(ARCHIVE) {
        Ok(()) => result,
        Err(error) if error.kind() == io::ErrorKind::NotFound => result,
        Err(_) => Err("cannot remove staged Cube stack download".into()),
    }
}

fn start_agent() -> Result<(), String> {
    let status = Command::new("/usr/bin/timeout")
        .args([
            "30s",
            "/usr/bin/systemctl",
            "start",
            "brocade-agent.service",
        ])
        .status()
        .map_err(|_| "cannot start Agent after Cube stack maintenance")?;
    if !status.success() {
        return Err("Agent service did not restart after Cube stack maintenance".into());
    }
    Ok(())
}

fn unit(id: i64) -> String {
    format!("brocade-cvm-stack-{id}.service")
}

fn launch(id: i64) -> Result<(), String> {
    let unit = unit(id);
    let state = Command::new("/usr/bin/timeout")
        .args([
            "10s",
            "/usr/bin/systemctl",
            "show",
            "--value",
            "--property=ActiveState",
            &unit,
        ])
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "cannot inspect Cube stack execution unit")?;
    if matches!(
        std::str::from_utf8(&state.stdout).unwrap_or("").trim(),
        "active" | "activating" | "deactivating" | "reloading"
    ) {
        return Ok(());
    }
    let status = Command::new("/usr/bin/timeout")
        .args([
            "15s",
            "/usr/bin/systemd-run",
            "--quiet",
            "--no-block",
            "--collect",
            "--unit",
            &unit,
            "--property=Type=exec",
            "--property=RuntimeMaxSec=1800",
            "--property=UMask=0077",
            "--property=Restart=on-failure",
            "--property=RestartSec=10s",
            "--property=StartLimitBurst=3",
            "--property=StartLimitIntervalSec=120s",
            AGENT,
            "cvm-stack-execute",
            &id.to_string(),
        ])
        .status()
        .map_err(|_| "cannot launch independent Cube stack execution unit")?;
    if !status.success() {
        return Err("independent Cube stack execution unit was not accepted".into());
    }
    Ok(())
}

fn advance(options: &Options, mut job: Job, key: [u8; 32]) -> Result<(), String> {
    let pair = verified(&job.offer, key)?;
    match &job.phase {
        Phase::Claiming => {
            let offer = poll(options, Observation::read()?, None)?;
            if !offer.as_ref().is_some_and(|offer| {
                offer.id == job.offer.id
                    && matches!(
                        offer.status,
                        StackReleaseStatus::Approved | StackReleaseStatus::Applying
                    )
            }) {
                // Poll is authoritative for a claim reply lost before the agent restarted.
                return clear_job();
            }
            let confirmed: StackOffer = post(
                options,
                &format!("{API}/{}/claim", job.offer.id),
                &StackClaim {
                    roots_sha256: job.roots_sha256.clone(),
                },
            )?;
            if confirmed.id != job.offer.id
                || confirmed.artifact != job.offer.artifact
                || confirmed.roots_sha256.as_deref() != Some(job.roots_sha256.as_str())
                || confirmed.status != StackReleaseStatus::Applying
            {
                return Err("Cube stack claim evidence has changed".into());
            }
            job.offer = confirmed;
            job.phase = Phase::Claimed;
            job.save()?;
            launch(job.offer.id)
        }
        Phase::Claimed | Phase::Started | Phase::Recovering => launch(job.offer.id),
        Phase::Done(report) => {
            let observed = Observation::read()?;
            let report = match report.status {
                StackReleaseStatus::Succeeded if observed.installed != Some(pair.target()) => {
                    StackReport {
                        status: StackReleaseStatus::Blocked,
                        error: Some("目标栈完成后实际状态发生漂移，保持维护".into()),
                        ..report.clone()
                    }
                }
                StackReleaseStatus::Recovered | StackReleaseStatus::Failed
                    if !observed.matches_source(&pair) =>
                {
                    StackReport {
                        status: StackReleaseStatus::Blocked,
                        error: Some("宿主状态无法核验，保持维护".into()),
                        ..report.clone()
                    }
                }
                _ => report.clone(),
            };
            if report.status == StackReleaseStatus::Blocked
                && !matches!(&job.phase, Phase::Done(done) if done.status == report.status)
            {
                job.phase = Phase::Done(report.clone());
                job.save()?;
            }
            let reply = poll(options, observed, Some(report.clone()))?;
            if let Some(offer) = reply.filter(|offer| offer.id == report.id) {
                if offer.status == StackReleaseStatus::Blocked
                    && report.status == StackReleaseStatus::Blocked
                {
                    return Ok(());
                }
                return Err("Console has not accepted the Cube stack receipt".into());
            }
            if report.status != StackReleaseStatus::Blocked {
                clear_job()?;
            }
            Ok(())
        }
    }
}

fn round(options: &Options) -> Result<(), String> {
    let Some(key) = key()? else {
        return Ok(());
    };
    if super::backend::state_path()? != Path::new(EXECUTOR_STATE) {
        return Err("Cube stack releases require the managed Agent execution state".into());
    }
    prepare_manager()?;
    if let Some(job) = Job::read()? {
        return advance(options, job, key);
    }
    let observed = Observation::read()?;
    let Some(offer) = poll(options, observed.clone(), None)? else {
        return Ok(());
    };
    let pair = verified(&offer, key)?;
    if timestamp()? >= offer.expires_at {
        return Ok(());
    }
    match offer.status {
        StackReleaseStatus::Preparing => {
            let staged = stage(options, &pair, &observed);
            let (status, roots_sha256, error) = match staged {
                Ok(roots) => (StackReleaseStatus::Ready, Some(roots), None),
                Err(error) => {
                    eprintln!("Cube stack preflight: {error}");
                    (
                        StackReleaseStatus::Failed,
                        None,
                        Some("预检未通过：请核对原盘、版本、存储空间和签名归档".into()),
                    )
                }
            };
            poll(
                options,
                Observation::read()?,
                Some(StackReport {
                    id: offer.id,
                    status,
                    roots_sha256,
                    error,
                }),
            )?;
            Ok(())
        }
        StackReleaseStatus::Approved => {
            let roots = offer
                .roots_sha256
                .as_deref()
                .filter(|digest| valid_digest(digest))
                .ok_or("Cube stack approval lacks cold root evidence")?;
            if !observed.matches_source(&pair) || source_roots(&pair)? != roots {
                return Err("approved Cube stack starting state has changed".into());
            }
            let job = Job {
                roots_sha256: roots.to_owned(),
                offer,
                phase: Phase::Claiming,
            };
            {
                let _guard = switch_guard()?;
                // Ordinary installer mutations check the fence under this same lock.
                let _installer_lock = lock_file("/opt/brocade/cvm/install.lock")?;
                let running =
                    crate::identity::self_sha256().ok_or("cannot identify the running Agent")?;
                if crate::fsutil::file_sha256(Path::new(AGENT)).ok().as_deref() != Some(running)
                    || options
                        .state_dir
                        .join("agent-release-pending.json")
                        .try_exists()
                        .map_err(|_| "cannot inspect Agent self-update state")?
                {
                    return Err(
                        "Agent replacement is not settled; cannot claim Cube upgrade".into(),
                    );
                }
                job.save()?;
            }
            advance(options, job, key)
        }
        StackReleaseStatus::Applying | StackReleaseStatus::Blocked => Err(
            "Console has an in-flight Cube stack job with no local marker; inspect manually".into(),
        ),
        _ => Ok(()),
    }
}

fn verifying(pair: &StackInstalled, roots: &str) -> bool {
    installed().is_ok_and(|observed| &observed == pair)
        && cold().is_ok_and(|observed| observed == roots)
}

fn restore_source(source: &StackInstalled, roots: &str) -> bool {
    if verifying(source, roots) {
        return true;
    }
    if let Err(error) = run_manager(
        &[
            "stack-recover",
            "--source",
            &source.bundle_sha256,
            "--expected-roots-sha256",
            roots,
        ],
        840,
    ) {
        eprintln!("Cube stack source recovery: {error}");
    }
    verifying(source, roots)
}

fn execute_first_install(mut job: Job, pair: &StackPair, resume: bool) -> Result<(), String> {
    if job.roots_sha256 != pair.storage_backend.empty_roots_sha256() {
        return Err("Cube stack first install lacks canonical empty roots evidence".into());
    }
    let command = if resume {
        job.phase = Phase::Recovering;
        Some("stack-install-resume")
    } else {
        match job.phase {
            Phase::Claimed if timestamp()? >= job.offer.expires_at => {
                return if source_roots(pair).as_deref() == Ok(job.roots_sha256.as_str()) {
                    job.finish(
                        StackReleaseStatus::Failed,
                        Some("首装窗口已结束，宿主仍为空"),
                    )
                } else {
                    job.finish(
                        StackReleaseStatus::Blocked,
                        Some("首装窗口已结束，空机证据不完整"),
                    )
                };
            }
            Phase::Claimed => {
                job.phase = Phase::Started;
                Some("stack-install-activate")
            }
            Phase::Started | Phase::Recovering => None,
            Phase::Done(_) => return start_agent(),
            Phase::Claiming => return Err("Cube stack first install is not claimed".into()),
        }
    };
    if let Some(command) = command {
        job.save()?;
        let mut args = vec![command, "--expected-roots-sha256", &job.roots_sha256];
        args.extend(target_args(pair));
        if let Err(error) = run_manager(&args, 840) {
            eprintln!("Cube stack first installation: {error}");
        }
    }
    if verifying(&pair.target(), &job.roots_sha256) {
        return job.finish(StackReleaseStatus::Succeeded, None);
    }
    if matches!(job.phase, Phase::Started)
        && source_roots(pair).as_deref() == Ok(job.roots_sha256.as_str())
    {
        return job.finish(
            StackReleaseStatus::Failed,
            Some("首装未改变宿主，已核验空机"),
        );
    }
    job.finish(
        StackReleaseStatus::Blocked,
        Some("无法核验首装目标，保留现场以恢复同一目标"),
    )
}

/// Executed independently of the root-owned Agent service. A process death
/// after `Started` is never interpreted as an instruction to reapply the target.
pub(crate) fn execute(args: &[String]) -> Result<(), String> {
    let (id, retry_blocked) = match args {
        [id] => (id, false),
        [id, option] if option == "--recover-blocked" => (id, true),
        _ => return Err("invalid Cube stack execution identity".into()),
    };
    manager()?;
    let _execution_lock = lock_file("/opt/brocade/cvm/stack-execute.lock")?;
    let mut job = Job::read()?.ok_or("no authorized local Cube stack job")?;
    if id.parse::<i64>().ok() != Some(job.offer.id) {
        return Err("Cube stack job identity changed".into());
    }
    let pair = verified(
        &job.offer,
        key()?.ok_or("Cube stack signing trust unavailable")?,
    )?;
    if job.offer.status != StackReleaseStatus::Applying {
        return Err("Cube stack job has not been claimed".into());
    }
    if retry_blocked
        && !matches!(&job.phase, Phase::Done(report) if report.status == StackReleaseStatus::Blocked)
    {
        return Err("only a locally blocked Cube stack job can be recovered manually".into());
    }
    let Some(source) = pair.source() else {
        return execute_first_install(job, &pair, retry_blocked);
    };
    if retry_blocked {
        if restore_source(&source, &job.roots_sha256) {
            return job.finish(
                StackReleaseStatus::Recovered,
                Some("人工恢复后已核验原版本及原盘"),
            );
        }
        start_agent()?;
        return Err("the original Cube stack and roots still cannot be verified".into());
    }
    if let Phase::Claimed = job.phase {
        if timestamp()? >= job.offer.expires_at {
            return if verifying(&source, &job.roots_sha256) {
                job.finish(
                    StackReleaseStatus::Recovered,
                    Some("窗口已结束，未切换，已核验原栈"),
                )
            } else {
                job.finish(
                    StackReleaseStatus::Blocked,
                    Some("窗口已结束，但原栈证据不完整"),
                )
            };
        }
        job.phase = Phase::Started;
        job.save()?;
        let mut arguments = vec![
            "stack-activate",
            "--expected-roots-sha256",
            &job.roots_sha256,
        ];
        arguments.extend(target_args(&pair));
        arguments.extend(source_args(&source));
        if let Err(error) = run_manager(&arguments, 840) {
            eprintln!("Cube stack activation: {error}");
        }
    } else if matches!(job.phase, Phase::Done(_)) {
        // The last process may have died after recording the outcome but before
        // restarting the polling Agent. Retrying the service only restores it.
        return start_agent();
    } else if !matches!(job.phase, Phase::Started | Phase::Recovering) {
        return Err("Cube stack job is not executable in its current phase".into());
    }
    if matches!(job.phase, Phase::Started) && verifying(&pair.target(), &job.roots_sha256) {
        return job.finish(StackReleaseStatus::Succeeded, None);
    }
    if matches!(job.phase, Phase::Recovering) && verifying(&source, &job.roots_sha256) {
        return job.finish(
            StackReleaseStatus::Recovered,
            Some("本次切换未成功；已核验原版本及原盘"),
        );
    }
    if !matches!(job.phase, Phase::Recovering) {
        job.phase = Phase::Recovering;
        job.save()?;
        if restore_source(&source, &job.roots_sha256) {
            return job.finish(
                StackReleaseStatus::Recovered,
                Some("本次切换未成功；已恢复并核验原版本及原盘"),
            );
        }
    }
    job.finish(
        StackReleaseStatus::Blocked,
        Some("无法证明已恢复原版本及原盘，保留维护现场"),
    )
}

pub(super) fn spawn(options: Options) -> Result<(), String> {
    if options.apply_mode != ApplyMode::Linux || crate::identity::self_arch() != "x86_64" {
        return Ok(());
    }
    thread::Builder::new()
        .name("cvm-stack-update".into())
        .spawn(move || {
            let mut previous = String::new();
            loop {
                match round(&options) {
                    Ok(()) => previous.clear(),
                    Err(error) if error != previous => {
                        eprintln!("Cube stack maintenance: {error}");
                        previous = error;
                    }
                    Err(_) => {}
                }
                thread::sleep(Duration::from_secs(20));
            }
        })
        .map_err(|_| "cannot spawn Cube stack update worker")?;
    Ok(())
}
