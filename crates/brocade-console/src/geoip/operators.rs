//! Local network attribution from gaoyifan/china-operator-ip (MIT).
//! These are BGP-derived network operators, not necessarily the user's retail ISP.
//! Fetch every list at one immutable revision; never publish a partially refreshed index.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::IpAddr,
    path::Path,
    sync::Arc,
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use tokio::{io::AsyncReadExt, sync::RwLock};

use super::{age, cache_dir, prefix_mask, prefix_mask_v6, FRESH_FOR, RETRY_MAX, RETRY_MIN};

const REVISION_URL: &str =
    "https://api.github.com/repos/gaoyifan/china-operator-ip/git/ref/heads/ip-lists";
const RAW_BASE: &str = "https://raw.githubusercontent.com/gaoyifan/china-operator-ip";
const CACHE_FILE: &str = "geoip-china-operators-v1.json";
const MAX_LIST_BYTES: usize = 2 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 6 * MAX_LIST_BYTES;
const MAX_LIST_PREFIXES: usize = 65_536;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkOperator {
    Chinanet,
    Cmcc,
    Unicom,
    Cernet,
    Cstnet,
}

impl NetworkOperator {
    const ALL: [Self; 5] = [
        Self::Chinanet,
        Self::Cmcc,
        Self::Unicom,
        Self::Cernet,
        Self::Cstnet,
    ];

    fn slug(self) -> &'static str {
        match self {
            Self::Chinanet => "chinanet",
            Self::Cmcc => "cmcc",
            Self::Unicom => "unicom",
            Self::Cernet => "cernet",
            Self::Cstnet => "cstnet",
        }
    }

    fn bit(self) -> u8 {
        match self {
            Self::Chinanet => 1,
            Self::Cmcc => 2,
            Self::Unicom => 4,
            Self::Cernet => 8,
            Self::Cstnet => 16,
        }
    }
}

#[derive(Default, Clone)]
pub(super) struct OperatorLookup(Arc<RwLock<Option<Arc<OperatorDatabase>>>>);

