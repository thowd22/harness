//! Content-hash cache for remote file transfers.
//!
//! Agents read the same files many times. The client and the helper each
//! keep a bounded cache of file contents keyed by their sha256
//! ([`content_hash`]) for the life of a session, so a read or write whose
//! content the other side already holds sends only the hash:
//!
//! - **read:** the client sends the hash it last saw for the path
//!   (`have`). The helper always reads the file; if its hash is the same it
//!   answers `cached` with the hash and no bytes, and the client serves its
//!   copy.
//! - **write:** when the client knows the helper holds the new content (it
//!   sent or received it this session), it sends only the hash. The helper
//!   writes its cached copy, or answers `CACHE_MISS` and the client sends
//!   the bytes.
//!
//! Every cached copy is checked against its hash before use; a miss, an
//! eviction or a mismatch on either side falls back to a full transfer, so
//! the cache never changes what a read returns or what a write stores.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub use harness_tools::tracker::content_hash;

/// Default per-session cache size on each side (64 MiB).
pub const DEFAULT_CONTENT_CACHE: usize = 64 * 1024 * 1024;

/// Fraction of the capacity a single entry may take (larger files are
/// transferred in full every time rather than flushing the cache).
const MAX_ENTRY_DIVISOR: usize = 4;

#[derive(Debug)]
struct Entry<V> {
    value: V,
    weight: usize,
    tick: u64,
}

/// A least-recently-used map bounded by the total weight of its entries.
#[derive(Debug)]
struct Lru<V> {
    capacity: usize,
    used: usize,
    tick: u64,
    map: HashMap<String, Entry<V>>,
    order: BTreeMap<u64, String>,
    evictions: u64,
}

impl<V> Lru<V> {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            used: 0,
            tick: 0,
            map: HashMap::new(),
            order: BTreeMap::new(),
            evictions: 0,
        }
    }

    fn admits(&self, weight: usize) -> bool {
        self.capacity > 0 && weight <= self.capacity / MAX_ENTRY_DIVISOR
    }

    fn bump(&mut self, key: &str) -> Option<&Entry<V>> {
        self.tick += 1;
        let tick = self.tick;
        let e = self.map.get_mut(key)?;
        self.order.remove(&e.tick);
        e.tick = tick;
        self.order.insert(tick, key.to_owned());
        Some(e)
    }

    fn insert(&mut self, key: String, value: V, weight: usize) -> bool {
        if !self.admits(weight) {
            self.remove(&key);
            return false;
        }
        self.remove(&key);
        while self.used + weight > self.capacity {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some(e) = self.map.remove(&oldest) {
                self.used -= e.weight;
                self.evictions += 1;
            }
        }
        self.tick += 1;
        self.order.insert(self.tick, key.clone());
        self.map.insert(
            key,
            Entry {
                value,
                weight,
                tick: self.tick,
            },
        );
        self.used += weight;
        true
    }

    fn remove(&mut self, key: &str) -> bool {
        match self.map.remove(key) {
            Some(e) => {
                self.order.remove(&e.tick);
                self.used -= e.weight;
                true
            }
            None => false,
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.used = 0;
    }
}

/// Counters of a [`ContentCache`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Entries held.
    pub entries: usize,
    /// Bytes held.
    pub bytes: usize,
    /// Entries dropped to make room.
    pub evictions: u64,
}

/// File contents keyed by content hash, bounded by total size (least
/// recently used entries are evicted first). A capacity of 0 disables it.
#[derive(Debug)]
pub struct ContentCache {
    lru: Lru<Arc<Vec<u8>>>,
}

impl ContentCache {
    /// A cache holding at most `capacity` bytes; a single entry may take a
    /// quarter of it.
    pub fn new(capacity: usize) -> Self {
        Self {
            lru: Lru::new(capacity),
        }
    }

    /// The capacity in bytes.
    pub fn capacity(&self) -> usize {
        self.lru.capacity
    }

    /// True when the capacity is 0.
    pub fn is_disabled(&self) -> bool {
        self.lru.capacity == 0
    }

    /// Store `data` under `hash` (computed by the caller with
    /// [`content_hash`]). Returns false when it is too large to cache.
    pub fn insert(&mut self, hash: &str, data: Arc<Vec<u8>>) -> bool {
        let weight = data.len();
        self.lru.insert(hash.to_owned(), data, weight)
    }

    /// The contents for `hash`, if held and still matching the hash. An
    /// entry whose contents no longer hash to its key is dropped.
    pub fn get(&mut self, hash: &str) -> Option<Arc<Vec<u8>>> {
        let data = self.lru.bump(hash)?.value.clone();
        if content_hash(&data) == hash {
            Some(data)
        } else {
            self.lru.remove(hash);
            None
        }
    }

    /// True if `hash` is held (no verification, no recency update).
    pub fn contains(&self, hash: &str) -> bool {
        self.lru.map.contains_key(hash)
    }

