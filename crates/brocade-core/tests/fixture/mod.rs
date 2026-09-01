//! The demo model's fixture: reading the JSON files in `tests/golden/` into a
//! `ModelSnapshot`.
//!
//! It is its own module because it has two consumers: `demo_raw.rs` compares the two
//! compilers' artifacts with it, and `probe.rs` checks the probe work list with it. Copied,
//! the two tests would evolve their own versions of the demo while they are meant to watch
//! one model.
//!
//! The 22 files fall into two kinds: `route.all.json`, `system.json`, and `meta.json` are
//! inputs — the demo model itself, and editing them by hand is editing the demo; the other
//! 19 are outputs, compared byte for byte.
//!
//! Those 19 outputs must not be hand-written. To change them, run
//! `BROCADE_UPDATE_GOLDEN=1 cargo test -p brocade-core --test demo_raw`, then read the
//! `git diff` file by file before committing. They were originally produced by a
//! JavaScript-side demo implementation and compared byte for byte against Rust; that has
//! since been removed (see OPENSOURCE_TODO.md §G), so their character now is a snapshot —
//! able to catch unintended changes in the artifacts, unable to catch a misunderstanding of
//! the specification itself.

#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

use brocade_core::model::{
    Accept, Action, AppView, Chain, DestMatch, Dns, DomainStrategy, Front, FrontStrategy, Grant,
    HopDial, HopIn, HopPool, HopWire, Ingress, IngressWires, ModelSnapshot, Network, Node, Rule,
    Step, Transport, User, WireGuardKeys,
};
use serde_json::Value;

pub fn demo_snapshot() -> ModelSnapshot {
    let route = read_json(&golden_file("route.all.json"));
    let system = read_json(&golden_file("system.json"));
    let meta = read_json(&golden_file("meta.json"));
    let system_nodes = system_nodes(&system);

    ModelSnapshot {
        revision: u64_value(&route["revision"]),
        overlay_cidr: str_value(&system["overlay_cidr"]).parse().unwrap(),
        settings: Default::default(),
        nodes: array(&route["nodes"])
            .iter()
            .map(|node| model_node(node, &system_nodes))
            .collect(),
        node_egress_dns: Vec::new(),
        users: array(&route["users"]).iter().map(model_user).collect(),
        external_outbounds: Vec::new(),
        apps: array(&meta["apps"])
            .iter()
            .map(|app| model_app(app, &route))
            .collect(),
    }
}

fn model_node(node: &Value, system_nodes: &BTreeMap<String, SystemNodeFixture>) -> Node {
    let id = str_value(&node["id"]);
    let system = system_nodes.get(id).unwrap();

    Node {
        mtu: None,
        connection: Default::default(),
        retired: false,
        id: id.to_owned(),
        tenant: str_value(&node["tenant"]).to_owned(),
        name: str_value(&node["name"]).to_owned(),
        // In the baseline this field is still called `host`, its name before the
        // dual-stack split. Both names are accepted, so that touching the baseline does not
        // require changing the mapping in step. The demo has one address, so the v6 side is
        // left empty and both NAT flags are false.
        public_ipv4: optional_string(&node["public_ipv4"])
            .or_else(|| optional_string(&node["host"])),
        public_ipv6: optional_string(&node["public_ipv6"]),
        public_ipv4_nat: bool_value(node.get("public_ipv4_nat").unwrap_or(&Value::Bool(false))),
        public_ipv6_nat: bool_value(node.get("public_ipv6_nat").unwrap_or(&Value::Bool(false))),
        overlay_addr: system.overlay_addr,
        certificate_name: None,
        wireguard: WireGuardKeys {
            private_key: system.private_key.clone(),
            public_key: system.public_key.clone(),
            listen_port: system.listen_port,
            transport: Default::default(),
        },
        api_port: optional_u16(&node["api_port"]),
        overlay: true,
        egress_allowed: bool_value(&node["egress_allowed"]),
        dns: match str_value(&node["dns"]["t"]) {
            "system" => Dns::System,
            "servers" => Dns::Servers(string_array(&node["dns"]["v"])),
            value => panic!("unknown dns kind {value}"),
        },
        // Absent from the baseline, which predates the field. Taking the default here is
        // what keeps the golden artifacts byte-identical: it is the value the artifact
        // layer used to hard-code.
        domain_strategy: match node.get("domain_strategy") {
            None => DomainStrategy::default(),
            Some(value) => serde_json::from_value(value.clone())
                .unwrap_or_else(|error| panic!("unknown domain strategy {value}: {error}")),
        },
    }
}

fn model_user(user: &Value) -> User {
    User {
        id: str_value(&user["id"]).to_owned(),
        tenant: str_value(&user["tenant"]).to_owned(),
        uuid: str_value(&user["uuid"]).to_owned(),
    }
}

