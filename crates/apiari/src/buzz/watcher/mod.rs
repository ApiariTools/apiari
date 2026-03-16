//! Watcher trait and registry.
//!
//! Each watcher polls an external source and returns `SignalUpdate`s that
//! get upserted into the SQLite signal store.

pub mod github;
pub mod review_queue;
pub mod sentry;
pub mod swarm;

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

/// Registry of active watchers.
pub struct WatcherRegistry {
    watchers: Vec<Box<dyn Watcher>>,
}

impl WatcherRegistry {
    pub fn new() -> Self {
        Self {
            watchers: Vec::new(),
        }
    }

    pub fn add(&mut self, watcher: Box<dyn Watcher>) {
        self.watchers.push(watcher);
    }

    pub fn watchers_mut(&mut self) -> &mut [Box<dyn Watcher>] {
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
