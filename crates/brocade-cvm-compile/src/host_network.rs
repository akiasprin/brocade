//! Frozen host products. Runtime readiness and delivery addresses are explicit late bindings.
use crate::{canonical, entry_network, Result};
use brocade_core::hash::sha256_hex;
use brocade_deployment::cvm::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProduct {
    pub kind: String,
    /// Digest of the credential-bearing executable source, never the display projection.
    pub sha256: String,
    pub content: String,
    pub redacted: bool,
    pub apply: String,
    pub bindings: Vec<String>,
}
fn redact(value: &mut Value) -> bool {
    let mut changed = false;
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "credential" | "peer_credential" | "private_key" | "password" | "secret"
                ) && !value.is_null()
                {
                    *value = json!("[redacted]");
                    changed = true;
                } else {
                    changed |= redact(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                changed |= redact(item)
            }
        }
        _ => {}
    }
    changed
}
fn product(kind: &str, value: &impl Serialize, bindings: &[&str]) -> Result<HostProduct> {
    let source = canonical(value)?;
    let mut safe = serde_json::to_value(value).map_err(|_| "host source encoding failed")?;
    let redacted = redact(&mut safe);
    Ok(HostProduct {
        kind: kind.into(),
        sha256: sha256_hex(source.as_bytes()),
        content: canonical(&safe)?,
        redacted,
        apply: "hot".into(),
        bindings: bindings.iter().map(|s| s.to_string()).collect(),
    })
}
pub fn chain_guard(a: &ChainClientArtifact, replace: bool) -> String {
    let name = format!("brocade_chain_{}", &a.binding.device[3..]);
    let comment = format!("brocade-chain:{}:{}", a.instance_id, a.chain_id);
    format!("{}table inet {name} {{\n comment \"{comment}\"\n chain guard {{\n  type filter hook forward priority -180; policy accept;\n  meta mark {} oifname != \"{}\" drop\n }}\n}}\n",if replace {format!("delete table inet {name}\n")}else{String::new()},a.binding.mark,a.binding.device)
}
pub fn compile(
    nic: &NicDesired,
    entry: &EntryDesired,
    clients: &[ChainClientArtifact],
) -> Result<Vec<HostProduct>> {
    let mut files = Vec::new();
    files.push(product(
        "host/nic-carriers.json",
        nic,
        &["host management addresses", "carrier device readiness"],
    )?);
    files.push(product(
        "host/entry-tunnels.json",
        entry,
        &["instance primary delivery address", "tunnel readiness"],
    )?);
    let ready = entry
        .tunnels
        .iter()
        .map(|t| t.id.as_str())
        .collect::<BTreeSet<_>>();
    let ports = BTreeSet::new();
    for (kind, ready) in [
        ("host/entries.nft", ready),
        ("host/entries-fenced.nft", BTreeSet::new()),
    ] {
        let content = entry_network::ruleset(entry, &ready, &ports)?;
        files.push(HostProduct {
            kind: kind.into(),
            sha256: sha256_hex(content.as_bytes()),
            content,
            redacted: false,
            apply: "hot".into(),
            bindings: vec![
                "instance primary delivery address".into(),
                "actual shared transport listeners".into(),
                "only verified tunnels enter the ready set".into(),
            ],
        });
    }
    for c in clients {
        let base = format!("host/chains/{}/{}", c.instance_id, c.chain_id);
        files.push(HostProduct {
            kind: format!("{base}/xray.json"),
            sha256: c.sha256.clone(),
            content: c.redacted.clone(),
            redacted: true,
            apply: "hot".into(),
            bindings: vec!["owned TUN descriptor".into()],
        });
        let content = c.guard.clone();
        files.push(HostProduct {
            kind: format!("{base}/guard.nft"),
            sha256: sha256_hex(content.as_bytes()),
            content,
            redacted: false,
            apply: "hot".into(),
            bindings: Vec::new(),
        });
        files.push(product(&format!("{base}/routing.json"),&json!({"device":c.binding.device,"link_ipv4":format!("{CHAIN_TUN_IPV4}/32"),"link_scope":"link","mtu":CHAIN_TUN_MTU,"noprefixroute":true,"mark":c.binding.mark,"table":c.binding.mark,"families":[4,6],"rule_priority":28800,"route_protocol":186,"unreachable_default_metric":42760,"tun_default_metric":10,"blocked":c.blocked}),&[])?);
    }
    files.sort_by(|a, b| a.kind.cmp(&b.kind));
    Ok(files)
}
