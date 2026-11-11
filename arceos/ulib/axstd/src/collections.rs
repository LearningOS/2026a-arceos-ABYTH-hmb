//! `std::collections`-like collections for `axstd`.
//!
//! ArceOS is a `no_std` environment, so the real `std::collections::HashMap`
//! is unavailable. This module provides a `HashMap` with a `std`-compatible
//! subset of the API, backed by a `BTreeMap` of buckets keyed by an FNV-1a
//! hash of the key.

use alloc::vec::Vec;
use core::hash::{BuildHasher, Hash, Hasher};
use core::marker::PhantomData;
use core::mem;

pub use alloc::collections::{BTreeMap, BTreeSet, BinaryHeap, LinkedList, VecDeque};

/// An FNV-1a hasher.
#[derive(Default)]
pub struct FnvHasher(u64);

impl FnvHasher {
    /// Creates a new hasher initialized with the FNV-1a offset basis.
    pub const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for FnvHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// The default hash builder of [`HashMap`].
#[derive(Clone, Copy, Default)]
pub struct DefaultHashBuilder;

impl BuildHasher for DefaultHashBuilder {
    type Hasher = FnvHasher;

    fn build_hasher(&self) -> FnvHasher {
        FnvHasher::new()
    }
}

fn hash_with<K: Hash>(k: &K) -> u64 {
    let mut hasher = FnvHasher::new();
    k.hash(&mut hasher);
    hasher.finish()
}

/// A hash map implemented with open bucketing over a `BTreeMap`.
///
/// This is a `no_std` replacement for `std::collections::HashMap` with a
/// compatible subset of its API.
///
/// The hash map must be keyed by types that implement [`Hash`] and [`Eq`].
pub struct HashMap<K, V, S = DefaultHashBuilder> {
    buckets: BTreeMap<u64, Vec<(K, V)>>,
    len: usize,
    _hasher: PhantomData<S>,
}

impl<K, V> HashMap<K, V, DefaultHashBuilder> {
    /// Creates an empty `HashMap`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty `HashMap` with the specified capacity.
    ///
    /// The capacity is only a hint; the map grows as needed.
    pub fn with_capacity(capacity: usize) -> Self {
        let _ = capacity;
        Self::default()
    }
}

impl<K, V, S> Default for HashMap<K, V, S> {
    fn default() -> Self {
        Self {
            buckets: BTreeMap::new(),
            len: 0,
            _hasher: PhantomData,
        }
    }
}

impl<K: Eq + Hash, V, S> HashMap<K, V, S> {
    /// Returns the number of elements in the map.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the map contains no elements.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Inserts a key-value pair, returning the old value if the key existed.
    pub fn insert(&mut self, k: K, v: V) -> Option<V> {
        let h = hash_with(&k);
        let bucket = self.buckets.entry(h).or_default();
        for (ek, ev) in bucket.iter_mut() {
            if *ek == k {
                return Some(mem::replace(ev, v));
            }
        }
        bucket.push((k, v));
        self.len += 1;
        None
    }

    /// Returns a reference to the value of the given key, if present.
    pub fn get(&self, k: &K) -> Option<&V> {
        let h = hash_with(k);
        self.buckets.get(&h).and_then(|bucket| {
            bucket.iter().find(|(ek, _)| ek == k).map(|(_, v)| v)
        })
    }

    /// Returns a mutable reference to the value of the given key, if present.
    pub fn get_mut(&mut self, k: &K) -> Option<&mut V> {
        let h = hash_with(k);
        self.buckets.get_mut(&h).and_then(|bucket| {
            bucket.iter_mut().find(|(ek, _)| ek == k).map(|(_, v)| v)
        })
    }

    /// Returns `true` if the map contains the given key.
    pub fn contains_key(&self, k: &K) -> bool {
        self.get(k).is_some()
    }

    /// Removes a key from the map, returning its value if it existed.
    pub fn remove(&mut self, k: &K) -> Option<V> {
        let h = hash_with(k);
        let bucket = self.buckets.get_mut(&h)?;
        for (i, (ek, _)) in bucket.iter().enumerate() {
            if *ek == *k {
                self.len -= 1;
                let (_, v) = bucket.remove(i);
                if bucket.is_empty() {
                    self.buckets.remove(&h);
                }
                return Some(v);
            }
        }
        None
    }

    /// Clears the map, removing all key-value pairs.
    pub fn clear(&mut self) {
        self.buckets.clear();
        self.len = 0;
    }

    /// An iterator visiting all key-value pairs in arbitrary order.
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            outer: self.buckets.iter(),
            inner: None,
        }
    }

    /// An iterator visiting all keys in arbitrary order.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(k, _)| k)
    }

    /// An iterator visiting all values in arbitrary order.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, v)| v)
    }
}

/// An iterator over the entries of a [`HashMap`].
pub struct Iter<'a, K, V> {
    outer: alloc::collections::btree_map::Iter<'a, u64, Vec<(K, V)>>,
    inner: Option<core::slice::Iter<'a, (K, V)>>,
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(inner) = &mut self.inner {
                if let Some((k, v)) = inner.next() {
                    return Some((k, v));
                }
            }
            let (_, bucket) = self.outer.next()?;
            self.inner = Some(bucket.iter());
        }
    }
}
