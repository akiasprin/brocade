use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use brocade_core::model::{
    grant_label, is_probe_label, parse_grant_label, Action, HopDial, ModelSnapshot,
};
use brocade_deployment::plan::{DesiredArtifact, DesiredGrants, NodeDesiredState, PlannedAction};

use brocade_deployment::protocol::{
    UsageChainSample, UsageMonthlySummary, UsageMonthlyViewRow, UsageNodeBucket, UsageNodeSeries,
    UsageNodeSeriesList, UsageReportRequest, UsageReportResult, UsageSample, UsageSampleList,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, Result, StoreError};

/// How far the agent's reported instant may diverge from the control plane's clock and still
/// count. Beyond it the whole round is refused — the window boundary decides which month these
/// bytes land in, and month boundaries are where bills divide.
const MAX_CLOCK_SKEW_SECS: i64 = 600;

/// Process-local observations for counters which are absent from the reporting generation.
///
/// Xray keeps removed user counters in process memory, so merely seeing an unknown label is not
/// actionable. What matters is whether its absolute byte count keeps moving inside the same exact
/// Xray epoch. This state is deliberately not durable: after a control-plane restart the first
/// report establishes a fresh baseline and the second can prove growth again.
#[derive(Debug, Default)]
pub(crate) struct UsageRuntimeState {
    unknown: Mutex<BTreeMap<String, UnknownNodeCounters>>,
}

#[derive(Debug, Default)]
struct UnknownNodeCounters {
    read_at_unix_secs: i64,
    heads: BTreeMap<[u8; 32], UnknownCounterHead>,
    growing: u64,
}

#[derive(Debug)]
struct UnknownCounterHead {
    xray_epoch: String,
    uplink_bytes: u64,
    downlink_bytes: u64,
}

#[derive(Debug)]
struct UnknownCounterObservation {
    fingerprint: [u8; 32],
    xray_epoch: String,
    uplink_bytes: u64,
    downlink_bytes: u64,
}

impl UsageRuntimeState {
    fn observe_unknown_counters(
        &self,
        node_id: &str,
        read_at_unix_secs: i64,
        observations: Vec<UnknownCounterObservation>,
    ) {
        let mut nodes = self
            .unknown
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = nodes.get(node_id);

        // A delayed spool report may commit after a newer observation. It remains valid billing
        // input, but must not rewind this explicitly latest-round operational indicator.
        if previous.is_some_and(|state| read_at_unix_secs <= state.read_at_unix_secs) {
            return;
        }

        let mut heads = BTreeMap::new();
        let mut growing = 0_u64;
        for observation in observations {
            if previous
                .and_then(|state| state.heads.get(&observation.fingerprint))
                .is_some_and(|head| {
                    head.xray_epoch == observation.xray_epoch
                        && observation.uplink_bytes >= head.uplink_bytes
                        && observation.downlink_bytes >= head.downlink_bytes
                        && (observation.uplink_bytes > head.uplink_bytes
                            || observation.downlink_bytes > head.downlink_bytes)
                })
            {
                growing += 1;
            }
            heads.insert(
                observation.fingerprint,
                UnknownCounterHead {
                    xray_epoch: observation.xray_epoch,
                    uplink_bytes: observation.uplink_bytes,
                    downlink_bytes: observation.downlink_bytes,
                },
            );
        }
        nodes.insert(
            node_id.to_owned(),
            UnknownNodeCounters {
                read_at_unix_secs,
                heads,
                growing,
            },
        );
    }

    pub(crate) fn growing_unknown_counters(&self, node_id: &str) -> Option<u64> {
        self.unknown
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(node_id)
            .map(|state| state.growing)
    }
}

fn unknown_counter_fingerprint(node_id: &str, label: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(node_id.as_bytes());
    digest.update([0]);
    digest.update(label.as_bytes());
    digest.finalize().into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum FrozenBinding {
    Foreign,
    User {
        tenant_id: String,
        user_id: String,
        ingress_id: String,
        app_id: String,
    },
    ChainHop {
        tenant_id: String,
        app_id: String,
        chain_id: String,
        node_id: String,
    },
}

impl FrozenBinding {
    fn to_counter_owner(&self, label: &str) -> Option<CounterOwner> {
        match self {
            Self::Foreign => None,
            Self::User {
                tenant_id,
                user_id,
                ingress_id,
                app_id,
            } => Some(CounterOwner::User(GrantMapping {
                tenant_id: tenant_id.clone(),
                user_id: user_id.clone(),
                ingress_id: ingress_id.clone(),
                app_id: app_id.clone(),
                label: label.to_owned(),
            })),
            Self::ChainHop {
                tenant_id,
                app_id,
                chain_id,
                node_id,
            } => Some(CounterOwner::ChainHop(ChainHopMapping {
                tenant_id: tenant_id.clone(),
                app_id: app_id.clone(),
                chain_id: chain_id.clone(),
                node_id: node_id.clone(),
                label: label.to_owned(),
            })),
        }
    }
}

#[derive(Debug)]
struct UsageGeneration {
    id: i64,
    deployment_id: Option<i64>,
    revision_id: Option<i64>,
    bindings: BTreeMap<String, FrozenBinding>,
}

fn report_identity(request: &UsageReportRequest) -> Result<(String, u64)> {
    match (&request.agent_instance_id, request.sequence) {
        (Some(instance), Some(sequence))
            if sequence > 0
                && instance.len() == 32
                && instance
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
        {
            Ok((instance.clone(), sequence))
        }
        (None, None) => Ok(("legacy".to_owned(), request.read_at_unix_secs as u64)),
        _ => Err(StoreError::InvalidData(
            "usage agent_instance_id must be 32 lowercase hex bytes and sequence must be positive; both are required together".to_owned(),
        )),
    }
}

/// Freeze the namespace for a target before it can be dispatched. This function reads only the
/// supplied revision snapshot and the target's already-frozen grant list; it never consults live
/// ownership while replaying usage.
pub(crate) async fn create_usage_generation_for_target(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
    node_id: &str,
    snapshot: &ModelSnapshot,
    desired: &NodeDesiredState,
    actions: &[PlannedAction],
) -> Result<Option<i64>> {
    if !actions.iter().any(|action| {
        matches!(
            action,
            PlannedAction::ApplyXray | PlannedAction::DisableXray | PlannedAction::SyncGrants
        )
    }) {
        return Ok(None);
    }
    let disabled = matches!(desired.xray, DesiredArtifact::Disabled { .. });
    let mut bindings = if disabled {
        BTreeMap::new()
    } else {
        frozen_bindings(snapshot, node_id, Some(&desired.grants))
    };
    if let DesiredArtifact::Present { content, .. } = &desired.xray {
        let actual = xray_client_labels(content)?;
        for (label, binding) in &mut bindings {
            if parse_grant_label(label).is_none()
                && matches!(binding, FrozenBinding::ChainHop { .. })
                && !actual.contains(label)
            {
                *binding = FrozenBinding::Foreign;
            }
        }
    }
    let grants_only = actions.contains(&PlannedAction::SyncGrants)
        && !actions.iter().any(|action| {
            matches!(
                action,
                PlannedAction::ApplyXray | PlannedAction::DisableXray
            )
        });
    if grants_only {
        let previous = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT g.bindings
             FROM node_agent_state s
             JOIN usage_generations g ON g.id = s.usage_generation_id
             WHERE s.node_id = $1",
        )
        .bind(node_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(serde_json::from_value::<BTreeMap<String, FrozenBinding>>)
        .transpose()?;
        if let Some(previous) = previous {
            // SyncGrants changes only runtime users; the Xray topology stays exactly as it was in
            // the previous generation, even when a newer unshipped model revision already exists.
            bindings.retain(|label, _| parse_grant_label(label).is_some());
            for (label, owner) in &previous {
                if parse_grant_label(label).is_none() {
                    bindings.insert(label.clone(), owner.clone());
                }
            }
            if let DesiredGrants::Present { inbounds } = &desired.grants {
                for email in inbounds
                    .iter()
                    .flat_map(|inbound| inbound.clients.iter().map(|client| &client.email))
                {
                    if !bindings.contains_key(email) {
                        if let Some(owner) = previous.get(email) {
                            bindings.insert(email.clone(), owner.clone());
                        }
                    }
                }
            }
        }
    }
    let revision_id: i64 = sqlx::query_scalar("SELECT revision_id FROM deployments WHERE id = $1")
        .bind(deployment_id)
        .fetch_one(&mut **tx)
        .await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO usage_generations (node_id, deployment_id, revision_id, bindings)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (deployment_id, node_id) DO UPDATE SET node_id = usage_generations.node_id
         RETURNING id",
    )
    .bind(node_id)
    .bind(deployment_id)
    .bind(revision_id)
    .bind(serde_json::to_value(bindings)?)
    .fetch_one(&mut **tx)
    .await?;
    Ok(Some(id))
}

