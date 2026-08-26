//! Hysteria 2 port hopping: the machine half of it.
//!
//! The server binds one UDP port. A hopping client rotates over a whole range, and every port in
//! that range other than the listener reaches the server only because this machine redirects it.
//! xray knows nothing about any of this — which is exactly why the range is an artifact of its
//! own and not a corner of `xray.json`: changing a range must not rewrite the xray config, and
//! tearing the redirect down must not restart the process.
//!
//! # Its own nft table
//!
//! `inet brocade_hy2_port_hop`, not the `inet brocade` that `phantun.rs` uses. That one is
//! deleted and rebuilt wholesale on every phantun convergence, so sharing it would have each
//! feature's
//! convergence silently delete the other's rules. The symptom — "hopping worked until somebody
//! touched fake TCP" — points nowhere near the cause, and the two features have no reason to
//! know about each other.
//!
//! Rebuilt wholesale here too, for the same reason phantun does it: the alternative is an agent
//! that incrementally edits a machine's firewall, and then a rule nobody can account for outlives
//! every convergence.

use crate::{run_shell, DesiredArtifact};

/// One range folded onto one port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Redirect {
    start: u16,
    end: u16,
    to: u16,
}

/// Apply the artifact, or take the table down when there is nothing to apply.
pub(crate) fn apply(desired: &DesiredArtifact) -> Result<(), String> {
    match desired {
        DesiredArtifact::Present { content, .. } => apply_content(content),
        DesiredArtifact::Disabled { .. } => disable(),
        // Nothing was asked of us. Notably not the same as Disabled: an artifact this agent does
        // not manage must be left exactly as it is, including a table somebody else put there.
        DesiredArtifact::Unmanaged { .. } => Ok(()),
    }
}

/// Apply the artifact text on its own. The local reconcile loop has only the file on disk — no
/// `DesiredArtifact`, because it never talks to the control plane.
pub(crate) fn apply_content(content: &str) -> Result<(), String> {
    let redirects = parse(content)?;
    {
        {
            if redirects.is_empty() {
                // The compiler emits `Disabled` rather than an empty list, so this is a snapshot
                // that took an unexpected route here. Tearing down is the same end state as
                // Disabled, and leaves nothing behind either way.
                return disable();
            }
            install(&redirects)
        }
    }
}

/// Whether the machine already matches this artifact text.
///
/// The NOTRACK check rides along with the redirect comparison. A machine that upgrades its
/// agent to a build with the raw chains would otherwise compare its old table — redirects
/// identical, rules missing — and read as matched, so the new rules would wait for a manual
/// `nft flush` that never comes.
pub(crate) fn matches_content(content: &str) -> bool {
    let mut wanted = parse(content)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.start, r.end, r.to))
        .collect::<Vec<_>>();
    let mut found = installed();
    wanted.sort_unstable();
    found.sort_unstable();
    wanted == found && notrack_installed()
}

/// The four NOTRACK rules, in their chains, are present in `nft list table` output.
///
/// `nft` prints the family as `ipv6-icmp` even though `icmpv6` was written, so the match
/// happens on the printed form. Split from `notrack_installed` so a test can feed it text
/// without a machine to run `nft` on.
fn notrack_present(text: &str) -> bool {
    let mut seen = [false; 4];
    let mut in_prerouting_raw = false;
    let mut in_output_raw = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("chain ") {
            in_prerouting_raw = line.contains("prerouting_raw");
            in_output_raw = line.contains("output_raw");
            continue;
        }
        if in_prerouting_raw && line == "ip protocol icmp notrack" {
            seen[0] = true;
        }
        if in_prerouting_raw && line == "ip6 nexthdr ipv6-icmp notrack" {
            seen[1] = true;
        }
        if in_output_raw && line == "ip protocol icmp notrack" {
            seen[2] = true;
        }
        if in_output_raw && line == "ip6 nexthdr ipv6-icmp notrack" {
            seen[3] = true;
        }
    }
    seen.iter().all(|&present| present)
}

/// Reads the machine rather than remembering what was written — same reason as `installed`.
/// A missing table means the rules are missing too.
pub(crate) fn notrack_installed() -> bool {
    let Ok(text) = run_shell(&format!("nft list table inet {TABLE} 2>/dev/null || true")) else {
        return false;
    };
    notrack_present(&text)
}

