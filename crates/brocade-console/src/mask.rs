//! Hiding asset identifiers and credentials from viewers who are not allowed to keep them.
//!
//! A `readonly` operator is a reviewer: they read the model to check it, and they
//! must not walk away with the addresses or UUID credentials. So every IP, domain and port on
//! the admin read surface is replaced, and every `uuid` member is removed, before it leaves the
//! process.
//!
//! # Where it happens
//!
//! Server side, in one place — the middleware in `http`. Masking in the front end
//! would be theatre: the real values would still be on the wire and one open dev
//! console away. It is also why this walks the finished JSON rather than each
//! response type: a masking rule written per struct is a rule somebody forgets to
//! apply to the struct they add next month, and the failure is silent.
//!
//! # What survives
//!
//! Enough shape to review by, never enough to dial:
//!
//! ```text
//! 123.123.45.67     -> 123.123.***.***     the first two octets place the network
//! 2001:db8:1f::c0a8 -> 2001:db8:***        the same idea, first two groups
//! sg-01.example.net -> ***.net             only the top-level domain
//! 51820             -> ***                 a port narrows the search either way
//! ```

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde_json::Value;

const HIDDEN: &str = "***";

/// Keys whose value is a host — an address or a name. Needed because a domain has
/// no reliable test by value: `sg-01.example.net` and a tenant path like
/// `acme.cn.sales` are the same shape, and only the field name says which is which.
///
/// Matched **whole**, never as a substring. `total_targets`, `changed_targets` and
/// `skipped_counters` are counts, and a `contains("target")` rule would replace a
/// deployment's numbers with `***` — a mask that mangles the page it is protecting
/// gets switched off, and then nothing is protected.
const HOST_KEYS: &[&str] = &[
    "address",
    "addr",
    // REALITY's borrowed site, as `host:port`. It needs naming here because the
    // key-independent pass cannot reach it: that one only treats `host:port` as an
    // endpoint when the host parses as an address, and this one is a domain
    // (`apps.apple.com:443`). Left alone it is the one field on the settings page that
    // still names a real host to a reviewer.
    "dest",
    "dial_host",
    "dns_servers",
    "endpoint",
    "endpoint_host",
    "exit_ip",
    "expected_exit_ips",
    "host",
    "ipv4",
    "ipv6",
    "overlay_addr",
    "peer_endpoint",
    "public_ipv4",
    "public_ipv6",
    "server_name",
    "server_names",
    "servers",
    "sni",
];

/// Keys whose value is prose. The sentence goes; nothing is masked inside it.
///
/// A compiler diagnostic reads `overlay 地址 10.66.0.2 与 sg-01 重复`. The address sits
/// inside a sentence, so the value-based pass never fires — that pass recognizes a
/// string that *is* an address, not one that contains one. Reaching inside would mean
/// hunting address-shaped tokens in free text, and two of the three kinds cannot be
/// told from ordinary content: a bare port is a number like any count (`端口 20000`
/// against `20000 条`), and a domain is a dotted label like a tenant path. Guessing
/// there yields either a leak or a mangled sentence, and this module's whole stance is
/// that a mask which mangles the page gets switched off, after which nothing is
/// protected.
///
/// So the skeleton stays and the sentence goes. `level`, `code` and `location` say
/// which check failed, how severe it is, and on which asset — enough to review by and
/// enough to ask about — while the summary counts are untouched.
const PROSE_KEYS: &[&str] = &[
    "detail", "details", "error", "message", "note", "notes", "reason", "warning", "warnings",
];

/// What replaces prose: nothing at all.
///
/// Not `***`, which says "a value was blanked" when a whole sentence was dropped. And
/// not a sentence explaining the omission either — that was tried, and a line that has
/// to both apologize and point elsewhere manages neither. A row carrying a level, a
/// code and a location with no detail beside it already reads as what it is.
///
/// The field stays, empty, rather than being removed: it is declared on the client's
/// `Diagnostic` type, and an absent one turns a rendered blank into an `undefined`.
const PROSE_HIDDEN: &str = "";

