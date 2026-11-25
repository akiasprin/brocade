//! Canonical protocol selectors. Persistence and allocation remain independent of matching.
use super::{EntryMode, L4Protocol, MAX_ENTRY_RANGE_PORTS};
use serde::{Deserialize, Serialize};

pub const MAX_ENTRY_POLICY_RANGES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntryPortRange {
    pub start: u16,
    pub end: u16,
}
impl EntryPortRange {
    pub fn valid(self) -> bool {
        self.start > 0 && self.start <= self.end
    }
    pub fn count(self) -> u64 {
        u64::from(self.end) - u64::from(self.start) + 1
    }
    pub fn overlaps(self, other: Self) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

pub fn normalize_entry_ranges(input: &[EntryPortRange]) -> Result<Vec<EntryPortRange>, String> {
    if input.len() > MAX_ENTRY_POLICY_RANGES || input.iter().any(|r| !r.valid()) {
        return Err("端口列表无效：端口须在 1–65535，区间起点不能大于终点，且最多 256 段".into());
    }
    merge_entry_ranges(input)
}

/// Union of validated, derived intervals; subtraction can produce more intervals than its input.
pub fn merge_entry_ranges(input: &[EntryPortRange]) -> Result<Vec<EntryPortRange>, String> {
    if input.iter().any(|r| !r.valid()) {
        return Err("无效的派生端口区间".into());
    }
    let mut sorted = input.to_vec();
    sorted.sort();
    let mut out: Vec<EntryPortRange> = Vec::new();
    for range in sorted {
        if let Some(last) = out.last_mut() {
            if u32::from(range.start) <= u32::from(last.end) + 1 {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        out.push(range);
    }
    Ok(out)
}

pub fn subtract_entry_ranges(
    base: &[EntryPortRange],
    excluded: &[EntryPortRange],
) -> Vec<EntryPortRange> {
    let mut out = Vec::new();
    for range in base {
        let mut cursor = u32::from(range.start);
        for cut in excluded.iter().filter(|cut| cut.overlaps(*range)) {
            if u32::from(cut.start) > cursor {
                out.push(EntryPortRange {
                    start: cursor as u16,
                    end: cut.start - 1,
                });
            }
            cursor = cursor.max(u32::from(cut.end) + 1);
            if cursor > u32::from(range.end) {
                break;
            }
        }
        if cursor <= u32::from(range.end) {
            out.push(EntryPortRange {
                start: cursor as u16,
                end: range.end,
            });
        }
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntryReservedPorts {
    pub tcp: Vec<EntryPortRange>,
    pub udp: Vec<EntryPortRange>,
}
impl EntryReservedPorts {
    pub fn is_empty(&self) -> bool {
        self.tcp.is_empty() && self.udp.is_empty()
    }
    pub fn normalized(&self) -> Result<Self, String> {
        Ok(Self {
            tcp: normalize_entry_ranges(&self.tcp)?,
            udp: normalize_entry_ranges(&self.udp)?,
        })
    }
    pub fn for_protocol(&self, protocol: L4Protocol) -> &[EntryPortRange] {
        match protocol {
            L4Protocol::Tcp => &self.tcp,
            L4Protocol::Udp => &self.udp,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryPortSet {
    All,
    Ranges { ranges: Vec<EntryPortRange> },
}
impl EntryPortSet {
    pub fn ranges(&self) -> Result<Vec<EntryPortRange>, String> {
        match self {
            Self::All => Ok(vec![EntryPortRange {
                start: 1,
                end: u16::MAX,
            }]),
            Self::Ranges { ranges } => normalize_entry_ranges(ranges),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntryPortMapping {
    pub listen: EntryPortRange,
    pub target: EntryPortRange,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryTransportSelection {
    Disabled,
    Forward {
        ports: EntryPortSet,
        mappings: Vec<EntryPortMapping>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntryTraffic {
    pub tcp: EntryTransportSelection,
    pub udp: EntryTransportSelection,
    pub icmp: bool,
    pub other_ip: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryPortSegment {
    pub listen: EntryPortRange,
    pub target: EntryPortRange,
}
impl EntryPortSegment {
    pub fn preserving(self) -> bool {
        self.listen == self.target
    }
    pub fn without(self, reserved: &[EntryPortRange]) -> Vec<Self> {
        subtract_entry_ranges(&[self.listen], reserved)
            .into_iter()
            .map(|listen| {
                let start = self.target.start + (listen.start - self.listen.start);
                Self {
                    listen,
                    target: EntryPortRange {
                        start,
                        end: start + (listen.end - listen.start),
                    },
                }
            })
            .collect()
    }
}
impl EntryTransportSelection {
    pub fn segments(&self) -> Result<Vec<EntryPortSegment>, String> {
        let Self::Forward { ports, mappings } = self else {
            return Ok(Vec::new());
        };
        let base = ports.ranges()?;
        if base.is_empty() || mappings.len() > MAX_ENTRY_POLICY_RANGES {
            return Err("转发端口不能为空或超过映射数量上限".into());
        }
        let mut maps = mappings.clone();
        maps.sort_by_key(|m| m.listen);
        let mut remapped = 0;
        for (index, map) in maps.iter().enumerate() {
            if !map.listen.valid()
                || !map.target.valid()
                || map.listen.count() != map.target.count()
            {
                return Err("入口和目标端口范围须有效且等长".into());
            }
            if !base
                .iter()
                .any(|r| r.start <= map.listen.start && map.listen.end <= r.end)
            {
                return Err("端口映射必须位于转发范围内".into());
            }
            if index > 0 && maps[index - 1].listen.overlaps(map.listen) {
                return Err("入口端口映射不能重叠".into());
            }
            if map.listen != map.target {
                remapped += map.listen.count();
            }
        }
        if remapped > u64::from(MAX_ENTRY_RANGE_PORTS) {
            return Err("改端口的映射最多包含 4096 个端口".into());
        }
        let cuts = normalize_entry_ranges(&maps.iter().map(|m| m.listen).collect::<Vec<_>>())?;
        let mut out: Vec<_> = subtract_entry_ranges(&base, &cuts)
            .into_iter()
            .map(|r| EntryPortSegment {
                listen: r,
                target: r,
            })
            .collect();
        out.extend(maps.into_iter().map(|m| EntryPortSegment {
            listen: m.listen,
            target: m.target,
        }));
        out.sort_by_key(|m| m.listen);
        Ok(out)
    }
}
impl EntryTraffic {
    pub fn all() -> Self {
        Self {
            tcp: EntryTransportSelection::Forward {
                ports: EntryPortSet::All,
                mappings: vec![],
            },
            udp: EntryTransportSelection::Forward {
                ports: EntryPortSet::All,
                mappings: vec![],
            },
            icmp: true,
            other_ip: true,
        }
    }
    pub fn transport(&self, protocol: L4Protocol) -> &EntryTransportSelection {
        match protocol {
            L4Protocol::Tcp => &self.tcp,
            L4Protocol::Udp => &self.udp,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        let tcp = self.tcp.segments()?;
        let udp = self.udp.segments()?;
        if tcp.is_empty() && udp.is_empty() && !self.icmp && !self.other_ip {
            return Err("至少选择一种转发协议".into());
        }
        Ok(())
    }
    pub fn segments(
        &self,
        protocol: L4Protocol,
        reserved: &EntryReservedPorts,
    ) -> Result<Vec<EntryPortSegment>, String> {
        let cuts = normalize_entry_ranges(reserved.for_protocol(protocol))?;
        Ok(self
            .transport(protocol)
            .segments()?
            .into_iter()
            .flat_map(|s| s.without(&cuts))
            .collect())
    }
    pub fn normalized(&self) -> Result<Self, String> {
        self.validate()?;
        fn transport(s: &EntryTransportSelection) -> Result<EntryTransportSelection, String> {
            Ok(match s {
                EntryTransportSelection::Disabled => EntryTransportSelection::Disabled,
                EntryTransportSelection::Forward { ports, mappings } => {
                    let ports = match ports {
                        EntryPortSet::All => EntryPortSet::All,
                        EntryPortSet::Ranges { ranges } => EntryPortSet::Ranges {
                            ranges: normalize_entry_ranges(ranges)?,
                        },
                    };
                    let mut mappings = mappings.clone();
                    mappings.sort_by_key(|m| m.listen);
                    EntryTransportSelection::Forward { ports, mappings }
                }
            })
        }
        Ok(Self {
            tcp: transport(&self.tcp)?,
            udp: transport(&self.udp)?,
            icmp: self.icmp,
            other_ip: self.other_ip,
        })
    }
}
impl EntryMode {
    pub fn requires_protocol_rules(&self, reserved: &EntryReservedPorts) -> bool {
        matches!(self, Self::Protocols { .. }) || !reserved.is_empty()
    }
    pub fn traffic(&self) -> EntryTraffic {
        match self {
            Self::L3 => EntryTraffic::all(),
            Self::Protocols { traffic } => traffic.clone(),
            Self::L4Range {
                protocol,
                listen_start,
                listen_end,
                target_start,
                target_end,
            } => {
                let selection = EntryTransportSelection::Forward {
                    ports: EntryPortSet::Ranges {
                        ranges: vec![EntryPortRange {
                            start: *listen_start,
                            end: *listen_end,
                        }],
                    },
                    mappings: vec![EntryPortMapping {
                        listen: EntryPortRange {
                            start: *listen_start,
                            end: *listen_end,
                        },
                        target: EntryPortRange {
                            start: *target_start,
                            end: *target_end,
                        },
                    }],
                };
                EntryTraffic {
                    tcp: if protocol.covers(L4Protocol::Tcp) {
                        selection.clone()
                    } else {
                        EntryTransportSelection::Disabled
                    },
                    udp: if protocol.covers(L4Protocol::Udp) {
                        selection
                    } else {
                        EntryTransportSelection::Disabled
                    },
                    icmp: false,
                    other_ip: false,
                }
            }
        }
    }
    pub fn protocol_segments(
        &self,
        protocol: L4Protocol,
        reserved: &EntryReservedPorts,
    ) -> Result<Vec<EntryPortSegment>, String> {
        self.traffic().segments(protocol, reserved)
    }
    /// Bounded endpoint samples of explicitly selected TCP services, never arbitrary all-port endpoints.
    pub fn sample_tcp_targets(&self, reserved: &EntryReservedPorts) -> Result<Vec<u16>, String> {
        if matches!(self, Self::L3) {
            return Ok(vec![]);
        }
        let traffic = self.traffic();
        let segments = traffic.segments(L4Protocol::Tcp, reserved)?;
        let explicit = match &traffic.tcp {
            EntryTransportSelection::Forward {
                ports: EntryPortSet::All,
                mappings,
            } => Some(mappings),
            _ => None,
        };
        let mut out = std::collections::BTreeSet::new();
        for s in segments {
            if let Some(mappings) = explicit {
                for m in mappings {
                    for listen in subtract_entry_ranges(&[m.listen], &reserved.normalized()?.tcp) {
                        out.insert(m.target.start + (listen.start - m.listen.start));
                        out.insert(m.target.start + (listen.end - m.listen.start));
                    }
                }
                break;
            }
            out.insert(s.target.start);
            out.insert(s.target.end);
        }
        Ok(out.into_iter().take(8).collect())
    }
    pub fn covers_udp(&self) -> bool {
        !matches!(self.traffic().udp, EntryTransportSelection::Disabled)
    }
    pub fn normalized(&self) -> Result<Self, String> {
        match self {
            Self::Protocols { traffic } => Ok(Self::Protocols {
                traffic: traffic.normalized()?,
            }),
            _ if self.valid() => Ok(self.clone()),
            _ => Err("入口端口配置无效".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn r(start: u16, end: u16) -> EntryPortRange {
        EntryPortRange { start, end }
    }
    #[test]
    fn normalization_and_subtraction_are_canonical_and_bounded() {
        assert_eq!(
            normalize_entry_ranges(&[r(30, 40), r(22, 22), r(20, 29), r(40, 40)]).unwrap(),
            vec![r(20, 40)]
        );
        assert_eq!(
            subtract_entry_ranges(&[r(1, 65535)], &[r(22, 22), r(100, 200), r(65535, 65535)]),
            vec![r(1, 21), r(23, 99), r(201, 65534)]
        );
        assert!(normalize_entry_ranges(&[r(0, 1)]).is_err());
        assert!(normalize_entry_ranges(&vec![r(1, 1); 257]).is_err());
    }
    #[test]
    fn reservation_cuts_keep_original_mapping_offset() {
        let selection = EntryTransportSelection::Forward {
            ports: EntryPortSet::Ranges {
                ranges: vec![r(30000, 30099)],
            },
            mappings: vec![EntryPortMapping {
                listen: r(30000, 30099),
                target: r(40000, 40099),
            }],
        };
        let traffic = EntryTraffic {
            tcp: selection,
            udp: EntryTransportSelection::Disabled,
            icmp: false,
            other_ip: false,
        };
        let segments = traffic
            .segments(
                L4Protocol::Tcp,
                &EntryReservedPorts {
                    tcp: vec![r(30050, 30050)],
                    udp: vec![],
                },
            )
            .unwrap();
        assert_eq!(
            segments,
            vec![
                EntryPortSegment {
                    listen: r(30000, 30049),
                    target: r(40000, 40049)
                },
                EntryPortSegment {
                    listen: r(30051, 30099),
                    target: r(40051, 40099)
                }
            ]
        );
    }
    #[test]
    fn reserved_host_ssh_does_not_block_explicit_guest_ssh_mapping() {
        let mut traffic = EntryTraffic::all();
        traffic.tcp = EntryTransportSelection::Forward {
            ports: EntryPortSet::All,
            mappings: vec![EntryPortMapping {
                listen: r(30022, 30022),
                target: r(22, 22),
            }],
        };
        let segments = traffic
            .segments(
                L4Protocol::Tcp,
                &EntryReservedPorts {
                    tcp: vec![r(22, 22)],
                    udp: vec![],
                },
            )
            .unwrap();
        assert!(!segments
            .iter()
            .any(|s| s.listen.start <= 22 && 22 <= s.listen.end));
        assert!(segments
            .iter()
            .any(|s| s.listen == r(30022, 30022) && s.target == r(22, 22)));
    }
    #[test]
    fn legacy_encodings_keep_their_identity_and_new_rules_normalize_deterministically() {
        for raw in [
            r#"{"kind":"l3"}"#,
            r#"{"kind":"l4_range","protocol":"tcp","listen_start":30022,"listen_end":30022,"target_start":22,"target_end":22}"#,
        ] {
            let value: EntryMode = serde_json::from_str(raw).unwrap();
            assert_eq!(serde_json::to_string(&value).unwrap(), raw);
        }
        let first = EntryTraffic {
            tcp: EntryTransportSelection::Forward {
                ports: EntryPortSet::Ranges {
                    ranges: vec![r(443, 443), r(80, 81), r(81, 82)],
                },
                mappings: vec![],
            },
            udp: EntryTransportSelection::Disabled,
            icmp: false,
            other_ip: false,
        };
        let mut second = first.clone();
        if let EntryTransportSelection::Forward {
            ports: EntryPortSet::Ranges { ranges },
            ..
        } = &mut second.tcp
        {
            ranges.reverse();
        }
        assert_eq!(
            serde_json::to_vec(&first.normalized().unwrap()).unwrap(),
            serde_json::to_vec(&second.normalized().unwrap()).unwrap()
        );
        let invalid = EntryTransportSelection::Forward {
            ports: EntryPortSet::All,
            mappings: vec![EntryPortMapping {
                listen: r(1, 3),
                target: r(10, 11),
            }],
        };
        assert!(invalid.segments().is_err());
        let overlap = EntryTransportSelection::Forward {
            ports: EntryPortSet::All,
            mappings: vec![
                EntryPortMapping {
                    listen: r(1, 3),
                    target: r(10, 12),
                },
                EntryPortMapping {
                    listen: r(3, 4),
                    target: r(20, 21),
                },
            ],
        };
        assert!(overlap.segments().is_err());
        // Derived intervals are not subject to the raw-input segment limit.
        assert_eq!(
            merge_entry_ranges(&(1..=600).step_by(2).map(|p| r(p, p)).collect::<Vec<_>>())
                .unwrap()
                .len(),
            300
        );
    }
    #[test]
    fn service_samples_follow_effective_mapping_without_probing_unspecified_all_ports() {
        let mut traffic = EntryTraffic::all();
        assert!(EntryMode::Protocols {
            traffic: traffic.clone()
        }
        .sample_tcp_targets(&EntryReservedPorts::default())
        .unwrap()
        .is_empty());
        traffic.tcp = EntryTransportSelection::Forward {
            ports: EntryPortSet::All,
            mappings: vec![EntryPortMapping {
                listen: r(30000, 30009),
                target: r(9090, 9099),
            }],
        };
        assert_eq!(
            EntryMode::Protocols { traffic }
                .sample_tcp_targets(&EntryReservedPorts {
                    tcp: vec![r(30004, 30004)],
                    udp: vec![]
                })
                .unwrap(),
            vec![9090, 9093, 9095, 9099]
        );
    }
}
