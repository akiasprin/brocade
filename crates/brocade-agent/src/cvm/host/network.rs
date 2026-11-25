//! A native business NIC is a host macvlan attached to the existing transport bridge.
//! Only our links, policy tables and nft table are modified; the host main table is retained.
use super::*;
use std::collections::BTreeSet;
#[cfg(test)]
mod tests;

const MARK: u32 = 0x00b8_0000;
const MASK: u32 = 0xffff_0000;
const TABLE: &str = "brocade_host_output";
#[derive(Default, Serialize, Deserialize)]
struct Owned {
    instance: String,
    nics: BTreeMap<String, AllocatedNic>,
    retiring: Vec<(String, AllocatedNic)>,
    protected: Vec<IpPrefix>,
}
fn ownership_path(root: &Path) -> PathBuf {
    root.join("network").join("state.json")
}
fn ownership(root: &Path, instance: &str) -> Result<Owned> {
    let Some(bytes) = read_bounded(&ownership_path(root), 512 * 1024)? else {
        return Ok(Owned {
            instance: instance.into(),
            ..Owned::default()
        });
    };
    let value: Owned = serde_json::from_slice(&bytes)
        .map_err(|_| "invalid native network ownership; resources retained")?;
    if value.protected.len() > 4096
        || value.instance != instance
        || value.nics.len() + value.retiring.len() > MAX_INTERFACES * 2
        || value
            .retiring
            .iter()
            .any(|(id, n)| !valid_id(id) || n.slot >= 8192)
        || value
            .nics
            .iter()
            .any(|(id, n)| !valid_id(id) || n.slot >= 8192)
    {
        return Err("native network belongs to another instance or has invalid bounds".into());
    }
    Ok(value)
}
fn save_ownership(root: &Path, owned: &Owned) -> Result<()> {
    super::super::nics::private_dir(&root.join("network"))?;
    atomic_write_private(
        &ownership_path(root),
        &serde_json::to_vec(owned).map_err(|_| "invalid native ownership")?,
    )
}
fn fence_nic(instance: &str, id: &str, nic: &AllocatedNic) -> Result<()> {
    prepare_policy(nic)?;
    let dev = device(id);
    if checked(&dev, &alias(instance, id))? {
        ip(&["link", "delete", "dev", &dev])?;
    }
    super::super::nics::native_clear_flows([nic.ipv4, nic.ipv6].into_iter().flatten())?;
    Ok(())
}
fn clear_policy(nic: &AllocatedNic) -> Result<()> {
    let table = (MARK + u32::from(nic.slot)).to_string();
    for family in ["-4", "-6"] {
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&ip(&[family, "-j", "rule", "show"])?)
                .map_err(|_| "invalid native cleanup rule inventory")?;
        for _ in rows
            .iter()
            .filter(|r| r["priority"] == 31650 && r["table"].to_string().trim_matches('"') == table)
        {
            ip(&[
                family,
                "rule",
                "delete",
                "priority",
                "31650",
                "fwmark",
                &format!("{table}/0xffffffff"),
                "lookup",
                &table,
            ])?;
        }
        let routes: Vec<serde_json::Value> =
            serde_json::from_str(&ip(&[family, "-j", "route", "show", "table", "all"])?)
                .map_err(|_| "invalid native route cleanup inventory")?;
        if routes
            .iter()
            .any(|r| r["table"].to_string().trim_matches('"') == table)
        {
            ip(&[family, "route", "flush", "table", &table, "proto", "186"])?;
        }
    }
    Ok(())
}
fn device(id: &str) -> String {
    format!("bh{}", &brocade_core::hash::sha256_hex(id.as_bytes())[..12])
}
fn bridge(id: &str) -> String {
    format!("bn{}", &brocade_core::hash::sha256_hex(id.as_bytes())[..12])
}
fn alias(instance: &str, nic: &str) -> String {
    format!("brocade-host:{instance}:{nic}")
}
fn ip(args: &[&str]) -> Result<String> {
    command("ip", args)
}
fn link(dev: &str) -> Result<Option<serde_json::Value>> {
    let rows: Vec<serde_json::Value> = serde_json::from_str(&ip(&["-d", "-j", "link", "show"])?)
        .map_err(|_| "invalid host interface inventory")?;
    Ok(rows.into_iter().find(|r| r["ifname"] == dev))
}
fn checked(dev: &str, expected: &str) -> Result<bool> {
    let Some(row) = link(dev)? else {
        return Ok(false);
    };
    if row["ifalias"] != expected || row["linkinfo"]["info_kind"] != "macvlan" {
        return Err("native business interface belongs to another owner".into());
    }
    Ok(true)
}
pub(super) fn capabilities(caps: &mut Capabilities) -> Result<()> {
    // Actual privileged read, not just executable presence. Transport readiness is
    // independently reported by the NIC controller before Store allocates a NIC.
    command("nft", &["-j", "list", "tables"])?;
    ip(&["-j", "rule", "show"])?;
    caps.managed_networks = true;
    caps.configure_network = true;
    caps.primary_ip_delivery = true;
    caps.route_services = true;
    caps.hot_attach = true;
    caps.hot_detach = true;
    caps.chain_egress = match chain_capability() {
        Ok(()) => true,
        Err(error) => {
            // An optional chain prerequisite must not disable existing native
            // NICs, entry delivery, observations or route repair.
            eprintln!("native chain capability unavailable: {error}");
            false
        }
    };
    Ok(())
}

