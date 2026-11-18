//! Same-source geodata catalogues, bounded previews and write-time validation.
use brocade_core::{
    model::{DestMatch, ModelSnapshot},
    rule_validation::{atoms, comparable, issue},
};
use prost::Message;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub(crate) struct Lookup {
    pub site: crate::geosite::Lookup,
    ip: Arc<Mutex<Option<IpCache>>>,
}
struct IpCache {
    source: String,
    at: Instant,
    loaded_at: u64,
    data: Result<Arc<IpList>, String>,
}
#[derive(Clone, Serialize)]
pub(crate) struct Category {
    pub code: String,
    pub count: usize,
    pub attrs: Vec<String>,
}
#[derive(Serialize)]
pub(crate) struct Catalog {
    pub source: String,
    pub loaded_at: u64,
    pub entries: Vec<Category>,
}
#[derive(Clone, Serialize)]
pub(crate) struct Entry {
    pub kind: String,
    pub value: String,
    pub attrs: Vec<String>,
}
#[derive(Serialize)]
pub(crate) struct Page {
    pub code: String,
    pub total: usize,
    pub counts: BTreeMap<String, usize>,
    pub entries: Vec<Entry>,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub hit: Option<bool>,
}
#[derive(Clone)]
pub(crate) enum Database {
    Site(Arc<crate::geosite::Database>),
    Ip(Arc<IpList>, u64),
}

impl Lookup {
    pub async fn database(&self, kind: &str, source: &str) -> Result<Database, String> {
        if kind == "geosite" {
            return self.site.database(source).await.map(Database::Site);
        }
        if kind != "geoip" {
            return Err("规则集类型无效".into());
        }
        let mut cache = self.ip.lock().await;
        if let Some(c) = cache.as_ref() {
            if c.source == source
                && c.at.elapsed() < Duration::from_secs(if c.data.is_ok() { 86400 } else { 30 })
            {
                return c.data.clone().map(|db| Database::Ip(db, c.loaded_at));
            }
        }
        let data = async {
            let bytes = crate::geosite::download_bytes(source).await?;
            tokio::task::spawn_blocking(move || {
                let mut list = IpList::decode(bytes.as_slice())
                    .map_err(|_| "geoip.dat 格式无效".to_owned())?;
                if list.entry.is_empty() {
                    return Err("geoip.dat 为空".to_owned());
                }
                for site in &mut list.entry {
                    site.code = site.code.to_ascii_lowercase();
                    for cidr in &site.cidr {
                        cidr.network()?;
                    }
                }
                Ok(Arc::new(list))
            })
            .await
            .map_err(|_| "geoip.dat 解析失败".to_owned())?
        }
        .await;
        let loaded_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        *cache = Some(IpCache {
            source: source.into(),
            at: Instant::now(),
            loaded_at,
            data: data.clone(),
        });
        data.map(|db| Database::Ip(db, loaded_at))
    }
}

impl Database {
    pub fn catalog(&self, source: &str) -> Catalog {
        let entries = match self {
            Self::Site(db) => {
                db.0.values()
                    .map(|s| Category {
                        code: s.code.to_ascii_lowercase(),
                        count: s.domain.len(),
                        attrs: s
                            .domain
                            .iter()
                            .flat_map(|d| d.attribute.iter().map(|a| a.key.to_ascii_lowercase()))
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect(),
                    })
                    .collect()
            }
            Self::Ip(db, _) => db
                .entry
                .iter()
                .map(|s| Category {
                    code: s.code.clone(),
                    count: s.cidr.len(),
                    attrs: ["ipv4", "ipv6"]
                        .into_iter()
                        .filter(|a| s.cidr.iter().any(|c| (c.ip.len() == 4) == (*a == "ipv4")))
                        .map(str::to_owned)
                        .collect(),
                })
                .collect(),
        };
        Catalog {
            source: display_source(source),
            loaded_at: match self {
                Self::Site(db) => db.1,
                Self::Ip(_, loaded_at) => *loaded_at,
            },
            entries,
        }
    }

