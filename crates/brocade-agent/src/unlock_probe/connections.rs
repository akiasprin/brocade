//! Evidence emitted by the private Xray for the actual HTTP connections, never DNS prechecks.
use brocade_deployment::unlock_probe::{
    UnlockConnectionObservation, UnlockConnectionTrace, UnlockPart, UnlockService,
};
use serde::Deserialize;
use std::{collections::BTreeMap, io::Read, path::PathBuf};

pub(super) const MAX_FILE: u64 = 2 * 1024 * 1024;
pub(super) fn service_key(service: UnlockService) -> String {
    serde_json::to_value(service)
        .expect("a fieldless service enum always serializes")
        .as_str()
        .expect("a fieldless service enum serializes as its name")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        sync::atomic::{AtomicU64, Ordering},
    };
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn fixture() -> (Fixture, Source) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "brocade-connection-evidence-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, []).unwrap();
        (
            Fixture(path.clone()),
            Source {
                path,
                clients: Default::default(),
            },
        )
    }
    fn append(source: &Source, id: u32, started: bool, group: &str, exit: &str) {
        let value = serde_json::json!({"group":group,"started":started,"version":2,"id":format!("{id:032x}"),"domain":"gemini.google.com","exit":exit,"outbound":"out:forward:probe-v4","profile":"a".repeat(64),"normal_strategy":"UseIP","family":"v4","connected":!started,"complete":!started,"lookups":[]});
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&source.path)
            .unwrap();
        writeln!(file, "{value}").unwrap();
    }
    #[test]
    fn delayed_completions_remain_bound_to_their_connection_and_subitem() {
        let (_file, source) = fixture();
        append(&source, 1, true, "gemini.service", "");
        append(&source, 2, true, "gemini.service", "");
        append(&source, 2, false, "gemini.service", "exit-two");
        append(&source, 1, false, "gemini.service", "exit-one");
        let records = source.read(UnlockService::Gemini);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].connection.exit, "exit-one");
        assert_eq!(records[1].connection.exit, "exit-two");
        assert_ne!(records[0].connection.id, records[1].connection.id);
    }
    #[test]
    fn pending_orphan_duplicate_and_wrong_subitem_cannot_look_complete() {
        let (_file, source) = fixture();
        append(&source, 1, true, "gemini.service", "");
        let records = source.read(UnlockService::Gemini);
        assert_eq!(records.len(), 1);
        assert!(!records[0].connection.complete);
        append(&source, 2, false, "gemini.service", "orphan");
        assert_eq!(source.read(UnlockService::Gemini).len(), 1);
        append(&source, 1, false, "gemini.cdn", "wrong-part");
        assert!(source.read(UnlockService::Gemini).is_empty());
        std::fs::write(&source.path, []).unwrap();
        append(&source, 1, true, "gemini.service", "");
        append(&source, 1, true, "gemini.service", "");
        assert!(source.read(UnlockService::Gemini).is_empty());
    }
    #[test]
    fn unrelated_services_and_malformed_or_oversized_files_do_not_supply_evidence() {
        let (_file, source) = fixture();
        append(&source, 1, true, "gemini.service", "");
        append(&source, 1, false, "gemini.service", "exit-one");
        append(&source, 2, true, "chatgpt.service", "");
        append(&source, 2, false, "chatgpt.service", "different-service");
        assert_eq!(source.read(UnlockService::Gemini).len(), 1);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&source.path)
            .unwrap()
            .write_all(b"{truncated")
            .unwrap();
        assert!(source.read(UnlockService::Gemini).is_empty());
        std::fs::write(&source.path, vec![b' '; MAX_FILE as usize]).unwrap();
        assert!(source.read(UnlockService::Gemini).is_empty());
    }
}
pub(super) fn group(service: UnlockService, part: UnlockPart) -> String {
    format!("{}.{}", service_key(service), part.key())
}
pub(super) fn groups(services: &[UnlockService]) -> Vec<String> {
    services
        .iter()
        .flat_map(|service| {
            let parts: &[UnlockPart] = if *service == UnlockService::YoutubePremium {
                &[UnlockPart::Webpage, UnlockPart::Premium, UnlockPart::Cdn]
            } else {
                &[UnlockPart::Service]
            };
            parts
                .iter()
                .map(|part| group(*service, *part))
                .collect::<Vec<_>>()
        })
        .collect()
}
pub(super) struct Source {
    path: PathBuf,
    clients: BTreeMap<String, reqwest::blocking::Client>,
}
#[derive(Deserialize)]
struct Raw {
    group: String,
    #[serde(default)]
    started: bool,
    #[serde(flatten)]
    connection: UnlockConnectionTrace,
}
impl Source {
    #[cfg(test)]
    pub(super) fn fixture_source(path: PathBuf) -> Self {
        Self {
            path,
            clients: Default::default(),
        }
    }

