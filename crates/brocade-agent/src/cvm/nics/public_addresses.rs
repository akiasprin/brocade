//! Dedicated addresses are routed to the owned namespace, never moved off a host NIC.
use super::*;
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Asset {
    address: IpAddr,
    upstream: String,
    link: String,
    gateway: IpAddr,
}
fn path(root: &Path, b: &NicBinding) -> PathBuf {
    root.join("cvm-nics")
        .join(format!("public-{}.json", b.interface_id))
}
fn ip(args: &[String]) -> Result<String> {
    run_command("ip", &args.iter().map(String::as_str).collect::<Vec<_>>())
        .map_err(|_| "dedicated address routing operation failed".into())
}
fn family(address: IpAddr) -> String {
    if address.is_ipv4() {
        "-4".into()
    } else {
        "-6".into()
    }
}
fn prefix(address: IpAddr) -> String {
    format!("{address}/{}", if address.is_ipv4() { 32 } else { 128 })
}
fn routes(address: IpAddr) -> Result<Vec<serde_json::Value>> {
    let raw = ip(&[
        family(address),
        "-N".into(),
        "-j".into(),
        "route".into(),
        "show".into(),
        "exact".into(),
        prefix(address),
    ])?;
    serde_json::from_str(&raw).map_err(|_| "invalid dedicated address route inventory".into())
}
fn owned(route: &serde_json::Value, asset: &Asset) -> bool {
    route["dev"].as_str() == Some(&asset.link)
        && route["gateway"].as_str() == Some(asset.gateway.to_string().as_str())
        && (route["protocol"].as_str() == Some("186") || route["protocol"].as_u64() == Some(186))
}
fn upstream(address: IpAddr) -> Result<String> {
    let raw = ip(&[
        family(address),
        "-j".into(),
        "route".into(),
        "get".into(),
        address.to_string(),
    ])?;
    let values: Vec<serde_json::Value> =
        serde_json::from_str(&raw).map_err(|_| "invalid dedicated address upstream route")?;
    let dev = values
        .first()
        .and_then(|route| route["dev"].as_str())
        .ok_or("dedicated address has no upstream route")?;
    if !host::external_interface(dev)?
        || dev.len() > 15
        || !dev
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
    {
        return Err("dedicated public address conflicts with host or managed routing".into());
    }
    Ok(dev.into())
}
fn proxy_exists(asset: &Asset) -> Result<bool> {
    let raw = ip(&[
        family(asset.address),
        "-j".into(),
        "neigh".into(),
        "show".into(),
        "proxy".into(),
        "dev".into(),
        asset.upstream.clone(),
    ])?;
    let values: Vec<serde_json::Value> =
        serde_json::from_str(&raw).map_err(|_| "invalid public proxy neighbor inventory")?;
    Ok(values
        .iter()
        .any(|entry| entry["dst"].as_str() == Some(asset.address.to_string().as_str())))
}
fn install(asset: &Asset) -> Result<()> {
    if routes(asset.address)?
        .iter()
        .any(|route| !owned(route, asset))
    {
        return Err("dedicated address route belongs to another owner".into());
    }
    ip(&[
        family(asset.address),
        "route".into(),
        "replace".into(),
        prefix(asset.address),
        "via".into(),
        asset.gateway.to_string(),
        "dev".into(),
        asset.link.clone(),
        "proto".into(),
        "186".into(),
    ])?;
    if asset.address.is_ipv6() {
        run_command(
            "sysctl",
            &[
                "-qw",
                &format!("net/ipv6/conf/{}/proxy_ndp=1", asset.upstream),
            ],
        )
        .map_err(|_| "cannot enable dedicated IPv6 proxy neighbor")?;
    }
    if !proxy_exists(asset)? {
        ip(&[
            family(asset.address),
            "neigh".into(),
            "add".into(),
            "proxy".into(),
            asset.address.to_string(),
            "dev".into(),
            asset.upstream.clone(),
        ])?;
    }
    Ok(())
}
fn remove(asset: &Asset) -> Result<()> {
    for route in routes(asset.address)? {
        if !owned(&route, asset) {
            return Err("dedicated address route ownership changed; allocation retained".into());
        }
        ip(&[
            family(asset.address),
            "route".into(),
            "del".into(),
            prefix(asset.address),
            "via".into(),
            asset.gateway.to_string(),
            "dev".into(),
            asset.link.clone(),
            "proto".into(),
            "186".into(),
        ])?;
    }
    if proxy_exists(asset)? {
        ip(&[
            family(asset.address),
            "neigh".into(),
            "del".into(),
            "proxy".into(),
            asset.address.to_string(),
            "dev".into(),
            asset.upstream.clone(),
        ])?;
    }
    super::flows::addresses([asset.address])
}
pub(super) fn reconcile(root: &Path, b: &NicBinding, peers: &[&NicBinding]) -> Result<()> {
    if b.side != BindingSide::Provider {
        return Ok(());
    }
    let saved = read_bounded(&path(root, b), 32768)?;
    let mut held: Vec<Asset> = saved
        .map(|bytes| {
            serde_json::from_slice(&bytes)
                .map_err(|_| "invalid dedicated address ownership evidence")
        })
        .transpose()?
        .unwrap_or_default();
    let mut wanted = BTreeSet::new();
    for peer in peers
        .iter()
        .filter(|peer| peer.nic.address_mode != NicAddressMode::SharedNat)
    {
        wanted.extend(
            [peer.nic.public_ipv4, peer.nic.public_ipv6]
                .into_iter()
                .flatten(),
        );
    }
    for address in &wanted {
        if held.iter().any(|asset| asset.address == *address) {
            continue;
        }
        let l = host::layout(b);
        let asset = Asset {
            address: *address,
            upstream: upstream(*address)?,
            link: l.root_link,
            gateway: if address.is_ipv4() {
                l.namespace_v4.into()
            } else {
                l.namespace_v6.into()
            },
        };
        if !routes(*address)?.is_empty() || proxy_exists(&asset)? {
            return Err("dedicated address already has unowned route or proxy neighbor".into());
        }
        held.push(asset);
        // Persist creation intent before installing either kernel resource.
        atomic_write_private(
            &path(root, b),
            &serde_json::to_vec(&held).map_err(|_| "cannot encode public address ownership")?,
        )?;
    }
    for asset in &held {
        if wanted.contains(&asset.address) {
            install(asset)?;
        } else {
            remove(asset)?;
        }
    }
    held.retain(|asset| wanted.contains(&asset.address));
    atomic_write_private(
        &path(root, b),
        &serde_json::to_vec(&held).map_err(|_| "cannot encode public address ownership")?,
    )
}
