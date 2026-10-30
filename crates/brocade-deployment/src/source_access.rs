//! Operational source-IP denials, independent of configuration and grant releases.
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

pub const SOURCE_ACCESS_PROTOCOL_VERSION: u32 = 45;
pub const MAX_SOURCE_ACCESS_IPS: usize = 256;
pub const MAX_SOURCE_ACCESS_ENTRIES: usize = 8192;
pub const MAX_SOURCE_ACCESS_BYTES: usize = 2 * 1024 * 1024;

pub fn normalize_ip(value: &str) -> Result<String, String> {
    let ip: IpAddr = value
        .parse()
        .map_err(|_| "请输入有效的 IPv4 或 IPv6 地址（不含端口或网段）")?;
    Ok(match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map_or_else(|| ip.to_string(), |ip| ip.to_string()),
        IpAddr::V4(ip) => ip.to_string(),
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAccessUser {
    pub tenant_id: String,
    pub user_id: String,
    pub ips: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAccessDesired {
    pub revision: i64,
    pub node_ips: Vec<String>,
    /// Fleet-wide user denials also cover draining and newly granted ingress generations.
    /// This authenticated infrastructure channel contains no credentials.
    pub users: Vec<SourceAccessUser>,
}

impl SourceAccessDesired {
    pub fn validate(&self) -> Result<(), String> {
        if self.revision < 0 || self.users.len() > MAX_SOURCE_ACCESS_ENTRIES {
            return Err("invalid source access revision or size".into());
        }
        let mut count = self.node_ips.len();
        for ips in std::iter::once(&self.node_ips).chain(self.users.iter().map(|user| &user.ips)) {
            if ips.len() > MAX_SOURCE_ACCESS_IPS {
                return Err("source access list too large".into());
            }
            for ip in ips {
                normalize_ip(ip)?;
            }
        }
        let mut users = std::collections::BTreeSet::new();
        for user in &self.users {
            if !user
                .tenant_id
                .split('.')
                .all(brocade_core::model::is_valid_slug)
                || !brocade_core::model::is_valid_slug(&user.user_id)
                || !users.insert((&user.tenant_id, &user.user_id))
            {
                return Err("invalid source access user identity".into());
            }
            count += user.ips.len();
        }
        if count > MAX_SOURCE_ACCESS_ENTRIES {
            return Err("source access snapshot too large".into());
        }
        Ok(())
    }
    pub fn active(&self) -> bool {
        !self.node_ips.is_empty() || self.users.iter().any(|user| !user.ips.is_empty())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAccessWrite {
    /// Optimistic concurrency token of this scope, independent of unrelated policy edits.
    pub revision: i64,
    pub ips: Vec<String>,
}
impl SourceAccessWrite {
    pub fn normalized(&self) -> Result<Vec<String>, String> {
        if self.revision < 0 || self.ips.len() > MAX_SOURCE_ACCESS_IPS {
            return Err("封禁名单最多 256 个 IP，且版本必须有效".into());
        }
        self.ips
            .iter()
            .map(|ip| normalize_ip(ip))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
            .map(|ips| ips.into_iter().collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAccessObservation {
    pub revision: i64,
    pub supported: bool,
    pub error: Option<String>,
}

/// Existing Agents use the observation's diagnostic field for both pending
/// confirmation and failures. Keep these exact messages compatible during upgrades.
#[derive(Debug, Clone, Copy)]
pub enum SourceAccessWaitReason {
    Configuration,
    SplitHandoff,
    GenerationSwitch,
    Receipt,
    Revision,
}

impl SourceAccessWaitReason {
    pub const fn message(self) -> &'static str {
        match self {
            Self::Configuration => "等待 Xray 接入配置",
            Self::SplitHandoff => "XHTTP 独立下载入口交接中，等待旧代退出后确认来源策略",
            Self::GenerationSwitch => "Xray 代际正在切换，等待接入策略确认",
            Self::Receipt => "等待 Xray 应用接入策略",
            Self::Revision => "等待全部 Xray 代际应用接入策略",
        }
    }

    pub fn matches_message(message: &str) -> bool {
        [
            Self::Configuration,
            Self::SplitHandoff,
            Self::GenerationSwitch,
            Self::Receipt,
            Self::Revision,
        ]
        .into_iter()
        .any(|reason| reason.message() == message)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAccessNode {
    pub node_id: String,
    pub phase: SourceAccessPhase,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAccessPhase {
    Pending,
    Applied,
    Unsupported,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAccessView {
    pub revision: i64,
    pub ips: Vec<String>,
    pub nodes: Vec<SourceAccessNode>,
    /// Read-only user denials relevant to a node's current or quota-suspended grants.
    /// Omitted for user-scoped views and by older Consoles; `ips` remains scope-local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_bans: Option<Vec<SourceAccessUser>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_access_views_accept_older_consoles_without_user_bans() {
        let old_view = serde_json::json!({"revision": 0, "ips": [], "nodes": []});
        let mut view: SourceAccessView = serde_json::from_value(old_view.clone()).unwrap();
        assert!(view.user_bans.is_none());
        assert_eq!(serde_json::to_value(&view).unwrap(), old_view);
        view.user_bans = Some(vec![SourceAccessUser {
            tenant_id: "platform.acme".into(),
            user_id: "alice".into(),
            ips: vec!["192.0.2.1".into()],
        }]);
        let json = serde_json::to_value(&view).unwrap();
        let decoded: SourceAccessView = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.user_bans, view.user_bans);
    }

    #[test]
    fn source_ips_are_numeric_canonical_and_bounded() {
        let write = SourceAccessWrite {
            revision: 0,
            ips: vec![
                "::ffff:192.0.2.1".into(),
                "192.0.2.1".into(),
                "2001:0db8::1".into(),
            ],
        };
        assert_eq!(
            write.normalized().unwrap(),
            vec!["192.0.2.1", "2001:db8::1"]
        );
        let nested = SourceAccessDesired {
            revision: 1,
            node_ips: vec![],
            users: vec![SourceAccessUser {
                tenant_id: "long-tenant-segment.another-tenant-segment".into(),
                user_id: "alice".into(),
                ips: vec!["192.0.2.1".into()],
            }],
        };
        assert!(nested.validate().is_ok());
        for ip in [
            "example.com",
            "192.0.2.1:80",
            "192.0.2.0/24",
            "fe80::1%eth0",
            " 192.0.2.1",
        ] {
            assert!(normalize_ip(ip).is_err(), "{ip}");
        }
        assert!(SourceAccessWrite {
            revision: 0,
            ips: vec!["192.0.2.1".into(); 257]
        }
        .normalized()
        .is_err());
    }
}
