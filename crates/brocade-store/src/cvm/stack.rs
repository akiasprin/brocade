//! Independent, single-host maintenance leases; Cube's root proof stays on the host.
use super::*;
use brocade_deployment::cvm::stack::*;

/// A signed compatibility document verified with the configured operator trust key.
#[derive(Debug)]
pub struct VerifiedStackPair {
    artifact: SignedStackPair,
    pair: StackPair,
}

impl VerifiedStackPair {
    pub fn verify(artifact: SignedStackPair, public_key: &[u8; 32]) -> Result<Self> {
        let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key);
        let pair = artifact
            .checked(|payload, signature| key.verify(payload, signature).is_ok())
            .map_err(invalid)?;
        Ok(Self { artifact, pair })
    }

    pub fn pair(&self) -> &StackPair {
        &self.pair
    }

    pub fn id(&self) -> String {
        sha256_hex(self.artifact.payload.as_bytes())
    }
}

const EXPIRED: &str = "Cube stack maintenance window expired before claim";

#[derive(Debug, Serialize)]
pub struct StackHost {
    pub node_id: String,
    #[serde(flatten)]
    pub installed: Option<StackInstalled>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installable: Option<String>,
    pub observed_at: u64,
}

struct Attempt {
    id: i64,
    node_id: String,
    tenant_id: String,
    artifact: sqlx::types::Json<SignedStackPair>,
    status: String,
    expires_at: i64,
    roots_sha256: Option<String>,
    error: Option<String>,
}

impl sqlx::FromRow<'_, sqlx::postgres::PgRow> for Attempt {
    fn from_row(row: &sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        Ok(Self {
            id: row.try_get("id")?,
            node_id: row.try_get("node_id")?,
            tenant_id: row.try_get("tenant_id")?,
            artifact: row.try_get("artifact")?,
            status: row.try_get("status")?,
            expires_at: row.try_get("expires_at")?,
            roots_sha256: row.try_get("roots_sha256")?,
            error: row.try_get("error")?,
        })
    }
}

impl Attempt {
    fn status(&self) -> Result<StackReleaseStatus> {
        Ok(serde_json::from_value(self.status.clone().into())?)
    }

    fn pair(&self) -> Result<StackPair> {
        // The immutable document was verified against the operator's trust key at admission.
        let pair: StackPair = serde_json::from_str(&self.artifact.payload)?;
        pair.validate().map_err(invalid)?;
        Ok(pair)
    }

    fn offer(&self) -> Result<StackOffer> {
        Ok(StackOffer {
            id: self.id,
            expires_at: self.expires_at as u64,
            status: self.status()?,
            roots_sha256: self.roots_sha256.clone(),
            artifact: self.artifact.0.clone(),
        })
    }

    fn release(&self) -> Result<StackRelease> {
        let pair = self.pair()?;
        Ok(StackRelease {
            id: self.id,
            node_id: self.node_id.clone(),
            source_sha256: pair.source_sha256,
            target_sha256: pair.target_sha256,
            status: self.status()?,
            expires_at: self.expires_at as u64,
            error: self.error.clone(),
            roots_sha256: self.roots_sha256.clone(),
        })
    }
}

