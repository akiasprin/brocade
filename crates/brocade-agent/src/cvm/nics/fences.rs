//! Held dedicated addresses stay unreachable while their binding is not active, and
//! never accept new inbound connections: inbound access is the job of entries.
use super::*;
const TABLE: &str = "brocade_cvm_nic_fences";
pub(super) fn apply(root: &Path, desired: &NicDesired) -> Result<()> {
    let mut script=format!("add chain inet {TABLE} incoming {{ type filter hook prerouting priority -110; policy accept; }}\nadd chain inet {TABLE} local {{ type filter hook output priority -110; policy accept; }}\n");
    for fence in &desired.fences {
        // The Store only fences whole addresses; port ranges belonged to NIC inbound.
        if fence.protocol.is_some() {
            continue;
        }
        let active = desired.bindings.iter().any(|b| {
            b.interface_id == fence.interface_id
                && b.side == BindingSide::Provider
                && b.phase == BindingPhase::Active
                && !b.nic.is_blocked()
                && b.nic.address_mode != NicAddressMode::SharedNat
        });
        let family = if fence.address.is_ipv4() { "ip" } else { "ip6" };
        // An active dedicated address only carries replies to the container's own egress.
        let state = if active { "ct state new " } else { "" };
        for chain in ["incoming", "local"] {
            script.push_str(&format!(
                "add rule inet {TABLE} {chain} {family} daddr {} {state}drop\n",
                fence.address
            ));
        }
    }
    host::nft_apply(root, None, TABLE, "brocade-cvm-nics-v2", &script)
}