    /// Identity sets use the already-expanded dat entries, including all selected attributes.
    /// Comparing these sets handles include relationships without inventing an upstream list.
    fn identities(&self, selector: &str) -> Result<BTreeSet<String>, String> {
        let (code, attrs) = selector_parts(selector);
        match self {
            Self::Site(db) => {
                let site =
                    db.0.get(&code.to_ascii_uppercase())
                        .ok_or_else(|| format!("{code} 不存在"))?;
                let values: BTreeSet<_> = site
                    .domain
                    .iter()
                    .filter(|d| {
                        attrs
                            .iter()
                            .all(|a| d.attribute.iter().any(|v| v.key.eq_ignore_ascii_case(a)))
                    })
                    .map(|d| {
                        format!(
                            "{}:{}",
                            d.kind,
                            if d.kind == 1 {
                                d.value.clone()
                            } else {
                                d.value.to_ascii_lowercase()
                            }
                        )
                    })
                    .collect();
                if values.is_empty() && !attrs.is_empty() {
                    return Err(format!("{code} 无 @{}", attrs.join("@")));
                }
                Ok(values)
            }
            Self::Ip(db, _) => {
                let inverted = code.bytes().take_while(|b| *b == b'!').count() % 2 == 1;
                let code = code.trim_start_matches('!');
                let site = db
                    .entry
                    .iter()
                    .find(|s| s.code.eq_ignore_ascii_case(code))
                    .ok_or_else(|| format!("{code} 不存在"))?;
                let values: BTreeSet<_> = site
                    .cidr
                    .iter()
                    .filter(|c| {
                        attrs.iter().all(|a| {
                            (a == "ipv4" && c.ip.len() == 4) || (a == "ipv6" && c.ip.len() == 16)
                        })
                    })
                    .map(|c| {
                        format!(
                            "{}:{}",
                            site.reverse_match ^ inverted,
                            c.network().expect("CIDRs validated during decode")
                        )
                    })
                    .collect();
                if values.is_empty() && !attrs.is_empty() {
                    return Err(format!("{code} 无 @{}", attrs.join("@")));
                }
                Ok(values)
            }
        }
    }

    fn reversed(&self, selector: &str) -> bool {
        if let Self::Ip(db, _) = self {
            let (code, _) = selector_parts(selector);
            let inverted = code.bytes().take_while(|b| *b == b'!').count() % 2 == 1;
            return db
                .entry
                .iter()
                .find(|s| s.code.eq_ignore_ascii_case(code.trim_start_matches('!')))
                .is_some_and(|s| s.reverse_match ^ inverted);
        }
        false
    }

    pub fn relations(&self, values: &[String]) -> serde_json::Value {
        let mut errors = BTreeMap::new();
        let mut sets = BTreeMap::new();
        for value in values {
            match self.identities(value) {
                Ok(set) => {
                    sets.insert(value, set);
                }
                Err(error) => {
                    errors.insert(value, error);
                }
            }
        }
        let mut covers = vec![];
        for (narrower, set) in &sets {
            if set.is_empty() {
                continue;
            }
            for (wider, wide) in &sets {
                if narrower != wider
                    && set.is_subset(wide)
                    && ((!self.reversed(narrower) && !self.reversed(wider)) || set == wide)
                {
                    covers.push(serde_json::json!({"wider":wider,"narrower":narrower}));
                }
            }
        }
        serde_json::json!({"covers":covers,"errors":errors})
    }