impl PgStore {
    pub async fn cvm_stack_overview(
        &self,
        actor: &AdminContext,
    ) -> Result<(Vec<StackHost>, Vec<StackRelease>)> {
        require_admin(actor)?;
        let mut tx = begin(self).await?;
        sqlx::query(
            "WITH expired AS (
                UPDATE cvm_stack_releases r SET status='cancelled', error=$3
                WHERE r.expires_at <= floor(extract(epoch FROM clock_timestamp()))
                  AND r.status IN ('preparing','ready','approved')
                  AND ($1::text IS NULL OR r.tenant_id=$1 OR r.tenant_id LIKE $2)
                RETURNING id, roots_sha256
            ) INSERT INTO cvm_stack_release_events(release_id,actor_kind,actor_id,status,roots_sha256,error)
              SELECT id,'system','expiry','cancelled',roots_sha256,$3 FROM expired",
        )
        .bind(actor.tenant_scope())
        .bind(actor.tenant_scope_like_pattern())
        .bind(EXPIRED)
        .execute(&mut *tx)
        .await?;
        type HostObservationRow = (
            String,
            Option<sqlx::types::Json<StackInstalled>>,
            Option<String>,
            i64,
        );
        let hosts = sqlx::query_as::<_, HostObservationRow>(
            "SELECT o.node_id, o.installed, o.installable, floor(extract(epoch FROM o.observed_at))::bigint
             FROM cvm_stack_observations o JOIN nodes n ON n.id=o.node_id
             JOIN node_lifecycle_state l ON l.node_id=n.id AND l.phase='active'
             LEFT JOIN node_agent_state a ON a.node_id=n.id
             WHERE n.retired_at IS NULL
               AND o.observed_at >= clock_timestamp()-$3*interval '1 second'
               AND (o.installable IS NULL OR o.observed_at >= a.token_created_at)
               AND ($1::text IS NULL OR n.tenant_id=$1 OR n.tenant_id LIKE $2)
             ORDER BY o.node_id",
        )
        .bind(actor.tenant_scope())
        .bind(actor.tenant_scope_like_pattern())
        .bind(STACK_OBSERVATION_MAX_AGE_SECS as i64)
        .fetch_all(&mut *tx)
        .await?;
        let attempts = sqlx::query_as::<_, Attempt>(
            "SELECT r.* FROM cvm_stack_releases r LEFT JOIN nodes n ON n.id=r.node_id
             WHERE ($1::text IS NULL OR coalesce(n.tenant_id,r.tenant_id)=$1
                                     OR coalesce(n.tenant_id,r.tenant_id) LIKE $2)
             ORDER BY (r.status IN ('preparing','ready','approved','applying','blocked')) DESC, r.id DESC
             LIMIT 256",
        )
        .bind(actor.tenant_scope())
        .bind(actor.tenant_scope_like_pattern())
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((
            hosts
                .into_iter()
                .map(|(node_id, installed, installable, observed_at)| StackHost {
                    node_id,
                    installed: installed.map(|value| value.0),
                    installable,
                    observed_at: observed_at as u64,
                })
                .collect(),
            attempts
                .iter()
                .map(Attempt::release)
                .collect::<Result<_>>()?,
        ))
    }

    /// Called only with an Ed25519 document verified by Console. The offer keeps its exact bytes.
    pub async fn create_cvm_stack_release(
        &self,
        actor: &AdminContext,
        node: &str,
        verified: VerifiedStackPair,
        expires_at: u64,
    ) -> Result<StackRelease> {
        require_admin(actor)?;
        if node.is_empty() || node.len() > 150 {
            return Err(invalid("invalid Cube stack node ID"));
        }
        let VerifiedStackPair { artifact, pair } = verified;
        let mut tx = begin(self).await?;
        let tenant = host(&mut tx, node).await?;
        require_scope(actor, &tenant)?;
        let now = now(&mut tx).await?;
        if !(now < expires_at
            && expires_at <= now + MAX_STACK_WINDOW_SECS
            && expires_at <= pair.expires_at)
        {
            return Err(invalid("invalid Cube stack maintenance window"));
        }
        expire_node(&mut tx, node).await?;
        if open_attempt(&mut tx, node).await?.is_some() {
            return Err(conflict("the host already has an open Cube stack release"));
        }
        check_claimable(&mut tx, node, &pair).await?;
        let entry = sqlx::query_as::<_, Attempt>(
            "INSERT INTO cvm_stack_releases(node_id,tenant_id,artifact,status,expires_at)
             VALUES($1,$2,$3,'preparing',$4) RETURNING *",
        )
        .bind(node)
        .bind(tenant)
        .bind(sqlx::types::Json(artifact))
        .bind(expires_at as i64)
        .fetch_one(&mut *tx)
        .await?;
        audit(&mut tx, &entry, "admin", actor.operator_id()).await?;
        tx.commit().await?;
        entry.release()
    }

    pub async fn approve_cvm_stack_release(
        &self,
        actor: &AdminContext,
        id: i64,
    ) -> Result<StackRelease> {
        self.edit_cvm_stack_release(actor, id, true).await
    }

    pub async fn cancel_cvm_stack_release(
        &self,
        actor: &AdminContext,
        id: i64,
    ) -> Result<StackRelease> {
        self.edit_cvm_stack_release(actor, id, false).await
    }

    async fn edit_cvm_stack_release(
        &self,
        actor: &AdminContext,
        id: i64,
        approve: bool,
    ) -> Result<StackRelease> {
        require_admin(actor)?;
        let mut tx = begin(self).await?;
        let mut entry = attempt(&mut tx, id).await?;
        let tenant = sqlx::query_scalar::<_, String>("SELECT tenant_id FROM nodes WHERE id=$1")
            .bind(&entry.node_id)
            .fetch_optional(&mut *tx)
            .await?;
        require_scope(actor, tenant.as_deref().unwrap_or(&entry.tenant_id))?;
        if expire_node(&mut tx, &entry.node_id).await? {
            entry = attempt(&mut tx, id).await?;
        }
        let status = entry.status()?;
        let deadline = entry.expires_at as u64;
        if approve && status == StackReleaseStatus::Ready {
            check_claimable(&mut tx, &entry.node_id, &entry.pair()?).await?;
            if deadline <= now(&mut tx).await? {
                expire_node(&mut tx, &entry.node_id).await?;
                tx.commit().await?;
                return Err(conflict(EXPIRED));
            }
            entry = transition(
                &mut tx,
                &entry,
                "approved",
                entry.roots_sha256.as_deref(),
                None,
                "admin",
                actor.operator_id(),
            )
            .await?;
        } else if !approve
            && matches!(
                status,
                StackReleaseStatus::Preparing
                    | StackReleaseStatus::Ready
                    | StackReleaseStatus::Approved
            )
        {
            entry = transition(
                &mut tx,
                &entry,
                "cancelled",
                entry.roots_sha256.as_deref(),
                None,
                "admin",
                actor.operator_id(),
            )
            .await?;
        } else if !matches!(
            (approve, status),
            (true, StackReleaseStatus::Approved) | (false, StackReleaseStatus::Cancelled)
        ) {
            tx.commit().await?;
            return Err(conflict(
                "the Cube stack release is not in the required state",
            ));
        }
        tx.commit().await?;
        entry.release()
    }

    pub async fn poll_cvm_stack_release(
        &self,
        node: &str,
        poll: StackPoll,
    ) -> Result<Option<StackOffer>> {
        if poll.installed.as_ref().is_some_and(|value| !value.valid()) {
            return Err(invalid("invalid installed Cube stack observation"));
        }
        let installable = poll.installable.map(StackStorage::as_str);
        if poll.installed.is_some() && installable.is_some() {
            return Err(invalid("conflicting Cube stack observations"));
        }
        let mut tx = begin(self).await?;
        if poll.installed.is_some() || installable.is_some() {
            sqlx::query(
                "INSERT INTO cvm_stack_observations(node_id,installed,installable,observed_at)
                 VALUES($1,$2,$3,clock_timestamp()) ON CONFLICT(node_id) DO UPDATE SET
                 installed=excluded.installed, installable=excluded.installable, observed_at=excluded.observed_at",
            )
            .bind(node)
            .bind(poll.installed.as_ref().map(sqlx::types::Json))
            .bind(installable)
            .execute(&mut *tx)
            .await?;
        } else {
            // An unknown host invalidates any previous positive evidence.
            sqlx::query("DELETE FROM cvm_stack_observations WHERE node_id=$1")
                .bind(node)
                .execute(&mut *tx)
                .await?;
        }
        expire_node(&mut tx, node).await?;
        let mut entry = open_attempt(&mut tx, node).await?;
        if let (Some(current), Some(report)) = (&entry, &poll.report) {
            if let Some((status, roots, error)) =
                report_transition(current, report, poll.installed.as_ref(), installable)?
            {
                entry =
                    Some(transition(&mut tx, current, status, roots, error, "agent", node).await?);
            }
        }
        let offer = match &entry {
            Some(entry)
                if matches!(
                    entry.status()?,
                    StackReleaseStatus::Preparing
                        | StackReleaseStatus::Ready
                        | StackReleaseStatus::Approved
                        | StackReleaseStatus::Applying
                        | StackReleaseStatus::Blocked
                ) =>
            {
                // A rejected recovery receipt must leave its lease visible to the Agent.
                Some(entry.offer()?)
            }
            _ => None,
        };
        tx.commit().await?;
        Ok(offer)
    }

    pub async fn claim_cvm_stack_release(
        &self,
        node: &str,
        id: i64,
        claim: StackClaim,
    ) -> Result<StackOffer> {
        if !valid_digest(&claim.roots_sha256) {
            return Err(invalid("invalid Cube stack roots digest"));
        }
        let mut tx = begin(self).await?;
        let mut entry = attempt(&mut tx, id).await?;
        if entry.node_id != node {
            return Err(missing());
        }
        expire_node(&mut tx, node).await?;
        entry = attempt(&mut tx, id).await?;
        let status = entry.status()?;
        if !matches!(
            status,
            StackReleaseStatus::Approved | StackReleaseStatus::Applying
        ) || entry.roots_sha256.as_deref() != Some(&claim.roots_sha256)
        {
            tx.commit().await?;
            return Err(conflict(
                "the Cube stack release is not approved for these roots",
            ));
        }
        // Never expire or reacquire an in-flight lease. A lost response replays the same offer.
        if status == StackReleaseStatus::Approved {
            check_claimable(&mut tx, node, &entry.pair()?).await?;
            if entry.expires_at as u64 <= now(&mut tx).await? {
                expire_node(&mut tx, node).await?;
                tx.commit().await?;
                return Err(conflict(EXPIRED));
            }
            entry = transition(
                &mut tx,
                &entry,
                "applying",
                Some(&claim.roots_sha256),
                None,
                "agent",
                node,
            )
            .await?;
        }
        tx.commit().await?;
        entry.offer()
    }

    pub async fn cvm_stack_bundle_bytes(&self, node: &str, digest: &str) -> Result<u64> {
        if !valid_digest(digest) {
            return Err(missing());
        }
        let mut tx = self.pool.begin().await?;
        let entry = open_attempt(&mut tx, node).await?.ok_or_else(missing)?;
        let pair = entry.pair()?;
        if entry.status()? != StackReleaseStatus::Preparing
            || pair.target_sha256 != digest
            || entry.expires_at as u64 <= now(&mut tx).await?
        {
            return Err(missing());
        }
        tx.commit().await?;
        Ok(pair.target_bytes)
    }
}

