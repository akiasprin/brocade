//! Deciding whether a config change can be installed into a running xray.
//!
//! Shared on purpose. The agent uses it to choose between talking to the process and
//! restarting it; the control plane uses the same answer to decide whether a deployment is
//! destructive and therefore has to wave and be confirmed. Two implementations would drift,
//! and the drift is not symmetric: a control plane promising "nothing will drop" while the
//! agent restarts is a promise broken on live traffic.
//!
//! Routing changes use upstream AddRule with shouldAppend=false and the complete routing
//! configuration. The Agent checks the running kernel's atomic reload capability first.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// A bounded, reusable runtime alias. Telemetry folds its counters into the original hop.
pub const ENDPOINT_SWAP_SUFFIX: &str = "~brocade-endpoint-swap";
const ENDPOINT_RULE_PREFIX: &str = "brocade:endpoint-swap:";

pub enum HotSwapOperation {
    AddOutbounds(Vec<Value>),
    RemoveOutbounds(Vec<String>),
    ReloadRouting(Value),
}

pub struct EndpointSwap {
    pub outbounds: Vec<Value>,
    pub bridge_outbounds: Vec<Value>,
    pub bridge_rules: Vec<Value>,
}

/// What one xray can be made to accept without being restarted.
///
/// Only two things go in it. Routing rules can be swapped wholesale, and outbounds can be
/// added or taken away — everything else in the config (inbounds, dns, policy, the api
/// block, the balancers) either has no runtime verb at all or has one that cannot express
/// a change, and reaching those still means a restart.
pub struct HotSwap {
    /// Whole outbounds that did not exist before. They go in first: a rule may name one,
    /// and a rule naming an outbound that is not there yet routes into nothing.
    pub add_outbounds: Vec<Value>,
    /// Tags of outbounds that no longer appear. They come out last, once nothing refers
    /// to them any more.
    pub remove_outbounds: Vec<String>,
    /// Complete routing configuration, including balancers; None leaves it unchanged.
    pub reload_routing: Option<Value>,
    /// Temporary routing protects the same-tag replacement's remove/add interval.
    pub endpoint_swap: Option<EndpointSwap>,
}

impl HotSwap {
    /// All mutations in dependency order. A failed prefix keeps listeners and grants intact,
    /// so the Agent's existing handoff recovery can still install the desired configuration.
    pub fn operations(&self) -> Vec<HotSwapOperation> {
        use HotSwapOperation::*;
        let mut operations = Vec::new();
        if !self.add_outbounds.is_empty() {
            operations.push(AddOutbounds(self.add_outbounds.clone()));
        }
        if let Some(swap) = &self.endpoint_swap {
            let routing = self
                .reload_routing
                .as_ref()
                .expect("endpoint swaps always carry the complete routing configuration");
            let mut bridge = routing.clone();
            bridge["rules"] = Value::Array(swap.bridge_rules.clone());
            operations.extend([
                AddOutbounds(swap.bridge_outbounds.clone()),
                ReloadRouting(bridge),
                RemoveOutbounds(tags_of(&swap.outbounds, "tag")),
                AddOutbounds(swap.outbounds.clone()),
                ReloadRouting(routing.clone()),
                RemoveOutbounds(tags_of(&swap.bridge_outbounds, "tag")),
            ]);
        } else {
            if let Some(routing) = &self.reload_routing {
                operations.push(ReloadRouting(routing.clone()));
            }
        }
        if !self.remove_outbounds.is_empty() {
            operations.push(RemoveOutbounds(self.remove_outbounds.clone()));
        }
        operations
    }
}

fn tags_of(entries: &[Value], key: &str) -> Vec<String> {
    entries
        .iter()
        .map(|entry| {
            entry[key]
                .as_str()
                .expect("hot_swap validates every outbound tag")
                .to_owned()
        })
        .collect()
}