fn xray_client_labels(content: &str) -> Result<BTreeSet<String>> {
    let value: serde_json::Value = serde_json::from_str(content)?;
    let mut labels = BTreeSet::new();
    for inbound in value
        .get("inbounds")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        for client in inbound
            .get("settings")
            .and_then(|settings| settings.get("clients"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(email) = client.get("email").and_then(serde_json::Value::as_str) {
                labels.insert(email.to_owned());
            }
        }
    }
    Ok(labels)
}

pub(crate) async fn activate_usage_generation(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    generation_id: i64,
    deployment_id: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO usage_generation_activations
             (node_id, generation_id, deployment_id, activated_at)
         VALUES ($1, $2, $3, now())
         ON CONFLICT (node_id, generation_id) DO NOTHING",
    )
    .bind(node_id)
    .bind(generation_id)
    .bind(deployment_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO node_agent_state (node_id, usage_generation_id)
         VALUES ($1, $2)
         ON CONFLICT (node_id) DO UPDATE SET usage_generation_id = EXCLUDED.usage_generation_id",
    )
    .bind(node_id)
    .bind(generation_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn frozen_bindings(
    snapshot: &ModelSnapshot,
    reporter_node: &str,
    desired_grants: Option<&DesiredGrants>,
) -> BTreeMap<String, FrozenBinding> {
    let allowed_users: Option<BTreeSet<String>> = desired_grants.map(|grants| match grants {
        DesiredGrants::Present { inbounds } => inbounds
            .iter()
            .flat_map(|inbound| inbound.clients.iter().map(|client| client.email.clone()))
            .collect(),
        DesiredGrants::Disabled { .. } | DesiredGrants::Unmanaged { .. } => BTreeSet::new(),
    });
    let mut bindings = BTreeMap::new();
    for app in &snapshot.apps {
        for grant in &app.grants {
            let label = grant_label(&grant.user, &grant.tenant, &grant.ingress);
            if allowed_users
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&label))
            {
                continue;
            }
            let binding = if app
                .ingresses
                .iter()
                .any(|ingress| ingress.id == grant.ingress && ingress.node == reporter_node)
            {
                FrozenBinding::User {
                    tenant_id: grant.tenant.clone(),
                    user_id: grant.user.clone(),
                    ingress_id: grant.ingress.clone(),
                    app_id: app.id.clone(),
                }
            } else {
                FrozenBinding::Foreign
            };
            bindings.insert(label, binding);
        }
        for step in &app.steps {
            let Some(accept) = &step.accept else { continue };
            let carried_here = step.node == reporter_node
                || app.steps.iter().any(|portal| {
                    portal.node == reporter_node
                        && portal.chain == step.chain
                        && portal.rules.iter().any(|rule| {
                            matches!(
                                &rule.action,
                                Action::Forward { to, dial: HopDial::Reverse(_), .. }
                                    if to == &step.node
                            )
                        })
                });
            if let Some(chain) = app.chains.iter().find(|chain| chain.id == step.chain) {
                bindings.insert(
                    accept.label.clone(),
                    if carried_here {
                        FrozenBinding::ChainHop {
                            tenant_id: chain.tenant.clone(),
                            app_id: app.id.clone(),
                            chain_id: chain.id.clone(),
                            node_id: step.node.clone(),
                        }
                    } else {
                        FrozenBinding::Foreign
                    },
                );
            }
        }
    }
    bindings
}

async fn resolve_usage_generation(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    explicit: Option<i64>,
    read_at_unix_secs: i64,
) -> Result<UsageGeneration> {
    let row = if let Some(id) = explicit {
        sqlx::query(
            "SELECT id, deployment_id, revision_id, bindings
             FROM usage_generations WHERE id = $1 AND node_id = $2",
        )
        .bind(id)
        .bind(node_id)
        .fetch_optional(&mut **tx)
        .await?
    } else {
        sqlx::query(
            "SELECT g.id, g.deployment_id, g.revision_id, g.bindings
             FROM usage_generation_activations a
             JOIN usage_generations g ON g.id = a.generation_id
             WHERE a.node_id = $1
               AND a.activated_at <= to_timestamp($2::double precision)
             ORDER BY a.activated_at DESC, g.id DESC LIMIT 1",
        )
        .bind(node_id)
        .bind(read_at_unix_secs)
        .fetch_optional(&mut **tx)
        .await?
    };
    let row = match row {
        Some(row) => row,
        None if explicit.is_some() => {
            return Err(StoreError::InvalidData(format!(
                "usage generation {} does not belong to node {node_id}",
                explicit.unwrap_or_default()
            )))
        }
        None => {
            // Rolling upgrade baseline: create one immutable interpretation of the current model.
            // All protocol-v3 targets create their own generation before dispatch, so this branch
            // is used only until the first such target reaches this machine.
            let snapshot = crate::materialize::load_snapshot_tx(tx, None).await?;
            let bindings = frozen_bindings(&snapshot, node_id, None);
            let revision_id = i64::try_from(snapshot.revision).map_err(|_| {
                StoreError::InvalidData("current revision is out of range".to_owned())
            })?;
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO usage_generations (node_id, revision_id, bindings)
                 VALUES ($1, $2, $3) RETURNING id",
            )
            .bind(node_id)
            .bind(revision_id)
            .bind(serde_json::to_value(bindings)?)
            .fetch_one(&mut **tx)
            .await?;
            sqlx::query(
                "INSERT INTO usage_generation_activations (node_id, generation_id, activated_at)
                 VALUES ($1, $2, '-infinity'::timestamptz)",
            )
            .bind(node_id)
            .bind(id)
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "SELECT id, deployment_id, revision_id, bindings
                 FROM usage_generations WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&mut **tx)
            .await?
        }
    };
    Ok(UsageGeneration {
        id: row.try_get("id")?,
        deployment_id: row.try_get("deployment_id")?,
        revision_id: row.try_get("revision_id")?,
        bindings: serde_json::from_value(row.try_get("bindings")?)?,
    })
}

