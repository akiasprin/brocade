//! Withdraw NAT bindings as well as rules, retaining a fence across interruption.
use super::*;
use serde::{Deserialize, Serialize};

const TABLE: &str = "brocade_cvm_entry_withdraw";
const OWNER: &str = "brocade-entry-withdraw-v1";
const BATCH: usize = 128;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Scope {
    address: Ipv4Addr,
    target: Ipv4Addr,
    mode: EntryMode,
    #[serde(default, skip_serializing_if = "EntryReservedPorts::is_empty")]
    reserved_ports: EntryReservedPorts,
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Checkpoint {
    intent: Vec<EntryRule>,
    pending: Vec<Scope>,
    completed: usize,
}

fn path(root: &Path) -> PathBuf {
    root.join("cvm-entry-flow-cleanup.json")
}
pub(super) fn has_checkpoint(root: &Path) -> bool {
    path(root).exists()
}
fn read(root: &Path) -> Result<Option<Checkpoint>> {
    let Some(bytes) = read_bounded(&path(root), 16 * 1024 * 1024)? else {
        return Ok(None);
    };
    let state: Checkpoint =
        serde_json::from_slice(&bytes).map_err(|_| "invalid entry NAT cleanup checkpoint")?;
    let desired = EntryDesired {
        rules: state.intent.clone(),
        ..Default::default()
    };
    desired.validate_ports()?;
    if state.intent.len() > MAX_ENTRIES_PER_NODE || state.pending.len() > MAX_ENTRIES_PER_NODE * 4 {
        return Err("entry NAT cleanup checkpoint exceeds limits".into());
    }
    for scope in &state.pending {
        scope.reserved_ports.normalized()?;
        if !scope.mode.valid() {
            return Err("invalid entry NAT cleanup port interval".into());
        }
        if !entry_address(scope.address) || !entry_address(scope.target) {
            return Err("invalid entry NAT cleanup address".into());
        }
    }
    if state.completed > tasks(&state.pending)?.len() {
        return Err("invalid entry NAT cleanup cursor".into());
    }
    Ok(Some(state))
}
fn save(root: &Path, state: &Checkpoint) -> Result<()> {
    atomic_write_private(
        &path(root),
        &serde_json::to_vec(state).map_err(|_| "cannot encode entry NAT cleanup checkpoint")?,
    )
}

/// Bootstrap an existing Agent without dropping healthy, unchanged connections.
pub(super) fn seed(root: &Path, previous: &EntryDesired) -> Result<()> {
    if read(root)?.is_none() {
        save(
            root,
            &Checkpoint {
                intent: previous.rules.clone(),
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

fn changed(state: &mut Checkpoint, desired: &EntryDesired) {
    let mut added = false;
    for rule in state
        .intent
        .iter()
        .filter(|old| !desired.rules.contains(old))
        .chain(
            desired
                .rules
                .iter()
                .filter(|new| !state.intent.contains(new)),
        )
    {
        let Some(target) = rule.target else {
            continue;
        };
        let scope = Scope {
            address: rule.address,
            target,
            mode: rule.mode.clone(),
            reserved_ports: rule.reserved_ports.clone(),
        };
        if !state.pending.contains(&scope) {
            state.pending.push(scope);
            added = true;
        }
    }
    if added {
        state.completed = 0;
    }
    state.intent = desired.rules.clone();
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Task {
    Legacy(Vec<String>),
    Selected(usize),
}
fn base_filter(scope: &Scope) -> Vec<String> {
    vec![
        "-f".into(),
        "ipv4".into(),
        "-d".into(),
        scope.address.to_string(),
        "--dst-nat".into(),
        scope.target.to_string(),
    ]
}
fn tasks(scopes: &[Scope]) -> Result<Vec<Task>> {
    let mut result = BTreeSet::new();
    for (index, scope) in scopes.iter().enumerate() {
        if scope.mode.requires_protocol_rules(&scope.reserved_ports) {
            result.insert(Task::Selected(index));
            continue;
        }
        let base = vec![
            "-f".into(),
            "ipv4".into(),
            "-d".into(),
            scope.address.to_string(),
            "--dst-nat".into(),
            scope.target.to_string(),
        ];
        match scope.mode.ports() {
            None => {
                result.insert(Task::Legacy(base));
            }
            Some(ports) => {
                for protocol in ports.protocol.protocols() {
                    for port in ports.listen_start..=ports.listen_end {
                        let mut args = base.clone();
                        args.extend([
                            "-p".into(),
                            if protocol == L4Protocol::Tcp {
                                "tcp"
                            } else {
                                "udp"
                            }
                            .into(),
                            "--dport".into(),
                            port.to_string(),
                        ]);
                        result.insert(Task::Legacy(args));
                        if result.len() > MAX_ENTRY_PORTS_PER_NODE as usize * 4 {
                            return Err("entry NAT cleanup work exceeds limits".into());
                        }
                    }
                }
            }
        }
    }
    Ok(result.into_iter().collect())
}

/// The CLI accepts individual ports. Discover only ports with existing NAT bindings
/// instead of expanding an all-port policy into 131070 cleanup commands.
fn selected_work(scope: &Scope, raw: &[u8]) -> Result<Vec<Vec<String>>> {
    if raw.len() > 16 * 1024 * 1024 {
        return Err("entry NAT inventory exceeds limits; withdrawal fenced".into());
    }
    let text = std::str::from_utf8(raw).map_err(|_| "invalid entry NAT inventory encoding")?;
    let tcp = scope
        .mode
        .protocol_segments(L4Protocol::Tcp, &scope.reserved_ports)?;
    let udp = scope
        .mode
        .protocol_segments(L4Protocol::Udp, &scope.reserved_ports)?;
    let traffic = scope.mode.traffic();
    let mut selected = BTreeSet::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let protocol: u8 = line
            .split_whitespace()
            .nth(1)
            .ok_or("missing NAT inventory protocol")?
            .parse()
            .map_err(|_| "invalid NAT inventory protocol")?;
        let port = match protocol {
            6 | 17 => {
                let port: u16 = line
                    .split_whitespace()
                    .find_map(|f| f.strip_prefix("dport="))
                    .ok_or("missing NAT inventory original port")?
                    .parse()
                    .map_err(|_| "invalid NAT inventory original port")?;
                let segments = if protocol == 6 { &tcp } else { &udp };
                if !segments
                    .iter()
                    .any(|s| s.listen.start <= port && port <= s.listen.end)
                {
                    continue;
                }
                Some(port)
            }
            1 if traffic.icmp => None,
            1 => continue,
            _ if traffic.other_ip => None,
            _ => continue,
        };
        selected.insert((protocol, port));
        // Each subsequent round re-queries under the persistent fence. Keep only
        // enough work to prove that another bounded batch is needed.
        if selected.len() > BATCH {
            break;
        }
    }
    Ok(selected
        .into_iter()
        .map(|(protocol, port)| {
            let mut args = base_filter(scope);
            args.extend(["-p".into(), protocol.to_string()]);
            if let Some(port) = port {
                args.extend(["--dport".into(), port.to_string()]);
            }
            args
        })
        .collect())
}
fn selected_inventory(scope: &Scope) -> Result<Vec<Vec<String>>> {
    let args = base_filter(scope);
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    let output = capture_command_with_timeout(
        "conntrack",
        &[&["-L"][..], &args].concat(),
        Duration::from_secs(10),
    )?;
    if !output.status.success() {
        return Err("entry NAT inventory failed; withdrawal fenced".into());
    }
    selected_work(scope, &output.stdout)
}
fn remove(args: &[String]) -> Result<()> {
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    let deleted = capture_command_with_timeout(
        "conntrack",
        &[&["-D"][..], &args].concat(),
        Duration::from_secs(10),
    )?;
    if !deleted.status.success() && deleted.status.code() != Some(1) {
        return Err("entry NAT connection removal failed; withdrawal fenced".into());
    }
    let remaining = capture_command_with_timeout(
        "conntrack",
        &[&["-L"][..], &args].concat(),
        Duration::from_secs(10),
    )?;
    if !remaining.status.success() || !remaining.stdout.is_empty() {
        return Err("entry NAT connection cleanup unconfirmed; withdrawal fenced".into());
    }
    Ok(())
}
fn guard(state: &Checkpoint, ports: &BTreeSet<(L4Protocol, u16)>) -> Result<String> {
    let mut script = format!("add table ip {TABLE}\ndelete table ip {TABLE}\nadd table ip {TABLE} {{ comment \"{OWNER}\"; }}\nadd chain ip {TABLE} block {{ type filter hook prerouting priority -115; policy accept; }}\n");
    for (protocol, port) in ports {
        let protocol = if *protocol == L4Protocol::Tcp {
            "tcp"
        } else {
            "udp"
        };
        script.push_str(&format!(
            "add rule ip {TABLE} block {protocol} dport {port} return\n"
        ));
    }
    for scope in &state.pending {
        for packet in plan::packet_selectors(scope.address, &scope.mode, &scope.reserved_ports)? {
            script.push_str(&format!(
                "add rule ip {TABLE} block {packet} ct direction original drop\n"
            ));
        }
    }
    Ok(script)
}

fn owned() -> Result<bool> {
    let raw = run_command("nft", &["-j", "list", "tables"])?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "invalid entry NAT guard inventory")?;
    let Some(_) = value["nftables"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| &v["table"])
        .find(|v| v["family"] == "ip" && v["name"] == TABLE)
    else {
        return Ok(false);
    };
    let raw = run_command("nft", &["-j", "list", "table", "ip", TABLE])?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "invalid entry NAT guard ownership")?;
    if !value["nftables"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|row| row["table"]["comment"] == OWNER)
    {
        return Err("entry NAT guard belongs to another owner".into());
    }
    Ok(true)
}

pub(super) fn begin(
    root: &Path,
    desired: &EntryDesired,
    ports: &BTreeSet<(L4Protocol, u16)>,
) -> Result<Checkpoint> {
    let mut state = read(root)?.unwrap_or_default();
    changed(&mut state, desired);
    if state.pending.is_empty() {
        return Ok(state);
    }
    if state.pending.len() > MAX_ENTRIES_PER_NODE * 4 {
        return Err("pending entry NAT cleanup exceeds limits".into());
    }
    let work = tasks(&state.pending)?;
    save(root, &state)?;
    owned()?;
    crate::command::run_command_with_input("nft", &["-f", "-"], guard(&state, ports)?.as_bytes())?;
    let mut budget = BATCH;
    while budget > 0 && state.completed < work.len() {
        match &work[state.completed] {
            Task::Legacy(args) => {
                remove(args)?;
                budget -= 1;
                state.completed += 1;
            }
            Task::Selected(index) => {
                budget -= 1;
                let scope = &state.pending[*index];
                let candidates = selected_inventory(scope)?;
                let count = candidates.len().min(budget);
                for args in candidates.iter().take(count) {
                    remove(args)?;
                    budget -= 1;
                }
                if count < candidates.len() || !selected_inventory(scope)?.is_empty() {
                    break;
                }
                state.completed += 1;
            }
        }
    }
    save(root, &state)?;
    if state.completed != work.len() {
        return Err("entry NAT cleanup continues next round; withdrawal fenced".into());
    }
    Ok(state)
}

pub(super) fn finish(root: &Path, mut state: Checkpoint) -> Result<()> {
    state.pending.clear();
    state.completed = 0;
    save(root, &state)?;
    if owned()? {
        run_command("nft", &["delete", "table", "ip", TABLE])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_filters_only_original_entry_ports_and_nat_connections() {
        let scope = Scope {
            reserved_ports: Default::default(),
            address: "203.0.113.10".parse().unwrap(),
            target: "192.0.2.10".parse().unwrap(),
            mode: EntryMode::L4Range {
                protocol: EntryProtocol::TcpUdp,
                listen_start: 30000,
                listen_end: 30001,
                target_start: 80,
                target_end: 81,
            },
        };
        let work = tasks(&[scope.clone(), scope]).unwrap();
        assert_eq!(work.len(), 4);
        assert!(work
            .iter()
            .all(|task| matches!(task,Task::Legacy(args) if args.contains(&"--dst-nat".into()) && !args.contains(&"80".into()))));
    }

    #[test]
    fn pending_cleanup_survives_a_changed_intent_and_replay() {
        let mut state = Checkpoint::default();
        let desired = EntryDesired {
            rules: vec![EntryRule {
                reserved_ports: Default::default(),
                id: "entry".into(),
                address: "203.0.113.10".parse().unwrap(),
                mode: EntryMode::L3,
                target: Some("192.0.2.10".parse().unwrap()),
                tunnel: None,
                host_local: false,
            }],
            ..Default::default()
        };
        changed(&mut state, &desired);
        state.completed = 1;
        changed(&mut state, &EntryDesired::default());
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.completed, 1);
        changed(&mut state, &EntryDesired::default());
        assert_eq!(state.pending.len(), 1);
        let script = guard(&state, &BTreeSet::from([(L4Protocol::Tcp, 51820)])).unwrap();
        assert!(
            script.find("tcp dport 51820 return").unwrap()
                < script.find("ct direction original drop").unwrap()
        );
    }
    #[test]
    fn protocol_cleanup_preserves_reserved_ports_and_uses_original_mapping_ports() {
        let scope = Scope {
            address: "203.0.113.10".parse().unwrap(),
            target: "192.0.2.10".parse().unwrap(),
            mode: EntryMode::Protocols {
                traffic: EntryTraffic {
                    tcp: EntryTransportSelection::Forward {
                        ports: EntryPortSet::All,
                        mappings: vec![EntryPortMapping {
                            listen: EntryPortRange {
                                start: 30022,
                                end: 30022,
                            },
                            target: EntryPortRange { start: 22, end: 22 },
                        }],
                    },
                    udp: EntryTransportSelection::Disabled,
                    icmp: true,
                    other_ip: false,
                },
            },
            reserved_ports: EntryReservedPorts {
                tcp: vec![EntryPortRange { start: 22, end: 22 }],
                udp: vec![],
            },
        };
        let work = tasks(std::slice::from_ref(&scope)).unwrap();
        assert_eq!(work.len(), 1);
        assert!(matches!(work[0], Task::Selected(0)));
        let raw=b"tcp 6 10 ESTABLISHED src=198.51.100.1 dst=203.0.113.10 sport=9000 dport=22 src=192.0.2.10 dst=198.51.100.1 sport=22 dport=9000\ntcp 6 10 ESTABLISHED src=198.51.100.1 dst=203.0.113.10 sport=9001 dport=30022 src=192.0.2.10 dst=198.51.100.1 sport=22 dport=9001\nudp 17 10 src=198.51.100.1 dst=203.0.113.10 sport=9002 dport=30022\nicmp 1 10 src=198.51.100.1 dst=203.0.113.10 type=8 code=0 id=10\ngre 47 10 src=198.51.100.1 dst=203.0.113.10\n";
        let selected = selected_work(&scope, raw).unwrap();
        assert_eq!(selected.len(), 2);
        assert!(selected
            .iter()
            .any(|args| args.ends_with(&["--dport".into(), "30022".into()])));
        assert!(!selected
            .iter()
            .any(|args| args.ends_with(&["--dport".into(), "22".into()])));
        let script = guard(
            &Checkpoint {
                pending: vec![scope],
                ..Default::default()
            },
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(!script.contains("tcp dport 22 "));
        assert!(!script.contains("udp dport"));
        assert!(script.contains("ip protocol icmp"));
        assert!(script.lines().count() < 20);
    }
    #[test]
    fn protocol_inventory_batches_only_existing_distinct_ports() {
        let scope = Scope {
            address: "203.0.113.10".parse().unwrap(),
            target: "192.0.2.10".parse().unwrap(),
            mode: EntryMode::Protocols {
                traffic: EntryTraffic::all(),
            },
            reserved_ports: Default::default(),
        };
        let raw = (1..=1000)
            .map(|p| format!("tcp 6 10 src=198.51.100.1 dst=203.0.113.10 sport=9000 dport={p}\n"))
            .collect::<String>();
        assert_eq!(
            selected_work(&scope, raw.as_bytes()).unwrap().len(),
            BATCH + 1
        );
        assert!(selected_work(&scope, b"invalid inventory").is_err());
    }
}
