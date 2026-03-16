//! Watcher trait and registry.
//!
//! Each watcher polls an external source and returns `SignalUpdate`s that
//! get upserted into the SQLite signal store.

pub mod github;
pub mod review_queue;
pub mod sentry;
pub mod swarm;

use std::time::{Duration, Instant};

use async_trait::async_trait;
use color_eyre::Result;

use crate::buzz::signal::SignalUpdate;
use crate::buzz::signal::store::SignalStore;

/// A pluggable source that can be polled for new signals.
#[async_trait]
pub trait Watcher: Send + Sync {
    /// Human-readable name (used in logging and cursor keys).
    fn name(&self) -> &str;

    /// Poll the external source and return signal updates.
    /// The store is passed so watchers can read/write their own cursors.
    async fn poll(&mut self, store: &SignalStore) -> Result<Vec<SignalUpdate>>;

    /// Reconcile the store after a successful poll.
    ///
    /// Resolves DB signals from this source that are no longer in the latest poll results.
    /// This is synchronous (not async) because SignalStore is not Sync.
    /// Default implementation is a no-op; watchers override to provide current external IDs.
    fn reconcile(&self, _store: &SignalStore) -> Result<usize> {
        Ok(0)
    }
}

/// A watcher with per-watcher poll throttling.
pub struct ThrottledWatcher {
    inner: Box<dyn Watcher>,
    interval: Duration,
    last_poll: Option<Instant>,
}

impl ThrottledWatcher {
    pub fn new(watcher: Box<dyn Watcher>, interval_secs: u64) -> Self {
        Self {
            inner: watcher,
            interval: Duration::from_secs(interval_secs),
            last_poll: None,
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
