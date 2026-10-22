//! [`CatalogService`]: an always-available catalog that refreshes live
//! model lists in the background.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

use super::cache::{DEFAULT_TTL, LiveList, ModelListCache};
use super::live::{ListKind, ModelListSource};
use super::{AuthMethod, Catalog, CredentialState, Credentials, PROVIDER_ORDER, ProviderStatus};
use crate::provider::ProviderError;
use crate::registry::Registry;

/// The current time in seconds since the Unix epoch. Injected so tests can
/// move time.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

fn system_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    })
}

/// Everything a [`CatalogService`] needs.
#[derive(Clone)]
pub struct CatalogConfig {
    /// Built-in models, aliases and price overrides.
    pub registry: Arc<Registry>,
    /// Credential state per provider.
    pub credentials: Credentials,
    /// Live lists to fetch, usually one per provider with a working key.
    pub sources: Vec<ModelListSource>,
    /// Where live lists are cached.
    pub cache: ModelListCache,
    /// How long a cached list stays fresh.
    pub ttl: Duration,
    /// Time source.
    pub clock: Clock,
}

impl CatalogConfig {
    /// A config with no live sources, the default TTL and the system clock.
    pub fn new(registry: Arc<Registry>, credentials: Credentials, cache: ModelListCache) -> Self {
        Self {
            registry,
            credentials,
            sources: Vec::new(),
            cache,
            ttl: DEFAULT_TTL,
            clock: system_clock(),
        }
    }

    /// Sets the live sources.
    pub fn with_sources(mut self, sources: Vec<ModelListSource>) -> Self {
        self.sources = sources;
        self
    }

    /// Sets the TTL.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Sets the clock.
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }
}

/// What one refresh did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshReport {
    /// Providers whose list was fetched and cached.
    pub refreshed: Vec<String>,
    /// Providers whose fetch failed, with the error.
    pub failed: Vec<(String, String)>,
    /// True when another refresh was already running, so nothing was done.
    pub already_running: bool,
}

struct State {
    credentials: Credentials,
    sources: Vec<ModelListSource>,
    live: BTreeMap<String, LiveList>,
    errors: BTreeMap<String, String>,
    rejected: BTreeMap<String, String>,
    refreshing: bool,
}

struct Inner {
    registry: Arc<Registry>,
    cache: ModelListCache,
    ttl: Duration,
    clock: Clock,
    client: reqwest::Client,
    state: Mutex<State>,
    tx: watch::Sender<Arc<Catalog>>,
}

/// Keeps a [`Catalog`] current.
///
/// Construction reads the on-disk cache only, so [`CatalogService::catalog`]
/// is available instantly and offline. [`CatalogService::refresh`] (or
/// [`CatalogService::spawn_refresh`] for the background) fetches lists that
/// are missing or older than the TTL, writes them to the cache and
/// publishes a new catalog to every [`CatalogService::subscribe`]r. A failed
/// fetch keeps the cached list; an HTTP 401/403 marks the provider's API key
/// as rejected so its models stop being usable.
#[derive(Clone)]
pub struct CatalogService {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for CatalogService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogService")
            .field("cache", &self.inner.cache)
            .field("ttl", &self.inner.ttl)
            .finish_non_exhaustive()
    }
}