impl OperatorLookup {
    pub(super) fn spawn(&self, http: reqwest::Client) {
        let worker = self.clone();
        tokio::spawn(async move {
            let path = cache_dir().join(CACHE_FILE);
            let mut loaded_at = match load_cached(&path).await {
                Ok((snapshot, at)) => match worker.publish(&snapshot).await {
                    Ok(()) => Some(at),
                    Err(error) => {
                        eprintln!("geoip: 运营商缓存无效：{error}");
                        None
                    }
                },
                Err(error) => {
                    eprintln!("geoip: 运营商缓存未就绪：{error}");
                    None
                }
            };
            let mut backoff = RETRY_MIN;
            loop {
                if let Some(at) = loaded_at {
                    let elapsed = age(at);
                    if elapsed < FRESH_FOR {
                        tokio::time::sleep(FRESH_FOR - elapsed).await;
                    }
                }
                let refreshed = async {
                    let snapshot = fetch(&http, REVISION_URL, RAW_BASE).await?;
                    worker.publish(&snapshot).await?;
                    if let Err(error) = write_cached(&path, &snapshot).await {
                        eprintln!("geoip: 保存运营商缓存失败：{error}");
                    }
                    Ok::<_, String>(())
                }
                .await;
                match refreshed {
                    Ok(()) => {
                        loaded_at = Some(SystemTime::now());
                        backoff = RETRY_MIN;
                    }
                    Err(error) => {
                        // Keep both the last published index and the last good disk snapshot.
                        eprintln!(
                            "geoip: 更新运营商库失败：{error}（{} 秒后重试）",
                            backoff.as_secs()
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(RETRY_MAX);
                    }
                }
            }
        });
    }

    async fn publish(&self, snapshot: &Snapshot) -> Result<(), String> {
        let database = OperatorDatabase::decode(snapshot)?;
        eprintln!("geoip: 运营商库已载入（版本 {}）", snapshot.revision);
        *self.0.write().await = Some(Arc::new(database));
        Ok(())
    }

    pub(super) async fn lookup(&self, addresses: &[String]) -> HashMap<String, NetworkOperator> {
        let Some(database) = self.0.read().await.clone() else {
            return HashMap::new();
        };
        addresses
            .iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .filter_map(|raw| {
                let ip = raw.parse::<IpAddr>().ok()?;
                database
                    .operator(ip)
                    .map(|operator| (raw.clone(), operator))
            })
            .collect()
    }
}

#[derive(Deserialize, Serialize)]
struct Snapshot {
    version: u8,
    revision: String,
    lists: BTreeMap<NetworkOperator, String>,
}

fn valid_revision(revision: &str) -> bool {
    revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn fetch(
    http: &reqwest::Client,
    revision_url: &str,
    raw_base: &str,
) -> Result<Snapshot, String> {
    #[derive(Deserialize)]
    struct Reference {
        object: Commit,
    }
    #[derive(Deserialize)]
    struct Commit {
        sha: String,
        #[serde(rename = "type")]
        kind: String,
    }
    let reference: Reference =
        serde_json::from_slice(&download(http, revision_url, 16 * 1024).await?)
            .map_err(|error| format!("读取运营商库版本：{error}"))?;
    if reference.object.kind != "commit" || !valid_revision(&reference.object.sha) {
        return Err("运营商库版本不是有效的 commit SHA".to_owned());
    }
    let revision = reference.object.sha;
    let mut lists = BTreeMap::new();
    for operator in NetworkOperator::ALL {
        let url = format!("{raw_base}/{revision}/{}46.txt", operator.slug());
        let text = String::from_utf8(download(http, &url, MAX_LIST_BYTES).await?)
            .map_err(|error| format!("运营商网段不是 UTF-8：{error}"))?;
        lists.insert(operator, text);
    }
    Ok(Snapshot {
        version: 1,
        revision,
        lists,
    })
}

async fn download(http: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let mut response = http
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| format!("下载运营商库：{error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("运营商库响应超过大小限制".to_owned());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("运营商库响应超过大小限制".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn load_cached(path: &Path) -> Result<(Snapshot, SystemTime), String> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|error| error.to_string())?;
    let metadata = file.metadata().await.map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_CACHE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_CACHE_BYTES {
        return Err("运营商缓存超过大小限制".to_owned());
    }
    let snapshot =
        serde_json::from_slice(&bytes).map_err(|error| format!("读取运营商缓存：{error}"))?;
    Ok((
        snapshot,
        metadata.modified().map_err(|error| error.to_string())?,
    ))
}

async fn write_cached(path: &Path, snapshot: &Snapshot) -> Result<(), String> {
    let bytes = serde_json::to_vec(snapshot).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_CACHE_BYTES {
        return Err("运营商缓存超过大小限制".to_owned());
    }
    let temporary = path.with_extension("json.part");
    tokio::fs::write(&temporary, bytes)
        .await
        .map_err(|error| error.to_string())?;
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error.to_string());
    }
    Ok(())
}

struct OperatorDatabase {
    v4: Vec<HashMap<u32, u8>>,
    v6: Vec<HashMap<u128, u8>>,
}

impl OperatorDatabase {
    fn decode(snapshot: &Snapshot) -> Result<Self, String> {
        if snapshot.version != 1 || !valid_revision(&snapshot.revision) {
            return Err("运营商缓存版本无效".to_owned());
        }
        let mut database = Self {
            v4: vec![HashMap::new(); 33],
            v6: vec![HashMap::new(); 129],
        };
        for operator in NetworkOperator::ALL {
            let text = snapshot
                .lists
                .get(&operator)
                .ok_or_else(|| format!("缺少 {} 网段", operator.slug()))?;
            if text.len() > MAX_LIST_BYTES {
                return Err(format!("{} 网段超过大小限制", operator.slug()));
            }
            let mut count = 0;
            let mut has_v4 = false;
            let mut has_v6 = false;
            for (index, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                count += 1;
                if count > MAX_LIST_PREFIXES {
                    return Err(format!("{} 网段数量超过限制", operator.slug()));
                }
                // Do not log untrusted file contents (or addresses) on malformed input.
                let invalid = || format!("{} 第 {} 行不是规范 CIDR", operator.slug(), index + 1);
                let (address, prefix) = line.split_once('/').ok_or_else(invalid)?;
                let address: IpAddr = address.parse().map_err(|_| invalid())?;
                let prefix: u32 = prefix.parse().map_err(|_| invalid())?;
                match address {
                    IpAddr::V4(ip) if (1..=32).contains(&prefix) => {
                        let address = u32::from(ip);
                        if address & prefix_mask(prefix) != address {
                            return Err(invalid());
                        }
                        *database.v4[prefix as usize].entry(address).or_default() |= operator.bit();
                        has_v4 = true;
                    }
                    IpAddr::V6(ip)
                        if (1..=128).contains(&prefix) && ip.to_ipv4_mapped().is_none() =>
                    {
                        let address = u128::from(ip);
                        if address & prefix_mask_v6(prefix) != address {
                            return Err(invalid());
                        }
                        *database.v6[prefix as usize].entry(address).or_default() |= operator.bit();
                        has_v6 = true;
                    }
                    _ => return Err(invalid()),
                }
            }
            // All maintained 46.txt lists contain both families; an empty/truncated family
            // must not replace a previously complete snapshot.
            if !has_v4 || !has_v6 {
                return Err(format!("{} 缺少 IPv4 或 IPv6 网段", operator.slug()));
            }
        }
        Ok(database)
    }

