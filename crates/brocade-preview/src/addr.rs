use std::{
    collections::BTreeSet,
    net::{Ipv4Addr, Ipv6Addr},
    ops::Range,
};

use axum::http::HeaderMap;
use serde_json::Value;

use crate::{
    config::Config,
    console::get_console_json,
    docker::{
        docker_output, label_template, LABEL_IPV4, LABEL_IPV6, LABEL_IP_LEGACY, LABEL_PREVIEW,
    },
    error::PreviewError,
    AppState,
};

// The address layout within a preview network, with IPv4 and IPv6 sharing one slot numbering:
// 10 is the probe client, 11..199 go to nodes, and 200 is the probe echo end.
pub(crate) const CLIENT_SLOT: u16 = 10;
pub(crate) const ECHO_SLOT: u16 = 200;
const NODE_SLOTS: Range<u16> = 11..ECHO_SLOT;

// The three ranges must not overlap, or an allocated node address collides with one of the probe
// ends.
const _: () = assert!(CLIENT_SLOT < NODE_SLOTS.start && ECHO_SLOT >= NODE_SLOTS.end);

#[derive(Debug, Clone, Copy)]
pub(crate) struct PreviewAddress {
    pub(crate) ipv4: Ipv4Addr,
    pub(crate) ipv6: Ipv6Addr,
}

pub(crate) fn preview_ipv4(config: &Config, slot: u16) -> Result<Ipv4Addr, PreviewError> {
    format!("{}.{}", config.ip_prefix, slot)
        .parse::<Ipv4Addr>()
        .map_err(|error| PreviewError::internal(format!("invalid preview ip prefix: {error}")))
}

pub(crate) fn preview_ipv6(config: &Config, slot: u16) -> Result<Ipv6Addr, PreviewError> {
    format!("{}{slot}", config.ip6_prefix)
        .parse::<Ipv6Addr>()
        .map_err(|error| PreviewError::internal(format!("invalid preview ip6 prefix: {error}")))
}

/// The echo address the probe script targets.
///
/// It still assembles one from an invalid prefix, so that the failure lands at the probing step
/// where the error reads better.
pub(crate) fn echo_ipv4(config: &Config) -> String {
    preview_ipv4(config, ECHO_SLOT)
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| format!("{}.{}", config.ip_prefix, ECHO_SLOT))
}

/// Confirm both prefixes can assemble the layout's two end addresses before creating the network.
///
/// The prefixes are assembled as strings, and a mistyped one errors only at docker run, by which
/// point the network has been created.
pub(crate) fn validate_slot_prefixes(config: &Config) -> Result<(), PreviewError> {
    preview_ipv6(config, NODE_SLOTS.start)?;
    preview_ipv6(config, ECHO_SLOT)?;
    preview_ipv4(config, NODE_SLOTS.start)?;
    preview_ipv4(config, ECHO_SLOT)?;
    Ok(())
}

/// Pick a node slot free in both v4 and v6.
///
/// Addresses in use come from two sources: the nodes the control plane records, and the local
/// containers carrying a preview label.
pub(crate) async fn allocate_address(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<PreviewAddress, PreviewError> {
    let mut used_ipv4 = BTreeSet::<Ipv4Addr>::new();
    let mut used_ipv6 = BTreeSet::<Ipv6Addr>::new();
    if let Ok(nodes) = get_console_json(state, "/nodes/agent-state", headers).await {
        if let Some(nodes) = nodes.get("nodes").and_then(Value::as_array) {
            for node in nodes {
                if let Some(public_ipv4) = node.get("public_ipv4").and_then(Value::as_str) {
                    if let Ok(ip) = public_ipv4.parse::<Ipv4Addr>() {
                        used_ipv4.insert(ip);
                    }
                }
                if let Some(public_ipv6) = node.get("public_ipv6").and_then(Value::as_str) {
                    if let Ok(ip) = public_ipv6.parse::<Ipv6Addr>() {
                        used_ipv6.insert(ip);
                    }
                }
            }
        }
    }
    let filter = format!("label={LABEL_PREVIEW}=1");
    let template = [LABEL_IP_LEGACY, LABEL_IPV4, LABEL_IPV6]
        .map(label_template)
        .join(" ");
    if let Ok(output) = docker_output(
        &state.config,
        &["ps", "-a", "--filter", &filter, "--format", &template],
    )
    .await
    {
        for line in output.lines() {
            for value in line.split_whitespace() {
                if let Ok(ip) = value.parse::<Ipv4Addr>() {
                    used_ipv4.insert(ip);
                }
                if let Ok(ip) = value.parse::<Ipv6Addr>() {
                    used_ipv6.insert(ip);
                }
            }
        }
    }
    for slot in NODE_SLOTS {
        let ipv4 = preview_ipv4(&state.config, slot)?;
        let ipv6 = preview_ipv6(&state.config, slot)?;
        if !used_ipv4.contains(&ipv4) && !used_ipv6.contains(&ipv6) {
            return Ok(PreviewAddress { ipv4, ipv6 });
        }
    }
    Err(PreviewError::bad_request(
        "preview subnet has no free node IPs",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_ipv6_uses_the_configured_prefix_and_slot() {
        let config = Config::for_test();

        assert_eq!(
            preview_ipv6(&config, 11).unwrap(),
            "fd42:31:90::11".parse::<Ipv6Addr>().unwrap()
        );
    }

    #[test]
    fn preview_ipv4_uses_the_configured_prefix_and_slot() {
        let config = Config::for_test();

        assert_eq!(
            preview_ipv4(&config, 200).unwrap(),
            "172.31.90.200".parse::<Ipv4Addr>().unwrap()
        );
    }

    #[test]
    fn validate_slot_prefixes_rejects_a_prefix_that_cannot_form_an_address() {
        let mut config = Config::for_test();
        config.ip_prefix = "172.31.90.".to_owned();

        assert!(validate_slot_prefixes(&config).is_err());
    }
}
