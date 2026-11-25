//! One selected data transport owns one local terminal attachment. A failed RTC
//! connection detaches; the browser recovers the same Shell through a new relay
//! attachment. Neither handshake fallback nor recovery replays keyboard input.
use super::{
    connect, flush,
    rtc::{Config, Peer, PeerEvent},
    send,
};
use crate::options::Options;
use brocade_cube::{TerminalCommand, TerminalEvent, TerminalSession};
use brocade_deployment::cvm::terminal as wire;
use futures_util::SinkExt;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use yawc::{Frame, OpCode};

struct Lease {
    expires: Instant,
    confirmed: bool,
    sequence: u64,
    pending: VecDeque<(u64, Instant)>,
}
impl Lease {
    fn new(now: Instant) -> Self {
        Self {
            expires: now + Duration::from_secs(wire::AGENT_LEASE_SECONDS),
            confirmed: false,
            sequence: 0,
            pending: VecDeque::new(),
        }
    }
    fn probe(&mut self, now: Instant) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.pending.retain(|(_, sent)| {
            now.duration_since(*sent) < Duration::from_secs(wire::LEASE_SECONDS)
        });
        if self.pending.len() >= 8 {
            self.pending.pop_front();
        }
        self.pending.push_back((self.sequence, now));
        self.sequence
    }
    fn grant(&mut self, nonce: u64, now: Instant) -> bool {
        let Some(at) = self.pending.iter().position(|(n, _)| *n == nonce) else {
            return false;
        };
        let Some((_, sent)) = self.pending.remove(at) else {
            return false;
        };
        let deadline = sent + Duration::from_secs(wire::AGENT_LEASE_SECONDS);
        if now >= deadline {
            return false;
        }
        // Anchor expiry to the outgoing request. Delayed/replayed responses
        // cannot restart a lease when they finally arrive.
        self.expires = self.expires.max(deadline);
        self.confirmed = true;
        true
    }
}

pub(super) async fn session(
    options: &Options,
    offer: wire::Offer,
    rtc_offer: wire::RtcOffer,
    config: &Config,
) {
    let path = format!("/agent/v1/cvm/terminal/{}/stream", offer.session_id);
    let Ok(mut socket) = connect(options, &path, false).await else {
        return;
    };
    let prepared =
        tokio::time::timeout(Duration::from_secs(5), Peer::answer(config, &rtc_offer)).await;
    let mut peer = match prepared {
        Ok(Ok((peer, sdp))) => {
            let Ok(text) = serde_json::to_string(&wire::Output::RtcAnswer { sdp }) else {
                return;
            };
            if send(&mut socket, Frame::text(text)).await.is_err() {
                return;
            }
            Some(peer)
        }
        _ => {
            eprintln!("terminal RTC setup unavailable; using Console relay");
            None
        }
    };
    let mut lease = Lease::new(Instant::now());
    let mut renew = tokio::time::interval(Duration::from_secs(1));
    renew.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let negotiation = Instant::now() + Duration::from_secs(wire::RTC_CONNECT_SECONDS);
    let lifetime = Instant::now() + Duration::from_secs(wire::MAX_SECONDS);
    let mut open = false;
    let mut selected: Option<bool> = None;
    let mut terminal: Option<TerminalSession> = None;
    let mut pending: Option<(bool, Vec<u8>)> = None;
    let mut ending: Option<Instant> = None;
    loop {
        let now = Instant::now();
        if now >= lease.expires || now >= lifetime || ending.is_some_and(|end| now >= end) {
            break;
        }
        if selected.is_none() && lease.confirmed && (open || peer.is_none() || now >= negotiation) {
            selected = Some(open);
            let value = wire::Output::Transport {
                kind: if open {
                    wire::TransportKind::Webrtc
                } else {
                    wire::TransportKind::Console
                },
                reason: (!open).then(|| "direct_unavailable".into()),
            };
            let Ok(text) = serde_json::to_string(&value) else {
                break;
            };
            if open {
                if !matches!(
                    peer.as_mut()
                        .and_then(|p| p.write(false, text.as_bytes()).ok()),
                    Some(true)
                ) {
                    break;
                }
            } else {
                peer = None;
                if lease_send(&mut socket, Frame::text(text), lease.expires)
                    .await
                    .is_err()
                {
                    break;
                }
            }
            let Some(backend) = super::super::backend::current() else {
                break;
            };
            let target = offer.clone();
            match tokio::time::timeout_at(
                lease.expires.into(),
                tokio::task::spawn_blocking(move || backend.persistent_terminal(target)),
            )
            .await
            {
                Ok(Ok(Ok(value))) => terminal = Some(value),
                Err(_) => break,
                _ => {
                    let value = wire::Output::Error {
                        message: "Guest 终端不可用，请确认实例状态后重试".into(),
                    };
                    if let Ok(text) = serde_json::to_string(&value) {
                        let _ = lease_send(&mut socket, Frame::text(text), lease.expires).await;
                    }
                    break;
                }
            }
        }
        if let Some((binary, data)) = pending.as_ref() {
            if selected == Some(true) {
                match peer.as_mut().map(|p| p.write(*binary, data)) {
                    Some(Ok(true)) => pending = None,
                    Some(Ok(false)) => {}
                    _ => break,
                }
            } else {
                let frame = if *binary {
                    Frame::binary(data.clone())
                } else {
                    Frame::text(data.clone())
                };
                if lease_send(&mut socket, frame, lease.expires).await.is_err() {
                    break;
                }
                pending = None;
            }
        }
        tokio::select! {
            _=tokio::time::sleep_until(lease.expires.into())=>break,
            _=tokio::time::sleep_until(lifetime.into())=>break,
            _=tokio::time::sleep_until(negotiation.into()),if selected.is_none() && Instant::now()<negotiation=>{},
            _=renew.tick()=>{
                let value=wire::AgentControl::LeaseProbe{nonce:lease.probe(Instant::now())};
                let Ok(text)=serde_json::to_string(&value) else{break};
                if lease_send(&mut socket,Frame::text(text),lease.expires).await.is_err(){break}
            },
            frame=socket.next_frame()=>{
                let Ok(frame)=frame else{break};
                match frame.opcode() {
                    OpCode::Ping=>if !matches!(tokio::time::timeout_at(lease.expires.into(),flush(&mut socket)).await,Ok(Ok(()))){break},
                    OpCode::Pong=>{},
                    OpCode::Close=>break,
                    OpCode::Text if frame.payload().len()<=wire::MAX_CONTROL=>{
                        if let Ok(control)=serde_json::from_slice::<wire::AgentControl>(frame.payload()) {
                            match control {wire::AgentControl::LeaseGrant{nonce}=>{lease.grant(nonce,Instant::now());},_=>break}
                        } else if selected==Some(false) && lease.confirmed {
                            if forward(&terminal,false,frame.payload(),lease.expires).await.is_err(){break}
                        } else {break}
                    },
                    OpCode::Binary if selected==Some(false) && lease.confirmed=>{
                        if forward(&terminal,true,frame.payload(),lease.expires).await.is_err(){break}
                    },
                    _=>break,
                }
            },
            event=async {match peer.as_mut(){Some(peer)=>peer.next().await,None=>std::future::pending().await}}=>{
                match event {
                    Ok(PeerEvent::Open)=>open=true,
                    Ok(PeerEvent::Data{binary,data}) if selected==Some(true) && lease.confirmed=>{
                        if forward(&terminal,binary,&data,lease.expires).await.is_err(){break}
                    },
                    Ok(PeerEvent::Data{..})=>break,
                    Ok(PeerEvent::Closed)|Err(_) if selected.is_none()=>{peer=None;open=false;},
                    _=>break,
                }
            },
            event=async{match terminal.as_mut(){Some(terminal)=>terminal.output.recv().await,None=>std::future::pending().await}},if pending.is_none() && ending.is_none()=>{
                match event {
                    Some(TerminalEvent::Data(data))=>pending=Some((true,data)),
                    Some(TerminalEvent::Output(value))=>{
                        if matches!(value,wire::Output::Exit{..}|wire::Output::Error{..}) {ending=Some(Instant::now()+Duration::from_secs(30));}
                        let Ok(text)=serde_json::to_vec(&value) else{break};
                        pending=Some((false,text));
                    },
                    None=>break,
                }
            },
        }
    }
    // Detach before a potentially blocked close handshake can delay cleanup.
    drop(terminal);
    drop(peer);
    // A transport loss only drops the attachment. Termination is exclusively
    // the explicit Input::Close command or the existing lifecycle/policy path.
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        socket.send(Frame::close(
            yawc::close::CloseCode::Normal,
            b"terminal detached",
        )),
    )
    .await;
}

