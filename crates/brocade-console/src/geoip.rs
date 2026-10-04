//! Country lookup for machines and user source IPs, backed by the configured `geoip.dat`.
//!
//! The agents already download this file for Xray rules, but it lives on remote machines. The
//! control plane keeps its own copy: in memory for lookups, and on disk so a restart does not
//! need the network. One download per day.
//! Network-operator attribution lives alongside it in `operators`: same cache directory,
//! refresh cadence and local-only request path, but an independent source and index.
//!
//! # Why there is a file on disk
//!
//! There was not one at first, and the reason recorded here was that the configured URL is
//! durable, so a stale copy is less useful than fetching the current one. That is true of the
//! copy and false of the situation: when the current one cannot be fetched, there is no copy at
//! all, and the machine list loses every flag until the download works. Yesterday's countries are
//! better than none, and they are what the page showed a moment before the restart.
//!
//! So the bytes are written next to the process and read back on start. The file is named after
//! the URL's digest, which is what binds the two — see `cache_path`.
//!
//! # Why the download is a worker and not part of the request
//!
//! It used to run inside the `/nodes/agent-state` handler: the first request after a restart
//! carried a 17 MiB download, and the result was cached for a day. That shape had a failure mode
//! with no trace at all.
//!
//! `attempted_at` was written *before* awaiting the download, so that a server which is down does
//! not get hammered by a list that refreshes every ten seconds. But if the request was cancelled
//! while the body was still arriving — a refresh, a tab switch, the front end giving up, a proxy
//! timing out — the future was dropped **on that await**. The `Err` arm never ran, so nothing was
//! logged; `attempted_at` stayed set, so nothing retried for the next hour. Every list in that
//! hour came back with no countries, and the only evidence was the absence of flags, which looks
//! exactly like a fleet whose addresses are not in the database.
//!
//! Moving the download to a worker removes the whole class: no request awaits the network, so no
//! request can strand the retry state by being cancelled, and the loop that owns the timing is
//! only ever cancelled at shutdown. `countries` became a pure read of what the worker published.
//!
//! It also makes the warm-up observable — the worker says so when a database lands, so "the flags
//! are missing" can be told apart from "the console never got a database" by reading the journal.

use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use brocade_store::PgStore;
use prost::Message;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

mod operators;
pub use operators::NetworkOperator;

const FRESH_FOR: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the worker re-reads the configured URL. One row of `control_state`; it exists so a
/// settings change takes effect within a minute instead of waiting out `FRESH_FOR`.
const POLL: Duration = Duration::from_secs(60);
/// Retry floor and ceiling after a failed download, doubling in between. The old flat hour was
/// chosen to protect a server that is down, but it also meant a single transient failure cost a
/// full hour of blank flags; backing off from 30 seconds protects the server just as well while
/// recovering from a blip in one round.
const RETRY_MIN: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(30 * 60);
/// Generous now that nobody is waiting on it. It was 8 seconds when a request carried the
/// download and a slow CDN would have held the machine list open; off the request path the only
/// cost of waiting is that the first flags appear later.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_DATABASE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct GeoIpLookup {
    http: reqwest::Client,
    cache: Arc<Mutex<Cache>>,
    operators: operators::OperatorLookup,
}

/// What the worker published, and which configured URL it came from. Freshness and retry timing
/// are not here: they belong to the worker's own loop, which is the only thing that acts on them.
#[derive(Default)]
struct Cache {
    source: String,
    database: Option<GeoDatabase>,
}

