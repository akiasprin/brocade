//! Bounded process-local rendezvous. Tickets and terminal contents are never persisted.
use brocade_deployment::cvm::terminal::{
    self as wire, ConnectionOffer, Offer, Target, MAX_ATTACHMENTS_PER_NODE, MAX_SESSIONS,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, watch};

#[derive(Debug)]
pub enum Packet {
    Data(Vec<u8>),
    Control(String),
}
pub struct Link {
    pub send: mpsc::Sender<Packet>,
    pub receive: mpsc::Receiver<Packet>,
    pub cancelled: watch::Receiver<bool>,
}
struct Session {
    created: Instant,
    operator: String,
    node: String,
    target: Target,
    ticket: Option<String>,
    browser: Option<Link>,
    agent: Option<Link>,
    cancel: watch::Sender<bool>,
    terminal_id: Option<String>,
    new_session: bool,
    policy_revision: u64,
    rtc: bool,
    ice: Option<wire::IceCandidates>,
    validated_at: Instant,
    browser_seen_at: Option<Instant>,
}
struct Agent {
    id: String,
    offers: mpsc::Sender<ConnectionOffer>,
    features: AgentFeatures,
}
#[derive(serde::Deserialize)]
pub struct Attach {
    pub ticket: String,
    #[serde(default)]
    pub rtc_offer: Option<String>,
    #[serde(default)]
    pub trickle: bool,
}

#[derive(Default)]
pub struct AgentFeatures {
    pub rtc: bool,
    pub latency: bool,
    pub trickle: bool,
}
#[derive(Default)]
struct Inner {
    sessions: HashMap<String, Session>,
    agents: HashMap<String, Agent>,
    stun_servers: Vec<String>,
}
#[derive(Clone, Default)]
pub struct Service(Arc<Mutex<Inner>>);

