//! Minimal RFC 5389 binding discovery, not a TURN client or a STUN server.
use brocade_deployment::cvm::terminal::stun_endpoint;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};
use tokio::net::UdpSocket;

const COOKIE: [u8; 4] = 0x2112_A442u32.to_be_bytes();

pub(super) async fn discover(socket: &UdpSocket, server: &str) -> Result<SocketAddr, String> {
    let (host, port) = stun_endpoint(server).map_err(str::to_owned)?;
    let local = socket
        .local_addr()
        .map_err(|_| "STUN local socket unavailable")?;
    let addresses = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    .map_err(|_| "STUN DNS timed out")?
    .map_err(|_| "STUN DNS failed")?;
    let target = addresses
        .take(8)
        .find(|a| {
            a.is_ipv4() == local.is_ipv4() && !a.ip().is_unspecified() && !a.ip().is_multicast()
        })
        .ok_or("STUN server has no matching address family")?;
    let mut request = [0u8; 20];
    request[..2].copy_from_slice(&1u16.to_be_bytes());
    request[4..8].copy_from_slice(&COOKIE);
    getrandom::fill(&mut request[8..]).map_err(|_| "STUN entropy unavailable")?;
    let mut data = [0u8; 1024];
    for wait in [Duration::from_millis(300), Duration::from_millis(700)] {
        socket
            .send_to(&request, target)
            .await
            .map_err(|_| "STUN request failed")?;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let Ok(packet) = tokio::time::timeout_at(deadline, socket.recv_from(&mut data)).await
            else {
                break;
            };
            let (size, source) = packet.map_err(|_| "STUN receive failed")?;
            if source == target {
                if let Some(address) = response(&data[..size], &request[8..20]) {
                    return Ok(address);
                }
            }
        }
    }
    Err("STUN binding timed out".into())
}

fn response(data: &[u8], transaction: &[u8]) -> Option<SocketAddr> {
    if data.len() < 20 || data[..2] != [1, 1] || data[4..8] != COOKIE || &data[8..20] != transaction
    {
        return None;
    }
    let length = u16::from_be_bytes([data[2], data[3]]) as usize;
    if !length.is_multiple_of(4) || data.len() != 20 + length {
        return None;
    }
    let mut at = 20;
    while at + 4 <= data.len() {
        let kind = u16::from_be_bytes([data[at], data[at + 1]]);
        let len = u16::from_be_bytes([data[at + 2], data[at + 3]]) as usize;
        at += 4;
        let value = data.get(at..at.checked_add(len)?)?;
        if kind == 0x0020 {
            if value.len() < 4 || value[0] != 0 {
                return None;
            }
            let port = u16::from_be_bytes([value[2] ^ COOKIE[0], value[3] ^ COOKIE[1]]);
            let ip = match (value[1], value.len()) {
                (1, 8) => IpAddr::V4(Ipv4Addr::new(
                    value[4] ^ COOKIE[0],
                    value[5] ^ COOKIE[1],
                    value[6] ^ COOKIE[2],
                    value[7] ^ COOKIE[3],
                )),
                (2, 20) => {
                    let mut ip = [0u8; 16];
                    for i in 0..16 {
                        ip[i] = value[i + 4] ^ if i < 4 { COOKIE[i] } else { transaction[i - 4] };
                    }
                    IpAddr::V6(Ipv6Addr::from(ip))
                }
                _ => return None,
            };
            if port == 0 || ip.is_unspecified() || ip.is_multicast() {
                return None;
            }
            return Some(SocketAddr::new(ip, port));
        }
        at = at.checked_add((len + 3) & !3)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binding_response_is_transaction_and_length_bound() {
        let id = [7u8; 12];
        let mut data = vec![1, 1, 0, 12];
        data.extend(COOKIE);
        data.extend(id);
        data.extend([
            0,
            32,
            0,
            8,
            0,
            1,
            0x21 ^ 0x30,
            0x12 ^ 0x39,
            192 ^ COOKIE[0],
            COOKIE[1],
            2 ^ COOKIE[2],
            1 ^ COOKIE[3],
        ]);
        assert_eq!(
            response(&data, &id),
            Some("192.0.2.1:12345".parse().unwrap())
        );
        assert_eq!(response(&data, &[8u8; 12]), None);
        data.pop();
        assert_eq!(response(&data, &id), None);
    }
    #[tokio::test]
    async fn controlled_stun_binding_ignores_wrong_transaction() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let serving = tokio::spawn(async move {
            let mut data = [0u8; 64];
            let (size, peer) = server.recv_from(&mut data).await.unwrap();
            assert_eq!(size, 20);
            let mut reply = vec![1, 1, 0, 12];
            reply.extend(COOKIE);
            reply.extend_from_slice(&data[8..20]);
            let port = peer.port().to_be_bytes();
            reply.extend([
                0,
                32,
                0,
                8,
                0,
                1,
                port[0] ^ COOKIE[0],
                port[1] ^ COOKIE[1],
                127 ^ COOKIE[0],
                COOKIE[1],
                COOKIE[2],
                1 ^ COOKIE[3],
            ]);
            reply[8] ^= 1;
            server.send_to(&reply, peer).await.unwrap();
            reply[8] ^= 1;
            server.send_to(&reply, peer).await.unwrap();
        });
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(
            discover(&socket, &format!("stun:{address}")).await.unwrap(),
            socket.local_addr().unwrap()
        );
        serving.await.unwrap();
    }
    #[test]
    fn ipv6_xor_mapping_uses_cookie_and_transaction() {
        let id = [9u8; 12];
        let address: Ipv6Addr = "2001:db8::1234".parse().unwrap();
        let mut data = vec![1, 1, 0, 24];
        data.extend(COOKIE);
        data.extend(id);
        data.extend([0, 32, 0, 20, 0, 2, COOKIE[0] ^ 0x30, COOKIE[1] ^ 0x39]);
        for (i, byte) in address.octets().iter().enumerate() {
            data.push(byte ^ if i < 4 { COOKIE[i] } else { id[i - 4] });
        }
        assert_eq!(
            response(&data, &id),
            Some(SocketAddr::new(IpAddr::V6(address), 12345))
        );
    }
    #[tokio::test]
    #[ignore = "requires public UDP access to Cloudflare STUN"]
    async fn cloudflare_stun_binding() {
        let socket = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let address = discover(&socket, "stun:stun.cloudflare.com:3478")
            .await
            .unwrap();
        assert!(address.is_ipv4() && !address.ip().is_unspecified() && address.port() != 0);
        println!("Cloudflare STUN binding succeeded (IPv4; mapped address withheld)");
    }
}
