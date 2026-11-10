//! Product identity is independent of protocol versions and executable digests.
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProductFlavor {
    Base,
    Cvm,
}

impl ProductFlavor {
    pub const fn from_cvm(enabled: bool) -> Self {
        if enabled {
            Self::Cvm
        } else {
            Self::Base
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Cvm => "cvm",
        }
    }
}

impl fmt::Display for ProductFlavor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ProductFlavor {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "base" => Ok(Self::Base),
            "cvm" => Ok(Self::Cvm),
            _ => Err("product flavor must be base or cvm"),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ProductFeatures {
    pub cvm: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ProductManifest {
    pub flavor: ProductFlavor,
    pub features: ProductFeatures,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_explicit_and_reject_unknown_variants() {
        for flavor in [ProductFlavor::Base, ProductFlavor::Cvm] {
            assert_eq!(flavor.as_str().parse(), Ok(flavor));
            assert_eq!(
                serde_json::to_string(&flavor).unwrap(),
                format!("\"{flavor}\"")
            );
        }
        assert!("".parse::<ProductFlavor>().is_err());
        assert!(serde_json::from_str::<ProductFlavor>("\"unknown\"").is_err());
    }
}
