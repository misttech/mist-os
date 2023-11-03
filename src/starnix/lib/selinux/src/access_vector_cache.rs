// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{AccessVector, ObjectClass, SecurityId};

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

/// An interface for computing the rights permitted to a source accessing a target of a particular
/// SELinux object type. This interface requires implementers to update state via interior mutability.
pub trait Query {
    /// Computes the [`AccessVector`] permitted to `source_sid` for accessing `target_sid`, an
    /// object of of type `target_class`.
    fn query(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector;
}

/// An interface for computing the rights permitted to a source accessing a target of a particular
/// SELinux object type.
pub trait QueryMut {
    /// Computes the [`AccessVector`] permitted to `source_sid` for accessing `target_sid`, an
    /// object of type `target_class`.
    fn query(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector;
}

impl<Q: Query> QueryMut for Q {
    fn query(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector {
        (self as &dyn Query).query(source_sid, target_sid, target_class)
    }
}

/// An interface for emptying caches that store [`Query`] input/output pairs. This interface
/// requires implementers to update state via interior mutability.
pub trait Reset {
    /// Removes all entries from this cache and any reset delegate caches encapsulated in this
    /// cache. Returns true only if the cache is still valid after reset.
    fn reset(&self) -> bool;
}

/// An interface for emptying caches that store [`Query`] input/output pairs.
pub trait ResetMut {
    /// Removes all entries from this cache and any reset delegate caches encapsulated in this
    /// cache. Returns true only if the cache is still valid after reset.
    fn reset(&mut self) -> bool;
}

impl<R: Reset> ResetMut for R {
    fn reset(&mut self) -> bool {
        (self as &dyn Reset).reset()
    }
}

/// A default implementation for [`AccessQueryable`] that permits no [`AccessVector`].
#[derive(Default)]
pub struct DenyAll;

impl Query for DenyAll {
    fn query(
        &self,
        _source_sid: SecurityId,
        _target_sid: SecurityId,
        _target_class: ObjectClass,
    ) -> AccessVector {
        AccessVector::NONE
    }
}

impl Reset for DenyAll {
    /// A no-op implementation: [`DenyAll`] has no state to reset and no delegates to notify
    /// when it is being treated as a cache to be reset.
    fn reset(&self) -> bool {
        true
    }
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
pub struct Empty<D = DenyAll> {
    delegate: D,
}

impl<D> Empty<D> {
    /// Constructs an empty access vector cache that delegates to `delegate`.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn new(delegate: D) -> Self {
        Self { delegate }
    }
}

impl<D: QueryMut> QueryMut for Empty<D> {
    fn query(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector {
        self.delegate.query(source_sid, target_sid, target_class)
    }
}

impl<D: ResetMut> ResetMut for Empty<D> {
    fn reset(&mut self) -> bool {
        self.delegate.reset()
    }
}

/// Default size of a fixed-sized (pre-allocated) access vector cache.
const DEFAULT_FIXED_SIZE: usize = 10;

/// An access vector cache of fixed size and memory allocation. The underlying caching strategy is
/// FIFO. Entries are evicted one at a time when entries are added to a full cache.
///
/// This implementation is thread-hostile; it expects all operations to be executed on the same
/// thread.
pub struct Fixed<D = DenyAll, const SIZE: usize = DEFAULT_FIXED_SIZE> {
    cache: [QueryAndResult; SIZE],
    next_index: usize,
    is_full: bool,
    delegate: D,
}

impl<D, const SIZE: usize> Fixed<D, SIZE> {
    /// Constructs a fixed-size access vector cache that delegates to `delegate`.
    ///
    /// # Panics
    ///
    /// This will panic when `SIZE` is 0; i.e., for any `Fixed<D, 0>`.
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

    /// Returns a boolean indicating whether the local cache is empty.
    #[inline]
    fn is_empty(&self) -> bool {
        self.next_index == 0 && !self.is_full
    }

