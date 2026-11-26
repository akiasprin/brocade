//! Bounded, DataChannel-only WebRTC transport. SDP arrives through the existing
//! authenticated Console rendezvous; no HTTP listener or browser CA certificate.
use brocade_deployment::cvm::terminal as wire;
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use str0m::channel::{ChannelId, Reliability};
use str0m::{Candidate, Event, Input, Output, Rtc};
use tokio::net::UdpSocket;

#[cfg(test)]
mod browser;
mod stun;

#[derive(Clone, Debug)]
pub(super) struct Config {
    bind: SocketAddr,
}

impl Config {
    pub fn from_env() -> Result<Option<Self>, String> {
        match std::env::var("BROCADE_AGENT_TERMINAL_WEBRTC_BIND") {
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err("terminal WebRTC bind must be UTF-8".into()),
            Ok(value) => Self::parse(&value).map(Some),
        }
    }
    fn parse(value: &str) -> Result<Self, String> {
        let bind: SocketAddr = value.parse().map_err(|_| "invalid terminal WebRTC bind")?;
        if bind.ip().is_unspecified()
            || bind.ip().is_multicast()
            || bind
                .port()
                .checked_add(wire::MAX_ATTACHMENTS_PER_NODE as u16 - 1)
                .is_none()
        {
            return Err(
                "terminal WebRTC requires one explicit local IP and a bounded port pool".into(),
            );
        }
        Ok(Self { bind })
    }
    async fn socket(&self) -> Result<UdpSocket, String> {
        for offset in 0..wire::MAX_ATTACHMENTS_PER_NODE as u16 {
            let mut addr = self.bind;
            if addr.port() != 0 {
                addr.set_port(addr.port() + offset);
            }
            match UdpSocket::bind(addr).await {
                Ok(socket) => return Ok(socket),
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                Err(_) => return Err("terminal WebRTC UDP bind failed".into()),
            }
        }
        Err("terminal WebRTC UDP port pool exhausted".into())
    }
}

pub(super) enum PeerEvent {
    Open,
    Data { binary: bool, data: Vec<u8> },
    Closed,
}

pub(super) struct Peer {
    rtc: Rtc,
    socket: UdpSocket,
    local: SocketAddr,
    channel: Option<ChannelId>,
    input: Vec<u8>,
    ice: Option<wire::IceCandidates>,
    mdns_reported: bool,
}

impl Peer {
    pub async fn answer(config: &Config, offer: &wire::RtcOffer) -> Result<(Self, String), String> {
        if !wire::valid_sdp(&offer.sdp) || offer.stun_servers.len() > 2 {
            return Err("invalid terminal WebRTC offer".into());
        }
        let ice = offer
            .trickle
            .then(|| wire::IceCandidates::from_sdp(&offer.sdp))
            .transpose()
            .map_err(str::to_owned)?;
        let socket = config.socket().await?;
        let local = socket
            .local_addr()
            .map_err(|_| "terminal WebRTC socket unavailable")?;
        let mut rtc = Rtc::builder()
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
            .set_fingerprint_verification(true)
            .set_sctp_max_message_size(wire::MAX_DATA as u32)
            .set_sctp_max_buffered_amount(64 * 1024)
            .build(Instant::now());
        rtc.add_local_candidate(
            Candidate::host(local, "udp").map_err(|_| "invalid terminal ICE address")?,
        )
        .ok_or("terminal ICE candidate failed")?;
        for server in &offer.stun_servers {
            match stun::discover(&socket, server).await {
                Ok(address) if address != local => {
                    let candidate = Candidate::server_reflexive(address, local, "udp")
                        .map_err(|_| "invalid STUN mapped address")?;
                    rtc.add_local_candidate(candidate)
                        .ok_or("terminal ICE candidate failed")?;
                }
                Ok(_) => {}
                Err(_) => {
                    eprintln!("terminal STUN discovery unavailable; retaining local ICE candidate")
                }
            }
        }
        let sdp = str0m::change::SdpOffer::from_sdp_string(&offer.sdp)
            .map_err(|_| "invalid terminal SDP")?;
        let mut answer = rtc
            .sdp_api()
            .accept_offer(sdp)
            .map_err(|_| "terminal SDP negotiation failed")?
            .to_sdp_string();
        // This single-interface responder finishes discovery before answering.
        // Browser candidates may still arrive incrementally over authenticated WSS.
        if offer.trickle && !answer.lines().any(|line| line == "a=end-of-candidates") {
            answer.push_str("a=end-of-candidates\r\n");
        }
        if answer.len() > wire::MAX_SDP {
            return Err("terminal SDP answer exceeds limit".into());
        }
        Ok((
            Self {
                rtc,
                socket,
                local,
                channel: None,
                input: vec![0; 2048],
                ice,
                mdns_reported: false,
            },
            answer,
        ))
    }

