//! Business IP bytes are counted once at H, independently of transport replacement.
use super::*;
const TABLE: &str = "brocade_nic_meter";
pub(super) fn ensure(root: &Path, b: &NicBinding) -> Result<()> {
    if b.side != BindingSide::Consumer {
        return Ok(());
    }
    let ns = host::namespace(b);
    let owner = format!("brocade-nic-meter:{}", b.interface_id);
    let mut script = if host::nft_owned(Some(&ns), TABLE, &owner)? {
        format!("flush chain inet {TABLE} business\n")
    } else {
        format!("add table inet {TABLE} {{ comment \"{owner}\"; }}\nadd counter inet {TABLE} uploaded\nadd counter inet {TABLE} downloaded\nadd chain inet {TABLE} business {{ type filter hook forward priority 0; policy accept; }}\n")
    };
    for address in [b.nic.ipv4, b.nic.ipv6].into_iter().flatten() {
        let family = if address.is_ipv4() { "ip" } else { "ip6" };
        script.push_str(&format!("add rule inet {TABLE} business iifname \"lan\" {family} saddr {address} counter name uploaded\nadd rule inet {TABLE} business oifname \"lan\" {family} daddr {address} counter name downloaded\n"));
    }
    let path = root
        .join("cvm-nics")
        .join(format!("meter-{}.nft", b.interface_id));
    atomic_write_private(&path, script.as_bytes())?;
    host::ns_command(
        &ns,
        &[
            "nft".into(),
            "-f".into(),
            path.to_str().ok_or("invalid NIC meter path")?.into(),
        ],
    )?;
    Ok(())
}
pub(super) fn read(b: &NicBinding) -> Result<NicTrafficSample> {
    if b.side != BindingSide::Consumer {
        return Err("provider does not duplicate NIC usage".into());
    }
    let ns = host::namespace(b);
    let raw = host::ns_command(
        &ns,
        &[
            "nft".into(),
            "-j".into(),
            "list".into(),
            "counters".into(),
            "inet".into(),
            TABLE.into(),
        ],
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "invalid NIC byte counters")?;
    let bytes = |name: &str| {
        value["nftables"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["counter"]["name"] == name)
            .and_then(|entry| entry["counter"]["bytes"].as_u64())
            .ok_or("NIC byte counter unavailable")
    };
    let inode = std::fs::metadata(Path::new("/run/netns").join(ns))
        .map_err(|_| "NIC counter namespace disappeared")?
        .ino();
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|_| "NIC counter boot identity unavailable")?;
    Ok(NicTrafficSample {
        epoch: brocade_core::hash::sha256_hex(
            format!("{}:{inode}:{}", boot.trim(), b.interface_id).as_bytes(),
        ),
        observed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "invalid NIC meter clock")?
            .as_millis()
            .try_into()
            .map_err(|_| "NIC meter clock overflow")?,
        uploaded_bytes: bytes("uploaded")?,
        downloaded_bytes: bytes("downloaded")?,
    })
}

#[derive(Default, Serialize, Deserialize)]
struct LocalUsage {
    period_start: u64,
    used: u64,
    last: Option<NicTrafficSample>,
}
fn delta(current: &NicTrafficSample, previous: Option<&NicTrafficSample>) -> u64 {
    let bytes = current
        .uploaded_bytes
        .saturating_add(current.downloaded_bytes);
    match previous.filter(|previous| previous.epoch == current.epoch) {
        Some(previous) => bytes.saturating_sub(
            previous
                .uploaded_bytes
                .saturating_add(previous.downloaded_bytes),
        ),
        None => bytes,
    }
}
fn local_usage(root: &Path, b: &NicBinding, budget: &NicTrafficBudget, period: u64) -> Result<u64> {
    let path = root
        .join("cvm-nics")
        .join(format!("quota-{}.json", b.interface_id));
    let mut saved: LocalUsage = read_bounded(&path, 8192)?
        .map(|bytes| {
            serde_json::from_slice(&bytes).map_err(|_| "invalid local NIC quota checkpoint")
        })
        .transpose()?
        .unwrap_or(LocalUsage {
            period_start: period,
            used: if budget.period_start == period {
                budget.used_bytes
            } else {
                0
            },
            last: budget.sample.clone(),
        });
    let sample = if Path::new("/run/netns").join(host::namespace(b)).exists() {
        Some(read(b)?)
    } else {
        None
    };
    if saved.period_start != period {
        saved.period_start = period;
        saved.used = 0;
    }
    let mut used = saved.used;
    if let Some(current) = &sample {
        used = used.saturating_add(delta(current, saved.last.as_ref()));
        if budget.period_start == period {
            used = used.max(
                budget
                    .used_bytes
                    .saturating_add(delta(current, budget.sample.as_ref())),
            );
        }
        saved.last = Some(current.clone());
    } else if budget.period_start == period {
        used = used.max(budget.used_bytes);
        if saved.last.is_none() {
            saved.last = budget.sample.clone();
        }
    }
    saved.used = used;
    atomic_write_private(
        &path,
        &serde_json::to_vec(&saved).map_err(|_| "cannot encode local NIC quota checkpoint")?,
    )?;
    Ok(used)
}
/// The data plane keeps enforcing its last assigned limit during a control outage.
/// Providers can clear last month's block independently; H is the single meter.
pub(super) fn effective(
    root: &Path,
    desired: &NicDesired,
) -> (NicDesired, std::collections::BTreeMap<String, String>) {
    let mut result = desired.clone();
    let mut reasons = std::collections::BTreeMap::new();
    let mut decisions = std::collections::BTreeMap::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_secs() as i64);
    let period = now
        .ok()
        .map(|now| brocade_deployment::traffic_limit::utc_month_period(now).0 as u64 * 1000);
    for binding in desired
        .bindings
        .iter()
        .filter(|b| b.phase != BindingPhase::Withdrawing && b.nic.limits.monthly_bytes.is_some())
    {
        let key = (
            binding.interface_id.clone(),
            binding.side == BindingSide::Provider,
        );
        if decisions.contains_key(&key) {
            continue;
        }
        let decision: Result<bool> = (|| {
            let period = period.ok_or("NIC quota clock unavailable")?;
            let budget = desired
                .budgets
                .get(&binding.interface_id)
                .ok_or("NIC quota allocation anchor unavailable")?;
            if binding.side == BindingSide::Provider {
                return Ok(budget.period_start == period && binding.nic.quota_exceeded);
            }
            let active = desired
                .bindings
                .iter()
                .find(|b| {
                    b.interface_id == binding.interface_id
                        && b.side == BindingSide::Consumer
                        && b.phase == BindingPhase::Active
                })
                .unwrap_or(binding);
            let used = local_usage(root, active, budget, period)?;
            Ok(used
                >= active
                    .nic
                    .limits
                    .monthly_bytes
                    .ok_or("NIC monthly quota disappeared")?)
        })();
        let blocked = match decision {
            Ok(blocked) => {
                if blocked {
                    reasons.insert(binding.interface_id.clone(), "本月流量额度已用尽".into());
                }
                blocked
            }
            Err(error) => {
                reasons.insert(
                    binding.interface_id.clone(),
                    format!("计量暂不可用，业务已阻断：{error}"),
                );
                true
            }
        };
        decisions.insert(key, blocked);
    }
    for binding in &mut result.bindings {
        if let Some(blocked) = decisions.get(&(
            binding.interface_id.clone(),
            binding.side == BindingSide::Provider,
        )) {
            binding.nic.quota_exceeded = *blocked;
        }
    }
    (result, reasons)
}
