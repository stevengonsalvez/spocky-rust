//! `ProviderSnapshotManager` from pinned Paseo
//! `agent/provider-snapshot-manager.ts`: per-target provider snapshots over
//! catalogues shared by key, with single-flight discovery, a refresh
//! deadline, and the create-config resolution `create.ts` uses.
//!
//! Provider definitions come from the caller in registry order (the
//! baseline builds them with `buildProviderRegistry`). The plugin-provider,
//! mutable-config, diagnostic, validation and shutdown members are not
//! ported here.
//!
//! The baseline's promises become tokio tasks: catalogue discovery runs
//! whether or not a caller awaits it, so the manager must be used inside a
//! tokio runtime. One provider's calls (`getCatalogCacheKey`, `isAvailable`,
//! `fetchCatalog`) keep the baseline's order and count; how two providers'
//! calls interleave follows task scheduling, as it follows provider I/O in
//! the baseline.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use sha2::{Digest, Sha256};
use spocky_contracts::js::{js_string, spread, truthy};
use spocky_store::js_value::{JsObject, JsValue, stringify};
use tokio::sync::{Semaphore, watch};

use crate::agent_sdk::{
    AbortController, AbortReason, AbortSignal, ActivityGuard, AgentClient,
    AgentCreateConfigUnattendedInput, AgentError, AgentProvider, AgentResult, BoxFuture,
    FetchCatalogOptions, ProviderRefreshContext, ResolveAgentCreateConfigInput,
    ResolveAgentCreateConfigResult,
};
use crate::clock::now_iso;
use crate::create_agent_mode::{
    is_default_agent_create_config_unattended, resolve_default_agent_create_config,
};
use crate::paths::{expand_tilde, resolve_from_cwd};
use crate::text::js_trim;

const DEFAULT_REFRESH_TIMEOUT_MS: u64 = 120_000;
const MAX_REFRESH_TIMEOUT_MS: f64 = 2_147_483_647.0;
const PROVIDER_REFRESH_DEADLINE_ENV: &str = "PASEO_PROVIDER_REFRESH_TIMEOUT_MS";
/// `GLOBAL_PROVIDER_SNAPSHOT_KEY`.
pub const GLOBAL_PROVIDER_SNAPSHOT_KEY: &str = "paseo:global";
/// p-limit concurrency of each provider's discovery.
const DISCOVERY_CONCURRENCY: usize = 4;

/// `validRefreshDeadline(value)`.
fn valid_refresh_deadline(value: f64) -> Option<u64> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (value.fract() == 0.0 && value > 0.0 && value <= MAX_REFRESH_TIMEOUT_MS).then_some(value as u64)
}

/// `Number(text)` for the environment value: trimmed decimal, `""` is 0.
fn js_number_of(text: &str) -> f64 {
    let trimmed = js_trim(text);
    if trimmed.is_empty() {
        return 0.0;
    }
    trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

/// `providerRefreshDeadline(configured)`.
fn provider_refresh_deadline(configured: Option<f64>) -> u64 {
    configured
        .and_then(valid_refresh_deadline)
        .or_else(|| {
            std::env::var(PROVIDER_REFRESH_DEADLINE_ENV)
                .ok()
                .and_then(|value| valid_refresh_deadline(js_number_of(&value)))
        })
        .unwrap_or(DEFAULT_REFRESH_TIMEOUT_MS)
}

/// `definition.fetchCatalog(options, client, context)`.
pub type FetchCatalogHook = Arc<
    dyn Fn(
            FetchCatalogOptions,
            Arc<dyn AgentClient>,
            Arc<dyn ProviderRefreshContext>,
        ) -> BoxFuture<'static, AgentResult<JsValue>>
        + Send
        + Sync,
>;

/// `definition.resolveCreateConfig(input)`; it may throw.
pub type ResolveCreateConfigHook = Arc<
    dyn Fn(&ResolveAgentCreateConfigInput) -> Result<ResolveAgentCreateConfigResult, AgentError>
        + Send
        + Sync,
>;

/// `definition.isCreateConfigUnattended(input)`.
pub type CreateConfigUnattendedHook =
    Arc<dyn Fn(&AgentCreateConfigUnattendedInput) -> bool + Send + Sync>;

/// The members of a `ProviderDefinition` the snapshot manager reads.
#[derive(Clone)]
pub struct SnapshotProviderDefinition {
    pub provider: AgentProvider,
    pub enabled: bool,
    /// `source: "custom"` (plugin, or a non-builtin override that extends
    /// another provider); `"builtin"` otherwise.
    pub custom: bool,
    pub label: String,
    pub description: Option<String>,
    pub icon_svg: Option<String>,
    pub default_mode_id: Option<String>,
    /// `definition.modes` (`AgentMode[]`), the parent fallback.
    pub modes: Option<JsValue>,
    /// The provider's client (`createClient` or an extra client).
    pub client: Arc<dyn AgentClient>,
    /// `None`: `client.fetchCatalog(options, context)`.
    pub fetch_catalog: Option<FetchCatalogHook>,
    /// `None`: the client's `resolveCreateConfig`, else
    /// `resolveDefaultAgentCreateConfig`.
    pub resolve_create_config: Option<ResolveCreateConfigHook>,
    /// `None`: the client's `isCreateConfigUnattended`, else
    /// `isDefaultAgentCreateConfigUnattended`.
    pub is_create_config_unattended: Option<CreateConfigUnattendedHook>,
}

impl SnapshotProviderDefinition {
    fn resolve_create_config(
        &self,
        input: &ResolveAgentCreateConfigInput,
    ) -> Result<ResolveAgentCreateConfigResult, AgentError> {
        if let Some(hook) = &self.resolve_create_config {
            return hook(input);
        }
        match self.client.resolve_create_config(input) {
            Some(result) => Ok(result),
            None => resolve_default_agent_create_config(input),
        }
    }

    fn is_create_config_unattended(&self, input: &AgentCreateConfigUnattendedInput) -> bool {
        if let Some(hook) = &self.is_create_config_unattended {
            return hook(input);
        }
        self.client
            .is_create_config_unattended(input)
            .unwrap_or_else(|| is_default_agent_create_config_unattended(input))
    }
}

/// `ProviderSnapshotManagerOptions` for the ported members.
#[derive(Clone, Default)]
pub struct ProviderSnapshotManagerOptions {
    /// Definitions in registry order.
    pub definitions: Vec<SnapshotProviderDefinition>,
    pub refresh_timeout_ms: Option<f64>,
    /// `homedir()`; `$HOME` when `None`.
    pub home: Option<String>,
}

/// `ProviderSnapshotRecord`: an entry and its content hash.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderSnapshotRecord {
    /// `ProviderSnapshotEntry`.
    pub entry: JsValue,
    pub content_hash: String,
}

