use std::collections::BTreeSet;

use crate::artifacts::subscription::{
    Subscription, SubscriptionEntry, SubscriptionExternalProxy, SubscriptionSecurity,
    SubscriptionStream,
};
use crate::model::{
    ExternalOutboundProtocol, ExternalOutboundSecurity, ExternalVlessTransport, XhttpTuning,
    XhttpXmux, XhttpXmuxRange,
};
const TEST_URL: &str = "https://www.gstatic.com/generate_204";
const TEST_INTERVAL: u32 = 300;
// Pinned at implementation time. The generated configuration never follows a mutable branch,
// so the same Brocade build and the same snapshot produce the same bytes even after upstream
// changes its rule sets.
const RULE_PROVIDER_BASE_URL: &str = "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/7a26c86cf0e7497a423ab86c37274b34e3ce4153/geo";

#[derive(Clone, Copy)]
enum StandardGroupKind {
    Select,
    UrlTest,
    DirectFirst,
    RejectFirst,
}

#[derive(Clone, Copy)]
struct StandardRule {
    id: &'static str,
    behavior: &'static str,
    path: &'static str,
    no_resolve: bool,
}

#[derive(Clone, Copy)]
struct StandardModule {
    id: &'static str,
    name: &'static str,
    kind: StandardGroupKind,
    rules: &'static [StandardRule],
}

const STANDARD_MODULES: &[StandardModule] = &[
    StandardModule {
        id: "select",
        name: "🚀 节点选择",
        kind: StandardGroupKind::Select,
        rules: &[],
    },
    StandardModule {
        id: "auto",
        name: "⚡ 自动选择",
        kind: StandardGroupKind::UrlTest,
        rules: &[],
    },
    StandardModule {
        id: "ad",
        name: "🛑 广告拦截",
        kind: StandardGroupKind::RejectFirst,
        rules: &[StandardRule {
            id: "category-ads-all",
            behavior: "domain",
            path: "geosite/category-ads-all.mrs",
            no_resolve: false,
        }],
    },
    StandardModule {
        id: "ai",
        name: "🤖 AI 服务",
        kind: StandardGroupKind::Select,
        rules: &[
            StandardRule {
                id: "openai",
                behavior: "domain",
                path: "geosite/openai.mrs",
                no_resolve: false,
            },
            StandardRule {
                id: "anthropic",
                behavior: "domain",
                path: "geosite/anthropic.mrs",
                no_resolve: false,
            },
            StandardRule {
                id: "category-ai-chat-!cn",
                behavior: "domain",
                path: "geosite/category-ai-chat-!cn.mrs",
                no_resolve: false,
            },
        ],
    },
    StandardModule {
        id: "youtube",
        name: "📹 油管视频",
        kind: StandardGroupKind::Select,
        rules: &[StandardRule {
            id: "youtube",
            behavior: "domain",
            path: "geosite/youtube.mrs",
            no_resolve: false,
        }],
    },
    StandardModule {
        id: "google",
        name: "🔍 谷歌服务",
        kind: StandardGroupKind::Select,
        rules: &[
            StandardRule {
                id: "google",
                behavior: "domain",
                path: "geosite/google.mrs",
                no_resolve: false,
            },
            StandardRule {
                id: "google-ip",
                behavior: "ipcidr",
                path: "geoip/google.mrs",
                no_resolve: true,
            },
        ],
    },
    StandardModule {
        id: "microsoft",
        name: "Ⓜ️ 微软服务",
        kind: StandardGroupKind::Select,
        rules: &[
            StandardRule {
                id: "microsoft",
                behavior: "domain",
                path: "geosite/microsoft.mrs",
                no_resolve: false,
            },
            StandardRule {
                id: "onedrive",
                behavior: "domain",
                path: "geosite/onedrive.mrs",
                no_resolve: false,
            },
        ],
    },
    StandardModule {
        id: "apple",
        name: "🍏 苹果服务",
        kind: StandardGroupKind::Select,
        rules: &[
            StandardRule {
                id: "apple",
                behavior: "domain",
                path: "geosite/apple.mrs",
                no_resolve: false,
            },
            StandardRule {
                id: "icloud",
                behavior: "domain",
                path: "geosite/icloud.mrs",
                no_resolve: false,
            },
        ],
    },
    StandardModule {
        id: "telegram",
        name: "📲 电报消息",
        kind: StandardGroupKind::Select,
        rules: &[
            StandardRule {
                id: "telegram",
                behavior: "domain",
                path: "geosite/telegram.mrs",
                no_resolve: false,
            },
            StandardRule {
                id: "telegram-ip",
                behavior: "ipcidr",
                path: "geoip/telegram.mrs",
                no_resolve: true,
            },
        ],
    },
    StandardModule {
        id: "github",
        name: "🐱 代码托管",
        kind: StandardGroupKind::Select,
        rules: &[
            StandardRule {
                id: "github",
                behavior: "domain",
                path: "geosite/github.mrs",
                no_resolve: false,
            },
            StandardRule {
                id: "gitlab",
                behavior: "domain",
                path: "geosite/gitlab.mrs",
                no_resolve: false,
            },
            StandardRule {
                id: "atlassian",
                behavior: "domain",
                path: "geosite/atlassian.mrs",
                no_resolve: false,
            },
        ],
    },
    StandardModule {
        id: "private",
        name: "🏠 私有网络",
        kind: StandardGroupKind::DirectFirst,
        rules: &[
            StandardRule {
                id: "private",
                behavior: "domain",
                path: "geosite/private.mrs",
                no_resolve: false,
            },
            StandardRule {
                id: "private-ip",
                behavior: "ipcidr",
                path: "geoip/private.mrs",
                no_resolve: true,
            },
        ],
    },
    StandardModule {
        id: "cn",
        name: "🔒 国内服务",
        kind: StandardGroupKind::DirectFirst,
        rules: &[
            StandardRule {
                id: "geolocation-cn",
                behavior: "domain",
                path: "geosite/geolocation-cn.mrs",
                no_resolve: false,
            },
            StandardRule {
                id: "cn-ip",
                behavior: "ipcidr",
                path: "geoip/cn.mrs",
                no_resolve: true,
            },
        ],
    },
    StandardModule {
        id: "global",
        name: "🌍 非中国",
        kind: StandardGroupKind::Select,
        rules: &[StandardRule {
            id: "geolocation-!cn",
            behavior: "domain",
            path: "geosite/geolocation-!cn.mrs",
            no_resolve: false,
        }],
    },
    StandardModule {
        id: "final",
        name: "🐟 漏网之鱼",
        kind: StandardGroupKind::Select,
        rules: &[],
    },
];