fn parse(content: &str) -> Result<Vec<Redirect>, String> {
    let value: serde_json::Value = serde_json::from_str(content)
        .map_err(|error| format!("hy2_port_hop.json 解析失败：{error}"))?;
    let Some(items) = value.get("redirects").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    items
        .iter()
        .map(|item| {
            let field = |name: &str| {
                item.get(name)
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|port| u16::try_from(port).ok())
                    .ok_or_else(|| format!("hy2_port_hop.json: redirects[].{name} 缺失或越界"))
            };
            let (start, end, to) = (field("start")?, field("end")?, field("to")?);
            if start > end {
                return Err(format!(
                    "hy2_port_hop.json: 区间 {start}-{end} 起点大于终点"
                ));
            }
            // The listener has to be inside the range it serves. The compiler refuses the other
            // case (`ingress.hy2-hop-listener`); repeated here because this is where a wrong pair
            // would become a firewall rule that swallows a range and answers on none of it.
            if to < start || to > end {
                return Err(format!(
                    "hy2_port_hop.json: 区间 {start}-{end} 不包含监听口 {to}"
                ));
            }
            Ok(Redirect { start, end, to })
        })
        .collect()
}

/// The nft table name. Distinct from phantun's, deliberately — see the module docs.
const TABLE: &str = "brocade_hy2_port_hop";

/// The rule lines, split out so a test can read them without a machine to run them on.
fn rule_lines(redirects: &[Redirect]) -> Vec<String> {
    redirects
        .iter()
        .map(|redirect| {
            format!(
                "add rule inet {TABLE} prerouting udp dport {}-{} redirect to :{}",
                redirect.start, redirect.end, redirect.to
            )
        })
        .collect()
}

/// The ICMP NOTRACK lines: static, installed next to the redirects so the redirects keep
/// working.
///
/// conntrack tracks every flow that enters the machine, before the NAT rules ever match —
/// a burst of ICMP (probes, a fleet-wide ping) fills the table, and once it is full the
/// machine drops *new* flows, the port-hop UDP flows included. ICMP is stateless, so not
/// tracking it costs nothing and removes that failure mode.
///
/// Both directions are needed: only the ingress half would still build the entry from the
/// echo reply on the way out. The raw priority (-300) runs before the nat chain (-100), so
/// a notrack-marked packet never reaches conntrack at all.
fn notrack_lines() -> Vec<String> {
    [
        format!("add rule inet {TABLE} prerouting_raw ip protocol icmp notrack"),
        format!("add rule inet {TABLE} prerouting_raw ip6 nexthdr icmpv6 notrack"),
        format!("add rule inet {TABLE} output_raw ip protocol icmp notrack"),
        format!("add rule inet {TABLE} output_raw ip6 nexthdr icmpv6 notrack"),
    ]
    .to_vec()
}

fn install(redirects: &[Redirect]) -> Result<(), String> {
    // A real newline. `"\\n"` here would be a backslash and an `n`, which joins two rules into one
    // malformed line inside the heredoc — and only ever with two ranges on one machine, so a test
    // covering a single range passes and the fleet's common case never sees it.
    let rules = rule_lines(redirects).join("\n");
    let notrack = notrack_lines().join("\n");
    // Table and rules go to one `nft -f -` together. Outside the heredoc the shell would try to
    // run `add rule ...` as a command and report `add: not found` — the same trap phantun's
    // installer documents. The raw chains carry the NOTRACK rules; `notrack` is only legal on
    // a raw-priority chain, and a same-hook chain needs its own name, hence `_raw` suffixes.
    run_shell(&format!(
        "set -eu\n\
         command -v nft >/dev/null 2>&1 || {{ echo '需要 nft（nftables）才能做端口跳转' >&2; exit 1; }}\n\
         nft delete table inet {TABLE} 2>/dev/null || true\n\
         nft -f - <<'NFT'\n\
table inet {TABLE} {{\n\
  chain prerouting {{ type nat hook prerouting priority -100; }}\n\
  chain prerouting_raw {{ type filter hook prerouting priority raw; }}\n\
  chain output_raw {{ type filter hook output priority raw; }}\n\
}}\n\
{rules}\n\
{notrack}\n\
NFT\n"
    ))
    .map(|_| ())
}

