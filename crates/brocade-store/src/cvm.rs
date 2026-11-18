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
mod host_policy;
mod instances;
mod interfaces;
mod monitoring;
mod network_quota;
pub(crate) mod nics;
mod node_state;
mod operations;
mod quota;
mod readings;
pub(crate) mod releases;
mod terminal;
mod types;

use access::*;
pub use baremetal::CvmAdopt;
pub use host_policy::HostPolicyUpdate;
use instances::*;
use interfaces::*;
pub use monitoring::{CvmMonitoring, MonitoringSample};
pub use network_quota::{
    CvmHostNetworkQuota, CvmHostNetworkQuotaChange, CvmNetworkQuotaPreview,
    CvmNetworkQuotaPreviewRequest,
};
pub use nics::{
    CvmEntryAddressUse, CvmEntryBoard, CvmEntryPortUse, CvmEntryPublicationView, CvmEntryState,
    CvmEntryView, CvmNetworkLogDomain, CvmNetworkLogs, CvmNetworkNodeLogs, CvmNetworkProvider,
    CvmNicBindingView, CvmNicCatalog, CvmNicState,
};
use operations::*;
use quota::*;
pub use readings::{CvmCardReading, CvmCardSample, CvmReadings};
pub use terminal::TerminalPolicyUpdate;
pub use types::*;