const RULE_MODULE_ORDER: &[&str] = &[
    "ad",
    "private",
    "ai",
    "cn",
    "youtube",
    "google",
    "telegram",
    "github",
    "microsoft",
    "apple",
    "global",
];

const EXPERIMENTAL_CN_RULE: StandardRule = StandardRule {
    id: "cn",
    behavior: "domain",
    path: "geosite/cn.mrs",
    no_resolve: false,
};

pub fn clash_subscription(subscription: &Subscription) -> String {
    let mut lines = Vec::new();
    push_standard_base(&mut lines, subscription);
    lines.push(String::new());
    lines.push("proxies:".to_owned());

    for proxy in &subscription.external_proxies {
        push_external_proxy(&mut lines, proxy);
    }
    for entry in &subscription.entries {
        push_proxy(&mut lines, entry);
    }

    let standard_names = standard_group_names(subscription);
    lines.push(String::new());
    lines.push("proxy-groups:".to_owned());
    for group in &subscription.front_groups {
        lines.push(format!("  - name: {}", yaml_quote(&group.name)));
        lines.push(format!("    type: {}", group.strategy.as_str()));
        lines.push(format!("    proxies: {}", inline_list(&group.members)));
    }
    push_standard_groups(&mut lines, subscription, &standard_names);
    push_rule_providers(&mut lines);
    push_rules(&mut lines, &standard_names);

    format!("{}\n", lines.join("\n"))
}