impl CatalogService {
    /// Loads cached lists for every known provider and builds the first
    /// catalog. Does no network I/O.
    pub fn new(config: CatalogConfig) -> Self {
        let mut providers: BTreeSet<String> =
            PROVIDER_ORDER.iter().map(|p| (*p).to_owned()).collect();
        providers.extend(config.sources.iter().map(|s| s.provider.clone()));
        let live = providers
            .iter()
            .filter(|p| ListKind::for_provider(p).is_some())
            .filter_map(|p| config.cache.load(p).map(|l| (p.clone(), l)))
            .collect();
        let state = State {
            credentials: config.credentials,
            sources: config.sources,
            live,
            errors: BTreeMap::new(),
            rejected: BTreeMap::new(),
            refreshing: false,
        };
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("harness/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        let (tx, _) = watch::channel(Arc::new(Catalog::default()));
        let inner = Inner {
            registry: config.registry,
            cache: config.cache,
            ttl: config.ttl,
            clock: config.clock,
            client,
            state: Mutex::new(state),
            tx,
        };
        let service = Self {
            inner: Arc::new(inner),
        };
        service.publish(&service.lock());
        service
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current catalog.
    pub fn catalog(&self) -> Arc<Catalog> {
        self.inner.tx.borrow().clone()
    }

    /// A receiver that sees every new catalog (after each refresh starts
    /// and finishes, and after credential changes).
    pub fn subscribe(&self) -> watch::Receiver<Arc<Catalog>> {
        self.inner.tx.subscribe()
    }

    /// Replaces credentials and live sources (after a login or logout),
    /// forgets earlier key rejections and publishes a new catalog. Call
    /// [`CatalogService::spawn_refresh`] afterwards to fetch new lists.
    pub fn set_credentials(&self, credentials: Credentials, sources: Vec<ModelListSource>) {
        let mut st = self.lock();
        st.credentials = credentials;
        st.sources = sources;
        st.rejected.clear();
        st.errors.clear();
        self.publish(&st);
    }

    /// Providers whose source has no fresh cached list.
    pub fn stale_providers(&self) -> Vec<String> {
        let st = self.lock();
        self.stale_sources(&st)
            .into_iter()
            .map(|s| s.provider)
            .collect()
    }

    fn stale_sources(&self, st: &State) -> Vec<ModelListSource> {
        let now = (self.inner.clock)();
        st.sources
            .iter()
            .filter(|s| {
                !st.live
                    .get(&s.provider)
                    .is_some_and(|l| l.is_fresh(now, self.inner.ttl, &s.base_url))
            })
            .cloned()
            .collect()
    }

    /// Fetches stale lists (every list when `force`), caches them and
    /// publishes the new catalog. Fetches run concurrently.
    pub async fn refresh(&self, force: bool) -> RefreshReport {
        let targets = {
            let mut st = self.lock();
            if st.refreshing {
                return RefreshReport {
                    already_running: true,
                    ..RefreshReport::default()
                };
            }
            let targets = if force {
                st.sources.clone()
            } else {
                self.stale_sources(&st)
            };
            if targets.is_empty() {
                return RefreshReport::default();
            }
            st.refreshing = true;
            self.publish(&st);
            targets
        };
        let client = self.inner.client.clone();
        let results = futures::future::join_all(targets.iter().map(|s| {
            let client = client.clone();
            async move { s.fetch(&client).await }
        }))
        .await;

        let now = (self.inner.clock)();
        let mut report = RefreshReport::default();
        let mut st = self.lock();
        for (source, result) in targets.iter().zip(results) {
            let p = source.provider.clone();
            match result {
                Ok(models) => {
                    let list = LiveList::new(&p, &source.base_url, now, models);
                    if let Err(e) = self.inner.cache.store(&list) {
                        tracing::warn!(provider = %p, error = %e, "could not cache model list");
                    }
                    st.live.insert(p.clone(), list);
                    st.errors.remove(&p);
                    st.rejected.remove(&p);
                    report.refreshed.push(p);
                }
                Err(e) => {
                    let msg = e.to_string();
                    tracing::warn!(provider = %p, error = %msg, "model list refresh failed");
                    if let ProviderError::Auth(m) = &e {
                        st.rejected
                            .insert(p.clone(), format!("rejected by {p}: {m}"));
                    }
                    st.errors.insert(p.clone(), msg.clone());
                    report.failed.push((p, msg));
                }
            }
        }
        st.refreshing = false;
        self.publish(&st);
        report
    }

    /// Runs [`CatalogService::refresh`] on the current Tokio runtime. Returns
    /// `None` when there is nothing to fetch, a refresh is already running,
    /// or no runtime is available.
    pub fn spawn_refresh(&self, force: bool) -> Option<tokio::task::JoinHandle<RefreshReport>> {
        {
            let st = self.lock();
            if st.refreshing || (!force && self.stale_sources(&st).is_empty()) {
                return None;
            }
            if force && st.sources.is_empty() {
                return None;
            }
        }
        let handle = tokio::runtime::Handle::try_current().ok()?;
        let me = self.clone();
        Some(handle.spawn(async move { me.refresh(force).await }))
    }

    fn effective_credentials(&self, st: &State) -> Credentials {
        let mut creds = st.credentials.clone();
        for (p, msg) in &st.rejected {
            creds.set(
                p,
                AuthMethod::ApiKey,
                CredentialState::Rejected(msg.clone()),
            );
        }
        creds
    }

    fn publish(&self, st: &State) {
        let creds = self.effective_credentials(st);
        let now = (self.inner.clock)();
        let mut providers: Vec<String> = PROVIDER_ORDER.iter().map(|p| (*p).to_owned()).collect();
        for p in creds
            .providers()
            .map(str::to_owned)
            .chain(st.sources.iter().map(|s| s.provider.clone()))
        {
            if !providers.contains(&p) {
                providers.push(p);
            }
        }
        let statuses = providers
            .into_iter()
            .map(|p| {
                let list = st.live.get(&p);
                let base = st
                    .sources
                    .iter()
                    .find(|s| s.provider == p)
                    .map(|s| s.base_url.clone());
                // Only a configured source can be refreshed, so only it
                // can be stale.
                let stale = base
                    .is_some_and(|b| !list.is_some_and(|l| l.is_fresh(now, self.inner.ttl, &b)));
                ProviderStatus {
                    auth: creds.statuses(&p),
                    fetched_at: list.map(|l| l.fetched_at),
                    live_models: list.map_or(0, |l| l.models.len()),
                    stale,
                    error: st.errors.get(&p).cloned(),
                    provider: p,
                }
            })
            .collect();
        let catalog = Catalog::build(&self.inner.registry, &creds, &st.live)
            .with_status(statuses, st.refreshing);
        self.inner.tx.send_replace(Arc::new(catalog));
    }
}