    /// Drop `hash`.
    pub fn remove(&mut self, hash: &str) -> bool {
        self.lru.remove(hash)
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.lru.clear();
    }

    /// Current counters.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.lru.map.len(),
            bytes: self.lru.used,
            evictions: self.lru.evictions,
        }
    }

    /// Replace the stored bytes for `hash` without rehashing (tests use it
    /// to simulate a corrupted entry).
    #[doc(hidden)]
    pub fn corrupt_for_test(&mut self, hash: &str, data: Vec<u8>) {
        if let Some(e) = self.lru.map.get_mut(hash) {
            e.value = Arc::new(data);
        }
    }
}

/// The client's model of what the helper holds: hashes and sizes, bounded
/// like the helper's cache so it forgets roughly what the helper evicts.
/// It is only a hint; the helper answers `CACHE_MISS` when it is wrong.
#[derive(Debug)]
pub struct PeerCache {
    lru: Lru<()>,
}

impl PeerCache {
    /// Mirror a peer cache of `capacity` bytes.
    pub fn new(capacity: usize) -> Self {
        Self {
            lru: Lru::new(capacity),
        }
    }

    /// Set the peer's capacity (from the handshake) and forget everything.
    pub fn reset(&mut self, capacity: usize) {
        self.lru = Lru::new(capacity);
    }

    /// The peer now holds `hash` of `size` bytes (if it fits its cache).
    pub fn note(&mut self, hash: &str, size: usize) {
        self.lru.insert(hash.to_owned(), (), size);
    }

    /// True if the peer probably holds `hash`; counts as a use.
    pub fn holds(&mut self, hash: &str) -> bool {
        self.lru.bump(hash).is_some()
    }

    /// The peer does not hold `hash`.
    pub fn forget(&mut self, hash: &str) {
        self.lru.remove(hash);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(n: usize, b: u8) -> Arc<Vec<u8>> {
        Arc::new(vec![b; n])
    }

    #[test]
    fn stores_and_verifies_by_hash() {
        let mut c = ContentCache::new(1000);
        let d = blob(100, b'a');
        let h = content_hash(&d);
        assert!(c.insert(&h, d.clone()));
        assert_eq!(c.get(&h).as_deref(), Some(&*d));
        assert!(c.get("nope").is_none());
        c.corrupt_for_test(&h, vec![b'b'; 100]);
        assert!(c.get(&h).is_none(), "a mismatching entry is never served");
        assert!(!c.contains(&h), "and is dropped");
    }

    #[test]
    fn bounded_by_bytes_with_lru_eviction() {
        let mut c = ContentCache::new(1000);
        let (a, b, d) = (blob(200, 1), blob(200, 2), blob(200, 3));
        let (ha, hb, hd) = (content_hash(&a), content_hash(&b), content_hash(&d));
        c.insert(&ha, a);
        c.insert(&hb, b);
        c.insert(&hd, d);
        assert!(c.get(&ha).is_some(), "touch a so b is the oldest");
        for i in 0..2u8 {
            let e = blob(200, 10 + i);
            c.insert(&content_hash(&e), e);
        }
        // 5 x 200 = 1000 fits; one more evicts the least recently used (b).
        let f = blob(200, 20);
        c.insert(&content_hash(&f), f);
        assert!(!c.contains(&hb));
        assert!(c.contains(&ha) && c.contains(&hd));
        let s = c.stats();
        assert!(s.bytes <= 1000, "{s:?}");
        assert_eq!(s.evictions, 1);
    }

    #[test]
    fn oversized_entries_and_disabled_cache_are_not_stored() {
        let mut c = ContentCache::new(1000);
        let big = blob(251, 0);
        assert!(!c.insert(&content_hash(&big), big));
        assert_eq!(c.stats().entries, 0);
        let mut off = ContentCache::new(0);
        assert!(off.is_disabled());
        let d = blob(1, 0);
        assert!(!off.insert(&content_hash(&d), d));
    }

    #[test]
    fn reinsert_replaces_weight() {
        let mut c = ContentCache::new(1000);
        let d = blob(100, 1);
        let h = content_hash(&d);
        c.insert(&h, d.clone());
        c.insert(&h, d);
        assert_eq!(c.stats().bytes, 100);
        assert!(c.remove(&h));
        assert_eq!(
            c.stats(),
            CacheStats {
                entries: 0,
                bytes: 0,
                evictions: 0
            }
        );
    }

    #[test]
    fn peer_cache_forgets_like_the_peer() {
        let mut p = PeerCache::new(400);
        p.note("a", 100);
        p.note("b", 100);
        assert!(p.holds("a"));
        p.note("c", 100);
        p.note("d", 100);
        p.note("e", 100);
        assert!(!p.holds("b"), "oldest evicted");
        assert!(p.holds("a"));
        p.forget("a");
        assert!(!p.holds("a"));
        p.reset(0);
        p.note("x", 1);
        assert!(!p.holds("x"), "no peer cache");
    }
}
