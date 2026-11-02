//! Bounded, process-local hints. Data and authorization stay with the ordinary read endpoints.
use serde::Serialize;
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationDomain {
    Nodes,
    Load,
    Traffic,
    E2e,
    Unlock,
    Deployments,
    TunnelProbes,
}

impl ObservationDomain {
    pub const ALL: [Self; 7] = [
        Self::Nodes,
        Self::Load,
        Self::Traffic,
        Self::E2e,
        Self::Unlock,
        Self::Deployments,
        Self::TunnelProbes,
    ];
}

#[derive(Clone, Debug)]
pub(crate) struct ObservationEvents(watch::Sender<[u64; 7]>);

impl Default for ObservationEvents {
    fn default() -> Self {
        Self(watch::channel([0; 7]).0)
    }
}

impl ObservationEvents {
    pub fn subscribe(&self) -> watch::Receiver<[u64; 7]> {
        self.0.subscribe()
    }

    pub fn changed(&self, domains: &[ObservationDomain]) {
        if domains.is_empty() {
            return;
        }
        // A slow browser needs the newest version of each domain, never a queue of reports.
        self.0.send_modify(|versions| {
            for domain in domains {
                versions[*domain as usize] = versions[*domain as usize].wrapping_add(1);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn coalesces_reports_without_losing_independent_domains() {
        let events = ObservationEvents::default();
        let mut receiver = events.subscribe();
        for _ in 0..10_000 {
            events.changed(&[ObservationDomain::Load]);
        }
        events.changed(&[ObservationDomain::E2e]);
        receiver.changed().await.unwrap();
        let versions = *receiver.borrow_and_update();
        assert_eq!(versions[ObservationDomain::Load as usize], 10_000);
        assert_eq!(versions[ObservationDomain::E2e as usize], 1);
        assert_eq!(versions[ObservationDomain::Nodes as usize], 0);
        assert!(!receiver.has_changed().unwrap());
    }
}
