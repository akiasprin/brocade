//! System DNS cannot be canceled once getaddrinfo starts. Keep its permit in the worker,
//! including after the HTTP future or WebSocket caller has stopped waiting. No waiting queue.
use super::{remaining, AddressFamily};
use std::{
    io,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, OnceLock,
    },
    thread,
    time::Instant,
};

// Unlock detection may run five service workers; keep room for other optional probes.
const PUBLIC_DNS_LOOKUP_LIMIT: usize = 8;

/// The lookup succeeded, but its addresses cannot serve an IPv6-only request.
/// Keep this distinct from DNS failures and an exhausted resolver budget.
#[derive(Debug)]
pub(super) struct NoIpv6Addresses;
impl std::fmt::Display for NoIpv6Addresses {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DNS returned no IPv6 addresses")
    }
}
impl std::error::Error for NoIpv6Addresses {}

#[derive(Debug)]
struct Pool {
    active: AtomicUsize,
    maximum: usize,
}
struct Permit(Arc<Pool>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Pool {
    fn start(
        self: &Arc<Self>,
        lookup: impl FnOnce() -> io::Result<Vec<SocketAddr>> + Send + 'static,
        publish: impl FnOnce(io::Result<Vec<SocketAddr>>) + Send + 'static,
    ) -> io::Result<()> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.maximum).then_some(active + 1)
            })
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "DNS capacity exhausted"))?;
        let permit = Permit(self.clone());
        thread::Builder::new()
            .name("agent-dns".to_owned())
            .spawn(move || {
                let result = lookup();
                drop(permit);
                publish(result);
            })?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(super) struct BoundedResolver {
    pool: Arc<Pool>,
    family: Option<AddressFamily>,
}
impl BoundedResolver {
    fn new(maximum: usize) -> Self {
        Self {
            pool: Arc::new(Pool {
                active: AtomicUsize::new(0),
                maximum,
            }),
            family: None,
        }
    }
    pub(super) fn with_family(&self, family: Option<AddressFamily>) -> Self {
        Self {
            pool: self.pool.clone(),
            family,
        }
    }
    pub(super) fn resolve_blocking(
        &self,
        host: &str,
        port: u16,
        deadline: Instant,
    ) -> io::Result<Vec<SocketAddr>> {
        remaining(deadline)?;
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        let (send, receive) = mpsc::sync_channel(1);
        let host = host.to_owned();
        self.pool.start(
            move || system_lookup(&host),
            move |result| {
                let _ = send.send(result);
            },
        )?;
        let mut addresses = receive
            .recv_timeout(remaining(deadline)?)
            .map_err(|error| {
                io::Error::new(
                    match error {
                        mpsc::RecvTimeoutError::Timeout => io::ErrorKind::TimedOut,
                        mpsc::RecvTimeoutError::Disconnected => io::ErrorKind::BrokenPipe,
                    },
                    "DNS lookup did not complete",
                )
            })??;
        for address in &mut addresses {
            address.set_port(port);
        }
        Ok(addresses)
    }
}
fn system_lookup(host: &str) -> io::Result<Vec<SocketAddr>> {
    let addresses: Vec<_> = (host, 0).to_socket_addrs()?.collect();
    if addresses.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "DNS returned no addresses",
        ));
    }
    Ok(addresses)
}
impl reqwest::dns::Resolve for BoundedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let pool = self.pool.clone();
        let family = self.family;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let (send, receive) = tokio::sync::oneshot::channel();
            pool.start(
                move || system_lookup(&host),
                move |result| {
                    let _ = send.send(result);
                },
            )?;
            let addresses = receive
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "DNS worker ended"))??;
            let addresses: Vec<_> = addresses
                .into_iter()
                .filter(|address| family.is_none_or(|value| value.accepts(address)))
                .collect();
            if addresses.is_empty() {
                if family == Some(AddressFamily::V6) {
                    return Err(io::Error::new(io::ErrorKind::NotFound, NoIpv6Addresses).into());
                }
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "DNS returned no matching addresses",
                )
                .into());
            }
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}
pub(super) fn control() -> Arc<BoundedResolver> {
    static RESOLVER: OnceLock<Arc<BoundedResolver>> = OnceLock::new();
    RESOLVER
        .get_or_init(|| Arc::new(BoundedResolver::new(200)))
        .clone()
}
pub(super) fn public() -> Arc<BoundedResolver> {
    static RESOLVER: OnceLock<Arc<BoundedResolver>> = OnceLock::new();
    RESOLVER
        .get_or_init(|| Arc::new(BoundedResolver::new(PUBLIC_DNS_LOOKUP_LIMIT)))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn public_dns_admits_all_negotiated_unlock_workers() {
        let resolver = BoundedResolver::new(PUBLIC_DNS_LOOKUP_LIMIT);
        let workers = brocade_deployment::unlock_probe::V53_UNLOCK_SERVICES
            .len()
            .div_ceil(3);
        let (started, entered) = mpsc::sync_channel(workers);
        let (finished, completed) = mpsc::sync_channel(workers);
        let mut releases = Vec::new();
        for _ in 0..workers {
            let (release, wait) = mpsc::sync_channel(0);
            releases.push(release);
            let started = started.clone();
            let finished = finished.clone();
            resolver
                .pool
                .start(
                    move || {
                        started.send(()).unwrap();
                        wait.recv_timeout(Duration::from_secs(5)).unwrap();
                        Ok(vec![])
                    },
                    move |_| {
                        finished.send(()).unwrap();
                    },
                )
                .unwrap();
        }
        for _ in 0..workers {
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        for release in releases {
            release.send(()).unwrap();
        }
        for _ in 0..workers {
            completed.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert_eq!(resolver.pool.active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn control_dns_accepts_a_startup_burst_and_releases_workers() {
        let resolver = control();
        let (started, entered) = mpsc::sync_channel(8);
        let (finished, completed) = mpsc::sync_channel(8);
        let mut releases = Vec::new();
        for _ in 0..8 {
            let (release, wait) = mpsc::sync_channel(0);
            releases.push(release);
            let started = started.clone();
            let finished = finished.clone();
            resolver
                .pool
                .start(
                    move || {
                        started.send(()).unwrap();
                        let _ = wait.recv_timeout(Duration::from_secs(30));
                        Ok(vec![])
                    },
                    move |_| {
                        finished.send(()).unwrap();
                    },
                )
                .unwrap();
        }
        for _ in 0..8 {
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(resolver.pool.active.load(Ordering::Acquire) >= 8);
        for release in releases {
            release.send(()).unwrap();
        }
        for _ in 0..8 {
            completed.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        // Completion is published only after the worker's permit has been returned.
        let (done, completion) = mpsc::sync_channel(1);
        resolver
            .pool
            .start(
                || Ok(vec![]),
                move |_| {
                    done.send(()).unwrap();
                },
            )
            .unwrap();
        completion.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn canceled_dns_keeps_its_permit_until_the_actual_worker_exits() {
        let resolver = BoundedResolver::new(1);
        let (release, wait) = mpsc::sync_channel(1);
        let (started, entered) = mpsc::sync_channel(1);
        let (publish, canceled) = tokio::sync::oneshot::channel::<io::Result<Vec<SocketAddr>>>();
        resolver
            .pool
            .start(
                move || {
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                    Ok(vec![])
                },
                move |result| {
                    let _ = publish.send(result);
                },
            )
            .unwrap();
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(canceled);
        assert_eq!(
            resolver
                .pool
                .start(|| Ok(vec![]), |_| {})
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(resolver.pool.active.load(Ordering::Acquire), 1);
        // A saturated optional resolver never spends the independent control budget.
        let independent = BoundedResolver::new(1);
        let (done, completion) = mpsc::sync_channel(1);
        independent
            .pool
            .start(
                || Ok(vec![]),
                move |_| {
                    done.send(()).unwrap();
                },
            )
            .unwrap();
        completion.recv_timeout(Duration::from_secs(1)).unwrap();
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while resolver.pool.active.load(Ordering::Acquire) != 0 {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        let (done, completion) = mpsc::sync_channel(1);
        resolver
            .pool
            .start(
                || Ok(vec![]),
                move |_| {
                    done.send(()).unwrap();
                },
            )
            .unwrap();
        completion.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    #[test]
    fn expired_websocket_dns_deadline_does_not_start_work() {
        let resolver = BoundedResolver::new(1);
        assert_eq!(
            resolver
                .resolve_blocking("localhost", 80, Instant::now())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(resolver.pool.active.load(Ordering::Acquire), 0);
    }
}
