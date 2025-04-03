// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use indexmap::IndexMap;
use std::hash::Hash;
use std::ops::Add;

#[cfg(test)]
use indexmap::map::Iter;

/// Describes the performance statistics of a cache implementation.
#[derive(Default, Debug, Clone, PartialEq)]
pub struct CacheStats {
    /// Cumulative count of lookups performed on the cache.
    pub lookups: u64,
    /// Cumulative count of lookups that returned data from an existing cache entry.
    pub hits: u64,
    /// Cumulative count of lookups that did not match any existing cache entry.
    pub misses: u64,
    /// Cumulative count of insertions into the cache.
    pub allocs: u64,
    /// Cumulative count of evictions from the cache, to make space for a new insertion.
    pub reclaims: u64,
    /// Cumulative count of evictions from the cache due to no longer being deemed relevant.
    /// This is not used in our current implementation.
    pub frees: u64,
}

impl Add for &CacheStats {
    type Output = CacheStats;

    fn add(self, other: &CacheStats) -> CacheStats {
        CacheStats {
            lookups: self.lookups + other.lookups,
            hits: self.hits + other.hits,
            misses: self.misses + other.misses,
            allocs: self.allocs + other.allocs,
            reclaims: self.reclaims + other.reclaims,
            frees: self.frees + other.frees,
        }
    }
}

/// An interface through which statistics may be obtained from each cache.
pub trait HasCacheStats {
    /// Returns statistics for the cache implementation.
    fn cache_stats(&self) -> CacheStats;
}

/// Associative FIFO cache with capacity defined at creation.
///
/// Lookups in the cache are O(1), as are evictions.
///
/// This implementation is thread-hostile; it expects all operations to be executed on the same
/// thread.
pub(super) struct FifoCache<A: Hash + Eq, R> {
    cache: IndexMap<A, R>,
    capacity: usize,
    oldest_index: usize,
    stats: CacheStats,
}

impl<A: Hash + Eq, R> FifoCache<A, R> {
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "cannot instantiate fixed access vector cache of size 0");

        Self {
            // Request `capacity` plus one element working-space for insertions that trigger
            // an eviction.
            cache: IndexMap::with_capacity(capacity + 1),
            capacity,
            oldest_index: 0,
            stats: CacheStats::default(),
        }
    }

    /// Returns true if the cache has reached capacity.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.cache.len() == self.capacity
    }

    /// Returns the capacity with which this instance was initialized.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Searches the cache and returns the index of a [`QueryAndResult`] matching
    /// the given `source_sid`, `target_sid`, and `target_class` (or returns `None`).
    #[inline]
    pub fn get(&mut self, query_args: &A) -> Option<&mut R> {
        self.stats.lookups += 1;

        let result = self.cache.get_mut(query_args);

        if result.is_some() {
            self.stats.hits += 1;
        } else {
            self.stats.misses += 1;
        }

        result
    }

    /// Inserts the specified `query` and `result` into the cache, evicting the oldest existing
    /// entry if capacity has been reached.
    #[inline]
    pub fn insert(&mut self, query: A, result: R) -> &mut R {
        self.stats.allocs += 1;

        // If the cache is already full then it will be necessary to evict an existing entry.
        // Eviction is performed after inserting the new entry, to allow the eviction operation to
        // be implemented via swap-into-place.
        let must_evict = self.is_full();

        // Insert the entry, at the end of the `IndexMap` queue, then evict the oldest element.
        let (mut index, _) = self.cache.insert_full(query, result);
        if must_evict {
            // The final element in the ordered container is the newly-added entry, so we can simply
            // swap it with the oldest element, and then remove the final element, to achieve FIFO
            // eviction.
            assert_eq!(index, self.capacity);

            self.cache.swap_remove_index(self.oldest_index);
            self.stats.reclaims += 1;

            index = self.oldest_index;

            self.oldest_index += 1;
            if self.oldest_index == self.capacity {
                self.oldest_index = 0;
            }
        }

        self.cache.get_index_mut(index).map(|(_, v)| v).expect("invalid index after insert!")
    }

    /// Returns an iterator over the cache elements, for use by tests.
    #[cfg(test)]
    #[allow(dead_code)] // Only used by `access_vector_cache.rs` tests when built for Fuchsia.
    pub fn iter(&self) -> Iter<'_, A, R> {
        self.cache.iter()
    }
}

impl<A: Hash + Eq, R> HasCacheStats for FifoCache<A, R> {
    fn cache_stats(&self) -> CacheStats {
        self.stats.clone()
    }
}
