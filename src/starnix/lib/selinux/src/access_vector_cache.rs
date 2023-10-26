// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    AccessQueryable, AccessVector, DenyAll, MutableAccessQueryable, ObjectClass, SecurityId,
};

use std::{
    borrow::{Borrow, BorrowMut},
    marker::PhantomData,
    sync::atomic::{AtomicU64, Ordering},
};

/// A cache of access vectors that associate source SID, target SID, and object type to a set of
/// rights permitted for the source accessing the target object.
pub trait MutableAccessVectorCache {
    /// Removes all entries from the nearest cache without notifying additional delegate caches
    /// encapsulated in this cache. Note that the "nearest cache" may be a delegate in cases where,
    /// for example, the implementation manages synchronization but delegates storage of cache
    /// entries.
    fn local_reset(&mut self);

    /// Removes all entries from this cache and any delegate caches encapsulated in this cache.
    fn reset(&mut self);
}

/// A [`MutableAccessVectorCache`] that uses interior mutability.
pub trait AccessVectorCache {
    /// Removes all entries from the nearest cache without notifying additional delegate caches
    /// encapsulated in this cache. Note that the "nearest cache" may be a delegate in cases where,
    /// for example, the implementation manages synchronization but delegates storage of cache
    /// entries.
    fn local_reset(&self);

    /// Removes all entries from this cache and any delegate caches encapsulated in this cache.
    fn reset(&self);
}

impl<AVC: AccessVectorCache> MutableAccessVectorCache for AVC {
    fn local_reset(&mut self) {
        (self as &dyn AccessVectorCache).local_reset()
    }

    fn reset(&mut self) {
        (self as &dyn AccessVectorCache).reset()
    }
}

impl AccessVectorCache for DenyAll {
    /// A no-op implementation: [`DenyAll`] has no state to reset when it is being treated as a
    /// cache to be reset.
    fn local_reset(&self) {}

    /// A no-op implementation: [`DenyAll`] has no state to reset and no delegates to notify
    /// when it is being treated as a cache to be reset.
    fn reset(&self) {}
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
pub struct EmptyAccessVectorCache<D: AccessQueryable + AccessVectorCache = DenyAll> {
    delegate: D,
}

impl<D: AccessQueryable + AccessVectorCache> EmptyAccessVectorCache<D> {
    /// Constructs an empty access vector cache that delegates to `delegate`.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn new(delegate: D) -> Self {
        Self { delegate }
    }
}

impl<D: AccessQueryable + AccessVectorCache> AccessQueryable for EmptyAccessVectorCache<D> {
    fn query(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector {
        self.delegate.query(source_sid, target_sid, target_class)
    }
}

impl<D: AccessQueryable + AccessVectorCache> AccessVectorCache for EmptyAccessVectorCache<D> {
    fn local_reset(&self) {}

