//! Scoping and redaction of snapshots. Private keys never leave this layer — the console
//! receives masked values.
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;

use brocade_core::model::ModelSnapshot;

use super::*;
use crate::{AdminContext, Result, StoreError};

pub(crate) async fn load_scoped_snapshot(
    pool: &PgPool,
    actor: &AdminContext,
    revision: Option<u64>,
) -> Result<ModelSnapshot> {
    let snapshot = crate::materialize::load_snapshot(pool, revision).await?;
    Ok(scope_snapshot(actor, snapshot))
}

pub(crate) fn scope_snapshot(actor: &AdminContext, mut snapshot: ModelSnapshot) -> ModelSnapshot {
    if actor.is_system_admin() {
        return snapshot;
    }
    snapshot
        .nodes
        .retain(|node| actor.can_access_tenant(&node.tenant));
    snapshot
        .users
        .retain(|user| actor.can_access_tenant(&user.tenant));
    snapshot
        .external_outbounds
        .retain(|outbound| actor.can_access_tenant(&outbound.tenant));
    let visible_external_ids = snapshot
        .external_outbounds
        .iter()
        .map(|outbound| outbound.id.clone())
        .collect::<BTreeSet<_>>();

    let visible_node_ids = snapshot
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    snapshot
        .node_egress_dns
        .retain(|policy| visible_node_ids.contains(&policy.node));
    let visible_user_keys = snapshot
        .users
        .iter()
        .map(|user| format!("{}:{}", user.tenant, user.id))
        .collect::<BTreeSet<_>>();

    for app in &mut snapshot.apps {
        app.chains
            .retain(|chain| actor.can_access_tenant(&chain.tenant));
        app.fronts
            .retain(|front| actor.can_access_tenant(&front.tenant));
        let visible_chain_ids = app
            .chains
            .iter()
            .map(|chain| chain.id.clone())
            .collect::<BTreeSet<_>>();
        app.ingresses
            .retain(|ingress| visible_chain_ids.contains(&ingress.chain));
        let visible_ingress_ids = app
            .ingresses
            .iter()
            .map(|ingress| ingress.id.clone())
            .collect::<BTreeSet<_>>();
        for front in &mut app.fronts {
            front
                .via
                .retain(|ingress_id| visible_ingress_ids.contains(ingress_id));
            front
                .external_via
                .retain(|outbound_id| visible_external_ids.contains(outbound_id));
        }
        app.steps.retain(|step| {
            visible_chain_ids.contains(&step.chain) && visible_node_ids.contains(&step.node)
        });
        app.grants.retain(|grant| {
            visible_ingress_ids.contains(&grant.ingress)
                && visible_user_keys.contains(&format!("{}:{}", grant.tenant, grant.user))
        });
    }
    snapshot.apps.retain(|app| {
        !app.chains.is_empty()
            || !app.fronts.is_empty()
            || !app.ingresses.is_empty()
            || !app.steps.is_empty()
            || !app.grants.is_empty()
    });

    snapshot
}

pub(crate) fn redact_private_keys(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("private_key");
            object.remove("privateKey");
            // Symmetric keys are secret in whole — there is no public half to keep, so the
            // field goes rather than being trimmed. Matched by the `psk` suffix rather than by
            // name: a relay port carries two of them and a third would be one more thing to
            // remember to register, which is how a deny list eventually leaks. `psk` is a
            // narrow enough word to claim as a naming convention; `key` would not be.
            object.retain(|name, _| !name.ends_with("psk"));
            // Hysteria's Salamander password is a symmetric credential. Keep the field so a
            // full-object edit can round-trip the enabled state, but never send its value.
            if let Some(password) = object.get_mut("password") {
                *password = Value::String("<redacted>".to_owned());
            }
            if let Some(credential) = object.get_mut("credential") {
                *credential = Value::String("<redacted>".to_owned());
            }
            for value in object.values_mut() {
                redact_private_keys(value);
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_private_keys(item);
            }
        }
        _ => {}
    }
}

pub(crate) fn redacted_value<T: Serialize>(value: T) -> Result<Value> {
    let mut value = serde_json::to_value(value)?;
    redact_private_keys(&mut value);
    Ok(value)
}

pub(crate) fn artifact_content_from_text(
    revision: u64,
    target_kind: &str,
    target_id: &str,
    artifact_kind: &str,
    content: String,
    redact: bool,
) -> Result<ArtifactContent> {
    let sha256 = sha256_hex(content.as_bytes());
    let byte_len = u64::try_from(content.len())
        .map_err(|_| StoreError::InvalidData(format!("artifact too large: {sha256}")))?;
    let (content, redacted) = if redact {
        redact_artifact_text(content)?
    } else {
        (content, false)
    };
    Ok(ArtifactContent {
        revision,
        target_kind: target_kind.to_owned(),
        target_id: target_id.to_owned(),
        artifact_kind: artifact_kind.to_owned(),
        state: "present".to_owned(),
        sha256: Some(sha256),
        byte_len: Some(byte_len),
        content: Some(content),
        redacted,
    })
}