/// `ProviderSnapshot`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderSnapshot {
    pub cwd: String,
    pub records: Vec<ProviderSnapshotRecord>,
}

/// `ProviderSnapshotTransition`.
#[derive(Debug, Clone)]
pub struct ProviderSnapshotTransition {
    pub previous: Arc<ProviderSnapshot>,
    pub current: Arc<ProviderSnapshot>,
}

/// A `change` listener.
pub type ProviderSnapshotListener = Arc<dyn Fn(&ProviderSnapshotTransition) + Send + Sync>;

/// An agent whose mode a child may inherit (`ManagedAgent` as
/// `resolveParent` reads it).
#[derive(Debug, Clone)]
pub struct CreateConfigParentAgent {
    pub provider: AgentProvider,
    pub current_mode_id: Option<String>,
    /// `AgentSessionConfig`.
    pub config: JsValue,
    /// `AgentFeature[]`.
    pub features: Option<JsValue>,
    /// `AgentMode[]`, `None` when unknown.
    pub available_modes: Option<JsValue>,
}

/// `ResolveProviderCreateConfigOptions`.
#[derive(Debug, Clone)]
pub struct ResolveProviderCreateConfigOptions {
    pub cwd: Option<String>,
    pub provider: AgentProvider,
    pub requested_mode: Option<String>,
    pub feature_values: Option<JsValue>,
    pub parent: Option<CreateConfigParentAgent>,
    pub unattended: bool,
}

/// `toErrorMessage(error)`.
fn to_error_message(error: &AgentError) -> String {
    if error.message.is_empty() {
        "Unknown error".to_owned()
    } else {
        error.message.clone()
    }
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |acc, (index, byte)| {
            acc | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..=chunk.len() {
            out.push(char::from(
                ALPHABET[(value >> (18 - 6 * index) & 63) as usize],
            ));
        }
    }
    out
}

/// `identifyEntry(entry)`: the hash leaves out `fetchedAt`.
fn identify_entry(entry: JsValue) -> ProviderSnapshotRecord {
    let mut content = JsObject::new();
    if let JsValue::Object(object) = &entry {
        for (key, value) in object.iter() {
            if key != "fetchedAt" {
                content.insert(key, value.clone());
            }
        }
    }
    let text = stringify(&JsValue::Array(vec![
        JsValue::String("paseo.provider-result/1".to_owned()),
        JsValue::Object(content),
    ]));
    ProviderSnapshotRecord {
        content_hash: base64url(&Sha256::digest(text.as_bytes())),
        entry,
    }
}

/// `sameSnapshotRecords(previous, current)`.
#[must_use]
pub fn same_snapshot_records(
    previous: &[ProviderSnapshotRecord],
    current: &[ProviderSnapshotRecord],
) -> bool {
    previous.len() == current.len()
        && previous.iter().zip(current).all(|(before, after)| {
            before.entry.get("provider") == after.entry.get("provider")
                && before.content_hash == after.content_hash
                && before.entry.get("fetchedAt") == after.entry.get("fetchedAt")
        })
}

/// Settles when a load or binding finishes.
type Done = watch::Receiver<bool>;

fn settled() -> Done {
    watch::channel(true).1
}

async fn wait_done(mut done: Done) {
    let _ = done.wait_for(|finished| *finished).await;
}

#[derive(Clone)]
enum CatalogScope {
    Global,
    Workspace(String),
}

#[derive(Clone)]
struct SnapshotTarget {
    snapshot_cwd: String,
    catalog_scope: CatalogScope,
}

fn fetch_catalog_options(scope: &CatalogScope, force: bool) -> FetchCatalogOptions {
    match scope {
        CatalogScope::Global => FetchCatalogOptions::Global { force },
        CatalogScope::Workspace(cwd) => FetchCatalogOptions::Workspace {
            cwd: cwd.clone(),
            force,
        },
    }
}

struct Catalog {
    id: u64,
    result: Option<ProviderSnapshotRecord>,
    stale: bool,
    load: Option<(u64, Done)>,
}

struct Binding {
    id: u64,
    key: Option<String>,
    failure: Option<ProviderSnapshotRecord>,
    force: bool,
    done: Done,
}

struct Target {
    bindings: HashMap<AgentProvider, Binding>,
    snapshot: Arc<ProviderSnapshot>,
}

struct State {
    catalogs: HashMap<String, HashMap<AgentProvider, Catalog>>,
    /// `targets`, in `Map` insertion order.
    targets: Vec<(String, Target)>,
    destroyed: bool,
    refresh_timeout_ms: u64,
    listeners: Vec<(u64, ProviderSnapshotListener)>,
    next_id: u64,
}

impl State {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn target(&self, cwd: &str) -> Option<&Target> {
        self.targets
            .iter()
            .find(|(key, _)| key == cwd)
            .map(|(_, target)| target)
    }

    fn target_mut(&mut self, cwd: &str) -> Option<&mut Target> {
        self.targets
            .iter_mut()
            .find(|(key, _)| key == cwd)
            .map(|(_, target)| target)
    }
}

struct Inner {
    definitions: Vec<SnapshotProviderDefinition>,
    initial: Vec<ProviderSnapshotRecord>,
    limits: Vec<Arc<Semaphore>>,
    home: String,
    state: Mutex<State>,
}

/// `ProviderSnapshotManager`.
#[derive(Clone)]
pub struct ProviderSnapshotManager {
    inner: Arc<Inner>,
}