    fn reset(&self) {
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
    B: Borrow<D>,
    D: AccessQueryable + AccessVectorCache = DenyAll,
    const SIZE: usize = DEFAULT_FIXED_ACCESS_VECTOR_CACHE_SIZE,
> {
    cache: [QueryAndResult; SIZE],
    next_index: usize,
    is_full: bool,
    delegate: B,
    // Type must depend on `D` to be parameterized by `D`.
    _marker: PhantomData<D>,
}

impl<B: Borrow<D>, D: AccessQueryable + AccessVectorCache, const SIZE: usize>
    FixedAccessVectorCache<B, D, SIZE>
{
    /// Constructs a fixed-size access vector cache that delegates to `delegate`.
    ///
    /// # Panics
    ///
    /// This will panic when `SIZE` is 0; i.e., for any `FixedAccessVectorCache<B, D, 0>`.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn new(delegate: B) -> Self {
        if SIZE == 0 {
            panic!("cannot instantiate fixed access vector cache of size 0");
        }
        let empty_cache_item: QueryAndResult = QueryAndResult::default();
        Self {
            cache: [empty_cache_item; SIZE],
            next_index: 0,
            is_full: false,
            delegate,
            _marker: PhantomData,
        }
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

impl<B: Borrow<D> + Send, D: AccessQueryable + AccessVectorCache, const SIZE: usize>
    MutableAccessQueryable for FixedAccessVectorCache<B, D, SIZE>
{
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

        let access_vector = self.delegate.borrow().query(source_sid, target_sid, target_class);

        self.insert(QueryAndResult { source_sid, target_sid, target_class, access_vector });

        access_vector
    }
}

impl<B: Borrow<D>, D: AccessQueryable + AccessVectorCache, const SIZE: usize>
    MutableAccessVectorCache for FixedAccessVectorCache<B, D, SIZE>
{
    fn local_reset(&mut self) {
        self.next_index = 0;
        self.is_full = false;
    }

    fn reset(&mut self) {
        self.local_reset();
        self.delegate.borrow().reset();
    }
}

#[derive(Default)]
pub struct CacheVersion {
    local_version: AtomicU64,
    global_version: AtomicU64,
}

impl CacheVersion {
    pub fn local_version(&self) -> u64 {
        self.local_version.load(Ordering::Relaxed)
    }

    pub fn global_version(&self) -> u64 {
        self.global_version.load(Ordering::Relaxed)
    }

    pub fn increment_local_version(&self) {
        self.local_version.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_global_version(&self) {
        self.global_version.fetch_add(1, Ordering::Relaxed);
    }
}

impl AccessVectorCache for CacheVersion {
    fn local_reset(&self) {
        self.increment_local_version();
    }

    fn reset(&self) {
        self.increment_global_version();
    }
}

/// An access vector cache that may be reset from any thread, but expects to always be queried
/// from the same thread. The cache does not implement any specific caching strategies, but
/// delegates *all* operations, *including* `local_reset()`.
///
/// Resets are delegated lazily during queries. `local_reset()` and `reset()` induce an internal
/// state change that results in at most one `local_reset()` or `reset()` call to the delegate on
/// the next query. This strategy allows [`ThreadLocalQueryAccessVectorCache`] to expose
/// thread-safe reset implementation over thread-hostile access vector cache implementations.
pub struct ThreadLocalQueryAccessVectorCache<
    BCV: Borrow<CacheVersion>,
    BD: BorrowMut<D>,
    D: MutableAccessQueryable + MutableAccessVectorCache = DenyAll,
> {
    delegate: BD,
    current_local_version: u64,
    current_global_version: u64,
    active_version: BCV,
    // Type must depend on `D` to be parameterized by `D`.
    _marker: PhantomData<D>,
}

impl<
        BCV: Borrow<CacheVersion>,
        BD: BorrowMut<D>,
        D: MutableAccessQueryable + MutableAccessVectorCache,
    > ThreadLocalQueryAccessVectorCache<BCV, BD, D>
{
    /// Constructs a [`ThreadLocalQueryAccessVectorCache`] that delegates to `delegate`.
    ///
    /// TODO: Eliminate `dead_code` guard.
    #[allow(dead_code)]
    pub fn new(active_version: BCV, delegate: BD) -> Self {
        Self {
            delegate,
            current_local_version: Default::default(),
            current_global_version: Default::default(),
            active_version,
            _marker: PhantomData,
        }
    }
}

impl<
        BCV: Borrow<CacheVersion> + Send,
        BD: BorrowMut<D> + Send,
        D: MutableAccessQueryable + MutableAccessVectorCache,
    > MutableAccessQueryable for ThreadLocalQueryAccessVectorCache<BCV, BD, D>
{
    fn query(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector {
        // Check global version first because `reset()` subsumes `local_reset()`.
        let global_version = self.active_version.borrow().global_version();
        if self.current_global_version != global_version {
            self.current_global_version = global_version;
            self.current_local_version = self.active_version.borrow().local_version();
            self.delegate.borrow_mut().reset();
        } else {
            // Check need for `local_reset()` only when `reset()` was not required.
            let local_version = self.active_version.borrow().local_version();
            if self.current_local_version != local_version {
                self.current_local_version = local_version;
                self.delegate.borrow_mut().local_reset();
            }
        }

        // Allow `self.delegate` to implement caching strategy and prepare response.
        self.delegate.borrow_mut().query(source_sid, target_sid, target_class)
    }
}

impl<
        BCV: Borrow<CacheVersion>,
        BD: BorrowMut<D>,
        D: MutableAccessQueryable + MutableAccessVectorCache,
    > MutableAccessVectorCache for ThreadLocalQueryAccessVectorCache<BCV, BD, D>
{
    fn local_reset(&mut self) {
        self.active_version.borrow().local_reset();
    }

    fn reset(&mut self) {
        self.active_version.borrow().reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use once_cell::sync::Lazy;
    use std::{
        collections::HashSet,
        sync::{
            atomic::{AtomicU32, AtomicUsize, Ordering},
            Arc,
        },
        thread::spawn,
    };

    #[derive(Default)]
    struct CountingAccessVectorCache<D: AccessQueryable + AccessVectorCache = DenyAll> {
        query_count: AtomicUsize,
        local_reset_count: AtomicUsize,
        reset_count: AtomicUsize,
        delegate: D,
    }

    impl<D: AccessQueryable + AccessVectorCache> CountingAccessVectorCache<D> {
        fn query_count(&self) -> usize {
            self.query_count.load(Ordering::Relaxed)
        }

        fn local_reset_count(&self) -> usize {
            self.local_reset_count.load(Ordering::Relaxed)
        }

        fn reset_count(&self) -> usize {
            self.reset_count.load(Ordering::Relaxed)
        }
    }

    impl<D: AccessQueryable + AccessVectorCache> AccessQueryable for CountingAccessVectorCache<D> {
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

    impl<D: AccessQueryable + AccessVectorCache> AccessVectorCache for CountingAccessVectorCache<D> {
        fn local_reset(&self) {
            self.local_reset_count.fetch_add(1, Ordering::Relaxed);
        }

        fn reset(&self) {
            self.reset_count.fetch_add(1, Ordering::Relaxed);
            self.delegate.reset();
        }
    }

    #[fuchsia::test]
    fn empty_access_vector_cache_default_deny_all() {
        let avc = EmptyAccessVectorCache::<DenyAll>::default();
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
    }

    #[fuchsia::test]
    fn fixed_access_vector_cache_add_entry() {
        let mut avc: FixedAccessVectorCache<_, _, 10> =
            FixedAccessVectorCache::new(CountingAccessVectorCache::<DenyAll>::default());
        assert_eq!(0, avc.delegate.query_count());
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
        assert_eq!(1, avc.delegate.query_count());
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
        assert_eq!(1, avc.delegate.query_count());
        assert_eq!(1, avc.next_index);
        assert_eq!(false, avc.is_full);
    }

    #[fuchsia::test]
    fn fixed_access_vector_cache_local_reset() {
        let mut avc: FixedAccessVectorCache<_, _, 10> =
            FixedAccessVectorCache::new(CountingAccessVectorCache::<DenyAll>::default());

        // No reset signals (local or otherwise) propagated to delegate.
        assert_eq!(0, avc.delegate.reset_count());
        assert_eq!(0, avc.delegate.local_reset_count());
        avc.local_reset();
        assert_eq!(0, avc.delegate.reset_count());
        assert_eq!(0, avc.delegate.local_reset_count());
    }

    #[fuchsia::test]
    fn fixed_access_vector_cache_reset() {
        let mut avc: FixedAccessVectorCache<_, _, 10> =
            FixedAccessVectorCache::new(CountingAccessVectorCache::<DenyAll>::default());

        assert_eq!(0, avc.delegate.reset_count());
        avc.reset();
        assert_eq!(1, avc.delegate.reset_count());
        assert_eq!(0, avc.next_index);
        assert_eq!(false, avc.is_full);

        assert_eq!(0, avc.delegate.query_count());
        assert_eq!(AccessVector::NONE, avc.query(0.into(), 0.into(), ObjectClass::Process));
        assert_eq!(1, avc.delegate.query_count());
        assert_eq!(1, avc.next_index);
        assert_eq!(false, avc.is_full);

        assert_eq!(1, avc.delegate.reset_count());
        avc.reset();
        assert_eq!(2, avc.delegate.reset_count());
        assert_eq!(0, avc.next_index);
        assert_eq!(false, avc.is_full);
    }

    #[fuchsia::test]
    fn fixed_access_vector_cache_fill() {
        let mut avc: FixedAccessVectorCache<_, _, 10> =
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
        let mut avc: FixedAccessVectorCache<_, _, 10> =
            FixedAccessVectorCache::new(CountingAccessVectorCache::<DenyAll>::default());

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
        static CACHE_VERSION: Lazy<CacheVersion> = Lazy::new(|| Default::default());
        let mut avc = ThreadLocalQueryAccessVectorCache::new(
            &*CACHE_VERSION,
            CountingAccessVectorCache::<DenyAll>::default(),
        );

        // Reset deferred to next query.
        assert_eq!(0, avc.delegate.reset_count());
        avc.reset();
        assert_eq!(0, avc.delegate.reset_count());
        avc.query(0.into(), 0.into(), ObjectClass::Process);
        assert_eq!(1, avc.delegate.reset_count());

        // Local reset deferred to next query.
        assert_eq!(0, avc.delegate.local_reset_count());
        avc.local_reset();
        assert_eq!(0, avc.delegate.local_reset_count());
        avc.query(0.into(), 0.into(), ObjectClass::Process);
        assert_eq!(1, avc.delegate.local_reset_count());

        // Reset + local reset = reset deferred to next query.
        assert_eq!(1, avc.delegate.reset_count());
        assert_eq!(1, avc.delegate.local_reset_count());
        avc.reset();
        avc.local_reset();
        assert_eq!(1, avc.delegate.reset_count());
        assert_eq!(1, avc.delegate.local_reset_count());
        avc.query(0.into(), 0.into(), ObjectClass::Process);
        assert_eq!(2, avc.delegate.reset_count());
        assert_eq!(1, avc.delegate.local_reset_count());
    }

    #[fuchsia::test]
    async fn thread_local_query_access_vector_cache_coherence() {
        for _ in 0..10 {
            test_thread_local_query_access_vector_cache_coherence().await
        }
    }

    /// Tests cache coherence over two policy changes over a [`ThreadLocalQueryAccessVectorCache`].
    async fn test_thread_local_query_access_vector_cache_coherence() {
        const NO_RIGHTS: u32 = 0;
        const READ_RIGHTS: u32 = 1;
        const WRITE_RIGHTS: u32 = 2;

        let active_policy: Arc<AtomicU32> = Arc::new(Default::default());

        struct PolicyServer<B: Borrow<AtomicU32>> {
            policy: B,
        }

        impl<B: Borrow<AtomicU32>> PolicyServer<B> {
            fn set_policy(&self, policy: u32) {
                if policy > 2 {
                    panic!("attempt to set policy to invalid value: {}", policy);
                }
                self.policy.borrow().store(policy, Ordering::Relaxed);
            }
        }

        impl<B: Borrow<AtomicU32> + Send + Sync> AccessQueryable for PolicyServer<B> {
            fn query(
                &self,
                _source_sid: SecurityId,
                _target_sid: SecurityId,
                _target_class: ObjectClass,
            ) -> AccessVector {
                let policy = self.policy.borrow().load(Ordering::Relaxed);
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

        impl<B: Borrow<AtomicU32>> AccessVectorCache for PolicyServer<B> {
            fn local_reset(&self) {}

            fn reset(&self) {}
        }

        let policy_server: Arc<PolicyServer<Arc<AtomicU32>>> =
            Arc::new(PolicyServer { policy: active_policy.clone() });
        let cache_version = Arc::new(CacheVersion::default());

        let fixed_avc = FixedAccessVectorCache::<
            Arc<PolicyServer<Arc<AtomicU32>>>,
            PolicyServer<Arc<AtomicU32>>,
            10,
        >::new(policy_server.clone());
        let cache_version_for_avc = cache_version.clone();
        let mut query_avc =
            ThreadLocalQueryAccessVectorCache::new(cache_version_for_avc, fixed_avc);

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
            cache_version_for_read.local_reset();
        });

        let policy_server = PolicyServer { policy: active_policy.clone() };
        let cache_version_for_write = cache_version;
        let set_write_thread = spawn(move || {
            std::thread::sleep(std::time::Duration::from_micros(2));
            policy_server.set_policy(WRITE_RIGHTS);
            cache_version_for_write.local_reset();
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