pub(crate) fn disabled_artifact_content(
    revision: u64,
    target_kind: &str,
    target_id: &str,
    artifact_kind: &str,
) -> ArtifactContent {
    ArtifactContent {
        revision,
        target_kind: target_kind.to_owned(),
        target_id: target_id.to_owned(),
        artifact_kind: artifact_kind.to_owned(),
        state: "disabled".to_owned(),
        sha256: None,
        byte_len: None,
        content: None,
        redacted: false,
    }
}

pub(crate) fn redact_artifact_text(content: String) -> Result<(String, bool)> {
    if let Ok(mut value) = serde_json::from_str::<Value>(&content) {
        let redacted = mask_private_key_values(&mut value);
        return Ok((serde_json::to_string_pretty(&value)?, redacted));
    }

    let mut redacted = false;
    let lines = content
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("PrivateKey") {
                if let Some((left, _)) = line.split_once('=') {
                    redacted = true;
                    return format!("{left}= <redacted>");
                }
            }
            line.to_owned()
        })
        .collect::<Vec<_>>();
    Ok((lines.join("\n"), redacted))
}

pub(crate) fn mask_private_key_values(value: &mut Value) -> bool {
    match value {
        Value::Object(object) => {
            let mut redacted = false;
            // A VLESS external outbound uses `id` for its credential, but `id` elsewhere is
            // usually an ordinary resource identifier and cannot be masked generically. Its tag
            // supplies the missing context. Shadowsocks and authenticated proxy outbounds use
            // password/pass; WireGuard uses secretKey. They are handled by the common branch.
            let external_vless = object
                .get("tag")
                .and_then(Value::as_str)
                .is_some_and(|tag| tag.contains("/external/"))
                && object.get("protocol").and_then(Value::as_str) == Some("vless");
            if external_vless {
                if let Some(id) = object
                    .get_mut("settings")
                    .and_then(Value::as_object_mut)
                    .and_then(|settings| settings.get_mut("id"))
                {
                    *id = json!("<redacted>");
                    redacted = true;
                }
            }
            for (key, value) in object {
                if key == "private_key"
                    || key == "privateKey"
                    || key == "secretKey"
                    || key == "password"
                    || key == "pass"
                {
                    *value = json!("<redacted>");
                    redacted = true;
                } else {
                    redacted |= mask_private_key_values(value);
                }
            }
            redacted
        }
        Value::Array(items) => {
            let mut redacted = false;
            for value in items {
                redacted |= mask_private_key_values(value);
            }
            redacted
        }
        _ => false,
    }
}

pub(crate) fn split_user_target(target_id: &str) -> Result<(&str, &str)> {
    target_id
        .split_once(':')
        .filter(|(tenant, user)| !tenant.trim().is_empty() && !user.trim().is_empty())
        .ok_or_else(|| {
            StoreError::InvalidData("user artifact target_id must use tenant_id:user_id".to_owned())
        })
}

pub(crate) fn ensure_node_visible(snapshot: &ModelSnapshot, node_id: &str) -> Result<()> {
    snapshot
        .nodes
        .iter()
        .any(|node| node.id == node_id)
        .then_some(())
        .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))
}

