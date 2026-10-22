//! Coalesced work with weak completed results. This cache owns no completed pixel buffers.
//! Callers bound the number of distinct in-flight requests (a `sched::Queue`, a `Task`'s
//! background capacity).
use std::collections::HashMap;
use std::hash::Hash;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// Weak metadata operations must not block or panic. Values remain owned by their users.
pub trait WeakValue: Clone + Send + 'static {
    type Weak: Send;
    fn downgrade(&self) -> Self::Weak;
    fn upgrade(weak: &Self::Weak) -> Option<Self>;
    fn alive(weak: &Self::Weak) -> bool;
}
impl<T: Send + Sync + 'static> WeakValue for Arc<T> {
    type Weak = std::sync::Weak<T>;
    fn downgrade(&self) -> Self::Weak {
        Arc::downgrade(self)
    }
    fn upgrade(weak: &Self::Weak) -> Option<Self> {
        weak.upgrade()
    }
    fn alive(weak: &Self::Weak) -> bool {
        weak.strong_count() != 0
    }
}
impl<M: Clone + Send + 'static, T: Send + Sync + 'static> WeakValue for (M, Arc<T>) {
    type Weak = (M, std::sync::Weak<T>);
    fn downgrade(&self) -> Self::Weak {
        (self.0.clone(), Arc::downgrade(&self.1))
    }
    fn upgrade(weak: &Self::Weak) -> Option<Self> {
        Some((weak.0.clone(), weak.1.upgrade()?))
    }
    fn alive(weak: &Self::Weak) -> bool {
        weak.1.strong_count() != 0
    }
}

struct Flight<V> {
    result: Mutex<Option<Option<V>>>,
    ready: Condvar,
}
enum Entry<V: WeakValue> {
    Loading(Arc<Flight<V>>),
    Ready { value: V::Weak, touched: u64 },
}
struct State<K, V: WeakValue> {
    entries: HashMap<K, Entry<V>>,
    clock: u64,
}
impl<K, V: WeakValue> State<K, V> {
    /// Moves the clock a step and gives its new time (an entry's `touched`).
    fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CacheStats {
    pub hits: u64,
    pub loads: u64,
    pub waits: u64,
}
pub struct WeakCache<K, V: WeakValue> {
    state: Mutex<State<K, V>>,
    max_ready: usize,
    #[cfg(test)]
    hits: AtomicU64,
    #[cfg(test)]
    loads: AtomicU64,
    #[cfg(test)]
    waits: AtomicU64,
}
impl<K: Eq + Hash + Clone, V: WeakValue> WeakCache<K, V> {
    /// Limits completed lookup metadata, not live image bytes or pending job memory.
    pub fn new(max_ready: usize) -> Self {
        Self {
            state: Mutex::new(State { entries: HashMap::new(), clock: 0 }),
            max_ready,
            #[cfg(test)]
            hits: AtomicU64::new(0),
            #[cfg(test)]
            loads: AtomicU64::new(0),
            #[cfg(test)]
            waits: AtomicU64::new(0),
        }
    }
    #[cfg(test)]
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            loads: self.loads.load(Ordering::Relaxed),
            waits: self.waits.load(Ordering::Relaxed),
        }
    }
    pub fn get_or_load(&self, key: K, load: impl FnOnce() -> Option<V>) -> Option<V> {
        let mut state = self.state.lock().unwrap();
        let clock = state.tick();
        if let Some(entry) = state.entries.get_mut(&key) {
            match entry {
                Entry::Ready { value, touched } => {
                    if let Some(value) = V::upgrade(value) {
                        *touched = clock;
                        #[cfg(test)]
                        {
                            self.hits.fetch_add(1, Ordering::Relaxed);
                        }
                        return Some(value);
                    }
                }
                Entry::Loading(flight) => {
                    let flight = flight.clone();
                    #[cfg(test)]
                    {
                        self.waits.fetch_add(1, Ordering::Relaxed);
                    }
                    drop(state);
                    let mut result = flight.result.lock().unwrap();
                    while result.is_none() {
                        result = flight.ready.wait(result).unwrap();
                    }
                    return result.as_ref().unwrap().clone();
                }
            }
        }
        let flight = Arc::new(Flight { result: Mutex::new(None), ready: Condvar::new() });
        state.entries.insert(key.clone(), Entry::Loading(flight.clone()));
        #[cfg(test)]
        {
            self.loads.fetch_add(1, Ordering::Relaxed);
        }
        drop(state);
        // Publish failure even when a decoder unwinds, so waiters cannot be stranded.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(load));
        let value = result.as_ref().ok().and_then(|v| v.clone());
        let mut state = self.state.lock().unwrap();
        let completed_at = state.tick();
        if let Some(value) = &value {
            state.entries.insert(key.clone(), Entry::Ready { value: value.downgrade(), touched: completed_at });
        } else {
            state.entries.remove(&key);
        }
        state.entries.retain(|_, entry| match entry {
            Entry::Loading(_) => true,
            Entry::Ready { value, .. } => V::alive(value),
        });
        while state.entries.values().filter(|v| matches!(v, Entry::Ready { .. })).count() > self.max_ready {
            let oldest = state
                .entries
                .iter()
                .filter_map(|(k, v)| match v {
                    Entry::Ready { touched, .. } => Some((k.clone(), *touched)),
                    _ => None,
                })
                .min_by_key(|(_, age)| *age)
                .map(|(key, _)| key);
            if let Some(key) = oldest {
                state.entries.remove(&key);
            } else {
                break;
            }
        }
        drop(state);
        *flight.result.lock().unwrap() = Some(value.clone());
        flight.ready.notify_all();
        match result {
            Ok(_) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}