fn chain_capability() -> Result<()> {
    // Check the same kernel expressions used by the selector without installing
    // probe tables or changing the host's routing, firewall or sysctls.
    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .map_err(|_| "裸机线路出口需要可访问的 /dev/net/tun")?;
    let service = management_service()?;
    let mut probe = String::from("add table inet brocade_host_chain_probe\nadd chain inet brocade_host_chain_probe select { type route hook output priority -140; policy accept; }\nadd chain inet brocade_host_chain_probe guard { type filter hook postrouting priority -1; policy accept; }\nadd rule inet brocade_host_chain_probe select meta l4proto { tcp, udp } meta mark set 1 ct mark set meta mark return\nadd rule inet brocade_host_chain_probe guard meta mark 1 oifname != \"lo\" drop\n");
    if let Some(service) = service.as_deref() {
        probe.push_str(&format!(
            "add rule inet brocade_host_chain_probe select {} return\n",
            service_match(service)?
        ));
    }
    crate::command::run_command_with_input("nft", &["--check", "-f", "-"], probe.as_bytes())?;
    Ok(())
}

fn management_service() -> Result<Option<String>> {
    let text = String::from_utf8(read(Path::new("/proc/self/cgroup"), 65536)?)
        .map_err(|_| "invalid Agent service cgroup")?;
    let Some(path) = text.lines().find_map(|line| line.strip_prefix("0::/")) else {
        return Ok(None);
    };
    // Never exempt a whole slice, the root cgroup, or an interactive shell's
    // session scope. Only the Agent's own service and its managed children bypass.
    if !path.ends_with(".service") {
        return Ok(None);
    }
    service_match(path)?;
    Ok(Some(path.to_owned()))
}