    /// Inserts `query_and_result` into the cache.
    #[inline]
    fn insert(&mut self, query_and_result: QueryAndResult) {
        self.cache[self.next_index] = query_and_result;
        self.next_index = (self.next_index + 1) % SIZE;
        if self.next_index == 0 {
            self.is_full = true;
        }
    }
}

impl<D: QueryMut, const SIZE: usize> QueryMut for Fixed<D, SIZE> {
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

impl<D, const SIZE: usize> ResetMut for Fixed<D, SIZE> {
    fn reset(&mut self) -> bool {
        self.next_index = 0;
        self.is_full = false;
        true
    }
}

/// A wrapper around an atomic integer that implements [`Reset`]. Instances of this type are used as
/// a version number to indicate when a cache needs to be emptied.
#[derive(Default)]
pub struct AtomicVersion(AtomicU64);

impl AtomicVersion {
    /// Atomically load the version number.
    pub fn version(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Atomically increment the version number.
    pub fn increment_version(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

impl Reset for AtomicVersion {
    fn reset(&self) -> bool {
        self.increment_version();
        true
    }
}

impl<Q: Query> Query for Arc<Q> {
    fn query(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector {
        self.as_ref().query(source_sid, target_sid, target_class)
    }
}

impl<R: Reset> Reset for Arc<R> {
    fn reset(&self) -> bool {
        self.as_ref().reset()
    }
}

/// An access vector cache that may be reset from any thread, but expects to always be queried
/// from the same thread. The cache does not implement any specific caching strategies, but
/// delegates *all* operations.
///
/// Resets are delegated lazily during queries.  A `reset()` induces an internal state change that
/// results in at most one `reset()` call to the query delegate on the next query. This strategy
/// allows [`ThreadLocalQuery`] to expose thread-safe reset implementation over thread-hostile
/// access vector cache implementations.
pub struct ThreadLocalQuery<D = DenyAll> {
    delegate: D,
    current_version: u64,
    active_version: Arc<AtomicVersion>,
}

impl<D> ThreadLocalQuery<D> {
    /// Constructs a [`ThreadLocalQuery`] that delegates to `delegate`.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn new(active_version: Arc<AtomicVersion>, delegate: D) -> Self {
        Self { delegate, current_version: Default::default(), active_version }
    }
}

impl<D: QueryMut + ResetMut> QueryMut for ThreadLocalQuery<D> {
    fn query(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector {
        let version = self.active_version.as_ref().version();
        if self.current_version != version {
            self.current_version = version;
            self.delegate.reset();
        }

        // Allow `self.delegate` to implement caching strategy and prepare response.
        self.delegate.query(source_sid, target_sid, target_class)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        collections::HashSet,
        sync::{
            atomic::{AtomicU32, AtomicUsize, Ordering},
            Arc,
        },
        thread::spawn,
    };

    #[derive(Default)]
    struct Counter<D = DenyAll> {
        query_count: AtomicUsize,
        reset_count: AtomicUsize,
        delegate: D,
    }

    impl<D> Counter<D> {
        fn query_count(&self) -> usize {
            self.query_count.load(Ordering::Relaxed)
        }

        fn reset_count(&self) -> usize {
            self.reset_count.load(Ordering::Relaxed)
        }
    }

    impl<D: Query> Query for Counter<D> {
        fn query(
            &self,
            source_sid: SecurityId,
            target_sid: SecurityId,
            target_class: ObjectClass,
        ) -> AccessVector {
            self.query_count.fetch_add(1, Ordering::Relaxed);
            self.delegate.query(source_sid, target_sid, target_class)
        }
    }

    impl<D: Reset> Reset for Counter<D> {
        fn reset(&self) -> bool {
            self.reset_count.fetch_add(1, Ordering::Relaxed);
            self.delegate.reset();
            true
        }
    }

    #[fuchsia::test]
    fn empty_access_vector_cache_default_deny_all() {
        let mut avc = Empty::<DenyAll>::default();
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
    }

    #[fuchsia::test]
    fn fixed_access_vector_cache_add_entry() {
        let mut avc = Fixed::<_, 10>::new(Counter::<DenyAll>::default());
        assert_eq!(0, avc.delegate.query_count());
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
        assert_eq!(1, avc.delegate.query_count());
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
        assert_eq!(1, avc.delegate.query_count());
        assert_eq!(1, avc.next_index);
        assert_eq!(false, avc.is_full);
    }

    #[fuchsia::test]
    fn fixed_access_vector_cache_reset() {
        let mut avc = Fixed::<_, 10>::new(Counter::<DenyAll>::default());

        avc.reset();
        assert_eq!(0, avc.next_index);
        assert_eq!(false, avc.is_full);

        assert_eq!(0, avc.delegate.query_count());
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
        assert_eq!(1, avc.delegate.query_count());
        assert_eq!(1, avc.next_index);
        assert_eq!(false, avc.is_full);

        avc.reset();
        assert_eq!(0, avc.next_index);
        assert_eq!(false, avc.is_full);
    }

    #[fuchsia::test]
    fn fixed_access_vector_cache_fill() {
        let mut avc = Fixed::<_, 10>::new(Counter::<DenyAll>::default());

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
        let mut avc = Fixed::<_, 10>::new(Counter::<DenyAll>::default());

        // Fill with (i, 0, 0 => 0), then overwrite (0, 0, 0 => 0) with (10, 0, 0 => 0).
        for i in 0..11 {
            avc.query(i.into(), 0.into(), ObjectClass::Process);
        }
        assert_eq!(1, avc.next_index);
        assert_eq!(true, avc.is_full);

        // Query (0, 0, 0) should miss, then overwrite (1, 0, 0 => 0) with (0, 0, 0 => 0).
        let delegate_query_count = avc.delegate.query_count();
        avc.query(0.into(), 0.into(), ObjectClass::Process);
        assert_eq!(delegate_query_count + 1, avc.delegate.query_count());

        // Query (2, 0, 0) should still hit.
        let delegate_query_count = avc.delegate.query_count();
        avc.query(2.into(), 0.into(), ObjectClass::Process);
        assert_eq!(delegate_query_count, avc.delegate.query_count());

        // Cache is not LRU: Querying (0, 0, 0), then (i + 100, 0, 0) repeatedly will evict
        // (0, 0, 0 => ...) from the cache after filling the cache with (i + 100, 0, 0 => ...)
        // entries.
        for i in 0..10 {
            avc.query(0.into(), 0.into(), ObjectClass::Process);
            avc.query((i + 100).into(), 0.into(), ObjectClass::Process);
        }
        // Query (0, 0, 0) should now miss.
        let delegate_query_count = avc.delegate.query_count();
        avc.query(0.into(), 0.into(), ObjectClass::Process);
        assert_eq!(delegate_query_count + 1, avc.delegate.query_count());
    }

    #[fuchsia::test]
    fn thread_local_query_access_vector_cache_reset() {
        let cache_version = Arc::new(AtomicVersion::default());
        let mut avc = ThreadLocalQuery::new(cache_version.clone(), Counter::<DenyAll>::default());

        // Reset deferred to next query.
        assert_eq!(0, avc.delegate.reset_count());
        cache_version.reset();
        assert_eq!(0, avc.delegate.reset_count());
        avc.query(0.into(), 0.into(), ObjectClass::Process);
        assert_eq!(1, avc.delegate.reset_count());
    }

    #[fuchsia::test]
    async fn thread_local_query_access_vector_cache_coherence() {
        for _ in 0..10 {
            test_thread_local_query_access_vector_cache_coherence().await
        }
    }

    /// Tests cache coherence over two policy changes over a [`ThreadLocalQuery`].
    async fn test_thread_local_query_access_vector_cache_coherence() {
        const NO_RIGHTS: u32 = 0;
        const READ_RIGHTS: u32 = 1;
        const WRITE_RIGHTS: u32 = 2;

        let active_policy: Arc<AtomicU32> = Arc::new(Default::default());

        struct PolicyServer {
            policy: Arc<AtomicU32>,
        }

        impl PolicyServer {
            fn set_policy(&self, policy: u32) {
                if policy > 2 {
                    panic!("attempt to set policy to invalid value: {}", policy);
                }
                self.policy.as_ref().store(policy, Ordering::Relaxed);
            }
        }

        impl Query for PolicyServer {
            fn query(
                &self,
                _source_sid: SecurityId,
                _target_sid: SecurityId,
                _target_class: ObjectClass,
            ) -> AccessVector {
                let policy = self.policy.as_ref().load(Ordering::Relaxed);
                if policy == NO_RIGHTS {
                    AccessVector::NONE
                } else if policy == READ_RIGHTS {
                    AccessVector::READ
                } else if policy == WRITE_RIGHTS {
                    AccessVector::WRITE
                } else {
                    panic!("query found invalid poilcy: {}", policy);
                }
            }
        }

        impl Reset for PolicyServer {
            fn reset(&self) -> bool {
                true
            }
        }

        let policy_server: Arc<PolicyServer> =
            Arc::new(PolicyServer { policy: active_policy.clone() });
        let cache_version = Arc::new(AtomicVersion::default());

        let fixed_avc = Fixed::<_, 10>::new(policy_server.clone());
        let cache_version_for_avc = cache_version.clone();
        let mut query_avc = ThreadLocalQuery::new(cache_version_for_avc, fixed_avc);

        policy_server.set_policy(NO_RIGHTS);
        let (tx, rx) = futures::channel::oneshot::channel();
        let query_thread = spawn(move || {
            let mut trace = vec![];

            for _ in 0..2000 {
                trace.push(query_avc.query(0.into(), 0.into(), ObjectClass::Process))
            }

            tx.send(trace).expect("send trace");
        });

        let policy_server = PolicyServer { policy: active_policy.clone() };
        let cache_version_for_read = cache_version.clone();
        let set_read_thread = spawn(move || {
            std::thread::sleep(std::time::Duration::from_micros(1));
            policy_server.set_policy(READ_RIGHTS);
            cache_version_for_read.reset();
        });

        let policy_server = PolicyServer { policy: active_policy.clone() };
        let cache_version_for_write = cache_version;
        let set_write_thread = spawn(move || {
            std::thread::sleep(std::time::Duration::from_micros(2));
            policy_server.set_policy(WRITE_RIGHTS);
            cache_version_for_write.reset();
        });

        set_read_thread.join().expect("join set-policy-to-read");
        set_write_thread.join().expect("join set-policy-to-write");
        query_thread.join().expect("join query");
        let trace = rx.await.expect("receive trace");
        let mut observed_rights: HashSet<AccessVector> = Default::default();
        let mut prev_rights = AccessVector::NONE;
        for (i, rights) in trace.into_iter().enumerate() {
            if i != 0 && rights != prev_rights {
                // Return-to-previous-rights => cache incoherence!
                assert!(!observed_rights.contains(&rights));
                observed_rights.insert(rights);
            }

            prev_rights = rights;
        }
    }
}