/// A deliberately small, self-contained Mihomo document for koipy.
///
/// The bot measures concrete proxies itself, so SubBoost's DNS policy, remote rule providers and
/// daily-use service groups only add parser and network dependencies. Front groups are retained:
/// entries carrying `dialer-proxy` would otherwise look valid but test a different route from the
/// one handed to the user. The final group contains only user-facing entries, never helper
/// external proxies.
pub fn clash_haitun_subscription(subscription: &Subscription) -> String {
    let mut lines = vec![
        "# Brocade · koipy 测速（请求时动态生成）".to_owned(),
        "mixed-port: 7897".to_owned(),
        "allow-lan: false".to_owned(),
        "mode: rule".to_owned(),
        "log-level: warning".to_owned(),
        "ipv6: true".to_owned(),
        String::new(),
        "proxies:".to_owned(),
    ];

    for proxy in &subscription.external_proxies {
        push_external_proxy(&mut lines, proxy);
    }
    for entry in &subscription.entries {
        push_proxy(&mut lines, entry);
    }

    let reserved = subscription
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .chain(
            subscription
                .external_proxies
                .iter()
                .map(|proxy| proxy.name.as_str()),
        )
        .chain(
            subscription
                .front_groups
                .iter()
                .map(|group| group.name.as_str()),
        )
        .collect::<BTreeSet<_>>();
    let mut test_group = "koipy 测速".to_owned();
    let mut suffix = 1;
    while reserved.contains(test_group.as_str()) {
        suffix += 1;
        test_group = format!("koipy 测速 · Brocade {suffix}");
    }
    lines.push(String::new());
    lines.push("proxy-groups:".to_owned());
    for group in &subscription.front_groups {
        lines.push(format!("  - name: {}", yaml_quote(&group.name)));
        lines.push(format!("    type: {}", group.strategy.as_str()));
        lines.push(format!("    proxies: {}", inline_list(&group.members)));
    }
    let mut candidates = subscription
        .entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        candidates.push("DIRECT".to_owned());
    }
    lines.push(format!("  - name: {}", yaml_quote(&test_group)));
    lines.push("    type: select".to_owned());
    lines.push(format!("    proxies: {}", inline_list(&candidates)));
    lines.push(String::new());
    lines.push("rules:".to_owned());
    lines.push(format!("  - MATCH,{}", test_group));

    format!("{}\n", lines.join("\n"))
}

fn push_standard_base(lines: &mut Vec<String>, subscription: &Subscription) {
    lines.extend(
        [
            "# Brocade · SubBoost 标准版（请求时动态生成）",
            "mixed-port: 7897",
            "allow-lan: true",
            "mode: rule",
            "log-level: info",
            "ipv6: true",
            "unified-delay: true",
            "tcp-concurrent: true",
            "find-process-mode: strict",
            "",
            "dns:",
            "  enable: true",
            "  listen: 127.0.0.1:5335",
            "  ipv6: true",
            "  use-system-hosts: false",
            "  enhanced-mode: fake-ip",
            "  fake-ip-range: 198.18.0.1/16",
            "  default-nameserver: [223.5.5.5, 119.29.29.29, 8.8.8.8]",
            "  nameserver: [223.5.5.5, 119.29.29.29, \"https://dns.alidns.com/dns-query\", \"https://cloudflare-dns.com/dns-query\"]",
            "  fallback: [\"https://dns.quad9.net/dns-query\", \"https://dns.google/dns-query\"]",
            "  fallback-filter:",
            "    geoip: true",
            "    ipcidr: [240.0.0.0/4, 0.0.0.0/32, 127.0.0.1/32]",
            "  fake-ip-filter: [\"*.lan\", \"*.local\", \"stun.*.*\", time.windows.com, time.apple.com, pool.ntp.org, localhost]",
            "",
            "profile:",
            "  store-selected: true",
            "  store-fake-ip: false",
            "",
            "sniffer:",
            "  enable: true",
            "  parse-pure-ip: true",
            "  sniff:",
            "    TLS: {ports: [443, 8443]}",
            "    HTTP: {ports: [80, \"8080-8880\"], override-destination: true}",
            "    QUIC: {ports: [443, 8443]}",
        ]
        .into_iter()
        .map(str::to_owned),
    );

    let server_names = subscription_server_names(subscription);
    if !server_names.is_empty() {
        lines.push("  skip-domain:".to_owned());
        lines.extend(
            server_names
                .into_iter()
                .map(|server_name| format!("    - {}", yaml_quote(server_name))),
        );
    }
}