async fn begin(store: &PgStore) -> Result<Transaction<'_, Postgres>> {
    let mut tx = store.pool.begin().await?;
    // Deployment claim takes ROW SHARE before its repeatable-read snapshot. This forwards
    // committed maintenance state without running the ordinary claim at a weaker isolation level.
    sqlx::query("LOCK TABLE cvm_stack_releases IN EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    crate::resource_coordination::lock(&mut tx).await?;
    Ok(tx)
}

fn missing() -> StoreError {
    StoreError::NotFound("Cube stack release or bundle".into())
}

fn conflict(message: &str) -> StoreError {
    StoreError::Conflict(message.into())
}

fn require_admin(actor: &AdminContext) -> Result<()> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "Cube stack releases require system-admin".into(),
        ));
    }
    Ok(())
}

fn require_scope(actor: &AdminContext, tenant: &str) -> Result<()> {
    if actor
        .tenant_scope()
        .is_some_and(|scope| !crate::tenant_in_scope(tenant, scope))
    {
        return Err(StoreError::Forbidden(
            "Cube stack host is outside tenant scope".into(),
        ));
    }
    Ok(())
}

async fn now(tx: &mut Transaction<'_, Postgres>) -> Result<u64> {
    let value: i64 =
        sqlx::query_scalar("SELECT floor(extract(epoch FROM clock_timestamp()))::bigint")
            .fetch_one(&mut **tx)
            .await?;
    u64::try_from(value).map_err(|_| invalid("invalid database clock"))
}