    pub fn page(
        &self,
        code: &str,
        attr: &str,
        query: &str,
        offset: usize,
        limit: usize,
        target: &str,
    ) -> Result<Page, String> {
        let selector = if attr.is_empty() {
            code.into()
        } else {
            format!("{code}@{attr}")
        };
        self.identities(&selector)?;
        if matches!(self, Self::Site(_))
            && !target.is_empty()
            && !brocade_core::rule_validation::valid_domain(target)
        {
            return Err("请输入有效域名".into());
        }
        let (_, attrs) = selector_parts(&selector);
        let mut counts = BTreeMap::new();
        let mut entries = Vec::new();
        let mut total = 0;
        let mut hit = if target.is_empty() { None } else { Some(false) };
        let query = query.to_ascii_lowercase();
        let target_ip = if target.is_empty() {
            None
        } else {
            target.parse::<IpAddr>().ok()
        };
        if matches!(self, Self::Ip(..)) && !target.is_empty() && target_ip.is_none() {
            return Err("请输入有效 IP".into());
        }
        let mut add = |entry: Entry, matched: bool| {
            *counts.entry(entry.kind.clone()).or_insert(0) += 1;
            if matched {
                hit = Some(true);
            }
            if !entry.value.to_ascii_lowercase().contains(&query) {
                return;
            }
            if total >= offset && entries.len() < limit {
                entries.push(entry);
            }
            total += 1;
        };
        match self {
            Self::Site(db) => {
                let site = &db.0[&code.to_ascii_uppercase()];
                for d in &site.domain {
                    if !attrs
                        .iter()
                        .all(|a| d.attribute.iter().any(|v| v.key.eq_ignore_ascii_case(a)))
                    {
                        continue;
                    }
                    let matched = if target.is_empty() {
                        false
                    } else {
                        match d.kind {
                            0 => target
                                .to_ascii_lowercase()
                                .contains(&d.value.to_ascii_lowercase()),
                            1 => regex::bytes::RegexBuilder::new(&d.value)
                                .unicode(false)
                                .build()
                                .map(|r| r.is_match(target.as_bytes()))
                                .unwrap_or(false),
                            2 => {
                                target.eq_ignore_ascii_case(&d.value)
                                    || target
                                        .to_ascii_lowercase()
                                        .ends_with(&format!(".{}", d.value.to_ascii_lowercase()))
                            }
                            3 => target.eq_ignore_ascii_case(&d.value),
                            _ => false,
                        }
                    };
                    add(
                        Entry {
                            kind: match d.kind {
                                0 => "keyword",
                                1 => "regexp",
                                2 => "domain",
                                3 => "full",
                                _ => "unknown",
                            }
                            .into(),
                            value: d.value.clone(),
                            attrs: d.attribute.iter().map(|a| a.key.clone()).collect(),
                        },
                        matched,
                    );
                }
            }
            Self::Ip(db, _) => {
                let inverted = code.bytes().take_while(|b| *b == b'!').count() % 2 == 1;
                let code = code.trim_start_matches('!');
                let site = db
                    .entry
                    .iter()
                    .find(|s| s.code.eq_ignore_ascii_case(code))
                    .expect("selector checked above");
                for c in &site.cidr {
                    if !attrs.iter().all(|a| {
                        (a == "ipv4" && c.ip.len() == 4) || (a == "ipv6" && c.ip.len() == 16)
                    }) {
                        continue;
                    }
                    let network = c.network()?;
                    add(
                        Entry {
                            kind: if c.ip.len() == 4 { "ipv4" } else { "ipv6" }.into(),
                            value: network.to_string(),
                            attrs: vec![],
                        },
                        target_ip.is_some_and(|ip| network.contains(&ip)),
                    );
                }
                if (site.reverse_match ^ inverted) && !target.is_empty() {
                    hit = hit.map(|v| !v);
                }
            }
        }
        if let Self::Site(db) = self {
            if !target.is_empty() {
                hit = db.check(&selector, target).hit;
            }
        }
        Ok(Page {
            code: code.into(),
            total,
            counts,
            next_offset: (offset + entries.len() < total).then_some(offset + entries.len()),
            entries,
            offset,
            hit,
        })
    }
}

fn display_source(source: &str) -> String {
    match reqwest::Url::parse(source) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => "已配置来源".into(),
    }
}

fn selector_parts(selector: &str) -> (String, Vec<String>) {
    let mut parts = selector.split('@');
    (
        parts.next().unwrap_or_default().to_ascii_lowercase(),
        parts.map(str::to_ascii_lowercase).collect(),
    )
}

pub(crate) fn selectors(snapshot: &ModelSnapshot) -> Vec<(String, Vec<DestMatch>)> {
    let mut tables = vec![];
    for app in &snapshot.apps {
        for step in &app.steps {
            tables.push((
                format!("{}/{}/{}", app.id, step.chain, step.node),
                step.rules.iter().map(|r| r.dest_match.clone()).collect(),
            ));
        }
    }
    // DNS selection is independently ordered per machine, never compared with routing rules.
    let mut dns = BTreeMap::<String, Vec<_>>::new();
    let mut policies = snapshot.node_egress_dns.iter().collect::<Vec<_>>();
    policies.sort_by_key(|p| (&p.node, p.position));
    for p in policies {
        dns.entry(format!("dns/{}", p.node))
            .or_default()
            .push(p.selector.clone());
    }
    tables.extend(dns);
    tables
}

