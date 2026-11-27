//! CVM ownership, immutable operations and durable capacity reservations.
use crate::{AdminContext, AdminRole, PgStore, Result, StoreError};
use brocade_core::hash::sha256_hex;
use brocade_deployment::cvm::*;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};

mod access;
mod artifacts;
mod chain_egress;
pub use artifacts::{CvmArtifactHistory, CvmMutationResult, CvmMutationReview};
pub use chain_egress::{CvmChainCatalog, CvmChainOption};
mod baremetal;
mod credentials;
mod host_capacity;
mod host_policy;
mod instances;
mod interfaces;
mod monitoring;
mod network_quota;
pub(crate) mod nics;
mod node_state;
mod operations;
mod readings;
mod recovery;
pub(crate) mod releases;
pub(crate) mod retirement;
mod route_match;
pub mod stack;
pub use route_match::{
    CvmRouteBlockReason, CvmRouteMatch, CvmRouteMatchProtocol, CvmRouteMatchRequest,
};
mod terminal;
mod types;

use access::*;
pub use baremetal::CvmAdopt;
pub use host_policy::HostPolicyUpdate;
use instances::*;
use interfaces::*;
pub use monitoring::{CvmMonitoring, MonitoringSample};

use host_capacity::*;
pub use nics::{
    CvmEntryAddressUse, CvmEntryBoard, CvmEntryPortUse, CvmEntryPublicationView, CvmEntryState,
    CvmEntryView, CvmNetworkEditRequest, CvmNetworkEditResult, CvmNetworkEntryAddition,
    CvmNetworkEntryRelease, CvmNetworkLogDomain, CvmNetworkLogs, CvmNetworkMount,
    CvmNetworkNodeLogs, CvmNetworkProvider, CvmNetworkRoutesWrite, CvmNicBindingView,
    CvmNicCatalog, CvmNicState,
};
use operations::*;
pub use readings::{CvmCardReading, CvmCardSample, CvmReadings};
pub use terminal::TerminalPolicyUpdate;
pub use types::*;