/// Decides whether the difference between two configs is one a running xray can be talked
/// into, and if so what to say to it.
///
/// Deliberately conservative — every case it is unsure about ends as `None`, which costs a
/// restart that was happening anyway before this existed. Three of the exclusions are worth
/// naming:
///
/// - **Any inbound difference.** Replacing an inbound means removing it first (xray refuses
///   a second one under the same tag), which closes the listener and drops the users that
///   were added to it over gRPC. That is a real interruption and belongs to its own change,
///   not smuggled in behind a routing edit.
/// - **Any balancer difference.** Rule edits reload the complete routing configuration
///   against the existing observatory. Balancer changes remain excluded so selectors and
///   the startup probe configuration cannot diverge.
/// - **Same-tag changes other than ordinary VLESS endpoints.** Endpoint replacements use
///   a temporary outbound and routing table. Default, reverse and indirectly referenced
///   outbounds cannot be protected by that routing-table switch and remain excluded.
/// - **A WireGuard outbound appearing or disappearing.** HandlerService can acknowledge the
///   add/remove operation while the stateful WireGuard outbound is not yet carrying traffic.
///   Starting or retiring one therefore goes through a full xray restart; unrelated rule edits
///   may still be hot-swapped while an unchanged WireGuard outbound remains installed.
pub fn hot_swap(previous: &str, desired: &str) -> Option<HotSwap> {
    let (before, after): (Value, Value) = (
        serde_json::from_str(previous).ok()?,
        serde_json::from_str(desired).ok()?,
    );
    let (before, after) = (before.as_object()?, after.as_object()?);

    // Every key the swap cannot speak about has to be identical, including keys that appear
    // on one side only — iterating one map would miss a block that was added or dropped.
    let untouched = |value: &serde_json::Map<String, Value>, key: &str| -> Option<Value> {
        match key {
            "outbounds" => None,
            "routing" => value.get(key).and_then(|routing| {
                let mut routing = routing.as_object()?.clone();
                routing.remove("rules");
                Some(Value::Object(routing))
            }),
            _ => Some(value.get(key).cloned().unwrap_or(Value::Null)),
        }
    };
    for key in before.keys().chain(after.keys()) {
        if untouched(before, key) != untouched(after, key) {
            return None;
        }
    }

    let by_tag =
        |value: &serde_json::Map<String, Value>, key: &str| -> Option<BTreeMap<String, Value>> {
            let mut tagged = BTreeMap::new();
            for entry in value.get(key)?.as_array()? {
                let tag = entry.get("tag")?.as_str()?;
                if tag.is_empty() || tagged.insert(tag.to_owned(), entry.clone()).is_some() {
                    return None;
                }
            }
            Some(tagged)
        };
    let (before_outbounds, after_outbounds) =
        (by_tag(before, "outbounds")?, by_tag(after, "outbounds")?);

    // The first outbound is the one xray sends anything no rule matched to, and it is bound
    // once at startup: measured on 26.4.25, an outbound added at runtime does not take that
    // role over. So adding is safe and removing the first one is not — the default would go
    // on naming something that is no longer there. Compared positionally because the map
    // above is keyed by tag and has already lost the order this depends on.
    let head = |value: &serde_json::Map<String, Value>| -> Option<String> {
        Some(
            value
                .get("outbounds")?
                .as_array()?
                .first()?
                .get("tag")?
                .as_str()?
                .to_owned(),
        )
    };
    if head(before)? != head(after)? {
        return None;
    }

    let changed = after_outbounds
        .iter()
        .filter(|(tag, outbound)| {
            before_outbounds
                .get(*tag)
                .is_some_and(|old| old != *outbound)
        })
        .map(|(tag, outbound)| (tag.clone(), outbound.clone()))
        .collect::<BTreeMap<_, _>>();
    for (tag, outbound) in &changed {
        if *tag == head(before)? || !vless_endpoint_only(&before_outbounds[tag], outbound) {
            return None;
        }
    }

    let is_wireguard =
        |outbound: &Value| outbound.get("protocol").and_then(Value::as_str) == Some("wireguard");
    let adds_wireguard = after_outbounds
        .iter()
        .any(|(tag, outbound)| !before_outbounds.contains_key(tag) && is_wireguard(outbound));
    let removes_wireguard = before_outbounds
        .iter()
        .any(|(tag, outbound)| !after_outbounds.contains_key(tag) && is_wireguard(outbound));
    if adds_wireguard || removes_wireguard {
        return None;
    }

    let rules = |value: &serde_json::Map<String, Value>| -> Option<Vec<Value>> {
        Some(value.get("routing")?.get("rules")?.as_array()?.clone())
    };
    let (before_rules, after_rules) = (rules(before)?, rules(after)?);
    let valid_tags = |rules: &[Value]| {
        let mut tags = BTreeSet::new();
        rules.iter().all(|rule| match rule.get("ruleTag") {
            Some(Value::String(tag)) if !tag.is_empty() => tags.insert(tag),
            None | Some(Value::Null) | Some(Value::String(_)) => true,
            _ => false,
        })
    };
    if !valid_tags(&after_rules) {
        return None;
    }

    let endpoint_swap = if changed.is_empty() {
        None
    } else {
        Some(endpoint_swap_plan(
            before,
            after,
            &changed,
            &before_rules,
            &after_rules,
        )?)
    };

    let reload_routing = if before_rules == after_rules && endpoint_swap.is_none() {
        None
    } else {
        Some(after.get("routing")?.clone())
    };

    Some(HotSwap {
        add_outbounds: after_outbounds
            .iter()
            .filter(|(tag, _)| !before_outbounds.contains_key(*tag))
            .map(|(_, outbound)| outbound.clone())
            .collect(),
        remove_outbounds: before_outbounds
            .keys()
            .filter(|tag| !after_outbounds.contains_key(*tag))
            .cloned()
            .collect(),
        reload_routing,
        endpoint_swap,
    })
}

