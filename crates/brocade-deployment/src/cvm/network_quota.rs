//! Node-wide network capacity and durable per-user, per-host assignments.
use super::*;

pub const NETWORK_QUOTA_PROTOCOL_VERSION: u32 = 66;
pub const NETWORK_QUOTA_CONTRACT: &str = "cubevs-user-quota-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkQuotaLimits {
    pub max_states: u32,
    pub new_per_second: u32,
    pub burst: u32,
}
impl NetworkQuotaLimits {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_states > 1_048_576
            || self.new_per_second == 0
            || self.new_per_second > 100_000
            || self.burst == 0
            || self.burst > 200_000
        {
            return Err("网络额度或新建速率超出支持范围".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkQuotaCapacity {
    pub physical_memory_bytes: u64,
    pub host_reserved_memory_bytes: u64,
    pub contract: String,
    pub total_states: u32,
    pub system_reserved_states: u32,
    pub memory_budget_bytes: u64,
    pub fixed_memory_bytes: u64,
    pub bytes_per_state: u32,
    pub model: String,
}
impl NetworkQuotaCapacity {
    pub fn allocatable(&self) -> u32 {
        self.total_states
            .saturating_sub(self.system_reserved_states)
    }
    pub fn validate(&self) -> Result<(), String> {
        let required = self
            .fixed_memory_bytes
            .checked_add(
                u64::from(self.total_states)
                    .checked_mul(u64::from(self.bytes_per_state))
                    .ok_or("网络容量内存计算溢出")?,
            )
            .ok_or("网络容量内存计算溢出")?;
        if self.contract != NETWORK_QUOTA_CONTRACT
            || self.total_states == 0
            || self.total_states > 1_048_576
            || self.system_reserved_states >= self.total_states
            || self.bytes_per_state == 0
            || required > self.memory_budget_bytes
            || self.memory_budget_bytes
                > self
                    .physical_memory_bytes
                    .saturating_sub(self.host_reserved_memory_bytes)
            || self.model.is_empty()
            || self.model.len() > 80
        {
            return Err("宿主网络容量或内存模型无效".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkQuotaAssignment {
    /// Stable hex identity chosen by Store, never by a Guest.
    pub id: String,
    pub revision: u64,
    pub limits: NetworkQuotaLimits,
    pub instances: Vec<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkQuotaDesired {
    pub assignments: Vec<NetworkQuotaAssignment>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkQuotaReport {
    pub id: String,
    pub revision: u64,
    pub applied: bool,
    pub states: u64,
    pub denied_capacity: u64,
    pub denied_rate: u64,
    pub allocation_failures: u64,
    pub error: Option<String>,
}

impl NetworkQuotaDesired {
    pub fn validate(&self) -> Result<(), String> {
        if self.assignments.len() > 512 {
            return Err("宿主网络额度数量超限".into());
        }
        let mut groups = BTreeSet::new();
        let mut instances = BTreeSet::new();
        for a in &self.assignments {
            if a.id.len() != 16
                || !a.id.bytes().all(|c| c.is_ascii_hexdigit())
                || a.id == "0000000000000000"
                || !groups.insert(&a.id)
                || a.revision == 0
                || a.instances.len() > 256
                || a.instances
                    .iter()
                    .any(|id| !valid_id(id) || !instances.insert(id))
            {
                return Err("宿主网络额度身份、版本或容器归属无效".into());
            }
            a.limits.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_capacity_counts_fixed_cost_and_never_oversells() {
        let mut c = NetworkQuotaCapacity {
            physical_memory_bytes: 1024 * 1024 * 1024,
            host_reserved_memory_bytes: 256 * 1024 * 1024,
            contract: NETWORK_QUOTA_CONTRACT.into(),
            total_states: 131072,
            system_reserved_states: 4096,
            memory_budget_bytes: 144 * 1024 * 1024,
            fixed_memory_bytes: 16 * 1024 * 1024,
            bytes_per_state: 1024,
            model: "test-v1".into(),
        };
        assert!(c.validate().is_ok());
        assert_eq!(c.allocatable(), 126976);
        c.memory_budget_bytes -= 1;
        assert!(c.validate().is_err());
    }
    #[test]
    fn two_groups_cannot_charge_the_same_instance() {
        let a = NetworkQuotaAssignment {
            id: "1111111111111111".into(),
            revision: 1,
            limits: NetworkQuotaLimits {
                max_states: 10,
                new_per_second: 1,
                burst: 2,
            },
            instances: vec!["cvm-one".into()],
        };
        let mut d = NetworkQuotaDesired {
            assignments: vec![a.clone()],
        };
        assert!(d.validate().is_ok());
        d.assignments.push(NetworkQuotaAssignment {
            id: "2222222222222222".into(),
            ..a
        });
        assert!(d.validate().is_err());
    }
}