impl ProviderSnapshotManager {
    #[must_use]
    pub fn new(options: ProviderSnapshotManagerOptions) -> Self {
        let initial = options
            .definitions
            .iter()
            .map(|definition| {
                let mut entry = JsObject::new();
                entry.insert("provider", JsValue::String(definition.provider.clone()));
                entry.insert(
                    "status",
                    JsValue::String(
                        if definition.enabled {
                            "loading"
                        } else {
                            "unavailable"
                        }
                        .to_owned(),
                    ),
                );
                entry.insert("enabled", JsValue::Bool(definition.enabled));
                entry.insert(
                    "source",
                    JsValue::String(
                        if definition.custom {
                            "custom"
                        } else {
                            "builtin"
                        }
                        .to_owned(),
                    ),
                );
                entry.insert("label", JsValue::String(definition.label.clone()));
                entry.insert(
                    "description",
                    optional_string(definition.description.as_ref()),
                );
                entry.insert("iconSvg", optional_string(definition.icon_svg.as_ref()));
                entry.insert(
                    "defaultModeId",
                    definition
                        .default_mode_id
                        .clone()
                        .map_or(JsValue::Null, JsValue::String),
                );
                identify_entry(JsValue::Object(entry))
            })
            .collect();
        let limits = options
            .definitions
            .iter()
            .map(|_| Arc::new(Semaphore::new(DISCOVERY_CONCURRENCY)))
            .collect();
        Self {
            inner: Arc::new(Inner {
                definitions: options.definitions,
                initial,
                limits,
                home: options
                    .home
                    .unwrap_or_else(|| std::env::var("HOME").unwrap_or_default()),
                state: Mutex::new(State {
                    catalogs: HashMap::new(),
                    targets: Vec::new(),
                    destroyed: false,
                    refresh_timeout_ms: provider_refresh_deadline(options.refresh_timeout_ms),
                    listeners: Vec::new(),
                    next_id: 0,
                }),
            }),
        }
    }

    /// `resolveSnapshotCwd(cwd)`.
    #[must_use]
    pub fn resolve_snapshot_cwd(&self, cwd: Option<&str>) -> String {
        self.inner.resolve_snapshot_cwd(cwd)
    }

    /// `getSnapshot(cwd)`.
    #[must_use]
    pub fn get_snapshot(&self, cwd: Option<&str>) -> Arc<ProviderSnapshot> {
        let target = self.inner.resolve_target(cwd);
        self.inner.snapshot_for_target(&target, None)
    }

    /// `refreshSnapshotForCwd({ cwd, providers })`.
    pub async fn refresh_snapshot_for_cwd(&self, cwd: &str, providers: Option<&[AgentProvider]>) {
        let target = self.inner.workspace_target(cwd);
        let providers = self
            .inner
            .resolve_refresh_providers(providers)
            .unwrap_or_else(|| self.inner.provider_ids());
        self.inner.load_providers(&target, &providers, true).await;
    }

    /// `refresh(options)`.
    pub async fn refresh(&self, cwd: &str, providers: Option<&[AgentProvider]>) {
        self.refresh_snapshot_for_cwd(cwd, providers).await;
    }

    /// `refreshSettingsSnapshot({ providers })`.
    pub async fn refresh_settings_snapshot(&self, providers: Option<&[AgentProvider]>) {
        let inner = &self.inner;
        let target = SnapshotTarget {
            snapshot_cwd: GLOBAL_PROVIDER_SNAPSHOT_KEY.to_owned(),
            catalog_scope: CatalogScope::Global,
        };
        let providers = inner
            .resolve_refresh_providers(providers)
            .unwrap_or_else(|| inner.provider_ids());
        {
            let mut state = inner.lock();
            inner.get_or_create_target(&mut state, GLOBAL_PROVIDER_SNAPSHOT_KEY);
            let keys: Vec<String> = state.catalogs.keys().cloned().collect();
            for key in keys {
                for provider in &providers {
                    let id = state.next_id();
                    if let Some(catalogs) = state.catalogs.get_mut(&key)
                        && let Some(catalog) = catalogs.get(provider)
                    {
                        let result = catalog.result.clone();
                        catalogs.insert(
                            provider.clone(),
                            Catalog {
                                id,
                                result,
                                stale: true,
                                load: None,
                            },
                        );
                    }
                }
            }
        }
        // Refresh each known target: provider keys coalesce reads, while
        // target-scoped providers must discover again in their own context.
        inner.load_providers(&target, &providers, true).await;
        let others: Vec<String> = inner
            .lock()
            .targets
            .iter()
            .map(|(cwd, _)| cwd.clone())
            .filter(|cwd| cwd != GLOBAL_PROVIDER_SNAPSHOT_KEY)
            .collect();
        let warmups: Vec<Done> = others
            .iter()
            .map(|cwd| {
                let target = inner.workspace_target(cwd);
                inner.spawn_load(&target, &providers, false)
            })
            .collect();
        for warmup in warmups {
            wait_done(warmup).await;
        }
    }

    /// `warmUpSnapshotForCwd({ cwd, providers })`.
    pub async fn warm_up_snapshot_for_cwd(
        &self,
        cwd: Option<&str>,
        providers: Option<&[AgentProvider]>,
    ) {
        let inner = &self.inner;
        let target = inner.resolve_target(cwd);
        let resolved = inner.resolve_refresh_providers(providers);
        if providers.is_some() && resolved.as_ref().is_some_and(Vec::is_empty) {
            return;
        }
        let warm = inner.resolve_providers_to_warm(&target.snapshot_cwd, resolved);
        if warm.is_empty() {
            return;
        }
        inner.load_providers(&target, &warm, false).await;
    }

    /// `listRegisteredProviderIds()`.
    #[must_use]
    pub fn list_registered_provider_ids(&self) -> Vec<AgentProvider> {
        self.inner.provider_ids()
    }

    /// `hasProvider(provider)`.
    #[must_use]
    pub fn has_provider(&self, provider: &str) -> bool {
        self.inner.definition(provider).is_some()
    }

    /// `getProviderLabel(provider)`.
    #[must_use]
    pub fn get_provider_label(&self, provider: &str) -> String {
        self.inner.definition(provider).map_or_else(
            || provider.to_owned(),
            |definition| definition.label.clone(),
        )
    }