pub async fn record_usage_report(
    pool: &PgPool,
    runtime: &UsageRuntimeState,
    node_id: &str,
    mut request: UsageReportRequest,
) -> Result<UsageReportResult> {
    if request.read_at_unix_secs <= 0 || request.xray_started_at_unix_secs <= 0 {
        return Err(StoreError::InvalidData(
            "usage report timestamps must be positive unix seconds".to_owned(),
        ));
    }
    if request.read_at_unix_secs < request.xray_started_at_unix_secs {
        return Err(StoreError::InvalidData(
            "usage report read_at must not be before xray_started_at".to_owned(),
        ));
    }
    if request.counters.len() > 100_000 {
        return Err(StoreError::InvalidData(
            "usage report contains too many counters".to_owned(),
        ));
    }
    if request
        .xray_epoch
        .as_ref()
        .is_some_and(|value| value.trim().is_empty() || value.len() > 128)
    {
        return Err(StoreError::InvalidData(
            "usage xray_epoch must contain 1..128 bytes".to_owned(),
        ));
    }
    if request.usage_generation_id.is_some_and(|id| id <= 0) {
        return Err(StoreError::InvalidData(
            "usage generation id must be positive".to_owned(),
        ));
    }

    // Spool replay is intentionally old. Only a clock in the future is unsafe, because it can
    // put bytes in an accounting period which has not begun. Conflict is retryable by the agent;
    // once NTP repairs the clock, a newer report can move the queue again without losing this one.
    let (server_now,): (i64,) = sqlx::query_as("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(pool)
        .await?;
    let future_secs = request.read_at_unix_secs - server_now;
    if future_secs > MAX_CLOCK_SKEW_SECS {
        return Err(StoreError::Conflict(format!(
            "usage report is {future_secs}s in the future; check the node's clock"
        )));
    }

    let payload_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&request)?));
    let (agent_instance_id, sequence) = report_identity(&request)?;
    let sequence_i64 = u64_to_i64("sequence", sequence)?;
    let has_exact_xray_epoch = request
        .xray_epoch
        .as_ref()
        .is_some_and(|v| !v.trim().is_empty());
    let xray_epoch = request
        .xray_epoch
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "legacy".to_owned());

    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO node_agent_state (node_id) VALUES ($1) ON CONFLICT (node_id) DO NOTHING",
    )
    .bind(node_id)
    .execute(&mut *tx)
    .await?;
    // Machine-level serialization makes lock order independent of counter order and turns the
    // whole report, its receipt and all counter heads into one atomic decision.
    sqlx::query("SELECT node_id FROM node_agent_state WHERE node_id = $1 FOR UPDATE")
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await?;

    if let Some(row) = sqlx::query(
        "SELECT payload_sha256, result FROM usage_report_receipts
         WHERE node_id = $1 AND agent_instance_id = $2 AND sequence = $3",
    )
    .bind(node_id)
    .bind(&agent_instance_id)
    .bind(sequence_i64)
    .fetch_optional(&mut *tx)
    .await?
    {
        let seen: String = row.try_get("payload_sha256")?;
        if seen != payload_sha256 {
            return Err(StoreError::InvalidData(format!(
                "usage report sequence {sequence} was reused with different content"
            )));
        }
        let mut result: UsageReportResult = serde_json::from_value(row.try_get("result")?)?;
        result.duplicate = true;
        tx.commit().await?;
        return Ok(result);
    }
    if let Some(last) = sqlx::query_scalar::<_, i64>(
        "SELECT last_sequence FROM usage_agent_cursors
         WHERE node_id = $1 AND agent_instance_id = $2",
    )
    .bind(node_id)
    .bind(&agent_instance_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        if sequence_i64 <= last {
            return Err(StoreError::InvalidData(format!(
                "usage report sequence {sequence} is behind committed sequence {last}"
            )));
        }
    }

    let generation = resolve_usage_generation(
        &mut tx,
        node_id,
        request.usage_generation_id,
        request.read_at_unix_secs,
    )
    .await?;
    let metadata = UsageMetadata {
        revision_id: generation.revision_id,
        deployment_id: generation.deployment_id,
    };
    let mut accepted_readings = 0_u64;
    let mut inserted_samples = 0_u64;
    let mut skipped_counters = 0_u64;
    let mut rejected_counters = 0_u64;
    let mut gap_samples = 0_u64;
    let mut unknown_counters = Vec::new();
    let route = request.route.clone();
    request.counters.sort_by(|a, b| a.label.cmp(&b.label));
    let mut labels = BTreeSet::new();
    for counter in request.counters {
        let label = counter.label.trim().to_owned();
        if label.is_empty() {
            skipped_counters += 1;
            continue;
        }
        let Ok(uplink_bytes) = u64_to_i64("uplink_bytes", counter.uplink_bytes) else {
            skipped_counters += 1;
            continue;
        };
        let Ok(downlink_bytes) = u64_to_i64("downlink_bytes", counter.downlink_bytes) else {
            skipped_counters += 1;
            continue;
        };
        if !labels.insert(label.clone()) {
            return Err(StoreError::InvalidData(format!(
                "usage report contains duplicate counter label {label}"
            )));
        }
        // The E2E probe deliberately authenticates through the real ingress and therefore owns
        // an Xray `user>>>` counter. It is not a user and is intentionally absent from frozen
        // billing generations; treating it as an unknown label makes every healthy probe look
        // like attribution drift. Keep it outside billing, skipped counts and the in-memory
        // unknown-growth detector alike.
        if is_probe_label(&label) {
            continue;
        }
        let Some(binding) = generation.bindings.get(&label) else {
            skipped_counters += 1;
            if has_exact_xray_epoch {
                unknown_counters.push(UnknownCounterObservation {
                    fingerprint: unknown_counter_fingerprint(node_id, &label),
                    xray_epoch: xray_epoch.clone(),
                    uplink_bytes: counter.uplink_bytes,
                    downlink_bytes: counter.downlink_bytes,
                });
            }
            continue;
        };
        let Some(owner) = binding.to_counter_owner(&label) else {
            rejected_counters += 1;
            continue;
        };
        let canonical_label = match &owner {
            CounterOwner::User(grant) => &grant.label,
            CounterOwner::ChainHop(hop) => &hop.label,
        };

        let previous = usage_head_for_update(&mut tx, node_id, canonical_label).await?;
        let inserted = insert_usage_reading(
            &mut tx,
            UsageReadingInsert {
                node_id,
                label: canonical_label,
                agent_instance_id: &agent_instance_id,
                sequence: sequence_i64,
                generation_id: generation.id,
                xray_epoch: &xray_epoch,
                read_at_unix_secs: request.read_at_unix_secs,
                xray_started_at_unix_secs: request.xray_started_at_unix_secs,
                uplink_bytes,
                downlink_bytes,
            },
        )
        .await?;
        if !inserted {
            return Err(StoreError::InvalidData(format!(
                "duplicate usage reading for {canonical_label}"
            )));
        }
        let mut update_head = true;
        if let Some(previous) = &previous {
            if request.read_at_unix_secs <= previous.read_at_unix_secs {
                rejected_counters += 1;
                update_head = false;
            } else if has_exact_xray_epoch
                && previous.xray_epoch == xray_epoch
                && (uplink_bytes < previous.uplink_bytes
                    || downlink_bytes < previous.downlink_bytes)
            {
                // A counter cannot decrease within one exact process epoch. Preserve the known
                // good head so a later reading can recover; treating this as a restart would bill
                // the entire absolute counter a second time.
                rejected_counters += 1;
                update_head = false;
            } else {
                let restarted = if has_exact_xray_epoch {
                    previous.xray_epoch != xray_epoch
                } else {
                    // A rolling downgrade (or the last queued report from an old Agent) carries
                    // no exact epoch. Do not interpret the literal fallback value `legacy` as a
                    // process change after an exact v3 head: old reports retain the historical
                    // counter-regression rule and therefore cannot rebill a climbing counter.
                    uplink_bytes < previous.uplink_bytes || downlink_bytes < previous.downlink_bytes
                };
                let (window_start, uplink_delta, downlink_delta, has_gap) = if restarted {
                    (
                        previous
                            .read_at_unix_secs
                            .max(request.xray_started_at_unix_secs),
                        uplink_bytes,
                        downlink_bytes,
                        true,
                    )
                } else {
                    (
                        previous.read_at_unix_secs,
                        uplink_bytes - previous.uplink_bytes,
                        downlink_bytes - previous.downlink_bytes,
                        previous.generation_id != generation.id,
                    )
                };
                if window_start < request.read_at_unix_secs {
                    let insert = UsageSampleInsert {
                        node_id,
                        owner: &owner,
                        window_start_unix_secs: window_start,
                        window_end_unix_secs: request.read_at_unix_secs,
                        uplink_bytes: uplink_delta,
                        downlink_bytes: downlink_delta,
                        has_gap,
                        revision_id: metadata.revision_id,
                        deployment_id: metadata.deployment_id,
                        generation_id: generation.id,
                    };
                    let sample_inserted = match &owner {
                        CounterOwner::User(_) => insert_usage_sample(&mut tx, insert).await?,
                        CounterOwner::ChainHop(_) => insert_chain_sample(&mut tx, insert).await?,
                    };
                    if sample_inserted {
                        inserted_samples += 1;
                        if has_gap {
                            gap_samples += 1;
                        }
                    }
                }
            }
        }
        if update_head {
            accepted_readings += 1;
            upsert_usage_head(
                &mut tx,
                node_id,
                canonical_label,
                &xray_epoch,
                request.read_at_unix_secs,
                request.xray_started_at_unix_secs,
                uplink_bytes,
                downlink_bytes,
                generation.id,
                &agent_instance_id,
                sequence_i64,
            )
            .await?;
        }
    }

    let result = UsageReportResult {
        node_id: node_id.to_owned(),
        agent_instance_id: request.agent_instance_id.clone(),
        sequence: request.sequence,
        usage_generation_id: Some(generation.id),
        duplicate: false,
        accepted_readings,
        inserted_samples,
        skipped_counters,
        rejected_counters,
        gap_samples,
    };
    let result_json = serde_json::to_value(&result)?;
    sqlx::query(
        "INSERT INTO usage_report_receipts
             (node_id, agent_instance_id, sequence, payload_sha256, generation_id, read_at, result)
         VALUES ($1, $2, $3, $4, $5, to_timestamp($6::double precision), $7)",
    )
    .bind(node_id)
    .bind(&agent_instance_id)
    .bind(sequence_i64)
    .bind(&payload_sha256)
    .bind(generation.id)
    .bind(request.read_at_unix_secs)
    .bind(&result_json)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO usage_agent_cursors (node_id, agent_instance_id, last_sequence)
         VALUES ($1, $2, $3)
         ON CONFLICT (node_id, agent_instance_id) DO UPDATE SET
            last_sequence = EXCLUDED.last_sequence, updated_at = now()",
    )
    .bind(node_id)
    .bind(&agent_instance_id)
    .bind(sequence_i64)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO node_agent_state (
            node_id, last_usage_report_at, xray_started_at, route_ipv4, route_ipv6,
            usage_generation_id, usage_last_result
         )
         VALUES (
            $1, now(), to_timestamp($2::double precision),
            CASE WHEN $3::boolean THEN $4 ELSE NULL END,
            CASE WHEN $3::boolean THEN $5 ELSE NULL END,
            $6, $7
         )
         ON CONFLICT (node_id) DO UPDATE SET
            last_usage_report_at = EXCLUDED.last_usage_report_at,
            xray_started_at = EXCLUDED.xray_started_at,
            route_ipv4 = CASE WHEN $3::boolean THEN EXCLUDED.route_ipv4 ELSE node_agent_state.route_ipv4 END,
            route_ipv6 = CASE WHEN $3::boolean THEN EXCLUDED.route_ipv6 ELSE node_agent_state.route_ipv6 END,
            usage_generation_id = EXCLUDED.usage_generation_id,
            usage_last_result = EXCLUDED.usage_last_result",
    )
    .bind(node_id)
    .bind(request.xray_started_at_unix_secs)
    .bind(route.is_some())
    .bind(route.as_ref().and_then(|route| route.ipv4.as_deref()))
    .bind(route.as_ref().and_then(|route| route.ipv6.as_deref()))
    .bind(generation.id)
    .bind(result_json)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    runtime.observe_unknown_counters(node_id, request.read_at_unix_secs, unknown_counters);

    Ok(result)
}

