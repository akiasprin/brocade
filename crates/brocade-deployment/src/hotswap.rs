//! Deciding whether a config change can be installed into a running xray.
//!
//! Shared on purpose. The agent uses it to choose between talking to the process and
//! restarting it; the control plane uses the same answer to decide whether a deployment is
//! destructive and therefore has to wave and be confirmed. Two implementations would drift,
//! and the drift is not symmetric: a control plane promising "nothing will drop" while the
//! agent restarts is a promise broken on live traffic.
//!
//! Every rule here was measured against xray 26.4.25 rather than read off documentation —
//! several of them contradict what the command help implies.

use std::collections::BTreeMap;

use serde_json::Value;

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
    /// Inbounds to take out: the ones that vanished, plus the ones whose definition
    /// changed — xray refuses a second inbound under a tag that already exists, so a change
    /// in place can only be expressed as a removal followed by an addition.
    pub remove_inbounds: Vec<String>,
    /// Inbounds to put in: the ones that appeared, plus the replacements for the above.
    ///
    /// Whoever installs these owes the machine one more thing. An inbound comes back with an
    /// empty account list — the compiled config carries none, they are pushed in at runtime —
    /// and a configuration deployment does not sync accounts afterwards
    /// (`DesiredGrants::Unmanaged` is a no-op). Left there, the inbound listens and turns
    /// every subscription away as an unknown user.
    pub add_inbounds: Vec<Value>,
    /// The complete new table, in order.
    pub add_rules: Vec<Value>,
    /// The complete old table, by name.
    pub remove_rule_tags: Vec<String>,
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
/// - **Any balancer difference.** A balancer cannot be modified, cannot be removed, and is
///   never reclaimed once created; only entirely new tags can be added. Selector changes
///   therefore have no runtime expression at all.
/// - **An outbound that changed while keeping its tag.** Expressing that means taking it
///   out and putting it back, and in between the rules pointing at it name nothing.
///   Additions and removals have no such window, which is why those two are allowed.
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
            "inbounds" | "outbounds" => None,
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
            value
                .get(key)?
                .as_array()?
                .iter()
                .map(|entry| Some((entry.get("tag")?.as_str()?.to_owned(), entry.clone())))
                .collect()
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

    for (tag, outbound) in &after_outbounds {
        if before_outbounds
            .get(tag)
            .is_some_and(|previous| previous != outbound)
        {
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

    let (before_inbounds, after_inbounds) =
        (by_tag(before, "inbounds")?, by_tag(after, "inbounds")?);

    // The api inbound is the channel every call in this plan travels over. Taking it out
    // would sever the connection mid-plan and leave the machine holding whichever half had
    // been installed, with no way left to finish or undo it. The `api` block itself is
    // already required to be identical, so its tag is the same on both sides.
    let api_tag = before
        .get("api")
        .and_then(|api| api.get("tag"))
        .and_then(Value::as_str);
    if let Some(api_tag) = api_tag {
        if before_inbounds.get(api_tag) != after_inbounds.get(api_tag) {
            return None;
        }
    }

    let remove_inbounds = before_inbounds
        .iter()
        .filter(|(tag, inbound)| {
            after_inbounds
                .get(*tag)
                .is_none_or(|desired| desired != *inbound)
        })
        .map(|(tag, _)| tag.clone())
        .collect::<Vec<_>>();
    let add_inbounds = after_inbounds
        .iter()
        .filter(|(tag, inbound)| {
            before_inbounds
                .get(*tag)
                .is_none_or(|previous| previous != *inbound)
        })
        .map(|(_, inbound)| inbound.clone())
        .collect::<Vec<_>>();

    let rules = |value: &serde_json::Map<String, Value>| -> Option<Vec<Value>> {
        Some(value.get("routing")?.get("rules")?.as_array()?.clone())
    };
    let (before_rules, after_rules) = (rules(before)?, rules(after)?);
    let tags = |rules: &[Value]| -> Option<Vec<String>> {
        rules
            .iter()
            .map(|rule| Some(rule.get("ruleTag")?.as_str()?.to_owned()))
            .collect()
    };
    // A table with an unnamed rule in it cannot be swapped: `rmrules` works by name, so the
    // unnamed one would survive the removal and sit in front of the new table forever.
    let remove_rule_tags = tags(&before_rules)?;
    let new_tags = tags(&after_rules)?;

    // A table that did not change is not reinstalled. Beyond saving two calls this is what
    // keeps the check below meaningful: an unchanged table necessarily reuses every name,
    // which is indistinguishable from a genuine collision by name alone.
    let (add_rules, remove_rule_tags) = if before_rules == after_rules {
        (Vec::new(), Vec::new())
    } else {
        // Both tables are installed at once during the swap, and xray rejects a duplicate
        // name for the whole batch — atomically, so the machine would stay on its old
        // routing while the call reported failure. The compiler makes the generations
        // disjoint on purpose (`brocade_core::artifacts::xray`'s rule naming); this is the
        // agent declining to find out the hard way if that ever stops being true.
        if new_tags.iter().any(|tag| remove_rule_tags.contains(tag)) {
            return None;
        }
        (after_rules, remove_rule_tags)
    };

    Some(HotSwap {
        remove_inbounds,
        add_inbounds,
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
        add_rules,
        remove_rule_tags,
    })
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

    #[test]
    fn an_identical_config_asks_for_nothing() {
        let swap = hot_swap(&base(), &base()).expect("同一份配置当然可以热切");
        assert!(swap.add_rules.is_empty() && swap.remove_rule_tags.is_empty());
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
        assert_eq!(swap.remove_rule_tags, vec!["r:g1:000", "r:g1:001"]);
        assert_eq!(swap.add_rules.len(), 2);
        // The old table goes out by name and the new one goes in whole — a rule that
        // happens to be unchanged is still reinstalled, because its position in the new
        // table is what decides routing and position cannot be patched.
        assert!(swap.add_outbounds.is_empty() && swap.remove_outbounds.is_empty());
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
                "tag": "out:external/warp",
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
                "outboundTag": "out:external/warp"
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
            "tag": "out:external/warp",
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
        assert!(swap.add_rules.is_empty() && swap.remove_rule_tags.is_empty());
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

    /// Replacing an inbound is expressible: xray refuses a second inbound under a tag that
    /// already exists, so the change becomes a removal and an addition of the same tag.
    ///
    /// Measured on 26.4.25, this costs far less than it reads: removing an inbound closes
    /// its listener but does not touch the connections already on it (a transfer in flight
    /// ran on to completion 15 seconds after its inbound was gone), and the traffic
    /// counters survive under the same names. What it does cost is the accounts, which is
    /// why `add_inbounds` obliges whoever installs it to push them back.
    #[test]
    fn an_inbound_change_is_swappable_as_a_removal_and_an_addition() {
        let after = config(
            8443,
            &["out:egress", "out:a"],
            &[("r:g1:000", "out:egress"), ("r:g1:001", "out:a")],
            &["out:a"],
        );
        let swap = hot_swap(&base(), &after).expect("换入站是可表达的");
        assert_eq!(swap.remove_inbounds, vec!["in:frt/i-a".to_owned()]);
        assert_eq!(swap.add_inbounds.len(), 1);
        assert_eq!(swap.add_inbounds[0]["port"], 8443);
        // The rest of the config did not move, so nothing else is asked for.
        assert!(swap.add_rules.is_empty() && swap.add_outbounds.is_empty());
    }

    /// An inbound that only appears has nothing to remove, and therefore not even the
    /// moment of not listening that a replacement has.
    #[test]
    fn a_new_inbound_is_added_without_removing_anything() {
        let mut after: Value = serde_json::from_str(&base()).unwrap();
        after["inbounds"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "tag": "in:frt/i-b", "port": 8443, "protocol": "vless" }));
        let swap =
            hot_swap(&base(), &serde_json::to_string(&after).unwrap()).expect("新增入站没有空窗");
        assert!(swap.remove_inbounds.is_empty());
        assert_eq!(swap.add_inbounds.len(), 1);
        assert_eq!(swap.add_inbounds[0]["tag"], "in:frt/i-b");
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

    /// `rmrules` works by name. One unnamed rule would survive the removal and sit ahead
    /// of the whole new table, deciding routing forever.
    #[test]
    fn an_unnamed_rule_anywhere_forces_a_restart() {
        let mut before: Value = serde_json::from_str(&base()).unwrap();
        before["routing"]["rules"][1]
            .as_object_mut()
            .unwrap()
            .remove("ruleTag");
        let before = serde_json::to_string(&before).unwrap();
        assert!(hot_swap(&before, &base()).is_none(), "旧表里有无名规则");

        let mut after: Value = serde_json::from_str(&base()).unwrap();
        after["routing"]["rules"][0]
            .as_object_mut()
            .unwrap()
            .remove("ruleTag");
        let after = serde_json::to_string(&after).unwrap();
        assert!(hot_swap(&base(), &after).is_none(), "新表里有无名规则");
    }

    /// Both tables are installed at once during the swap and xray rejects a duplicate
    /// name for the entire batch, leaving the machine on its old routing. The compiler
    /// keeps the generations disjoint; this is the agent not betting on it.
    #[test]
    fn a_name_shared_between_the_two_tables_forces_a_restart() {
        let after = config(
            443,
            &["out:egress", "out:a"],
            // Same names, different targets.
            &[("r:g1:000", "out:a"), ("r:g1:001", "out:egress")],
            &["out:a"],
        );
        assert!(hot_swap(&base(), &after).is_none());
    }

    #[test]
    fn unparseable_input_forces_a_restart() {
        assert!(hot_swap("not json", &base()).is_none());
        assert!(hot_swap(&base(), "{").is_none());
    }
}