async fn lease_send(
    socket: &mut yawc::WebSocket<crate::http::Stream>,
    frame: Frame,
    expires: Instant,
) -> Result<(), String> {
    tokio::time::timeout_at(expires.into(), send(socket, frame))
        .await
        .map_err(|_| "terminal authorization lease expired")?
}

async fn forward(
    terminal: &Option<TerminalSession>,
    binary: bool,
    data: &[u8],
    expires: Instant,
) -> Result<(), String> {
    if data.is_empty() || data.len() > wire::MAX_DATA || Instant::now() >= expires {
        return Err("invalid or expired terminal input".into());
    }
    let command = if binary {
        TerminalCommand::Data(data.to_vec())
    } else {
        if data.len() > wire::MAX_CONTROL {
            return Err("terminal control exceeds limit".into());
        }
        let value: wire::Input =
            serde_json::from_slice(data).map_err(|_| "invalid terminal control")?;
        if !value.valid() {
            return Err("invalid terminal control".into());
        }
        TerminalCommand::Control(value)
    };
    tokio::time::timeout_at(
        expires.into(),
        terminal
            .as_ref()
            .ok_or("terminal not attached")?
            .input
            .send(command),
    )
    .await
    .map_err(|_| "terminal lease expired during backpressure")?
    .map_err(|_| "terminal closed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_and_replayed_grants_cannot_extend_the_request_deadline() {
        let now = Instant::now();
        let mut lease = Lease::new(now);
        let nonce = lease.probe(now);
        assert!(!lease.grant(nonce + 1, now));
        assert!(lease.grant(nonce, now + Duration::from_secs(2)));
        assert_eq!(lease.expires, now + Duration::from_secs(3));
        assert!(!lease.grant(nonce, now + Duration::from_secs(2)));
        let nonce = lease.probe(now + Duration::from_secs(1));
        assert!(!lease.grant(nonce, now + Duration::from_secs(6)));
    }
}
