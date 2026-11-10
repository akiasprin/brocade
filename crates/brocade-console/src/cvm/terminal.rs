//! Bounded process-local rendezvous. Tickets and terminal contents are never persisted.
use brocade_deployment::cvm::terminal::{Offer, Target, MAX_ATTACHMENTS_PER_NODE, MAX_SESSIONS};
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
}
struct Agent {
    id: String,
    offers: mpsc::Sender<Offer>,
}
#[derive(Default)]
struct Inner {
    sessions: HashMap<String, Session>,
    agents: HashMap<String, Agent>,
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
    pub fn expire(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .expire()
    }
    pub fn register(&self, node: &str) -> Result<(String, mpsc::Receiver<Offer>), &'static str> {
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
            },
        );
        Ok((id, ticket))
    }
    pub fn browser(
        &self,
        id: &str,
        ticket: &str,
        operator: &str,
        node: &str,
        target: &Target,
    ) -> Result<Link, &'static str> {
        let mut inner = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.expire();
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
            .try_send(offer)
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
}