/// Names used by the subscription's own encrypted transports must never become replacement
/// destinations when the standard template sniffs TLS.  A REALITY entry dials its literal
/// `server` address while borrowing `servername`; replacing the former with a fresh DNS lookup
/// of the latter sends the handshake to the cover website and produces a genuine certificate
/// instead of a REALITY session.
///
/// A set is intentional: one cover name is commonly shared by many entries, and lexical order
/// keeps a dynamic subscription byte-stable when unrelated projection order changes.
fn subscription_server_names(subscription: &Subscription) -> BTreeSet<&str> {
    subscription
        .entries
        .iter()
        .map(|entry| match &entry.security {
            SubscriptionSecurity::Reality(reality) => reality.server_name.as_str(),
            SubscriptionSecurity::Tls(tls) => tls.server_name.as_str(),
            SubscriptionSecurity::Hysteria2(hysteria) => hysteria.server_name.as_str(),
        })
        .chain(
            subscription
                .external_proxies
                .iter()
                .filter_map(|proxy| match &proxy.security {
                    ExternalOutboundSecurity::None => None,
                    ExternalOutboundSecurity::Tls { server_name, .. }
                    | ExternalOutboundSecurity::Reality { server_name, .. } => {
                        Some(server_name.as_str())
                    }
                }),
        )
        .filter(|server_name| !server_name.trim().is_empty())
        .collect()
}

fn standard_group_names(subscription: &Subscription) -> Vec<String> {
    let mut reserved = subscription
        .entries
        .iter()
        .map(|entry| entry.name.clone())
        .chain(
            subscription
                .external_proxies
                .iter()
                .map(|proxy| proxy.name.clone()),
        )
        .chain(
            subscription
                .front_groups
                .iter()
                .map(|group| group.name.clone()),
        )
        .collect::<BTreeSet<_>>();

    STANDARD_MODULES
        .iter()
        .map(|module| {
            let mut candidate = module.name.to_owned();
            let mut suffix = 1;
            while reserved.contains(&candidate) {
                suffix += 1;
                candidate = format!("{} · Brocade {suffix}", module.name);
            }
            reserved.insert(candidate.clone());
            candidate
        })
        .collect()
}

fn standard_name<'a>(names: &'a [String], id: &str) -> &'a str {
    let index = STANDARD_MODULES
        .iter()
        .position(|module| module.id == id)
        .expect("标准模板引用的模块必须存在");
    &names[index]
}

fn push_standard_groups(lines: &mut Vec<String>, subscription: &Subscription, names: &[String]) {
    let node_names = subscription
        .entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect::<Vec<_>>();
    let select = standard_name(names, "select").to_owned();
    let auto = standard_name(names, "auto").to_owned();

    for (index, module) in STANDARD_MODULES.iter().enumerate() {
        let name = &names[index];
        let mut members = match module.kind {
            StandardGroupKind::UrlTest => node_names.clone(),
            StandardGroupKind::RejectFirst => {
                vec!["REJECT".to_owned(), "DIRECT".to_owned(), select.clone()]
            }
            StandardGroupKind::DirectFirst => vec![
                "DIRECT".to_owned(),
                "REJECT".to_owned(),
                select.clone(),
                auto.clone(),
            ],
            StandardGroupKind::Select if module.id == "select" => {
                vec![auto.clone(), "DIRECT".to_owned(), "REJECT".to_owned()]
            }
            StandardGroupKind::Select => vec![
                select.clone(),
                auto.clone(),
                "DIRECT".to_owned(),
                "REJECT".to_owned(),
            ],
        };
        if !matches!(module.kind, StandardGroupKind::RejectFirst) {
            members.extend(node_names.iter().cloned());
        }
        deduplicate(&mut members);
        // A url-test with no candidates is rejected by Mihomo. Empty user subscriptions are not
        // served publicly, but keeping the pure formatter valid makes preview artifacts useful.
        if members.is_empty() {
            members.push("DIRECT".to_owned());
        }

        lines.push(format!("  - name: {}", yaml_quote(name)));
        lines.push(format!(
            "    type: {}",
            if matches!(module.kind, StandardGroupKind::UrlTest) {
                "url-test"
            } else {
                "select"
            }
        ));
        lines.push(format!("    proxies: {}", inline_list(&members)));
        if matches!(module.kind, StandardGroupKind::UrlTest) {
            lines.push(format!("    url: {}", yaml_quote(TEST_URL)));
            lines.push(format!("    interval: {TEST_INTERVAL}"));
            lines.push("    lazy: false".to_owned());
        }
    }
}