impl Default for GeoIpLookup {
    fn default() -> Self {
        Self {
            http: reqwest::Client::builder()
                .use_rustls_tls()
                .timeout(DOWNLOAD_TIMEOUT)
                .user_agent(concat!("brocade-console/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("the static GeoIP HTTP client configuration is valid"),
            cache: Arc::new(Mutex::new(Cache::default())),
            operators: operators::OperatorLookup::default(),
        }
    }
}

/// Starts the refresher and hands back the handle the request path reads.
///
/// Shaped like `certs::spawn`: `main.rs` owns the worker, and every other caller — the tests,
/// `brocade-preview` — keeps building a plain `GeoIpLookup::default()`. That one is never fed, so
/// `countries` returns nothing and the list renders without flags, which is the same degradation
/// as a database that has not arrived yet.
///
/// Reading the on-disk copy before reaching for the network, for the reason `certs::spawn` gives:
/// a restart is not a quiet moment for this. It is exactly when the in-memory copy is gone, and
/// the file is what makes a restart cost nothing — the countries are back before the first page
/// load, whether or not the CDN answers.
pub fn spawn(store: PgStore) -> GeoIpLookup {
    let lookup = GeoIpLookup::default();
    lookup.operators.spawn(lookup.http.clone());
    let worker = lookup.clone();
    tokio::spawn(async move {
        let mut backoff = RETRY_MIN;
        // `(url, fetched_at)` of what is currently published. Kept here rather than in the shared
        // cache because nothing else acts on it, and a value only this loop touches cannot be
        // left inconsistent by anything else.
        //
        // `SystemTime`, not `Instant`: the file's mtime is the fetch time for a database that
        // came off disk, and an `Instant` cannot carry a moment from before this process started.
        // The daily cadence therefore survives a restart instead of restarting with it.
        let mut loaded: Option<(String, SystemTime)> = None;
        // Which URL the disk was already consulted for. One attempt per configured URL: a miss
        // must not turn into a file read on every poll.
        let mut disk_tried = String::new();
        loop {
            let configured = match store.settings().await {
                Ok(settings) => settings.geodata.geoip_url.trim().to_owned(),
                Err(error) => {
                    eprintln!("geoip: 读取配置失败：{error}");
                    tokio::time::sleep(POLL).await;
                    continue;
                }
            };
            if configured.is_empty() {
                if loaded.is_some() {
                    worker.clear().await;
                    loaded = None;
                }
                tokio::time::sleep(POLL).await;
                continue;
            }

            if disk_tried != configured {
                disk_tried = configured.clone();
                if let Some(at) = worker.load_cached(&configured).await {
                    loaded = Some((configured.clone(), at));
                }
            }

            let due = match &loaded {
                Some((source, at)) => source != &configured || age(*at) >= FRESH_FOR,
                None => true,
            };
            if !due {
                tokio::time::sleep(POLL).await;
                continue;
            }

            match worker.fetch(&configured).await {
                Ok(bytes) => match GeoDatabase::decode(&bytes) {
                    Ok(database) => {
                        // Said out loud on success, not only on failure. Without it there is no
                        // way to tell "the worker has a database and these addresses are not in
                        // it" from "the worker never got one" — the two look identical on the
                        // page, and that ambiguity is what made the previous shape hard to
                        // diagnose.
                        eprintln!("geoip: 国家库已更新（{} 条网段）", database.len());
                        worker.publish(configured.clone(), database).await;
                        write_cached(&configured, &bytes).await;
                        loaded = Some((configured, SystemTime::now()));
                        backoff = RETRY_MIN;
                        tokio::time::sleep(POLL).await;
                    }
                    Err(error) => {
                        eprintln!(
                            "geoip: 国家库解析失败：{error}（{} 秒后重试）",
                            backoff.as_secs()
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(RETRY_MAX);
                    }
                },
                Err(error) => {
                    eprintln!(
                        "geoip: 更新国家库失败：{error}（{} 秒后重试）",
                        backoff.as_secs()
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(RETRY_MAX);
                }
            }
        }
    });
    lookup
}

/// How long ago, treating a clock that ran backwards as "due". Being wrong here costs one
/// download; refusing to refresh because the clock moved costs a day of a stale database.
fn age(at: SystemTime) -> Duration {
    SystemTime::now().duration_since(at).unwrap_or(FRESH_FOR)
}

/// Where the downloaded database is kept between restarts.
///
/// `BROCADE_CACHE_DIR`, defaulting to the working directory — which the unit file pins to
/// `/opt/brocade`. No directory is created and none is required: an unwritable path costs a
/// download per restart and nothing else.
fn cache_dir() -> PathBuf {
    std::env::var_os("BROCADE_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The file name carries the URL's digest, so the file *is* the binding between the two.
///
/// The alternative — one fixed name plus a sidecar naming the URL — has a torn state: whichever
/// of the two is written second, a process killed in between leaves a database attributed to the
/// wrong URL, and the countries on screen would then be looked up in a database the fleet is no
/// longer configured to use. With the digest in the name there is one file and one rename, and a
/// URL change simply misses.
fn cache_path(source: &str) -> PathBuf {
    let digest = Sha256::digest(source.as_bytes());
    cache_dir().join(format!("geoip-{:x}.dat", digest))
}

/// Writes the copy the next restart will read. Best effort throughout: this is a cache, and every
/// failure here costs one download.
async fn write_cached(source: &str, bytes: &[u8]) {
    let path = cache_path(source);
    // Through a temporary file and a rename. A process killed mid-write would otherwise leave a
    // truncated database that the next start reads, fails to decode, and has to download anyway —
    // the same cost as no cache, plus a confusing parse error in the journal.
    let temporary = path.with_extension("dat.part");
    if let Err(error) = tokio::fs::write(&temporary, bytes).await {
        eprintln!("geoip: 写缓存 {} 失败：{error}", temporary.display());
        return;
    }
    if let Err(error) = tokio::fs::rename(&temporary, &path).await {
        eprintln!("geoip: 缓存改名到 {} 失败：{error}", path.display());
        let _ = tokio::fs::remove_file(&temporary).await;
    }
}

impl GeoIpLookup {
    /// Local network attribution, independent of the country database and Xray routing data.
    pub async fn operators(&self, addresses: &[String]) -> HashMap<String, NetworkOperator> {
        self.operators.lookup(addresses).await
    }

    /// Publishes the copy on disk, if there is one for this URL and it still decodes.
    ///
    /// Returns when it was fetched — the file's mtime — so the caller can tell a copy that is
    /// still within the day from one that needs replacing. A stale copy is published either way:
    /// yesterday's countries are better than none while the download is in flight, and they are
    /// what the page showed a moment before the restart.
    async fn load_cached(&self, source: &str) -> Option<SystemTime> {
        let path = cache_path(source);
        let bytes = tokio::fs::read(&path).await.ok()?;
        let at = tokio::fs::metadata(&path).await.ok()?.modified().ok()?;
        match GeoDatabase::decode(&bytes) {
            Ok(database) => {
                eprintln!(
                    "geoip: 从 {} 读到国家库（{} 条网段，{} 小时前的）",
                    path.display(),
                    database.len(),
                    age(at).as_secs() / 3600
                );
                self.publish(source.to_owned(), database).await;
                Some(at)
            }
            Err(error) => {
                eprintln!("geoip: 缓存 {} 解析失败，当作没有：{error}", path.display());
                None
            }
        }
    }

    /// Publishes a freshly downloaded database. The URL travels with it so `countries` can tell
    /// whether what it holds answers the question being asked.
    async fn publish(&self, source: String, database: GeoDatabase) {
        let mut cache = self.cache.lock().await;
        cache.source = source;
        cache.database = Some(database);
    }

    /// Drops what is held when the setting is emptied. Keeping the cached database
    /// would show countries the fleet is no longer configured to look up.
    async fn clear(&self) {
        let mut cache = self.cache.lock().await;
        cache.source.clear();
        cache.database = None;
    }

    /// Returns country codes for literal IPv4/IPv6 addresses. Hostnames and masked addresses
    /// are intentionally absent: DNS resolution here would make opening the list trigger an
    /// operator-controlled collection of outbound lookups.
    ///
    /// Never touches the network: it reads whatever the worker last published, or nothing.
    pub async fn countries(&self, source: &str, addresses: &[String]) -> HashMap<String, String> {
        let parsed = addresses
            .iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .filter_map(|address| address.parse::<IpAddr>().ok().map(|ip| (address, ip)))
            .collect::<Vec<_>>();
        if parsed.is_empty() || source.trim().is_empty() {
            return HashMap::new();
        }

        let cache = self.cache.lock().await;
        // Answering from a database fetched for a different URL would keep the old fleet's
        // countries on screen after an operator changes the setting, for up to a minute, with
        // nothing saying which of the two is being shown. Blank until the worker catches up.
        if cache.source != source.trim() {
            return HashMap::new();
        }
        let Some(database) = cache.database.as_ref() else {
            return HashMap::new();
        };
        parsed
            .into_iter()
            .filter_map(|(raw, ip)| {
                database
                    .country(ip)
                    .map(|country| (raw.clone(), country.to_owned()))
            })
            .collect()
    }

    /// The raw bytes, not a decoded database: they are what gets written to the cache file, and
    /// re-encoding a decoded one would store something that is not the file the URL serves.
    async fn fetch(&self, source: &str) -> Result<Vec<u8>, String> {
        let response = self
            .http
            .get(source)
            .send()
            .await
            .map_err(|error| format!("下载 {source}：{error}"))?
            .error_for_status()
            .map_err(|error| format!("下载 {source}：{error}"))?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_DATABASE_BYTES as u64)
        {
            return Err(format!(
                "{source} 超过 {} MiB",
                MAX_DATABASE_BYTES / 1024 / 1024
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("读取 {source}：{error}"))?;
        if bytes.len() > MAX_DATABASE_BYTES {
            return Err(format!(
                "{source} 超过 {} MiB",
                MAX_DATABASE_BYTES / 1024 / 1024
            ));
        }
        Ok(bytes.to_vec())
    }
}

/// One hash table for each prefix length. Lookup is at most 33/129 probes instead of walking
/// every CIDR in the 10+ MiB database for every machine on every list refresh.
struct GeoDatabase {
    v4: Vec<HashMap<u32, String>>,
    v6: Vec<HashMap<u128, String>>,
}

impl GeoDatabase {
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let list = GeoIpList::decode(bytes).map_err(|error| format!("解析 geoip.dat：{error}"))?;
        let mut v4 = (0..=32).map(|_| HashMap::new()).collect::<Vec<_>>();
        let mut v6 = (0..=128).map(|_| HashMap::new()).collect::<Vec<_>>();
        for entry in list.entry {
            let code = entry.country_code.trim().to_ascii_uppercase();
            // The database also contains tags such as PRIVATE. A flag requires an ISO alpha-2
            // country; treating those tags as a country would render boxed letters in the UI.
            if entry.reverse_match
                || code.len() != 2
                || !code.bytes().all(|byte| byte.is_ascii_alphabetic())
            {
                continue;
            }
            for cidr in entry.cidr {
                match cidr.ip.as_slice() {
                    [a, b, c, d] if cidr.prefix <= 32 => {
                        let raw = u32::from_be_bytes([*a, *b, *c, *d]);
                        v4[cidr.prefix as usize]
                            .entry(raw & prefix_mask(cidr.prefix))
                            .or_insert_with(|| code.clone());
                    }
                    bytes if bytes.len() == 16 && cidr.prefix <= 128 => {
                        let raw = u128::from_be_bytes(
                            bytes
                                .try_into()
                                .expect("the IPv6 address has exactly 16 bytes"),
                        );
                        v6[cidr.prefix as usize]
                            .entry(raw & prefix_mask_v6(cidr.prefix))
                            .or_insert_with(|| code.clone());
                    }
                    _ => {}
                }
            }
        }
        Ok(Self { v4, v6 })
    }

    /// Networks held, across every prefix length. Only for the line the worker logs on success.
    fn len(&self) -> usize {
        self.v4.iter().map(HashMap::len).sum::<usize>()
            + self.v6.iter().map(HashMap::len).sum::<usize>()
    }

    fn country(&self, ip: IpAddr) -> Option<&str> {
        match ip {
            IpAddr::V4(ip) => {
                let raw = u32::from(ip);
                for prefix in (0..=32).rev() {
                    let network = raw & prefix_mask(prefix);
                    if let Some(country) = self.v4[prefix as usize].get(&network) {
                        return Some(country);
                    }
                }
            }
            IpAddr::V6(ip) => {
                if let Some(ip) = ip.to_ipv4_mapped() {
                    return self.country(IpAddr::V4(ip));
                }
                let raw = u128::from(ip);
                for prefix in (0..=128).rev() {
                    let network = raw & prefix_mask_v6(prefix);
                    if let Some(country) = self.v6[prefix as usize].get(&network) {
                        return Some(country);
                    }
                }
            }
        }
        None
    }
}

fn prefix_mask(prefix: u32) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

fn prefix_mask_v6(prefix: u32) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    }
}

#[derive(Clone, PartialEq, Message)]
struct GeoIpList {
    #[prost(message, repeated, tag = "1")]
    entry: Vec<GeoIp>,
}

#[derive(Clone, PartialEq, Message)]
struct GeoIp {
    #[prost(string, tag = "1")]
    country_code: String,
    #[prost(message, repeated, tag = "2")]
    cidr: Vec<Cidr>,
    #[prost(bool, tag = "3")]
    reverse_match: bool,
}

#[derive(Clone, PartialEq, Message)]
struct Cidr {
    #[prost(bytes = "vec", tag = "1")]
    ip: Vec<u8>,
    #[prost(uint32, tag = "2")]
    prefix: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file name is the only thing tying a cached database to the URL it came from, so it has
    /// to be stable for one URL and distinct across URLs. Getting this wrong is not visible at
    /// runtime: the console would serve the previous URL's countries after the setting changed.
    #[test]
    fn the_cache_file_is_named_after_the_url() {
        let a = "https://example.net/geoip.dat";
        let b = "https://example.org/geoip.dat";
        assert_eq!(cache_path(a), cache_path(a));
        assert_ne!(cache_path(a), cache_path(b));
        let name = cache_path(a)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap()
            .to_owned();
        assert!(name.starts_with("geoip-"), "{name}");
        assert!(name.ends_with(".dat"), "{name}");
    }

    #[test]
    fn decodes_country_cidrs_and_prefers_the_longest_prefix() {
        let list = GeoIpList {
            entry: vec![
                GeoIp {
                    country_code: "US".to_owned(),
                    cidr: vec![Cidr {
                        ip: vec![203, 0, 113, 0],
                        prefix: 24,
                    }],
                    reverse_match: false,
                },
                GeoIp {
                    country_code: "JP".to_owned(),
                    cidr: vec![Cidr {
                        ip: vec![203, 0, 113, 128],
                        prefix: 25,
                    }],
                    reverse_match: false,
                },
                GeoIp {
                    country_code: "private".to_owned(),
                    cidr: vec![Cidr {
                        ip: vec![10, 0, 0, 0],
                        prefix: 8,
                    }],
                    reverse_match: false,
                },
            ],
        };
        let database = GeoDatabase::decode(&list.encode_to_vec()).unwrap();
        assert_eq!(database.country("203.0.113.7".parse().unwrap()), Some("US"));
        assert_eq!(
            database.country("203.0.113.200".parse().unwrap()),
            Some("JP")
        );
        assert_eq!(database.country("10.0.0.1".parse().unwrap()), None);
    }

    #[tokio::test]
    async fn source_countries_use_only_the_matching_local_database_for_both_families() {
        let lookup = GeoIpLookup::default();
        let addresses = vec![
            "203.0.113.7".to_owned(),
            "2001:db8::1".to_owned(),
            "2001:db8::2".to_owned(),
            "::ffff:203.0.113.7".to_owned(),
            "203.0.113.7".to_owned(),
            "example.invalid".to_owned(),
            "203.0.***.***".to_owned(),
            "fd00::1".to_owned(),
        ];
        let source = "https://example.invalid/geoip.dat";
        // An unfed lookup must return immediately; it never resolves or contacts this URL.
        assert!(lookup.countries(source, &addresses).await.is_empty());
        let v6 = |ip: &str, prefix| Cidr {
            ip: ip.parse::<std::net::Ipv6Addr>().unwrap().octets().to_vec(),
            prefix,
        };
        let entry = |country: &str, cidr| GeoIp {
            country_code: country.to_owned(),
            cidr,
            reverse_match: false,
        };
        let list = GeoIpList {
            entry: vec![
                entry(
                    "US",
                    vec![Cidr {
                        ip: vec![203, 0, 113, 0],
                        prefix: 24,
                    }],
                ),
                entry("JP", vec![v6("2001:db8::", 32)]),
                entry("TW", vec![v6("2001:db8::1", 128)]),
                entry("PRIVATE", vec![v6("fd00::", 8)]),
                entry("XX", vec![v6("2001:db8::2", 129)]),
                GeoIp {
                    reverse_match: true,
                    ..entry("DE", vec![v6("2001:db8::2", 128)])
                },
            ],
        };
        let database = GeoDatabase::decode(&list.encode_to_vec()).unwrap();
        assert_eq!(database.len(), 3);
        lookup.publish(source.to_owned(), database).await;
        let countries = lookup.countries(source, &addresses).await;
        assert_eq!(countries.len(), 4);
        assert_eq!(countries["203.0.113.7"], "US");
        assert_eq!(countries["2001:db8::1"], "TW");
        assert_eq!(countries["2001:db8::2"], "JP");
        assert_eq!(countries["::ffff:203.0.113.7"], "US");
        assert!(lookup
            .countries("https://other.invalid/geoip.dat", &addresses)
            .await
            .is_empty());
        assert!(lookup.countries("", &addresses).await.is_empty());
        lookup.clear().await;
        assert!(lookup.countries(source, &addresses).await.is_empty());
    }

    #[test]
    fn ipv6_zero_prefix_matches_without_overflow() {
        let list = GeoIpList {
            entry: vec![GeoIp {
                country_code: "JP".to_owned(),
                cidr: vec![Cidr {
                    ip: vec![0; 16],
                    prefix: 0,
                }],
                reverse_match: false,
            }],
        };
        let database = GeoDatabase::decode(&list.encode_to_vec()).unwrap();
        assert_eq!(database.country("2001:db8::1".parse().unwrap()), Some("JP"));
        assert_eq!(database.country("203.0.113.1".parse().unwrap()), None);
    }
}
