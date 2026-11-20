//! User routing preferences are applied only while rendering a client subscription.
//! They never enter ModelSnapshot, revisions, or node desired state.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RuleKind {
    Domain,
    Keyword,
    Ip,
    Process,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomRule {
    pub id: String,
    pub kind: RuleKind,
    pub value: String,
    pub target: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModulePreference {
    pub on: bool,
    pub target: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RulePreferences {
    pub custom: Vec<CustomRule>,
    pub modules: BTreeMap<String, ModulePreference>,
    pub extra: Vec<String>,
    #[serde(rename = "final")]
    pub final_target: String,
}
impl Default for RulePreferences {
    fn default() -> Self {
        Self {
            custom: vec![],
            modules: BTreeMap::new(),
            extra: vec![],
            final_target: "select".into(),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct RoutingModule {
    pub id: String,
    pub name: String,
    pub yaml: String,
    pub standard: String,
    pub sets: Vec<String>,
    pub locked: bool,
    pub block_only: bool,
    pub catalog: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct RoutingLine {
    pub id: String,
    pub name: String,
    pub yaml: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct RoutingCatalog {
    pub modules: Vec<RoutingModule>,
    pub lines: Vec<RoutingLine>,
    pub select: String,
    pub auto: String,
    pub final_group: String,
}

const EXTRA_MODULES: &[(&str, &str)] = &[
    ("netflix", "🎯 Netflix"),
    ("disney", "🎬 Disney+"),
    ("spotify", "🎵 Spotify"),
    ("tiktok", "🎶 TikTok"),
    ("twitter", "𝕏 X"),
    ("instagram", "📷 Instagram"),
    ("discord", "💬 Discord"),
    ("reddit", "🔴 Reddit"),
    ("twitch", "🎮 Twitch"),
    ("whatsapp", "💬 WhatsApp"),
    ("steam", "🎮 Steam"),
];
fn unique_name(base: &str, reserved: &mut BTreeSet<String>) -> String {
    let mut name = base.to_owned();
    let mut suffix = 1;
    while reserved.contains(&name) {
        suffix += 1;
        name = format!("{base} · Brocade {suffix}");
    }
    reserved.insert(name.clone());
    name
}
fn default_target(module: &StandardModule) -> &'static str {
    match module.kind {
        StandardGroupKind::DirectFirst => "direct",
        StandardGroupKind::RejectFirst => "reject",
        _ => "select",
    }
}
pub fn routing_catalog(subscription: &Subscription) -> RoutingCatalog {
    let subscription = clash_compatible_subscription(subscription);
    let names = standard_group_names(&subscription);
    let mut reserved = subscription
        .entries
        .iter()
        .map(|x| x.name.clone())
        .chain(subscription.external_proxies.iter().map(|x| x.name.clone()))
        .chain(subscription.front_groups.iter().map(|x| x.name.clone()))
        .chain(names.iter().cloned())
        .collect();
    let mut modules: Vec<_> = RULE_MODULE_ORDER
        .iter()
        .map(|id| {
            let module = STANDARD_MODULES
                .iter()
                .find(|m| m.id == *id)
                .expect("standard routing module exists");
            RoutingModule {
                id: (*id).into(),
                name: module
                    .name
                    .split_once(' ')
                    .map_or(module.name, |(_, name)| name)
                    .into(),
                yaml: standard_name(&names, id).into(),
                standard: default_target(module).into(),
                sets: module.rules.iter().map(|r| r.id.into()).collect(),
                locked: matches!(*id, "private" | "cn"),
                block_only: *id == "ad",
                catalog: false,
            }
        })
        .collect();
    for (id, name) in EXTRA_MODULES {
        modules.push(RoutingModule {
            id: (*id).into(),
            name: name.split_once(' ').map_or(*name, |(_, name)| name).into(),
            yaml: unique_name(name, &mut reserved),
            standard: "select".into(),
            sets: vec![(*id).into()],
            locked: false,
            block_only: false,
            catalog: true,
        });
    }
    let lines = subscription
        .entries
        .iter()
        .map(|entry| RoutingLine {
            id: format!("line:{}", entry.name),
            name: entry.name.clone(),
            // Mihomo splits rule fields on ASCII commas even inside a quoted YAML scalar.
            // Keep the original proxy label, but make its rule-facing group name safe.
            yaml: unique_name(
                &format!("📍 {}", entry.name.replace(',', "，")),
                &mut reserved,
            ),
        })
        .collect();
    RoutingCatalog {
        modules,
        lines,
        select: standard_name(&names, "select").into(),
        auto: standard_name(&names, "auto").into(),
        final_group: standard_name(&names, "final").into(),
    }
}
impl RulePreferences {
    pub fn validate(&self, catalog: &RoutingCatalog) -> Result<(), String> {
        if self.custom.len() > 200
            || self.extra.len() > EXTRA_MODULES.len()
            || self.modules.len() > catalog.modules.len()
        {
            return Err("规则数量超出限制".into());
        }
        let valid_target = |target: &str| {
            matches!(target, "select" | "auto" | "direct" | "reject")
                || catalog.lines.iter().any(|line| line.id == target)
        };
        if !valid_target(&self.final_target) {
            return Err("兜底去向已不可用，请重新选择".into());
        }
        let mut seen = BTreeSet::new();
        for id in &self.extra {
            if !EXTRA_MODULES.iter().any(|(key, _)| key == id) || !seen.insert(id) {
                return Err("服务目录包含无效或重复项目".into());
            }
        }
        for (id, state) in &self.modules {
            let module = catalog
                .modules
                .iter()
                .find(|m| m.id == *id)
                .ok_or("分流不存在")?;
            if module.catalog && !self.extra.contains(id) {
                return Err("请先添加目录服务".into());
            }
            if (module.locked && !state.on)
                || !valid_target(&state.target)
                || (module.block_only && !matches!(state.target.as_str(), "direct" | "reject"))
            {
                return Err("分流设置无效或线路已不可用".into());
            }
        }
        let mut ids = BTreeSet::new();
        for rule in &self.custom {
            if rule.id.is_empty()
                || rule.id.len() > 80
                || !ids.insert(&rule.id)
                || !valid_target(&rule.target)
            {
                return Err("自定义规则编号重复或去向已不可用".into());
            }
            let value = rule.value.trim();
            if value.is_empty()
                || value.len() > 253
                || (value.contains(',') || value.chars().any(char::is_control))
                || value != rule.value
            {
                return Err("规则内容为空、过长或包含分隔符".into());
            }
            match rule.kind {
                RuleKind::Domain if !valid_domain(value) => return Err("域名格式无效".into()),
                RuleKind::Ip if value.parse::<ipnet::IpNet>().is_err() => {
                    return Err("IP 段格式无效".into())
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub fn is_standard(&self) -> bool {
        self.custom.is_empty()
            && self.extra.is_empty()
            && self.final_target == "select"
            && self.modules.iter().all(|(id, state)| {
                STANDARD_MODULES
                    .iter()
                    .find(|m| m.id == id)
                    .is_some_and(|module| state.on && state.target == default_target(module))
            })
    }
}
fn valid_domain(value: &str) -> bool {
    value.len() <= 253
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}
fn resolve_target(target: &str, catalog: &RoutingCatalog) -> String {
    match target {
        "select" => catalog.select.clone(),
        "auto" => catalog.auto.clone(),
        "direct" => "DIRECT".into(),
        "reject" => "REJECT".into(),
        _ => catalog
            .lines
            .iter()
            .find(|line| line.id == target)
            .map_or_else(|| catalog.select.clone(), |line| line.yaml.clone()),
    }
}
pub fn routing_module_order<'a>(
    preferences: &RulePreferences,
    catalog: &'a RoutingCatalog,
) -> Vec<&'a RoutingModule> {
    let mut order = Vec::new();
    for id in RULE_MODULE_ORDER {
        if *id == "global" {
            for extra in &preferences.extra {
                if let Some(module) = catalog.modules.iter().find(|m| m.id == *extra) {
                    order.push(module);
                }
            }
        }
        order.push(
            catalog
                .modules
                .iter()
                .find(|m| m.id == *id)
                .expect("standard catalog module exists"),
        );
    }
    order
}
/// Uses current subscription names, not the names present when the preference was saved.
/// Removed lines fall back to node selection at pull time; saving a stale line is rejected.
pub fn clash_subscription_with_rules(
    subscription: &Subscription,
    preferences: &RulePreferences,
) -> String {
    if preferences.is_standard() {
        return clash_subscription(subscription);
    }
    let compatible = clash_compatible_subscription(subscription);
    let catalog = routing_catalog(&compatible);
    let mut content = clash_subscription(&compatible);
    let index = content
        .find("\nproxy-groups:\n")
        .expect("standard formatter writes proxy groups");
    content.truncate(index);
    let mut lines = vec![content, "\nproxy-groups:".into()];
    for group in &compatible.front_groups {
        lines.push(format!(
            "  - name: {}\n    type: {}\n    proxies: {}",
            yaml_quote(&group.name),
            group.strategy.as_str(),
            inline_list(&group.members)
        ));
    }
    let mut defaults = BTreeMap::new();
    for (id, state) in &preferences.modules {
        defaults.insert(id.clone(), resolve_target(&state.target, &catalog));
    }
    defaults.insert(
        "final".into(),
        resolve_target(&preferences.final_target, &catalog),
    );
    push_standard_groups_with_defaults(
        &mut lines,
        &compatible,
        &standard_group_names(&compatible),
        &defaults,
    );
    let mut used = BTreeSet::new();
    used.extend(preferences.custom.iter().map(|r| r.target.as_str()));
    used.extend(preferences.modules.values().map(|r| r.target.as_str()));
    used.insert(&preferences.final_target);
    for line in &catalog.lines {
        if used.contains(line.id.as_str()) {
            lines.push(format!(
                "  - name: {}\n    type: select\n    proxies: {}",
                yaml_quote(&line.yaml),
                inline_list(std::slice::from_ref(&line.name))
            ));
        }
    }
    for module in catalog
        .modules
        .iter()
        .filter(|m| m.catalog && preferences.extra.contains(&m.id))
    {
        let target = preferences
            .modules
            .get(&module.id)
            .map_or("select", |s| s.target.as_str());
        let mut members = vec![
            resolve_target(target, &catalog),
            catalog.select.clone(),
            catalog.auto.clone(),
            "DIRECT".into(),
            "REJECT".into(),
        ];
        deduplicate(&mut members);
        lines.push(format!(
            "  - name: {}\n    type: select\n    proxies: {}",
            yaml_quote(&module.yaml),
            inline_list(&members)
        ));
    }
    push_rule_providers(&mut lines);
    for module in catalog
        .modules
        .iter()
        .filter(|m| m.catalog && preferences.extra.contains(&m.id))
    {
        lines.push(format!("  {}:\n    type: http\n    behavior: domain\n    format: mrs\n    url: {}\n    path: {}\n    interval: 86400",yaml_quote(&module.id),yaml_quote(&format!("{RULE_PROVIDER_BASE_URL}/geosite/{}.mrs",module.id)),yaml_quote(&format!("./ruleset/{}.mrs",module.id))));
    }
    lines.push("\nrules:".into());
    for rule in &preferences.custom {
        let kind = match rule.kind {
            RuleKind::Domain => "DOMAIN-SUFFIX",
            RuleKind::Keyword => "DOMAIN-KEYWORD",
            RuleKind::Process => "PROCESS-NAME",
            RuleKind::Ip if rule.value.contains(':') => "IP-CIDR6",
            RuleKind::Ip => "IP-CIDR",
        };
        let value = format!(
            "{kind},{},{target}{}",
            rule.value,
            if matches!(rule.kind, RuleKind::Ip) {
                ",no-resolve"
            } else {
                ""
            },
            target = resolve_target(&rule.target, &catalog)
        );
        lines.push(format!("  - {}", yaml_quote(&value)));
    }
    for module in routing_module_order(preferences, &catalog) {
        if preferences.modules.get(&module.id).is_some_and(|s| !s.on) {
            continue;
        }
        for id in &module.sets {
            let no_resolve = STANDARD_MODULES
                .iter()
                .flat_map(|m| m.rules)
                .find(|r| r.id == id)
                .is_some_and(|r| r.no_resolve);
            lines.push(format!(
                "  - RULE-SET,{id},{}{}",
                module.yaml,
                if no_resolve { ",no-resolve" } else { "" }
            ));
        }
    }
    lines.push(format!(
        "  - RULE-SET,cn,{}",
        catalog
            .modules
            .iter()
            .find(|m| m.id == "cn")
            .expect("cn exists")
            .yaml
    ));
    lines.push(format!("  - MATCH,{}", catalog.final_group));
    format!("{}\n", lines.join("\n"))
}

pub fn custom_rule_matches(rule: &CustomRule, kind: &RuleKind, value: &str) -> bool {
    match (&rule.kind, kind) {
        (RuleKind::Domain, RuleKind::Domain) => {
            value.eq_ignore_ascii_case(&rule.value)
                || value
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{}", rule.value.to_ascii_lowercase()))
        }
        (RuleKind::Keyword, RuleKind::Domain) => value
            .to_ascii_lowercase()
            .contains(&rule.value.to_ascii_lowercase()),
        (RuleKind::Process, RuleKind::Process) => value == rule.value,
        (RuleKind::Ip, RuleKind::Ip) => rule
            .value
            .parse::<ipnet::IpNet>()
            .ok()
            .zip(value.parse::<std::net::IpAddr>().ok())
            .is_some_and(|(net, ip)| net.contains(&ip)),
        _ => false,
    }
}
/// Domain rule sets of the curated subscription modules, in module order. Rule editors offer
/// them as the common geosite choices so both surfaces share one list of featured services.
pub fn featured_geosite_sets() -> Vec<&'static str> {
    let mut sets = Vec::new();
    let standard = RULE_MODULE_ORDER.iter().flat_map(|id| {
        STANDARD_MODULES
            .iter()
            .filter(move |module| module.id == *id)
            .flat_map(|module| module.rules)
            .filter(|rule| rule.behavior == "domain")
            .map(|rule| rule.id)
    });
    for id in standard.chain(EXTRA_MODULES.iter().map(|(id, _)| *id)) {
        if !sets.contains(&id) {
            sets.push(id);
        }
    }
    sets
}
/// Fixed source text for the same revision that produces the subscription's MRS providers.
pub fn rule_source_url(id: &str, ip: bool) -> Option<String> {
    let kind = if ip { "ipcidr" } else { "domain" };
    STANDARD_MODULES
        .iter()
        .flat_map(|module| module.rules)
        .chain(std::iter::once(&EXPERIMENTAL_CN_RULE))
        .find(|rule| rule.id == id && rule.behavior == kind)
        .map(|rule| {
            format!(
                "{RULE_PROVIDER_BASE_URL}/{}",
                rule.path.replace(".mrs", ".list")
            )
        })
        .or_else(|| {
            (!ip && EXTRA_MODULES.iter().any(|(key, _)| *key == id))
                .then(|| format!("{RULE_PROVIDER_BASE_URL}/geosite/{id}.list"))
        })
}
pub fn source_rule_matches(pattern: &str, kind: &RuleKind, value: &str) -> bool {
    if matches!(kind, RuleKind::Ip) {
        return pattern
            .parse::<ipnet::IpNet>()
            .ok()
            .zip(value.parse::<std::net::IpAddr>().ok())
            .is_some_and(|(net, ip)| net.contains(&ip));
    }
    if !matches!(kind, RuleKind::Domain) {
        return false;
    }
    let pattern = pattern.trim().to_ascii_lowercase();
    let value = value.to_ascii_lowercase();
    if let Some(suffix) = pattern
        .strip_prefix("+.")
        .filter(|suffix| !suffix.contains('*'))
    {
        return value == suffix || value.ends_with(&format!(".{suffix}"));
    }
    if !pattern.contains('*') && !pattern.starts_with('.') {
        return pattern == value;
    }
    // Mihomo DomainTrie: '*' is one label; a leading '.' is subdomains only.
    // https://github.com/MetaCubeX/mihomo/blob/Meta/component/trie/domain.go
    let (suffix, include_root, subdomains) = if let Some(rest) = pattern.strip_prefix("+.") {
        (rest, true, true)
    } else if let Some(rest) = pattern.strip_prefix('.') {
        (rest, false, true)
    } else {
        (pattern.as_str(), true, false)
    };
    let labels: Vec<_> = value.split('.').collect();
    let wanted: Vec<_> = suffix.split('.').collect();
    let length_ok = if subdomains {
        if include_root {
            labels.len() >= wanted.len()
        } else {
            labels.len() > wanted.len()
        }
    } else {
        labels.len() == wanted.len()
    };
    length_ok
        && labels[labels.len() - wanted.len()..]
            .iter()
            .zip(wanted)
            .all(|(actual, pattern)| pattern == "*" || *actual == pattern)
}

/// Display labels may change independently of a user's saved route. Persist an ingress / protocol /
/// family identity and resolve its current label only when generating a subscription.
pub fn routing_catalog_for_plan(
    subscription: &Subscription,
    plan: &crate::physical::user::UserPlan,
) -> RoutingCatalog {
    let mut catalog = routing_catalog(subscription);
    for line in &mut catalog.lines {
        if let Some(entry) = plan.entries.iter().find(|entry| entry.name == line.name) {
            let protocol = match entry.security {
                crate::physical::user::UserSecurityPlan::AnyTls(_) => "anytls",
                crate::physical::user::UserSecurityPlan::Hysteria2(_) => "hysteria2",
                crate::physical::user::UserSecurityPlan::MtProto { .. } => "mtproto",
                crate::physical::user::UserSecurityPlan::VlessEncryption { .. } => {
                    "vless-encryption"
                }
                crate::physical::user::UserSecurityPlan::Reality(_)
                | crate::physical::user::UserSecurityPlan::Tls(_) => "vless",
            };
            let family = match entry.family {
                Some(crate::model::IpFamily::V4) => "v4",
                Some(crate::model::IpFamily::V6) => "v6",
                None => "both",
            };
            let identity = serde_json::to_vec(&(&entry.ingress_id, protocol, family))
                .expect("entry identity consists of strings");
            line.id = format!("line:{}", crate::hash::sha256_hex(&identity));
        }
    }
    catalog
}
pub fn clash_subscription_with_rules_for_plan(
    subscription: &Subscription,
    plan: &crate::physical::user::UserPlan,
    preferences: &RulePreferences,
) -> String {
    let catalog = routing_catalog_for_plan(subscription, plan);
    let resolve = |target: &str| {
        catalog
            .lines
            .iter()
            .find(|line| line.id == target)
            .map_or_else(|| target.to_owned(), |line| format!("line:{}", line.name))
    };
    let mut resolved = preferences.clone();
    for rule in &mut resolved.custom {
        rule.target = resolve(&rule.target);
    }
    for state in resolved.modules.values_mut() {
        state.target = resolve(&state.target);
    }
    resolved.final_target = resolve(&resolved.final_target);
    clash_subscription_with_rules(subscription, &resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::subscription::{
        SubscriptionEntry, SubscriptionSecurity, SubscriptionStream,
    };
    fn subscription() -> Subscription {
        Subscription {
            tenant: "test".into(),
            user: "alice".into(),
            entries: vec![SubscriptionEntry {
                name: "香港 01".into(),
                server: "node.example.invalid".into(),
                port: 443,
                uuid: "synthetic".into(),
                front_name: None,
                stream: SubscriptionStream::Tcp,
                security: SubscriptionSecurity::Tls(
                    crate::artifacts::subscription::SubscriptionTls {
                        server_name: "node.example.invalid".into(),
                        flow: None,
                        self_signed: false,
                        certificate_fingerprint: None,
                    },
                ),
            }],
            external_proxies: vec![],
            front_groups: vec![],
        }
    }
    fn rule(kind: RuleKind, value: &str, target: &str) -> CustomRule {
        CustomRule {
            id: "rule-1".into(),
            kind,
            value: value.into(),
            target: target.into(),
        }
    }
    fn plan() -> crate::physical::user::UserPlan {
        use crate::physical::user::{
            UserPlan, UserSecurityPlan, UserSubscriptionEntryPlan, UserTlsPlan,
        };
        UserPlan {
            tenant: "test".into(),
            user: "alice".into(),
            uuid: "synthetic".into(),
            entries: vec![UserSubscriptionEntryPlan {
                grant_id: "grant".into(),
                ingress_id: "ingress-one".into(),
                name: "香港 01".into(),
                server: "node.example.invalid".into(),
                port: 443,
                family: Some(crate::model::IpFamily::V4),
                download: None,
                uuid: "synthetic".into(),
                security: UserSecurityPlan::Tls(UserTlsPlan {
                    server_name: "node.example.invalid".into(),
                    flow: None,
                }),
                xhttp: None,
                front_id: None,
                front_name: None,
            }],
            external_proxies: vec![],
            front_groups: vec![],
        }
    }
    #[test]
    fn line_identity_survives_label_endpoint_and_key_changes() {
        use crate::physical::user::UserSecurityPlan;
        for security in [
            plan().entries[0].security.clone(),
            UserSecurityPlan::VlessEncryption {
                port: 8444,
                public_key: "synthetic-key".into(),
                options: Default::default(),
            },
        ] {
            let mut plan = plan();
            plan.entries[0].security = security;
            let artifact = crate::artifacts::subscription::build(&plan);
            let before = routing_catalog_for_plan(&artifact, &plan);
            let mut preferences = RulePreferences::default();
            preferences
                .custom
                .push(rule(RuleKind::Domain, "example.com", &before.lines[0].id));
            plan.entries[0].name = "Renamed".into();
            plan.entries[0].server = "changed.example.invalid".into();
            plan.entries[0].port = 9443;
            plan.entries[0].uuid = "rotated-synthetic".into();
            if let UserSecurityPlan::VlessEncryption {
                public_key, port, ..
            } = &mut plan.entries[0].security
            {
                *public_key = "rotated-synthetic-key".into();
                *port = 9444;
            }
            let artifact = crate::artifacts::subscription::build(&plan);
            let after = routing_catalog_for_plan(&artifact, &plan);
            assert_eq!(before.lines[0].id, after.lines[0].id);
            let yaml = clash_subscription_with_rules_for_plan(&artifact, &plan, &preferences);
            assert!(yaml.contains("DOMAIN-SUFFIX,example.com,📍 Renamed"));
        }
    }
    #[test]
    fn vless_encryption_and_tls_have_distinct_saved_targets() {
        use crate::physical::user::UserSecurityPlan;
        let mut plan = plan();
        let mut encrypted = plan.entries[0].clone();
        encrypted.name = "香港 01 | VLESS Encryption".into();
        encrypted.security = UserSecurityPlan::VlessEncryption {
            port: 8444,
            public_key: "synthetic-key".into(),
            options: Default::default(),
        };
        // Encryption is emitted first by the physical user planner.
        plan.entries.insert(0, encrypted);
        let artifact = crate::artifacts::subscription::build(&plan);
        let catalog = routing_catalog_for_plan(&artifact, &plan);
        assert_ne!(catalog.lines[0].id, catalog.lines[1].id);
        // Keep the pre-existing identity for ordinary VLESS.
        let old_identity = serde_json::to_vec(&("ingress-one", "vless", "v4")).unwrap();
        assert_eq!(
            catalog.lines[1].id,
            format!("line:{}", crate::hash::sha256_hex(&old_identity))
        );
        for line in &catalog.lines {
            let mut prefs = RulePreferences::default();
            prefs
                .custom
                .push(rule(RuleKind::Domain, "example.com", &line.id));
            prefs.validate(&catalog).unwrap();
            let output = clash_subscription_with_rules_for_plan(&artifact, &plan, &prefs);
            let raw = output
                .lines()
                .find_map(|line| {
                    line.trim()
                        .strip_prefix("- ")
                        .filter(|line| line.starts_with("\"DOMAIN-SUFFIX,"))
                })
                .unwrap();
            let actual: String = serde_json::from_str(raw).unwrap();
            assert_eq!(actual, format!("DOMAIN-SUFFIX,example.com,{}", line.yaml));
        }
    }
    #[test]
    fn comma_names_keep_safe_unique_rule_targets_and_original_proxy_labels() {
        let mut artifact = subscription();
        artifact.entries = ["Main, Premium", "Main， Premium", "📍 Main， Premium"]
            .into_iter()
            .map(|name| {
                let mut entry = artifact.entries[0].clone();
                entry.name = name.into();
                entry
            })
            .collect();
        let catalog = routing_catalog(&artifact);
        assert_eq!(
            catalog
                .lines
                .iter()
                .map(|line| &line.yaml)
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
        let mut prefs = RulePreferences::default();
        for (i, line) in catalog.lines.iter().enumerate() {
            let mut custom = rule(RuleKind::Domain, &format!("line-{i}.example.com"), &line.id);
            custom.id = format!("rule-{i}");
            prefs.custom.push(custom);
        }
        prefs.modules.insert(
            "ai".into(),
            ModulePreference {
                on: true,
                target: catalog.lines[0].id.clone(),
            },
        );
        prefs.validate(&catalog).unwrap();
        let output = clash_subscription_with_rules(&artifact, &prefs);
        for (i, line) in catalog.lines.iter().enumerate() {
            assert!(!line.yaml.contains(','));
            assert!(output.contains(&format!(
                "name: {}\n    type: select\n    proxies: [{}]",
                yaml_quote(&line.yaml),
                yaml_quote(&line.name)
            )));
            let rule = format!("DOMAIN-SUFFIX,line-{i}.example.com,{}", line.yaml);
            assert_eq!(rule.split(',').count(), 3);
            assert!(output.contains(&yaml_quote(&rule)));
        }
        assert!(output.contains(&format!(
            "name: \"🤖 AI 服务\"\n    type: select\n    proxies: [{},",
            yaml_quote(&catalog.lines[0].yaml)
        )));
    }
    #[test]
    fn defaults_preserve_existing_subscription_bytes() {
        let artifact = subscription();
        assert_eq!(
            clash_subscription(&artifact),
            clash_subscription_with_rules(&artifact, &RulePreferences::default())
        );
    }
    #[test]
    fn custom_rules_precede_ads_and_keep_ipv6_no_resolve() {
        let artifact = subscription();
        let mut prefs = RulePreferences::default();
        prefs
            .custom
            .push(rule(RuleKind::Ip, "2001:db8::/32", "direct"));
        let output = clash_subscription_with_rules(&artifact, &prefs);
        assert!(
            output
                .find("IP-CIDR6,2001:db8::/32,DIRECT,no-resolve")
                .unwrap()
                < output.find("RULE-SET,category-ads-all,").unwrap()
        );
    }
    #[test]
    fn lines_get_dedicated_groups_and_service_default() {
        let artifact = subscription();
        let catalog = routing_catalog(&artifact);
        let mut prefs = RulePreferences::default();
        prefs.modules.insert(
            "ai".into(),
            ModulePreference {
                on: true,
                target: catalog.lines[0].id.clone(),
            },
        );
        prefs.validate(&catalog).unwrap();
        let output = clash_subscription_with_rules(&artifact, &prefs);
        assert!(
            output.contains("name: \"📍 香港 01\"\n    type: select\n    proxies: [\"香港 01\"]")
        );
        assert!(output.contains(
            "name: \"🤖 AI 服务\"\n    type: select\n    proxies: [\"📍 香港 01\", \"🚀 节点选择\""
        ));
    }
    #[test]
    fn missing_saved_lines_fall_back_only_at_render_time() {
        let artifact = subscription();
        let catalog = routing_catalog(&artifact);
        let mut prefs = RulePreferences::default();
        prefs
            .custom
            .push(rule(RuleKind::Domain, "example.com", "line:gone"));
        assert!(prefs.validate(&catalog).is_err());
        assert!(clash_subscription_with_rules(&artifact, &prefs)
            .contains("DOMAIN-SUFFIX,example.com,🚀 节点选择"));
    }
    #[test]
    fn disabled_services_and_added_services_keep_priority() {
        let artifact = subscription();
        let mut prefs = RulePreferences::default();
        prefs.extra.push("netflix".into());
        prefs.modules.insert(
            "ai".into(),
            ModulePreference {
                on: false,
                target: "select".into(),
            },
        );
        let output = clash_subscription_with_rules(&artifact, &prefs);
        assert!(!output.contains("RULE-SET,openai,"));
        assert!(
            output.find("RULE-SET,netflix,").unwrap()
                < output.find("RULE-SET,geolocation-!cn,").unwrap()
        );
    }
    #[test]
    fn preferences_reject_injection_unknown_modules_and_locked_disable() {
        let catalog = routing_catalog(&subscription());
        let mut prefs = RulePreferences::default();
        prefs.custom.push(rule(
            RuleKind::Domain,
            "example.com,DIRECT\nrules:",
            "direct",
        ));
        assert!(prefs.validate(&catalog).is_err());
        prefs.custom.clear();
        prefs.modules.insert(
            "private".into(),
            ModulePreference {
                on: false,
                target: "direct".into(),
            },
        );
        assert!(prefs.validate(&catalog).is_err());
        prefs.modules.clear();
        prefs.extra.push("../../secrets".into());
        assert!(prefs.validate(&catalog).is_err());
    }
    #[test]
    fn names_avoid_proxy_and_group_collisions() {
        let mut artifact = subscription();
        artifact.entries[0].name = "🚀 节点选择".into();
        let catalog = routing_catalog(&artifact);
        assert_eq!(catalog.select, "🚀 节点选择 · Brocade 2");
        let mut prefs = RulePreferences::default();
        prefs
            .custom
            .push(rule(RuleKind::Domain, "example.com", "select"));
        assert!(clash_subscription_with_rules(&artifact, &prefs)
            .contains("DOMAIN-SUFFIX,example.com,🚀 节点选择 · Brocade 2"));
    }
    #[test]
    fn probe_uses_exact_suffix_keyword_ipv6_and_case_sensitive_process_semantics() {
        assert!(custom_rule_matches(
            &rule(RuleKind::Domain, "example.com", "direct"),
            &RuleKind::Domain,
            "a.example.com"
        ));
        assert!(!custom_rule_matches(
            &rule(RuleKind::Domain, "example.com", "direct"),
            &RuleKind::Domain,
            "notexample.com"
        ));
        assert!(custom_rule_matches(
            &rule(RuleKind::Ip, "2001:db8::/32", "direct"),
            &RuleKind::Ip,
            "2001:db8::1"
        ));
        assert!(!custom_rule_matches(
            &rule(RuleKind::Process, "Curl", "direct"),
            &RuleKind::Process,
            "curl"
        ));
        assert!(source_rule_matches(
            "+.chatgpt.com",
            &RuleKind::Domain,
            "chatgpt.com"
        ));
        assert!(source_rule_matches(
            "api.*.example.com",
            &RuleKind::Domain,
            "api.a.example.com"
        ));
        assert!(!source_rule_matches(
            "openai.com",
            &RuleKind::Domain,
            "notopenai.com"
        ));
        assert!(!source_rule_matches(
            "*.example.com",
            &RuleKind::Domain,
            "a.b.example.com"
        ));
        assert!(source_rule_matches(
            ".example.com",
            &RuleKind::Domain,
            "a.b.example.com"
        ));
        assert!(!source_rule_matches(
            ".example.com",
            &RuleKind::Domain,
            "example.com"
        ));
        assert!(rule_source_url("../../secrets", false).is_none());
    }

    #[test]
    fn featured_geosite_sets_follow_module_order_without_ip_sets_or_duplicates() {
        let sets = featured_geosite_sets();
        assert_eq!(
            &sets[..4],
            ["category-ads-all", "private", "openai", "anthropic"]
        );
        assert!(sets.contains(&"geolocation-!cn") && sets.contains(&"netflix"));
        assert!(!sets.iter().any(|id| id.ends_with("-ip")));
        let unique: BTreeSet<_> = sets.iter().collect();
        assert_eq!(unique.len(), sets.len());
    }
}