fn push_rule_providers(lines: &mut Vec<String>) {
    lines.push(String::new());
    lines.push("rule-providers:".to_owned());
    for rule in STANDARD_MODULES
        .iter()
        .flat_map(|module| module.rules.iter())
        .chain(std::iter::once(&EXPERIMENTAL_CN_RULE))
    {
        lines.push(format!("  {}:", yaml_quote(rule.id)));
        lines.push("    type: http".to_owned());
        lines.push(format!("    behavior: {}", rule.behavior));
        lines.push("    format: mrs".to_owned());
        lines.push(format!(
            "    url: {}",
            yaml_quote(&format!("{RULE_PROVIDER_BASE_URL}/{}", rule.path))
        ));
        lines.push(format!(
            "    path: {}",
            yaml_quote(&format!("./ruleset/{}.mrs", rule.id))
        ));
        lines.push("    interval: 86400".to_owned());
    }
}

fn push_rules(lines: &mut Vec<String>, names: &[String]) {
    lines.push(String::new());
    lines.push("rules:".to_owned());
    for module_id in RULE_MODULE_ORDER {
        let module = STANDARD_MODULES
            .iter()
            .find(|module| module.id == *module_id)
            .expect("标准规则顺序引用的模块必须存在");
        let target = standard_name(names, module.id);
        for rule in module.rules {
            lines.push(format!(
                "  - RULE-SET,{},{target}{}",
                rule.id,
                if rule.no_resolve { ",no-resolve" } else { "" }
            ));
        }
    }
    lines.push(format!(
        "  - RULE-SET,{},{}",
        EXPERIMENTAL_CN_RULE.id,
        standard_name(names, "cn")
    ));
    lines.push(format!("  - MATCH,{}", standard_name(names, "final")));
}

