use super::*;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::{
    fs::{File, OpenOptions},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Layout {
    pub namespace: String,
    pub root_link: String,
    pub bridge: String,
    pub root_v4: Ipv4Addr,
    pub namespace_v4: Ipv4Addr,
    pub business_v4: Ipv4Addr,
    pub root_v6: Ipv6Addr,
    pub namespace_v6: Ipv6Addr,
    pub business_v6: Ipv6Addr,
}
fn suffix(side: BindingSide) -> &'static str {
    if side == BindingSide::Consumer {
        "c"
    } else {
        "p"
    }
}
fn digest(id: &str) -> String {
    brocade_core::hash::sha256_hex(id.as_bytes())[..12].to_owned()
}
pub(super) fn namespace(binding: &NicBinding) -> String {
    format!(
        "bcn-{}-{}",
        digest(&binding.interface_id),
        suffix(binding.side)
    )
}
pub(super) fn tunnel(binding: &NicBinding) -> String {
    format!("bt{}", digest(&binding.id))
}
pub(super) fn layout(binding: &NicBinding) -> Layout {
    let id = digest(&binding.interface_id);
    let index = u32::from(binding.nic.slot) * 2 + u32::from(binding.side == BindingSide::Provider);
    let base = u32::from(Ipv4Addr::new(198, 18, 0, 0)) + index * 8;
    let v6 = |host| Ipv6Addr::new(0xfd42, 0x6263, 0x6e69, index as u16, 0, 0, 0, host);
    Layout {
        namespace: namespace(binding),
        root_link: format!("bo{}{id}", suffix(binding.side)),
        bridge: format!("bn{id}"),
        root_v4: Ipv4Addr::from(base + 1),
        namespace_v4: Ipv4Addr::from(base + 2),
        business_v4: Ipv4Addr::from(base + 3),
        root_v6: v6(1),
        namespace_v6: v6(2),
        business_v6: v6(3),
    }
}
fn anchor_path(root: &Path, b: &NicBinding) -> PathBuf {
    root.join("cvm-nics")
        .join(format!("anchor-{}-{}.json", b.interface_id, suffix(b.side)))
}
fn owner(b: &NicBinding) -> String {
    format!("brocade-nic:{}:{}", b.interface_id, suffix(b.side))
}

fn namespace_handle(path: &Path) -> Result<Option<File>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot inspect owned namespace handle".into()),
    };
    // SAFETY: statfs is an output-only C structure and file owns this descriptor.
    let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut filesystem) } != 0 {
        return Err("cannot inspect namespace filesystem".into());
    }
    if i128::from(filesystem.f_type) == i128::from(libc::NSFS_MAGIC) {
        return Ok(Some(file));
    }
    let metadata = file
        .metadata()
        .map_err(|_| "cannot inspect namespace placeholder")?;
    if !metadata.is_file() || metadata.len() != 0 || metadata.uid() != 0 {
        return Err("namespace path has an unexpected owner or type".into());
    }
    Ok(None)
}

fn host_mount_view() -> Result<bool> {
    let read = |pid: &str, kind: &str| {
        std::fs::read_link(format!("/proc/{pid}/ns/{kind}"))
            .map_err(|_| "cannot inspect namespace boundary")
    };
    // Private user/network test environments must never enter the real host.
    Ok(read("self", "user")? == read("1", "user")?
        && read("self", "net")? == read("1", "net")?
        && read("self", "mnt")? != read("1", "mnt")?)
}

fn host_mount_command(program: &str, args: &[&str]) -> Result<()> {
    let mut command = vec![
        "--mount=/proc/1/ns/mnt",
        "--root=/proc/1/root",
        "--wd=/proc/1/root",
        "--",
        program,
    ];
    command.extend_from_slice(args);
    run_command("nsenter", &command)
        .map(|_| ())
        .map_err(|_| "cannot persist owned namespace in the host mount view".into())
}

fn retain_namespace(b: &NicBinding) -> Result<()> {
    if !host_mount_view()? {
        return Ok(());
    }
    let path = Path::new("/run/netns").join(namespace(b));
    let local = namespace_handle(&path)?.ok_or("owned namespace mount is absent")?;
    let canonical = Path::new("/proc/1/root").join(
        path.strip_prefix("/")
            .map_err(|_| "invalid namespace path")?,
    );
    if let Some(existing) = namespace_handle(&canonical)? {
        let a = local
            .metadata()
            .map_err(|_| "cannot inspect local namespace")?;
        let c = existing
            .metadata()
            .map_err(|_| "cannot inspect host namespace")?;
        if (a.dev(), a.ino()) != (c.dev(), c.ino()) {
            return Err(
                "host namespace pin belongs to a different namespace; both retained".into(),
            );
        }
        return Ok(());
    }
    // Stop host unmount propagation from removing the Agent's own handle.
    run_command(
        "mount",
        &[
            "--no-canonicalize",
            "--make-private",
            path.to_str().ok_or("invalid namespace path")?,
        ],
    )
    .map_err(|_| "cannot isolate owned namespace mount propagation")?;
    if !canonical.exists() {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&canonical)
            .map_err(|_| "cannot create owned host namespace placeholder")?;
    }
    let source = format!("/proc/{}/fd/{}", std::process::id(), local.as_raw_fd());
    host_mount_command(
        "mount",
        &[
            "--no-canonicalize",
            "--bind",
            &source,
            path.to_str().ok_or("invalid namespace path")?,
        ],
    )?;
    let actual = namespace_handle(&canonical)?.ok_or("host namespace pin was not installed")?;
    let a = local
        .metadata()
        .map_err(|_| "cannot inspect local namespace")?;
    let c = actual
        .metadata()
        .map_err(|_| "cannot inspect host namespace")?;
    if (a.dev(), a.ino()) != (c.dev(), c.ino()) {
        return Err("host namespace pin identity changed".into());
    }
    Ok(())
}