fn vless_endpoint_only(before: &Value, after: &Value) -> bool {
    if before["protocol"] != "vless" || after["protocol"] != "vless" {
        return false;
    }
    let Some(old) = before.pointer("/settings/vnext").and_then(Value::as_array) else {
        return false;
    };
    let Some(new) = after.pointer("/settings/vnext").and_then(Value::as_array) else {
        return false;
    };
    if old.len() != 1 || new.len() != 1 {
        return false;
    }
    let Some(users) = old[0]["users"].as_array() else {
        return false;
    };
    if users.iter().any(|user| user.get("reverse").is_some()) {
        return false;
    }
    let mut normalized = after.clone();
    for key in ["address", "port"] {
        normalized["settings"]["vnext"][0][key] = old[0][key].clone();
    }
    normalized == *before
}

fn endpoint_swap_plan(
    before: &serde_json::Map<String, Value>,
    after: &serde_json::Map<String, Value>,
    changed: &BTreeMap<String, Value>,
    before_rules: &[Value],
    after_rules: &[Value],
) -> Option<EndpointSwap> {
    // The bridge protects explicit routes. Implicit default-outbound selection cannot
    // be redirected through a temporary endpoint alias.
    if before_rules.is_empty() || after_rules.is_empty() {
        return None;
    }
    for config in [before, after] {
        for (key, value) in config {
            if key != "routing" && has_indirect_reference(value, changed) {
                return None;
            }
        }
        for outbound in config.get("outbounds")?.as_array()? {
            if outbound["tag"].as_str()?.ends_with(ENDPOINT_SWAP_SUFFIX) {
                return None;
            }
        }
        let routing = config.get("routing")?;
        if let Some(balancers) = routing.get("balancers").and_then(Value::as_array) {
            for balancer in balancers {
                let references_changed = balancer["selector"].as_array()?.iter().any(|selector| {
                    selector
                        .as_str()
                        .is_some_and(|prefix| changed.keys().any(|tag| tag.starts_with(prefix)))
                }) || balancer["fallbackTag"]
                    .as_str()
                    .is_some_and(|tag| changed.contains_key(tag));
                if references_changed {
                    // The compiler's health-only balancer does not route application traffic.
                    // Real balancer selections / fallback tags cannot be redirected by our
                    // bridge table, and therefore must use handoff instead.
                    use brocade_core::artifacts::xray::{
                        HOP_HEALTH_BALANCER_TAG, NEVER_MATCH_DOMAIN,
                    };
                    if balancer["tag"] != HOP_HEALTH_BALANCER_TAG
                        || balancer.get("fallbackTag").is_some()
                        || before_rules.iter().chain(after_rules).any(|rule| {
                            rule["balancerTag"] == HOP_HEALTH_BALANCER_TAG
                                && rule["domain"] != serde_json::json!([NEVER_MATCH_DOMAIN])
                        })
                    {
                        return None;
                    }
                }
            }
        }
    }
    if before_rules.iter().chain(after_rules).any(|rule| {
        rule["ruleTag"]
            .as_str()
            .is_some_and(|tag| tag.starts_with(ENDPOINT_RULE_PREFIX))
    }) {
        return None;
    }
    let bridge_outbounds = changed
        .values()
        .map(|outbound| {
            let mut bridge = outbound.clone();
            bridge["tag"] = format!(
                "{}{ENDPOINT_SWAP_SUFFIX}",
                outbound["tag"].as_str().expect("validated outbound tag")
            )
            .into();
            bridge
        })
        .collect();
    let bridge_rules = after_rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            let mut bridge = rule.clone();
            bridge["ruleTag"] = format!("{ENDPOINT_RULE_PREFIX}{index}").into();
            if let Some(tag) = rule["outboundTag"]
                .as_str()
                .filter(|tag| changed.contains_key(*tag))
            {
                bridge["outboundTag"] = format!("{tag}{ENDPOINT_SWAP_SUFFIX}").into();
            }
            bridge
        })
        .collect();
    Some(EndpointSwap {
        outbounds: changed.values().cloned().collect(),
        bridge_outbounds,
        bridge_rules,
    })
}