    fn operator(&self, ip: IpAddr) -> Option<NetworkOperator> {
        let matches = match ip {
            IpAddr::V4(ip) => self
                .v4
                .iter()
                .enumerate()
                .fold(0, |bits, (prefix, entries)| {
                    bits | entries
                        .get(&(u32::from(ip) & prefix_mask(prefix as u32)))
                        .copied()
                        .unwrap_or(0)
                }),
            IpAddr::V6(ip) => {
                if let Some(ip) = ip.to_ipv4_mapped() {
                    return self.operator(IpAddr::V4(ip));
                }
                self.v6
                    .iter()
                    .enumerate()
                    .fold(0, |bits, (prefix, entries)| {
                        bits | entries
                            .get(&(u128::from(ip) & prefix_mask_v6(prefix as u32)))
                            .copied()
                            .unwrap_or(0)
                    })
            }
        };
        // Upstream aggregates each operator independently, and multi-origin routes can overlap.
        // Even a longer prefix is not proof of exclusive attribution: union *all* matches.
        NetworkOperator::ALL
            .into_iter()
            .find(|operator| matches == operator.bit())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, http::StatusCode, routing::get, Router};
    use std::{sync::Mutex, time::Duration};

    fn fixture() -> Snapshot {
        Snapshot {
            version: 1,
            revision: "a".repeat(40),
            lists: NetworkOperator::ALL
                .into_iter()
                .enumerate()
                .map(|(index, operator)| {
                    (
                        operator,
                        format!("192.0.2.{index}/32\n2001:db8::{}/128\n", index + 1),
                    )
                })
                .collect(),
        }
    }