pub fn random_id() -> Result<String, &'static str> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "terminal entropy unavailable")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn links() -> (Link, Link, watch::Sender<bool>) {
    let (a, b) = mpsc::channel(4);
    let (c, d) = mpsc::channel(4);
    let (cancel, rx) = watch::channel(false);
    (
        Link {
            send: a,
            receive: d,
            cancelled: rx.clone(),
        },
        Link {
            send: c,
            receive: b,
            cancelled: rx,
        },
        cancel,
    )
}
impl Inner {
    fn remove(&mut self, id: &str) {
        if let Some(s) = self.sessions.remove(id) {
            let _ = s.cancel.send(true);
        }
    }
    fn expire(&mut self) {
        let expired: Vec<_> = self
            .sessions
            .iter()
            .filter(|(_, s)| {
                s.created.elapsed()
                    > Duration::from_secs(if s.ticket.is_some() { 30 } else { 7200 })
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.remove(&id)
        }
    }
}
impl Service {
    pub fn uses_rtc(&self, id: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get(id)
            .is_some_and(|s| s.rtc)
    }
    pub fn configure_stun(&self, servers: Vec<String>) -> Result<(), &'static str> {
        if servers.len() > 2 {
            return Err("at most two controlled STUN servers may be configured");
        }
        for server in &servers {
            wire::stun_endpoint(server)?;
        }
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stun_servers = servers;
        Ok(())
    }
    pub fn rtc_settings(&self, node: &str) -> wire::RtcSettings {
        let inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        wire::RtcSettings {
            supported: inner
                .agents
                .get(node)
                .is_some_and(|a| a.features.rtc && !a.offers.is_closed()),
            stun_servers: inner.stun_servers.clone(),
            trickle: inner
                .agents
                .get(node)
                .is_some_and(|a| a.features.trickle && !a.offers.is_closed()),
        }
    }
    pub fn latency_supported(&self, node: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .agents
            .get(node)
            .is_some_and(|a| a.features.latency && !a.offers.is_closed())
    }
    pub fn accept_ice_signal(
        &self,
        id: &str,
        signal: &wire::IceSignal,
    ) -> Result<bool, &'static str> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get_mut(id)
            .and_then(|s| s.ice.as_mut())
            .ok_or("terminal trickle ICE was not negotiated")?
            .accept(signal)
    }
    pub fn validated(&self, id: &str) {
        if let Some(s) = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get_mut(id)
        {
            s.validated_at = Instant::now();
        }
    }
    pub fn rtc_lease_allowed(&self, id: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get(id)
            .is_some_and(|s| {
                s.rtc
                    && s.ticket.is_none()
                    && s.validated_at.elapsed() <= Duration::from_secs(wire::CONTROL_FRESH_SECONDS)
                    && s.browser_seen_at.is_some_and(|seen| {
                        seen.elapsed() <= Duration::from_secs(wire::CONTROL_FRESH_SECONDS)
                    })
            })
    }
    pub fn browser_seen(&self, id: &str) {
        if let Some(s) = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get_mut(id)
        {
            s.browser_seen_at = Some(Instant::now());
        }
    }
    pub fn expire(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .expire()
    }
    #[cfg(test)]
    pub fn register(
        &self,
        node: &str,
    ) -> Result<(String, mpsc::Receiver<ConnectionOffer>), &'static str> {
        self.register_with_capabilities(node, false, false)
    }
    #[cfg(test)]
    pub fn register_with_capabilities(
        &self,
        node: &str,
        rtc: bool,
        latency: bool,
    ) -> Result<(String, mpsc::Receiver<ConnectionOffer>), &'static str> {
        self.register_with_features(
            node,
            AgentFeatures {
                rtc,
                latency,
                trickle: false,
            },
        )
    }
    pub fn register_with_features(
        &self,
        node: &str,
        features: AgentFeatures,
    ) -> Result<(String, mpsc::Receiver<ConnectionOffer>), &'static str> {
        if features.trickle && !features.rtc {
            return Err("trickle ICE requires RTC");
        }
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.expire();
        if inner.agents.len() >= 4096 && !inner.agents.contains_key(node) {
            return Err("terminal machine limit reached");
        }
        let id = random_id()?;
        let (tx, rx) = mpsc::channel(MAX_ATTACHMENTS_PER_NODE);
        let old: Vec<_> = inner
            .sessions
            .iter()
            .filter(|(_, s)| s.node == node)
            .map(|(id, _)| id.clone())
            .collect();
        for session in old {
            inner.remove(&session)
        }
        inner.agents.insert(
            node.to_owned(),
            Agent {
                id: id.clone(),
                offers: tx,
                features,
            },
        );
        Ok((id, rx))
    }
    pub fn unregister(&self, node: &str, id: &str) {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner.agents.get(node).is_none_or(|a| a.id != id) {
            return;
        }
        inner.agents.remove(node);
        let ids: Vec<_> = inner
            .sessions
            .iter()
            .filter(|(_, s)| s.node == node)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            inner.remove(&id)
        }
    }
    #[cfg(test)]
    pub fn create(
        &self,
        operator: &str,
        node: &str,
        target: Target,
    ) -> Result<(String, String), &'static str> {
        self.create_attachment(operator, node, target, None, false, 0)
    }
    pub fn create_attachment(
        &self,
        operator: &str,
        node: &str,
        target: Target,
        terminal_id: Option<String>,
        new_session: bool,
        policy_revision: u64,
    ) -> Result<(String, String), &'static str> {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.expire();
        if inner.agents.get(node).is_none_or(|a| a.offers.is_closed()) {
            return Err("承载机器终端通道未连接，请升级 Agent 或稍后重试");
        }
        if inner.sessions.len() >= MAX_SESSIONS
            || inner.sessions.values().filter(|s| s.node == node).count()
                >= MAX_ATTACHMENTS_PER_NODE
        {
            return Err("终端会话数已达到上限");
        }
        let id = random_id()?;
        let ticket = random_id()?;
        let (browser, agent, cancel) = links();
        inner.sessions.insert(
            id.clone(),
            Session {
                created: Instant::now(),
                operator: operator.into(),
                node: node.into(),
                target,
                ticket: Some(ticket.clone()),
                browser: Some(browser),
                agent: Some(agent),
                cancel,
                terminal_id,
                new_session,
                policy_revision,
                rtc: false,
                ice: None,
                validated_at: Instant::now(),
                browser_seen_at: None,
            },
        );
        Ok((id, ticket))
    }
    #[cfg(test)]
    pub fn browser(
        &self,
        id: &str,
        ticket: &str,
        operator: &str,
        node: &str,
        target: &Target,
    ) -> Result<Link, &'static str> {
        self.browser_with_rtc(
            id,
            operator,
            node,
            target,
            &Attach {
                ticket: ticket.to_owned(),
                rtc_offer: None,
                trickle: false,
            },
        )
    }
    pub fn browser_with_rtc(
        &self,
        id: &str,
        operator: &str,
        node: &str,
        target: &Target,
        attachment: &Attach,
    ) -> Result<Link, &'static str> {
        let Attach {
            ticket,
            rtc_offer: sdp,
            trickle,
        } = attachment;
        let trickle = *trickle;
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.expire();
        let rtc = if let Some(sdp) = sdp {
            if !wire::valid_sdp(sdp)
                || !inner
                    .agents
                    .get(node)
                    .is_some_and(|a| a.features.rtc && (!trickle || a.features.trickle))
            {
                return Err("terminal WebRTC offer is unsupported or invalid");
            }
            Some(wire::RtcOffer {
                sdp: sdp.clone(),
                stun_servers: inner.stun_servers.clone(),
                trickle,
            })
        } else {
            if trickle {
                return Err("trickle ICE requires an offer");
            }
            None
        };
        let ice = rtc
            .as_ref()
            .filter(|r| r.trickle)
            .map(|r| wire::IceCandidates::from_sdp(&r.sdp))
            .transpose()?;
        let s = inner
            .sessions
            .get_mut(id)
            .ok_or("terminal session expired")?;
        if s.operator != operator
            || s.node != node
            || &s.target != target
            || !s
                .ticket
                .as_ref()
                .is_some_and(|expected| bool::from(expected.as_bytes().ct_eq(ticket.as_bytes())))
        {
            return Err("terminal ticket is invalid or already used");
        }
        s.ticket = None;
        s.rtc = rtc.is_some();
        s.ice = ice;
        s.validated_at = Instant::now();
        let link = s.browser.take().ok_or("terminal is already attached")?;
        let offer = Offer {
            session_id: id.into(),
            target: s.target.clone(),
            owner: brocade_core::hash::sha256_hex(s.operator.as_bytes()),
            terminal_id: s.terminal_id.clone(),
            new_session: s.new_session,
            policy_revision: s.policy_revision,
        };
        inner
            .agents
            .get(node)
            .ok_or("terminal machine disconnected")?
            .offers
            .try_send(ConnectionOffer {
                terminal: offer,
                rtc,
            })
            .map_err(|_| "terminal machine is busy")?;
        Ok(link)
    }
    pub fn agent(&self, id: &str, node: &str) -> Result<Link, &'static str> {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.expire();
        let s = inner
            .sessions
            .get_mut(id)
            .ok_or("terminal session expired")?;
        if s.node != node || s.ticket.is_some() {
            return Err("terminal does not belong to this machine");
        }
        s.agent.take().ok_or("terminal agent already attached")
    }
    pub fn close_instance_sessions(&self, instance: &str) {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ids: Vec<_> = inner
            .sessions
            .iter()
            .filter(|(_, s)| s.target.instance_id == instance)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            inner.remove(&id);
        }
    }
    pub fn close_node_sessions(&self, node: &str) {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ids: Vec<_> = inner
            .sessions
            .iter()
            .filter(|(_, s)| s.node == node)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            inner.remove(&id)
        }
    }
    pub fn remove(&self, id: &str) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id)
    }
}

