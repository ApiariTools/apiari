use std::sync::{Mutex, MutexGuard, OnceLock};

/// Process-wide mutex that serializes any test that mutates `PATH` (or any
/// other process-wide environment variable).  Both `buzz::coordinator` and
/// `buzz::swarm_reconciler` install fake binaries by prepending a temp dir to
/// `PATH`.  If those test modules used independent locks they could race; a
/// single shared lock prevents that.
pub(crate) fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}
