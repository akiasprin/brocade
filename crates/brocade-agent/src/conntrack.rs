//! Keeping xray's inbound TCP out of the connection tracking table.
//!
//! Every flow entering a machine is tracked from the moment any nat table exists, and a VLESS
//! client without mux opens one TCP connection per proxied connection — a minute of ordinary
//! browsing is hundreds of them. Worse, closing one does not release its entry: it sits in
//! TIME_WAIT for `nf_conntrack_tcp_timeout_time_wait`, two minutes by default. What the table
//! holds is therefore not the concurrent connections but every connection of the last two
//! minutes, and on a small machine (the kernel sizes the table from RAM — 431MB yields 3584
//! entries) that is what fills it. A full table drops *new* flows while established ones keep
//! working, so the symptom is "it stopped taking connections", never a clean failure.
//!
//! # Why only the ingress ports, and only inbound
//!
//! `notrack` is legal on a raw-priority chain and exempts exactly the packets a rule matches, so
//! the match is the whole design. Two properties make this pair the safe one:
//!
//! - **The first packet of an inbound connection is a SYN, which is never `established`.** A
//!   machine that accepts users at all therefore already has an explicit ACCEPT for that port in
//!   whatever firewall it runs; untracked packets hit that same rule. Traffic gated only by
//!   `ct state established,related accept` — which is what ufw and firewalld do for return
//!   traffic — is never what arrives here.
//! - **Nothing here touches outbound connections this machine originates.** The agent reaches the
//!   control plane over one of those, so even a rule that turns out to be wrong leaves the channel
//!   a fix travels over intact. That is the whole reason the outbound half is not attempted: it
//!   cannot be matched precisely (netfilter sees packets, not "xray's proxying"), and getting it
//!   wrong severs the machine from the control plane rather than degrading it.
//!
//! # Its own nft table
//!
//! `inet brocade_conntrack`, distinct from phantun's `inet brocade` and from
//! `brocade_hy2_port_hop`. Each of those is deleted and rebuilt wholesale on its own
//! convergence, so sharing one would have each feature silently delete the others' rules —
//! `hy2_port_hop.rs` documents the same reasoning and the same symptom.
//!
//! # What must not be matched
//!
//! - **UDP.** Hysteria 2's hop range is folded onto the listener by a `redirect`, which is NAT,
//!   and NAT is implemented *in* conntrack. Those flows are the ones that cannot be exempted, and
//!   they are also the ones that churn hardest. Nothing to do about it.
//! - **Loopback listens.** xray's gRPC api inbound sits on `127.0.0.1`, as do the XHTTP inbounds
//!   a public front hands off to. They cost one long-lived entry each, and exempting them would
//!   trade nothing for risk.
//! - **A port phantun DNATs.** raw (priority -300) runs before nat (-100), so a notracked packet
//!   never reaches the DNAT and the redirect silently stops happening. Ports are read from
//!   `xray.json`, and phantun's DNAT fronts wireguard rather than an xray inbound, so the two sets
//!   do not intersect — a machine where they did would already be broken, with phantun and xray
//!   claiming one port.

use std::{collections::BTreeSet, net::IpAddr};

use crate::{
    probe::{xray_listen_ports, XrayListenProtocol},
    run_shell, DesiredArtifact,
};

/// The nft table name. Distinct from phantun's and from port hopping's — see the module docs.
const TABLE: &str = "brocade_conntrack";

/// Apply for this desired xray artifact, or take the table down when there is no xray.
pub(crate) fn apply(desired: &DesiredArtifact) -> Result<(), String> {
    match desired {
        DesiredArtifact::Present { content, .. } => apply_content(content),
        DesiredArtifact::Disabled { .. } => disable(),
        // Nothing was asked of us. An artifact this agent does not manage must be left exactly as
        // it is, including a table somebody else put there.
        DesiredArtifact::Unmanaged { .. } => Ok(()),
    }
}

