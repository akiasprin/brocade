use std::{collections::BTreeSet, net::IpAddr};

use serde::{Deserialize, Serialize};

use crate::{
    model::{Action, Network, INGRESS_GUARD_AMPLIFICATION_PORTS, INGRESS_GUARD_MAIL_PORTS},
    physical::user::{subscription_endpoints, SubscriptionEndpoint},
};

use super::{
    routing::{AppIr, DestMatch, Ingress, Rule},
    system::SystemIr,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FrontRouteStatus {
    Reachable,
    Blocked,
    Conditional,
    Unknown,
    External,
    Pending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FrontRouteMemberKind {
    Internal,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontRouteDecision {
    pub status: FrontRouteStatus,
    pub chain_id: Option<String>,
    pub node_id: Option<String>,
    /// One-based position in the compiled rule table at `node_id`.
    pub rule_index: Option<usize>,
    pub selector: Option<String>,
    pub action: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontRouteMember {
    pub id: String,
    pub kind: FrontRouteMemberKind,
    pub chain_id: Option<String>,
    pub node_id: Option<String>,
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontRouteTarget {
    pub id: String,
    pub chain_id: Option<String>,
    pub node_id: Option<String>,
    pub endpoints: Vec<String>,
    pub landing: FrontRouteDecision,
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontEndpointRoute {
    pub endpoint: String,
    pub decision: FrontRouteDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontRouteCell {
    pub member_id: String,
    pub target_id: String,
    pub endpoints: Vec<FrontEndpointRoute>,
    pub relay: FrontRouteDecision,
    pub landing: FrontRouteDecision,
    pub combined: FrontRouteDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontRouteAnalysis {
    pub front_id: String,
    pub members: Vec<FrontRouteMember>,
    pub targets: Vec<FrontRouteTarget>,
    pub cells: Vec<FrontRouteCell>,
    pub blocking: bool,
}

struct AnalyzedTarget {
    view: FrontRouteTarget,
    endpoints: Vec<SubscriptionEndpoint>,
}

/// Analyze one proposed Front against the server topology represented by `app`.
///
/// The caller deliberately supplies the complete member and target lists. A client checkpoint may
/// already name resources which are absent from the serving topology; retaining those ids here is
/// what lets the result say `pending` instead of silently dropping a row or column.
pub fn analyze_front_routes(
    system: Option<&SystemIr>,
    app: Option<&AppIr>,
    front_id: &str,
    internal_members: &[String],
    external_members: &[String],
    target_ids: &[String],
) -> FrontRouteAnalysis {
    let mut members = Vec::with_capacity(internal_members.len() + external_members.len());
    for id in internal_members {
        let ingress = app.and_then(|app| app.ingresses.iter().find(|ingress| ingress.id == *id));
        members.push(FrontRouteMember {
            id: id.clone(),
            kind: FrontRouteMemberKind::Internal,
            chain_id: ingress.map(|ingress| ingress.chain.clone()),
            node_id: ingress.map(|ingress| ingress.node.clone()),
            pending: ingress.is_none(),
        });
    }
    for id in external_members {
        let present = app.is_some_and(|app| {
            app.external_outbounds
                .iter()
                .any(|outbound| outbound.id == *id)
        });
        members.push(FrontRouteMember {
            id: id.clone(),
            kind: FrontRouteMemberKind::External,
            chain_id: None,
            node_id: None,
            pending: !present,
        });
    }

    let analyzed_targets = target_ids
        .iter()
        .map(|id| analyze_target(app, id))
        .collect::<Vec<_>>();
    let targets = analyzed_targets
        .iter()
        .map(|target| target.view.clone())
        .collect::<Vec<_>>();
    let mut cells = Vec::with_capacity(members.len().saturating_mul(analyzed_targets.len()));
    for member in &members {
        for target in &analyzed_targets {
            let (endpoints, relay) =
                relay_decision(system, app, member, &target.view, &target.endpoints);
            let landing = target.view.landing.clone();
            let combined = combine_decisions(&relay, &landing);
            cells.push(FrontRouteCell {
                member_id: member.id.clone(),
                target_id: target.view.id.clone(),
                endpoints,
                relay,
                landing,
                combined,
            });
        }
    }
    let blocking = cells
        .iter()
        .any(|cell| cell.combined.status == FrontRouteStatus::Blocked);

    FrontRouteAnalysis {
        front_id: front_id.to_owned(),
        members,
        targets,
        cells,
        blocking,
    }
}

fn analyze_target(app: Option<&AppIr>, id: &str) -> AnalyzedTarget {
    let Some(app) = app else {
        return pending_target(id);
    };
    let Some(ingress) = app.ingresses.iter().find(|ingress| ingress.id == id) else {
        return pending_target(id);
    };
    let endpoints = app
        .nodes
        .iter()
        .find(|node| node.id == ingress.node)
        .map(|node| subscription_endpoints(node, ingress))
        .unwrap_or_default();
    let landing = route_decision(app, &ingress.chain, &ingress.node, RouteSubject::Internet);
    AnalyzedTarget {
        view: FrontRouteTarget {
            id: id.to_owned(),
            chain_id: Some(ingress.chain.clone()),
            node_id: Some(ingress.node.clone()),
            endpoints: endpoints.iter().map(SubscriptionEndpoint::label).collect(),
            landing,
            pending: false,
        },
        endpoints,
    }
}

fn pending_target(id: &str) -> AnalyzedTarget {
    AnalyzedTarget {
        view: FrontRouteTarget {
            id: id.to_owned(),
            chain_id: None,
            node_id: None,
            endpoints: Vec::new(),
            landing: simple_decision(
                FrontRouteStatus::Pending,
                format!("目标入口 {id} 尚未进入服务中拓扑"),
            ),
            pending: true,
        },
        endpoints: Vec::new(),
    }
}

fn relay_decision(
    system: Option<&SystemIr>,
    app: Option<&AppIr>,
    member: &FrontRouteMember,
    target: &FrontRouteTarget,
    target_endpoints: &[SubscriptionEndpoint],
) -> (Vec<FrontEndpointRoute>, FrontRouteDecision) {
    if member.pending {
        return (
            Vec::new(),
            simple_decision(
                FrontRouteStatus::Pending,
                format!("成员 {} 尚未进入服务中拓扑", member.id),
            ),
        );
    }
    if target.pending {
        return (
            Vec::new(),
            simple_decision(
                FrontRouteStatus::Pending,
                format!("目标入口 {} 尚未进入服务中拓扑", target.id),
            ),
        );
    }
    if target_endpoints.is_empty() {
        return (
            Vec::new(),
            simple_decision(
                FrontRouteStatus::Unknown,
                format!("目标入口 {} 没有可分析的订阅地址", target.id),
            ),
        );
    }
    if member.kind == FrontRouteMemberKind::External {
        let endpoints = target_endpoints
            .iter()
            .map(|endpoint| FrontEndpointRoute {
                endpoint: endpoint.label(),
                decision: simple_decision(
                    FrontRouteStatus::External,
                    format!("外部成员 {} 的第一段路由不受 Brocade 管理", member.id),
                ),
            })
            .collect();
        return (
            endpoints,
            simple_decision(
                FrontRouteStatus::External,
                format!("外部成员 {} 的第一段路由不受 Brocade 管理", member.id),
            ),
        );
    }

    let Some(app) = app else {
        return (
            Vec::new(),
            simple_decision(FrontRouteStatus::Pending, "尚未发布服务端拓扑".to_owned()),
        );
    };
    let (Some(chain), Some(node)) = (member.chain_id.as_deref(), member.node_id.as_deref()) else {
        return (
            Vec::new(),
            simple_decision(
                FrontRouteStatus::Unknown,
                format!("无法定位成员 {} 的服务端链路", member.id),
            ),
        );
    };
    let Some(ingress) = app.ingresses.iter().find(|ingress| ingress.id == member.id) else {
        return (
            Vec::new(),
            simple_decision(
                FrontRouteStatus::Unknown,
                format!("无法定位成员 {} 的入口防护", member.id),
            ),
        );
    };
    let endpoints = target_endpoints
        .iter()
        .map(|endpoint| FrontEndpointRoute {
            endpoint: endpoint.label(),
            decision: ingress_guard_decision(system, ingress, endpoint).unwrap_or_else(|| {
                route_decision(app, chain, node, RouteSubject::Endpoint(endpoint))
            }),
        })
        .collect::<Vec<_>>();
    let relay = aggregate_endpoint_decisions(&endpoints);
    (endpoints, relay)
}

/// Evaluate the member ingress's fixed admission rules before its chain table.
///
/// Xray emits these guards ahead of every chain rule and scopes them to this ingress's inbound
/// tags. They therefore apply only at the client-selected member, not again at relay hops. The
/// target proxy handshake has a known socket but no BitTorrent application payload, so the
/// protocol-sniffing guard cannot match here; every guard whose inputs are present is exact.
fn ingress_guard_decision(
    system: Option<&SystemIr>,
    ingress: &Ingress,
    endpoint: &SubscriptionEndpoint,
) -> Option<FrontRouteDecision> {
    let guard = &ingress.guard;
    let (selector, reason) = if guard.no_private
        && (is_private_subscription_address(&endpoint.host)
            || system.is_some_and(|system| overlay_contains(system, &endpoint.host)))
    {
        (
            "ingress-guard:no-private",
            "成员入口的私网隔离阻断该订阅地址",
        )
    } else if guard.no_mail && INGRESS_GUARD_MAIL_PORTS.contains(&endpoint.port) {
        (
            "ingress-guard:no-mail",
            "成员入口的邮件端口防护阻断该订阅地址",
        )
    } else if guard.no_udp_amplification
        && endpoint.network == Network::Udp
        && INGRESS_GUARD_AMPLIFICATION_PORTS.contains(&endpoint.port)
    {
        (
            "ingress-guard:no-udp-amplification",
            "成员入口的 UDP 放大防护阻断该订阅地址",
        )
    } else if guard.tcp_and_quic_only && endpoint.network == Network::Udp && endpoint.port != 443 {
        (
            "ingress-guard:tcp-and-quic-only",
            "成员入口仅允许 TCP 和 QUIC，阻断该 UDP 订阅地址",
        )
    } else {
        return None;
    };

    Some(located_decision(
        FrontRouteStatus::Blocked,
        &ingress.chain,
        &ingress.node,
        None,
        Some(selector.to_owned()),
        Some("block".to_owned()),
        reason.to_owned(),
    ))
}

fn overlay_contains(system: &SystemIr, host: &str) -> bool {
    host.parse::<std::net::Ipv4Addr>()
        .is_ok_and(|address| system.overlay_cidr.contains(&address))
}

fn is_private_subscription_address(host: &str) -> bool {
    host.parse::<IpAddr>().is_ok_and(|address| match address {
        IpAddr::V4(address) => address.is_private(),
        IpAddr::V6(address) => address.is_unique_local(),
    })
}

fn aggregate_endpoint_decisions(endpoints: &[FrontEndpointRoute]) -> FrontRouteDecision {
    for status in [
        FrontRouteStatus::Blocked,
        FrontRouteStatus::Unknown,
        FrontRouteStatus::Conditional,
        FrontRouteStatus::Reachable,
    ] {
        if let Some(endpoint) = endpoints
            .iter()
            .find(|endpoint| endpoint.decision.status == status)
        {
            let mut decision = endpoint.decision.clone();
            decision.reason = match status {
                FrontRouteStatus::Blocked => {
                    format!("订阅地址 {} 被成员链路明确阻断", endpoint.endpoint)
                }
                FrontRouteStatus::Unknown => {
                    format!("订阅地址 {} 的成员链路无法静态判定", endpoint.endpoint)
                }
                FrontRouteStatus::Conditional => {
                    format!("订阅地址 {} 的成员链路取决于动态规则", endpoint.endpoint)
                }
                FrontRouteStatus::Reachable => "所有订阅地址均由成员链路明确放行".to_owned(),
                FrontRouteStatus::External | FrontRouteStatus::Pending => unreachable!(),
            };
            return decision;
        }
    }
    simple_decision(
        FrontRouteStatus::Unknown,
        "目标入口没有可分析的订阅地址".to_owned(),
    )
}

fn combine_decisions(
    relay: &FrontRouteDecision,
    landing: &FrontRouteDecision,
) -> FrontRouteDecision {
    if relay.status == FrontRouteStatus::Blocked {
        return relay.clone();
    }
    if landing.status == FrontRouteStatus::Blocked {
        return landing.clone();
    }
    if relay.status == FrontRouteStatus::Pending || landing.status == FrontRouteStatus::Pending {
        return simple_decision(
            FrontRouteStatus::Pending,
            "成员或目标尚未进入服务中拓扑".to_owned(),
        );
    }
    if relay.status == FrontRouteStatus::External {
        return simple_decision(
            FrontRouteStatus::External,
            "落地未明确阻断，但第一段由外部成员管理，端到端尚未验证".to_owned(),
        );
    }
    if relay.status == FrontRouteStatus::Unknown || landing.status == FrontRouteStatus::Unknown {
        return simple_decision(
            FrontRouteStatus::Unknown,
            "至少一段路由无法从当前服务中规则确定".to_owned(),
        );
    }
    if relay.status == FrontRouteStatus::Conditional
        || landing.status == FrontRouteStatus::Conditional
    {
        return simple_decision(
            FrontRouteStatus::Conditional,
            "至少一段路由依赖动态或目的地址条件".to_owned(),
        );
    }
    simple_decision(
        FrontRouteStatus::Reachable,
        "中继链路明确放行，落地链路具有无条件出网路径".to_owned(),
    )
}

#[derive(Clone, Copy)]
enum RouteSubject<'a> {
    Endpoint(&'a SubscriptionEndpoint),
    Internet,
}

fn route_decision(
    app: &AppIr,
    chain: &str,
    node: &str,
    subject: RouteSubject<'_>,
) -> FrontRouteDecision {
    route_decision_inner(app, chain, node, subject, &mut BTreeSet::new())
}

fn route_decision_inner(
    app: &AppIr,
    chain: &str,
    node: &str,
    subject: RouteSubject<'_>,
    seen: &mut BTreeSet<String>,
) -> FrontRouteDecision {
    let key = format!("{chain}|{node}");
    if !seen.insert(key) {
        return located_decision(
            FrontRouteStatus::Unknown,
            chain,
            node,
            None,
            None,
            None,
            "链路规则形成循环，无法得到终点".to_owned(),
        );
    }

    let Some(step) = app
        .steps
        .iter()
        .find(|step| step.chain == chain && step.node == node)
    else {
        return located_decision(
            FrontRouteStatus::Unknown,
            chain,
            node,
            None,
            None,
            None,
            "服务中拓扑没有对应的链路步骤".to_owned(),
        );
    };

    let mut possible = Vec::new();
    for (index, rule) in step.rules.iter().enumerate() {
        match match_subject(&rule.dest_match, subject) {
            MatchVerdict::Miss => {}
            MatchVerdict::Maybe => {
                possible.push(route_action_decision(
                    app,
                    chain,
                    node,
                    index + 1,
                    rule,
                    subject,
                    &mut seen.clone(),
                ));
            }
            MatchVerdict::Hit => {
                possible.push(route_action_decision(
                    app,
                    chain,
                    node,
                    index + 1,
                    rule,
                    subject,
                    &mut seen.clone(),
                ));
                return aggregate_rule_decisions(chain, node, possible);
            }
        }
    }

    possible.push(located_decision(
        FrontRouteStatus::Unknown,
        chain,
        node,
        None,
        None,
        None,
        "规则可能全部不命中，无法得到最终动作".to_owned(),
    ));
    aggregate_rule_decisions(chain, node, possible)
}

fn route_action_decision(
    app: &AppIr,
    chain: &str,
    node: &str,
    rule_index: usize,
    rule: &Rule,
    subject: RouteSubject<'_>,
    seen: &mut BTreeSet<String>,
) -> FrontRouteDecision {
    let selector = selector_name(&rule.dest_match).to_owned();
    match &rule.action {
        Action::Egress { .. } => located_decision(
            FrontRouteStatus::Reachable,
            chain,
            node,
            Some(rule_index),
            Some(selector),
            Some("egress".to_owned()),
            "规则选择本机出网".to_owned(),
        ),
        Action::Proxy { .. } => located_decision(
            FrontRouteStatus::Reachable,
            chain,
            node,
            Some(rule_index),
            Some(selector),
            Some("proxy".to_owned()),
            "规则选择受管代理出口".to_owned(),
        ),
        Action::Block => located_decision(
            FrontRouteStatus::Blocked,
            chain,
            node,
            Some(rule_index),
            Some(selector),
            Some("block".to_owned()),
            "规则明确阻断".to_owned(),
        ),
        Action::Forward { to, .. } => {
            let mut decision = route_decision_inner(app, chain, to, subject, seen);
            if decision.action.is_none() {
                decision.action = Some("forward".to_owned());
            }
            decision
        }
        Action::ReuseListener { listener, .. } => {
            let mut decision =
                route_decision_inner(app, &listener.chain, &listener.node, subject, seen);
            if decision.action.is_none() {
                decision.action = Some("reuse-listener".to_owned());
            }
            decision
        }
    }
}

fn aggregate_rule_decisions(
    chain: &str,
    node: &str,
    decisions: Vec<FrontRouteDecision>,
) -> FrontRouteDecision {
    if decisions
        .iter()
        .all(|decision| decision.status == FrontRouteStatus::Blocked)
    {
        let mut decision = decisions
            .first()
            .expect("a route table always contributes a hit or no-match decision")
            .clone();
        if decisions.len() > 1 {
            decision.reason = "所有可能先命中的规则都明确阻断".to_owned();
        }
        return decision;
    }
    if decisions
        .iter()
        .all(|decision| decision.status == FrontRouteStatus::Reachable)
    {
        let mut decision = decisions
            .first()
            .expect("a route table always contributes a hit or no-match decision")
            .clone();
        if decisions.len() > 1 {
            decision.reason = "所有可能先命中的规则都具有出网路径".to_owned();
        }
        return decision;
    }
    if let Some(unknown) = decisions
        .iter()
        .find(|decision| decision.status == FrontRouteStatus::Unknown)
    {
        let mut decision = unknown.clone();
        decision.reason = "至少一个可能的规则分支无法得到最终动作".to_owned();
        return decision;
    }
    located_decision(
        FrontRouteStatus::Conditional,
        chain,
        node,
        None,
        None,
        None,
        "不同的运行时规则分支会得到不同结果".to_owned(),
    )
}

fn located_decision(
    status: FrontRouteStatus,
    chain: &str,
    node: &str,
    rule_index: Option<usize>,
    selector: Option<String>,
    action: Option<String>,
    reason: String,
) -> FrontRouteDecision {
    FrontRouteDecision {
        status,
        chain_id: Some(chain.to_owned()),
        node_id: Some(node.to_owned()),
        rule_index,
        selector,
        action,
        reason,
    }
}

fn simple_decision(status: FrontRouteStatus, reason: String) -> FrontRouteDecision {
    FrontRouteDecision {
        status,
        chain_id: None,
        node_id: None,
        rule_index: None,
        selector: None,
        action: None,
        reason,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchVerdict {
    Hit,
    Miss,
    Maybe,
}

fn match_subject(dest_match: &DestMatch, subject: RouteSubject<'_>) -> MatchVerdict {
    match subject {
        RouteSubject::Endpoint(host) => match_endpoint(dest_match, host),
        RouteSubject::Internet => match dest_match {
            DestMatch::Any => MatchVerdict::Hit,
            // A target carries arbitrary user traffic. Any narrower selector may or may not match
            // a particular request, so only the unconditional fallback can prove a landing path.
            _ => MatchVerdict::Maybe,
        },
    }
}

fn match_endpoint(dest_match: &DestMatch, endpoint: &SubscriptionEndpoint) -> MatchVerdict {
    match dest_match {
        DestMatch::Any => MatchVerdict::Hit,
        DestMatch::DomainSuffix(values) => {
            if values
                .iter()
                .any(|value| domain_suffix_match(&endpoint.host, value))
            {
                MatchVerdict::Hit
            } else {
                MatchVerdict::Miss
            }
        }
        DestMatch::DomainKeyword(values) => {
            let host = endpoint.host.to_ascii_lowercase();
            if values
                .iter()
                .any(|value| host.contains(&value.to_ascii_lowercase()))
            {
                MatchVerdict::Hit
            } else {
                MatchVerdict::Miss
            }
        }
        DestMatch::IpCidr(values) => ip_match(&endpoint.host, values),
        DestMatch::Port(values) => port_match(endpoint.port, values),
        DestMatch::PortExcept(values) => {
            if values.contains(&endpoint.port) {
                MatchVerdict::Miss
            } else {
                MatchVerdict::Hit
            }
        }
        DestMatch::Network(network) => {
            if *network == endpoint.network {
                MatchVerdict::Hit
            } else {
                MatchVerdict::Miss
            }
        }
        DestMatch::All(values) => {
            let mut maybe = false;
            for value in values {
                match match_endpoint(value, endpoint) {
                    MatchVerdict::Hit => {}
                    MatchVerdict::Miss => return MatchVerdict::Miss,
                    MatchVerdict::Maybe => maybe = true,
                }
            }
            if maybe {
                MatchVerdict::Maybe
            } else {
                MatchVerdict::Hit
            }
        }
        // The endpoint projection provides host, port and TCP/UDP, but not runtime GeoIP/geosite
        // data or the protocol a sniffer may identify. Treating these as a miss would incorrectly
        // skip a rule which can win in production.
        DestMatch::DomainRegex(_)
        | DestMatch::Geosite(_)
        | DestMatch::Geoip(_)
        | DestMatch::Protocol(_)
        | DestMatch::SniffingFailed => MatchVerdict::Maybe,
    }
}

fn selector_name(dest_match: &DestMatch) -> &'static str {
    match dest_match {
        DestMatch::Any => "any",
        DestMatch::SniffingFailed => "sniffing-failed",
        DestMatch::DomainSuffix(_) => "domain-suffix",
        DestMatch::DomainKeyword(_) => "domain-keyword",
        DestMatch::DomainRegex(_) => "domain-regex",
        DestMatch::Geosite(_) => "geosite",
        DestMatch::IpCidr(_) => "ip-cidr",
        DestMatch::Geoip(_) => "geoip",
        DestMatch::Port(_) => "port",
        DestMatch::PortExcept(_) => "port-except",
        DestMatch::Network(_) => "network",
        DestMatch::Protocol(_) => "protocol",
        DestMatch::All(_) => "all",
    }
}

fn domain_suffix_match(host: &str, suffix: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let suffix = suffix.to_ascii_lowercase();
    host == suffix || host.ends_with(&format!(".{suffix}"))
}

fn ip_match(host: &str, values: &[String]) -> MatchVerdict {
    let Ok(ip) = host.parse::<IpAddr>() else {
        return MatchVerdict::Miss;
    };
    for value in values {
        if value
            .parse::<IpAddr>()
            .is_ok_and(|candidate| candidate == ip)
            || value
                .parse::<ipnet::IpNet>()
                .is_ok_and(|network| network.contains(&ip))
        {
            return MatchVerdict::Hit;
        }
    }
    MatchVerdict::Miss
}

fn port_match(port: u16, values: &[String]) -> MatchVerdict {
    let mut invalid = false;
    for value in values.iter().flat_map(|value| value.split(',')) {
        let value = value.trim();
        if let Ok(candidate) = value.parse::<u16>() {
            if candidate == port {
                return MatchVerdict::Hit;
            }
            invalid |= candidate == 0;
            continue;
        }
        let Some((low, high)) = value.split_once('-') else {
            invalid = true;
            continue;
        };
        let Ok(low) = low.trim().parse::<u16>() else {
            invalid = true;
            continue;
        };
        let Ok(high) = high.trim().parse::<u16>() else {
            invalid = true;
            continue;
        };
        if low == 0 || low > high {
            invalid = true;
        } else if (low..=high).contains(&port) {
            return MatchVerdict::Hit;
        }
    }
    if invalid {
        MatchVerdict::Maybe
    } else {
        MatchVerdict::Miss
    }
}