fn release_host_namespace(b: &NicBinding) -> Result<()> {
    if !host_mount_view()? {
        return Ok(());
    }
    let path = Path::new("/run/netns").join(namespace(b));
    let canonical = Path::new("/proc/1/root").join(
        path.strip_prefix("/")
            .map_err(|_| "invalid namespace path")?,
    );
    if let Some(global) = namespace_handle(&canonical)? {
        let local =
            namespace_handle(&path)?.ok_or("local namespace handle missing; host pin retained")?;
        let a = local
            .metadata()
            .map_err(|_| "cannot inspect local namespace")?;
        let c = global
            .metadata()
            .map_err(|_| "cannot inspect host namespace")?;
        if (a.dev(), a.ino()) != (c.dev(), c.ino()) {
            return Err("namespace cleanup identity differs; host pin retained".into());
        }
        drop(global);
        drop(local);
        run_command(
            "mount",
            &[
                "--no-canonicalize",
                "--make-private",
                path.to_str().ok_or("invalid namespace path")?,
            ],
        )
        .map_err(|_| "cannot isolate namespace cleanup")?;
        host_mount_command(
            "umount",
            &[
                "--no-canonicalize",
                path.to_str().ok_or("invalid namespace path")?,
            ],
        )?;
    }
    Ok(())
}