fn inline_list(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| yaml_quote(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn deduplicate(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

fn push_proxy(lines: &mut Vec<String>, entry: &SubscriptionEntry) {
    if let SubscriptionSecurity::Hysteria2(hysteria) = &entry.security {
        push_hysteria2_proxy(lines, entry, hysteria);
        return;
    }
    lines.push(format!("  - name: {}", yaml_quote(&entry.name)));
    lines.push("    type: vless".to_owned());
    lines.push(format!("    server: {}", scalar(&entry.server)));
    lines.push(format!("    port: {}", entry.port));
    lines.push(format!("    uuid: {}", scalar(&entry.uuid)));
    // mihomo names the network the same way xray does, and carries XHTTP's settings under
    // `xhttp-opts`. Written here rather than left out: a proxy entry that says `tcp` against an
    // XHTTP ingress imports without complaint and fails every connection, because the server
    // matches the request path and refuses anything else.
    match &entry.stream {
        SubscriptionStream::Tcp => lines.push("    network: tcp".to_owned()),
        SubscriptionStream::Xhttp { .. } => lines.push("    network: xhttp".to_owned()),
    }
    lines.push("    tls: true".to_owned());
    lines.push("    udp: true".to_owned());
    // Mihomo's common proxy field. It only affects a TCP transport, so the Hysteria 2 branch
    // above deliberately never reaches this line. A server or path without TFO support falls
    // back to the ordinary handshake.
    lines.push("    tfo: true".to_owned());
    // `reality-opts` is what tells mihomo to borrow a site rather than verify a certificate.
    // Written for a TLS entry it would have the client authenticate against a public key nobody
    // holds; omitted for a REALITY one, the client verifies a certificate that does not exist.
    let (server_name, flow, reality) = match &entry.security {
        SubscriptionSecurity::Reality(reality) => {
            (&reality.server_name, &reality.flow, Some(reality))
        }
        SubscriptionSecurity::Tls(tls) => (&tls.server_name, &tls.flow, None),
        SubscriptionSecurity::Hysteria2(_) => unreachable!("Hysteria 已在上方单独渲染"),
    };
    if let Some(flow) = flow {
        lines.push(format!("    flow: {}", scalar(flow)));
    }
    lines.push(format!("    servername: {}", scalar(server_name)));
    if let Some(reality) = reality {
        lines.push(format!(
            "    client-fingerprint: {}",
            scalar(&reality.fingerprint)
        ));
        lines.push("    reality-opts:".to_owned());
        lines.push(format!("      public-key: {}", scalar(&reality.public_key)));
        lines.push(format!("      short-id: {}", yaml_quote(&reality.short_id)));
    }
    if let SubscriptionStream::Xhttp {
        path,
        host,
        download,
        xmux,
        tuning,
        mode,
    } = &entry.stream
    {
        lines.push("    xhttp-opts:".to_owned());
        lines.push(format!("      path: {}", yaml_quote(path)));
        if let Some(host) = host {
            lines.push(format!("      host: {}", scalar(host)));
        }
        // Same rule as the path above, one level sharper: a server given an explicit upload
        // shape refuses every client that does not name the same one.
        if let Some(mode) = mode {
            lines.push(format!("      mode: {}", yaml_quote(mode)));
        }
        if let Some(tuning) = tuning {
            push_xhttp_tuning(lines, 6, tuning);
        }
        // `reuse-settings`, spelled exactly so. mihomo's `XHTTPOptions` has no `x-mux` key at
        // all, and its decoder reports nothing for input keys it does not recognise — it errors
        // only on struct fields left unset — so a name invented here is a setting that never
        // arrives and never complains. The field it feeds is `XHTTPReuseSettings`, tagged
        // `// aka XMUX` in mihomo's own source.
        //
        // Omitted as one complete object when the operator selects Xray defaults. A custom value
        // includes the lifecycle ranges too: carrying only max-concurrency would make Xray's
        // omitted request and time limits unlimited.
        if let Some(xmux) = xmux {
            push_xhttp_reuse_settings(lines, 6, xmux);
        }
        if let Some(download) = download {
            lines.push("      download-settings:".to_owned());
            lines.push(format!("        server: {}", scalar(&download.server)));
            lines.push(format!("        port: {}", download.port));
            lines.push("        tls: true".to_owned());
            lines.push(format!(
                "        servername: {}",
                scalar(&download.server_name)
            ));
            lines.push(format!(
                "        host: {}",
                scalar(
                    download
                        .http_host
                        .as_deref()
                        .unwrap_or(&download.server_name)
                )
            ));
            lines.push(format!("        path: {}", yaml_quote(path)));
            if let Some(concurrency) = download.mux {
                push_xhttp_reuse_settings(lines, 8, &XhttpXmux::with_concurrency(concurrency));
            }
        }
    }
    if let Some(front_name) = &entry.front_name {
        lines.push(format!("    dialer-proxy: {}", yaml_quote(front_name)));
    }
}

fn push_external_proxy(lines: &mut Vec<String>, proxy: &SubscriptionExternalProxy) {
    lines.push(format!("  - name: {}", yaml_quote(&proxy.name)));
    lines.push(format!("    server: {}", scalar(&proxy.server)));
    lines.push(format!("    port: {}", proxy.port));
    match &proxy.protocol {
        ExternalOutboundProtocol::Vless {
            credential,
            encryption: _,
            flow,
            transport,
        } => {
            lines.push("    type: vless".to_owned());
            lines.push(format!("    uuid: {}", scalar(credential)));
            match transport {
                ExternalVlessTransport::Raw => lines.push("    network: tcp".to_owned()),
                ExternalVlessTransport::Xhttp(xhttp) => {
                    lines.push("    network: xhttp".to_owned());
                    lines.push("    xhttp-opts:".to_owned());
                    lines.push(format!("      path: {}", yaml_quote(&xhttp.path)));
                    if let Some(host) = &xhttp.host {
                        lines.push(format!("      host: {}", scalar(host)));
                    }
                    if let Some(mode) = xhttp.mode.as_str() {
                        lines.push(format!("      mode: {}", yaml_quote(mode)));
                    }
                    if let Some(concurrency) = xhttp.mux {
                        push_xhttp_reuse_settings(
                            lines,
                            6,
                            &XhttpXmux::with_concurrency(concurrency),
                        );
                    }
                    if let Some(download) = &xhttp.download {
                        lines.push("      download-settings:".to_owned());
                        lines.push(format!("        server: {}", scalar(&download.address)));
                        lines.push(format!("        port: {}", download.port));
                        lines.push(format!("        path: {}", yaml_quote(&download.path)));
                        if let Some(host) = &download.host {
                            lines.push(format!("        host: {}", scalar(host)));
                        }
                        push_external_security(lines, &download.security, 8);
                        if let Some(concurrency) = download.mux {
                            push_xhttp_reuse_settings(
                                lines,
                                8,
                                &XhttpXmux::with_concurrency(concurrency),
                            );
                        }
                    }
                }
            }
            lines.push("    udp: true".to_owned());
            if let Some(flow) = flow.as_ref().filter(|flow| !flow.is_empty()) {
                lines.push(format!("    flow: {}", scalar(flow)));
            }
            push_external_security(lines, &proxy.security, 4);
        }
        ExternalOutboundProtocol::Shadowsocks2022 { credential, method } => {
            lines.push("    type: ss".to_owned());
            lines.push(format!("    cipher: {}", scalar(method)));
            lines.push(format!("    password: {}", scalar(credential)));
            lines.push("    udp: true".to_owned());
        }
        ExternalOutboundProtocol::Socks5 {
            username,
            credential,
        } => {
            lines.push("    type: socks5".to_owned());
            push_external_auth(lines, username.as_deref(), credential);
            lines.push("    udp: true".to_owned());
        }
        ExternalOutboundProtocol::HttpConnect {
            username,
            credential,
        } => {
            lines.push("    type: http".to_owned());
            push_external_auth(lines, username.as_deref(), credential);
            push_external_security(lines, &proxy.security, 4);
        }
        ExternalOutboundProtocol::Wireguard {
            credential,
            peer_public_key,
            local_addresses,
            mtu,
            reserved,
            keep_alive,
            allowed_ips,
            ..
        } => {
            lines.push("    type: wireguard".to_owned());
            if let Some(address) = local_addresses
                .iter()
                .find(|address| !address.contains(':'))
            {
                lines.push(format!(
                    "    ip: {}",
                    scalar(address.trim_end_matches("/32"))
                ));
            }
            if let Some(address) = local_addresses.iter().find(|address| address.contains(':')) {
                lines.push(format!(
                    "    ipv6: {}",
                    scalar(address.trim_end_matches("/128"))
                ));
            }
            lines.push(format!("    private-key: {}", scalar(credential)));
            lines.push(format!("    public-key: {}", scalar(peer_public_key)));
            lines.push(format!("    allowed-ips: {}", inline_list(allowed_ips)));
            if !reserved.is_empty() {
                lines.push(format!(
                    "    reserved: [{}]",
                    reserved
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            lines.push(format!("    persistent-keepalive: {keep_alive}"));
            lines.push(format!("    mtu: {mtu}"));
            lines.push("    udp: true".to_owned());
        }
        ExternalOutboundProtocol::Warp { .. } => {
            unreachable!("WARP has no user-scoped identity and is rejected from Clash fronts")
        }
    }
}

fn push_xhttp_reuse_settings(lines: &mut Vec<String>, indent: usize, xmux: &XhttpXmux) {
    let base = " ".repeat(indent);
    let field = " ".repeat(indent + 2);
    lines.push(format!("{base}reuse-settings:"));
    if let Some(concurrency) = xmux.max_concurrency {
        lines.push(format!("{field}max-concurrency: {concurrency}"));
    }
    if let Some(connections) = xmux.max_connections {
        lines.push(format!("{field}max-connections: {connections}"));
    }
    lines.push(format!(
        "{field}h-max-request-times: {}",
        yaml_quote(&xhttp_range(&xmux.h_max_request_times))
    ));
    lines.push(format!(
        "{field}h-max-reusable-secs: {}",
        yaml_quote(&xhttp_range(&xmux.h_max_reusable_secs))
    ));
    if let Some(period) = xmux.h_keep_alive_period_secs {
        lines.push(format!("{field}h-keep-alive-period: {period}"));
    }
}

fn push_xhttp_tuning(lines: &mut Vec<String>, indent: usize, tuning: &XhttpTuning) {
    let field = " ".repeat(indent);
    if let Some(range) = &tuning.x_padding_bytes {
        lines.push(format!(
            "{field}x-padding-bytes: {}",
            yaml_quote(&xhttp_range(range))
        ));
    }
}

fn xhttp_range(range: &XhttpXmuxRange) -> String {
    if range.from == range.to {
        range.from.to_string()
    } else {
        format!("{}-{}", range.from, range.to)
    }
}

fn push_external_auth(lines: &mut Vec<String>, username: Option<&str>, credential: &str) {
    if let Some(username) = username.filter(|username| !username.is_empty()) {
        lines.push(format!("    username: {}", scalar(username)));
        lines.push(format!("    password: {}", scalar(credential)));
    }
}

fn push_external_security(
    lines: &mut Vec<String>,
    security: &ExternalOutboundSecurity,
    indent: usize,
) {
    let pad = " ".repeat(indent);
    match security {
        ExternalOutboundSecurity::None => {}
        ExternalOutboundSecurity::Tls {
            server_name,
            fingerprint,
        } => {
            lines.push(format!("{pad}tls: true"));
            lines.push(format!("{pad}servername: {}", scalar(server_name)));
            lines.push(format!("{pad}client-fingerprint: {}", scalar(fingerprint)));
        }
        ExternalOutboundSecurity::Reality {
            server_name,
            public_key,
            short_id,
            fingerprint,
        } => {
            lines.push(format!("{pad}tls: true"));
            lines.push(format!("{pad}servername: {}", scalar(server_name)));
            lines.push(format!("{pad}client-fingerprint: {}", scalar(fingerprint)));
            lines.push(format!("{pad}reality-opts:"));
            lines.push(format!("{pad}  public-key: {}", scalar(public_key)));
            lines.push(format!("{pad}  short-id: {}", yaml_quote(short_id)));
        }
    }
}

fn push_hysteria2_proxy(
    lines: &mut Vec<String>,
    entry: &SubscriptionEntry,
    hysteria: &crate::artifacts::subscription::SubscriptionHysteria2,
) {
    lines.push(format!("  - name: {}", yaml_quote(&entry.name)));
    lines.push("    type: hysteria2".to_owned());
    lines.push(format!("    server: {}", scalar(&entry.server)));
    // Two spellings, one of which wins: mihomo documents `ports` as enabling port jumping and
    // *ignoring* `port`, so the two are alternatives rather than a pair. Emitting both would
    // still work, and would leave a config where the number a reader sees under `port` is not
    // the one in use.
    match &hysteria.settings.hop {
        // Quoted deliberately. `50000-50009` is not a number, so YAML would hand it over as a
        // string either way — but the one that matters is a single-port range like `50000-50000`
        // reduced by hand to `50000`, which unquoted becomes an integer under a key that expects
        // a range. Quoting keeps the type from depending on the value.
        Some(hop) => lines.push(format!("    ports: \"{}-{}\"", hop.start, hop.end)),
        None => lines.push(format!("    port: {}", entry.port)),
    }
    lines.push(format!("    password: {}", scalar(&entry.uuid)));
    lines.push(format!("    sni: {}", scalar(&hysteria.server_name)));
    lines.push("    udp: true".to_owned());
    if let Some(crate::model::HysteriaObfs::Salamander { password }) = &hysteria.settings.obfs {
        lines.push("    obfs: salamander".to_owned());
        lines.push(format!("    obfs-password: {}", scalar(password)));
    }
    if hysteria.settings.congestion != crate::model::HysteriaCongestion::Bbr {
        if let Some(up) = &hysteria.settings.bandwidth.up {
            lines.push(format!("    up: {}", scalar(up)));
        }
        if let Some(down) = &hysteria.settings.bandwidth.down {
            lines.push(format!("    down: {}", scalar(down)));
        }
    }
    // These are receive-side flow-control limits, so writing them only into the server artifact
    // tunes uploads but leaves downloads at mihomo's own (usually smaller) defaults. Keep the
    // subscription paired with the ingress exactly as the xray probe client is: an absent model
    // value remains absent, while an explicit value reaches both endpoints.
    let quic = &hysteria.settings.quic;
    for (key, value) in [
        (
            "initial-stream-receive-window",
            quic.init_stream_receive_window,
        ),
        ("max-stream-receive-window", quic.max_stream_receive_window),
        (
            "initial-connection-receive-window",
            quic.init_connection_receive_window,
        ),
        (
            "max-connection-receive-window",
            quic.max_connection_receive_window,
        ),
    ] {
        if let Some(value) = value {
            lines.push(format!("    {key}: {value}"));
        }
    }
    if let Some(front_name) = &entry.front_name {
        lines.push(format!("    dialer-proxy: {}", yaml_quote(front_name)));
    }
}

fn scalar(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
    {
        value.to_owned()
    } else {
        yaml_quote(value)
    }
}

fn yaml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