fn model_app(app: &Value, route: &Value) -> AppView {
    let app_id = str_value(&app["id"]);
    AppView {
        id: app_id.to_owned(),
        label: str_value(&app["label"]).to_owned(),
        chains: array(&route["chains"])
            .iter()
            .filter(|value| belongs_to_app(value, app_id))
            .map(model_chain)
            .collect(),
        ingresses: array(&route["ingresses"])
            .iter()
            .filter(|value| belongs_to_app(value, app_id))
            .map(model_ingress)
            .collect(),
        fronts: array(&route["fronts"])
            .iter()
            .filter(|value| belongs_to_app(value, app_id))
            .map(model_front)
            .collect(),
        steps: array(&route["steps"])
            .iter()
            .filter(|value| belongs_to_app(value, app_id))
            .map(model_step)
            .collect(),
        grants: array(&route["grants"])
            .iter()
            .filter(|value| belongs_to_app(value, app_id))
            .map(model_grant)
            .collect(),
    }
}

fn model_chain(chain: &Value) -> Chain {
    Chain {
        id: str_value(&chain["id"]).to_owned(),
        tenant: str_value(&chain["tenant"]).to_owned(),
        name: str_value(&chain["name"]).to_owned(),
        subscription_country: chain["subscription_country"].as_str().map(str::to_owned),
    }
}

fn model_ingress(ingress: &Value) -> Ingress {
    let reality = &ingress["transport"];
    Ingress {
        id: str_value(&ingress["id"]).to_owned(),
        chain: str_value(&ingress["chain"]).to_owned(),
        node: str_value(&ingress["node"]).to_owned(),
        bind: str_value(&ingress["bind"]).parse().unwrap(),
        port: u16_value(&ingress["port"]),
        front: optional_string(&ingress["front"]),
        projection: Default::default(),
        // Open, so that the goldens keep measuring what they were written to measure. The
        // guard's own effect on the rule table is asserted in compile.rs, where it is the
        // subject rather than four extra rules in front of every other assertion.
        guard: brocade_core::model::IngressGuard::OPEN,
        identity: brocade_core::model::IngressIdentity {
            private_key: str_value(&reality["private_key"]).to_owned(),
            public_key: str_value(&reality["public_key"]).to_owned(),
            short_ids: string_array(&reality["short_ids"]),
        },
        wires: IngressWires::Vless(Transport::VlessReality(
            brocade_core::model::RealitySettings {
                dest: str_value(&reality["dest"]).to_owned(),
                server_names: string_array(&reality["server_names"]),
                fingerprint: str_value(&reality["fingerprint"]).to_owned(),
                flow: optional_string(&reality["flow"]),
                fallback_mode: Default::default(),
                fallback_guard: true,
                fallback_limits: Default::default(),
            },
        )),
    }
}

fn model_front(front: &Value) -> Front {
    Front {
        id: str_value(&front["id"]).to_owned(),
        tenant: str_value(&front["tenant"]).to_owned(),
        name: str_value(&front["name"]).to_owned(),
        via: string_array(&front["via"]),
        external_via: front["external_via"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .map(|value| str_value(value).to_owned())
                    .collect()
            })
            .unwrap_or_default(),
        strategy: match str_value(&front["strategy"]) {
            "url-test" => FrontStrategy::UrlTest,
            "select" => FrontStrategy::Select,
            "fallback" => FrontStrategy::Fallback,
            value => panic!("unknown front strategy {value}"),
        },
    }
}

fn model_step(step: &Value) -> Step {
    Step {
        chain: str_value(&step["chain"]).to_owned(),
        node: str_value(&step["node"]).to_owned(),
        accept: if step["accept"].is_null() {
            None
        } else {
            Some(Accept {
                uuid: str_value(&step["accept"]["uuid"]).to_owned(),
                label: str_value(&step["accept"]["label"]).to_owned(),
            })
        },
        hop_in: if step["hop_in"].is_null() {
            None
        } else {
            Some(HopIn {
                port: u16_value(&step["hop_in"]["port"]),
                security: match str_value(&step["hop_in"]["security"]["t"]) {
                    "none" => HopWire::None,
                    value => panic!("demo 里还没有 {value} 这一档中转口传输层"),
                },
            })
        },
        rules: array(&step["rules"]).iter().map(model_rule).collect(),
    }
}

fn model_rule(rule: &Value) -> Rule {
    Rule {
        dest_match: model_match(&rule["m"]),
        action: model_action(&rule["a"]),
    }
}

fn model_match(value: &Value) -> DestMatch {
    match str_value(&value["t"]) {
        "any" => DestMatch::Any,
        "domain_suffix" => DestMatch::DomainSuffix(csv(&value["v"])),
        "domain_keyword" => DestMatch::DomainKeyword(csv(&value["v"])),
        "domain_regex" => DestMatch::DomainRegex(str_value(&value["v"]).to_owned()),
        "geosite" => DestMatch::Geosite(csv(&value["v"])),
        "ip_cidr" => DestMatch::IpCidr(csv(&value["v"])),
        "geoip" => DestMatch::Geoip(csv(&value["v"])),
        "port" => DestMatch::Port(csv(&value["v"])),
        "network" => match str_value(&value["v"]) {
            "tcp" => DestMatch::Network(Network::Tcp),
            "udp" => DestMatch::Network(Network::Udp),
            value => panic!("unknown network {value}"),
        },
        "front_downstream" => DestMatch::FrontDownstream,
        value => panic!("unknown match kind {value}"),
    }
}