fn discard_stale_namespace(b: &NicBinding) -> Result<()> {
    let path = Path::new("/run/netns").join(namespace(b));
    if !path.exists() || namespace_handle(&path)?.is_some() {
        return Ok(());
    }
    let l = layout(b);
    if inspect_link(None, &l.root_link)?.is_some()
        || inspect_link(None, &format!("bl{}", digest(&b.interface_id)))?.is_some()
    {
        return Err(
            "namespace handle unavailable while owned links remain; resources retained".into(),
        );
    }
    std::fs::remove_file(path).map_err(|_| "cannot remove stale owned namespace placeholder".into())
}
fn root_ip(args: &[String]) -> Result<String> {
    run_command("ip", &args.iter().map(String::as_str).collect::<Vec<_>>())
        .map_err(|error| format!("NIC network operation: {error}"))
}
pub(super) fn ns_command(ns: &str, args: &[String]) -> Result<String> {
    let mut command = vec!["netns".into(), "exec".into(), ns.into()];
    command.extend_from_slice(args);
    root_ip(&command)
}
pub(super) fn ns_ip(ns: &str, args: &[String]) -> Result<String> {
    let mut command = vec!["-n".into(), ns.into()];
    command.extend_from_slice(args);
    root_ip(&command)
}
fn inspect_link(ns: Option<&str>, name: &str) -> Result<Option<serde_json::Value>> {
    let args = if let Some(ns) = ns {
        vec!["-n", ns, "-j", "-d", "link", "show"]
    } else {
        vec!["-j", "-d", "link", "show"]
    };
    let raw = run_command("ip", &args).map_err(|_| "cannot inspect owned NIC devices")?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "invalid NIC device inventory")?;
    Ok(value
        .as_array()
        .ok_or("invalid NIC link inventory")?
        .iter()
        .find(|link| link["ifname"].as_str() == Some(name))
        .cloned())
}
fn verify_link(link: &serde_json::Value, alias: &str, kind: &str) -> Result<()> {
    if link["ifalias"].as_str() != Some(alias)
        || link
            .pointer("/linkinfo/info_kind")
            .and_then(serde_json::Value::as_str)
            != Some(kind)
    {
        return Err(format!(
            "NIC device {} does not match expected owner {alias} and kind {kind}",
            link["ifname"].as_str().unwrap_or("unknown")
        ));
    }
    Ok(())
}
fn device_intent(
    root: &Path,
    ns: Option<&str>,
    name: &str,
    kind: &str,
    alias: &str,
    existing: bool,
) -> Result<()> {
    let identity = serde_json::json!({"namespace":ns,"name":name,"kind":kind,"owner":alias});
    let key = brocade_core::hash::sha256_hex(format!("{}:{name}", ns.unwrap_or("root")).as_bytes());
    let path = root.join("cvm-nics").join(format!("create-{key}.json"));
    if let Some(bytes) = read_bounded(&path, 8192)? {
        let saved: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "invalid NIC device creation evidence")?;
        if saved != identity {
            return Err("NIC device creation is owned by another assignment".into());
        }
    } else {
        if existing {
            return Err("unlabelled NIC device has no creation evidence".into());
        }
        atomic_write_private(
            &path,
            &serde_json::to_vec(&identity)
                .map_err(|_| "cannot encode NIC device creation intent")?,
        )?;
    }
    Ok(())
}
pub(super) fn ensure_ns_link(
    root: &Path,
    ns: &str,
    name: &str,
    kind: &str,
    alias: &str,
) -> Result<()> {
    if let Some(link) = inspect_link(Some(ns), name)? {
        if link["ifalias"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
        {
            return verify_link(&link, alias, kind);
        }
        device_intent(root, Some(ns), name, kind, alias, true)?;
        if link
            .pointer("/linkinfo/info_kind")
            .and_then(serde_json::Value::as_str)
            != Some(kind)
        {
            return Err("unlabelled NIC device has a different kind".into());
        }
    } else {
        device_intent(root, Some(ns), name, kind, alias, false)?;
        ns_ip(
            ns,
            &[
                "link".into(),
                "add".into(),
                "name".into(),
                name.into(),
                "type".into(),
                kind.into(),
            ],
        )?;
    }
    ns_ip(
        ns,
        &[
            "link".into(),
            "set".into(),
            "dev".into(),
            name.into(),
            "alias".into(),
            alias.into(),
        ],
    )?;
    Ok(())
}
pub(super) fn remove_ns_link(ns: &str, name: &str, alias: &str) -> Result<()> {
    if !Path::new("/run/netns").join(ns).exists() {
        return Ok(());
    }
    if let Some(link) = inspect_link(Some(ns), name)? {
        if link["ifalias"].as_str() != Some(alias) {
            return Err("refusing to remove an unowned NIC tunnel device".into());
        }
        ns_ip(
            ns,
            &["link".into(), "delete".into(), "dev".into(), name.into()],
        )?;
    }
    Ok(())
}
fn ensure_veth(root: &Path, b: &NicBinding, root_name: &str, peer: &str) -> Result<()> {
    let ns = namespace(b);
    let alias = owner(b);
    if let Some(link) = inspect_link(None, root_name)? {
        if link["ifalias"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
        {
            verify_link(&link, &alias, "veth")?;
        } else {
            device_intent(root, None, root_name, "veth", &alias, true)?;
            let counterpart =
                inspect_link(Some(&ns), peer)?.ok_or("interrupted NIC peer is missing")?;
            if link["link_index"] != counterpart["ifindex"]
                || link["ifindex"] != counterpart["link_index"]
            {
                return Err("interrupted NIC device has a different peer".into());
            }
        }
    } else {
        device_intent(root, None, root_name, "veth", &alias, false)?;
        root_ip(&[
            "link".into(),
            "add".into(),
            "name".into(),
            root_name.into(),
            "alias".into(),
            alias.clone(),
            "type".into(),
            "veth".into(),
            "peer".into(),
            "name".into(),
            peer.into(),
            "netns".into(),
            ns.clone(),
        ])?;
    }
    root_ip(&[
        "link".into(),
        "set".into(),
        "dev".into(),
        root_name.into(),
        "alias".into(),
        alias.clone(),
    ])?;
    let peer_link = inspect_link(Some(&ns), peer)?.ok_or("NIC namespace peer is missing")?;
    if peer_link["ifalias"]
        .as_str()
        .is_some_and(|found| !found.is_empty() && found != alias)
    {
        return Err("NIC namespace peer belongs to a different owner".into());
    }
    ns_ip(
        &ns,
        &[
            "link".into(),
            "set".into(),
            "dev".into(),
            peer.into(),
            "alias".into(),
            alias,
        ],
    )?;
    root_ip(&[
        "link".into(),
        "set".into(),
        "dev".into(),
        root_name.into(),
        "up".into(),
    ])?;
    ns_ip(
        &ns,
        &[
            "link".into(),
            "set".into(),
            "dev".into(),
            peer.into(),
            "up".into(),
        ],
    )?;
    Ok(())
}
pub(super) fn verify_anchor(root: &Path, b: &NicBinding) -> Result<()> {
    let bytes =
        read_bounded(&anchor_path(root, b), 8192)?.ok_or("NIC ownership anchor is absent")?;
    let saved: Layout =
        serde_json::from_slice(&bytes).map_err(|_| "invalid NIC ownership anchor")?;
    let l = layout(b);
    if saved.namespace != l.namespace
        || saved.root_link != l.root_link
        || saved.root_v4 != l.root_v4
    {
        return Err("NIC ownership anchor changed".into());
    }
    let link = inspect_link(None, &l.root_link)?.ok_or("NIC host link is absent")?;
    verify_link(&link, &owner(b), "veth")
}
pub(super) fn external_interface(name: &str) -> Result<bool> {
    Ok(name != "lo"
        && inspect_link(None, name)?.is_some_and(|link| {
            !link["ifalias"]
                .as_str()
                .is_some_and(|alias| alias.starts_with("brocade-nic:"))
        }))
}
pub(crate) fn enable_forwarding() -> Result<()> {
    let read = |key: &str| {
        run_command("sysctl", &["-n", key])
            .map(|value| value.trim().to_owned())
            .map_err(|_| "cannot inspect host forwarding policy")
    };
    let ipv4 = read("net.ipv4.ip_forward")?;
    let redirects = read("net.ipv4.conf.all.accept_redirects")?;
    let ipv6 = read("net.ipv6.conf.all.forwarding")?;
    if ipv6 == "0" {
        // Keep the host's existing RA-based address/default-route acquisition
        // when enabling its explicit network-provider role.
        let links: Vec<serde_json::Value> = serde_json::from_str(
            &run_command("ip", &["-j", "link", "show"])
                .map_err(|_| "cannot inspect host RA interfaces")?,
        )
        .map_err(|_| "invalid host RA interface inventory")?;
        for link in links {
            let name = link["ifname"]
                .as_str()
                .ok_or("host interface identity unavailable")?;
            if name == "lo"
                || link["ifalias"]
                    .as_str()
                    .is_some_and(|alias| alias.starts_with("brocade-nic:"))
            {
                continue;
            }
            if name.len() > 15
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
            {
                return Err("invalid host interface identity".into());
            }
            let key = format!("net/ipv6/conf/{name}/accept_ra");
            if read(&key)? == "1" {
                run_command("sysctl", &["-qw", &format!("{key}=2")])
                    .map_err(|_| "cannot preserve host IPv6 RA acquisition")?;
            }
        }
        run_command("sysctl", &["-qw", "net.ipv6.conf.all.forwarding=1"])
            .map_err(|_| "cannot enable NIC IPv6 forwarding")?;
    }
    if ipv4 == "0" {
        run_command(
            "sysctl",
            &[
                "-qw",
                "net.ipv4.ip_forward=1",
                &format!("net.ipv4.conf.all.accept_redirects={redirects}"),
            ],
        )
        .map_err(|_| "cannot enable NIC IPv4 forwarding while preserving host policy")?;
    }
    Ok(())
}
pub(super) fn prepare(root: &Path, b: &NicBinding) -> Result<()> {
    let l = layout(b);
    let marker = anchor_path(root, b);
    let previous = read_bounded(&marker, 8192)?;
    if let Some(bytes) = previous.as_ref() {
        let saved: Layout =
            serde_json::from_slice(bytes).map_err(|_| "invalid NIC anchor ownership")?;
        if saved.namespace != l.namespace
            || saved.root_link != l.root_link
            || saved.root_v4 != l.root_v4
        {
            return Err("allocated NIC anchor identity changed".into());
        }
        discard_stale_namespace(b)?;
    }
    let exists = Path::new("/run/netns").join(&l.namespace).exists();
    if exists && previous.is_none() {
        return Err("NIC namespace already exists without ownership evidence".into());
    }
    atomic_write_private(
        &marker,
        &serde_json::to_vec(&l).map_err(|_| "cannot encode NIC anchor ownership")?,
    )?;
    if !exists {
        root_ip(&["netns".into(), "add".into(), l.namespace.clone()])?;
    }
    ensure_veth(root, b, &l.root_link, "uplink")?;
    retain_namespace(b)?;
    ns_ip(
        &l.namespace,
        &["link".into(), "set".into(), "lo".into(), "up".into()],
    )?;
    root_ip(&[
        "address".into(),
        "replace".into(),
        format!("{}/29", l.root_v4),
        "dev".into(),
        l.root_link.clone(),
    ])?;
    ns_ip(
        &l.namespace,
        &[
            "address".into(),
            "replace".into(),
            format!("{}/29", l.namespace_v4),
            "dev".into(),
            "uplink".into(),
        ],
    )?;
    ns_ip(
        &l.namespace,
        &[
            "address".into(),
            "replace".into(),
            format!("{}/32", l.business_v4),
            "dev".into(),
            "lo".into(),
        ],
    )?;
    for (ns, address, device) in [
        (None, l.root_v6, l.root_link.as_str()),
        (Some(l.namespace.as_str()), l.namespace_v6, "uplink"),
        (Some(l.namespace.as_str()), l.business_v6, "lo"),
    ] {
        let args = vec![
            "-6".into(),
            "address".into(),
            "replace".into(),
            format!("{address}/{}", if device == "lo" { 128 } else { 64 }),
            "dev".into(),
            device.into(),
            "nodad".into(),
        ];
        if let Some(ns) = ns {
            ns_ip(ns, &args)?;
        } else {
            root_ip(&args)?;
        }
    }
    for (family, business, gateway) in [
        ("-4", l.business_v4.to_string(), l.namespace_v4.to_string()),
        ("-6", l.business_v6.to_string(), l.namespace_v6.to_string()),
    ] {
        root_ip(&[
            family.into(),
            "route".into(),
            "replace".into(),
            format!("{business}/{}", if family == "-4" { 32 } else { 128 }),
            "via".into(),
            gateway,
            "dev".into(),
            l.root_link.clone(),
            "proto".into(),
            "186".into(),
        ])?;
    }
    ns_ip(
        &l.namespace,
        &[
            "route".into(),
            "replace".into(),
            "default".into(),
            "via".into(),
            l.root_v4.to_string(),
            "dev".into(),
            "uplink".into(),
            "proto".into(),
            "186".into(),
        ],
    )?;
    ns_ip(
        &l.namespace,
        &[
            "-6".into(),
            "route".into(),
            "replace".into(),
            "default".into(),
            "via".into(),
            l.root_v6.to_string(),
            "dev".into(),
            "uplink".into(),
            "proto".into(),
            "186".into(),
        ],
    )?;
    for setting in [
        "net.ipv4.ip_forward=1",
        "net.ipv4.conf.all.rp_filter=0",
        "net.ipv4.conf.default.rp_filter=0",
        "net.ipv4.conf.uplink.rp_filter=0",
        "net.ipv4.conf.uplink.accept_source_route=0",
        "net.ipv4.conf.all.accept_redirects=0",
        "net.ipv6.conf.uplink.accept_source_route=-1",
        "net.ipv4.conf.all.accept_source_route=0",
        "net.ipv6.conf.all.forwarding=1",
        "net.ipv6.conf.all.accept_ra=0",
        "net.ipv6.conf.all.accept_source_route=-1",
    ] {
        ns_command(
            &l.namespace,
            &["sysctl".into(), "-qw".into(), setting.into()],
        )?;
    }
    enable_forwarding()?;
    if b.side == BindingSide::Consumer {
        let alias = format!("brocade-nic:{}", b.interface_id);
        if let Some(link) = inspect_link(None, &l.bridge)? {
            if link["ifalias"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
            {
                verify_link(&link, &alias, "bridge")?;
            } else {
                device_intent(root, None, &l.bridge, "bridge", &alias, true)?;
            }
        } else {
            device_intent(root, None, &l.bridge, "bridge", &alias, false)?;
            root_ip(&[
                "link".into(),
                "add".into(),
                "name".into(),
                l.bridge.clone(),
                "type".into(),
                "bridge".into(),
            ])?;
        }
        root_ip(&[
            "link".into(),
            "set".into(),
            "dev".into(),
            l.bridge.clone(),
            "alias".into(),
            alias,
        ])?;
        root_ip(&[
            "link".into(),
            "set".into(),
            "dev".into(),
            l.bridge.clone(),
            "up".into(),
        ])?;
        let lan = format!("bl{}", digest(&b.interface_id));
        ensure_veth(root, b, &lan, "lan")?;
        root_ip(&[
            "link".into(),
            "set".into(),
            "dev".into(),
            lan,
            "master".into(),
            l.bridge.clone(),
        ])?;
        ns_command(
            &l.namespace,
            &[
                "sysctl".into(),
                "-qw".into(),
                "net.ipv4.conf.lan.rp_filter=0".into(),
                "net.ipv4.conf.lan.accept_source_route=0".into(),
                "net.ipv6.conf.lan.accept_source_route=-1".into(),
            ],
        )?;
        ns_ip(
            &l.namespace,
            &[
                "address".into(),
                "replace".into(),
                "169.254.128.1/32".into(),
                "dev".into(),
                "lan".into(),
            ],
        )?;
        ns_ip(
            &l.namespace,
            &[
                "-6".into(),
                "address".into(),
                "replace".into(),
                "fe80::1/64".into(),
                "dev".into(),
                "lan".into(),
                "nodad".into(),
            ],
        )?;
        for address in [b.nic.ipv4, b.nic.ipv6].into_iter().flatten() {
            ns_ip(
                &l.namespace,
                &[
                    if address.is_ipv4() {
                        "-4".into()
                    } else {
                        "-6".into()
                    },
                    "route".into(),
                    "replace".into(),
                    format!("{address}/{}", if address.is_ipv4() { 32 } else { 128 }),
                    "dev".into(),
                    "lan".into(),
                    "proto".into(),
                    "186".into(),
                ],
            )?;
        }
    }
    Ok(())
}

pub(super) fn nft_owned(ns: Option<&str>, table: &str, alias: &str) -> Result<bool> {
    let args = if let Some(ns) = ns {
        vec!["netns", "exec", ns, "nft", "-j", "list", "tables"]
    } else {
        vec![]
    };
    let raw = if args.is_empty() {
        run_command("nft", &["-j", "list", "tables"])
    } else {
        run_command("ip", &args)
    }
    .map_err(|_| "cannot inspect NIC firewall ownership")?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "invalid NIC firewall inventory")?;
    let found = value["nftables"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|entry| entry["table"]["family"] == "inet" && entry["table"]["name"] == table);
    if !found {
        return Ok(false);
    }
    let raw = if let Some(ns) = ns {
        ns_command(
            ns,
            &[
                "nft".into(),
                "-j".into(),
                "list".into(),
                "table".into(),
                "inet".into(),
                table.into(),
            ],
        )?
    } else {
        run_command("nft", &["-j", "list", "table", "inet", table])
            .map_err(|_| "cannot inspect NIC firewall")?
    };
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "invalid NIC firewall")?;
    if !value["nftables"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|entry| entry["table"]["comment"].as_str() == Some(alias))
    {
        return Err("NIC firewall belongs to a different owner".into());
    }
    Ok(true)
}
pub(super) fn nft_apply(
    root: &Path,
    ns: Option<&str>,
    table: &str,
    alias: &str,
    body: &str,
) -> Result<()> {
    let exists = nft_owned(ns, table, alias)?;
    let script = format!(
        "{}add table inet {table} {{ comment \"{alias}\"; }}\n{body}",
        if exists {
            format!("delete table inet {table}\n")
        } else {
            String::new()
        }
    );
    let path = root.join("cvm-nics").join(format!("firewall-{table}.nft"));
    atomic_write_private(&path, script.as_bytes())?;
    let path = path.to_str().ok_or("invalid NIC firewall path")?;
    if let Some(ns) = ns {
        ns_command(ns, &["nft".into(), "-f".into(), path.into()])?;
    } else {
        run_command("nft", &["-f", path])
            .map_err(|error| format!("NIC host firewall application failed: {error}"))?;
    }
    Ok(())
}
fn nft_header(table: &str, chain: &str, attributes: &str) -> String {
    format!("add chain inet {table} {chain} {{ {attributes} }}\n")
}
fn nft_rule(table: &str, chain: &str, rule: &str) -> String {
    format!("add rule inet {table} {chain} {rule}\n")
}
fn network_set(prefixes: &[IpPrefix], ipv4: bool) -> String {
    prefixes
        .iter()
        .filter(|p| p.is_ipv4() == ipv4)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
fn table(b: &NicBinding) -> String {
    format!("bcn{}{}", suffix(b.side), digest(&b.interface_id))
}

pub(super) fn prepare_transport(root: &Path, b: &NicBinding, desired: &NicDesired) -> Result<()> {
    let peers = desired
        .bindings
        .iter()
        .filter(|peer| {
            peer.interface_id == b.interface_id
                && peer.side == b.side
                && peer.phase != BindingPhase::Withdrawing
        })
        .collect::<Vec<_>>();
    let active = peers
        .iter()
        .copied()
        .find(|peer| peer.phase == BindingPhase::Active);
    configure_root(root, active.unwrap_or(b), &peers)
}
pub(super) fn configure(root: &Path, b: &NicBinding, desired: &NicDesired) -> Result<()> {
    let peers: Vec<&NicBinding> = desired
        .bindings
        .iter()
        .filter(|other| {
            other.interface_id == b.interface_id
                && other.side == b.side
                && other.phase != BindingPhase::Withdrawing
        })
        .collect();
    let active = peers
        .iter()
        .find(|other| other.phase == BindingPhase::Active)
        .copied();
    let b = active.unwrap_or(b);
    let l = layout(b);
    let ns = &l.namespace;
    let device = active.map(tunnel);
    let scope = table(b);
    let mut script = nft_header(
        &scope,
        "forward",
        "type filter hook forward priority -100; policy drop;",
    );
    script.push_str(&nft_header(
        &scope,
        "input",
        "type filter hook input priority -100; policy drop;",
    ));
    script.push_str(&nft_rule(&scope, "input", "iifname \"lo\" accept"));
    script.push_str(&nft_rule(
        &scope,
        "input",
        "ct state established,related accept",
    ));
    script.push_str(&nft_rule(
        &scope,
        "input",
        "meta l4proto ipv6-icmp icmpv6 type { nd-neighbor-solicit, nd-neighbor-advert } accept",
    ));
    for peer in &peers {
        let protocol = if peer.offer.kind == TransportKind::Wireguard {
            "udp"
        } else {
            "tcp"
        };
        if peer.offer.kind == TransportKind::Wireguard
            || peer.offer.listen_side.resolved()? == peer.side
        {
            script.push_str(&nft_rule(
                &scope,
                "input",
                &format!("iifname \"uplink\" {protocol} dport {} accept", peer.port),
            ));
        }
    }
    for v4 in [true, false] {
        let prefixes = network_set(&desired.management_networks, v4);
        if !prefixes.is_empty() {
            let family = if v4 { "ip" } else { "ip6" };
            let incoming = if b.side == BindingSide::Consumer {
                "lan".into()
            } else {
                device.clone().unwrap_or_default()
            };
            if !incoming.is_empty() {
                script.push_str(&nft_rule(&scope,"forward",&format!("iifname \"{incoming}\" ct direction original {family} daddr {{ {prefixes} }} drop")));
            }
        }
    }
    script.push_str(&nft_header(
        &scope,
        "prerouting",
        "type nat hook prerouting priority -100;",
    ));
    script.push_str(&nft_header(
        &scope,
        "postrouting",
        "type nat hook postrouting priority 100;",
    ));
    for address in [b.nic.ipv4, b.nic.ipv6].into_iter().flatten() {
        let family = if address.is_ipv4() { "ip" } else { "ip6" };
        if b.side == BindingSide::Consumer {
            script.push_str(&nft_rule(
                &scope,
                "forward",
                &format!("iifname \"lan\" {family} saddr != {address} drop"),
            ));
        }
        for peer in &peers {
            let dev = tunnel(peer);
            let selector = if b.side == BindingSide::Consumer {
                "daddr"
            } else {
                "saddr"
            };
            script.push_str(&nft_rule(
                &scope,
                "forward",
                &format!("iifname \"{dev}\" {family} {selector} != {address} drop"),
            ));
        }
    }
    if active.is_some_and(|active| !active.nic.is_blocked()) {
        if b.side == BindingSide::Provider {
            script.push_str(&nft_rule(
                &scope,
                "forward",
                "ct state established,related accept",
            ));
        } else {
            script.push_str(&nft_rule(
                &scope,
                "forward",
                "iifname \"lan\" ct direction reply ct state established,related accept",
            ));
            script.push_str(&nft_rule(
                &scope,
                "forward",
                "iifname != \"lan\" ct state established,related accept",
            ));
        }
        let active = active.ok_or("active NIC disappeared")?;
        let dev = tunnel(active);
        for v4 in [true, false] {
            let family = if v4 { "ip" } else { "ip6" };
            let address = if v4 { active.nic.ipv4 } else { active.nic.ipv6 };
            let Some(address) = address else { continue };
            let destinations = network_set(&active.nic.destinations, v4);
            if !destinations.is_empty() {
                if b.side == BindingSide::Provider {
                    script.push_str(&nft_rule(&scope,"forward",&format!("iifname \"{dev}\" {family} saddr {address} {family} daddr {{ {destinations} }} accept")));
                } else {
                    for rule in route_rules(&active.routing, &active.interface_id, v4) {
                        script.push_str(&nft_rule(&scope, "forward", &rule));
                    }
                }
            }
        }
    }
    if b.side == BindingSide::Consumer {
        if let Some(dev) = device.as_ref() {
            for address in [b.nic.ipv4, b.nic.ipv6].into_iter().flatten() {
                let family = if address.is_ipv4() { "ip" } else { "ip6" };
                let protocol = if address.is_ipv4() {
                    "icmp"
                } else {
                    "ipv6-icmp"
                };
                script.push_str(&nft_rule(&scope,"postrouting",&format!("oifname \"{dev}\" meta l4proto {protocol} {family} saddr != {address} snat {family} to {address}")));
            }
        }
    }
    if b.side == BindingSide::Provider && b.nic.address_mode != NicAddressMode::DedicatedRouted {
        for (address, business) in [
            (b.nic.ipv4, IpAddr::V4(l.business_v4)),
            (b.nic.ipv6, IpAddr::V6(l.business_v6)),
        ] {
            if let Some(address) = address {
                let family = if address.is_ipv4() { "ip" } else { "ip6" };
                script.push_str(&nft_rule(
                    &scope,
                    "prerouting",
                    &format!(
                        "iifname \"uplink\" {family} daddr {business} dnat {family} to {address}"
                    ),
                ));
                script.push_str(&nft_rule(
                    &scope,
                    "postrouting",
                    &format!(
                        "oifname \"uplink\" {family} saddr {address} snat {family} to {business}"
                    ),
                ));
            }
        }
    }
    nft_apply(root, Some(ns), &scope, &owner(b), &script)?;
    if b.side == BindingSide::Consumer {
        for address in [b.nic.ipv4, b.nic.ipv6].into_iter().flatten() {
            ns_ip(
                ns,
                &[
                    if address.is_ipv4() {
                        "-4".into()
                    } else {
                        "-6".into()
                    },
                    "route".into(),
                    "replace".into(),
                    format!("{address}/{}", if address.is_ipv4() { 32 } else { 128 }),
                    "dev".into(),
                    "lan".into(),
                    "proto".into(),
                    "186".into(),
                    "table".into(),
                    "100".into(),
                ],
            )?;
        }
        for family in ["-4", "-6"] {
            ns_ip(
                ns,
                &[
                    family.into(),
                    "route".into(),
                    "replace".into(),
                    "unreachable".into(),
                    "default".into(),
                    "metric".into(),
                    "42760".into(),
                    "proto".into(),
                    "186".into(),
                    "table".into(),
                    "100".into(),
                ],
            )?;
            let raw = ns_ip(
                ns,
                &[family.into(), "-j".into(), "rule".into(), "show".into()],
            )?;
            let rules: serde_json::Value =
                serde_json::from_str(&raw).map_err(|_| "invalid NIC namespace routing rules")?;
            if !rules.as_array().into_iter().flatten().any(|rule| {
                rule["priority"] == 1000
                    && rule["fwmark"]
                        .as_str()
                        .is_some_and(|mark| mark == "0xba" || mark == "186")
                    && (rule["table"].as_u64() == Some(100)
                        || rule["table"].as_str() == Some("100"))
            }) {
                ns_ip(
                    ns,
                    &[
                        family.into(),
                        "rule".into(),
                        "add".into(),
                        "pref".into(),
                        "1000".into(),
                        "not".into(),
                        "fwmark".into(),
                        "186".into(),
                        "lookup".into(),
                        "100".into(),
                    ],
                )?;
            }
            if let Some(dev) = device.as_ref() {
                ns_ip(
                    ns,
                    &[
                        family.into(),
                        "route".into(),
                        "replace".into(),
                        "default".into(),
                        "dev".into(),
                        dev.clone(),
                        "metric".into(),
                        "10".into(),
                        "proto".into(),
                        "186".into(),
                        "table".into(),
                        "100".into(),
                    ],
                )?;
            }
        }
    } else {
        for address in [b.nic.ipv4, b.nic.ipv6].into_iter().flatten() {
            let prefix = format!("{address}/{}", if address.is_ipv4() { 32 } else { 128 });
            let family = if address.is_ipv4() { "-4" } else { "-6" };
            ns_ip(
                ns,
                &[
                    family.into(),
                    "route".into(),
                    "replace".into(),
                    "unreachable".into(),
                    prefix.clone(),
                    "metric".into(),
                    "42760".into(),
                    "proto".into(),
                    "186".into(),
                ],
            )?;
            if let Some(dev) = device.as_ref() {
                ns_ip(
                    ns,
                    &[
                        family.into(),
                        "route".into(),
                        "replace".into(),
                        prefix,
                        "dev".into(),
                        dev.clone(),
                        "metric".into(),
                        "10".into(),
                        "proto".into(),
                        "186".into(),
                    ],
                )?;
            }
        }
    }
    super::meter::ensure(root, b)?;
    configure_root(root, b, &peers)
}

/// Consumer-side forward rules in route-selection order: a more specific prefix first,
/// port rules before whole-prefix rules at the same prefix, then NIC priority, then the
/// narrower port range. The first matching rule decides, exactly like
/// [`ContainerRoutes::select_flow`]; a flow selected for another NIC is dropped here.
pub(super) fn route_rules(routing: &ContainerRoutes, interface: &str, v4: bool) -> Vec<String> {
    let family = if v4 { "ip" } else { "ip6" };
    let mut ordered = routing
        .routes
        .iter()
        .filter(|route| route.destination.is_ipv4() == v4)
        .filter_map(|route| {
            let priority = routing
                .priorities
                .iter()
                .find(|p| p.interface_id == route.interface_id)
                .and_then(|p| if v4 { p.ipv4 } else { p.ipv6 })?;
            let width = route.service.map_or(u32::MAX, |s| {
                u32::from(s.port_end) - u32::from(s.port_start)
            });
            Some((
                std::cmp::Reverse(route.destination.length()),
                route.service.is_none(),
                priority,
                width,
                route,
            ))
        })
        .collect::<Vec<_>>();
    ordered.sort_by(|a, b| {
        (
            a.0,
            a.1,
            a.2,
            a.3,
            &a.4.destination,
            a.4.service,
            &a.4.interface_id,
        )
            .cmp(&(
                b.0,
                b.1,
                b.2,
                b.3,
                &b.4.destination,
                b.4.service,
                &b.4.interface_id,
            ))
    });
    ordered
        .into_iter()
        .map(|(_, _, _, _, route)| {
            let action = if route.interface_id == interface {
                "accept"
            } else {
                "drop"
            };
            let service = route.service.map_or(String::new(), |s| {
                format!(
                    " meta l4proto {} th dport {}-{}",
                    match s.protocol {
                        L4Protocol::Tcp => "tcp",
                        L4Protocol::Udp => "udp",
                    },
                    s.port_start,
                    s.port_end
                )
            });
            format!(
                "iifname \"lan\" {family} daddr {}{service} {action}",
                route.destination
            )
        })
        .collect()
}

fn configure_root(root: &Path, b: &NicBinding, peers: &[&NicBinding]) -> Result<()> {
    super::public_addresses::reconcile(root, b, peers)?;
    let l = layout(b);
    let scope = table(b);
    let mut script = String::new();
    script.push_str(&nft_header(
        &scope,
        "prerouting",
        "type nat hook prerouting priority -100;",
    ));
    script.push_str(&nft_header(
        &scope,
        "output",
        "type nat hook output priority -100;",
    ));
    script.push_str(&nft_header(
        &scope,
        "postrouting",
        "type nat hook postrouting priority 100;",
    ));
    script.push_str(&nft_header(
        &scope,
        "fence",
        "type filter hook prerouting priority -110; policy accept;",
    ));
    for family in ["ip", "ip6"] {
        let uplink = if family == "ip" {
            l.namespace_v4.to_string()
        } else {
            l.namespace_v6.to_string()
        };
        script.push_str(&nft_rule(
            &scope,
            "postrouting",
            &format!("{family} saddr {uplink} masquerade"),
        ));
    }
    for peer in peers
        .iter()
        .filter(|peer| peer.offer.listen_side.resolved().ok() == Some(b.side))
    {
        let protocol = if peer.nic.transport == TransportKind::Wireguard {
            "udp"
        } else {
            "tcp"
        };
        let family = if peer.offer.endpoint.is_ipv4() {
            "ip"
        } else {
            "ip6"
        };
        let target = if peer.offer.endpoint.is_ipv4() {
            l.namespace_v4.to_string()
        } else {
            format!("[{}]", l.namespace_v6)
        };
        for chain in ["prerouting", "output"] {
            script.push_str(&nft_rule(&scope,chain,&format!("meta nfproto {} fib daddr type local {protocol} dport {} dnat {family} to {target}:{}",if family=="ip"{"ipv4"}else{"ipv6"},peer.port,peer.port)));
        }
    }
    if b.side == BindingSide::Provider {
        for v4 in [true, false] {
            let public = if v4 {
                b.entry_source_ipv4
                    .filter(|_| b.nic.address_mode != NicAddressMode::DedicatedRouted)
                    .map(IpAddr::V4)
                    .or(b.nic.public_ipv4)
            } else {
                b.nic.public_ipv6
            };
            let family = if v4 { "ip" } else { "ip6" };
            let business = if v4 {
                IpAddr::V4(l.business_v4)
            } else {
                IpAddr::V6(l.business_v6)
            };
            let Some(public) = public else {
                if b.nic.address_mode == NicAddressMode::SharedNat
                    && (if v4 { b.nic.ipv4 } else { b.nic.ipv6 }).is_some()
                {
                    script.push_str(&nft_rule(
                        &scope,
                        "postrouting",
                        &format!("{family} saddr {business} masquerade"),
                    ));
                }
                continue;
            };
            if b.nic.address_mode != NicAddressMode::DedicatedRouted {
                script.push_str(&nft_rule(
                    &scope,
                    "postrouting",
                    &format!("{family} saddr {business} snat {family} to {public}"),
                ));
            }
        }
    }
    let checkpoint = root.join("cvm-nics").join(format!(
        "root-policy-{}-{}.json",
        b.interface_id,
        suffix(b.side)
    ));
    let digest = brocade_core::hash::sha256_hex(script.as_bytes());
    let previous = read_bounded(&checkpoint, 128)?;
    let changed = previous.as_deref() != Some(digest.as_bytes());
    let guard = format!("{scope}_guard");
    if changed {
        let mut block = nft_header(
            &guard,
            "forward",
            "type filter hook forward priority -120; policy accept;",
        );
        block.push_str(&nft_rule(
            &guard,
            "forward",
            &format!("iifname \"{}\" drop", l.root_link),
        ));
        block.push_str(&nft_rule(
            &guard,
            "forward",
            &format!("oifname \"{}\" drop", l.root_link),
        ));
        nft_apply(root, None, &guard, &owner(b), &block)?;
    }
    nft_apply(root, None, &scope, &owner(b), &script)?;
    if changed {
        super::flows::business(b)?;
    }
    let ports_path = root.join("cvm-nics").join(format!(
        "listener-{}-{}.json",
        b.interface_id,
        suffix(b.side)
    ));
    let saved: Vec<String> = read_bounded(&ports_path, 32768)?
        .map(|bytes| {
            serde_json::from_slice(&bytes).map_err(|_| "invalid NIC listener cleanup evidence")
        })
        .transpose()?
        .unwrap_or_default();
    let mut ports = Vec::new();
    for peer in peers
        .iter()
        .filter(|peer| peer.offer.listen_side.resolved().ok() == Some(peer.side))
    {
        let key = format!("{}:{}:{}", peer.id, peer.offer.endpoint, peer.port);
        if !saved.contains(&key) {
            super::flows::transport(peer)?;
        }
        ports.push(key);
    }
    atomic_write_private(
        &ports_path,
        &serde_json::to_vec(&ports).map_err(|_| "cannot save listener cleanup evidence")?,
    )?;
    if nft_owned(None, &guard, &owner(b))? {
        run_command("nft", &["delete", "table", "inet", &guard])
            .map_err(|_| "cannot remove converged NIC flow guard")?;
    }
    atomic_write_private(&checkpoint, digest.as_bytes())
}

pub(super) fn limits(b: &NicBinding) -> Result<()> {
    let rate = if b.side == BindingSide::Consumer {
        b.nic.limits.upload_bps
    } else {
        b.nic.limits.download_bps
    };
    if let Some(rate) = rate {
        ns_command(
            &namespace(b),
            &[
                "tc".into(),
                "qdisc".into(),
                "replace".into(),
                "dev".into(),
                tunnel(b),
                "root".into(),
                "tbf".into(),
                "rate".into(),
                format!("{rate}bit"),
                "burst".into(),
                "32kb".into(),
                "latency".into(),
                "100ms".into(),
            ],
        )?;
    } else {
        let ns = namespace(b);
        let raw = ns_command(
            &ns,
            &[
                "tc".into(),
                "-j".into(),
                "qdisc".into(),
                "show".into(),
                "dev".into(),
                tunnel(b),
            ],
        )?;
        let values: Vec<serde_json::Value> =
            serde_json::from_str(&raw).map_err(|_| "invalid NIC traffic shaping inventory")?;
        if values.iter().any(|entry| entry["kind"] == "tbf") {
            ns_command(
                &ns,
                &[
                    "tc".into(),
                    "qdisc".into(),
                    "del".into(),
                    "dev".into(),
                    tunnel(b),
                    "root".into(),
                ],
            )?;
        }
    }
    Ok(())
}
pub(super) fn counters(ns: &str, device: &str) -> Result<(u64, u64)> {
    let raw = ns_ip(
        ns,
        &[
            "-j".into(),
            "-s".into(),
            "link".into(),
            "show".into(),
            "dev".into(),
            device.into(),
        ],
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "invalid NIC transfer counters")?;
    Ok((
        value[0]["stats64"]["rx"]["bytes"]
            .as_u64()
            .ok_or("NIC receive counter is unavailable")?,
        value[0]["stats64"]["tx"]["bytes"]
            .as_u64()
            .ok_or("NIC send counter is unavailable")?,
    ))
}
pub(super) fn withdraw(root: &Path, b: &NicBinding, retain: bool) -> Result<()> {
    if let Some(bytes) = read_bounded(&anchor_path(root, b), 8192)? {
        let saved: Layout =
            serde_json::from_slice(&bytes).map_err(|_| "invalid NIC anchor ownership")?;
        let expected = layout(b);
        if saved.namespace != expected.namespace
            || saved.root_link != expected.root_link
            || saved.root_v4 != expected.root_v4
        {
            return Err("allocated NIC anchor identity changed".into());
        }
        discard_stale_namespace(b)?;
    }
    super::flows::transport(b)?;
    if retain {
        let desired = snapshot(root)?;
        let peers: Vec<&NicBinding> = desired
            .bindings
            .iter()
            .filter(|other| {
                other.interface_id == b.interface_id
                    && other.side == b.side
                    && other.phase != BindingPhase::Withdrawing
            })
            .collect();
        let active = peers
            .iter()
            .find(|peer| peer.phase == BindingPhase::Active)
            .copied();
        return configure_root(root, active.unwrap_or(b), &peers);
    }
    let l = layout(b);
    if read_bounded(&anchor_path(root, b), 8192)?.is_none() {
        if Path::new("/run/netns").join(&l.namespace).exists()
            || inspect_link(None, &l.root_link)?.is_some()
            || (b.side == BindingSide::Consumer && inspect_link(None, &l.bridge)?.is_some())
        {
            return Err("NIC cleanup lacks ownership evidence; allocation retained".into());
        }
        return configure_root(root, b, &[]);
    }
    configure_root(root, b, &[])?;
    super::flows::addresses([IpAddr::V4(l.namespace_v4), IpAddr::V6(l.namespace_v6)])?;
    if b.side == BindingSide::Consumer {
        if let Some(link) = inspect_link(None, &l.bridge)? {
            verify_link(&link, &format!("brocade-nic:{}", b.interface_id), "bridge")?;
            let raw = root_ip(&[
                "-j".into(),
                "link".into(),
                "show".into(),
                "master".into(),
                l.bridge.clone(),
            ])?;
            let entries: serde_json::Value =
                serde_json::from_str(&raw).map_err(|_| "cannot inspect NIC bridge members")?;
            let lan = format!("bl{}", digest(&b.interface_id));
            for entry in entries.as_array().ok_or("invalid NIC bridge inventory")? {
                if entry["ifname"].as_str() != Some(&lan)
                    || entry["ifalias"].as_str() != Some(&owner(b))
                {
                    return Err("等待容器实际网卡卸载，资源继续保留".into());
                }
            }
        }
    }
    for name in [&l.root_link, &format!("bl{}", digest(&b.interface_id))] {
        if let Some(link) = inspect_link(None, name)? {
            verify_link(&link, &owner(b), "veth")?;
            root_ip(&["link".into(), "delete".into(), "dev".into(), name.clone()])?;
        }
    }
    if Path::new("/run/netns").join(&l.namespace).exists() {
        release_host_namespace(b)?;
        root_ip(&["netns".into(), "delete".into(), l.namespace.clone()])?;
    }
    if b.side == BindingSide::Consumer && inspect_link(None, &l.bridge)?.is_some() {
        root_ip(&[
            "link".into(),
            "delete".into(),
            "dev".into(),
            l.bridge.clone(),
        ])?;
    }
    Ok(())
}
pub(super) fn forget(root: &Path, b: &NicBinding) -> Result<()> {
    let mut withdrawn = b.clone();
    withdrawn.phase = BindingPhase::Withdrawing;
    withdraw(root, &withdrawn, false)?;
    let scope = table(b);
    if nft_owned(None, &scope, &owner(b))? {
        run_command("nft", &["delete", "table", "inet", &scope])
            .map_err(|_| "cannot remove released NIC firewall")?;
    }
    let path = anchor_path(root, b);
    if path.exists() {
        std::fs::remove_file(path).map_err(|_| "cannot remove released NIC anchor evidence")?;
    }
    Ok(())
}

#[cfg(test)]
mod namespace_tests {
    use super::*;

    #[test]
    fn namespace_paths_reject_nonempty_and_symlink_placeholders() {
        let root = std::env::temp_dir().join(format!("brocade-ns-handle-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("handle");
        std::fs::write(&path, b"foreign file").unwrap();
        assert!(namespace_handle(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/proc/self/ns/net", &path).unwrap();
        assert!(namespace_handle(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"").unwrap();
        if std::fs::metadata(&path).unwrap().uid() == 0 {
            assert!(namespace_handle(&path).unwrap().is_none());
        } else {
            assert!(namespace_handle(&path).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