pub(crate) async fn validate(
    lookup: &Lookup,
    before: &ModelSnapshot,
    after: &ModelSnapshot,
    touched: &BTreeSet<String>,
) -> Result<(), brocade_store::StoreError> {
    let old = selectors(before).into_iter().collect::<BTreeMap<_, _>>();
    let tables = selectors(after);
    let mut issues = vec![];
    for kind in ["geosite", "geoip"] {
        let source = if kind == "geosite" {
            &after.settings.geodata.geosite_url
        } else {
            &after.settings.geodata.geoip_url
        };
        let old_source = if kind == "geosite" {
            &before.settings.geodata.geosite_url
        } else {
            &before.settings.geodata.geoip_url
        };
        let affected: Vec<_> = tables
            .iter()
            .filter(|(location, _)| touched.contains(location) || source != old_source)
            .collect();
        if source == old_source
            && !affected.iter().any(|(_, rules)| {
                rules
                    .iter()
                    .any(|r| atoms(r).iter().any(|(k, _)| *k == kind))
            })
        {
            continue;
        }
        let database = lookup.database(kind, source).await;
        if database.is_err() && source != old_source {
            let mut error = issue("settings", None, "", "规则集无法加载");
            error.field = format!("geodata.{kind}_url");
            issues.push(error);
            continue;
        }
        for (location, rules) in affected {
            let previous = old.get(location);
            let old_values: BTreeSet<_> = previous
                .into_iter()
                .flatten()
                .flat_map(atoms)
                .filter(|(k, _)| *k == kind)
                .map(|(_, v)| v.to_ascii_lowercase())
                .collect();
            let mut identities = BTreeMap::new();
            for (i, rule) in rules.iter().enumerate() {
                for (_, value) in atoms(rule).into_iter().filter(|(k, _)| *k == kind) {
                    match &database {
                        Err(_) => {
                            if source != old_source
                                || !old_values.contains(&value.to_ascii_lowercase())
                            {
                                issues.push(issue(location, Some(i), value, "规则集未加载"));
                            }
                        }
                        Ok(db) => match db.identities(value) {
                            Err(message) => issues.push(issue(location, Some(i), value, message)),
                            Ok(set) => {
                                identities.insert(value.to_owned(), set);
                            }
                        },
                    }
                }
            }
            for (i, rule) in rules.iter().enumerate() {
                for (_, value) in atoms(rule).into_iter().filter(|(k, _)| *k == kind) {
                    let Some(set) = identities.get(value) else {
                        continue;
                    };
                    if set.is_empty() {
                        continue;
                    }
                    for (j, other) in rules[..=i].iter().enumerate() {
                        if !comparable(other, rule, kind) {
                            continue;
                        }
                        for (_, wider) in atoms(other).into_iter().filter(|(k, _)| *k == kind) {
                            if j == i && wider == value {
                                continue;
                            }
                            let Some(wide) = identities.get(wider) else {
                                continue;
                            };
                            // Equal sets in one OR-list keep its first token, rather than marking both.
                            let is_first = j < i
                                || atoms(rule).iter().position(|(_, v)| *v == wider)
                                    < atoms(rule).iter().position(|(_, v)| *v == value);
                            let comparable_direction = database.as_ref().is_ok_and(|db| {
                                (!db.reversed(value) && !db.reversed(wider)) || set == wide
                            });
                            if comparable_direction
                                && set.is_subset(wide)
                                && (set != wide || is_first)
                            {
                                issues.push(issue(
                                    location,
                                    Some(i),
                                    value,
                                    if j == i {
                                        "本条已包含".into()
                                    } else {
                                        format!("规则 {} 已包含", j + 1)
                                    },
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(brocade_store::StoreError::RuleValidation(issues))
    }
}

#[derive(Clone, PartialEq, Message)]
pub(crate) struct IpList {
    #[prost(message, repeated, tag = "1")]
    entry: Vec<IpSite>,
}
#[derive(Clone, PartialEq, Message)]
struct IpSite {
    #[prost(string, tag = "1")]
    code: String,
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
impl Cidr {
    fn network(&self) -> Result<ipnet::IpNet, String> {
        let ip: IpAddr = match self.ip.as_slice() {
            [a, b, c, d] => std::net::Ipv4Addr::new(*a, *b, *c, *d).into(),
            v if v.len() == 16 => {
                std::net::Ipv6Addr::from(<[u8; 16]>::try_from(v).expect("length checked")).into()
            }
            _ => return Err("geoip.dat 地址无效".into()),
        };
        ipnet::IpNet::new(
            ip,
            u8::try_from(self.prefix).map_err(|_| "geoip.dat 前缀无效")?,
        )
        .map(|n| n.trunc())
        .map_err(|_| "geoip.dat 前缀无效".into())
    }
}