/// Keys whose value is a port number. The number becomes the string `"***"`: a port
/// has no neutral value to stand in for it — 0 reads as "not set", and any other
/// number is a lie that somebody will eventually dial.
const PORT_KEYS: &[&str] = &[
    "api_port",
    // `start`/`end` are the bounds of a Hysteria 2 port-hop range, and nothing else on the
    // read surface is a bare `start`/`end`: usage windows are `window_start` / `month_end`,
    // never the bare word. Left alone they hand a reviewer the exact UDP range every ingress
    // redirects to its listener.
    "end",
    "hop_base",
    "hy2_base",
    "ingress_base",
    "listen_port",
    // REALITY split-download's TLS origin port — the port the node actually listens on.
    "origin_port",
    "port",
    "start",
    // phantun's fake-TCP port. Not `wg_listen_port`, which is the UDP side already listed:
    // missed here, it was the one port on a node's agent state that survived masking.
    "wg_fake_tcp_port",
    "wg_listen_port",
];

/// Mask an entire response body in place.
pub fn mask_json(value: &mut Value) {
    match value {
        Value::String(text) => {
            if let Some(masked) = mask_free_text(text) {
                *text = masked;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(mask_json),
        Value::Object(map) => {
            // A UUID is a usable VLESS credential, not review material. Remove the member rather
            // than replacing its value: `***` still suggests a field callers may rely on, while
            // the readonly wire contract deliberately does not carry it at all. This is done at
            // every object depth so users, compiled clients and observed grant state all follow
            // the same rule.
            map.remove("uuid");
            for (key, child) in map.iter_mut() {
                mask_member(key, child);
            }
        }
        _ => {}
    }
}

fn mask_member(key: &str, value: &mut Value) {
    if PROSE_KEYS.contains(&key) {
        hide_prose(value);
        return;
    }
    if PORT_KEYS.contains(&key) && (value.is_number() || value.is_string()) {
        *value = Value::String(HIDDEN.to_owned());
        return;
    }
    if HOST_KEYS.contains(&key) {
        match value {
            Value::String(text) => {
                *text = mask_endpoint(text);
                return;
            }
            Value::Array(items) => {
                for item in items.iter_mut() {
                    if let Value::String(text) = item {
                        *text = mask_endpoint(text);
                    } else {
                        mask_json(item);
                    }
                }
                return;
            }
            // A host-ish key holding an object is one of the tagged enums (`Dns`,
            // `Projection`): keep walking, the leaves inside carry their own names.
            _ => {}
        }
    }
    mask_json(value);
}

fn hide_prose(value: &mut Value) {
    match value {
        Value::String(text) => *text = PROSE_HIDDEN.to_owned(),
        Value::Array(items) => items.iter_mut().for_each(hide_prose),
        Value::Object(map) => map.values_mut().for_each(hide_prose),
        _ => {}
    }
}

/// The key-independent pass, applied to every string in the body.
///
/// It is what keeps this honest as the API grows: a field added tomorrow, nested
/// three levels inside an observed state nobody remembers, still gets masked as
/// long as it holds something that really parses as an address. The tests here are
/// deliberately strict parses rather than "looks like" patterns — `26.4` is a
/// version, `12:30:00` is a clock, and neither may be touched.
fn mask_free_text(text: &str) -> Option<String> {
    if text.contains("://") {
        return Some(mask_url(text));
    }
    if text.parse::<IpAddr>().is_ok() {
        return Some(mask_host(text));
    }
    // An overlay allocation is written as a CIDR (`10.66.0.2/32`), which parses as
    // neither an address nor an endpoint
    if let Some((addr, prefix)) = text.split_once('/') {
        if addr.parse::<IpAddr>().is_ok() && prefix.chars().all(|c| c.is_ascii_digit()) {
            return Some(format!("{}/{prefix}", mask_host(addr)));
        }
    }
    match split_host_port(text) {
        Some((host, _)) if host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() => {
            Some(mask_endpoint(text))
        }
        _ => None,
    }
}

/// One host, with no port. An address keeps its leading groups, a name keeps only
/// its top-level domain, and anything else keeps nothing.
pub fn mask_host(host: &str) -> String {
    if let Some(inner) = host.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        return format!("[{}]", mask_host(inner));
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(addr)) => return mask_ipv4(addr),
        Ok(IpAddr::V6(addr)) => return mask_ipv6(addr),
        Err(_) => {}
    }
    match host.rsplit_once('.') {
        Some((_, tld)) if !tld.is_empty() && tld.chars().all(|c| c.is_ascii_alphanumeric()) => {
            format!("{HIDDEN}.{tld}")
        }
        // A bare label (`sg-01`, or a trailing-dot FQDN) has no suffix worth
        // keeping
        _ => HIDDEN.to_owned(),
    }
}