fn has_indirect_reference(value: &Value, changed: &BTreeMap<String, Value>) -> bool {
    match value {
        Value::Object(object) => {
            object
                .get("proxySettings")
                .and_then(|settings| settings.get("tag"))
                .and_then(Value::as_str)
                .is_some_and(|tag| changed.contains_key(tag))
                || ["dialerProxy", "outboundTag"].iter().any(|key| {
                    object
                        .get(*key)
                        .and_then(Value::as_str)
                        .is_some_and(|tag| changed.contains_key(tag))
                })
                || object
                    .values()
                    .any(|child| has_indirect_reference(child, changed))
        }
        Value::Array(array) => array
            .iter()
            .any(|child| has_indirect_reference(child, changed)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::hot_swap;

    /// A config shaped like the compiler's, small enough to vary one thing at a time.
    fn config(
        inbound_port: u16,
        outbounds: &[&str],
        rules: &[(&str, &str)],
        balancer_selector: &[&str],
    ) -> String {
        serde_json::to_string(&json!({
            "log": { "loglevel": "warning" },
            "api": { "tag": "api", "services": ["HandlerService", "StatsService", "RoutingService"] },
            "dns": { "servers": ["1.1.1.1"] },
            "inbounds": [
                { "tag": "api", "port": 10085, "protocol": "dokodemo-door" },
                { "tag": "in:frt/i-a", "port": inbound_port, "protocol": "vless" }
            ],
            "outbounds": outbounds.iter().map(|tag| json!({
                "tag": tag, "protocol": "freedom", "settings": { "domainStrategy": "UseIP" }
            })).collect::<Vec<_>>(),
            "routing": {
                "domainStrategy": "AsIs",
                "balancers": [ { "tag": "hop-health", "selector": balancer_selector } ],
                "rules": rules.iter().map(|(tag, out)| json!({
                    "type": "field", "ruleTag": tag, "outboundTag": out
                })).collect::<Vec<_>>(),
            },
        }))
        .unwrap()
    }

    fn base() -> String {
        config(
            443,
            &["out:egress", "out:a"],
            &[("r:g1:000", "out:egress"), ("r:g1:001", "out:a")],
            &["out:a"],
        )
    }

    fn endpoint_configs() -> (Value, Value) {
        let mut before: Value = serde_json::from_str(&base()).unwrap();
        before["outbounds"][1] = json!({
            "tag":"out:a", "protocol":"vless", "settings":{"vnext":[{
                "address":"192.0.2.1", "port":443,
                "users":[{"id":"00000000-0000-4000-8000-000000000001", "encryption":"none"}]
            }]}
        });
        let mut after = before.clone();
        after["outbounds"][1]["settings"]["vnext"][0]["address"] = json!("2001:db8::1");
        (before, after)
    }

    #[test]
    fn endpoint_swap_keeps_every_routing_target_present_at_each_api_boundary() {
        use super::HotSwapOperation::*;
        let (before, after) = endpoint_configs();
        let swap = hot_swap(&before.to_string(), &after.to_string()).unwrap();
        assert!(swap.endpoint_swap.is_some());
        let mut outbounds = before["outbounds"].as_array().unwrap().clone();
        let mut rules = before["routing"]["rules"].as_array().unwrap().clone();
        for operation in swap.operations() {
            match operation {
                AddOutbounds(added) => {
                    for added in &added {
                        assert!(outbounds.iter().all(|old| old["tag"] != added["tag"]));
                    }
                    outbounds.extend(added);
                }
                RemoveOutbounds(tags) => {
                    outbounds.retain(|out| !tags.iter().any(|tag| out["tag"] == *tag))
                }
                ReloadRouting(replacement) => {
                    assert_eq!(replacement["balancers"], after["routing"]["balancers"]);
                    assert_eq!(
                        replacement["domainStrategy"],
                        after["routing"]["domainStrategy"]
                    );
                    rules = replacement["rules"].as_array().unwrap().clone();
                }
            }
            assert!(!rules.is_empty());
            for rule in &rules {
                assert!(outbounds
                    .iter()
                    .any(|out| out["tag"] == rule["outboundTag"]));
            }
            assert_eq!(
                outbounds[0], before["outbounds"][0],
                "default handler remains installed"
            );
        }
        assert_eq!(&outbounds, after["outbounds"].as_array().unwrap());
        assert_eq!(&rules, after["routing"]["rules"].as_array().unwrap());
        assert!(hot_swap(&after.to_string(), &before.to_string())
            .unwrap()
            .endpoint_swap
            .is_some());
        assert!(hot_swap(&after.to_string(), &after.to_string())
            .unwrap()
            .operations()
            .is_empty());
    }

    #[test]
    fn endpoint_swap_allows_a_changed_rule_table_with_reused_names() {
        let (before, mut after) = endpoint_configs();
        after["routing"]["rules"][1]["domain"] = json!(["full:example.test"]);
        let swap = hot_swap(&before.to_string(), &after.to_string()).unwrap();
        assert_eq!(swap.reload_routing.as_ref().unwrap(), &after["routing"]);
        assert_eq!(
            swap.endpoint_swap.unwrap().bridge_rules[1]["domain"],
            json!(["full:example.test"])
        );
    }

    #[test]
    fn endpoint_swap_rejects_duplicate_tags() {
        let (before, mut after) = endpoint_configs();
        let duplicate = after["outbounds"][1].clone();
        after["outbounds"].as_array_mut().unwrap().push(duplicate);
        assert!(hot_swap(&before.to_string(), &after.to_string()).is_none());
        let (before, mut after) = endpoint_configs();
        after["routing"]["rules"][1]["ruleTag"] = after["routing"]["rules"][0]["ruleTag"].clone();
        assert!(hot_swap(&before.to_string(), &after.to_string()).is_none());
        after["routing"]["rules"][1]["ruleTag"] = json!("");
        assert!(hot_swap(&before.to_string(), &after.to_string()).is_some());
    }

    #[test]
    fn removing_the_last_rule_leaves_the_default_outbound_installed() {
        let mut after: Value = serde_json::from_str(&base()).unwrap();
        after["routing"]["rules"] = json!([]);
        let swap = hot_swap(&base(), &after.to_string()).unwrap();
        assert!(
            matches!(swap.operations().as_slice(), [super::HotSwapOperation::ReloadRouting(routing)] if routing["rules"].as_array().unwrap().is_empty())
        );
    }

    #[test]
    fn endpoint_swap_rejects_unsafe_references_and_other_outbound_changes() {
        let (before, after) = endpoint_configs();
        for (path, value) in [
            (
                "/outbounds/1/settings/vnext/0/users/0/reverse",
                json!({"tag":"bridge"}),
            ),
            ("/outbounds/0/proxySettings", json!({"tag":"out:a"})),
            (
                "/outbounds/0/streamSettings",
                json!({"sockopt":{"dialerProxy":"out:a"}}),
            ),
            ("/routing/balancers/0/tag", json!("application-balancer")),
            ("/routing/balancers/0/fallbackTag", json!("out:a")),
            ("/routing/rules/1/balancerTag", json!("hop-health")),
            ("/routing/rules/1/ruleTag", json!("brocade:endpoint-swap:0")),
        ] {
            let mut old = before.clone();
            let mut new = after.clone();
            let (parent, key) = path.rsplit_once('/').unwrap();
            old.pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.to_owned(), value.clone());
            new.pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.to_owned(), value);
            assert!(
                hot_swap(&old.to_string(), &new.to_string()).is_none(),
                "{path}"
            );
        }
        let mut new = after.clone();
        new["outbounds"][1]["settings"]["vnext"][0]["users"][0]["encryption"] = json!("changed");
        assert!(hot_swap(&before.to_string(), &new.to_string()).is_none());
        let mut old = before.clone();
        let mut new = after.clone();
        old["outbounds"].as_array_mut().unwrap().swap(0, 1);
        new["outbounds"].as_array_mut().unwrap().swap(0, 1);
        assert!(hot_swap(&old.to_string(), &new.to_string()).is_none());
        for config in [&mut old, &mut new] {
            config["outbounds"].as_array_mut().unwrap().swap(0, 1);
            config["outbounds"].as_array_mut().unwrap().push(json!({"tag":format!("out:a{}", super::ENDPOINT_SWAP_SUFFIX),"protocol":"blackhole"}));
        }
        assert!(hot_swap(&old.to_string(), &new.to_string()).is_none());
    }

    #[test]
    fn certificate_group_changes_require_restart_even_with_identical_listeners() {
        let mut before: Value = serde_json::from_str(&base()).unwrap();
        before["brocadeCertificateGroup"] = serde_json::json!("group-a");
        let mut after = before.clone();
        assert!(hot_swap(&before.to_string(), &after.to_string()).is_some());
        after["brocadeCertificateGroup"] = serde_json::json!("group-b");
        assert!(hot_swap(&before.to_string(), &after.to_string()).is_none());
        after
            .as_object_mut()
            .unwrap()
            .remove("brocadeCertificateGroup");
        assert!(hot_swap(&before.to_string(), &after.to_string()).is_none());
        assert!(hot_swap(&after.to_string(), &before.to_string()).is_none());
    }

    #[test]
    fn an_identical_config_asks_for_nothing() {
        let swap = hot_swap(&base(), &base()).expect("同一份配置当然可以热切");
        assert!(swap.reload_routing.is_none());
        assert!(swap.add_outbounds.is_empty() && swap.remove_outbounds.is_empty());
    }

    #[test]
    fn a_rule_table_edit_swaps_the_whole_table() {
        let after = config(
            443,
            &["out:egress", "out:a"],
            &[("r:g2:000", "out:a"), ("r:g2:001", "out:egress")],
            &["out:a"],
        );
        let swap = hot_swap(&base(), &after).expect("只动规则表，应当能热切");
        assert_eq!(
            swap.reload_routing.as_ref().unwrap()["rules"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(matches!(
            swap.operations().as_slice(),
            [super::HotSwapOperation::ReloadRouting(_)]
        ));
        assert!(swap.add_outbounds.is_empty() && swap.remove_outbounds.is_empty());
    }

    #[test]
    fn rule_updates_preserve_denials_at_every_api_boundary() {
        let mut before: Value = serde_json::from_str(&base()).unwrap();
        before["routing"]["rules"] = json!([
            {"type":"field","ruleTag":"deny-old","inboundTag":["denied"],"outboundTag":"out:a"},
            {"type":"field","ruleTag":"fallback-old","outboundTag":"out:egress"}
        ]);
        let mut after = before.clone();
        after["routing"]["rules"][0]["ruleTag"] = "deny-new".into();
        after["routing"]["rules"][1]["ruleTag"] = "fallback-new".into();
        let swap = hot_swap(&before.to_string(), &after.to_string()).unwrap();
        let mut rules = before["routing"]["rules"].as_array().unwrap().clone();
        for operation in swap.operations() {
            match operation {
                super::HotSwapOperation::ReloadRouting(replacement) => {
                    rules = replacement["rules"].as_array().unwrap().clone()
                }
                _ => panic!("rule-only change unexpectedly changes handlers"),
            }
            let decision = rules
                .iter()
                .find(|rule| {
                    rule.get("inboundTag").is_none()
                        || rule["inboundTag"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|tag| tag == "denied")
                })
                .unwrap();
            assert_eq!(
                decision["outboundTag"], "out:a",
                "deny fell through to fallback"
            );
        }
        assert_eq!(&rules, after["routing"]["rules"].as_array().unwrap());
    }

    #[test]
    fn adding_and_dropping_whole_outbounds_is_swappable() {
        let after = config(
            443,
            &["out:egress", "out:b"],
            &[("r:g2:000", "out:egress"), ("r:g2:001", "out:b")],
            &["out:a"],
        );
        let swap = hot_swap(&base(), &after).expect("整条出站的增删没有空窗");
        assert_eq!(swap.remove_outbounds, vec!["out:a".to_owned()]);
        assert_eq!(swap.add_outbounds.len(), 1);
        assert_eq!(swap.add_outbounds[0]["tag"], "out:b");
    }

    #[test]
    fn adding_or_removing_a_wireguard_outbound_forces_a_restart() {
        let mut with_wireguard: Value = serde_json::from_str(&base()).unwrap();
        with_wireguard["outbounds"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "tag": "out:external/warp-8f3a-2d71",
                "protocol": "wireguard",
                "settings": {
                    "secretKey": "private-key",
                    "address": ["172.16.0.2/32"],
                    "peers": [{
                        "endpoint": "engage.example:2408",
                        "publicKey": "peer-key",
                        "allowedIPs": ["0.0.0.0/0"]
                    }]
                }
            }));
        with_wireguard["routing"]["rules"] = json!([
            { "type": "field", "ruleTag": "r:g2:000", "outboundTag": "out:egress" },
            {
                "type": "field",
                "ruleTag": "r:g2:001",
                "outboundTag": "out:external/warp-8f3a-2d71"
            }
        ]);
        let with_wireguard = serde_json::to_string(&with_wireguard).unwrap();

        assert!(
            hot_swap(&base(), &with_wireguard).is_none(),
            "新增 WireGuard 出站必须重启 xray"
        );
        assert!(
            hot_swap(&with_wireguard, &base()).is_none(),
            "移除 WireGuard 出站必须重启 xray"
        );
    }

    #[test]
    fn an_unchanged_wireguard_outbound_does_not_block_a_rule_only_swap() {
        let mut before: Value = serde_json::from_str(&base()).unwrap();
        before["outbounds"].as_array_mut().unwrap().push(json!({
            "tag": "out:external/warp-8f3a-2d71",
            "protocol": "wireguard",
            "settings": { "secretKey": "private-key" }
        }));
        let mut after = before.clone();
        after["routing"]["rules"] = json!([
            { "type": "field", "ruleTag": "r:g2:000", "outboundTag": "out:a" },
            { "type": "field", "ruleTag": "r:g2:001", "outboundTag": "out:egress" }
        ]);

        assert!(
            hot_swap(
                &serde_json::to_string(&before).unwrap(),
                &serde_json::to_string(&after).unwrap()
            )
            .is_some(),
            "WireGuard 出站没变时，纯规则调整仍应热切"
        );
    }

    /// An outbound can appear or vanish without any rule moving. The rule table is then
    /// left alone — reinstalling an identical table would collide with itself by name.
    #[test]
    fn an_outbound_only_change_leaves_the_rule_table_alone() {
        let after = config(
            443,
            &["out:egress", "out:a", "out:block"],
            &[("r:g1:000", "out:egress"), ("r:g1:001", "out:a")],
            &["out:a"],
        );
        let swap = hot_swap(&base(), &after).expect("规则没动，只多了一条出站");
        assert_eq!(swap.add_outbounds.len(), 1);
        assert_eq!(swap.add_outbounds[0]["tag"], "out:block");
        assert!(swap.reload_routing.is_none());
    }

    /// Unmatched traffic goes to whichever outbound is first, and that role is fixed when
    /// xray starts. Removing the outbound holding it would leave the default naming
    /// something that no longer exists — and unmatched traffic is not hypothetical here:
    /// the compiled tables select on inbound and user and carry no catch-all.
    #[test]
    fn losing_the_first_outbound_forces_a_restart() {
        let after = config(443, &["out:a"], &[("r:g2:000", "out:a")], &["out:a"]);
        assert!(hot_swap(&base(), &after).is_none());
    }

    /// Reordering alone. The map the comparison is built on is keyed by tag and cannot see
    /// it, so the head is checked by position.
    #[test]
    fn reordering_the_outbounds_forces_a_restart() {
        let after = config(
            443,
            &["out:a", "out:egress"],
            &[("r:g1:000", "out:egress"), ("r:g1:001", "out:a")],
            &["out:a"],
        );
        assert!(hot_swap(&base(), &after).is_none());
    }

    /// Same tag, different content. Installing it means removing it first, and in that
    /// window every rule naming it points at nothing.
    #[test]
    fn an_outbound_changed_in_place_forces_a_restart() {
        let mut after: Value = serde_json::from_str(&base()).unwrap();
        after["outbounds"][1]["settings"]["domainStrategy"] = json!("UseIPv4");
        assert!(hot_swap(&base(), &serde_json::to_string(&after).unwrap()).is_none());
    }

    #[test]
    fn an_inbound_replacement_requires_handoff() {
        let after = config(
            8443,
            &["out:egress", "out:a"],
            &[("r:g1:000", "out:egress"), ("r:g1:001", "out:a")],
            &["out:a"],
        );
        assert!(hot_swap(&base(), &after).is_none());
    }

    #[test]
    fn adding_or_removing_an_inbound_requires_handoff() {
        let mut after: Value = serde_json::from_str(&base()).unwrap();
        after["inbounds"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "tag": "in:frt/i-b", "port": 8443, "protocol": "vless" }));
        let after = serde_json::to_string(&after).unwrap();
        assert!(hot_swap(&base(), &after).is_none());
        assert!(hot_swap(&after, &base()).is_none());
    }

    /// The api inbound carries every call the swap is made of. Removing it would cut the
    /// line in the middle of the plan and leave the machine holding whichever half had
    /// been installed, with nothing left to finish or undo it with.
    #[test]
    fn touching_the_api_inbound_forces_a_restart() {
        let mut after: Value = serde_json::from_str(&base()).unwrap();
        after["inbounds"][0]["port"] = json!(10086);
        assert!(hot_swap(&base(), &serde_json::to_string(&after).unwrap()).is_none());

        let mut without: Value = serde_json::from_str(&base()).unwrap();
        without["inbounds"].as_array_mut().unwrap().remove(0);
        assert!(hot_swap(&base(), &serde_json::to_string(&without).unwrap()).is_none());
    }

    /// A balancer cannot be modified, removed, or reclaimed — only new tags can appear.
    /// A selector change therefore has no runtime expression at all.
    #[test]
    fn a_balancer_change_forces_a_restart() {
        let after = config(
            443,
            &["out:egress", "out:a"],
            &[("r:g1:000", "out:egress"), ("r:g1:001", "out:a")],
            &["out:a", "out:egress"],
        );
        assert!(hot_swap(&base(), &after).is_none());
    }

    #[test]
    fn a_block_with_no_runtime_verb_forces_a_restart() {
        for (path, value) in [
            ("dns", json!({ "servers": ["8.8.8.8"] })),
            ("log", json!({ "loglevel": "debug" })),
            ("api", json!({ "tag": "api", "services": ["StatsService"] })),
        ] {
            let mut after: Value = serde_json::from_str(&base()).unwrap();
            after[path] = value;
            assert!(
                hot_swap(&base(), &serde_json::to_string(&after).unwrap()).is_none(),
                "{path} 变了却被判成可热切",
            );
        }
    }

    /// A block present on one side only. Comparing the keys of just one document would
    /// walk straight past it — `policy` appearing for the first time would be read as
    /// "nothing outside routing changed" and never reach the running process.
    #[test]
    fn a_block_appearing_or_vanishing_forces_a_restart() {
        let mut after: Value = serde_json::from_str(&base()).unwrap();
        after["policy"] = json!({ "levels": { "0": { "statsUserOnline": true } } });
        let after = serde_json::to_string(&after).unwrap();
        assert!(hot_swap(&base(), &after).is_none(), "新增的块被漏掉了");
        assert!(hot_swap(&after, &base()).is_none(), "消失的块被漏掉了");
    }

    #[test]
    fn unnamed_rules_are_replaced_with_the_complete_table() {
        let mut before: Value = serde_json::from_str(&base()).unwrap();
        before["routing"]["rules"][1]
            .as_object_mut()
            .unwrap()
            .remove("ruleTag");
        let before = serde_json::to_string(&before).unwrap();
        assert!(hot_swap(&before, &base()).is_some());

        let mut after: Value = serde_json::from_str(&base()).unwrap();
        after["routing"]["rules"][0]
            .as_object_mut()
            .unwrap()
            .remove("ruleTag");
        let after = serde_json::to_string(&after).unwrap();
        let swap = hot_swap(&base(), &after).unwrap();
        let expected: Value = serde_json::from_str(&after).unwrap();
        assert_eq!(swap.reload_routing.as_ref().unwrap(), &expected["routing"]);
    }

    #[test]
    fn rule_names_can_be_reused_across_atomic_replacements() {
        let after = config(
            443,
            &["out:egress", "out:a"],
            // Same names, different targets.
            &[("r:g1:000", "out:a"), ("r:g1:001", "out:egress")],
            &["out:a"],
        );
        assert!(hot_swap(&base(), &after).is_some());
    }

    #[test]
    fn unparseable_input_forces_a_restart() {
        assert!(hot_swap("not json", &base()).is_none());
        assert!(hot_swap(&base(), "{").is_none());
    }
}