async fn host(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<String> {
    sqlx::query_scalar(
        "SELECT n.tenant_id FROM nodes n JOIN node_lifecycle_state l ON l.node_id=n.id
         WHERE n.id=$1 AND n.retired_at IS NULL AND l.phase='active'",
    )
    .bind(node)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::NotFound("active Cube stack host".into()))
}

async fn attempt(tx: &mut Transaction<'_, Postgres>, id: i64) -> Result<Attempt> {
    sqlx::query_as::<_, Attempt>("SELECT * FROM cvm_stack_releases WHERE id=$1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(missing)
}

async fn open_attempt(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<Option<Attempt>> {
    Ok(sqlx::query_as::<_, Attempt>(
        "SELECT * FROM cvm_stack_releases WHERE node_id=$1 AND status IN ('preparing','ready','approved','applying','blocked')",
    )
    .bind(node)
    .fetch_optional(&mut **tx)
    .await?)
}

async fn expire_node(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<bool> {
    Ok(sqlx::query(
        "WITH expired AS (
            UPDATE cvm_stack_releases SET status='cancelled', error=$2
            WHERE node_id=$1 AND expires_at <= floor(extract(epoch FROM clock_timestamp()))
               AND status IN ('preparing','ready','approved')
            RETURNING id, roots_sha256
         ) INSERT INTO cvm_stack_release_events(release_id,actor_kind,actor_id,status,roots_sha256,error)
           SELECT id,'system','expiry','cancelled',roots_sha256,$2 FROM expired",
    )
    .bind(node)
    .bind(EXPIRED)
    .execute(&mut **tx)
    .await?
    .rows_affected() != 0)
}

async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    entry: &Attempt,
    actor_kind: &str,
    actor: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO cvm_stack_release_events(release_id,actor_kind,actor_id,status,roots_sha256,error)
         VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(entry.id)
    .bind(actor_kind)
    .bind(actor)
    .bind(&entry.status)
    .bind(&entry.roots_sha256)
    .bind(&entry.error)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn transition(
    tx: &mut Transaction<'_, Postgres>,
    entry: &Attempt,
    status: &str,
    roots: Option<&str>,
    error: Option<&str>,
    actor_kind: &str,
    actor: &str,
) -> Result<Attempt> {
    let updated = sqlx::query_as::<_, Attempt>(
        "UPDATE cvm_stack_releases SET status=$2, roots_sha256=$3, error=$4 WHERE id=$1 RETURNING *",
    )
    .bind(entry.id)
    .bind(status)
    .bind(roots)
    .bind(error)
    .fetch_one(&mut **tx)
    .await?;
    audit(tx, &updated, actor_kind, actor).await?;
    Ok(updated)
}

type ReportTransition<'a> = (&'static str, Option<&'a str>, Option<&'static str>);

// Only stable labels enter the audit table; agent-provided diagnostics can contain host secrets.
fn report_transition<'a>(
    entry: &Attempt,
    report: &'a StackReport,
    installed: Option<&StackInstalled>,
    installable: Option<&str>,
) -> Result<Option<ReportTransition<'a>>> {
    use StackReleaseStatus::*;
    if entry.id != report.id {
        return Ok(None);
    }
    let roots = report
        .roots_sha256
        .as_deref()
        .filter(|value| valid_digest(value));
    let pair = entry.pair()?;
    let fresh = pair.is_install();
    let claimed_roots = roots.is_some() && roots == entry.roots_sha256.as_deref();
    let status = entry.status()?;
    let result = match (status, report.status) {
        (Preparing, Ready)
            if roots.is_some()
                && (!fresh
                    || roots == Some(pair.storage_backend.empty_roots_sha256().as_str()))
                && matches_preflight(&pair, installed, installable) =>
        {
            ("ready", roots, None)
        }
        (Preparing, Failed) => ("failed", None, Some("Cube stack preparation failed")),
        (Applying, Failed)
            if fresh && claimed_roots && matches_preflight(&pair, installed, installable) =>
        {
            (
                "failed",
                roots,
                Some("Cube stack installation failed before activation"),
            )
        }
        // A blocked first install can complete once the same target and roots are proven.
        (Applying | Blocked, Succeeded)
            if (status == Applying || fresh)
                && claimed_roots
                && installed == Some(&pair.target()) =>
        {
            ("succeeded", roots, None)
        }
        (Applying | Blocked, Recovered)
            if claimed_roots && installed.is_some() && installed == pair.source().as_ref() =>
        {
            (
                "recovered",
                roots,
                Some("Cube stack switch failed; the source stack was restored"),
            )
        }
        (Applying, Blocked) if claimed_roots => (
            "blocked",
            roots,
            Some("Cube stack verification or recovery requires manual intervention"),
        ),
        _ => return Ok(None),
    };
    Ok(Some(result))
}

