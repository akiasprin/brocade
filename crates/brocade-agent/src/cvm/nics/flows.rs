//! Release evidence includes conntrack, whose NAT mappings outlive nft rules.
use super::*;
fn purge(ns: Option<&str>, filter: &[String]) -> Result<()> {
    let execute = |args: &[&str]| {
        let mut command = Vec::new();
        if let Some(ns) = ns {
            command.extend(["netns", "exec", ns, "conntrack"]);
        }
        command.extend_from_slice(args);
        capture_command_with_timeout(
            if ns.is_some() { "ip" } else { "conntrack" },
            &command,
            Duration::from_secs(10),
        )
    };
    let args = std::iter::once("-D")
        .chain(filter.iter().map(String::as_str))
        .collect::<Vec<_>>();
    let removed = execute(&args).map_err(|_| "cannot clear allocated NIC connections")?;
    if !removed.status.success() && removed.status.code() != Some(1) {
        return Err("NIC connection removal failed; resources retained".into());
    }
    let args = std::iter::once("-L")
        .chain(filter.iter().map(String::as_str))
        .collect::<Vec<_>>();
    let remaining = execute(&args).map_err(|_| "cannot verify NIC connection cleanup")?;
    if !remaining.status.success() || !remaining.stdout.is_empty() {
        return Err("NIC connection cleanup is unconfirmed; resources retained".into());
    }
    Ok(())
}
pub(super) fn addresses(addresses: impl IntoIterator<Item = IpAddr>) -> Result<()> {
    for address in addresses {
        for selector in ["-s", "-d", "-r", "-q"] {
            purge(
                None,
                &[
                    "-f".into(),
                    if address.is_ipv4() {
                        "ipv4".into()
                    } else {
                        "ipv6".into()
                    },
                    selector.into(),
                    address.to_string(),
                ],
            )?;
        }
    }
    Ok(())
}
pub(super) fn business(b: &NicBinding) -> Result<()> {
    let l = host::layout(b);
    addresses([IpAddr::V4(l.business_v4), IpAddr::V6(l.business_v6)])?;
    if b.side == BindingSide::Provider && b.nic.address_mode == NicAddressMode::DedicatedRouted {
        addresses([b.nic.ipv4, b.nic.ipv6].into_iter().flatten())?;
    }
    let ns = host::namespace(b);
    if Path::new("/run/netns").join(&ns).exists() {
        for address in [b.nic.ipv4, b.nic.ipv6].into_iter().flatten() {
            for selector in ["-s", "-d", "-r", "-q"] {
                purge(
                    Some(&ns),
                    &[
                        "-f".into(),
                        if address.is_ipv4() {
                            "ipv4".into()
                        } else {
                            "ipv6".into()
                        },
                        selector.into(),
                        address.to_string(),
                    ],
                )?;
            }
        }
    }
    Ok(())
}
pub(super) fn transport(b: &NicBinding) -> Result<()> {
    if b.offer.listen_side.resolved()? != b.side {
        return Ok(());
    }
    let raw = run_command("ip", &["-j", "address", "show"])
        .map_err(|_| "cannot inspect NIC transport listener addresses")?;
    let links: Vec<serde_json::Value> =
        serde_json::from_str(&raw).map_err(|_| "invalid transport listener inventory")?;
    let mut addresses = BTreeSet::from([b.offer.endpoint]);
    for link in links {
        if link["ifalias"]
            .as_str()
            .is_some_and(|alias| alias.starts_with("brocade-nic:"))
        {
            continue;
        }
        for entry in link["addr_info"].as_array().into_iter().flatten() {
            if let Some(address) = entry["local"]
                .as_str()
                .and_then(|s| s.parse::<IpAddr>().ok())
                .filter(|address| address.is_ipv4() == b.offer.endpoint.is_ipv4())
            {
                addresses.insert(address);
            }
        }
    }
    for address in addresses {
        purge(
            None,
            &[
                "-f".into(),
                if address.is_ipv4() {
                    "ipv4".into()
                } else {
                    "ipv6".into()
                },
                "-p".into(),
                if b.offer.kind == TransportKind::Wireguard {
                    "udp".into()
                } else {
                    "tcp".into()
                },
                "-d".into(),
                address.to_string(),
                "--dport".into(),
                b.port.to_string(),
            ],
        )?;
    }
    Ok(())
}