    pub fn add_remote_candidate(&mut self, signal: &wire::IceSignal) -> Result<(), String> {
        let ice = self.ice.as_mut().ok_or("trickle ICE was not negotiated")?;
        if !ice.accept(signal).map_err(str::to_owned)? {
            return Ok(());
        }
        if let wire::IceSignal::IceCandidate {
            candidate,
            username_fragment,
        } = signal
        {
            let fields: Vec<_> = candidate.split_ascii_whitespace().collect();
            if fields
                .get(4)
                .is_some_and(|address| address.ends_with(".local"))
                && fields.get(6) == Some(&"typ")
                && fields.get(7) == Some(&"host")
            {
                // The ICE library accepts IP candidates, just as its SDP parser does.
                // Browser mDNS names are not globally resolvable; preserve other
                // candidates and the browser-initiated peer-reflexive check path.
                if !self.mdns_reported {
                    eprintln!("terminal ICE mDNS host candidate skipped; awaiting IP or peer-reflexive checks");
                    self.mdns_reported = true;
                }
                return Ok(());
            }
            let mut candidate = Candidate::from_sdp_string(candidate)
                .map_err(|_| "invalid remote ICE candidate")?;
            if candidate
                .ufrag()
                .is_some_and(|ufrag| ufrag != username_fragment)
            {
                return Err("stale candidate ICE generation".into());
            }
            candidate.set_ufrag(username_fragment);
            self.rtc.add_remote_candidate(candidate);
        }
        Ok(())
    }

    pub fn write(&mut self, binary: bool, data: &[u8]) -> Result<bool, String> {
        if data.len() > wire::MAX_DATA {
            return Err("terminal RTC frame exceeds limit".into());
        }
        self.rtc
            .channel(self.channel.ok_or("terminal RTC channel not open")?)
            .ok_or("terminal RTC channel closed")?
            .write(binary, data)
            .map_err(|_| "terminal RTC write failed".into())
    }

    /// Cancelling this future is safe: buffered DTLS/SCTP state stays inside Peer.
    pub async fn next(&mut self) -> Result<PeerEvent, String> {
        loop {
            let timeout = loop {
                match self
                    .rtc
                    .poll_output()
                    .map_err(|_| "terminal RTC protocol failed")?
                {
                    Output::Transmit(packet) => {
                        tokio::time::timeout(
                            Duration::from_secs(1),
                            self.socket.send_to(&packet.contents, packet.destination),
                        )
                        .await
                        .map_err(|_| "terminal RTC UDP write timed out")?
                        .map_err(|_| "terminal RTC UDP write failed")?;
                    }
                    Output::Timeout(deadline) => break deadline,
                    Output::Event(Event::ChannelOpen(id, label)) => {
                        let channel = self.rtc.channel(id).ok_or("terminal RTC channel missing")?;
                        let config = channel
                            .config()
                            .ok_or("terminal RTC channel config missing")?;
                        if self.channel.is_some()
                            || label != wire::DATA_CHANNEL_LABEL
                            || !config.ordered
                            || config.reliability != Reliability::Reliable
                        {
                            return Err(
                                "terminal requires one reliable ordered data channel".into()
                            );
                        }
                        self.channel = Some(id);
                        return Ok(PeerEvent::Open);
                    }
                    Output::Event(Event::ChannelData(data)) => {
                        if Some(data.id) != self.channel
                            || data.data.is_empty()
                            || data.data.len() > wire::MAX_DATA
                        {
                            return Err("invalid terminal RTC frame".into());
                        }
                        return Ok(PeerEvent::Data {
                            binary: data.binary,
                            data: data.data,
                        });
                    }
                    Output::Event(Event::ChannelClose(_) | Event::Closed) => {
                        return Ok(PeerEvent::Closed)
                    }
                    Output::Event(_) => {}
                }
            };
            if !self.rtc.is_alive() {
                return Ok(PeerEvent::Closed);
            }
            tokio::select! {
                _=tokio::time::sleep_until(timeout.into()) => {
                    self.rtc.handle_input(Input::Timeout(Instant::now())).map_err(|_| "terminal RTC timer failed")?;
                },
                packet=self.socket.recv_from(&mut self.input) => {
                    let (size,source)=packet.map_err(|_| "terminal RTC UDP receive failed")?;
                    let Ok(contents)=self.input[..size].try_into() else {continue};
                    let packet=Input::Receive(Instant::now(),str0m::net::Receive {
                        proto:str0m::net::Protocol::Udp,source,destination:self.local,contents,
                    });
                    if self.rtc.accepts(&packet) {
                        self.rtc.handle_input(packet).map_err(|_| "terminal RTC input failed")?;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rtc_bind_requires_explicit_address_and_bounded_pool() {
        assert!(Config::parse("0.0.0.0:30000").is_err());
        assert!(Config::parse("[::]:30000").is_err());
        assert!(Config::parse("192.0.2.1:65535").is_err());
        assert!(Config::parse("192.0.2.1:30000").is_ok());
        assert!(Config::parse("[2001:db8::1]:30000").is_ok());
    }
}
