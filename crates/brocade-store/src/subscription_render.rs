//! Process-local, stale-while-refresh materialization for user-facing artifacts.
//!
//! The durable Serving checkpoint remains the authority. This cache only removes repeated
//! compilation and formatting from pulls: a newer checkpoint is built in the background while
//! the last complete generation remains readable. A cache miss after process start builds the
//! current generation once; subsequent publication, permission, certificate and rule changes
//! never make a pull wait for their replacement render.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
};

use brocade_core::{
    artifacts::subscription,
    format::{
        uri::{self, UriRenderOptions},
        yaml::{self, RulePreferences},
    },
    model::IpFamily,
    physical::user::{project_user, SubscriptionFilter, SubscriptionProtocol, UserPlan},
};
use sqlx::PgPool;
use tokio::sync::Mutex as AsyncMutex;

use crate::{DynamicSubscriptionFormat, PgStore, Result, StoreError};

const BACKGROUND_REFRESH_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FilterKey {
    family: u8,
    protocol: u8,
}

impl From<SubscriptionFilter> for FilterKey {
    fn from(filter: SubscriptionFilter) -> Self {
        Self {
            family: match filter.family {
                None => 0,
                Some(IpFamily::V4) => 4,
                Some(IpFamily::V6) => 6,
            },
            protocol: match filter.protocol {
                None => 0,
                Some(SubscriptionProtocol::Vless) => 1,
                Some(SubscriptionProtocol::AnyTls) => 2,
                Some(SubscriptionProtocol::Hysteria2) => 3,
                Some(SubscriptionProtocol::MtProto) => 4,
            },
        }
    }
}

#[derive(Debug)]
pub(crate) struct RenderedSubscriptionVariant {
    pub(crate) clash: String,
    pub(crate) clash_with_rules: String,
    pub(crate) haitun: String,
    pub(crate) native: String,
    pub(crate) native_format: DynamicSubscriptionFormat,
    pub(crate) uri: String,
    pub(crate) insecure_uri: String,
}

#[derive(Debug)]
pub(crate) struct RenderedSubscriptionUser {
    pub(crate) tenant_id: String,
    pub(crate) user_id: String,
    pub(crate) uuid: String,
    pub(crate) revision: u64,
    pub(crate) app_ids: Vec<String>,
    pub(crate) authorized: bool,
    plan: UserPlan,
    certificate_pins: BTreeMap<String, String>,
    rules: RulePreferences,
    variants: Mutex<BTreeMap<FilterKey, Arc<RenderedSubscriptionVariant>>>,
}

impl RenderedSubscriptionUser {
    fn new(
        plan: UserPlan,
        revision: u64,
        app_ids: Vec<String>,
        authorized: bool,
        certificate_pins: BTreeMap<String, String>,
        rules: RulePreferences,
    ) -> Self {
        let default = Arc::new(render_variant(
            plan.clone(),
            &certificate_pins,
            &rules,
            SubscriptionFilter::default(),
        ));
        let mut variants = BTreeMap::new();
        variants.insert(FilterKey::from(SubscriptionFilter::default()), default);
        Self {
            tenant_id: plan.tenant.clone(),
            user_id: plan.user.clone(),
            uuid: plan.uuid.clone(),
            revision,
            app_ids,
            authorized,
            plan,
            certificate_pins,
            rules,
            variants: Mutex::new(variants),
        }
    }

    pub(crate) fn has_entries(&self) -> bool {
        !self.plan.entries.is_empty()
    }

    /// At most fifteen family/protocol intersections exist. The first request for a non-default
    /// view formats it once; later pulls read the immutable string from this generation.
    pub(crate) fn variant(&self, filter: SubscriptionFilter) -> Arc<RenderedSubscriptionVariant> {
        let key = FilterKey::from(filter);
        if let Some(rendered) = self
            .variants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
            .cloned()
        {
            return rendered;
        }

        let rendered = Arc::new(render_variant(
            self.plan.clone(),
            &self.certificate_pins,
            &self.rules,
            filter,
        ));
        let mut variants = self
            .variants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(variants.entry(key).or_insert(rendered))
    }
}

fn render_variant(
    mut plan: UserPlan,
    certificate_pins: &BTreeMap<String, String>,
    rules: &RulePreferences,
    filter: SubscriptionFilter,
) -> RenderedSubscriptionVariant {
    plan.retain_filter(filter);
    let mut artifact = subscription::build(&plan);
    artifact.mark_self_signed(certificate_pins);
    let clash = yaml::clash_subscription(&artifact);
    let (native, native_format) = match uri::complete_subscription(&artifact) {
        Ok(content) => (content, DynamicSubscriptionFormat::Shadowrocket),
        Err(reason) => (
            clash.clone(),
            DynamicSubscriptionFormat::ClashCompatibility(reason),
        ),
    };
    RenderedSubscriptionVariant {
        clash: clash.clone(),
        clash_with_rules: yaml::clash_subscription_with_rules_for_plan(&artifact, &plan, rules),
        haitun: yaml::clash_haitun_subscription(&artifact),
        native,
        native_format,
        uri: uri::subscription(&artifact),
        insecure_uri: uri::subscription_with_options(
            &artifact,
            UriRenderOptions {
                allow_insecure: true,
            },
        ),
    }
}