pub struct SessionGuard {
    pub service: Service,
    pub id: String,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.service.remove(&self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target() -> Target {
        Target {
            instance_id: "cvm-one".into(),
            runtime_id: "runtime-one".into(),
            restart_epoch: 0,
            reinstall_epoch: 0,
        }
    }
    #[test]
    fn tickets_are_scoped_single_use_and_incarnation_bound() {
        let s = Service::default();
        let (_, mut offers) = s.register("node").unwrap();
        let (id, ticket) = s.create("owner", "node", target()).unwrap();
        assert!(s.browser(&id, &ticket, "other", "node", &target()).is_err());
        let mut newer = target();
        newer.restart_epoch = 1;
        assert!(s.browser(&id, &ticket, "owner", "node", &newer).is_err());
        assert!(s.agent(&id, "node").is_err());
        let mut browser = s.browser(&id, &ticket, "owner", "node", &target()).unwrap();
        assert!(s.browser(&id, &ticket, "owner", "node", &target()).is_err());
        assert_eq!(offers.try_recv().unwrap().session_id, id);
        assert!(s.agent(&id, "other-node").is_err());
        let agent = s.agent(&id, "node").unwrap();
        assert!(s.agent(&id, "node").is_err());
        agent
            .send
            .try_send(Packet::Data(b"Guest output".to_vec()))
            .unwrap();
        assert!(matches!(browser.receive.try_recv(), Ok(Packet::Data(_))));
        s.remove(&id);
        assert!(*browser.cancelled.borrow());
    }
    #[test]
    fn disabling_a_node_cancels_active_and_pending_sessions_without_dropping_control() {
        let s = Service::default();
        let (_, _node_offers) = s.register("node").unwrap();
        let (_, _other_offers) = s.register("other").unwrap();
        let (id, ticket) = s.create("owner", "node", target()).unwrap();
        let browser = s.browser(&id, &ticket, "owner", "node", &target()).unwrap();
        let (pending, pending_ticket) = s.create("owner", "node", target()).unwrap();
        let (other, other_ticket) = s.create("other-owner", "other", target()).unwrap();
        s.close_node_sessions("node");
        assert!(*browser.cancelled.borrow());
        assert!(s
            .browser(&pending, &pending_ticket, "owner", "node", &target())
            .is_err());
        assert!(s
            .browser(&other, &other_ticket, "other-owner", "other", &target())
            .is_ok());
        assert!(s.create("owner", "node", target()).is_ok());
    }
    #[test]
    fn changing_instance_access_closes_only_that_instances_sessions() {
        let service = Service::default();
        let (_, _offers) = service.register("node").unwrap();
        let (id, ticket) = service.create("owner", "node", target()).unwrap();
        let browser = service
            .browser(&id, &ticket, "owner", "node", &target())
            .unwrap();
        let mut other = target();
        other.instance_id = "cvm-other".into();
        let (other_id, other_ticket) = service.create("owner", "node", other.clone()).unwrap();
        service.close_instance_sessions("cvm-one");
        assert!(*browser.cancelled.borrow());
        assert!(service
            .browser(&other_id, &other_ticket, "owner", "node", &other)
            .is_ok());
    }
    #[test]
    fn quotas_expiry_and_machine_replacement_cancel_sessions() {
        let s = Service::default();
        let (registration, _offers) = s.register("node").unwrap();
        for _ in 0..MAX_ATTACHMENTS_PER_NODE {
            s.create("owner", "node", target()).unwrap();
        }
        assert!(s.create("owner", "node", target()).is_err());
        {
            let mut inner = s.0.lock().unwrap();
            for session in inner.sessions.values_mut() {
                session.created = Instant::now() - Duration::from_secs(31)
            }
        }
        let (id, ticket) = s.create("owner", "node", target()).unwrap();
        let browser = s.browser(&id, &ticket, "owner", "node", &target()).unwrap();
        let (_new, _rx) = s.register("node").unwrap();
        assert!(*browser.cancelled.borrow());
        s.unregister("node", &registration);
        assert!(s.create("owner", "node", target()).is_ok());
    }
    #[test]
    fn one_operator_can_attach_across_nodes_without_an_operator_quota() {
        let s = Service::default();
        let (_, _a) = s.register("a").unwrap();
        let (_, _b) = s.register("b").unwrap();
        for _ in 0..MAX_ATTACHMENTS_PER_NODE {
            s.create("same-owner", "a", target()).unwrap();
            s.create("same-owner", "b", target()).unwrap();
        }
        assert!(s.create("another-owner", "a", target()).is_err());
    }
    #[test]
    fn rtc_offer_keeps_ticket_scope_and_only_uses_console_stun_configuration() {
        let s = Service::default();
        s.configure_stun(vec!["stun:controlled.example:3478".into()])
            .unwrap();
        let (_, mut offers) = s.register_with_capabilities("node", true, true).unwrap();
        assert!(s.rtc_settings("node").supported);
        let (id, ticket) = s.create("owner", "node", target()).unwrap();
        let sdp = "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n".to_owned();
        assert!(!s.rtc_lease_allowed(&id));
        assert!(s
            .browser_with_rtc(
                &id,
                "other",
                "node",
                &target(),
                &Attach {
                    ticket: ticket.clone(),
                    rtc_offer: Some(sdp.clone()),
                    trickle: false
                }
            )
            .is_err());
        let mut changed = target();
        changed.reinstall_epoch = 1;
        assert!(s
            .browser_with_rtc(
                &id,
                "owner",
                "node",
                &changed,
                &Attach {
                    ticket: ticket.clone(),
                    rtc_offer: Some(sdp.clone()),
                    trickle: false
                }
            )
            .is_err());
        let browser = s
            .browser_with_rtc(
                &id,
                "owner",
                "node",
                &target(),
                &Attach {
                    ticket: ticket.clone(),
                    rtc_offer: Some(sdp),
                    trickle: false,
                },
            )
            .unwrap();
        let offer = offers.try_recv().unwrap();
        assert_eq!(
            offer.rtc.unwrap().stun_servers,
            vec!["stun:controlled.example:3478"]
        );
        assert!(!s.rtc_lease_allowed(&id));
        s.browser_seen(&id);
        assert!(s.rtc_lease_allowed(&id));
        s.0.lock()
            .unwrap()
            .sessions
            .get_mut(&id)
            .unwrap()
            .validated_at = Instant::now() - Duration::from_secs(3);
        assert!(!s.rtc_lease_allowed(&id));
        s.validated(&id);
        assert!(s.rtc_lease_allowed(&id));
        s.0.lock()
            .unwrap()
            .sessions
            .get_mut(&id)
            .unwrap()
            .browser_seen_at = Some(Instant::now() - Duration::from_secs(3));
        assert!(!s.rtc_lease_allowed(&id));
        s.close_instance_sessions("cvm-one");
        assert!(*browser.cancelled.borrow());
        assert!(!s.rtc_lease_allowed(&id));
    }
    #[test]
    fn latency_capability_requires_a_live_registration_and_is_independent_of_rtc() {
        let s = Service::default();
        let (_, legacy) = s.register("node").unwrap();
        assert!(!s.latency_supported("node"));
        let (_, current) = s.register_with_capabilities("node", false, true).unwrap();
        assert!(s.latency_supported("node"));
        assert!(!s.rtc_settings("node").supported);
        drop(legacy);
        assert!(s.latency_supported("node"));
        drop(current);
        assert!(!s.latency_supported("node"));
    }
    #[test]
    fn trickle_negotiation_keeps_candidates_scoped_to_the_authorized_attachment() {
        let s = Service::default();
        let (_, mut offers) = s
            .register_with_features(
                "node",
                AgentFeatures {
                    rtc: true,
                    trickle: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(s.rtc_settings("node").trickle);
        let (id, ticket) = s.create("owner", "node", target()).unwrap();
        let sdp = "v=0\r\na=ice-ufrag:abcd\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n"
            .to_owned();
        assert!(s
            .browser_with_rtc(
                &id,
                "other",
                "node",
                &target(),
                &Attach {
                    ticket: ticket.clone(),
                    rtc_offer: Some(sdp.clone()),
                    trickle: true
                }
            )
            .is_err());
        let _browser = s
            .browser_with_rtc(
                &id,
                "owner",
                "node",
                &target(),
                &Attach {
                    ticket: ticket.clone(),
                    rtc_offer: Some(sdp),
                    trickle: true,
                },
            )
            .unwrap();
        assert!(offers.try_recv().unwrap().rtc.unwrap().trickle);
        let candidate = wire::IceSignal::IceCandidate {
            candidate: "candidate:1 1 udp 2113929471 192.0.2.1 30000 typ host".into(),
            username_fragment: "abcd".into(),
        };
        assert!(s
            .accept_ice_signal("another-attachment", &candidate)
            .is_err());
        assert!(s.accept_ice_signal(&id, &candidate).unwrap());
        assert!(!s.accept_ice_signal(&id, &candidate).unwrap());
        assert!(s
            .accept_ice_signal(
                &id,
                &wire::IceSignal::IceComplete {
                    username_fragment: "old1".into()
                }
            )
            .is_err());
        assert!(s
            .accept_ice_signal(
                &id,
                &wire::IceSignal::IceComplete {
                    username_fragment: "abcd".into()
                }
            )
            .unwrap());
        s.close_instance_sessions("cvm-one");
        assert!(s.accept_ice_signal(&id, &candidate).is_err());
    }
    #[test]
    fn legacy_agents_reject_trickle_without_consuming_the_ticket() {
        let s = Service::default();
        let (_, mut offers) = s.register_with_capabilities("node", true, true).unwrap();
        assert!(!s.rtc_settings("node").trickle);
        let (id, ticket) = s.create("owner", "node", target()).unwrap();
        let sdp = "v=0\r\na=ice-ufrag:abcd\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n"
            .to_owned();
        assert!(s
            .browser_with_rtc(
                &id,
                "owner",
                "node",
                &target(),
                &Attach {
                    ticket: ticket.clone(),
                    rtc_offer: Some(sdp.clone()),
                    trickle: true
                }
            )
            .is_err());
        let _browser = s
            .browser_with_rtc(
                &id,
                "owner",
                "node",
                &target(),
                &Attach {
                    ticket: ticket.clone(),
                    rtc_offer: Some(sdp),
                    trickle: false,
                },
            )
            .unwrap();
        assert!(!offers.try_recv().unwrap().rtc.unwrap().trickle);
        assert!(s
            .accept_ice_signal(
                &id,
                &wire::IceSignal::IceComplete {
                    username_fragment: "abcd".into()
                }
            )
            .is_err());
    }
    #[test]
    fn unsupported_or_invalid_rtc_does_not_consume_the_relay_ticket() {
        let s = Service::default();
        let (_, mut offers) = s.register("node").unwrap();
        let (id, ticket) = s.create("owner", "node", target()).unwrap();
        assert!(!s.rtc_settings("node").supported);
        assert!(s
            .browser_with_rtc(
                &id,
                "owner",
                "node",
                &target(),
                &Attach {
                    ticket: ticket.clone(),
                    rtc_offer: Some("bad".into()),
                    trickle: false
                }
            )
            .is_err());
        s.browser(&id, &ticket, "owner", "node", &target()).unwrap();
        assert!(offers.try_recv().unwrap().rtc.is_none());
        assert!(!s.rtc_lease_allowed(&id));
        assert!(s.configure_stun(vec!["turn:example.com".into()]).is_err());
        assert!(s
            .configure_stun(vec!["stun:example.com".into(); 3])
            .is_err());
    }
}