fn model_action(value: &Value) -> Action {
    match str_value(&value["t"]) {
        // The demo JSON has no routing selection (everything goes auto). Take care not to
        // read `value["via"]`: in the egress variant "via" is the exit IP, which is not the
        // same thing as the selection here.
        // `pool` is read from the fixture, `dial` is not: every demo hop takes the overlay, but
        // the demo has to carry at least one pooled hop or the whole `mux` path compiles with no
        // baseline behind it — and the first time it regressed, every golden file would still
        // match.
        "forward" => Action::Forward {
            to: str_value(&value["to"]).to_owned(),
            dial: HopDial::Overlay,
            pool: match optional_string(&value["pool"]) {
                None => HopPool::None,
                Some(spec) if spec == "pool" => HopPool::Pool,
                Some(spec) => HopPool::Merge(
                    spec.strip_prefix("merge:")
                        .and_then(|n| n.parse().ok())
                        .unwrap_or_else(|| panic!("unknown pool spec {spec}")),
                ),
            },
        },
        "egress" => Action::Egress {
            send_through: optional_string(&value["via"]).map(|value| value.parse().unwrap()),
        },
        "block" => Action::Block,
        value => panic!("unknown action kind {value}"),
    }
}

fn model_grant(grant: &Value) -> Grant {
    Grant {
        tenant: str_value(&grant["tenant"]).to_owned(),
        user: str_value(&grant["user"]).to_owned(),
        ingress: str_value(&grant["ingress"]).to_owned(),
    }
}

fn system_nodes(system: &Value) -> BTreeMap<String, SystemNodeFixture> {
    array(&system["nodes"])
        .iter()
        .map(|node| {
            (
                str_value(&node["id"]).to_owned(),
                SystemNodeFixture {
                    overlay_addr: str_value(&node["overlay_addr"]).parse().unwrap(),
                    private_key: str_value(&node["wg"]["private_key"]).to_owned(),
                    public_key: str_value(&node["wg"]["public_key"]).to_owned(),
                    listen_port: u16_value(&node["wg"]["listen_port"]),
                },
            )
        })
        .collect()
}

#[derive(Debug, Clone)]
struct SystemNodeFixture {
    overlay_addr: Ipv4Addr,
    private_key: String,
    public_key: String,
    listen_port: u16,
}

// With `BROCADE_UPDATE_GOLDEN=1`, rewrite the baseline instead of asserting.
//
// It is needed when the artifact format changed deliberately — otherwise the only recourse
// is editing 19 files by hand, which is exactly what "no hand editing" prevents. After
// updating, the `git diff` must be read file by file: this switch turns the test from
// "catch unintended changes" into "accept this change", and a diff one cannot follow is a
// change one has not thought through.
fn updating() -> bool {
    std::env::var_os("BROCADE_UPDATE_GOLDEN").is_some_and(|v| v == "1")
}

fn rewrite(path: &Path, actual: &str) {
    fs::write(
        path,
        if actual.ends_with('\n') {
            actual.to_owned()
        } else {
            format!("{actual}\n")
        },
    )
    .unwrap_or_else(|error| panic!("写不进 {}: {error}", path.display()));
    eprintln!("更新基线 {}", path.display());
}

pub fn assert_json_file_eq(path: &Path, actual: &str) {
    if updating() {
        return rewrite(path, actual);
    }
    assert_eq!(
        read_json(path),
        parse_json(actual),
        "JSON differs for {}",
        path.display(),
    );
}

pub fn assert_text_file_eq(path: &Path, actual: &str) {
    if updating() {
        return rewrite(path, actual);
    }
    assert_eq!(
        read_text(path).trim_end(),
        actual.trim_end(),
        "text differs for {}",
        path.display(),
    );
}

pub fn golden_file(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn read_json(path: &Path) -> Value {
    parse_json(&read_text(path))
}

fn parse_json(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn belongs_to_app(value: &Value, app_id: &str) -> bool {
    str_value(&value["app"]) == app_id
}

fn csv(value: &Value) -> Vec<String> {
    str_value(value)
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn string_array(value: &Value) -> Vec<String> {
    array(value)
        .iter()
        .map(|value| str_value(value).to_owned())
        .collect()
}

fn array(value: &Value) -> &[Value] {
    value.as_array().unwrap()
}

fn str_value(value: &Value) -> &str {
    value.as_str().unwrap()
}

fn optional_string(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn bool_value(value: &Value) -> bool {
    value.as_bool().unwrap()
}

fn u64_value(value: &Value) -> u64 {
    value.as_u64().unwrap()
}

fn u16_value(value: &Value) -> u16 {
    u64_value(value).try_into().unwrap()
}

fn optional_u16(value: &Value) -> Option<u16> {
    value.as_u64().map(|value| value.try_into().unwrap())
}