#[derive(Debug)]
pub(crate) struct RenderedSubscriptionServing {
    generation: u64,
    runtime_epoch: u64,
    users_by_uuid: BTreeMap<String, Arc<RenderedSubscriptionUser>>,
    users_by_id: BTreeMap<(String, String), Arc<RenderedSubscriptionUser>>,
}

impl RenderedSubscriptionServing {
    pub(crate) fn user_by_uuid(&self, uuid: &str) -> Option<Arc<RenderedSubscriptionUser>> {
        self.users_by_uuid.get(uuid).cloned()
    }

    pub(crate) fn user(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Option<Arc<RenderedSubscriptionUser>> {
        self.users_by_id
            .get(&(tenant_id.to_owned(), user_id.to_owned()))
            .cloned()
    }
}

#[derive(Debug)]
pub(crate) struct SubscriptionRenderRuntime {
    ready: RwLock<Option<Arc<RenderedSubscriptionServing>>>,
    build_lock: AsyncMutex<()>,
    refreshing: AtomicBool,
    desired_epoch: AtomicU64,
}

impl Default for SubscriptionRenderRuntime {
    fn default() -> Self {
        Self {
            ready: RwLock::new(None),
            build_lock: AsyncMutex::new(()),
            refreshing: AtomicBool::new(false),
            desired_epoch: AtomicU64::new(1),
        }
    }
}

impl SubscriptionRenderRuntime {
    fn ready(&self) -> Option<Arc<RenderedSubscriptionServing>> {
        self.ready
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn install(&self, candidate: Arc<RenderedSubscriptionServing>) {
        let mut ready = self
            .ready
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let replace = ready.as_ref().is_none_or(|current| {
            (candidate.generation, candidate.runtime_epoch)
                >= (current.generation, current.runtime_epoch)
        });
        if replace {
            *ready = Some(candidate);
        }
    }

    pub(crate) async fn current(
        self: &Arc<Self>,
        pool: &PgPool,
    ) -> Result<Arc<RenderedSubscriptionServing>> {
        let desired_epoch = self.desired_epoch.load(Ordering::Acquire);
        let desired_generation = match crate::serving::subscription_generation(pool).await {
            Ok(generation) => generation,
            Err(error) => {
                if let Some(ready) = self.ready() {
                    return Ok(ready);
                }
                return Err(error);
            }
        };
        if let Some(ready) = self.ready() {
            if ready.generation == desired_generation && ready.runtime_epoch == desired_epoch {
                return Ok(ready);
            }
            self.schedule(pool.clone());
            return Ok(ready);
        }

        let _guard = self.build_lock.lock().await;
        if let Some(ready) = self.ready() {
            return Ok(ready);
        }
        let candidate = Arc::new(build_serving(pool, desired_epoch).await?);
        self.install(Arc::clone(&candidate));
        let latest_generation = crate::serving::subscription_generation(pool).await?;
        if candidate.generation != latest_generation
            || candidate.runtime_epoch != self.desired_epoch.load(Ordering::Acquire)
        {
            self.schedule(pool.clone());
        }
        Ok(candidate)
    }

    pub(crate) fn invalidate_and_schedule(self: &Arc<Self>, pool: PgPool) {
        self.desired_epoch.fetch_add(1, Ordering::AcqRel);
        self.schedule(pool);
    }

    pub(crate) fn schedule(self: &Arc<Self>, pool: PgPool) {
        if self.refreshing.swap(true, Ordering::AcqRel) {
            return;
        }
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            let refreshed = {
                let _guard = runtime.build_lock.lock().await;
                runtime.refresh_background(&pool).await
            };
            runtime.refreshing.store(false, Ordering::Release);
            let desired_epoch = runtime.desired_epoch.load(Ordering::Acquire);
            if refreshed
                && runtime
                    .ready()
                    .is_none_or(|ready| ready.runtime_epoch != desired_epoch)
            {
                runtime.schedule(pool);
            }
        });
    }