pub(crate) fn disable() -> Result<(), String> {
    // Absent table is success: this runs on every convergence of a machine that has never had a
    // range, and `nft delete` on a missing table exits non-zero.
    run_shell(&format!(
        "nft delete table inet {TABLE} 2>/dev/null || true"
    ))
    .map(|_| ())
}

/// What is actually installed, for the applied-state report and for local reconcile.
///
/// Reads the machine rather than remembering what was written: the failure this exists to catch
/// is somebody else's `nft flush ruleset` (or a firewalld reload) taking our table with it, and a
/// remembered value would report the rules as present for as long as the agent kept running.
pub(crate) fn installed() -> Vec<(u16, u16, u16)> {
    let Ok(text) = run_shell(&format!("nft list table inet {TABLE} 2>/dev/null || true")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("udp dport ")?;
            let (range, target) = rest.split_once(" redirect to :")?;
            let (start, end) = range.split_once('-')?;
            Some((
                start.trim().parse().ok()?,
                end.trim().parse().ok()?,
                target.split_whitespace().next()?.parse().ok()?,
            ))
        })
        .collect()
}

/// Whether the machine matches the artifact. Used by the local reconcile loop, which is the only
/// thing that would ever notice the table going missing between deployments.
pub(crate) fn matches(desired: &DesiredArtifact) -> bool {
    match desired {
        DesiredArtifact::Present { content, .. } => matches_content(content),
        DesiredArtifact::Disabled { .. } => installed().is_empty(),
        DesiredArtifact::Unmanaged { .. } => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule text is the whole feature and nothing type-checks it: a wrong keyword produces an
    /// nft error at convergence, and a *plausible* one (`dnat` instead of `redirect`, a missing
    /// `udp`) produces a rule that loads fine and quietly does the wrong thing to real traffic.
    #[test]
    fn a_range_becomes_one_udp_redirect_rule() {
        let rules = rule_lines(&[Redirect {
            start: 50_000,
            end: 50_009,
            to: 50_000,
        }]);
        assert_eq!(
            rules,
            vec!["add rule inet brocade_hy2_port_hop prerouting udp dport 50000-50009 redirect to :50000"]
        );
    }

    /// Two ranges on one machine, which is where the joining goes wrong and nowhere else: joining
    /// a one-element list inserts no separator at all, so a wrong separator is invisible until a
    /// second ingress on the same machine turns hopping on.
    #[test]
    fn several_ranges_are_joined_by_real_newlines() {
        let script = rule_lines(&[
            Redirect {
                start: 50_000,
                end: 50_009,
                to: 50_000,
            },
            Redirect {
                start: 50_010,
                end: 50_019,
                to: 50_010,
            },
        ])
        .join("\n");
        assert_eq!(script.lines().count(), 2, "{script:?}");
        assert!(!script.contains("\\n"), "{script:?}");
    }

    /// Not `inet brocade`. That table belongs to phantun, which deletes and rebuilds it whole on
    /// every convergence — sharing it would have each feature silently drop the other's rules.
    #[test]
    fn the_table_is_not_the_one_phantun_owns() {
        assert_ne!(TABLE, "brocade");
    }

    /// ICMP stays out of conntrack so a ping storm cannot exhaust the table the redirects
    /// depend on. Four lines: both families, both directions — the echo reply builds a flow
    /// of its own on the way out, so ingress-only would halve the effect.
    #[test]
    fn icmp_is_notracked_in_both_families_and_both_directions() {
        assert_eq!(
            notrack_lines(),
            vec![
                "add rule inet brocade_hy2_port_hop prerouting_raw ip protocol icmp notrack",
                "add rule inet brocade_hy2_port_hop prerouting_raw ip6 nexthdr icmpv6 notrack",
                "add rule inet brocade_hy2_port_hop output_raw ip protocol icmp notrack",
                "add rule inet brocade_hy2_port_hop output_raw ip6 nexthdr icmpv6 notrack",
            ]
        );
    }

    /// The upgrade path `matches_content` exists for: the old table has the redirects the
    /// artifact wants but none of the NOTRACK rules, and it must read as not matched so the
    /// next convergence installs the full table.
    #[test]
    fn a_table_missing_the_notrack_rules_does_not_read_as_present() {
        let old_table = "table inet brocade_hy2_port_hop {\n  chain prerouting {\n    type nat hook prerouting priority dstnat; policy accept;\n    udp dport 50000-50009 redirect to :50000\n  }\n}";
        assert!(!notrack_present(old_table), "{old_table:?}");
    }

    /// The printed form the parser has to accept: `nft` renders the written `icmpv6` as
    /// `ipv6-icmp`, and a parser looking for the written spelling would report the rules
    /// missing forever and reinstall on every round.
    #[test]
    fn the_notrack_rules_are_recognised_in_their_printed_form() {
        let printed = "table inet brocade_hy2_port_hop {\n  chain prerouting {\n    type nat hook prerouting priority dstnat; policy accept;\n    udp dport 50000-50009 redirect to :50000\n  }\n  chain prerouting_raw {\n    type filter hook prerouting priority raw; policy accept;\n    ip protocol icmp notrack\n    ip6 nexthdr ipv6-icmp notrack\n  }\n  chain output_raw {\n    type filter hook output priority raw; policy accept;\n    ip protocol icmp notrack\n    ip6 nexthdr ipv6-icmp notrack\n  }\n}";
        assert!(notrack_present(printed), "{printed:?}");
    }

    /// A rule that drifted into the wrong chain must not count: presence is per-chain, or a
    /// table with both rules in one chain and none in the other would pass.
    #[test]
    fn a_notrack_rule_in_the_wrong_chain_does_not_count() {
        let crossed = "table inet brocade_hy2_port_hop {\n  chain prerouting_raw {\n    type filter hook prerouting priority raw; policy accept;\n    ip protocol icmp notrack\n    ip6 nexthdr ipv6-icmp notrack\n    ip protocol icmp notrack\n  }\n  chain output_raw {\n    type filter hook output priority raw; policy accept;\n    ip6 nexthdr ipv6-icmp notrack\n  }\n}";
        assert!(!notrack_present(crossed), "{crossed:?}");
    }

    #[test]
    fn a_range_that_excludes_its_listener_is_refused_rather_than_installed() {
        let refused = parse(r#"{"redirects":[{"start":50000,"end":50009,"to":443}]}"#);
        assert!(refused.is_err(), "{refused:?}");
        let reversed = parse(r#"{"redirects":[{"start":50009,"end":50000,"to":50000}]}"#);
        assert!(reversed.is_err(), "{reversed:?}");
    }

    /// The disabled artifact is an empty object, and it has to read as "no ranges" rather than as
    /// a parse failure — every machine without hopping converges through this path.
    #[test]
    fn the_disabled_artifact_parses_as_no_ranges() {
        assert_eq!(parse("{}").unwrap(), Vec::new());
    }

    /// What `nft list table` prints has to come back out as what went in, or the reconcile loop
    /// reports drift on a machine that is in fact correct — and then reinstalls on every round.
    /// The listing includes the NOTRACK lines too: the parser must skip them, or the drift it
    /// fabricates would reinstall the table on every convergence.
    #[test]
    fn installed_rules_are_read_back_in_the_shape_they_were_written() {
        let listing = "table inet brocade_hy2_port_hop {\n  chain prerouting {\n    type nat hook prerouting priority dstnat; policy accept;\n    udp dport 50000-50009 redirect to :50000\n  }\n  chain prerouting_raw {\n    type filter hook prerouting priority raw; policy accept;\n    ip protocol icmp notrack\n  }\n  chain output_raw {\n    type filter hook output priority raw; policy accept;\n    ip protocol icmp notrack\n  }\n}";
        let parsed = listing
            .lines()
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("udp dport ")?;
                let (range, target) = rest.split_once(" redirect to :")?;
                let (start, end) = range.split_once('-')?;
                Some((
                    start.trim().parse::<u16>().ok()?,
                    end.trim().parse::<u16>().ok()?,
                    target.split_whitespace().next()?.parse::<u16>().ok()?,
                ))
            })
            .collect::<Vec<_>>();
        assert_eq!(parsed, vec![(50_000, 50_009, 50_000)]);
    }
}