/// What the bar chart at the right of the machine list and its "this month" reading need: a
/// per-machine aggregated usage series plus the month's running total.
///
/// Why `/usage/samples` is not reused: that returns detail rows (one row = one window × one
/// grant), so sixteen machines over ten minutes is `16 × 20 × users × ingresses` rows against a
/// limit capped at 500. Aggregating here yields everything the page needs in one query.
///
/// It goes through the `node_usage_windows` view (the machine-dimension union of both sample
/// families) rather than touching the two underlying tables: "how much did this machine carry"
/// has to count both an ingress's user traffic and a relay's link hops — ingress and relay are
/// merely one machine's roles on different chains. The first version queried only
/// `usage_samples`, and every relay machine's bar chart was empty.
///
/// `window_secs` sets how long the series is (not the bucket width — the buckets are the agent's
/// reporting windows themselves).
pub async fn list_usage_node_series(
    pool: &PgPool,
    actor: &AdminContext,
    window_secs: u32,
    node_id: Option<&str>,
) -> Result<UsageNodeSeriesList> {
    // Capped at 24 hours: anything longer belongs in a rollup table rather than scanning
    // detail rows window by window.
    let window_secs = i64::from(window_secs.clamp(60, 86_400));
    let tenant_scope = actor.tenant_scope();
    let tenant_pattern = actor.tenant_scope_like_pattern();

    // Month boundaries follow list_monthly_usage_summary: calendar months at +08, decided
    // server-side and never sent by the UI. wall_start is the human-facing wall-clock string,
    // inst_start carries the offset and is used only for filtering.
    let (wall_month_start, inst_month_start): (String, String) = sqlx::query_as(
        "SELECT to_char(date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong'),
                        'YYYY-MM-DD HH24:MI:SS'),
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                 AT TIME ZONE 'Asia/Hong_Kong')::text",
    )
    .fetch_one(pool)
    .await?;

    let (since,): (String,) =
        sqlx::query_as("SELECT (now() - make_interval(secs => $1::double precision))::text")
            .bind(window_secs as f64)
            .fetch_one(pool)
            .await?;

    // The window series. The two tables are UNIONed and aggregated by (node_id, window_end) —
    // window_end is the right edge of the agent's reporting window, both sides write the same
    // boundary for one report, and no further bucketing is needed. User traffic and relay
    // traffic are given in separate columns (FILTER): an ingress machine's bytes belong to a
    // user, a relay machine's belong to a link hop with no user dimension, and querying only
    // usage_samples leaves a relay machine's bar chart forever empty — while it is plainly
    // forwarding.
    let bucket_rows = sqlx::query(
        "SELECT node_id,
                window_end::text AS window_end,
                coalesce(sum(uplink_bytes) FILTER (WHERE kind = 'user'), 0)::bigint
                    AS user_uplink_bytes,
                coalesce(sum(downlink_bytes) FILTER (WHERE kind = 'user'), 0)::bigint
                    AS user_downlink_bytes,
                coalesce(sum(uplink_bytes) FILTER (WHERE kind = 'relay'), 0)::bigint
                    AS relay_uplink_bytes,
                coalesce(sum(downlink_bytes) FILTER (WHERE kind = 'relay'), 0)::bigint
                    AS relay_downlink_bytes
         FROM node_usage_windows
         WHERE window_end >= $1::timestamptz
           AND ($2::text IS NULL OR tenant_id = $2 OR tenant_id LIKE $3 ESCAPE '\\')
           AND ($4::text IS NULL OR node_id = $4)
         GROUP BY node_id, window_end
         ORDER BY node_id, window_end",
    )
    .bind(&since)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .bind(node_id)
    .fetch_all(pool)
    .await?;

    let month_rows = sqlx::query(
        "SELECT node_id,
                coalesce(sum(uplink_bytes) FILTER (WHERE kind = 'user'), 0)::bigint
                    AS user_uplink_bytes,
                coalesce(sum(downlink_bytes) FILTER (WHERE kind = 'user'), 0)::bigint
                    AS user_downlink_bytes,
                coalesce(sum(uplink_bytes) FILTER (WHERE kind = 'relay'), 0)::bigint
                    AS relay_uplink_bytes,
                coalesce(sum(downlink_bytes) FILTER (WHERE kind = 'relay'), 0)::bigint
                    AS relay_downlink_bytes,
                bool_or(has_gap) AS has_gap
         FROM node_usage_windows
         WHERE window_start >= $1::timestamptz
           AND ($2::text IS NULL OR tenant_id = $2 OR tenant_id LIKE $3 ESCAPE '\\')
           AND ($4::text IS NULL OR node_id = $4)
         GROUP BY node_id",
    )
    .bind(&inst_month_start)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .bind(node_id)
    .fetch_all(pool)
    .await?;

    // The two queries merge on node_id. A machine with a monthly total but no recent samples
    // (it ran this month and just stopped) must still appear in the result, or the UI treats it
    // as never enrolled.
    let blank = |node_id: String| UsageNodeSeries {
        node_id,
        buckets: Vec::new(),
        month_user_uplink_bytes: 0,
        month_user_downlink_bytes: 0,
        month_relay_uplink_bytes: 0,
        month_relay_downlink_bytes: 0,
        month_has_gap: false,
    };
    let mut by_node: BTreeMap<String, UsageNodeSeries> = BTreeMap::new();
    for row in &bucket_rows {
        let node_id: String = row.try_get("node_id")?;
        by_node
            .entry(node_id.clone())
            .or_insert_with(|| blank(node_id))
            .buckets
            .push(UsageNodeBucket {
                window_end: row.try_get("window_end")?,
                user_uplink_bytes: i64_to_u64(
                    "user_uplink_bytes",
                    row.try_get("user_uplink_bytes")?,
                )?,
                user_downlink_bytes: i64_to_u64(
                    "user_downlink_bytes",
                    row.try_get("user_downlink_bytes")?,
                )?,
                relay_uplink_bytes: i64_to_u64(
                    "relay_uplink_bytes",
                    row.try_get("relay_uplink_bytes")?,
                )?,
                relay_downlink_bytes: i64_to_u64(
                    "relay_downlink_bytes",
                    row.try_get("relay_downlink_bytes")?,
                )?,
            });
    }
    for row in &month_rows {
        let node_id: String = row.try_get("node_id")?;
        let entry = by_node
            .entry(node_id.clone())
            .or_insert_with(|| blank(node_id));
        entry.month_user_uplink_bytes =
            i64_to_u64("user_uplink_bytes", row.try_get("user_uplink_bytes")?)?;
        entry.month_user_downlink_bytes =
            i64_to_u64("user_downlink_bytes", row.try_get("user_downlink_bytes")?)?;
        entry.month_relay_uplink_bytes =
            i64_to_u64("relay_uplink_bytes", row.try_get("relay_uplink_bytes")?)?;
        entry.month_relay_downlink_bytes =
            i64_to_u64("relay_downlink_bytes", row.try_get("relay_downlink_bytes")?)?;
        entry.month_has_gap = row.try_get("has_gap")?;
    }

    Ok(UsageNodeSeriesList {
        since,
        month_start: wall_month_start,
        nodes: by_node.into_values().collect(),
    })
}

