//! Coalesced, per-domain generations. Each worker consumes its own cursor after a round.
use brocade_deployment::agent_channel::WakeDomain;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    versions: [u64; WakeDomain::ALL.len()],
    connected: bool,
}
#[derive(Default)]
struct Hub {
    state: Mutex<State>,
    changed: Condvar,
}
static HUB: OnceLock<Hub> = OnceLock::new();
fn hub() -> &'static Hub {
    HUB.get_or_init(Hub::default)
}

pub(crate) fn notify(domains: &[WakeDomain]) {
    let hub = hub();
    let mut state = hub
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for domain in domains {
        state.versions[*domain as usize] = state.versions[*domain as usize].wrapping_add(1);
    }
    hub.changed.notify_all();
}
pub(crate) fn set_connected(connected: bool) {
    let hub = hub();
    hub.state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .connected = connected;
    hub.changed.notify_all();
}
pub(crate) struct Cursor {
    domain: WakeDomain,
    seen: u64,
}
impl Cursor {
    pub(crate) fn new(domain: WakeDomain) -> Self {
        let seen = hub()
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .versions[domain as usize];
        Self { domain, seen }
    }
    pub(crate) fn wait(&mut self, fallback: Duration) -> bool {
        self.wait_connected(fallback, fallback)
    }
    pub(crate) fn wait_connected(&mut self, fallback: Duration, connected: Duration) -> bool {
        let hub = hub();
        let mut state = hub
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let was_connected = state.connected;
        let deadline = Instant::now() + if was_connected { connected } else { fallback };
        while state.versions[self.domain as usize] == self.seen && state.connected == was_connected
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            state = hub
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        let changed =
            self.seen != state.versions[self.domain as usize] || state.connected != was_connected;
        self.seen = state.versions[self.domain as usize];
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_signal_during_a_round_is_retained_and_domains_do_not_overwrite() {
        let mut agent = Cursor::new(WakeDomain::AgentRelease);
        let mut xray = Cursor::new(WakeDomain::XrayRelease);
        notify(&[WakeDomain::AgentRelease]);
        notify(&[WakeDomain::XrayRelease]);
        let start = Instant::now();
        agent.wait(Duration::from_secs(3));
        xray.wait(Duration::from_secs(3));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