    #[tokio::test]
    #[ignore = "downloads the public operator feed; run explicitly to check upstream compatibility"]
    async fn live_operator_feed_decodes_at_one_revision() {
        let http = reqwest::Client::builder()
            .timeout(super::super::DOWNLOAD_TIMEOUT)
            .user_agent(concat!("brocade-console/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap();
        let snapshot = fetch(&http, REVISION_URL, RAW_BASE).await.unwrap();
        OperatorDatabase::decode(&snapshot).unwrap();
        let counts = snapshot
            .lists
            .iter()
            .map(|(operator, list)| (operator.slug(), list.lines().count()))
            .collect::<Vec<_>>();
        eprintln!(
            "operator feed revision={}, prefixes={counts:?}",
            snapshot.revision
        );
    }

    #[test]
    fn both_families_and_mapped_addresses_use_stable_operator_codes() {
        let database = OperatorDatabase::decode(&fixture()).unwrap();
        for (index, operator) in NetworkOperator::ALL.into_iter().enumerate() {
            for address in [
                format!("192.0.2.{index}"),
                format!("::ffff:192.0.2.{index}"),
                format!("2001:db8::{}", index + 1),
            ] {
                assert_eq!(database.operator(address.parse().unwrap()), Some(operator));
            }
            assert_eq!(serde_json::to_value(operator).unwrap(), operator.slug());
        }
        for address in ["192.0.2.255", "2001:db8::ffff", "127.0.0.1", "::1"] {
            assert_eq!(database.operator(address.parse().unwrap()), None);
        }
    }

    #[test]
    fn union_of_all_matching_prefixes_never_guesses_an_ambiguous_operator() {
        let mut snapshot = fixture();
        snapshot
            .lists
            .get_mut(&NetworkOperator::Chinanet)
            .unwrap()
            .push_str("203.0.113.0/24\n203.0.113.0/25\n2001:db8:1::/48\n2001:db8:1::/49\n");
        snapshot
            .lists
            .get_mut(&NetworkOperator::Cmcc)
            .unwrap()
            .push_str("203.0.113.0/25\n203.0.113.128/26\n2001:db8:1::/49\n2001:db8:1:8000::/50\n");
        let database = OperatorDatabase::decode(&snapshot).unwrap();
        for address in [
            "203.0.113.1",
            "203.0.113.129",
            "2001:db8:1::1",
            "2001:db8:1:8000::1",
        ] {
            assert_eq!(database.operator(address.parse().unwrap()), None);
        }
        for address in ["203.0.113.250", "2001:db8:1:ffff::1"] {
            assert_eq!(
                database.operator(address.parse().unwrap()),
                Some(NetworkOperator::Chinanet)
            );
        }
        // Multiple prefixes for the same operator alone are not an ambiguity.
        snapshot.lists.insert(
            NetworkOperator::Cmcc,
            fixture().lists[&NetworkOperator::Cmcc].clone(),
        );
        let database = OperatorDatabase::decode(&snapshot).unwrap();
        assert_eq!(
            database.operator("203.0.113.1".parse().unwrap()),
            Some(NetworkOperator::Chinanet)
        );
    }

    #[test]
    fn malformed_partial_and_unbounded_lists_are_rejected() {
        for bad in [
            "",
            "<html>error</html>",
            "0.0.0.0/0",
            "::/0",
            "192.0.2.1/24",
            "2001:db8::1/64",
            "192.0.2.0/33",
            "2001:db8::/129",
            "example.com/24",
            "::ffff:192.0.2.1/128",
            "192.0.2.0/24",
            "2001:db8::/32",
        ] {
            let mut snapshot = fixture();
            snapshot
                .lists
                .insert(NetworkOperator::Chinanet, bad.to_owned());
            assert!(OperatorDatabase::decode(&snapshot).is_err(), "{bad}");
        }
        let mut snapshot = fixture();
        snapshot.lists.remove(&NetworkOperator::Cmcc);
        assert!(OperatorDatabase::decode(&snapshot).is_err());
        let mut snapshot = fixture();
        snapshot.version = 2;
        assert!(OperatorDatabase::decode(&snapshot).is_err());
        let mut snapshot = fixture();
        snapshot.revision = "../ip-lists".to_owned();
        assert!(OperatorDatabase::decode(&snapshot).is_err());
        for text in [
            " ".repeat(MAX_LIST_BYTES + 1),
            "192.0.2.0/24\n".repeat(MAX_LIST_PREFIXES + 1),
        ] {
            let mut snapshot = fixture();
            snapshot.lists.insert(NetworkOperator::Chinanet, text);
            assert!(OperatorDatabase::decode(&snapshot).is_err());
        }
    }

    #[tokio::test]
    async fn lookup_is_local_and_failed_refresh_keeps_the_last_complete_snapshot() {
        let lookup = OperatorLookup::default();
        let addresses = [
            "192.0.2.0",
            "192.0.2.0",
            "2001:db8::2",
            "::ffff:192.0.2.0",
            "192.0.2.***",
            "example.com",
        ]
        .map(str::to_owned);
        assert!(lookup.lookup(&addresses).await.is_empty());
        lookup.publish(&fixture()).await.unwrap();
        let expected = HashMap::from([
            ("192.0.2.0".to_owned(), NetworkOperator::Chinanet),
            ("::ffff:192.0.2.0".to_owned(), NetworkOperator::Chinanet),
            ("2001:db8::2".to_owned(), NetworkOperator::Cmcc),
        ]);
        assert_eq!(lookup.lookup(&addresses).await, expected);
        let mut invalid = fixture();
        invalid.lists.remove(&NetworkOperator::Cstnet);
        assert!(lookup.publish(&invalid).await.is_err());
        assert_eq!(lookup.lookup(&addresses).await, expected);
    }

    #[tokio::test]
    async fn disk_snapshot_survives_restart_and_rejects_oversized_or_corrupt_cache() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CACHE_FILE);
        write_cached(&path, &fixture()).await.unwrap();
        let (snapshot, at) = load_cached(&path).await.unwrap();
        assert!(age(at) < Duration::from_secs(10));
        let lookup = OperatorLookup::default();
        lookup.publish(&snapshot).await.unwrap();
        assert_eq!(lookup.lookup(&["192.0.2.0".to_owned()]).await.len(), 1);
        assert!(!path.with_extension("json.part").exists());
        tokio::fs::write(&path, b"{broken").await.unwrap();
        assert!(load_cached(&path).await.is_err());
        tokio::fs::File::create(&path)
            .await
            .unwrap()
            .set_len(MAX_CACHE_BYTES as u64 + 1)
            .await
            .unwrap();
        assert!(load_cached(&path).await.is_err());
    }