fn service_match(path: &str) -> Result<String> {
    let parts = path.split('/').collect::<Vec<_>>();
    if path.len() > 4096
        || parts.len() > 32
        || !path.ends_with(".service")
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
        || path.bytes().any(|c| c.is_ascii_control())
    {
        return Err("invalid Agent service cgroup path".into());
    }
    let quoted =
        serde_json::to_string(path).map_err(|_| "invalid Agent service cgroup encoding")?;
    Ok(format!("socket cgroupv2 level {} {quoted}", parts.len()))
}
pub(super) fn interfaces(
    config: &Config,
    desired: &InstanceDesired,
) -> Result<Vec<InterfaceObservation>> {
    let mut out = Vec::new();
    for nic in &desired.spec.interfaces {
        if nic.connection.purpose == InterfacePurpose::Contact {
            out.push(primary(config, &nic.id)?);
        } else {
            let assigned = nic
                .allocated
                .as_ref()
                .ok_or("native NIC has no allocation")?;
            let dev = device(&nic.id);
            if !checked(&dev, &alias(&desired.id, &nic.id))? {
                return Err("native business NIC is absent".into());
            }
            let raw = ip(&["-j", "address", "show", "dev", &dev])?;
            let rows: Vec<serde_json::Value> =
                serde_json::from_str(&raw).map_err(|_| "invalid native address inventory")?;
            let row = rows.first().ok_or("native address inventory is empty")?;
            let addresses = row["addr_info"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| {
                    Some(format!(
                        "{}/{}",
                        v["local"].as_str()?,
                        v["prefixlen"].as_u64()?
                    ))
                })
                .collect();
            if row["address"].as_str() != Some(assigned.mac.as_str()) {
                return Err("native NIC MAC differs from allocation".into());
            }
            out.push(InterfaceObservation {
                id: nic.id.clone(),
                device: dev,
                mac: assigned.mac.clone(),
                addresses,
                attached: true,
            });
        }
    }
    Ok(out)
}
// A delivery address is the existing primary address. Entry rules below explicitly
// authorize local INPUT rather than exposing every service reachable over a tunnel.
pub(super) fn delivery(_: &Path, desired: &InstanceDesired) -> Result<Option<String>> {
    if desired.power != PowerState::Running {
        return Ok(None);
    }
    let report = super::super::backend::config_path()?;
    let config = load_config(&report)?;
    Ok(primary(&config, "host")?
        .addresses
        .into_iter()
        .find_map(|a| {
            a.split('/')
                .next()?
                .parse::<std::net::Ipv4Addr>()
                .ok()
                .map(|a| a.to_string())
        }))
}
fn table_owned() -> Result<bool> {
    let value: serde_json::Value =
        serde_json::from_str(&command("nft", &["-j", "list", "tables"])?)
            .map_err(|_| "invalid native firewall inventory")?;
    if let Some(row) = value["nftables"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| &v["table"])
        .find(|v| v["family"] == "inet" && v["name"] == TABLE)
    {
        if row["comment"] != "brocade-native-output-v1" {
            return Err("native firewall table belongs to another owner".into());
        }
        return Ok(true);
    }
    Ok(false)
}
fn rules(
    desired: &InstanceDesired,
    protected: &BTreeSet<IpPrefix>,
    service: Option<&str>,
) -> Result<String> {
    let devices = desired
        .spec
        .interfaces
        .iter()
        .filter_map(|n| n.allocated.clone().map(|a| (n.id.clone(), a)))
        .collect::<BTreeMap<_, _>>();
    desired.spec.routing.validate(&devices)?;
    let chains = brocade_cvm_compile::chain_bindings(&desired.id, &desired.spec.routing);
    let mut out = format!("add table inet {TABLE} {{ comment \"brocade-native-output-v1\"; }}\nadd chain inet {TABLE} return_marks {{ type filter hook prerouting priority -140; policy accept; }}\nadd chain inet {TABLE} select {{ type route hook output priority -140; policy accept; }}\nadd chain inet {TABLE} source {{ type nat hook postrouting priority 99; }}\nadd chain inet {TABLE} inbound {{ type filter hook input priority -125; policy accept; }}\n");
    if !chains.is_empty() {
        out.push_str(&format!("add chain inet {TABLE} chain_guard {{ type filter hook postrouting priority -1; policy accept; }}\n"));
    }
    for binding in &chains {
        // Restore the reverse-path mark only on our TUN. Local replies still
        // resolve via the local table; strict IPv4 rp_filter sees the correct TUN.
        out.push_str(&format!("add rule inet {TABLE} return_marks iifname \"{}\" ct direction reply ct mark {} meta mark set ct mark\n",binding.device,binding.mark));
        // This also fences an offline restore before the client has recreated
        // its policy table. A failed lookup must not use the host main default.
        out.push_str(&format!(
            "add rule inet {TABLE} chain_guard meta mark {} oifname != \"{}\" counter drop\n",
            binding.mark, binding.device
        ));
    }
    for id in devices.keys() {
        out.push_str(&format!("add rule inet {TABLE} return_marks iifname \"{}\" ct mark & {MASK:#x} == {MARK:#x} meta mark set ct mark\n", device(id)));
    }
    let mut push = |text: String| {
        out.push_str(&format!("add rule inet {TABLE} select {text}\n"));
    };
    push("oifname lo return".into());
    if !chains.is_empty() {
        if let Some(service) = service {
            push(format!("{} return", service_match(service)?));
        }
    }
    for id in devices.keys() {
        push(format!("oifname \"{}\" ip6 hoplimit 255 icmpv6 type {{ nd-neighbor-solicit, nd-neighbor-advert }} return", device(id)));
    }
    // Explicit management destinations take precedence, including the Console and
    // tunnel endpoints. Existing foreign marks (entry replies, backbone) stay intact.
    for prefix in [
        "169.254.0.0/16",
        "224.0.0.0/4",
        "255.255.255.255/32",
        "fe80::/10",
        "ff00::/8",
    ]
    .into_iter()
    .map(str::parse::<IpPrefix>)
    .collect::<Result<Vec<_>>>()?
    {
        push(format!(
            "{} daddr {prefix} return",
            if prefix.is_ipv4() { "ip" } else { "ip6" }
        ));
    }
    for prefix in protected {
        push(format!(
            "{} daddr {prefix} return",
            if prefix.is_ipv4() { "ip" } else { "ip6" }
        ));
    }
    push(format!(
        "meta mark != 0 meta mark & {MASK:#x} != {MARK:#x} return"
    ));
    push(format!(
        "ct mark & {MASK:#x} == {MARK:#x} meta mark set ct mark return"
    ));
    push("ct direction reply return".into());
    for binding in &chains {
        push(format!(
            "ct mark {} meta mark set ct mark return",
            binding.mark
        ));
    }
    let routing = &desired.spec.routing;
    let mut routes = routing
        .routes
        .iter()
        .filter_map(|route| {
            let priority = routing.exit_priority(&route.target, route.destination.is_ipv4())?;
            Some((route, priority))
        })
        .collect::<Vec<_>>();
    // The table was validated above, so every route has a per-family priority.
    routes.sort_by_key(|&(route, priority)| (route.precedence(priority), &route.target));
    for (r, _) in routes {
        let mark = match &r.target {
            RouteTarget::Interface { interface_id } => {
                MARK + u32::from(
                    devices
                        .get(interface_id)
                        .ok_or("native route references an unknown NIC")?
                        .slot,
                )
            }
            RouteTarget::Chain { chain_id } => {
                brocade_cvm_compile::chain_binding(&desired.id, chain_id).mark
            }
        };
        let service = r.service.map_or_else(
            || {
                if r.target.chain_id().is_some() {
                    " meta l4proto { tcp, udp }".into()
                } else {
                    String::new()
                }
            },
            |s| {
                format!(
                    " meta l4proto {} th dport {}-{}",
                    if s.protocol == L4Protocol::Tcp {
                        "tcp"
                    } else {
                        "udp"
                    },
                    s.port_start,
                    s.port_end
                )
            },
        );
        for (other_id, other) in &devices {
            if Some(other_id.as_str()) == r.target.interface_id() {
                continue;
            }
            if let Some(source) = if r.destination.is_ipv4() {
                other.ipv4
            } else {
                other.ipv6
            } {
                push(format!(
                    "{} saddr {source} {} daddr {}{service} drop",
                    if source.is_ipv4() { "ip" } else { "ip6" },
                    if r.destination.is_ipv4() { "ip" } else { "ip6" },
                    r.destination
                ));
            }
        }
        push(format!(
            "{} daddr {}{service} meta mark set {mark:#x} ct mark set meta mark return",
            if r.destination.is_ipv4() { "ip" } else { "ip6" },
            r.destination
        ));
    }
    for nic in devices.values() {
        for address in [nic.ipv4, nic.ipv6].into_iter().flatten() {
            push(format!(
                "{} saddr {address} drop",
                if address.is_ipv4() { "ip" } else { "ip6" }
            ));
        }
    }
    for nic in devices.values() {
        let mark = MARK + u32::from(nic.slot);
        for address in [nic.ipv4, nic.ipv6].into_iter().flatten() {
            out.push_str(&format!("add rule inet {TABLE} source meta mark {mark:#x} {} saddr != {address} snat {} to {address}\n",if address.is_ipv4(){"ip"}else{"ip6"},if address.is_ipv4(){"ip"}else{"ip6"}));
        }
    }
    for nic in devices.values() {
        for address in [nic.ipv4, nic.ipv6].into_iter().flatten() {
            let family = if address.is_ipv4() { "ip" } else { "ip6" };
            if address.is_ipv6() {
                let (id, _) = devices
                    .iter()
                    .find(|(_, n)| n.slot == nic.slot)
                    .ok_or("native NIC ownership is absent")?;
                out.push_str(&format!("add rule inet {TABLE} inbound iifname \"{}\" ip6 hoplimit 255 icmpv6 type {{ nd-neighbor-solicit, nd-neighbor-advert }} accept\n",device(id)));
            }
            out.push_str(&format!("add rule inet {TABLE} inbound ct direction original {family} daddr {address} drop\nadd rule inet {TABLE} inbound ct state untracked {family} daddr {address} drop\n"));
        }
    }

    Ok(out)
}
pub(super) enum Convergence {
    Applied,
    WaitingForTransport,
}

