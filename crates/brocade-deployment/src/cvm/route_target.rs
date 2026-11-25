//! A routing target is an interface or a chain, never both. Existing interface JSON stays valid.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(untagged)]
pub enum RouteTarget {
    Interface { interface_id: String },
    Chain { chain_id: String },
}

impl<'de> Deserialize<'de> for RouteTarget {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            interface_id: Option<String>,
            chain_id: Option<String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        match (wire.interface_id, wire.chain_id) {
            (Some(interface_id), None) if valid_id(&interface_id) => {
                Ok(Self::Interface { interface_id })
            }
            (None, Some(chain_id)) if valid_id(&chain_id) => Ok(Self::Chain { chain_id }),
            _ => Err(serde::de::Error::custom(
                "每条路由须选择一张有效网卡或一条有效线路，不能同时选择两种出口",
            )),
        }
    }
}

impl RouteTarget {
    pub fn interface(id: impl Into<String>) -> Self {
        Self::Interface {
            interface_id: id.into(),
        }
    }
    pub fn chain(id: impl Into<String>) -> Self {
        Self::Chain {
            chain_id: id.into(),
        }
    }
    pub fn id(&self) -> &str {
        match self {
            Self::Interface { interface_id } => interface_id,
            Self::Chain { chain_id } => chain_id,
        }
    }
    pub fn interface_id(&self) -> Option<&str> {
        match self {
            Self::Interface { interface_id } => Some(interface_id),
            Self::Chain { .. } => None,
        }
    }
    pub fn chain_id(&self) -> Option<&str> {
        match self {
            Self::Chain { chain_id } => Some(chain_id),
            Self::Interface { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_interface_and_chain_are_exclusive_even_with_future_fields() {
        for (wire, target) in [
            (
                json!({"interface_id":"nic-a", "future":true}),
                RouteTarget::interface("nic-a"),
            ),
            (
                json!({"chain_id":"chain-a", "future":true}),
                RouteTarget::chain("chain-a"),
            ),
        ] {
            assert_eq!(serde_json::from_value::<RouteTarget>(wire).unwrap(), target);
        }
        for wire in [
            json!({}),
            json!({"chain_id":""}),
            json!({"interface_id":"nic-a", "chain_id":"chain-a"}),
        ] {
            assert!(serde_json::from_value::<RouteTarget>(wire).is_err());
        }
        assert_eq!(
            serde_json::to_value(RouteTarget::interface("nic-a")).unwrap(),
            json!({"interface_id":"nic-a"})
        );
    }
}