fn matches_preflight(
    pair: &StackPair,
    installed: Option<&StackInstalled>,
    installable: Option<&str>,
) -> bool {
    if pair.is_install() {
        installed.is_none() && installable == Some(pair.storage_backend.as_str())
    } else {
        installed.is_some() && installed == pair.source().as_ref()
    }
}

async fn check_observation(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    pair: &StackPair,
) -> Result<()> {
    let evidence: Option<(Option<sqlx::types::Json<StackInstalled>>, Option<String>)> =
        sqlx::query_as(
            "SELECT o.installed,o.installable FROM cvm_stack_observations o
             JOIN node_agent_state a ON a.node_id=o.node_id
             WHERE o.node_id=$1 AND o.observed_at >= clock_timestamp()-$2*interval '1 second'
               AND (NOT $3 OR o.observed_at >= a.token_created_at)",
        )
        .bind(node)
        .bind(STACK_OBSERVATION_MAX_AGE_SECS as i64)
        .bind(pair.is_install())
        .fetch_optional(&mut **tx)
        .await?;
    if evidence.is_some_and(|(installed, installable)| {
        matches_preflight(
            pair,
            installed.as_ref().map(|installed| &installed.0),
            installable.as_deref(),
        )
    }) {
        return Ok(());
    }
    Err(conflict(if pair.is_install() {
        "host has no recent first-install evidence for the signed storage backend"
    } else {
        "fresh installed Cube stack does not match the signed source"
    }))
}

