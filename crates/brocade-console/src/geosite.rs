//! Configuration previews use the same global geosite source as the managed nodes.
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use prost::Message;
use regex::bytes::RegexBuilder;
use serde::Serialize;
use tokio::sync::Mutex;

const MAX_BYTES: usize = 64 * 1024 * 1024;
const FRESH_FOR: Duration = Duration::from_secs(24 * 60 * 60);
const RETRY_AFTER: Duration = Duration::from_secs(30);

#[derive(Clone, Default)]
pub(crate) struct Lookup {
    cache: Arc<Mutex<Option<Cache>>>,
}

struct Cache {
    source: String,
    loaded_at: Instant,
    database: Result<Arc<Database>, String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Check {
    pub hit: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Check {
    fn known(hit: bool) -> Self {
        Self {
            hit: Some(hit),
            reason: None,
        }
    }

    fn unknown(reason: impl Into<String>) -> Self {
        Self {
            hit: None,
            reason: Some(reason.into()),
        }
    }
}

impl Lookup {
    pub(crate) async fn database(&self, source: &str) -> Result<Arc<Database>, String> {
        // One in-flight download and one source per Console. A cancelled request releases the
        // lock without recording a failed attempt, so the next request can retry immediately.
        let mut cache = self.cache.lock().await;
        if let Some(cached) = cache.as_ref() {
            let ttl = if cached.database.is_ok() {
                FRESH_FOR
            } else {
                RETRY_AFTER
            };
            if cached.source == source && cached.loaded_at.elapsed() < ttl {
                return cached.database.clone();
            }
        }
        let database = download(source).await;
        *cache = Some(Cache {
            source: source.to_owned(),
            loaded_at: Instant::now(),
            database: database.clone(),
        });
        database
    }

    pub async fn matches(
        &self,
        source: &str,
        domain: &str,
        sets: &[String],
    ) -> Result<BTreeMap<String, Check>, String> {
        let database = self.database(source.trim()).await?;
        let domain = domain.to_ascii_lowercase();
        let sets = sets.to_vec();
        tokio::task::spawn_blocking(move || {
            sets.into_iter()
                .map(|set| {
                    let check = database.check(&set, &domain);
                    (set, check)
                })
                .collect()
        })
        .await
        .map_err(|_| "统一 geosite 规则集匹配任务失败".to_owned())
    }
}

async fn download(source: &str) -> Result<Arc<Database>, String> {
    let bytes = download_bytes(source).await?;
    tokio::task::spawn_blocking(move || Database::decode(&bytes).map(Arc::new))
        .await
        .map_err(|_| "规则集解析任务失败".to_owned())?
}

pub(crate) async fn download_bytes(source: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| "统一 geosite 规则集连接未就绪".to_owned())?;
    let mut response = client
        .get(source)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|_| "统一 geosite 规则集暂时无法加载，请稍后重试".to_owned())?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "统一 geosite 规则集读取失败".to_owned())?
    {
        if bytes.len() + chunk.len() > MAX_BYTES {
            return Err("统一 geosite 规则集超过 64 MiB".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Sites keyed by upper-case code, the load time, and containment computed on first use.
pub(crate) struct Database(
    pub BTreeMap<String, Site>,
    pub u64,
    OnceLock<BTreeMap<String, Vec<String>>>,
);

/// A category that contains the looked-up domain, with the first entry that matched it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Hit {
    pub code: String,
    pub count: usize,
    pub kind: &'static str,
    pub value: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct DomainLookup {
    pub domain: String,
    /// Narrowest first, so the most specific category leads.
    pub hits: Vec<Hit>,
    /// Categories with entries this preview cannot evaluate and no confirmed hit.
    pub unknown: Vec<String>,
}

pub(crate) fn kind_name(kind: i32) -> &'static str {
    match kind {
        0 => "keyword",
        1 => "regexp",
        2 => "domain",
        3 => "full",
        _ => "unknown",
    }
}

/// `None` when the entry needs the Xray engine to decide. `domain` must be lower-case ASCII.
pub(crate) fn entry_matches(entry: &Domain, domain: &str) -> Option<bool> {
    let value = entry.value.to_ascii_lowercase();
    match entry.kind {
        0 => Some(domain.contains(&value)),
        // Go's Perl character classes and word boundaries are ASCII. Targets are normalized
        // ASCII hostnames; unsupported regex syntax remains unknown.
        1 => RegexBuilder::new(&entry.value)
            .unicode(false)
            .size_limit(256 * 1024)
            .build()
            .ok()
            .map(|regex| regex.is_match(domain.as_bytes())),
        2 => Some(
            domain == value
                || domain
                    .strip_suffix(&value)
                    .is_some_and(|prefix| prefix.ends_with('.')),
        ),
        3 => Some(domain == value),
        _ => None,
    }
}

impl Database {
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let list = SiteList::decode(bytes).map_err(|_| "统一 geosite 规则集格式无效".to_owned())?;
        if list.entry.is_empty() {
            return Err("统一 geosite 规则集为空".to_owned());
        }
        Ok(Self(
            list.entry
                .into_iter()
                .map(|site| (site.code.to_ascii_uppercase(), site))
                .collect(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            OnceLock::new(),
        ))
    }

    /// For each lower-case code, the other codes whose entries include all of its entries,
    /// narrowest first. Entries compare like rule validation does: by kind and value, with
    /// non-regex values case-folded. Computed once per loaded file; call off the async runtime.
    pub(crate) fn within(&self) -> &BTreeMap<String, Vec<String>> {
        self.2.get_or_init(|| {
            let mut ids = HashMap::<(i32, String), u32>::new();
            let mut sets: Vec<(String, Vec<u32>)> = Vec::with_capacity(self.0.len());
            for site in self.0.values() {
                let mut set: Vec<u32> = site
                    .domain
                    .iter()
                    .map(|d| {
                        let value = if d.kind == 1 {
                            d.value.clone()
                        } else {
                            d.value.to_ascii_lowercase()
                        };
                        let next = u32::try_from(ids.len()).expect("dat is bounded to 64 MiB");
                        *ids.entry((d.kind, value)).or_insert(next)
                    })
                    .collect();
                set.sort_unstable();
                set.dedup();
                sets.push((site.code.to_ascii_lowercase(), set));
            }
            let mut postings = vec![Vec::<u32>::new(); ids.len()];
            for (index, (_, set)) in sets.iter().enumerate() {
                let index = u32::try_from(index).expect("site count fits u32");
                for id in set {
                    postings[*id as usize].push(index);
                }
            }
            let mut within = BTreeMap::new();
            for (index, (code, set)) in sets.iter().enumerate() {
                // Only sets holding the rarest entry can contain this one.
                let Some(rarest) = set.iter().min_by_key(|id| postings[**id as usize].len()) else {
                    continue;
                };
                let mut wider: Vec<_> = postings[*rarest as usize]
                    .iter()
                    .map(|other| *other as usize)
                    .filter(|other| *other != index)
                    .filter(|other| {
                        let wide = &sets[*other].1;
                        wide.len() >= set.len()
                            && set.iter().all(|id| wide.binary_search(id).is_ok())
                    })
                    .map(|other| (sets[other].1.len(), sets[other].0.clone()))
                    .collect();
                if wider.is_empty() {
                    continue;
                }
                wider.sort();
                within.insert(
                    code.clone(),
                    wider.into_iter().map(|(_, code)| code).collect(),
                );
            }
            within
        })
    }

    /// Every category containing `domain` (lower-case ASCII, validated by the caller).
    pub(crate) fn lookup(&self, domain: &str) -> DomainLookup {
        let mut hits = vec![];
        let mut unknown = vec![];
        for site in self.0.values() {
            let mut undecided = false;
            let hit = site
                .domain
                .iter()
                .find(|entry| match entry_matches(entry, domain) {
                    Some(hit) => hit,
                    None => {
                        undecided = true;
                        false
                    }
                });
            let code = site.code.to_ascii_lowercase();
            match hit {
                Some(entry) => hits.push(Hit {
                    code,
                    count: site.domain.len(),
                    kind: kind_name(entry.kind),
                    value: entry.value.clone(),
                }),
                None if undecided => unknown.push(code),
                None => {}
            }
        }
        hits.sort_by(|a, b| a.count.cmp(&b.count).then_with(|| a.code.cmp(&b.code)));
        DomainLookup {
            domain: domain.to_owned(),
            hits,
            unknown,
        }
    }

    pub(crate) fn check(&self, selector: &str, domain: &str) -> Check {
        let mut parts = selector.split('@');
        let code = parts.next().unwrap_or_default().to_ascii_uppercase();
        let attrs: Vec<_> = parts.map(str::to_ascii_lowercase).collect();
        let Some(site) = self.0.get(&code) else {
            return Check::unknown(format!("统一 geosite 规则集不含 {selector}"));
        };
        let mut unknown = false;
        for entry in &site.domain {
            // Xray filters by attribute presence, regardless of its bool/int value.
            if !attrs
                .iter()
                .all(|key| entry.attribute.iter().any(|attr| &attr.key == key))
            {
                continue;
            }
            match entry_matches(entry, domain) {
                Some(true) => return Check::known(true),
                Some(false) => {}
                None => unknown = true,
            }
        }
        if unknown {
            Check::unknown(format!("geosite {selector} 含需 Xray 引擎确认的条目"))
        } else {
            Check::known(false)
        }
    }
}

// Wire fields follow components/xray-core/common/geodata/geodat.proto. Attribute values are
// deliberately omitted: Xray's selector only tests whether each key is present.
#[derive(Clone, PartialEq, Message)]
struct SiteList {
    #[prost(message, repeated, tag = "1")]
    entry: Vec<Site>,
}
#[derive(Clone, PartialEq, Message)]
pub(crate) struct Site {
    #[prost(string, tag = "1")]
    pub code: String,
    #[prost(message, repeated, tag = "2")]
    pub domain: Vec<Domain>,
}
#[derive(Clone, PartialEq, Message)]
pub(crate) struct Domain {
    #[prost(int32, tag = "1")]
    pub kind: i32,
    #[prost(string, tag = "2")]
    pub value: String,
    #[prost(message, repeated, tag = "3")]
    pub attribute: Vec<Attribute>,
}
#[derive(Clone, PartialEq, Message)]
pub(crate) struct Attribute {
    #[prost(string, tag = "1")]
    pub key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Database {
        let site = Site {
            code: "TEST".into(),
            domain: vec![
                Domain {
                    kind: 0,
                    value: "keyword".into(),
                    attribute: vec![],
                },
                Domain {
                    kind: 1,
                    value: r"^regex\d+\.example$".into(),
                    attribute: vec![],
                },
                Domain {
                    kind: 2,
                    value: "suffix.example".into(),
                    attribute: vec![
                        Attribute { key: "cn".into() },
                        Attribute { key: "ads".into() },
                    ],
                },
                Domain {
                    kind: 3,
                    value: "full.example".into(),
                    attribute: vec![],
                },
            ],
        };
        Database::decode(&SiteList { entry: vec![site] }.encode_to_vec()).unwrap()
    }

    #[test]
    fn preview_pages_follow_kind_order_and_name_the_matching_entries() {
        let db = crate::geodata::Database::Site(std::sync::Arc::new(database()));
        fn kinds(page: &crate::geodata::Page) -> Vec<&str> {
            page.entries.iter().map(|e| e.kind.as_str()).collect()
        }
        let page = db.page("test", "", "", "", 0, 10, "").unwrap();
        assert_eq!(kinds(&page), ["domain", "full", "regexp", "keyword"]);
        assert_eq!(page.counts.values().sum::<usize>(), 4);
        assert_eq!((page.hit, page.hits.len()), (None, 0));

        let page = db.page("test", "", "", "", 1, 2, "").unwrap();
        assert_eq!(kinds(&page), ["full", "regexp"]);
        assert_eq!((page.total, page.next_offset), (4, Some(3)));

        let page = db.page("test", "", "full", "", 0, 10, "").unwrap();
        assert_eq!(kinds(&page), ["full"]);
        assert_eq!(page.counts.len(), 4, "counts ignore the kind filter");
        assert!(db.page("test", "", "ipv4", "", 0, 10, "").is_err());

        let page = db
            .page("test", "", "", "", 0, 10, "A.Suffix.Example")
            .unwrap();
        assert_eq!(page.hit, Some(true));
        assert_eq!(page.hits.len(), 1);
        assert_eq!(
            (page.hits[0].kind.as_str(), page.hits[0].value.as_str()),
            ("domain", "suffix.example")
        );
        let page = db
            .page("test", "cn", "", "", 0, 10, "full.example")
            .unwrap();
        assert_eq!((page.hit, page.hits.len(), page.total), (Some(false), 0, 1));
    }

    #[test]
    fn evaluates_all_geosite_domain_kinds_and_suffix_boundaries() {
        let db = database();
        for domain in [
            "a-keyword.example",
            "regex123.example",
            "suffix.example",
            "a.suffix.example",
            "full.example",
        ] {
            assert_eq!(db.check("test", domain).hit, Some(true), "{domain}");
        }
        for domain in [
            "unknown.example",
            "notsuffix.example",
            "a.full.example",
            "regex１２.example",
        ] {
            assert_eq!(db.check("TEST", domain).hit, Some(false), "{domain}");
        }
    }

    #[test]
    fn filters_attributes_and_reports_missing_sets_as_unknown() {
        let db = database();
        assert_eq!(db.check("test@CN@ADS", "a.suffix.example").hit, Some(true));
        assert_eq!(db.check("test@cn", "full.example").hit, Some(false));
        assert_eq!(db.check("test@missing", "suffix.example").hit, Some(false));
        assert_eq!(db.check("missing", "full.example").hit, None);
        assert!(Database::decode(b"not protobuf").is_err());
        assert!(Database::decode(b"").is_err());
    }

    fn sites(sites: &[(&str, &[(i32, &str)])]) -> Database {
        let entry = sites
            .iter()
            .map(|(code, domains)| Site {
                code: (*code).into(),
                domain: domains
                    .iter()
                    .map(|(kind, value)| Domain {
                        kind: *kind,
                        value: (*value).into(),
                        attribute: vec![],
                    })
                    .collect(),
            })
            .collect();
        Database::decode(&SiteList { entry }.encode_to_vec()).unwrap()
    }

    #[test]
    fn containment_includes_equal_sets_and_orders_narrowest_first() {
        let db = sites(&[
            ("NARROW", &[(2, "Example.com"), (1, "^A$")]),
            ("SAME", &[(2, "example.com"), (1, "^A$")]),
            (
                "WIDE",
                &[(2, "example.com"), (1, "^A$"), (3, "other.example")],
            ),
            // Regex values keep their case, so this does not contain NARROW.
            ("CASED", &[(2, "example.com"), (1, "^a$"), (3, "x.example")]),
            ("EMPTY", &[]),
        ]);
        let within = db.within();
        assert_eq!(within["narrow"], ["same", "wide"]);
        assert_eq!(within["same"], ["narrow", "wide"]);
        assert!(!within.contains_key("wide"));
        assert!(!within.contains_key("cased"));
        assert!(!within.contains_key("empty"));
        assert!(std::ptr::eq(within, db.within()));
    }

    #[test]
    fn domain_lookup_lists_every_containing_category_narrowest_first() {
        let db = sites(&[
            (
                "WIDE",
                &[(3, "x.example"), (2, "example.com"), (3, "y.example")],
            ),
            ("SERVICE", &[(2, "chat.example.com")]),
            ("WORD", &[(0, "chat")]),
            ("BROKEN", &[(1, "(")]),
            ("ELSEWHERE", &[(2, "example.org")]),
        ]);
        let found = db.lookup("api.chat.example.com");
        assert_eq!(
            found
                .hits
                .iter()
                .map(|h| (h.code.as_str(), h.count, h.kind, h.value.as_str()))
                .collect::<Vec<_>>(),
            [
                ("service", 1, "domain", "chat.example.com"),
                ("word", 1, "keyword", "chat"),
                ("wide", 3, "domain", "example.com"),
            ]
        );
        assert_eq!(found.unknown, ["broken"]);
        assert!(db.lookup("example.net").hits.is_empty());
    }

    #[tokio::test]
    async fn never_reuses_a_database_from_a_different_global_source() {
        let lookup = Lookup::default();
        *lookup.cache.lock().await = Some(Cache {
            source: "https://old.example/geosite.dat".into(),
            loaded_at: Instant::now(),
            database: Ok(Arc::new(database())),
        });
        assert_eq!(
            lookup
                .matches(
                    "https://old.example/geosite.dat",
                    "FULL.EXAMPLE",
                    &["test".into()]
                )
                .await
                .unwrap()["test"]
                .hit,
            Some(true)
        );
        assert!(lookup
            .matches("invalid-source", "full.example", &["test".into()])
            .await
            .is_err());
    }
}
