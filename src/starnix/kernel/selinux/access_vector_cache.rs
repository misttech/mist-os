// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{AccessQueryable, AccessVector, DenyAll, ObjectClass, SecurityId};

/// A cache of access vectors that associate source SID, target SID, and object type to a set of
/// rights permitted for the source accessing the target object.
pub trait AccessVectorCache: AccessQueryable {
    /// Removes all entries from the nearest cache without notifying additional delegate caches
    /// encapsulated in this cache. Note that the "nearest cache" may be a delegate in cases where,
    /// for example, the implementation manages synchronization but delegates storage of cache
    /// entries.
    fn local_reset(&mut self);

    /// Removes all entries from this cache and any delegate caches encapsulated in this cache.
    fn reset(&mut self);
}

impl AccessVectorCache for DenyAll {
    /// A no-op implementation: [`DenyAll`] has no state to reset when it is being treated as a
    /// cache to be reset.
    fn local_reset(&mut self) {}

    /// A no-op implementation: [`DenyAll`] has no state to reset and no delegates to notify
    /// when it is being treated as a cache to be reset.
    fn reset(&mut self) {}
}

#[derive(Clone, Copy, Default)]
struct QueryAndResult {
    source_sid: SecurityId,
    target_sid: SecurityId,
    target_class: ObjectClass,
    access_vector: AccessVector,
}

/// An empty access vector cache that delegates to an [`AccessQueryable`].
#[derive(Default)]
pub struct EmptyAccessVectorCache<D: AccessVectorCache = DenyAll> {
    delegate: D,
}

impl<D: AccessVectorCache> EmptyAccessVectorCache<D> {
    /// Constructs an empty access vector cache that delegates to `delegate`.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn new(delegate: D) -> Self {
        Self { delegate }
    }
}

impl<D: AccessVectorCache> AccessQueryable for EmptyAccessVectorCache<D> {
    fn query(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector {
        self.delegate.query(source_sid, target_sid, target_class)
    }
}

impl<D: AccessVectorCache> AccessVectorCache for EmptyAccessVectorCache<D> {
    fn local_reset(&mut self) {}

    fn reset(&mut self) {
        self.delegate.reset()
    }
}

/// Default size of a fixed-sized (pre-allocated) access vector cache.
const DEFAULT_FIXED_ACCESS_VECTOR_CACHE_SIZE: usize = 10;

/// An access vector cache of fixed size and memory allocation. The underlying caching strategy is
/// FIFO. Entries are evicted one at a time when entries are added to a full cache.
///
/// This implementation is thread-hostile; it expects all operations to be executed on the same
/// thread.
pub struct FixedAccessVectorCache<
    D: AccessVectorCache = DenyAll,
    const SIZE: usize = DEFAULT_FIXED_ACCESS_VECTOR_CACHE_SIZE,
> {
    cache: [QueryAndResult; SIZE],
    next_index: usize,
    is_full: bool,
    delegate: D,
}

impl<D: AccessVectorCache, const SIZE: usize> FixedAccessVectorCache<D, SIZE> {
    /// Constructs a fixed-size access vector cache that delegates to `delegate`.
    ///
    /// # Panics
    ///
    /// This will panic when `SIZE` is 0; i.e., for any `FixedAccessVectorCache<D, 0>`.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn new(delegate: D) -> Self {
        if SIZE == 0 {
            panic!("cannot instantiate fixed access vector cache of size 0");
        }
        let empty_cache_item: QueryAndResult = QueryAndResult::default();
        Self { cache: [empty_cache_item; SIZE], next_index: 0, is_full: false, delegate }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.next_index == 0 && !self.is_full
    }

    #[inline]
    fn insert(&mut self, query_and_result: QueryAndResult) {
        self.cache[self.next_index] = query_and_result;
        self.next_index = (self.next_index + 1) % SIZE;
        if self.next_index == 0 {
            self.is_full = true;
        }
    }
}