fn mask_ipv4(addr: Ipv4Addr) -> String {
    let octets = addr.octets();
    format!("{}.{}.{HIDDEN}.{HIDDEN}", octets[0], octets[1])
}

fn mask_ipv6(addr: Ipv6Addr) -> String {
    let groups = addr.segments();
    format!("{:x}:{:x}:{HIDDEN}", groups[0], groups[1])
}

/// A host that may carry a port: `1.2.3.4:443`, `[2001:db8::1]:443`, `sg.example.net`.
pub fn mask_endpoint(text: &str) -> String {
    // A bare IPv6 address is full of colons and must be recognized before anything
    // splits on the last one — `2001:db8::1` would otherwise read as host
    // `2001:db8:` and port `1`.
    if text.parse::<IpAddr>().is_ok() {
        return mask_host(text);
    }
    match split_host_port(text) {
        Some((host, _)) => format!("{}:{HIDDEN}", mask_host(host)),
        None => mask_host(text),
    }
}

/// Split on the last colon, and only where what follows is a port. Returns the host
/// with any brackets left on, so the caller can tell `[::1]:80` from a name.
fn split_host_port(text: &str) -> Option<(&str, &str)> {
    let (host, port) = text.rsplit_once(':')?;
    if host.is_empty() || port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // Everything before the colon must be a plausible host: bracketed v6, an
    // address, or a name. Without this an ordinary `12:30` reads as an endpoint.
    let bare = host.trim_matches(['[', ']']);
    let plausible = host.starts_with('[')
        || bare.parse::<IpAddr>().is_ok()
        || bare
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    plausible.then_some((host, port))
}

/// Mask the authority of a URL, plus the query parameters that carry a host of
/// their own — `sni=`, `host=` and the rest are how a share link states its
/// address a second time.
fn mask_url(text: &str) -> String {
    let Some((scheme, rest)) = text.split_once("://") else {
        return text.to_owned();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    // `user@host` — the userinfo is a credential on this review surface. Structured artifacts
    // are refused before this layer, but masking it here closes any future JSON wrapper carrying
    // a share URL.
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((_, host)) => (format!("{HIDDEN}@"), host),
        None => (String::new(), authority),
    };
    format!(
        "{scheme}://{userinfo}{}{}",
        mask_endpoint(hostport),
        mask_query_hosts(tail)
    )
}

const HOST_PARAMS: &[&str] = &["add", "address", "host", "peer", "server", "sni", "spx"];