pub(super) fn converge(
    config: &Config,
    root: &Path,
    desired: &InstanceDesired,
) -> Result<Convergence> {
    if desired.spec.interfaces.len() > MAX_INTERFACES
        || desired
            .spec
            .interfaces
            .iter()
            .filter(|n| n.connection.purpose == InterfacePurpose::Contact)
            .count()
            != 1
    {
        return Err("native instance requires one original management interface".into());
    }
    let mut owned = ownership(root, &desired.id)?;
    let requested = desired
        .spec
        .interfaces
        .iter()
        .filter_map(|n| n.allocated.clone().map(|a| (n.id.clone(), a)))
        .collect::<BTreeMap<_, _>>();
    desired.spec.routing.validate(&requested)?;
    for (id, nic) in &requested {
        if !valid_id(id)
            || nic.slot >= 8192
            || !nic.mac.starts_with("02:")
            || nic.mac.split(':').count() != 6
            || nic
                .mac
                .split(':')
                .any(|v| v.len() != 2 || !v.bytes().all(|c| c.is_ascii_hexdigit()))
            || nic.mtu < 1280
            || nic.mtu > 1500
            || [nic.ipv4, nic.ipv6]
                .into_iter()
                .flatten()
                .any(|a| a.is_loopback() || a.is_unspecified() || a.is_multicast())
        {
            return Err("invalid native NIC allocation".into());
        }
        if owned.nics.get(id).is_none_or(|old| old.slot != nic.slot)
            && !owned.retiring.iter().any(|(_, old)| old.slot == nic.slot)
        {
            let table = (MARK + u32::from(nic.slot)).to_string();
            for family in ["-4", "-6"] {
                for category in ["route", "rule"] {
                    let args = if category == "route" {
                        vec![family, "-j", category, "show", "table", "all"]
                    } else {
                        vec![family, "-j", category, "show"]
                    };
                    let rows: Vec<serde_json::Value> = serde_json::from_str(&ip(&args)?)
                        .map_err(|_| "invalid existing routing inventory")?;
                    if rows
                        .iter()
                        .any(|r| r["table"].to_string().trim_matches('"') == table)
                    {
                        return Err(
                            "native routing table is already owned by another service".into()
                        );
                    }
                }
            }
        }
    }
    let snapshot = super::super::nics::native_snapshot()?;
    let Some(_activation) = super::super::nics::readiness::activation(desired, &snapshot)? else {
        return Ok(Convergence::WaitingForTransport);
    };
    if desired.power != PowerState::Deleted {
        if !desired.spec.routing.dns.is_empty() {
            return Err(
                "native DNS remains owned by the host; configure DNS in the operating system"
                    .into(),
            );
        }
        // Neither saved intent nor a bridge left by Preparing proves that the
        // Active forwarding policy was installed. Wait before changing ownership,
        // retiring the old NIC, or selecting any traffic into the new policy.
        for id in requested.keys() {
            let Some(parent) = link(&bridge(id))? else {
                return Ok(Convergence::WaitingForTransport);
            };
            if parent["ifalias"] != format!("brocade-nic:{id}") {
                return Err("native transport bridge ownership mismatch".into());
            }
        }
    }
    let active = if desired.power == PowerState::Deleted {
        BTreeMap::new()
    } else {
        requested.clone()
    };
    for (id, old) in &owned.nics {
        if active.get(id).is_none_or(|n| {
            n.slot != old.slot || n.mac != old.mac || n.ipv4 != old.ipv4 || n.ipv6 != old.ipv6
        }) {
            owned.retiring.push((id.clone(), old.clone()));
        }
    }
    owned.nics = active;
    if owned.nics.len() + owned.retiring.len() > MAX_INTERFACES * 2 {
        return Err("native pending network cleanup exceeds limit".into());
    }
    save_ownership(root, &owned)?;
    // Keep unreachable policy rules until the old OUTPUT selectors are gone.
    // A crash may retain a fence, but cannot open a fallback to the host default.
    for (id, nic) in &owned.retiring {
        fence_nic(&desired.id, id, nic)?;
    }
    let mut protected = config
        .protected_destinations
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    protected.extend(snapshot.management_networks.iter().cloned());
    for binding in &snapshot.bindings {
        protected.insert(
            format!(
                "{}/{}",
                binding.offer.endpoint,
                if binding.offer.endpoint.is_ipv4() {
                    32
                } else {
                    128
                }
            )
            .parse()?,
        );
    }
    if !desired.spec.routing.routes.is_empty() {
        match control_destinations() {
            Ok(current) => protected.extend(current),
            Err(error) if owned.protected.is_empty() => return Err(error),
            Err(_) => {}
        }
        protected.extend(owned.protected.iter().cloned());
    }
    if protected.len() > 4096 {
        return Err("native management route inventory exceeds limit".into());
    }
    owned.protected = protected.iter().cloned().collect();
    save_ownership(root, &owned)?;
    let current = desired
        .spec
        .interfaces
        .iter()
        .map(|n| n.id.as_str())
        .collect::<BTreeSet<_>>();
    let inventory: Vec<serde_json::Value> =
        serde_json::from_str(&ip(&["-d", "-j", "link", "show"])?)
            .map_err(|_| "invalid native link inventory")?;
    for row in inventory {
        let Some(owner) = row["ifalias"]
            .as_str()
            .and_then(|a| a.strip_prefix(&format!("brocade-host:{}:", desired.id)))
        else {
            continue;
        };
        if desired.power == PowerState::Deleted || !current.contains(owner) {
            let dev = device(owner);
            if row["ifname"] != dev || !checked(&dev, &alias(&desired.id, owner))? {
                return Err("native cleanup ownership mismatch".into());
            }
            ip(&["link", "delete", "dev", &dev])?;
        }
    }
    let mut plan = if table_owned()? {
        format!("delete table inet {TABLE}\n")
    } else {
        String::new()
    };
    if desired.power == PowerState::Deleted {
        if !plan.is_empty() {
            let file = root.join("output.nft");
            atomic_write_private(&file, plan.as_bytes())?;
            command(
                "nft",
                &["-f", file.to_str().ok_or("invalid native firewall path")?],
            )?;
        }
        if !owned.nics.is_empty() {
            return Err("native NIC cleanup is not complete".into());
        }
        for (_, nic) in &owned.retiring {
            clear_policy(nic)?;
        }
        std::fs::remove_file(ownership_path(root))
            .map_err(|_| "cannot finish native network release")?;
        return Ok(Convergence::Applied);
    }
    let mut tracking = super::super::nics::tracking_addresses()?;
    if let Ok(primary) = primary(config, "host") {
        tracking.extend(
            primary
                .addresses
                .iter()
                .filter_map(|a| a.split('/').next()?.parse::<std::net::IpAddr>().ok()),
        );
    }
    tracking.extend(
        requested
            .values()
            .flat_map(|n| [n.ipv4, n.ipv6].into_iter().flatten()),
    );
    crate::conntrack::track_delivery_scopes(&tracking, &super::super::nics::tracking_ports()?)?;
    for assigned in requested.values() {
        prepare_policy(assigned)?;
    }
    for nic in &desired.spec.interfaces {
        let Some(assigned) = &nic.allocated else {
            continue;
        };
        if assigned.slot >= 8192 || !valid_id(&nic.id) {
            return Err("invalid native NIC identity".into());
        }
        let dev = device(&nic.id);
        let br = bridge(&nic.id);
        let owner = alias(&desired.id, &nic.id);
        let Some(parent) = link(&br)? else {
            // The bridge disappeared after preflight. Preserve existing selectors
            // and do not activate the candidate while its transport is missing.
            return Ok(Convergence::WaitingForTransport);
        };
        if parent["ifalias"] != format!("brocade-nic:{}", nic.id) {
            return Err("native transport bridge ownership mismatch".into());
        }
        if !checked(&dev, &owner)? {
            // Claim intent is durable before creating the link. Unknown unlabelled
            // interfaces are not adopted after a crash.
            ip(&[
                "link",
                "add",
                "link",
                &br,
                "name",
                &dev,
                "address",
                &assigned.mac,
                "type",
                "macvlan",
                "mode",
                "bridge",
            ])?;
            ip(&["link", "set", "dev", &dev, "alias", &owner])?;
        }
        ip(&[
            "link",
            "set",
            "dev",
            &dev,
            "mtu",
            &assigned.mtu.to_string(),
            "up",
        ])?;
        // Reverse path validation uses the restored connection mark on our link.
        // Preserve all administrator settings on the host's original interfaces.
        command(
            "sysctl",
            &["-qw", &format!("net.ipv4.conf.{dev}.src_valid_mark=1")],
        )?;
        let table = (MARK + u32::from(assigned.slot)).to_string();
        for (family, address, gateway) in [
            ("-4", assigned.ipv4, "169.254.128.1"),
            ("-6", assigned.ipv6, "fe80::1"),
        ] {
            let Some(address) = address else {
                continue;
            };
            ip(&[
                family,
                "address",
                "replace",
                &format!("{address}/{}", if address.is_ipv4() { 32 } else { 128 }),
                "dev",
                &dev,
                "nodad",
            ])?;
            ip(&[
                family,
                "route",
                "replace",
                "default",
                "via",
                gateway,
                "dev",
                &dev,
                "onlink",
                "src",
                &address.to_string(),
                "table",
                &table,
                "metric",
                "10",
                "proto",
                "186",
            ])?;
        }
    }
    plan.push_str(&rules(
        desired,
        &protected,
        management_service()?.as_deref(),
    )?);
    apply_rules(root, &plan)?;
    for (_, nic) in &owned.retiring {
        if !requested.values().any(|active| active.slot == nic.slot) {
            clear_policy(nic)?;
        }
    }
    owned.retiring.clear();
    save_ownership(root, &owned)?;
    Ok(Convergence::Applied)
}