    async fn refresh_background(&self, pool: &PgPool) -> bool {
        for _ in 0..BACKGROUND_REFRESH_ATTEMPTS {
            let epoch = self.desired_epoch.load(Ordering::Acquire);
            let candidate = match build_serving(pool, epoch).await {
                Ok(candidate) => Arc::new(candidate),
                Err(error) => {
                    log::warn!("subscription artifact refresh failed: {error}");
                    return false;
                }
            };
            let generation = match crate::serving::subscription_generation(pool).await {
                Ok(generation) => generation,
                Err(error) => {
                    log::warn!("subscription artifact generation check failed: {error}");
                    return false;
                }
            };
            let current_epoch = self.desired_epoch.load(Ordering::Acquire);
            if candidate.generation == generation && candidate.runtime_epoch == current_epoch {
                self.install(candidate);
                return true;
            }
        }
        log::warn!("subscription artifact refresh stayed stale while Serving state changed");
        false
    }
}

async fn build_serving(pool: &PgPool, runtime_epoch: u64) -> Result<RenderedSubscriptionServing> {
    let serving = crate::serving::load_subscription_serving_projection(pool).await?;
    let generation = serving.generation();
    let certificate_pins = crate::cert::self_signed_certificate_pins(pool).await?;
    let rules = crate::clash_rules::load_all(pool).await?;
    tokio::task::spawn_blocking(move || {
        build_serving_sync(
            serving.snapshot,
            generation,
            runtime_epoch,
            certificate_pins,
            rules,
        )
    })
    .await
    .map_err(|error| {
        StoreError::InvalidData(format!(
            "subscription artifact renderer stopped unexpectedly: {error}"
        ))
    })?
}

fn build_serving_sync(
    snapshot: brocade_core::model::ModelSnapshot,
    generation: u64,
    runtime_epoch: u64,
    certificate_pins: BTreeMap<String, String>,
    mut rules: BTreeMap<(String, String), (i64, RulePreferences)>,
) -> Result<RenderedSubscriptionServing> {
    let revision = snapshot.revision;
    let output = crate::compile_cache::compile_incremental(&snapshot);
    output.ensure_publishable().map_err(|blocked| {
        StoreError::InvalidData(format!(
            "cannot render subscription Serving generation {generation}: {blocked:?}"
        ))
    })?;
    let mut users_by_uuid = BTreeMap::new();
    let mut users_by_id = BTreeMap::new();

    for user in &snapshot.users {
        let tenant_id = user.tenant.clone();
        let user_id = user.id.clone();
        let plan = crate::compile_cache::project_user(&output, &tenant_id, &user_id)
            .map(|plan| plan.as_ref().clone())
            .map_err(|blocked| {
                StoreError::InvalidData(format!(
                    "cannot render subscription user {tenant_id}/{user_id}: {blocked:?}"
                ))
            })?;
        let authorized = snapshot.apps.iter().any(|app| {
            app.grants
                .iter()
                .any(|grant| grant.tenant == tenant_id && grant.user == user_id)
        });
        let app_ids = output
            .unpublishable_view()
            .apps
            .iter()
            .filter_map(|app| {
                (!project_user(std::slice::from_ref(app), &tenant_id, &user_id)
                    .entries
                    .is_empty())
                .then(|| app.app_id.clone())
                .flatten()
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let preferences = rules
            .remove(&(tenant_id.clone(), user_id.clone()))
            .map(|(_, preferences)| preferences)
            .unwrap_or_default();
        let rendered = Arc::new(RenderedSubscriptionUser::new(
            plan,
            revision,
            app_ids,
            authorized,
            certificate_pins.clone(),
            preferences,
        ));
        users_by_uuid.insert(rendered.uuid.clone(), Arc::clone(&rendered));
        users_by_id.insert((tenant_id, user_id), rendered);
    }

    Ok(RenderedSubscriptionServing {
        generation,
        runtime_epoch,
        users_by_uuid,
        users_by_id,
    })
}

impl PgStore {
    pub(crate) async fn rendered_subscriptions(&self) -> Result<Arc<RenderedSubscriptionServing>> {
        self.subscription_render.current(&self.pool).await
    }

    pub(crate) fn refresh_rendered_subscriptions(&self) {
        self.subscription_render
            .invalidate_and_schedule(self.pool.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::FilterKey;
    use brocade_core::{
        model::IpFamily,
        physical::user::{SubscriptionFilter, SubscriptionProtocol},
    };

    #[test]
    fn filter_keys_keep_every_render_dimension_distinct() {
        let filters = [
            SubscriptionFilter::default(),
            SubscriptionFilter {
                family: Some(IpFamily::V4),
                protocol: None,
            },
            SubscriptionFilter {
                family: Some(IpFamily::V6),
                protocol: Some(SubscriptionProtocol::Vless),
            },
            SubscriptionFilter {
                family: None,
                protocol: Some(SubscriptionProtocol::AnyTls),
            },
            SubscriptionFilter {
                family: None,
                protocol: Some(SubscriptionProtocol::Hysteria2),
            },
            SubscriptionFilter {
                family: None,
                protocol: Some(SubscriptionProtocol::MtProto),
            },
        ];
        let keys = filters.into_iter().map(FilterKey::from).collect::<Vec<_>>();
        let unique = keys.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), keys.len());
    }
}