/// Delete raw readings past their retention.
///
/// `usage_readings` is audit history only; the non-expiring baseline lives in
/// `usage_counter_heads`. This split is what makes retention safe even when a label is quiet for
/// longer than the retention window. Idempotency receipts use the same retention: the on-disk
/// spool holds six hours, while the minimum here is one day; the permanent cursor still refuses
/// an older sequence after its detailed response has expired.
///
/// Seven days rather than only the most recent row: the difference needs one, and the extra week
/// is so that accounts can be reconciled after an incident (when a window's figure does not add
/// up, the raw cumulative values are the only thing that can reconstruct the truth).
///
/// Repetition is safe and several control-plane instances running at once is fine — DELETE is
/// idempotent.
pub async fn prune_usage_readings(pool: &PgPool, retain_days: u32) -> Result<u64> {
    let retain_days = i64::from(retain_days.clamp(1, 365));
    let mut tx = pool.begin().await?;
    let result = sqlx::query(
        "DELETE FROM usage_readings
         WHERE read_at < now() - make_interval(days => $1::int)",
    )
    .bind(retain_days as i32)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM usage_report_receipts
         WHERE received_at < now() - make_interval(days => $1::int)",
    )
    .bind(retain_days as i32)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(result.rows_affected())
}

pub async fn list_usage_samples(
    pool: &PgPool,
    actor: &AdminContext,
    limit: u32,
    tenant_id: Option<&str>,
    user_id: Option<&str>,
    node_id: Option<&str>,
) -> Result<UsageSampleList> {
    let limit = i64::from(limit.clamp(1, 500));
    let tenant_scope = actor.tenant_scope();
    let tenant_pattern = actor.tenant_scope_like_pattern();
    let rows = sqlx::query(
        "SELECT id,
                sampled_at::text AS sampled_at,
                window_start::text AS window_start,
                window_end::text AS window_end,
                node_id,
                tenant_id,
                user_id,
                ingress_id,
                grant_label,
                uplink_bytes,
                downlink_bytes,
                has_gap,
                revision_id,
                deployment_id
         FROM usage_samples
         WHERE ($2::text IS NULL OR tenant_id = $2)
           AND ($3::text IS NULL OR user_id = $3)
           AND ($4::text IS NULL OR node_id = $4)
           AND (
                $5::text IS NULL
                OR tenant_id = $5
                OR tenant_id LIKE $6 ESCAPE '\\'
           )
         ORDER BY window_end DESC, id DESC
         LIMIT $1",
    )
    .bind(limit)
    .bind(tenant_id)
    .bind(user_id)
    .bind(node_id)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(pool)
    .await?;

    let samples = rows
        .iter()
        .map(|row| {
            let revision_id = row
                .try_get::<Option<i64>, _>("revision_id")?
                .map(revision_to_u64)
                .transpose()?;
            Ok(UsageSample {
                id: row.try_get("id")?,
                sampled_at: row.try_get("sampled_at")?,
                window_start: row.try_get("window_start")?,
                window_end: row.try_get("window_end")?,
                node_id: row.try_get("node_id")?,
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
                ingress_id: row.try_get("ingress_id")?,
                grant_label: row.try_get("grant_label")?,
                uplink_bytes: i64_to_u64("uplink_bytes", row.try_get("uplink_bytes")?)?,
                downlink_bytes: i64_to_u64("downlink_bytes", row.try_get("downlink_bytes")?)?,
                has_gap: row.try_get("has_gap")?,
                revision_id,
                deployment_id: row.try_get("deployment_id")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    // Link hops are returned alongside user samples but in separate fields. Filtering by user_id
    // returns none of them: that filter means "how much did this person use", and link overhead
    // belongs to nobody.
    let chain_samples = if user_id.is_some() {
        Vec::new()
    } else {
        list_chain_samples(
            pool,
            limit,
            tenant_id,
            node_id,
            tenant_scope,
            &tenant_pattern,
        )
        .await?
    };

    Ok(UsageSampleList {
        samples,
        chain_samples,
    })
}

/// The calendar-month rollup: one row per user across all their access points (usage_samples
/// writes only granted rows, so a per-user sum is naturally the traffic of their view's access
/// points). Months are the calendar months of the control plane's local zone (+08), decided
/// server-side and never sent by the UI. Tenant-subtree scoping matches
/// list_usage_samples.
pub async fn list_monthly_usage_summary(
    pool: &PgPool,
    actor: &AdminContext,
) -> Result<UsageMonthlySummary> {
    let tenant_scope = actor.tenant_scope();
    let tenant_pattern = actor.tenant_scope_like_pattern();
    // Month boundaries need two representations:
    //  - wall_*: +08 wall-clock strings, human-facing (the month_start/month_end response). A
    //    timestamptz's ::text cannot be handed over directly — that rendering follows the
    //    database session's timezone, and under a UTC session midnight on 1 August renders as
    //    "2026-07-31 16:00:00+00", from which the UI slices 2026-07.
    //  - inst_*: timestamptz text (carrying an offset, unambiguous to parse), used only to
    //    filter samples.
    let (wall_start, wall_end, inst_start, inst_end): (String, String, String, String) =
        sqlx::query_as(
            "SELECT to_char(date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong'),
                        'YYYY-MM-DD HH24:MI:SS'),
                to_char(date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                        + INTERVAL '1 month', 'YYYY-MM-DD HH24:MI:SS'),
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                 AT TIME ZONE 'Asia/Hong_Kong')::text,
                ((date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                  + INTERVAL '1 month') AT TIME ZONE 'Asia/Hong_Kong')::text",
        )
        .fetch_one(pool)
        .await?;
    // View attribution prefers the sample's own column (frozen at insert), falling back to
    // deriving it through a JOIN only for older samples. LEFT JOIN rather than INNER: where the
    // frozen column has a value, the group still resolves even after the ingress was deleted —
    // INNER would discard those samples entirely, presenting as consumption inexplicably losing
    // a chunk.
    let rows = sqlx::query(
        "WITH monthly AS (
             SELECT s.tenant_id,
                    s.user_id,
                    coalesce(s.app_id, i.app_id) AS app_id,
                    sum(s.uplink_bytes)::bigint AS uplink_bytes,
                    sum(s.downlink_bytes)::bigint AS downlink_bytes,
                    bool_or(s.has_gap) AS has_gap
             FROM usage_samples s
             LEFT JOIN ingresses i ON i.id = s.ingress_id
             WHERE s.window_start >= $1::timestamptz
               AND s.window_start < $2::timestamptz
               AND coalesce(s.app_id, i.app_id) IS NOT NULL
               AND (
                    $3::text IS NULL
                    OR s.tenant_id = $3
                    OR s.tenant_id LIKE $4 ESCAPE '\\'
               )
             GROUP BY s.tenant_id, s.user_id, coalesce(s.app_id, i.app_id)
         )
         SELECT monthly.tenant_id, monthly.user_id, monthly.app_id,
                monthly.uplink_bytes, monthly.downlink_bytes, monthly.has_gap
         FROM monthly
         LEFT JOIN apps a ON a.id = monthly.app_id
         ORDER BY monthly.tenant_id, monthly.user_id,
                  a.position NULLS LAST, monthly.app_id",
    )
    .bind(&inst_start)
    .bind(&inst_end)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(pool)
    .await?;

    let views = rows
        .iter()
        .map(|row| {
            Ok(UsageMonthlyViewRow {
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
                app_id: row.try_get("app_id")?,
                uplink_bytes: i64_to_u64("uplink_bytes", row.try_get("uplink_bytes")?)?,
                downlink_bytes: i64_to_u64("downlink_bytes", row.try_get("downlink_bytes")?)?,
                has_gap: row.try_get("has_gap")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(UsageMonthlySummary {
        month_start: wall_start,
        month_end: wall_end,
        views,
    })
}

async fn list_chain_samples(
    pool: &PgPool,
    limit: i64,
    tenant_id: Option<&str>,
    node_id: Option<&str>,
    tenant_scope: Option<&str>,
    tenant_pattern: &Option<String>,
) -> Result<Vec<UsageChainSample>> {
    let rows = sqlx::query(
        "SELECT id,
                sampled_at::text AS sampled_at,
                window_start::text AS window_start,
                window_end::text AS window_end,
                node_id,
                tenant_id,
                app_id,
                chain_id,
                hop_label,
                uplink_bytes,
                downlink_bytes,
                has_gap,
                revision_id,
                deployment_id
         FROM usage_chain_samples
         WHERE ($2::text IS NULL OR tenant_id = $2)
           AND ($3::text IS NULL OR node_id = $3)
           AND (
                $4::text IS NULL
                OR tenant_id = $4
                OR tenant_id LIKE $5 ESCAPE '\\'
           )
         ORDER BY window_end DESC, id DESC
         LIMIT $1",
    )
    .bind(limit)
    .bind(tenant_id)
    .bind(node_id)
    .bind(tenant_scope)
    .bind(tenant_pattern)
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(|row| {
            let revision_id = row
                .try_get::<Option<i64>, _>("revision_id")?
                .map(revision_to_u64)
                .transpose()?;
            Ok(UsageChainSample {
                id: row.try_get("id")?,
                sampled_at: row.try_get("sampled_at")?,
                window_start: row.try_get("window_start")?,
                window_end: row.try_get("window_end")?,
                node_id: row.try_get("node_id")?,
                tenant_id: row.try_get("tenant_id")?,
                app_id: row.try_get("app_id")?,
                chain_id: row.try_get("chain_id")?,
                hop_label: row.try_get("hop_label")?,
                uplink_bytes: i64_to_u64("uplink_bytes", row.try_get("uplink_bytes")?)?,
                downlink_bytes: i64_to_u64("downlink_bytes", row.try_get("downlink_bytes")?)?,
                has_gap: row.try_get("has_gap")?,
                revision_id,
                deployment_id: row.try_get("deployment_id")?,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct UsageMetadata {
    revision_id: Option<i64>,
    deployment_id: Option<i64>,
}

#[derive(Debug)]
struct GrantMapping {
    tenant_id: String,
    user_id: String,
    ingress_id: String,
    app_id: String,
    label: String,
}

/// A relay hop's attribution. There is no user — this hop's credential is `{chain}@{node}`, and
/// that family of labels has no per-user dimension to begin with.
#[derive(Debug)]
struct ChainHopMapping {
    tenant_id: String,
    app_id: String,
    chain_id: String,
    label: String,
    /// Which machine this hop counts against. Usually the reporting one, though not under
    /// `HopDial::Reverse`: there the bytes were read by the portal machine while the hop belongs
    /// to the bridge.
    node_id: String,
}

#[derive(Debug)]
enum CounterOwner {
    User(GrantMapping),
    ChainHop(ChainHopMapping),
}

/// The baseline the next reading is differenced against. No start time among the fields: the
/// counters alone decide whether xray restarted (see `record_usage_report`), and keeping a
/// timestamp here would invite that decision to be made on it again.
#[derive(Debug)]
struct UsageHead {
    read_at_unix_secs: i64,
    uplink_bytes: i64,
    downlink_bytes: i64,
    xray_epoch: String,
    generation_id: i64,
}

async fn usage_head_for_update(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    label: &str,
) -> Result<Option<UsageHead>> {
    let row = sqlx::query(
        "SELECT EXTRACT(EPOCH FROM read_at)::bigint AS read_at_unix_secs,
                uplink_bytes, downlink_bytes, xray_epoch, generation_id
         FROM usage_counter_heads
         WHERE node_id = $1 AND label = $2
         FOR UPDATE",
    )
    .bind(node_id)
    .bind(label)
    .fetch_optional(&mut **tx)
    .await?;

    row.map(|row| {
        Ok(UsageHead {
            read_at_unix_secs: row.try_get("read_at_unix_secs")?,
            uplink_bytes: row.try_get("uplink_bytes")?,
            downlink_bytes: row.try_get("downlink_bytes")?,
            xray_epoch: row.try_get("xray_epoch")?,
            generation_id: row.try_get("generation_id")?,
        })
    })
    .transpose()
}

#[allow(clippy::too_many_arguments)]
async fn upsert_usage_head(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    label: &str,
    xray_epoch: &str,
    read_at_unix_secs: i64,
    xray_started_at_unix_secs: i64,
    uplink_bytes: i64,
    downlink_bytes: i64,
    generation_id: i64,
    agent_instance_id: &str,
    sequence: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO usage_counter_heads (
            node_id, label, xray_epoch, read_at, xray_started_at,
            uplink_bytes, downlink_bytes, generation_id, agent_instance_id, sequence
         ) VALUES (
            $1, $2, $3, to_timestamp($4::double precision),
            to_timestamp($5::double precision), $6, $7, $8, $9, $10
         )
         ON CONFLICT (node_id, label) DO UPDATE SET
            xray_epoch = EXCLUDED.xray_epoch,
            read_at = EXCLUDED.read_at,
            xray_started_at = EXCLUDED.xray_started_at,
            uplink_bytes = EXCLUDED.uplink_bytes,
            downlink_bytes = EXCLUDED.downlink_bytes,
            generation_id = EXCLUDED.generation_id,
            agent_instance_id = EXCLUDED.agent_instance_id,
            sequence = EXCLUDED.sequence",
    )
    .bind(node_id)
    .bind(label)
    .bind(xray_epoch)
    .bind(read_at_unix_secs)
    .bind(xray_started_at_unix_secs)
    .bind(uplink_bytes)
    .bind(downlink_bytes)
    .bind(generation_id)
    .bind(agent_instance_id)
    .bind(sequence)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

struct UsageReadingInsert<'a> {
    node_id: &'a str,
    label: &'a str,
    agent_instance_id: &'a str,
    sequence: i64,
    generation_id: i64,
    xray_epoch: &'a str,
    read_at_unix_secs: i64,
    xray_started_at_unix_secs: i64,
    uplink_bytes: i64,
    downlink_bytes: i64,
}

async fn insert_usage_reading(
    tx: &mut Transaction<'_, Postgres>,
    reading: UsageReadingInsert<'_>,
) -> Result<bool> {
    let result = sqlx::query(
        "INSERT INTO usage_readings (
            node_id, label, agent_instance_id, sequence, generation_id, xray_epoch,
            read_at, xray_started_at, uplink_bytes, downlink_bytes
         )
         VALUES (
            $1, $2, $3, $4, $5, $6,
            to_timestamp($7::double precision),
            to_timestamp($8::double precision),
            $9, $10
         )
         ON CONFLICT (node_id, agent_instance_id, sequence, label) DO NOTHING",
    )
    .bind(reading.node_id)
    .bind(reading.label)
    .bind(reading.agent_instance_id)
    .bind(reading.sequence)
    .bind(reading.generation_id)
    .bind(reading.xray_epoch)
    .bind(reading.read_at_unix_secs)
    .bind(reading.xray_started_at_unix_secs)
    .bind(reading.uplink_bytes)
    .bind(reading.downlink_bytes)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

struct UsageSampleInsert<'a> {
    node_id: &'a str,
    owner: &'a CounterOwner,
    window_start_unix_secs: i64,
    window_end_unix_secs: i64,
    uplink_bytes: i64,
    downlink_bytes: i64,
    has_gap: bool,
    revision_id: Option<i64>,
    deployment_id: Option<i64>,
    generation_id: i64,
}

async fn insert_usage_sample(
    tx: &mut Transaction<'_, Postgres>,
    sample: UsageSampleInsert<'_>,
) -> Result<bool> {
    let CounterOwner::User(grant) = sample.owner else {
        return Ok(false);
    };
    // app_id is looked up from ingresses once at insert and frozen, no longer following the
    // model: when an ingress is moved to another view, this row still says which view it counted
    // against at the time. Derived on demand, a quota's numerator would jump wholesale to the new
    // view the instant the model changed.
    // An ingress that cannot be found leaves NULL, and the query side falls back to a JOIN.
    let result = sqlx::query(
        "INSERT INTO usage_samples (
            window_start, window_end, node_id,
            tenant_id, user_id, ingress_id, app_id, grant_label,
            uplink_bytes, downlink_bytes, has_gap,
            revision_id, deployment_id, generation_id
         )
         VALUES (
            to_timestamp($1::double precision),
            to_timestamp($2::double precision),
            $3, $4, $5, $6,
            $7, $8, $9, $10, $11, $12, $13, $14
         )
         ON CONFLICT (node_id, grant_label, window_start, window_end) DO NOTHING",
    )
    .bind(sample.window_start_unix_secs)
    .bind(sample.window_end_unix_secs)
    .bind(sample.node_id)
    .bind(&grant.tenant_id)
    .bind(&grant.user_id)
    .bind(&grant.ingress_id)
    .bind(&grant.app_id)
    .bind(&grant.label)
    .bind(sample.uplink_bytes)
    .bind(sample.downlink_bytes)
    .bind(sample.has_gap)
    .bind(sample.revision_id)
    .bind(sample.deployment_id)
    .bind(sample.generation_id)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

async fn insert_chain_sample(
    tx: &mut Transaction<'_, Postgres>,
    sample: UsageSampleInsert<'_>,
) -> Result<bool> {
    let CounterOwner::ChainHop(hop) = sample.owner else {
        return Ok(false);
    };
    // `hop.node_id` rather than `sample.node_id`: under reverse access the counter was read by
    // the portal machine while the hop is forwarded by the bridge. The raw reading is still
    // recorded against whoever read it (differences must be taken against one source), and only
    // this step, turning it into accounting, attributes it to the hop's owner.
    let result = sqlx::query(
        "INSERT INTO usage_chain_samples (
            window_start, window_end, node_id,
            tenant_id, app_id, chain_id, hop_label,
            uplink_bytes, downlink_bytes, has_gap,
            revision_id, deployment_id, generation_id
         )
         VALUES (
            to_timestamp($1::double precision),
            to_timestamp($2::double precision),
            $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13
         )
         ON CONFLICT (node_id, hop_label, window_start, window_end) DO NOTHING",
    )
    .bind(sample.window_start_unix_secs)
    .bind(sample.window_end_unix_secs)
    .bind(&hop.node_id)
    .bind(&hop.tenant_id)
    .bind(&hop.app_id)
    .bind(&hop.chain_id)
    .bind(&hop.label)
    .bind(sample.uplink_bytes)
    .bind(sample.downlink_bytes)
    .bind(sample.has_gap)
    .bind(sample.revision_id)
    .bind(sample.deployment_id)
    .bind(sample.generation_id)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

fn u64_to_i64(field: &str, value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::InvalidData(format!("{field} is out of range")))
}

fn i64_to_u64(field: &str, value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| StoreError::InvalidData(format!("{field} is out of range")))
}

fn revision_to_u64(revision: i64) -> Result<u64> {
    u64::try_from(revision)
        .map_err(|_| StoreError::InvalidData(format!("revision_id out of range: {revision}")))
}

#[cfg(test)]
mod runtime_tests {
    use super::{unknown_counter_fingerprint, UnknownCounterObservation, UsageRuntimeState};

    fn observation(
        label: &str,
        epoch: &str,
        uplink: u64,
        downlink: u64,
    ) -> UnknownCounterObservation {
        UnknownCounterObservation {
            fingerprint: unknown_counter_fingerprint("n-test", label),
            xray_epoch: epoch.to_owned(),
            uplink_bytes: uplink,
            downlink_bytes: downlink,
        }
    }

    #[test]
    fn unknown_counter_warns_only_when_the_same_epoch_moves() {
        let runtime = UsageRuntimeState::default();
        assert_eq!(runtime.growing_unknown_counters("n-test"), None);

        runtime.observe_unknown_counters(
            "n-test",
            10,
            vec![observation("removed-user", "boot-a:100", 20, 30)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(0));

        runtime.observe_unknown_counters(
            "n-test",
            20,
            vec![observation("removed-user", "boot-a:100", 20, 30)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(0));

        runtime.observe_unknown_counters(
            "n-test",
            30,
            vec![observation("removed-user", "boot-a:100", 21, 30)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(1));

        // The finding describes the latest interval, rather than becoming a sticky alert.
        runtime.observe_unknown_counters(
            "n-test",
            40,
            vec![observation("removed-user", "boot-a:100", 21, 30)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(0));

        // An Xray restart establishes a new baseline; its smaller absolute value is not growth.
        runtime.observe_unknown_counters(
            "n-test",
            50,
            vec![observation("removed-user", "boot-a:900", 1, 2)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(0));
    }

    #[test]
    fn latest_unknown_round_prunes_disappeared_labels_and_ignores_old_replay() {
        let runtime = UsageRuntimeState::default();
        runtime.observe_unknown_counters(
            "n-test",
            20,
            vec![observation("removed-user", "boot-a:100", 10, 10)],
        );
        runtime.observe_unknown_counters(
            "n-test",
            30,
            vec![observation("removed-user", "boot-a:100", 20, 10)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(1));

        // An older queued report is billable history, not the latest operational state.
        runtime.observe_unknown_counters("n-test", 25, Vec::new());
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(1));

        runtime.observe_unknown_counters("n-test", 40, Vec::new());
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(0));

        // Reappearing after absence starts over instead of comparing against a stale head.
        runtime.observe_unknown_counters(
            "n-test",
            50,
            vec![observation("removed-user", "boot-a:100", 30, 10)],
        );
        assert_eq!(runtime.growing_unknown_counters("n-test"), Some(0));
    }
}
