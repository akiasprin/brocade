//! Version-pinned seed data. Provenance and upstream data license live beside the inputs.
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

pub const UPSTREAM_COMMIT: &str = "0d477e5f9939f037d0481f4e9a1f26f05810c545";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceRegion {
    pub id: String,
    pub country: String,
    pub state: String,
    pub city: String,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub language_params: String,
    pub keywords: Vec<String>,
    pub sites: Vec<String>,
    pub static_sites: Vec<String>,
}
pub fn regions() -> &'static [MaintenanceRegion] {
    static REGIONS: LazyLock<Vec<MaintenanceRegion>> = LazyLock::new(|| {
        serde_json::from_str(include_str!("../../data/ip-maintenance/regions.json"))
            .expect("versioned maintenance seed data is validated by catalog tests")
    });
    &REGIONS
}
pub fn region(id: &str) -> Option<&'static MaintenanceRegion> {
    regions().iter().find(|r| r.id == id)
}
pub fn user_agents() -> &'static [&'static str] {
    static AGENTS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        include_str!("../../data/ip-maintenance/user-agents.txt")
            .lines()
            .filter(|line| !line.is_empty())
            .collect()
    });
    &AGENTS
}

pub fn value_digest(value: &impl Serialize) -> Result<String, String> {
    serde_json::to_vec(value)
        .map(|bytes| brocade_core::hash::sha256_hex(&bytes))
        .map_err(|_| "maintenance_data_encoding".into())
}
pub fn manifest_digest(region_digest: &str, agents_digest: &str) -> String {
    brocade_core::hash::sha256_hex(
        format!("brocade-ip-maintenance-v1:{region_digest}:{agents_digest}").as_bytes(),
    )
}
pub fn seed_agents_digest() -> &'static str {
    static DIGEST: LazyLock<String> = LazyLock::new(|| {
        value_digest(&user_agents()).expect("embedded User-Agent strings are JSON serializable")
    });
    &DIGEST
}
pub fn seed_digest(id: &str) -> Option<String> {
    region(id).map(|region| {
        manifest_digest(
            &value_digest(region).expect("embedded region contains validated finite coordinates"),
            seed_agents_digest(),
        )
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceData {
    pub schema: u32,
    pub region: MaintenanceRegion,
    pub user_agents: Vec<String>,
    pub digest: String,
    pub region_updated_at_unix_secs: i64,
    pub agents_updated_at_unix_secs: i64,
}
impl MaintenanceData {
    pub fn seed(id: &str) -> Option<Self> {
        Some(Self {
            schema: 1,
            region: region(id)?.clone(),
            user_agents: user_agents().iter().map(|ua| (*ua).to_owned()).collect(),
            digest: seed_digest(id)?,
            region_updated_at_unix_secs: 0,
            agents_updated_at_unix_secs: 0,
        })
    }
    pub fn recompute_digest(&mut self) -> Result<(), String> {
        self.digest = manifest_digest(
            &value_digest(&self.region)?,
            &value_digest(&self.user_agents)?,
        );
        Ok(())
    }
    pub fn validate(&self) -> Result<(), String> {
        let invalid_url = |site: &String| {
            url::Url::parse(site).ok().is_none_or(|target| {
                !matches!(target.scheme(), "http" | "https")
                    || target.host_str().is_none()
                    || !target.username().is_empty()
                    || target.password().is_some()
                    || target.fragment().is_some()
                    || target
                        .host_str()
                        .and_then(|host| {
                            host.trim_matches(['[', ']'])
                                .parse::<std::net::IpAddr>()
                                .ok()
                        })
                        .is_some_and(|ip| {
                            brocade_core::forwarding::target_ip_is_blocked_by_policy(ip, false)
                        })
            })
        };
        if self.schema != 1
            || self.user_agents.len() != 4000
            || self.user_agents.iter().any(|ua| {
                ua.is_empty()
                    || ua.len() > 512
                    || !ua.is_ascii()
                    || ua.chars().any(char::is_control)
            })
            || self.region.keywords.is_empty()
            || self.region.keywords.len() > 100
            || self.region.sites.is_empty()
            || self.region.sites.len() > 30
            || self.region.static_sites.len() > 30
            || region(&self.region.id).is_none()
            || self
                .region
                .keywords
                .iter()
                .any(|word| word.len() > 2048 || word.chars().any(char::is_control))
            || self
                .region
                .sites
                .iter()
                .chain(self.region.static_sites.iter())
                .any(|site| {
                    site.len() > 8192 || site.chars().any(char::is_control) || invalid_url(site)
                })
            || !self.region.latitude.is_finite()
            || !self.region.longitude.is_finite()
            || !(-90.0..=90.0).contains(&self.region.latitude)
            || !(-180.0..=180.0).contains(&self.region.longitude)
            || self.digest
                != manifest_digest(
                    &value_digest(&self.region)?,
                    &value_digest(&self.user_agents)?,
                )
        {
            return Err("maintenance_data_invalid".into());
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_inputs_match_the_recorded_source_digests() {
        let provenance: serde_json::Value =
            serde_json::from_str(include_str!("../../data/ip-maintenance/PROVENANCE.json"))
                .unwrap();
        for (name, bytes) in [
            (
                "regions.json",
                include_bytes!("../../data/ip-maintenance/regions.json").as_slice(),
            ),
            (
                "user-agents.txt",
                include_bytes!("../../data/ip-maintenance/user-agents.txt").as_slice(),
            ),
        ] {
            assert_eq!(
                brocade_core::hash::sha256_hex(bytes),
                provenance["files"][name].as_str().unwrap()
            );
        }
    }
    #[test]
    fn complete_pinned_seed_catalog_is_well_formed() {
        assert_eq!(regions().len(), 67);
        assert_eq!(user_agents().len(), 4000);
        assert_eq!(
            regions()
                .iter()
                .map(|r| &r.country)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            31
        );
        for r in regions() {
            MaintenanceData::seed(&r.id).unwrap().validate().unwrap();
            assert!((-90.0..=90.0).contains(&r.latitude));
            assert!((-180.0..=180.0).contains(&r.longitude));
            assert!(!r.keywords.is_empty(), "{} lacks keywords", r.id);
            assert!(!r.sites.is_empty(), "{} lacks sites", r.id);
            assert_eq!(r.country.len(), 2);
        }
    }
    #[test]
    fn bundle_rejects_corruption_and_private_targets_even_with_a_valid_digest() {
        let mut bundle = MaintenanceData::seed(&regions()[0].id).unwrap();
        bundle.user_agents[0].push_str(" changed");
        assert!(bundle.validate().is_err());
        bundle.recompute_digest().unwrap();
        bundle.validate().unwrap();
        for target in [
            "http://127.0.0.1/",
            "http://[::1]/",
            "file:///etc/passwd",
            "https://user:secret@example.com/",
        ] {
            bundle.region.sites[0] = target.into();
            bundle.recompute_digest().unwrap();
            assert!(bundle.validate().is_err(), "accepted {target}");
        }
    }
}
