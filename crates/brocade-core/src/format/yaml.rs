use std::collections::BTreeSet;

use crate::artifacts::subscription::{
    Subscription, SubscriptionEntry, SubscriptionSecurity, SubscriptionStream,
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
    push_standard_base(&mut lines);
    lines.push(String::new());
    lines.push("proxies:".to_owned());

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

fn push_standard_base(lines: &mut Vec<String>) {
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
}

fn standard_group_names(subscription: &Subscription) -> Vec<String> {
    let mut reserved = subscription
        .entries
        .iter()
        .map(|entry| entry.name.clone())
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
    // `reality-opts` is what tells mihomo to borrow a site rather than verify a certificate.
    // Written for a TLS entry it would have the client authenticate against a public key nobody
    // holds; omitted for a REALITY one, the client verifies a certificate that does not exist.
    let (server_name, fingerprint, flow, reality) = match &entry.security {
        SubscriptionSecurity::Reality(reality) => (
            &reality.server_name,
            &reality.fingerprint,
            &reality.flow,
            Some(reality),
        ),
        SubscriptionSecurity::Tls(tls) => (&tls.server_name, &tls.fingerprint, &tls.flow, None),
        SubscriptionSecurity::Hysteria2(_) => unreachable!("Hysteria 已在上方单独渲染"),
    };
    if let Some(flow) = flow {
        lines.push(format!("    flow: {}", scalar(flow)));
    }
    lines.push(format!("    servername: {}", scalar(server_name)));
    lines.push(format!("    client-fingerprint: {}", scalar(fingerprint)));
    if let Some(reality) = reality {
        lines.push("    reality-opts:".to_owned());
        lines.push(format!("      public-key: {}", scalar(&reality.public_key)));
        lines.push(format!("      short-id: {}", yaml_quote(&reality.short_id)));
    }
    if let SubscriptionStream::Xhttp {
        path,
        host,
        download,
        mux,
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
        // `reuse-settings`, spelled exactly so. mihomo's `XHTTPOptions` has no `x-mux` key at
        // all, and its decoder reports nothing for input keys it does not recognise — it errors
        // only on struct fields left unset — so a name invented here is a setting that never
        // arrives and never complains. The field it feeds is `XHTTPReuseSettings`, tagged
        // `// aka XMUX` in mihomo's own source.
        //
        // The integer is fine against mihomo's `string` field: its decoder runs with
        // `WeaklyTypedInput`, which formats an int into the string.
        //
        // Omitted when unset — but note that absent does not mean the same thing here as it does
        // in the xray config: mihomo builds no reuse manager at all and reuses nothing, where
        // xray puts every stream on one connection. See `Xhttp::mux`.
        if let Some(concurrency) = mux {
            lines.push("      reuse-settings:".to_owned());
            lines.push(format!("        max-concurrency: {concurrency}"));
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
                "        client-fingerprint: {}",
                scalar(&download.fingerprint)
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
                lines.push("        reuse-settings:".to_owned());
                lines.push(format!("          max-concurrency: {concurrency}"));
            }
        }
    }
    if let Some(front_name) = &entry.front_name {
        lines.push(format!("    dialer-proxy: {}", yaml_quote(front_name)));
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