fn apply_rules(root: &Path, plan: &str) -> Result<()> {
    let file = root.join("output.nft");
    atomic_write_private(&file, plan.as_bytes())?;
    command(
        "nft",
        &[
            "--check",
            "-f",
            file.to_str().ok_or("invalid native firewall path")?,
        ],
    )?;
    command(
        "nft",
        &["-f", file.to_str().ok_or("invalid native firewall path")?],
    )?;
    Ok(())
}
pub(super) fn inspect_empty() -> Result<()> {
    if table_owned()? {
        return Err("native firewall is still present".into());
    }
    let links: Vec<serde_json::Value> = serde_json::from_str(&ip(&["-j", "link", "show"])?)
        .map_err(|_| "invalid native link cleanup evidence")?;
    if links.iter().any(|l| {
        l["ifalias"]
            .as_str()
            .is_some_and(|a| a.starts_with("brocade-host:"))
    }) {
        return Err("native business interfaces are still present".into());
    }
    for family in ["-4", "-6"] {
        for category in ["rule", "route"] {
            let args = if category == "route" {
                vec![family, "-j", category, "show", "table", "all"]
            } else {
                vec![family, "-j", category, "show"]
            };
            let rows: Vec<serde_json::Value> = serde_json::from_str(&ip(&args)?)
                .map_err(|_| "invalid native routing cleanup evidence")?;
            if rows.iter().any(|r| {
                r["table"]
                    .as_u64()
                    .or_else(|| r["table"].as_str()?.parse().ok())
                    .is_some_and(|t| (u64::from(MARK)..u64::from(MARK) + 8192).contains(&t))
            }) {
                return Err("native routing policies are still present".into());
            }
        }
    }
    Ok(())
}

fn prepare_policy(nic: &AllocatedNic) -> Result<()> {
    let table = (MARK + u32::from(nic.slot)).to_string();
    for (family, address) in [("-4", nic.ipv4), ("-6", nic.ipv6)] {
        if address.is_none() {
            continue;
        }
        ip(&[
            family,
            "route",
            "replace",
            "unreachable",
            "default",
            "table",
            &table,
            "metric",
            "32767",
            "proto",
            "186",
        ])?;
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&ip(&[family, "-j", "rule", "show"])?)
                .map_err(|_| "invalid native policy inventory")?;
        if !rows
            .iter()
            .any(|r| r["priority"] == 31650 && r["table"].to_string().trim_matches('"') == table)
        {
            ip(&[
                family,
                "rule",
                "add",
                "priority",
                "31650",
                "fwmark",
                &format!("{table}/0xffffffff"),
                "lookup",
                &table,
            ])?;
        }
    }
    Ok(())
}
