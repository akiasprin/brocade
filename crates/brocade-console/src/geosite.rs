//! Configuration previews use the same global geosite source as the managed nodes.
use std::{
    collections::BTreeMap,
    sync::Arc,
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
    async fn database(&self, source: &str) -> Result<Arc<Database>, String> {
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
    tokio::task::spawn_blocking(move || Database::decode(&bytes).map(Arc::new))
        .await
        .map_err(|_| "统一 geosite 规则集解析任务失败".to_owned())?
}

struct Database(BTreeMap<String, Site>);

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
        ))
    }

    fn check(&self, selector: &str, domain: &str) -> Check {
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
            let value = entry.value.to_ascii_lowercase();
            let hit = match entry.kind {
                0 => domain.contains(&value),
                1 => {
                    // Go's Perl character classes and word boundaries are ASCII. Targets are
                    // normalized ASCII hostnames; unsupported regex syntax remains unknown.
                    match RegexBuilder::new(&entry.value)
                        .unicode(false)
                        .size_limit(256 * 1024)
                        .build()
                    {
                        Ok(regex) => regex.is_match(domain.as_bytes()),
                        Err(_) => {
                            unknown = true;
                            false
                        }
                    }
                }
                2 => {
                    domain == value
                        || domain
                            .strip_suffix(&value)
                            .is_some_and(|prefix| prefix.ends_with('.'))
                }
                3 => domain == value,
                _ => {
                    unknown = true;
                    false
                }
            };
            if hit {
                return Check::known(true);
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
struct Site {
    #[prost(string, tag = "1")]
    code: String,
    #[prost(message, repeated, tag = "2")]
    domain: Vec<Domain>,
}
#[derive(Clone, PartialEq, Message)]
struct Domain {
    #[prost(int32, tag = "1")]
    kind: i32,
    #[prost(string, tag = "2")]
    value: String,
    #[prost(message, repeated, tag = "3")]
    attribute: Vec<Attribute>,
}
#[derive(Clone, PartialEq, Message)]
struct Attribute {
    #[prost(string, tag = "1")]
    key: String,
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