    #[cfg(test)]
    pub(super) fn fixture_with_ports(path: PathBuf, ports: &BTreeMap<String, u16>) -> Self {
        Self {
            path,
            clients: ports
                .iter()
                .map(|(part, port)| {
                    (
                        format!("youtube_premium.{part}"),
                        crate::http::diagnostic_socks_client(*port).unwrap(),
                    )
                })
                .collect(),
        }
    }

    pub fn new(
        session: &brocade_probe::ChainProbeClient,
        groups: &[String],
    ) -> Result<Self, String> {
        let mut clients = BTreeMap::new();
        for group in groups {
            let port = session.trace_port(group).ok_or("missing trace listener")?;
            clients.insert(group.clone(), crate::http::diagnostic_socks_client(port)?);
        }
        Ok(Self {
            path: PathBuf::from(session.trace_path().ok_or("missing evidence file")?),
            clients,
        })
    }
    pub fn client(
        &self,
        service: UnlockService,
        part: UnlockPart,
    ) -> Option<&reqwest::blocking::Client> {
        self.clients.get(&group(service, part))
    }
    pub fn read(&self, service: UnlockService) -> Vec<UnlockConnectionObservation> {
        let read = || -> Result<Vec<UnlockConnectionObservation>, std::io::Error> {
            let file = std::fs::File::open(&self.path)?;
            let total = file.metadata()?.len();
            if total >= MAX_FILE {
                return Ok(vec![]);
            }
            let mut data = String::new();
            file.take(MAX_FILE + 1).read_to_string(&mut data)?;
            if data.len() as u64 >= MAX_FILE || !data.is_empty() && !data.ends_with('\n') {
                return Ok(vec![]);
            }
            let prefix = format!("{}.", service_key(service));
            let mut observations = BTreeMap::new();
            for line in data.lines() {
                let Ok(raw) = serde_json::from_str::<Raw>(line) else {
                    return Ok(vec![]);
                };
                let Some(part) = raw.group.strip_prefix(&prefix) else {
                    continue;
                };
                let part = match part {
                    "service" => UnlockPart::Service,
                    "webpage" => UnlockPart::Webpage,
                    "premium" => UnlockPart::Premium,
                    "cdn" => UnlockPart::Cdn,
                    _ => return Ok(vec![]),
                };
                if !raw.connection.valid() {
                    return Ok(vec![]);
                }
                if raw.started {
                    if observations
                        .insert(
                            raw.connection.id.clone(),
                            UnlockConnectionObservation {
                                part,
                                connection: raw.connection,
                            },
                        )
                        .is_some()
                    {
                        return Ok(vec![]);
                    }
                } else if let Some(started) = observations.get_mut(&raw.connection.id) {
                    if started.part != part || started.connection.domain != raw.connection.domain {
                        return Ok(vec![]);
                    }
                    started.connection = raw.connection;
                }
                if observations.len() > 64 {
                    return Ok(vec![]);
                }
            }
            Ok(observations.into_values().collect())
        };
        read().unwrap_or_default()
    }
}