async fn check_claimable(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    pair: &StackPair,
) -> Result<()> {
    host(tx, node).await?;
    check_observation(tx, node, pair).await?;
    let settled: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM node_agent_state a
            WHERE a.node_id=$1 AND a.last_poll_at >= clock_timestamp()-$3*interval '1 second'
               AND a.token_hash IS NOT NULL AND a.token_revoked_at IS NULL
               AND a.agent_protocol_version=$4
               AND a.runtime_reported_at >= clock_timestamp()-$3*interval '1 second'
               AND a.xray_upgrade IS NULL
         ) AND ($5 OR EXISTS (
            SELECT 1 FROM cvm_hosts h WHERE h.node_id=$1
               AND h.observed_at >= clock_timestamp()-$3*interval '1 second' AND h.error IS NULL
               AND h.capabilities->>'backend'='cube-kvm-v1' AND h.capabilities->>'cube_storage_backend'=$2
         )) AND NOT EXISTS (SELECT 1 FROM node_operational_isolations WHERE node_id=$1)
           AND NOT EXISTS (
             SELECT 1 FROM node_applied_state WHERE node_id=$1
               AND 'dirty' IN (wireguard_state, xray_state, grants_state, phantun_state, hy2_port_hop_state)
         ) AND NOT EXISTS (
             SELECT 1 FROM binary_release_targets t JOIN binary_releases r ON r.id=t.release_id
             WHERE t.node_id=$1 AND (t.status='dispatched' OR (r.active AND t.status IN ('pending','unverified','failed-dirty')))
         ) AND NOT EXISTS (
             SELECT 1 FROM deployment_targets t JOIN deployments d ON d.id=t.deployment_id
             WHERE t.node_id=$1 AND d.active AND t.status IN
               ('pending','dispatched','converging','failed-recovered','failed-dirty')
         ) AND NOT EXISTS (
              SELECT 1 FROM node_convergence_obligations o JOIN node_lifecycle_state l
                ON l.node_id=o.node_id AND l.lifecycle_epoch=o.lifecycle_epoch
              WHERE o.node_id=$1 AND o.status IN
               ('pending','dispatched','converging','failed-recovered','failed-dirty')
         ) AND NOT EXISTS (
             SELECT 1 FROM cvm_revision_nodes r LEFT JOIN cvm_serving_nodes s ON s.node_id=r.node_id
             WHERE r.node_id=$1 AND (s.revision_id IS NULL OR r.revision_id>s.revision_id)
         )",
    )
    .bind(node)
    .bind(pair.storage_backend.as_str())
    .bind(STACK_OBSERVATION_MAX_AGE_SECS as i64)
    .bind(brocade_deployment::protocol::AGENT_PROTOCOL_VERSION as i32)
    .bind(pair.is_install())
    .fetch_one(&mut **tx)
    .await?;
    if !settled {
        return Err(conflict(
            "the host has unavailable Agent evidence or unsettled release work",
        ));
    }
    if pair.is_install() {
        check_empty_host(tx, node).await
    } else {
        check_stopped(tx, node).await
    }
}

async fn check_empty_host(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<()> {
    // Every instance operation references its instance, including adopted bare-metal instances.
    let assigned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cvm_instances WHERE node_id=$1)
           OR EXISTS(SELECT 1 FROM cvm_revision_nodes WHERE node_id=$1)
           OR EXISTS(SELECT 1 FROM cvm_serving_nodes WHERE node_id=$1)
           OR EXISTS(SELECT 1 FROM cvm_host_policies WHERE node_id=$1 AND enabled)
           OR EXISTS(SELECT 1 FROM cvm_terminal_policies WHERE node_id=$1 AND enabled)
           OR EXISTS(SELECT 1 FROM cvm_nic_publications WHERE provider_node=$1 AND NOT deleted)
           OR EXISTS(SELECT 1 FROM cvm_entry_publications WHERE provider_node=$1 AND NOT deleted)
           OR EXISTS(SELECT 1 FROM cvm_entry_nodes WHERE node_id=$1 AND desired_generation>applied_generation)
           OR EXISTS(SELECT 1 FROM cvm_network_quota_hosts WHERE node_id=$1)",
    )
    .bind(node)
    .fetch_one(&mut **tx)
    .await?;
    if assigned || has_linked_work(tx, node).await? {
        return Err(conflict(
            "first install requires a host without existing CVM assignments or configuration",
        ));
    }
    Ok(())
}