    /// `listProviders({ cwd, providers, wait })`.
    pub async fn list_providers(
        &self,
        cwd: Option<&str>,
        providers: Option<&[AgentProvider]>,
        wait: bool,
    ) -> Vec<JsValue> {
        let inner = &self.inner;
        let target = inner.resolve_target(cwd);
        if wait {
            self.warm_up_snapshot_for_cwd(cwd, providers).await;
        }
        let snapshot = if wait {
            let mut state = inner.lock();
            Arc::clone(
                &inner
                    .get_or_create_target(&mut state, &target.snapshot_cwd)
                    .snapshot,
            )
        } else {
            inner.snapshot_for_target(&target, providers)
        };
        snapshot
            .records
            .iter()
            .map(|record| record.entry.clone())
            .filter(|entry| {
                providers.is_none_or(|providers| {
                    entry
                        .get("provider")
                        .and_then(JsValue::as_str)
                        .is_some_and(|provider| providers.iter().any(|p| p == provider))
                })
            })
            .collect()
    }

    /// `getProvider({ cwd, provider, wait })`.
    ///
    /// # Errors
    ///
    /// `Provider <provider> is not configured`.
    pub async fn get_provider(
        &self,
        cwd: Option<&str>,
        provider: &str,
        wait: bool,
    ) -> Result<JsValue, AgentError> {
        self.list_providers(cwd, Some(&[provider.to_owned()]), wait)
            .await
            .into_iter()
            .find(|entry| entry.get("provider").and_then(JsValue::as_str) == Some(provider))
            .ok_or_else(|| AgentError::new(format!("Provider {provider} is not configured")))
    }

    /// `getReadyProvider({ cwd, provider, wait })`.
    ///
    /// # Errors
    ///
    /// The baseline's errors for an unconfigured, disabled, failed or
    /// unavailable provider.
    pub async fn get_ready_provider(
        &self,
        cwd: Option<&str>,
        provider: &str,
        wait: bool,
    ) -> Result<JsValue, AgentError> {
        let entry = self.get_provider(cwd, provider, wait).await?;
        let name = js_string(entry.get("provider"));
        if !matches!(entry.get("enabled"), Some(JsValue::Bool(true))) {
            return Err(AgentError::new(format!("Provider '{name}' is disabled")));
        }
        match entry.get("status").and_then(JsValue::as_str) {
            Some("ready") => Ok(entry),
            Some("error") => Err(AgentError::new(
                entry.get("error").and_then(JsValue::as_str).map_or_else(
                    || format!("Failed to load provider '{name}'"),
                    str::to_owned,
                ),
            )),
            _ => Err(AgentError::new(format!(
                "Provider '{name}' is not available"
            ))),
        }
    }