    #[tokio::test]
    async fn download_pins_every_list_to_one_revision_and_rejects_http_or_size_errors() {
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let app = Router::new().route("/chunked", get(|| async {
            axum::body::Body::from_stream(tokio_stream::iter([
                Ok::<_, std::io::Error>("too"),
                Ok("large"),
            ]))
        })).fallback(get(
            |State(requests): State<Arc<Mutex<Vec<String>>>>, uri: axum::http::Uri| async move {
                let path = uri.path().to_owned();
                requests.lock().unwrap().push(path.clone());
                if path == "/ref" {
                    return (StatusCode::OK, serde_json::json!({"object": {"sha": "a".repeat(40), "type": "commit"}}).to_string());
                }
                if path == "/bad-ref" {
                    return (StatusCode::OK, serde_json::json!({"object": {"sha": "../../main", "type": "commit"}}).to_string());
                }
                for (operator, list) in fixture().lists {
                    if path == format!("/{}/{}46.txt", "a".repeat(40), operator.slug()) {
                        return (StatusCode::OK, list);
                    }
                }
                (StatusCode::SERVICE_UNAVAILABLE, String::new())
            },
        )).with_state(requests.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let snapshot = fetch(&http, &format!("{base}/ref"), &base).await.unwrap();
        OperatorDatabase::decode(&snapshot).unwrap();
        assert_eq!(requests.lock().unwrap().len(), 6);
        assert!(download(&http, &format!("{base}/missing"), 1024)
            .await
            .is_err());
        assert!(download(&http, &format!("{base}/ref"), 4).await.is_err());
        assert!(download(&http, &format!("{base}/chunked"), 4)
            .await
            .is_err());
        assert!(
            fetch(&http, &format!("{base}/ref"), &format!("{base}/missing"))
                .await
                .is_err()
        );
        requests.lock().unwrap().clear();
        assert!(fetch(&http, &format!("{base}/bad-ref"), &base)
            .await
            .is_err());
        assert_eq!(*requests.lock().unwrap(), ["/bad-ref"]);
        server.abort();
    }
}