fn mask_query_hosts(tail: &str) -> String {
    if !tail.contains('=') {
        return tail.to_owned();
    }
    let mut out = String::with_capacity(tail.len());
    let mut rest = tail;
    while !rest.is_empty() {
        let cut = rest.find(['&', '#']).map(|i| i + 1).unwrap_or(rest.len());
        let (piece, next) = rest.split_at(cut);
        let separator = if piece.ends_with(['&', '#']) {
            piece.len() - 1
        } else {
            piece.len()
        };
        let (pair, sep) = piece.split_at(separator);
        match pair.split_once('=') {
            Some((name, value))
                if HOST_PARAMS.contains(&name.trim_start_matches(['?', '&']))
                    && !value.is_empty() =>
            {
                out.push_str(name);
                out.push('=');
                out.push_str(&mask_endpoint(value));
            }
            _ => out.push_str(pair),
        }
        out.push_str(sep);
        rest = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_address_keeps_its_first_two_groups_and_a_name_keeps_its_suffix() {
        assert_eq!(mask_host("123.123.45.67"), "123.123.***.***");
        assert_eq!(mask_host("10.66.0.2"), "10.66.***.***");
        assert_eq!(mask_host("2001:db8:1f::c0a8"), "2001:db8:***");
        assert_eq!(mask_host("sg-01.example.net"), "***.net");
        assert_eq!(mask_host("example.net"), "***.net");
        // Nothing worth keeping: a bare label would be the asset's own name
        assert_eq!(mask_host("sg-01"), "***");
    }

    #[test]
    fn a_port_is_never_partly_kept() {
        assert_eq!(mask_endpoint("123.123.45.67:51820"), "123.123.***.***:***");
        assert_eq!(mask_endpoint("[2001:db8::1]:8443"), "[2001:db8:***]:***");
        assert_eq!(mask_endpoint("sg-01.example.net:443"), "***.net:***");
        // A bare v6 address is not a host:port, however many colons it has
        assert_eq!(mask_endpoint("2001:db8::1"), "2001:db8:***");
    }

    /// REALITY's borrowed site is a host too, and the settings page is where a reviewer
    /// meets it. It is masked by key rather than by value: a domain with a port is
    /// indistinguishable from ordinary text once the host stops parsing as an address,
    /// so the free-text pass never reaches it.
    #[test]
    fn the_borrowed_site_is_masked_wherever_it_appears() {
        let mut settings = json!({
            "reality_site": {
                "dest": "apps.apple.com:443",
                "server_names": ["apps.apple.com"],
                "fingerprint": "chrome",
                "flow": "xtls-rprx-vision",
            }
        });
        mask_json(&mut settings);
        assert_eq!(settings["reality_site"]["dest"], "***.com:***");
        assert_eq!(settings["reality_site"]["server_names"][0], "***.com");
        // Neither of these names a host, and blanking them would cost the reviewer the
        // very thing they are reviewing.
        assert_eq!(settings["reality_site"]["fingerprint"], "chrome");
        assert_eq!(settings["reality_site"]["flow"], "xtls-rprx-vision");

        // The same key carries the same thing on an ingress's transport, and it is the
        // per-struct rules this module exists to avoid that would have masked one and
        // missed the other.
        let mut ingress = json!({ "transport": { "t": "reality", "v": {
            "dest": "www.microsoft.com:443",
            "server_names": ["www.microsoft.com"],
        }}});
        mask_json(&mut ingress);
        assert_eq!(ingress["transport"]["v"]["dest"], "***.com:***");
    }

    /// A diagnostic's sentence carries whatever the compiler had to say, and what it
    /// had to say routinely includes an address, a port or a host. None of the three can
    /// be masked in place: this checks that the sentence is dropped whole, and that the
    /// skeleton a reviewer works from survives it.
    #[test]
    fn a_diagnostic_keeps_its_skeleton_and_loses_its_sentence() {
        let mut compiled = json!({
            "summary": { "errors": 1, "warnings": 2, "infos": 0, "can_publish": false },
            "diagnostics": [
                { "level": "error", "code": "overlay.dup", "location": "sg-01",
                  "message": "overlay 地址 10.66.0.2 与 hk-01 重复" },
                { "level": "warn", "code": "port.dup", "location": "hk-01",
                  "message": "端口 20000 同时被 in:hop 和 in:frt 占用" },
                { "level": "warn", "code": "rule.blocked", "location": "i-frt",
                  "message": "规则表把 sg-01.example.net 挡掉了" },
            ]
        });
        mask_json(&mut compiled);

        for diagnostic in compiled["diagnostics"].as_array().unwrap() {
            let message = diagnostic["message"].as_str().unwrap();
            // Empty, not absent: the client's `Diagnostic` declares the field, and
            // removing it would render `undefined` where a blank belongs.
            assert_eq!(message, "", "详情该整句丢掉：{message}");
            // The three that the value-based pass cannot reach: an address inside a
            // sentence, a bare port, and a host that is only a dotted label.
            for leaked in ["10.66.0.2", "20000", "sg-01.example.net"] {
                assert!(
                    !message.contains(leaked),
                    "{leaked} 漏在诊断里了：{message}"
                );
            }
        }
        // Reviewable still: which check, how severe, on what — and how many there are.
        let first = &compiled["diagnostics"][0];
        assert_eq!(first["code"], "overlay.dup");
        assert_eq!(first["level"], "error");
        assert_eq!(
            first["location"], "sg-01",
            "定位到哪个资产要留着，它是身份不是地址"
        );
        assert_eq!(compiled["summary"]["errors"], 1);
        assert_eq!(compiled["summary"]["warnings"], 2);
        assert_eq!(compiled["summary"]["can_publish"], false);
    }

    #[test]
    fn e2e_details_and_string_ports_cannot_leak_assets() {
        let mut response = json!({
            "probes": [{
                "status": "mismatch",
                "detail": "出口不一致：期望 198.51.100.7，实际 203.0.113.9",
                "hysteria2": { "port": "18443" },
                "warnings": ["节点 hk-01.example.net 使用端口 18443"]
            }]
        });

        mask_json(&mut response);

        assert_eq!(response["probes"][0]["detail"], "");
        assert_eq!(response["probes"][0]["hysteria2"]["port"], "***");
        assert_eq!(response["probes"][0]["warnings"][0], "");
        let encoded = response.to_string();
        for secret in ["198.51.100.7", "203.0.113.9", "18443", "hk-01.example.net"] {
            assert!(!encoded.contains(secret), "{secret} leaked in {encoded}");
        }
    }

    /// The key-independent pass is the one that has to survive the API growing, so
    /// it must not fire on things that merely resemble an address. Each of these
    /// appears in a real response.
    #[test]
    fn strings_that_are_not_addresses_are_left_alone() {
        for text in [
            "Xray 26.7.28 (Xray, Penetrates Everything.)",
            "26.4",
            "2026-08-06T14:51:20Z",
            "12:30:00",
            "acme.cn.sales",
            "broc_enroll_7f3a",
            "sha256:9f86d081884c7d659a2feaa0c55ad015",
        ] {
            assert_eq!(mask_free_text(text), None, "{text} 不该被当成地址");
        }
    }

    #[test]
    fn an_address_is_masked_wherever_it_sits_even_under_an_unknown_key() {
        // `agent_version` now carries the sha256 of the running agent binary, arriving as the
        // User-Agent. Kept in this test in its real shape: it is a long opaque string sitting
        // under a key nothing here claims, which is exactly the sort of value a broad rule
        // eventually eats — and masked, the one number an operator compares during a rollout
        // becomes a row of asterisks.
        let agent =
            "brocade-agent/9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        let mut value = json!({
            "observed": { "some_future_field": "198.51.100.7", "cidr": "10.66.0.2/32" },
            "counts": { "total_targets": 12, "changed_targets": 3 },
            "agent_version": agent,
        });
        mask_json(&mut value);

        assert_eq!(value["observed"]["some_future_field"], "198.51.***.***");
        assert_eq!(value["observed"]["cidr"], "10.66.***.***/32");
        // Counts are not addresses. A substring rule on "target" would have eaten
        // these, and a report whose numbers are all *** is a report nobody keeps on
        assert_eq!(value["counts"]["total_targets"], 12);
        assert_eq!(value["counts"]["changed_targets"], 3);
        assert_eq!(value["agent_version"], agent);
    }

    #[test]
    fn uuid_credentials_are_removed_at_every_depth() {
        let mut value = json!({
            "users": [{ "id": "alice", "uuid": "user-secret" }],
            "compiled": {
                "clients": [{ "email": "alice@example", "uuid": "compiled-secret" }],
                "observed": { "uuid": "observed-secret", "state": "present" },
            },
        });
        mask_json(&mut value);

        let text = value.to_string();
        assert!(!text.contains("user-secret"), "{text}");
        assert!(!text.contains("compiled-secret"), "{text}");
        assert!(!text.contains("observed-secret"), "{text}");
        assert!(!text.contains("\"uuid\""), "{text}");
        assert_eq!(value["users"][0]["id"], "alice");
        assert_eq!(value["compiled"]["observed"]["state"], "present");
    }

    #[test]
    fn a_name_is_masked_only_where_the_key_says_it_is_a_host() {
        let mut value = json!({
            "nodes": [{
                "id": "sg-01",
                "name": "新加坡 01",
                "tenant": "acme.cn.sales",
                "public_ipv4": "123.123.45.67",
                "overlay_addr": "10.66.0.2",
                "api_port": 10085,
            }],
            "ingress": { "host": "sg-01.example.net", "port": 8443 },
        });
        mask_json(&mut value);

        let node = &value["nodes"][0];
        assert_eq!(node["public_ipv4"], "123.123.***.***");
        assert_eq!(node["overlay_addr"], "10.66.***.***");
        assert_eq!(node["api_port"], "***");
        assert_eq!(value["ingress"]["host"], "***.net");
        assert_eq!(value["ingress"]["port"], "***");
        // The identity of the machine is what the review is about, and a tenant
        // path is not an address however much it looks like a domain
        assert_eq!(node["id"], "sg-01");
        assert_eq!(node["name"], "新加坡 01");
        assert_eq!(node["tenant"], "acme.cn.sales");
    }

    #[test]
    fn a_url_loses_its_host_in_the_authority_and_in_the_query() {
        let mut value = json!({
            "endpoint_url": "http://cp.cloudflare.com/cdn-cgi/trace",
            "script_url": "https://console.example.net:8443/enroll/install.sh",
            "share": "vless://5f3a-uuid@123.123.45.67:8443?sni=www.example.org&flow=xtls-rprx-vision#sg-01",
        });
        mask_json(&mut value);

        assert_eq!(value["endpoint_url"], "http://***.com/cdn-cgi/trace");
        assert_eq!(value["script_url"], "https://***.net:***/enroll/install.sh");
        assert_eq!(
            value["share"],
            "vless://***@123.123.***.***:***?sni=***.org&flow=xtls-rprx-vision#sg-01"
        );
    }

    /// The three ports that used to survive masking: the hop range's `start`/`end` under a
    /// bare key, phantun's `wg_fake_tcp_port`, and REALITY split-download's `origin_port`.
    /// Each is a real port a reviewer could dial, and each is masked the same way as `port`.
    #[test]
    fn the_ports_that_used_to_survive_are_masked_now() {
        let mut value = json!({
            "nodes": [{ "id": "sg-01", "wg_fake_tcp_port": 41230, "wg_listen_port": 51820 }],
            "ingress": {
                "port": 8443,
                "hysteria2": { "port": 20000, "hop": { "start": 20000, "end": 20015 } },
                "projection": { "download": { "origin_port": 8444 } },
            },
        });
        mask_json(&mut value);

        assert_eq!(value["nodes"][0]["wg_fake_tcp_port"], "***");
        assert_eq!(value["ingress"]["hysteria2"]["hop"]["start"], "***");
        assert_eq!(value["ingress"]["hysteria2"]["hop"]["end"], "***");
        assert_eq!(
            value["ingress"]["projection"]["download"]["origin_port"],
            "***"
        );
        // The node's own id is its identity, not an address, and stays put.
        assert_eq!(value["nodes"][0]["id"], "sg-01");
        let encoded = value.to_string();
        for port in ["41230", "20000", "20015", "8444"] {
            assert!(!encoded.contains(port), "{port} 漏在脱敏结果里：{encoded}");
        }
    }
}