    /// `listModels({ cwd, provider, wait })`: the selectable models.
    ///
    /// # Errors
    ///
    /// As [`Self::get_ready_provider`].
    pub async fn list_models(
        &self,
        cwd: Option<&str>,
        provider: &str,
        wait: bool,
    ) -> Result<Vec<JsValue>, AgentError> {
        let entry = self.get_ready_provider(cwd, provider, wait).await?;
        Ok(entry
            .get("models")
            .and_then(JsValue::as_array)
            .map(|models| {
                models
                    .iter()
                    .filter(|model| {
                        !matches!(model.get("isSelectable"), Some(JsValue::Bool(false)))
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    /// `listModes({ cwd, provider, wait })`.
    ///
    /// # Errors
    ///
    /// As [`Self::get_ready_provider`].
    pub async fn list_modes(
        &self,
        cwd: Option<&str>,
        provider: &str,
        wait: bool,
    ) -> Result<JsValue, AgentError> {
        let entry = self.get_ready_provider(cwd, provider, wait).await?;
        Ok(entry
            .get("modes")
            .filter(|modes| !matches!(modes, JsValue::Undefined | JsValue::Null))
            .cloned()
            .unwrap_or(JsValue::Array(Vec::new())))
    }

    /// `resolveDefaultModel({ provider, requestedModel, cwd })`.
    pub async fn resolve_default_model(
        &self,
        provider: &str,
        requested_model: Option<&str>,
        cwd: Option<&str>,
    ) -> Option<String> {
        let trimmed = requested_model.map(js_trim).unwrap_or_default();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
        let cwd = cwd
            .filter(|cwd| !cwd.is_empty())
            .map(|cwd| expand_tilde(cwd, &self.inner.home));
        let models = self
            .list_models(cwd.as_deref(), provider, true)
            .await
            .ok()?;
        models
            .iter()
            .find(|model| truthy(model.get("isDefault")))
            .or_else(|| models.first())
            .and_then(|model| model.get("id"))
            .filter(|id| !matches!(id, JsValue::Undefined))
            .map(|id| js_string(Some(id)))
    }

    /// `resolveCreateConfig(input)`.
    ///
    /// # Errors
    ///
    /// As [`Self::get_ready_provider`], or the provider's create-config
    /// error.
    pub async fn resolve_create_config(
        &self,
        input: ResolveProviderCreateConfigOptions,
    ) -> Result<ResolveAgentCreateConfigResult, AgentError> {
        let entry = self
            .get_ready_provider(input.cwd.as_deref(), &input.provider, true)
            .await?;
        let definition = self.inner.require_provider(&input.provider)?;
        let parent = match &input.parent {
            Some(parent) => Some(self.inner.resolve_parent(parent)?),
            None => None,
        };
        let parent_unattended = parent
            .as_ref()
            .is_some_and(|parent| matches!(parent.get("isUnattended"), Some(JsValue::Bool(true))));
        definition.resolve_create_config(&ResolveAgentCreateConfigInput {
            provider: input.provider.clone(),
            requested_mode: input.requested_mode,
            feature_values: input.feature_values,
            parent,
            unattended: input.unattended || parent_unattended,
            available_modes: Some(
                entry
                    .get("modes")
                    .filter(|modes| !matches!(modes, JsValue::Undefined | JsValue::Null))
                    .cloned()
                    .unwrap_or(JsValue::Array(Vec::new())),
            ),
        })
    }

    /// `setRefreshTimeoutMs(refreshTimeoutMs)`.
    pub fn set_refresh_timeout_ms(&self, refresh_timeout_ms: Option<f64>) {
        self.inner.lock().refresh_timeout_ms = provider_refresh_deadline(refresh_timeout_ms);
    }

    /// `on("change", listener)`: the id [`Self::off_change`] takes.
    pub fn on_change(&self, listener: ProviderSnapshotListener) -> u64 {
        let mut state = self.inner.lock();
        let id = state.next_id();
        state.listeners.push((id, listener));
        id
    }

    /// `off("change", listener)`.
    pub fn off_change(&self, id: u64) {
        self.inner
            .lock()
            .listeners
            .retain(|(existing, _)| *existing != id);
    }

    /// `destroy()`.
    pub fn destroy(&self) {
        let mut state = self.inner.lock();
        state.destroyed = true;
        for limit in &self.inner.limits {
            limit.close();
        }
        state.catalogs.clear();
        state.targets.clear();
        state.listeners.clear();
    }
}

fn optional_string(value: Option<&String>) -> JsValue {
    value.map_or(JsValue::Undefined, |value| JsValue::String(value.clone()))
}

/// `ProviderRefreshContext` of `runProviderRefreshWithDeadline`.
struct DeadlineContext {
    signal: AbortSignal,
    /// `activityCounts`, in `Map` insertion order.
    activities: Arc<Mutex<Vec<(String, usize)>>>,
}

impl ProviderRefreshContext for DeadlineContext {
    fn signal(&self) -> &AbortSignal {
        &self.signal
    }

    fn begin_activity(&self, name: &str) -> ActivityGuard {
        {
            let mut activities = lock(&self.activities);
            match activities.iter_mut().find(|(existing, _)| existing == name) {
                Some(slot) => slot.1 += 1,
                None => activities.push((name.to_owned(), 1)),
            }
        }
        let activities = Arc::clone(&self.activities);
        let name = name.to_owned();
        ActivityGuard::new(move || {
            let mut activities = lock(&activities);
            if let Some(index) = activities
                .iter()
                .position(|(existing, _)| *existing == name)
            {
                if activities[index].1 <= 1 {
                    activities.remove(index);
                } else {
                    activities[index].1 -= 1;
                }
            }
        })
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn abort_error(signal: &AbortSignal) -> AgentError {
    match signal.reason() {
        Some(AbortReason::Error(error)) => error.clone(),
        Some(AbortReason::Value(value)) => AgentError::new(js_string(Some(value))),
        None => AgentError::new("This operation was aborted"),
    }
}

/// `raceProviderRefreshAbort(signal, operation)`.
async fn race_provider_refresh_abort<T>(
    signal: &AbortSignal,
    operation: impl Future<Output = AgentResult<T>>,
) -> AgentResult<T> {
    if signal.aborted() {
        return Err(abort_error(signal));
    }
    tokio::select! {
        biased;
        result = operation => result,
        () = signal.wait() => Err(abort_error(signal)),
    }
}

/// `context.runActivity(name, operation)` with its abort checks.
async fn run_checked_activity<T>(
    context: &DeadlineContext,
    name: &str,
    operation: impl Future<Output = AgentResult<T>>,
) -> AgentResult<T> {
    if context.signal.aborted() {
        return Err(abort_error(&context.signal));
    }
    let _activity = context.begin_activity(name);
    let result = operation.await?;
    if context.signal.aborted() {
        return Err(abort_error(&context.signal));
    }
    Ok(result)
}

/// `normalizeAgentModelCatalog(models)`: the first model of each id.
fn normalize_agent_model_catalog(models: &[JsValue]) -> Vec<JsValue> {
    let mut ids: Vec<Option<&JsValue>> = Vec::new();
    let mut unique = Vec::with_capacity(models.len());
    for model in models {
        let id = model
            .get("id")
            .filter(|id| !matches!(id, JsValue::Undefined));
        if ids.contains(&id) {
            continue;
        }
        ids.push(id);
        unique.push(model.clone());
    }
    unique
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn index(&self, provider: &str) -> Option<usize> {
        self.definitions
            .iter()
            .position(|definition| definition.provider == provider)
    }

    fn definition(&self, provider: &str) -> Option<&SnapshotProviderDefinition> {
        self.index(provider).map(|index| &self.definitions[index])
    }

    fn require_provider(&self, provider: &str) -> Result<&SnapshotProviderDefinition, AgentError> {
        self.definition(provider)
            .ok_or_else(|| AgentError::new(format!("Provider {provider} is not configured")))
    }

    fn provider_ids(&self) -> Vec<AgentProvider> {
        self.definitions
            .iter()
            .map(|definition| definition.provider.clone())
            .collect()
    }

    /// `resolveRefreshProviders(providers)`: known ids, first occurrence.
    fn resolve_refresh_providers(
        &self,
        providers: Option<&[AgentProvider]>,
    ) -> Option<Vec<AgentProvider>> {
        let providers = providers.filter(|providers| !providers.is_empty())?;
        let mut out: Vec<AgentProvider> = Vec::new();
        for provider in providers {
            if !out.contains(provider) && self.definition(provider).is_some() {
                out.push(provider.clone());
            }
        }
        Some(out)
    }

    /// `resolveSnapshotCwd(cwd)`.
    fn resolve_snapshot_cwd(&self, cwd: Option<&str>) -> String {
        let trimmed = cwd.map(js_trim).unwrap_or_default();
        if trimmed.is_empty() {
            return self.home.clone();
        }
        let expanded = if trimmed == "~" || trimmed.starts_with("~/") {
            format!("{}{}", self.home, &trimmed[1..])
        } else {
            trimmed.to_owned()
        };
        resolve_from_cwd(&expanded)
    }

    fn workspace_target(&self, cwd: &str) -> SnapshotTarget {
        let snapshot_cwd = self.resolve_snapshot_cwd(Some(cwd));
        SnapshotTarget {
            catalog_scope: CatalogScope::Workspace(snapshot_cwd.clone()),
            snapshot_cwd,
        }
    }

    /// `resolveProviderSnapshotTarget(cwd)`.
    fn resolve_target(&self, cwd: Option<&str>) -> SnapshotTarget {
        let trimmed = cwd.map(js_trim).unwrap_or_default();
        if trimmed.is_empty() {
            return SnapshotTarget {
                snapshot_cwd: GLOBAL_PROVIDER_SNAPSHOT_KEY.to_owned(),
                catalog_scope: CatalogScope::Global,
            };
        }
        let resolved = self.resolve_snapshot_cwd(Some(trimmed));
        self.workspace_target(&resolved)
    }

    fn get_or_create_target<'a>(&self, state: &'a mut State, cwd: &str) -> &'a mut Target {
        if state.target(cwd).is_none() {
            state.targets.push((
                cwd.to_owned(),
                Target {
                    bindings: HashMap::new(),
                    snapshot: Arc::new(ProviderSnapshot {
                        cwd: cwd.to_owned(),
                        records: self.initial.clone(),
                    }),
                },
            ));
        }
        state
            .target_mut(cwd)
            .unwrap_or_else(|| unreachable!("target created above"))
    }

    fn resolve_providers_to_warm(
        &self,
        cwd: &str,
        providers: Option<Vec<AgentProvider>>,
    ) -> Vec<AgentProvider> {
        self.get_or_create_target(&mut self.lock(), cwd);
        // Identity is provider-owned and may change without a config reload.
        providers.unwrap_or_else(|| self.provider_ids())
    }

    /// `getSnapshotForTarget(target, providers)`.
    fn snapshot_for_target(
        self: &Arc<Self>,
        target: &SnapshotTarget,
        providers: Option<&[AgentProvider]>,
    ) -> Arc<ProviderSnapshot> {
        let warm =
            self.resolve_providers_to_warm(&target.snapshot_cwd, providers.map(<[_]>::to_vec));
        if !warm.is_empty() {
            drop(self.spawn_load(target, &warm, false));
        }
        let mut state = self.lock();
        Arc::clone(
            &self
                .get_or_create_target(&mut state, &target.snapshot_cwd)
                .snapshot,
        )
    }

    /// `loadProviders(options)`, awaited (`Promise.allSettled`).
    async fn load_providers(
        self: &Arc<Self>,
        target: &SnapshotTarget,
        providers: &[AgentProvider],
        force: bool,
    ) {
        wait_done(self.spawn_load(target, providers, force)).await;
    }

    /// Starts `loadProviders`; the result settles when every provider has.
    fn spawn_load(
        self: &Arc<Self>,
        target: &SnapshotTarget,
        providers: &[AgentProvider],
        force: bool,
    ) -> Done {
        let loads: Vec<Done> = providers
            .iter()
            .map(|provider| self.load_provider(target, provider, force))
            .collect();
        let (finished, done) = watch::channel(false);
        tokio::spawn(async move {
            for load in loads {
                wait_done(load).await;
            }
            let _ = finished.send(true);
        });
        done
    }

    /// `loadProvider(options)`.
    fn load_provider(
        self: &Arc<Self>,
        target: &SnapshotTarget,
        provider: &str,
        force: bool,
    ) -> Done {
        if self.definition(provider).is_none() {
            return settled();
        }
        let (finished, done) = watch::channel(false);
        let (binding_id, force) = {
            let mut state = self.lock();
            if state.destroyed {
                return settled();
            }
            let id = state.next_id();
            let target_state = self.get_or_create_target(&mut state, &target.snapshot_cwd);
            let previous = target_state.bindings.get(provider);
            let binding = Binding {
                id,
                key: previous.and_then(|binding| binding.key.clone()),
                failure: previous.and_then(|binding| binding.failure.clone()),
                force: force || previous.is_some_and(|binding| binding.force),
                done: done.clone(),
            };
            let force = binding.force;
            target_state.bindings.insert(provider.to_owned(), binding);
            (id, force)
        };
        // `resolveCatalog` runs synchronously up to its first await: an
        // enabled provider's `getCatalogCacheKey` is called now.
        let definition = self.definition(provider);
        let lookup = definition
            .filter(|definition| definition.enabled)
            .and_then(|definition| {
                definition
                    .client
                    .get_catalog_cache_key(&fetch_catalog_options(&target.catalog_scope, force))
            });
        let inner = Arc::clone(self);
        let target = target.clone();
        let provider = provider.to_owned();
        tokio::spawn(async move {
            inner
                .resolve_catalog(&target, &provider, force, binding_id, lookup)
                .await;
            let _ = finished.send(true);
        });
        done
    }

    fn current_binding(&self, cwd: &str, provider: &str) -> Option<(u64, Done)> {
        self.lock()
            .target(cwd)
            .and_then(|target| target.bindings.get(provider))
            .map(|binding| (binding.id, binding.done.clone()))
    }

    /// `resolveCatalog(options, binding)`.
    #[allow(clippy::too_many_lines)]
    async fn resolve_catalog(
        self: &Arc<Self>,
        target: &SnapshotTarget,
        provider: &str,
        force: bool,
        binding_id: u64,
        lookup: Option<BoxFuture<'static, AgentResult<Option<String>>>>,
    ) {
        let Some(index) = self.index(provider) else {
            return;
        };
        let definition = self.definitions[index].clone();
        let cwd = &target.snapshot_cwd;
        let timeout_ms = {
            let mut state = self.lock();
            self.get_or_create_target(&mut state, cwd);
            state.refresh_timeout_ms
        };
        if !definition.enabled {
            return;
        }
        let options = fetch_catalog_options(&target.catalog_scope, force);
        let shared_key = if let Some(lookup) = lookup {
            match tokio::time::timeout(Duration::from_millis(timeout_ms), lookup).await {
                Ok(Ok(key)) => Ok(key),
                Ok(Err(error)) => Err(to_error_message(&error)),
                Err(_) => Err(format!("Timed out resolving {provider} catalogue key")),
            }
        } else {
            tokio::task::yield_now().await;
            Ok(None)
        };
        let key = match shared_key {
            Ok(shared) => stringify(&JsValue::Array(match shared {
                None => vec![
                    JsValue::String("target".to_owned()),
                    JsValue::String(cwd.clone()),
                ],
                Some(key) => vec![JsValue::String("provider".to_owned()), JsValue::String(key)],
            })),
            Err(message) => {
                let bound = {
                    let mut state = self.lock();
                    match state
                        .target_mut(cwd)
                        .and_then(|target| target.bindings.get_mut(provider))
                        .filter(|binding| binding.id == binding_id)
                    {
                        Some(binding) => {
                            let mut entry = spread(Some(&self.initial[index].entry));
                            entry.insert("status", JsValue::String("error".to_owned()));
                            entry.insert("error", JsValue::String(message));
                            binding.force = false;
                            binding.key = None;
                            binding.failure = Some(identify_entry(JsValue::Object(entry)));
                            true
                        }
                        None => false,
                    }
                };
                if !bound {
                    return self.await_current_binding(cwd, provider).await;
                }
                self.publish_targets(std::slice::from_ref(cwd));
                return;
            }
        };
        // Only the latest key resolution for this target may bind it.
        let bound = 'bound: {
            let mut state = self.lock();
            let current = state
                .target(cwd)
                .and_then(|target| target.bindings.get(provider))
                .is_some_and(|binding| binding.id == binding_id);
            if !current {
                break 'bound None;
            }
            let fresh_id = state.next_id();
            if let Some(binding) = state
                .target_mut(cwd)
                .and_then(|target| target.bindings.get_mut(provider))
            {
                binding.force = false;
                binding.key = Some(key.clone());
                binding.failure = None;
            }
            let catalog = state
                .catalogs
                .entry(key.clone())
                .or_default()
                .entry(provider.to_owned())
                .or_insert(Catalog {
                    id: fresh_id,
                    result: None,
                    stale: false,
                    load: None,
                });
            let skip =
                !force && (catalog.load.is_some() || (catalog.result.is_some() && !catalog.stale));
            let wait_for = if skip {
                Some(catalog.load.as_ref().map(|(_, done)| done.clone()))
            } else {
                catalog.stale = false;
                None
            };
            Some((catalog.id, wait_for))
        };
        let Some((catalog_id, wait_for)) = bound else {
            return self.await_current_binding(cwd, provider).await;
        };
        self.publish_targets(std::slice::from_ref(cwd));
        if let Some(wait_for) = wait_for {
            if let Some(load) = wait_for {
                wait_done(load).await;
            }
            return;
        }
        let (finished, load) = watch::channel(false);
        let load_id = {
            let mut state = self.lock();
            let load_id = state.next_id();
            match state
                .catalogs
                .get_mut(&key)
                .and_then(|catalogs| catalogs.get_mut(provider))
                .filter(|catalog| catalog.id == catalog_id)
            {
                Some(catalog) => catalog.load = Some((load_id, load.clone())),
                None => return,
            }
            load_id
        };
        let inner = Arc::clone(self);
        let provider_name = provider.to_owned();
        let limit = Arc::clone(&self.limits[index]);
        tokio::spawn(async move {
            if let Ok(_permit) = limit.acquire_owned().await
                && inner.is_current(&key, &provider_name, catalog_id, load_id)
            {
                inner
                    .refresh_provider(index, &definition, options, &key, catalog_id, load_id)
                    .await;
            }
            {
                let mut state = inner.lock();
                if let Some(catalog) = state
                    .catalogs
                    .get_mut(&key)
                    .and_then(|catalogs| catalogs.get_mut(&provider_name))
                    .filter(|catalog| catalog.id == catalog_id)
                    && catalog.load.as_ref().is_some_and(|(id, _)| *id == load_id)
                {
                    catalog.load = None;
                }
            }
            let _ = finished.send(true);
        });
        wait_done(load).await;
    }

    /// `return currentBinding()?.promise`.
    async fn await_current_binding(&self, cwd: &str, provider: &str) {
        if let Some((_, done)) = self.current_binding(cwd, provider) {
            wait_done(done).await;
        }
    }

    fn is_current(&self, key: &str, provider: &str, catalog_id: u64, load_id: u64) -> bool {
        let state = self.lock();
        !state.destroyed
            && state
                .catalogs
                .get(key)
                .and_then(|catalogs| catalogs.get(provider))
                .is_some_and(|catalog| {
                    catalog.id == catalog_id
                        && catalog.load.as_ref().is_some_and(|(id, _)| *id == load_id)
                })
    }

    /// `refreshProvider(options)`.
    async fn refresh_provider(
        self: &Arc<Self>,
        index: usize,
        definition: &SnapshotProviderDefinition,
        options: FetchCatalogOptions,
        key: &str,
        catalog_id: u64,
        load_id: u64,
    ) {
        let base = &self.initial[index].entry;
        let timeout_ms = self.lock().refresh_timeout_ms;
        let entry = match self
            .run_with_deadline(definition, options, timeout_ms)
            .await
        {
            Ok(None) => {
                let mut entry = spread(Some(base));
                entry.insert("status", JsValue::String("unavailable".to_owned()));
                entry.insert("enabled", JsValue::Bool(true));
                entry
            }
            Ok(Some(catalog)) => match ready_entry(base, &catalog) {
                Ok(entry) => entry,
                Err(message) => error_entry(base, message),
            },
            Err(error) => error_entry(base, to_error_message(&error)),
        };
        let published = {
            let mut state = self.lock();
            let current = !state.destroyed
                && state
                    .catalogs
                    .get(key)
                    .and_then(|catalogs| catalogs.get(&definition.provider))
                    .is_some_and(|catalog| {
                        catalog.id == catalog_id
                            && catalog.load.as_ref().is_some_and(|(id, _)| *id == load_id)
                    });
            if current {
                if let Some(catalog) = state
                    .catalogs
                    .get_mut(key)
                    .and_then(|catalogs| catalogs.get_mut(&definition.provider))
                {
                    catalog.result = Some(identify_entry(JsValue::Object(entry)));
                }
                Some(
                    state
                        .targets
                        .iter()
                        .filter(|(_, target)| {
                            target
                                .bindings
                                .get(&definition.provider)
                                .and_then(|binding| binding.key.as_deref())
                                == Some(key)
                        })
                        .map(|(cwd, _)| cwd.clone())
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            }
        };
        if let Some(bound) = published {
            self.publish_targets(&bound);
        }
    }

    /// `runProviderRefreshWithDeadline` around availability and catalogue
    /// discovery: the catalogue, or `None` when unavailable.
    async fn run_with_deadline(
        &self,
        definition: &SnapshotProviderDefinition,
        options: FetchCatalogOptions,
        timeout_ms: u64,
    ) -> AgentResult<Option<JsValue>> {
        let controller = AbortController::default();
        let activities = Arc::new(Mutex::new(Vec::new()));
        let context = Arc::new(DeadlineContext {
            signal: controller.signal(),
            activities: Arc::clone(&activities),
        });
        let client = Arc::clone(&definition.client);
        let operation = {
            let context = Arc::clone(&context);
            let fetch = definition.fetch_catalog.clone();
            async move {
                let available = run_checked_activity(
                    &context,
                    "availability",
                    race_provider_refresh_abort(
                        &context.signal,
                        client.is_available(Some(context.signal.clone()), Some(options.clone())),
                    ),
                )
                .await?;
                if !available {
                    return Ok(None);
                }
                let context: Arc<dyn ProviderRefreshContext> = context;
                let catalog = match fetch {
                    Some(fetch) => fetch(options, Arc::clone(&client), context).await?,
                    None => client.fetch_catalog(options, Some(context)).await?,
                };
                Ok(Some(catalog))
            }
        };
        tokio::pin!(operation);
        let sleep = tokio::time::sleep(Duration::from_millis(timeout_ms));
        tokio::pin!(sleep);
        let mut timeout_error = None;
        let result = tokio::select! {
            biased;
            result = &mut operation => result,
            () = &mut sleep => {
                let pending: Vec<String> =
                    lock(&activities).iter().map(|(name, _)| name.clone()).collect();
                let suffix = if pending.is_empty() {
                    String::new()
                } else {
                    format!("; pending: {}", pending.join(", "))
                };
                let error = AgentError::new(format!(
                    "Timed out refreshing {} after {timeout_ms}ms{suffix}",
                    definition.label
                ));
                controller.abort(AbortReason::Error(error.clone()));
                timeout_error = Some(error);
                operation.await
            }
        };
        match timeout_error {
            Some(error) => Err(error),
            None => result,
        }
    }

    /// `resolveParent(parent)`: `AgentCreateConfigParent`.
    fn resolve_parent(&self, parent: &CreateConfigParentAgent) -> Result<JsValue, AgentError> {
        let definition = self.require_provider(&parent.provider)?;
        let available_modes = parent
            .available_modes
            .clone()
            .or_else(|| definition.modes.clone())
            .unwrap_or(JsValue::Array(Vec::new()));
        let unattended =
            definition.is_create_config_unattended(&AgentCreateConfigUnattendedInput {
                mode_id: parent.current_mode_id.clone(),
                config: parent.config.clone(),
                features: parent.features.clone(),
                available_modes,
            });
        let mut out = JsObject::new();
        out.insert("provider", JsValue::String(parent.provider.clone()));
        out.insert(
            "modeId",
            parent
                .current_mode_id
                .clone()
                .map_or(JsValue::Null, JsValue::String),
        );
        out.insert("isUnattended", JsValue::Bool(unattended));
        Ok(JsValue::Object(out))
    }

    /// `publishTargets(cwds)`.
    fn publish_targets(&self, cwds: &[String]) {
        let (transitions, listeners) = {
            let mut state = self.lock();
            if state.destroyed {
                return;
            }
            let mut transitions = Vec::new();
            for cwd in cwds {
                let records: Vec<ProviderSnapshotRecord> = self
                    .definitions
                    .iter()
                    .enumerate()
                    .map(|(index, definition)| {
                        let binding = state
                            .target(cwd)
                            .and_then(|target| target.bindings.get(&definition.provider));
                        let result = binding
                            .and_then(|binding| binding.key.as_ref())
                            .and_then(|key| state.catalogs.get(key))
                            .and_then(|catalogs| catalogs.get(&definition.provider))
                            .and_then(|catalog| catalog.result.clone());
                        binding
                            .and_then(|binding| binding.failure.clone())
                            .or(result)
                            .unwrap_or_else(|| self.initial[index].clone())
                    })
                    .collect();
                let Some(target) = state.target_mut(cwd) else {
                    continue;
                };
                if same_snapshot_records(&target.snapshot.records, &records) {
                    continue;
                }
                let current = Arc::new(ProviderSnapshot {
                    cwd: cwd.clone(),
                    records,
                });
                let previous = std::mem::replace(&mut target.snapshot, Arc::clone(&current));
                transitions.push(ProviderSnapshotTransition { previous, current });
            }
            let listeners: Vec<ProviderSnapshotListener> = state
                .listeners
                .iter()
                .map(|(_, listener)| Arc::clone(listener))
                .collect();
            (transitions, listeners)
        };
        for transition in &transitions {
            for listener in &listeners {
                listener(transition);
            }
        }
    }
}

fn error_entry(base: &JsValue, message: String) -> JsObject {
    let mut entry = spread(Some(base));
    entry.insert("status", JsValue::String("error".to_owned()));
    entry.insert("enabled", JsValue::Bool(true));
    entry.insert("error", JsValue::String(message));
    entry
}

/// The ready entry for a catalogue, or the `TypeError` message reading a
/// catalogue without a `models` array throws.
fn ready_entry(base: &JsValue, catalog: &JsValue) -> Result<JsObject, String> {
    let models = match catalog.get("models") {
        Some(JsValue::Array(models)) => normalize_agent_model_catalog(models),
        Some(JsValue::Null) => {
            return Err("Cannot read properties of null (reading 'filter')".to_owned());
        }
        None | Some(JsValue::Undefined) => {
            return Err("Cannot read properties of undefined (reading 'filter')".to_owned());
        }
        Some(_) => return Err("models.filter is not a function".to_owned()),
    };
    let mut entry = spread(Some(base));
    let default_mode_id = match catalog.get("defaultModeId") {
        None | Some(JsValue::Undefined) => base.get("defaultModeId").cloned(),
        Some(value) => Some(value.clone()),
    };
    entry.insert(
        "defaultModeId",
        default_mode_id.unwrap_or(JsValue::Undefined),
    );
    entry.insert("status", JsValue::String("ready".to_owned()));
    entry.insert("enabled", JsValue::Bool(true));
    entry.insert("models", JsValue::Array(models));
    entry.insert(
        "modes",
        catalog.get("modes").cloned().unwrap_or(JsValue::Undefined),
    );
    entry.insert("fetchedAt", JsValue::String(now_iso()));
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::{base64url, valid_refresh_deadline};

    #[test]
    fn base64url_has_no_padding() {
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn refresh_deadlines_are_positive_safe_integers_within_the_timer_range() {
        assert_eq!(valid_refresh_deadline(5.0), Some(5));
        assert_eq!(valid_refresh_deadline(0.0), None);
        assert_eq!(valid_refresh_deadline(1.5), None);
        assert_eq!(valid_refresh_deadline(2_147_483_648.0), None);
        assert_eq!(valid_refresh_deadline(f64::NAN), None);
    }
}