async fn check_stopped(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<()> {
    let unsettled = || {
        conflict("stop all local Cube VMs using the existing power workflow and wait for published stop receipts")
    };
    let serving = releases::serving_tx(tx, node).await?;
    let published = serving
        .as_ref()
        .map_or(&[][..], |config| config.instances.as_slice());
    if has_linked_work(tx, node).await?
        || serving.as_ref().is_some_and(|config| {
            !config.nic.bindings.is_empty()
                || !config.entry.rules.is_empty()
                || !config.entry.deliveries.is_empty()
                || !config.entry.tunnels.is_empty()
                || !config.chain_clients.is_empty()
        })
    {
        return Err(conflict(
            "Cube stack maintenance does not support mixed or linked host configurations",
        ));
    }
    type StoppedInstanceRow = (
        sqlx::types::Json<InstanceDesired>,
        Option<sqlx::types::Json<InstanceReport>>,
        String,
        bool,
    );
    let records = sqlx::query_as::<_, StoppedInstanceRow>(
        "SELECT i.desired,i.report,i.status,i.reserved FROM cvm_instances i WHERE node_id=$1
         AND (reserved OR id=ANY($2)) ORDER BY id",
    )
    .bind(node)
    .bind(published.iter().map(|i| &i.id).collect::<Vec<_>>())
    .fetch_all(&mut **tx)
    .await?;
    if records.len() != published.len() {
        return Err(unsettled());
    }
    for (live, report, status, reserved) in records {
        let Some(frozen) = published.iter().find(|f| f.id == live.id) else {
            return Err(unsettled());
        };
        let Some(report) = report else {
            return Err(unsettled());
        };
        if !live.spec.execution.is_cube()
            || !frozen.spec.execution.is_cube()
            || !matches!(
                (reserved, live.power),
                (true, PowerState::Stopped) | (false, PowerState::Deleted)
            )
            || live.power != frozen.power
            || live.generation != frozen.generation
            || live.spec_sha256 != frozen.spec_sha256
            || live.operation_id != frozen.operation_id
            || status != "applied"
            || report.status != ApplyStatus::Applied
            || report.validate_for(&live).is_err()
            || report.validate_for(frozen).is_err()
        {
            return Err(unsettled());
        }
    }
    Ok(())
}

// These ledgers protect both ends of a linked configuration until cleanup is proven.
async fn has_linked_work(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cvm_nic_bindings WHERE (provider_node=$1 OR consumer_node=$1) AND NOT cleaned)
           OR EXISTS(SELECT 1 FROM cvm_nic_allocations WHERE (provider_node=$1 OR consumer_node=$1) AND NOT released)
           OR EXISTS(SELECT 1 FROM cvm_entry_tunnels WHERE (entry_node=$1 OR target_node=$1) AND NOT cleaned)
           OR EXISTS(SELECT 1 FROM cvm_entries e JOIN cvm_instances i ON i.id=e.instance_id
                     WHERE (e.provider_node=$1 OR i.node_id=$1) AND NOT e.cleaned)",
    )
    .bind(node)
    .fetch_one(&mut **tx)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_completion_requires_exact_receipts_and_never_persists_agent_text() {
        let pair = StackPair {
            contract: "brocade-cvm-stack-cold-pair-v1".into(),
            architecture: "x86_64".into(),
            storage_backend: StackStorage::Plain,
            source_sha256: Some("a".repeat(64)),
            target_sha256: "b".repeat(64),
            target_bytes: 1,
            source_cubelet_sha256: Some("c".repeat(64)),
            source_shim_sha256: Some("d".repeat(64)),
            target_cubelet_sha256: "e".repeat(64),
            target_shim_sha256: "f".repeat(64),
            expires_at: 100,
        };
        let roots = "1".repeat(64);
        let mut entry = attempt_from(&pair, "applying", Some(roots.clone()));
        let mut report = StackReport {
            id: 7,
            status: StackReleaseStatus::Recovered,
            roots_sha256: Some(roots),
            error: Some("sensitive agent text".into()),
        };
        assert!(
            report_transition(&entry, &report, Some(&pair.target()), None)
                .unwrap()
                .is_none()
        );
        let outcome = report_transition(&entry, &report, pair.source().as_ref(), None)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.0, "recovered");
        assert_eq!(
            outcome.2,
            Some("Cube stack switch failed; the source stack was restored")
        );
        report.status = StackReleaseStatus::Succeeded;
        assert!(
            report_transition(&entry, &report, pair.source().as_ref(), None)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            report_transition(&entry, &report, Some(&pair.target()), None)
                .unwrap()
                .unwrap()
                .0,
            "succeeded"
        );
        report.status = StackReleaseStatus::Blocked;
        assert_eq!(
            report_transition(&entry, &report, None, None)
                .unwrap()
                .unwrap()
                .0,
            "blocked"
        );
        report.id = 1;
        assert!(report_transition(&entry, &report, None, None)
            .unwrap()
            .is_none());
        report.id = 7;
        entry.status = "blocked".into();
        report.status = StackReleaseStatus::Succeeded;
        assert!(
            report_transition(&entry, &report, Some(&pair.target()), None)
                .unwrap()
                .is_none()
        );
        report.status = StackReleaseStatus::Recovered;
        assert!(
            report_transition(&entry, &report, Some(&pair.target()), None)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            report_transition(&entry, &report, pair.source().as_ref(), None)
                .unwrap()
                .unwrap()
                .0,
            "recovered"
        );
        report.roots_sha256 = Some("2".repeat(64));
        assert!(
            report_transition(&entry, &report, pair.source().as_ref(), None)
                .unwrap()
                .is_none()
        );
        entry.status = "ready".into();
        report.status = StackReleaseStatus::Failed;
        assert!(report_transition(&entry, &report, None, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn first_install_requires_empty_readiness_and_exact_target_after_claim() {
        use StackReleaseStatus::*;
        let pair = install_pair();
        let mut entry = attempt_from(&pair, "preparing", None);
        let roots = pair.storage_backend.empty_roots_sha256();
        let wrong_roots = StackStorage::Plain.empty_roots_sha256();
        let storage = Some(pair.storage_backend.as_str());
        let mut report = StackReport {
            id: entry.id,
            status: Ready,
            roots_sha256: Some(wrong_roots.clone()),
            error: None,
        };
        assert_eq!(
            report_transition(&entry, &report, None, storage).unwrap(),
            None
        );
        report.roots_sha256 = Some(roots.clone());
        assert_eq!(
            report_transition(&entry, &report, None, None).unwrap(),
            None
        );
        assert_eq!(
            report_transition(&entry, &report, None, storage).unwrap(),
            Some(("ready", Some(roots.as_str()), None))
        );

        entry.roots_sha256 = Some(roots.clone());
        for status in ["applying", "blocked"] {
            entry.status = status.into();
            report.status = Recovered;
            assert_eq!(
                report_transition(&entry, &report, None, storage).unwrap(),
                None
            );
            report.status = Succeeded;
            assert_eq!(
                report_transition(&entry, &report, None, storage).unwrap(),
                None
            );
            report.roots_sha256 = Some(wrong_roots.clone());
            assert_eq!(
                report_transition(&entry, &report, Some(&pair.target()), None).unwrap(),
                None
            );
            report.roots_sha256 = Some(roots.clone());
            assert_eq!(
                report_transition(&entry, &report, Some(&pair.target()), None).unwrap(),
                Some(("succeeded", Some(roots.as_str()), None))
            );
        }
    }

    fn install_pair() -> StackPair {
        StackPair {
            contract: "brocade-cvm-stack-fresh-install-v1".into(),
            architecture: "x86_64".into(),
            storage_backend: StackStorage::Xfs,
            source_sha256: None,
            target_sha256: "b".repeat(64),
            target_bytes: 1,
            source_cubelet_sha256: None,
            source_shim_sha256: None,
            target_cubelet_sha256: "e".repeat(64),
            target_shim_sha256: "f".repeat(64),
            expires_at: 100,
        }
    }

    #[test]
    fn first_install_failure_releases_only_before_activation_with_claimed_roots() {
        let pair = install_pair();
        let roots = pair.storage_backend.empty_roots_sha256();
        let entry = attempt_from(&pair, "applying", Some(roots.clone()));
        let mut report = StackReport {
            id: entry.id,
            status: StackReleaseStatus::Failed,
            roots_sha256: Some(roots.clone()),
            error: Some("sensitive agent text".into()),
        };
        for (installed, installable) in [
            (None, None),
            (None, Some(StackStorage::Plain.as_str())),
            (Some(pair.target()), None),
        ] {
            assert!(
                report_transition(&entry, &report, installed.as_ref(), installable)
                    .unwrap()
                    .is_none()
            );
        }
        let installable = Some(pair.storage_backend.as_str());
        assert_eq!(
            report_transition(&entry, &report, None, installable).unwrap(),
            Some((
                "failed",
                Some(roots.as_str()),
                Some("Cube stack installation failed before activation"),
            ))
        );
        report.roots_sha256 = Some(StackStorage::Plain.empty_roots_sha256());
        assert!(report_transition(&entry, &report, None, installable)
            .unwrap()
            .is_none());
    }

    fn attempt_from(pair: &StackPair, status: &str, roots_sha256: Option<String>) -> Attempt {
        Attempt {
            id: 7,
            node_id: "host".into(),
            tenant_id: "tenant".into(),
            status: status.into(),
            artifact: sqlx::types::Json(SignedStackPair {
                payload: serde_json::to_string(pair).unwrap(),
                signature: String::new(),
            }),
            expires_at: 90,
            roots_sha256,
            error: None,
        }
    }
}
