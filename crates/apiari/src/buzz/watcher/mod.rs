//! Watcher trait and registry.
//!
//! Each watcher polls an external source and returns `SignalUpdate`s that
//! get upserted into the SQLite signal store.
//!
//! After each poll, stale signals are automatically reconciled: any open
//! signal in the DB whose `external_id` was NOT emitted in the latest poll
//! is resolved. Watchers that need custom reconciliation (e.g. swarm, which
//! builds IDs from tracked state) can override `reconcile()`.

pub mod email;
pub mod github;
pub mod linear;
pub mod notion;
pub mod review_queue;
pub mod script;
pub mod sentry;
pub mod swarm;

use std::time::{Duration, Instant};

use async_trait::async_trait;
use color_eyre::Result;
use tracing::info;

use crate::buzz::signal::SignalUpdate;
use crate::buzz::signal::store::SignalStore;

/// A pluggable source that can be polled for new signals.
#[async_trait]
pub trait Watcher: Send + Sync {
    /// Human-readable name (used in logging and cursor keys).
    fn name(&self) -> &str;

    /// Signal source string stored in the DB. Defaults to `name()`.
    /// Override if the source differs from the watcher name (e.g. "github_review_queue").
    fn signal_source(&self) -> &str {
        self.name()
    }

    /// Poll the external source and return signal updates.
    /// The store is passed so watchers can read/write their own cursors.
    async fn poll(&mut self, store: &SignalStore) -> Result<Vec<SignalUpdate>>;

    /// Reconcile the store after a successful poll.
    ///
    /// Resolves DB signals from this source that are no longer in the latest poll results.
    /// Default: uses `signal_source()` + IDs collected by `ThrottledWatcher` (auto-reconcile).
    /// Override for custom reconciliation logic (e.g. swarm builds IDs from tracked state).
    fn reconcile(
        &self,
        _source: &str,
        _poll_ids: &[String],
        _store: &SignalStore,
    ) -> Result<usize> {
        Ok(0)
    }
}

/// A watcher with per-watcher poll throttling and automatic signal reconciliation.
///
/// After each successful poll, collects emitted signal `external_id`s and
/// resolves any open signals in the DB that weren't in the latest poll.
/// Watchers that override `reconcile()` get their custom logic called instead.
pub struct ThrottledWatcher {
    inner: Box<dyn Watcher>,
    interval: Duration,
    last_poll: Option<Instant>,
    /// External IDs from the last successful poll, for auto-reconciliation.
    last_poll_ids: Option<Vec<String>>,
}

impl ThrottledWatcher {
    pub fn new(watcher: Box<dyn Watcher>, interval_secs: u64) -> Self {
        Self {
            inner: watcher,
            interval: Duration::from_secs(interval_secs),
            last_poll: None,
            last_poll_ids: None,
        }
    }

    /// Returns true if this watcher has never been polled or enough time has elapsed.
    pub fn should_poll(&self) -> bool {
        match self.last_poll {
            None => true,
            Some(t) => t.elapsed() >= self.interval,
        }
    }

    /// Mark this watcher as just polled.
    pub fn mark_polled(&mut self) {
        self.last_poll = Some(Instant::now());
    }

    /// Store the external IDs from the latest poll for reconciliation.
    pub fn set_poll_ids(&mut self, ids: Vec<String>) {
        self.last_poll_ids = Some(ids);
    }

    /// Reconcile stale signals after a successful poll.
    ///
    /// First tries the watcher's custom `reconcile()`. If it returns 0
    /// (no custom logic), falls back to auto-reconcile using poll IDs.
    pub fn reconcile(&self, store: &SignalStore) -> Result<usize> {
        let source = self.inner.signal_source();
        let poll_ids = self.last_poll_ids.as_deref().unwrap_or(&[]);

        // Let the watcher handle it if it has custom logic
        let custom = self.inner.reconcile(source, poll_ids, store)?;
        if custom > 0 {
            return Ok(custom);
        }

        // Auto-reconcile: resolve signals not in latest poll
        let Some(ref ids) = self.last_poll_ids else {
            return Ok(0); // Never polled — skip
        };
        let resolved = store.resolve_missing_signals(source, ids)?;
        if resolved > 0 {
            info!(
                "{}: reconciled {resolved} stale signal(s)",
                self.inner.name()
            );
        }
        Ok(resolved)
    }

    /// Access the underlying watcher.
    pub fn watcher_mut(&mut self) -> &mut dyn Watcher {
        &mut *self.inner
    }

    /// Access the underlying watcher (shared ref).
    pub fn watcher(&self) -> &dyn Watcher {
        &*self.inner
    }
}

/// Registry of active watchers.
pub struct WatcherRegistry {
    watchers: Vec<ThrottledWatcher>,
}

impl WatcherRegistry {
    pub fn new() -> Self {
        Self {
            watchers: Vec::new(),
        }
    }

    /// Add a watcher that polls every tick (interval = 0).
    pub fn add(&mut self, watcher: Box<dyn Watcher>) {
        self.watchers.push(ThrottledWatcher::new(watcher, 0));
    }

    /// Add a watcher with a specific poll interval in seconds.
    pub fn add_with_interval(&mut self, watcher: Box<dyn Watcher>, interval_secs: u64) {
        self.watchers
            .push(ThrottledWatcher::new(watcher, interval_secs));
    }

    pub fn watchers_mut(&mut self) -> &mut [ThrottledWatcher] {
        &mut self.watchers
    }

    pub fn is_empty(&self) -> bool {
        self.watchers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.watchers.len()
    }
}

impl Default for WatcherRegistry {
    fn default() -> Self {
        Self::new()
    }
}