/// Apply from the xray config text alone. The local reconcile has only the file on disk.
pub(crate) fn apply_content(content: &str) -> Result<(), String> {
    let ports = ports(content);
    if ports.is_empty() {
        // An xray with no public TCP inbound — hysteria only, or api-only. Tearing down is the
        // same end state as having never installed, and leaves nothing behind either way.
        return disable();
    }
    install(&ports)
}

/// Whether the machine already matches this xray config.
pub(crate) fn matches_content(content: &str) -> bool {
    installed() == ports(content)
}

/// The ports to exempt: every public TCP inbound in `xray.json`.
///
/// A `BTreeSet` so the result is deduplicated and ordered, which is what makes comparing it
/// against what the machine reports a plain equality rather than a sort at each call site.
fn ports(content: &str) -> BTreeSet<u16> {
    xray_listen_ports(content)
        .into_iter()
        .filter(|(_, listen, _, protocol)| {
            *protocol == XrayListenProtocol::Tcp && !is_loopback(listen)
        })
        .map(|(_, _, port, _)| port)
        .collect()
}

/// Whether an inbound's `listen` keeps it on this machine.
///
/// An unparseable value counts as public. xray defaults the field to `0.0.0.0` when it is absent
/// and `xray_listen_ports` reproduces that, so the remaining unparseable cases are a hostname or
/// something malformed — and treating those as loopback would silently skip a port that is in
/// fact reachable, which is the failure that is invisible.
fn is_loopback(listen: &str) -> bool {
    listen
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// The rule lines, split out so a test can read them without a machine to run them on.
///
/// Two per port, and both are needed. Untracked packets have no flow for the reverse direction to
/// be recognised by, so the reply has to be matched on its own: `dport` on the way in, `sport` on
/// the way back out. With only the first, xray's replies would build the conntrack entry the
/// first rule declined to create, and the exemption would save nothing — the same trap the ICMP
/// rules in `hy2_port_hop.rs` document.
fn rule_lines(ports: &BTreeSet<u16>) -> Vec<String> {
    ports
        .iter()
        .flat_map(|port| {
            [
                format!("add rule inet {TABLE} prerouting_raw tcp dport {port} notrack"),
                format!("add rule inet {TABLE} output_raw tcp sport {port} notrack"),
            ]
        })
        .collect()
}

fn install(ports: &BTreeSet<u16>) -> Result<(), String> {
    // A real newline. `"\\n"` here would be a backslash and an `n`, joining two rules into one
    // malformed line inside the heredoc — and only ever with two ports on one machine, so a test
    // covering a single port passes and the fleet's common case never sees it.
    let rules = rule_lines(ports).join("\n");
    // Table and rules go to one `nft -f -` together. Outside the heredoc the shell would try to
    // run `add rule ...` as a command and report `add: not found`, the trap both other installers
    // document. `notrack` is legal only on a raw-priority chain, and a chain on the same hook
    // needs its own name, hence the `_raw` suffixes.
    run_shell(&format!(
        "set -eu\n\
         command -v nft >/dev/null 2>&1 || {{ echo '需要 nft（nftables）才能免除连接跟踪' >&2; exit 1; }}\n\
         nft delete table inet {TABLE} 2>/dev/null || true\n\
         nft -f - <<'NFT'\n\
table inet {TABLE} {{\n\
  chain prerouting_raw {{ type filter hook prerouting priority raw; }}\n\
  chain output_raw {{ type filter hook output priority raw; }}\n\
}}\n\
{rules}\n\
NFT\n"
    ))
    .map(|_| ())
}

pub(crate) fn disable() -> Result<(), String> {
    // Absent table is success: this runs on every convergence of a machine that has no public TCP
    // inbound, and `nft delete` on a missing table exits non-zero.
    run_shell(&format!(
        "nft delete table inet {TABLE} 2>/dev/null || true"
    ))
    .map(|_| ())
}

/// Which ports are actually exempt on this machine.
///
/// Reads the machine rather than remembering what was written: the failure this exists to catch is
/// somebody else's `nft flush ruleset` (or a firewalld reload) taking our table with it, and a
/// remembered value would report the rules as present for as long as the agent kept running.
///
/// Only the `prerouting_raw` half is parsed. Both halves are written together and the table is
/// rebuilt wholesale, so they cannot diverge; parsing one and comparing the other would only
/// report a state this module cannot produce.
pub(crate) fn installed() -> BTreeSet<u16> {
    let Ok(text) = run_shell(&format!("nft list table inet {TABLE} 2>/dev/null || true")) else {
        return BTreeSet::new();
    };
    parse_installed(&text)
}

/// Split from `installed` so a test can feed it `nft list table` output without a machine.
fn parse_installed(text: &str) -> BTreeSet<u16> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("tcp dport ")?;
            let port = rest.strip_suffix(" notrack")?;
            port.trim().parse().ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"{"inbounds":[
        {"tag":"api","listen":"127.0.0.1","port":10085,"protocol":"dokodemo-door"},
        {"tag":"in:vless","listen":"0.0.0.0","port":444,"protocol":"vless"},
        {"tag":"in:xhttp","listen":"::1","port":8080,"protocol":"vless"},
        {"tag":"in:hy2","listen":"0.0.0.0","port":443,"protocol":"hysteria"}
    ]}"#;

    /// The selection is the whole feature. Taking the hysteria port would break port hopping,
    /// because `redirect` is NAT and NAT lives in conntrack; taking a loopback port would trade
    /// nothing for risk.
    #[test]
    fn only_public_tcp_inbounds_are_exempted() {
        assert_eq!(ports(CONFIG), BTreeSet::from([444]));
    }

    /// An `nft` error at convergence is loud, but a *plausible* rule — `drop` instead of
    /// `notrack`, a missing direction — loads fine and quietly does the wrong thing to real
    /// traffic. Both directions are pinned here for that reason.
    #[test]
    fn each_port_becomes_one_rule_in_each_direction() {
        assert_eq!(
            rule_lines(&BTreeSet::from([444, 8443])),
            vec![
                "add rule inet brocade_conntrack prerouting_raw tcp dport 444 notrack",
                "add rule inet brocade_conntrack output_raw tcp sport 444 notrack",
                "add rule inet brocade_conntrack prerouting_raw tcp dport 8443 notrack",
                "add rule inet brocade_conntrack output_raw tcp sport 8443 notrack",
            ]
        );
    }

    /// What `nft list table` prints has to come back out as what went in, or the reconcile loop
    /// rebuilds a table that already matches on every single round.
    #[test]
    fn what_nft_prints_parses_back_to_what_was_written() {
        let printed = "\
table inet brocade_conntrack {
	chain prerouting_raw {
		type filter hook prerouting priority raw; policy accept;
		tcp dport 444 notrack
		tcp dport 8443 notrack
	}

	chain output_raw {
		type filter hook output priority raw; policy accept;
		tcp sport 444 notrack
		tcp sport 8443 notrack
	}
}";
        assert_eq!(parse_installed(printed), BTreeSet::from([444, 8443]));
    }

    /// An xray with no public TCP inbound must take the table down rather than leave a stale one:
    /// the rules name ports, and a port that moved would keep being exempted at its old number.
    #[test]
    fn a_config_with_nothing_to_exempt_yields_no_ports() {
        let hysteria_only = r#"{"inbounds":[
            {"tag":"api","listen":"127.0.0.1","port":10085,"protocol":"dokodemo-door"},
            {"tag":"in:hy2","listen":"0.0.0.0","port":443,"protocol":"hysteria"}
        ]}"#;
        assert!(ports(hysteria_only).is_empty());
    }

    /// A `listen` that is neither an address nor absent counts as public. Reading it as loopback
    /// would skip a port that is in fact reachable, and nothing downstream would say so.
    #[test]
    fn an_unparseable_listen_counts_as_public() {
        assert!(!is_loopback("example.net"));
        assert!(!is_loopback("0.0.0.0"));
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("::1"));
        assert!(is_loopback("[::1]"));
    }
}