pub(crate) fn ensure_user_visible(
    snapshot: &ModelSnapshot,
    tenant_id: &str,
    user_id: &str,
) -> Result<()> {
    snapshot
        .users
        .iter()
        .any(|user| user.tenant == tenant_id && user.id == user_id)
        .then_some(())
        .ok_or_else(|| StoreError::NotFound(format!("user {tenant_id}/{user_id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A shadowsocks key is the entire credential, so a viewer who may not see private keys may
    /// not see it either. The deny list works by field name, which is why this is worth a test:
    /// the day somebody adds a secret under a name nobody registered, everything still compiles
    /// and the value simply appears in the console.
    #[test]
    fn redaction_strips_a_symmetric_hop_key() {
        let mut value = json!({
            "steps": [{
                "hop_in": { "port": 20000, "wire": { "t": "shadowsocks2022", "v": { "psk": "SECRET" } } }
            }],
        });

        redact_private_keys(&mut value);

        let rendered = value.to_string();
        assert!(!rendered.contains("SECRET"), "{rendered}");
        // The rest of the shape survives — redaction removes the secret, it does not blank the
        // row, and a caller still has to be able to tell which wire format is in use.
        assert_eq!(value["steps"][0]["hop_in"]["port"], 20000);
        assert_eq!(value["steps"][0]["hop_in"]["wire"]["t"], "shadowsocks2022");
    }

    #[test]
    fn redaction_masks_hysteria2_salamander_password_but_keeps_it_enabled() {
        let mut value = json!({
            "transport": {
                "kind": "hysteria2",
                "obfs": { "kind": "salamander", "password": "HY2-SECRET" }
            }
        });

        redact_private_keys(&mut value);

        assert_eq!(value["transport"]["obfs"]["password"], "<redacted>");
        assert_eq!(value["transport"]["obfs"]["kind"], "salamander");
        assert!(!value.to_string().contains("HY2-SECRET"));
    }

    #[test]
    fn redaction_masks_external_proxy_credentials_in_snapshots_and_artifacts() {
        let mut snapshot = json!({
            "external_outbounds": [{
                "protocol": { "t": "vless", "v": { "credential": "VLESS-SECRET" } }
            }]
        });
        redact_private_keys(&mut snapshot);
        assert_eq!(
            snapshot["external_outbounds"][0]["protocol"]["v"]["credential"],
            "<redacted>"
        );

        let mut artifact = json!({
            "tag": "out:app/external/vendor-edge",
            "protocol": "vless",
            "settings": { "id": "VLESS-SECRET" }
        });
        assert!(mask_private_key_values(&mut artifact));
        assert_eq!(artifact["settings"]["id"], "<redacted>");
        assert!(!artifact.to_string().contains("VLESS-SECRET"));

        let mut wireguard = json!({
            "tag": "out:app/external/wg",
            "protocol": "wireguard",
            "settings": { "secretKey": "WG-PRIVATE", "peers": [{ "publicKey": "PUBLIC" }] }
        });
        assert!(mask_private_key_values(&mut wireguard));
        assert_eq!(wireguard["settings"]["secretKey"], "<redacted>");
        assert_eq!(wireguard["settings"]["peers"][0]["publicKey"], "PUBLIC");
    }

    /// Every shape an ingress can take, run through the redactor and searched for the secret.
    ///
    /// Written against the whole enum rather than one variant because the deny list works by
    /// field name, and a shape added later carries its secret under whatever name its author
    /// picked. That is not hypothetical: the TLS shapes called this field `secret` at first, and
    /// the model snapshot served every TLS ingress's private key to anybody who could read it.
    #[test]
    fn no_transport_shape_carries_its_private_key_past_the_redactor() {
        use brocade_core::model::{
            Hysteria2, HysteriaObfs, RealitySettings, RealityXhttp, Tls, TlsXhttp, Transport,
            Xhttp, XhttpMode,
        };

        const SECRET: &str = "PRIVATE-KEY-THAT-MUST-NOT-ESCAPE";
        let reality = || RealitySettings {
            dest: "www.example.com:443".to_owned(),
            server_names: vec!["www.example.com".to_owned()],
            fingerprint: "chrome".to_owned(),
            flow: None,
            fallback_mode: Default::default(),
            fallback_guard: true,
            fallback_limits: Default::default(),
        };
        let tls = || Tls { flow: None };
        let xhttp = || Xhttp {
            path: "/probe".to_owned(),
            host: None,
            xmux: None,
            tuning: None,
            mode: XhttpMode::Auto,
        };

        for transport in [
            IngressWires::Vless(Transport::VlessReality(reality())),
            IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
                reality: reality(),
                xhttp: xhttp(),
            })),
            IngressWires::Vless(Transport::VlessTls(tls())),
            IngressWires::Vless(Transport::VlessTlsXhttp(TlsXhttp {
                tls: tls(),
                xhttp: xhttp(),
            })),
            IngressWires::Hysteria2(Hysteria2 {
                port: 50000,
                hop: None,
                obfs: Some(HysteriaObfs::Salamander {
                    password: "another-secret".to_owned(),
                }),
                ..Default::default()
            }),
        ] {
            let kind = transport.vless_kind().unwrap_or("hysteria2");
            let mut value = serde_json::json!({
                "identity": {
                    "private_key": SECRET,
                    "public_key": "pub",
                    "short_ids": ["ab"],
                },
                "wires": transport,
            });
            assert!(
                value.to_string().contains(SECRET),
                "{kind}：测试本身没把密钥放进去，这条断言就不说明任何事"
            );
            redact_private_keys(&mut value);
            assert!(
                !value.to_string().contains(SECRET),
                "{kind} 把私钥带出去了：{value}"
            );
        }
    }
}
