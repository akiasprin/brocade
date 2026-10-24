use super::{
    diagnostic, resolver, tls_config, AddressFamily, HTTP_CONNECT_TIMEOUT, HTTP_REQUEST_TIMEOUT,
};
use std::{
    sync::{Arc, Mutex, TryLockError},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub(super) enum TrafficClass {
    Control,
    Public,
}
#[derive(Debug, PartialEq)]
enum InitializationError {
    Busy,
    Backoff,
    Build(String),
}

struct State<T> {
    client: Option<T>,
    retry_at: Option<Instant>,
    backoff: Duration,
}
struct ClientSlot<T>(Mutex<State<T>>);
impl<T: Clone> ClientSlot<T> {
    const fn new() -> Self {
        Self(Mutex::new(State {
            client: None,
            retry_at: None,
            backoff: Duration::from_secs(1),
        }))
    }
    fn get(
        &self,
        now: Instant,
        build: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, InitializationError> {
        // Never queue caller threads behind initialization outside their request deadline.
        let mut state = match self.0.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => return Err(InitializationError::Busy),
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        if let Some(client) = &state.client {
            return Ok(client.clone());
        }
        if state.retry_at.is_some_and(|retry_at| now < retry_at) {
            return Err(InitializationError::Backoff);
        }
        match build() {
            Ok(client) => {
                state.client = Some(client.clone());
                state.retry_at = None;
                Ok(client)
            }
            Err(error) => {
                state.retry_at = Some(Instant::now() + state.backoff);
                state.backoff = (state.backoff * 2).min(Duration::from_secs(15));
                Err(InitializationError::Build(error))
            }
        }
    }
}

pub(super) fn shared(
    class: TrafficClass,
    family: Option<AddressFamily>,
    deadline: Instant,
) -> Result<reqwest::blocking::Client, String> {
    static CONTROL: ClientSlot<reqwest::blocking::Client> = ClientSlot::new();
    static PUBLIC: ClientSlot<reqwest::blocking::Client> = ClientSlot::new();
    static PUBLIC_V4: ClientSlot<reqwest::blocking::Client> = ClientSlot::new();
    static PUBLIC_V6: ClientSlot<reqwest::blocking::Client> = ClientSlot::new();
    let slot = match class {
        TrafficClass::Control => &CONTROL,
        TrafficClass::Public => match family {
            None => &PUBLIC,
            Some(AddressFamily::V4) => &PUBLIC_V4,
            Some(AddressFamily::V6) => &PUBLIC_V6,
        },
    };
    loop {
        match slot.get(Instant::now(), || build(class, family, tls_config()?)) {
            Err(InitializationError::Busy) => {
                let budget = super::remaining(deadline).map_err(|_| "initialization_timeout")?;
                std::thread::sleep(budget.min(Duration::from_millis(1)));
            }
            Err(InitializationError::Backoff) => return Err("initialization_backoff".to_owned()),
            Err(InitializationError::Build(error)) => return Err(error),
            Ok(client) => return Ok(client),
        }
    }
}
fn build(
    class: TrafficClass,
    family: Option<AddressFamily>,
    tls: Arc<rustls::ClientConfig>,
) -> Result<reqwest::blocking::Client, String> {
    let resolver = match class {
        TrafficClass::Control => resolver::control(),
        TrafficClass::Public => resolver::public(),
    };
    let mut builder = reqwest::blocking::Client::builder()
        .use_preconfigured_tls((*tls).clone())
        .dns_resolver(Arc::new(resolver.with_family(family)))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .http1_only()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .pool_max_idle_per_host(match class {
            TrafficClass::Control => 4,
            TrafficClass::Public => 0,
        })
        .pool_idle_timeout(Duration::from_secs(60))
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .timeout(HTTP_REQUEST_TIMEOUT)
        .user_agent(format!(
            "brocade-agent/{}",
            crate::identity::self_identity()
        ));
    if let Some(family) = family {
        builder = builder.local_address(match family {
            AddressFamily::V4 => std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            AddressFamily::V6 => std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
        });
    }
    builder
        .build()
        .map_err(|error| diagnostic::category(&error))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transient_initialization_failure_retries_after_backoff_and_caches_only_success() {
        let slot = ClientSlot::<usize>::new();
        let now = Instant::now();
        assert_eq!(
            slot.get(now, || Err("temporary_fd_exhaustion".to_owned()))
                .unwrap_err(),
            InitializationError::Build("temporary_fd_exhaustion".to_owned())
        );
        assert_eq!(
            slot.get(now, || panic!("must respect backoff"))
                .unwrap_err(),
            InitializationError::Backoff
        );
        assert_eq!(slot.get(now + Duration::from_secs(2), || Ok(7)).unwrap(), 7);
        assert_eq!(
            slot.get(now, || panic!("success must be reused")).unwrap(),
            7
        );
    }
    #[test]
    fn concurrent_initialization_never_blocks_another_request() {
        let slot = ClientSlot::<usize>::new();
        let _guard = slot.0.lock().unwrap();
        assert_eq!(
            slot.get(Instant::now(), || panic!("cannot initialize concurrently"))
                .unwrap_err(),
            InitializationError::Busy
        );
    }
}