impl<D: AccessVectorCache, const SIZE: usize> AccessQueryable for FixedAccessVectorCache<D, SIZE> {
    fn query(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector {
        if !self.is_empty() {
            let mut index = if self.next_index == 0 { SIZE - 1 } else { self.next_index - 1 };
            loop {
                let query_and_result = &self.cache[index];
                if &source_sid == &query_and_result.source_sid
                    && &target_sid == &query_and_result.target_sid
                    && &target_class == &query_and_result.target_class
                {
                    return query_and_result.access_vector;
                }

                if index == self.next_index || (index == 0 && !self.is_full) {
                    break;
                }

                index = if index == 0 { SIZE - 1 } else { index - 1 };
            }
        }

        let access_vector = self.delegate.query(source_sid, target_sid, target_class);

        self.insert(QueryAndResult { source_sid, target_sid, target_class, access_vector });

        access_vector
    }
}

impl<D: AccessVectorCache, const SIZE: usize> AccessVectorCache
    for FixedAccessVectorCache<D, SIZE>
{
    fn local_reset(&mut self) {
        self.next_index = 0;
        self.is_full = false;
    }

    fn reset(&mut self) {
        self.local_reset();
        self.delegate.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct CountingAccessVectorCache<D: AccessVectorCache = DenyAll> {
        query_count: usize,
        local_reset_count: usize,
        reset_count: usize,
        delegate: D,
    }

    impl<D: AccessVectorCache> AccessQueryable for CountingAccessVectorCache<D> {
        fn query(
            &mut self,
            source_sid: SecurityId,
            target_sid: SecurityId,
            target_class: ObjectClass,
        ) -> AccessVector {
            self.query_count = self.query_count + 1;
            self.delegate.query(source_sid, target_sid, target_class)
        }
    }

    impl<D: AccessVectorCache> AccessVectorCache for CountingAccessVectorCache<D> {
        fn local_reset(&mut self) {
            self.local_reset_count = self.local_reset_count + 1;
        }

        fn reset(&mut self) {
            self.reset_count = self.reset_count + 1;
            self.delegate.reset();
        }
    }

    #[fuchsia::test]
    fn empty_access_vector_cache_default_deny_all() {
        let mut avc = EmptyAccessVectorCache::<DenyAll>::default();
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
    }

    #[fuchsia::test]
    fn fixed_access_vector_cache_add_entry() {
        let mut avc: FixedAccessVectorCache<_, 10> =
            FixedAccessVectorCache::new(CountingAccessVectorCache::<DenyAll>::default());
        assert_eq!(0, avc.delegate.query_count);
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
        assert_eq!(1, avc.delegate.query_count);
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
        assert_eq!(1, avc.delegate.query_count);
        assert_eq!(1, avc.next_index);
        assert_eq!(false, avc.is_full);
    }

    #[fuchsia::test]
    fn fixed_access_vector_cache_local_reset() {
        let mut avc: FixedAccessVectorCache<_, 10> =
            FixedAccessVectorCache::new(CountingAccessVectorCache::<DenyAll>::default());

        // No reset signals (local or otherwise) propagated to delegate.
        assert_eq!(0, avc.delegate.reset_count);
        assert_eq!(0, avc.delegate.local_reset_count);
        avc.local_reset();
        assert_eq!(0, avc.delegate.reset_count);
        assert_eq!(0, avc.delegate.local_reset_count);
    }

    #[fuchsia::test]
    fn fixed_access_vector_cache_reset() {
        let mut avc: FixedAccessVectorCache<_, 10> =
            FixedAccessVectorCache::new(CountingAccessVectorCache::<DenyAll>::default());

        assert_eq!(0, avc.delegate.reset_count);
        avc.reset();
        assert_eq!(1, avc.delegate.reset_count);
        assert_eq!(0, avc.next_index);
        assert_eq!(false, avc.is_full);

        assert_eq!(0, avc.delegate.query_count);
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
        assert_eq!(1, avc.delegate.query_count);
        assert_eq!(1, avc.next_index);
        assert_eq!(false, avc.is_full);

        assert_eq!(1, avc.delegate.reset_count);
        avc.reset();
        assert_eq!(2, avc.delegate.reset_count);
        assert_eq!(0, avc.next_index);
        assert_eq!(false, avc.is_full);
    }

    #[fuchsia::test]
    fn fixed_access_vector_cache_fill() {
        let mut avc: FixedAccessVectorCache<_, 10> =
            FixedAccessVectorCache::new(CountingAccessVectorCache::<DenyAll>::default());

        for i in 0..10 {
            avc.query(i.into(), 0.into(), ObjectClass::Process);
        }
        assert_eq!(0, avc.next_index);
        assert_eq!(true, avc.is_full);

        avc.reset();
        assert_eq!(0, avc.next_index);
        assert_eq!(false, avc.is_full);

        for i in 0..10 {
            avc.query(0.into(), i.into(), ObjectClass::Process);
        }
        assert_eq!(0, avc.next_index);
        assert_eq!(true, avc.is_full);

        avc.reset();
        assert_eq!(0, avc.next_index);
        assert_eq!(false, avc.is_full);
    }

    #[fuchsia::test]
    fn fixed_access_vector_cache_full_miss() {
        let mut avc: FixedAccessVectorCache<_, 10> =
            FixedAccessVectorCache::new(CountingAccessVectorCache::<DenyAll>::default());

        // Fill with (i, 0, 0 => 0), then overwrite (0, 0, 0 => 0) with (10, 0, 0 => 0).
        for i in 0..11 {
            avc.query(i.into(), 0.into(), ObjectClass::Process);
        }
        assert_eq!(1, avc.next_index);
        assert_eq!(true, avc.is_full);

        // Query (0, 0, 0) should miss, then overwrite (1, 0, 0 => 0) with (0, 0, 0 => 0).
        let delegate_query_count = avc.delegate.query_count;
        avc.query(0.into(), 0.into(), ObjectClass::Process);
        assert_eq!(delegate_query_count + 1, avc.delegate.query_count);

        // Query (2, 0, 0) should still hit.
        let delegate_query_count = avc.delegate.query_count;
        avc.query(2.into(), 0.into(), ObjectClass::Process);
        assert_eq!(delegate_query_count, avc.delegate.query_count);

        // Cache is not LRU: Querying (0, 0, 0), then (i + 100, 0, 0) repeatedly will evict
        // (0, 0, 0 => ...) from the cache after filling the cache with (i + 100, 0, 0 => ...)
        // entries.
        for i in 0..10 {
            avc.query(0.into(), 0.into(), ObjectClass::Process);
            avc.query((i + 100).into(), 0.into(), ObjectClass::Process);
        }
        // Query (0, 0, 0) should now miss.
        let delegate_query_count = avc.delegate.query_count;
        avc.query(0.into(), 0.into(), ObjectClass::Process);
        assert_eq!(delegate_query_count + 1, avc.delegate.query_count);
    }
}
