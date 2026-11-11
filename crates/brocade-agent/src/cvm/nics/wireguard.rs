use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};

pub(super) fn apply(root: &Path, binding: &NicBinding) -> Result<LinkObservation> {
    if STANDARD
        .decode(&binding.credential)
        .map_or(true, |key| key.len() != 32)
        || STANDARD
            .decode(&binding.peer_credential)
            .map_or(true, |key| key.len() != 32)
    {
        return Err("invalid WireGuard binding credentials".into());
    }
    let ns = host::namespace(binding);
    let device = host::tunnel(binding);
    let owner = format!("brocade-binding:{}", binding.id);
    host::ensure_ns_link(root, &ns, &device, "wireguard", &owner)?;
    let directory = root.join("cvm-nics").join(format!(
        "{}-{}",
        binding.id,
        if binding.side == BindingSide::Consumer {
            "c"
        } else {
            "p"
        }
    ));
    private_dir(&directory)?;
    let path = directory.join("wireguard.key");
    atomic_write_private(&path, format!("{}\n", binding.credential).as_bytes())?;
    let peer_addresses = if binding.side == BindingSide::Consumer {
        [
            binding.nic.ipv4.map(|_| "0.0.0.0/0".to_owned()),
            binding.nic.ipv6.map(|_| "::/0".to_owned()),
        ]
    } else {
        [
            binding.nic.ipv4.map(|ip| format!("{ip}/32")),
            binding.nic.ipv6.map(|ip| format!("{ip}/128")),
        ]
    }
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",");
    let mut args = vec![
        "wg".into(),
        "set".into(),
        device.clone(),
        "private-key".into(),
        path.to_str()
            .ok_or("invalid private WireGuard key path")?
            .into(),
        "listen-port".into(),
        binding.port.to_string(),
        "fwmark".into(),
        "186".into(),
        "peer".into(),
        binding.peer_credential.clone(),
        "allowed-ips".into(),
        peer_addresses,
    ];
    if binding.side != binding.offer.listen_side.resolved()? {
        args.extend([
            "endpoint".into(),
            std::net::SocketAddr::new(binding.offer.endpoint, binding.port).to_string(),
            "persistent-keepalive".into(),
            "25".into(),
        ]);
    }
    host::ns_command(&ns, &args).map_err(|_| "WireGuard binding configuration failed")?;
    host::ns_ip(
        &ns,
        &[
            "link".into(),
            "set".into(),
            "dev".into(),
            device.clone(),
            "mtu".into(),
            binding.nic.mtu.to_string(),
            "up".into(),
        ],
    )?;
    host::limits(binding)?;
    let handshakes = host::ns_command(
        &ns,
        &[
            "wg".into(),
            "show".into(),
            device.clone(),
            "latest-handshakes".into(),
        ],
    )?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "invalid host clock")?
        .as_secs();
    let reachable = handshakes
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
        .any(|timestamp| timestamp > 0 && now.saturating_sub(timestamp) < 180);
    let (rx, tx) = host::counters(&ns, &device)?;
    Ok(LinkObservation {
        configured: true,
        reachable: Some(reachable),
        rx,
        tx,
    })
}
pub(super) fn remove(binding: &NicBinding) -> Result<()> {
    if binding.nic.transport != TransportKind::Wireguard {
        return Ok(());
    }
    host::remove_ns_link(
        &host::namespace(binding),
        &host::tunnel(binding),
        &format!("brocade-binding:{}", binding.id),
    )
}
