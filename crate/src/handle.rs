//! Parse-once document handles. A handle is an opaque i64 naming a parsed,
//! validated, spring-baked document ([`Compiled`]) stored in a process-global
//! registry. Handles are immutable after creation, so any number of concurrent
//! renders on the same or different handles are safe (per-render state is
//! created per call). IDs are monotonic and never reused, so a freed handle is
//! simply absent from the registry — a use-after-free reports an error, never a
//! crash.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use crate::validate::Compiled;

static REGISTRY: OnceLock<RwLock<HashMap<i64, Arc<Compiled>>>> = OnceLock::new();
// Starts at 1 so 0 is always the error/invalid sentinel.
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

fn registry() -> &'static RwLock<HashMap<i64, Arc<Compiled>>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Store a compiled document, returning its handle (always > 0).
pub fn insert(compiled: Compiled) -> i64 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    registry()
        .write()
        .expect("handle registry poisoned")
        .insert(id, Arc::new(compiled));
    id
}

/// Look up a handle. `None` for an invalid or already-freed handle. The `Arc`
/// keeps the document alive for the duration of a render even if another thread
/// frees the handle concurrently.
pub fn get(handle: i64) -> Option<Arc<Compiled>> {
    registry().read().ok()?.get(&handle).cloned()
}

/// Drop a handle. Idempotent — freeing an unknown or already-freed handle is a
/// no-op.
pub fn remove(handle: i64) {
    if let Ok(mut map) = registry().write() {
        map.remove(&handle);
    }
}

/// Whether a handle is currently live. Test-only: lets leak tests assert a
/// specific id was removed, which is isolation-proof under concurrent tests
/// (ids are monotonic and unique) unlike a process-RSS gauge.
#[cfg(test)]
pub fn contains(handle: i64) -> bool {
    registry()
        .read()
        .map(|map| map.contains_key(&handle))
        .unwrap_or(false)
}
