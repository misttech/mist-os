// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fifo_cache::FifoCache;
use crate::policy::{AccessDecision, IoctlAccessDecision};
use crate::sync::Mutex;
use crate::{FsNodeClass, KernelClass, NullessByteStr, ObjectClass, SecurityId};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

pub use crate::fifo_cache::{CacheStats, HasCacheStats};

/// Interface used internally by the `SecurityServer` implementation to implement policy queries
/// such as looking up the set of permissions to grant, or the Security Context to apply to new
/// files, etc.
///
/// This trait allows layering of caching, delegation, and thread-safety between the policy-backed
/// calculations, and the caller-facing permission-check interface.
pub(super) trait Query {
    /// Computes the [`AccessDecision`] permitted to `source_sid` for accessing `target_sid`, an
    /// object of of type `target_class`.
    fn compute_access_decision(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessDecision;

    /// Returns the security identifier (SID) with which to label a new `fs_node_class` instance
    /// created by `source_sid` in a parent directory labeled `target_sid` should be labeled,
    /// if no more specific SID was specified by `compute_new_fs_node_sid_with_name()`, based on
    /// the file's name.
    fn compute_new_fs_node_sid(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
    ) -> Result<SecurityId, anyhow::Error>;

    /// Returns the security identifier (SID) with which to label a new `fs_node_class` instance of
    /// name `fs_node_name`, created by `source_sid` in a parent directory labeled `target_sid`.
    /// If no filename-transition rules exist for the specified `fs_node_name` then `None` is
    /// returned.
    fn compute_new_fs_node_sid_with_name(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
        fs_node_name: NullessByteStr<'_>,
    ) -> Option<SecurityId>;

    /// Computes the [`IoctlAccessDecision`] permitted to `source_sid` for accessing `target_sid`,
    /// an object of of type `target_class`, for ioctls with high byte `ioctl_prefix`.
    fn compute_ioctl_access_decision(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
        ioctl_prefix: u8,
    ) -> IoctlAccessDecision;
}

/// An interface for computing the rights permitted to a source accessing a target of a particular
/// SELinux object type.
pub trait QueryMut {
    /// Computes the [`AccessDecision`] permitted to `source_sid` for accessing `target_sid`, an
    /// object of type `target_class`.
    fn compute_access_decision(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessDecision;

    /// Returns the security identifier (SID) with which to label a new `fs_node_class` instance
    /// created by `source_sid` in a parent directory labeled `target_sid` should be labeled,
    /// if no more specific SID was specified by `compute_new_fs_node_sid_with_name()`, based on
    /// the file's name.
    fn compute_new_fs_node_sid(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
    ) -> Result<SecurityId, anyhow::Error>;

    /// Returns the security identifier (SID) with which to label a new `fs_node_class` instance of
    /// name `fs_node_name`, created by `source_sid` in a parent directory labeled `target_sid`.
    /// If no filename-transition rules exist for the specified `fs_node_name` then `None` is
    /// returned.
    fn compute_new_fs_node_sid_with_name(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
        fs_node_name: NullessByteStr<'_>,
    ) -> Option<SecurityId>;

    /// Computes the [`IoctlAccessDecision`] permitted to `source_sid` for accessing `target_sid`,
    /// an object of of type `target_class`, for ioctls with high byte `ioctl_prefix`.
    fn compute_ioctl_access_decision(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
        ioctl_prefix: u8,
    ) -> IoctlAccessDecision;
}

impl<Q: Query> QueryMut for Q {
    fn compute_access_decision(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessDecision {
        (self as &dyn Query).compute_access_decision(source_sid, target_sid, target_class)
    }

    fn compute_new_fs_node_sid(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
    ) -> Result<SecurityId, anyhow::Error> {
        (self as &dyn Query).compute_new_fs_node_sid(source_sid, target_sid, fs_node_class)
    }

    fn compute_new_fs_node_sid_with_name(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
        fs_node_name: NullessByteStr<'_>,
    ) -> Option<SecurityId> {
        (self as &dyn Query).compute_new_fs_node_sid_with_name(
            source_sid,
            target_sid,
            fs_node_class,
            fs_node_name,
        )
    }

    fn compute_ioctl_access_decision(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
        ioctl_prefix: u8,
    ) -> IoctlAccessDecision {
        (self as &dyn Query).compute_ioctl_access_decision(
            source_sid,
            target_sid,
            target_class,
            ioctl_prefix,
        )
    }
}

/// An interface for emptying caches that store [`Query`] input/output pairs. This interface
/// requires implementers to update state via interior mutability.
pub(super) trait Reset {
    /// Removes all entries from this cache and any reset delegate caches encapsulated in this
    /// cache. Returns true only if the cache is still valid after reset.
    fn reset(&self) -> bool;
}

/// An interface for emptying caches that store [`Query`] input/output pairs.
pub(super) trait ResetMut {
    /// Removes all entries from this cache and any reset delegate caches encapsulated in this
    /// cache. Returns true only if the cache is still valid after reset.
    fn reset(&mut self) -> bool;
}

impl<R: Reset> ResetMut for R {
    fn reset(&mut self) -> bool {
        (self as &dyn Reset).reset()
    }
}

pub(super) trait ProxyMut<D> {
    fn set_delegate(&mut self, delegate: D) -> D;
}

/// A default implementation for [`AccessQueryable`] that permits no [`AccessVector`].
#[derive(Default)]
pub(super) struct DenyAll;

impl Query for DenyAll {
    fn compute_access_decision(
        &self,
        _source_sid: SecurityId,
        _target_sid: SecurityId,
        _target_class: ObjectClass,
    ) -> AccessDecision {
        AccessDecision::default()
    }

    fn compute_new_fs_node_sid(
        &self,
        _source_sid: SecurityId,
        _target_sid: SecurityId,
        _fs_node_class: FsNodeClass,
    ) -> Result<SecurityId, anyhow::Error> {
        unreachable!()
    }

    fn compute_new_fs_node_sid_with_name(
        &self,
        _source_sid: SecurityId,
        _target_sid: SecurityId,
        _fs_node_class: FsNodeClass,
        _fs_node_name: NullessByteStr<'_>,
    ) -> Option<SecurityId> {
        unreachable!()
    }

    fn compute_ioctl_access_decision(
        &self,
        _source_sid: SecurityId,
        _target_sid: SecurityId,
        _target_class: ObjectClass,
        _ioctl_prefix: u8,
    ) -> IoctlAccessDecision {
        IoctlAccessDecision::DENY_ALL
    }
}

impl Reset for DenyAll {
    /// A no-op implementation: [`DenyAll`] has no state to reset and no delegates to notify
    /// when it is being treated as a cache to be reset.
    fn reset(&self) -> bool {
        true
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct AccessQueryArgs {
    source_sid: SecurityId,
    target_sid: SecurityId,
    target_class: ObjectClass,
}

#[derive(Clone)]
struct AccessQueryResult {
    access_decision: AccessDecision,
    new_file_sid: Option<SecurityId>,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct IoctlAccessQueryArgs {
    source_sid: SecurityId,
    target_sid: SecurityId,
    target_class: ObjectClass,
    ioctl_prefix: u8,
}

/// An empty access vector cache that delegates to an [`AccessQueryable`].
#[derive(Default)]
struct Empty<D = DenyAll> {
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
    fn compute_access_decision(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessDecision {
        self.delegate.compute_access_decision(source_sid, target_sid, target_class)
    }

    fn compute_new_fs_node_sid(
        &mut self,
        _source_sid: SecurityId,
        _target_sid: SecurityId,
        _fs_node_class: FsNodeClass,
    ) -> Result<SecurityId, anyhow::Error> {
        unreachable!()
    }

    fn compute_new_fs_node_sid_with_name(
        &mut self,
        _source_sid: SecurityId,
        _target_sid: SecurityId,
        _fs_node_class: FsNodeClass,
        _fs_node_name: NullessByteStr<'_>,
    ) -> Option<SecurityId> {
        unreachable!()
    }

    fn compute_ioctl_access_decision(
        &mut self,
        _source_sid: SecurityId,
        _target_sid: SecurityId,
        _target_class: ObjectClass,
        _ioctl_prefix: u8,
    ) -> IoctlAccessDecision {
        todo!()
    }
}

impl<D: ResetMut> ResetMut for Empty<D> {
    fn reset(&mut self) -> bool {
        self.delegate.reset()
    }
}

/// Thread-hostile associative cache with capacity defined at construction and FIFO eviction.
pub(super) struct FifoQueryCache<D = DenyAll> {
    access_cache: FifoCache<AccessQueryArgs, AccessQueryResult>,
    ioctl_access_cache: FifoCache<IoctlAccessQueryArgs, IoctlAccessDecision>,
    delegate: D,
}

impl<D> FifoQueryCache<D> {
    // The multiplier used to compute the ioctl access cache capacity from the main cache capacity.
    const IOCTL_CAPACITY_MULTIPLIER: f32 = 0.25;

    /// Constructs a fixed-size access vector cache that delegates to `delegate`.
    ///
    /// # Panics
    ///
    /// This will panic if called with a `capacity` of zero.
    pub fn new(delegate: D, capacity: usize) -> Self {
        assert!(capacity > 0, "cannot instantiate fixed access vector cache of size 0");
        let ioctl_access_cache_capacity =
            (Self::IOCTL_CAPACITY_MULTIPLIER * (capacity as f32)) as usize;
        assert!(
            ioctl_access_cache_capacity > 0,
            "cannot instantiate ioctl cache partition of size 0"
        );

        Self {
            // Request `capacity` plus one element working-space for insertions that trigger
            // an eviction.
            access_cache: FifoCache::with_capacity(capacity),
            ioctl_access_cache: FifoCache::with_capacity(ioctl_access_cache_capacity),
            delegate,
        }
    }

    /// Returns true if the main access decision cache has reached capacity.
    #[cfg(test)]
    fn access_cache_is_full(&self) -> bool {
        self.access_cache.is_full()
    }

    /// Returns true if the ioctl access decision cache has reached capacity.
    #[cfg(test)]
    fn ioctl_access_cache_is_full(&self) -> bool {
        self.ioctl_access_cache.is_full()
    }
}

impl<D: QueryMut> QueryMut for FifoQueryCache<D> {
    fn compute_access_decision(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessDecision {
        let query_args =
            AccessQueryArgs { source_sid, target_sid, target_class: target_class.clone() };
        if let Some(result) = self.access_cache.get(&query_args) {
            return result.access_decision.clone();
        }

        let access_decision =
            self.delegate.compute_access_decision(source_sid, target_sid, target_class);

        self.access_cache.insert(
            query_args,
            AccessQueryResult { access_decision: access_decision.clone(), new_file_sid: None },
        );

        access_decision
    }

    fn compute_new_fs_node_sid(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
    ) -> Result<SecurityId, anyhow::Error> {
        let target_class = ObjectClass::System(KernelClass::from(fs_node_class));

        let query_args =
            AccessQueryArgs { source_sid, target_sid, target_class: target_class.clone() };
        let query_result = if let Some(result) = self.access_cache.get(&query_args) {
            result
        } else {
            let access_decision =
                self.delegate.compute_access_decision(source_sid, target_sid, target_class);
            self.access_cache
                .insert(query_args, AccessQueryResult { access_decision, new_file_sid: None })
        };

        if let Some(new_file_sid) = query_result.new_file_sid {
            Ok(new_file_sid)
        } else {
            let new_file_sid =
                self.delegate.compute_new_fs_node_sid(source_sid, target_sid, fs_node_class);
            if let Ok(new_file_sid) = new_file_sid {
                query_result.new_file_sid = Some(new_file_sid);
            }
            new_file_sid
        }
    }

    fn compute_new_fs_node_sid_with_name(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
        fs_node_name: NullessByteStr<'_>,
    ) -> Option<SecurityId> {
        self.delegate.compute_new_fs_node_sid_with_name(
            source_sid,
            target_sid,
            fs_node_class,
            fs_node_name,
        )
    }

    fn compute_ioctl_access_decision(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
        ioctl_prefix: u8,
    ) -> IoctlAccessDecision {
        let query_args = IoctlAccessQueryArgs {
            source_sid,
            target_sid,
            target_class: target_class.clone(),
            ioctl_prefix,
        };
        if let Some(result) = self.ioctl_access_cache.get(&query_args) {
            return result.clone();
        }

        let ioctl_access_decision = self.delegate.compute_ioctl_access_decision(
            source_sid,
            target_sid,
            target_class,
            ioctl_prefix,
        );

        self.ioctl_access_cache.insert(query_args, ioctl_access_decision.clone());

        ioctl_access_decision
    }
}

impl<D> HasCacheStats for FifoQueryCache<D> {
    fn cache_stats(&self) -> CacheStats {
        &self.access_cache.cache_stats() + &self.ioctl_access_cache.cache_stats()
    }
}

impl<D> ResetMut for FifoQueryCache<D> {
    fn reset(&mut self) -> bool {
        self.access_cache = FifoCache::with_capacity(self.access_cache.capacity());
        self.ioctl_access_cache = FifoCache::with_capacity(self.ioctl_access_cache.capacity());
        true
    }
}

impl<D> ProxyMut<D> for FifoQueryCache<D> {
    fn set_delegate(&mut self, mut delegate: D) -> D {
        std::mem::swap(&mut self.delegate, &mut delegate);
        delegate
    }
}

/// A locked access vector cache.
pub(super) struct Locked<D = DenyAll> {
    delegate: Arc<Mutex<D>>,
}

impl<D> Clone for Locked<D> {
    fn clone(&self) -> Self {
        Self { delegate: self.delegate.clone() }
    }
}

impl<D> Locked<D> {
    /// Constructs a locked access vector cache that delegates to `delegate`.
    pub fn new(delegate: D) -> Self {
        Self { delegate: Arc::new(Mutex::new(delegate)) }
    }
}

impl<D: QueryMut> Query for Locked<D> {
    fn compute_access_decision(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessDecision {
        self.delegate.lock().compute_access_decision(source_sid, target_sid, target_class)
    }

    fn compute_new_fs_node_sid(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
    ) -> Result<SecurityId, anyhow::Error> {
        self.delegate.lock().compute_new_fs_node_sid(source_sid, target_sid, fs_node_class)
    }

    fn compute_new_fs_node_sid_with_name(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
        fs_node_name: NullessByteStr<'_>,
    ) -> Option<SecurityId> {
        self.delegate.lock().compute_new_fs_node_sid_with_name(
            source_sid,
            target_sid,
            fs_node_class,
            fs_node_name,
        )
    }

    fn compute_ioctl_access_decision(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
        ioctl_prefix: u8,
    ) -> IoctlAccessDecision {
        self.delegate.lock().compute_ioctl_access_decision(
            source_sid,
            target_sid,
            target_class,
            ioctl_prefix,
        )
    }
}

impl<D: HasCacheStats> HasCacheStats for Locked<D> {
    fn cache_stats(&self) -> CacheStats {
        self.delegate.lock().cache_stats()
    }
}

impl<D: ResetMut> Reset for Locked<D> {
    fn reset(&self) -> bool {
        self.delegate.lock().reset()
    }
}

impl<D> Locked<D> {
    pub fn set_stateful_cache_delegate<PD>(&self, delegate: PD) -> PD
    where
        D: ProxyMut<PD>,
    {
        self.delegate.lock().set_delegate(delegate)
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
    fn compute_access_decision(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessDecision {
        self.as_ref().compute_access_decision(source_sid, target_sid, target_class)
    }

    fn compute_new_fs_node_sid(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
    ) -> Result<SecurityId, anyhow::Error> {
        self.as_ref().compute_new_fs_node_sid(source_sid, target_sid, fs_node_class)
    }

    fn compute_new_fs_node_sid_with_name(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
        fs_node_name: NullessByteStr<'_>,
    ) -> Option<SecurityId> {
        self.as_ref().compute_new_fs_node_sid_with_name(
            source_sid,
            target_sid,
            fs_node_class,
            fs_node_name,
        )
    }

    fn compute_ioctl_access_decision(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
        ioctl_prefix: u8,
    ) -> IoctlAccessDecision {
        self.as_ref().compute_ioctl_access_decision(
            source_sid,
            target_sid,
            target_class,
            ioctl_prefix,
        )
    }
}

impl<R: Reset> Reset for Arc<R> {
    fn reset(&self) -> bool {
        self.as_ref().reset()
    }
}

impl<Q: Query> Query for Weak<Q> {
    fn compute_access_decision(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessDecision {
        self.upgrade()
            .map(|q| q.compute_access_decision(source_sid, target_sid, target_class))
            .unwrap_or_default()
    }

    fn compute_new_fs_node_sid(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
    ) -> Result<SecurityId, anyhow::Error> {
        self.upgrade()
            .map(|q| q.compute_new_fs_node_sid(source_sid, target_sid, fs_node_class))
            .unwrap_or(Err(anyhow::anyhow!("weak reference failed to resolve")))
    }

    fn compute_new_fs_node_sid_with_name(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
        fs_node_name: NullessByteStr<'_>,
    ) -> Option<SecurityId> {
        let delegate = self.upgrade()?;
        delegate.compute_new_fs_node_sid_with_name(
            source_sid,
            target_sid,
            fs_node_class,
            fs_node_name,
        )
    }

    fn compute_ioctl_access_decision(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
        ioctl_prefix: u8,
    ) -> IoctlAccessDecision {
        self.upgrade()
            .map(|q| {
                q.compute_ioctl_access_decision(source_sid, target_sid, target_class, ioctl_prefix)
            })
            .unwrap_or(IoctlAccessDecision::DENY_ALL)
    }
}

impl<R: Reset> Reset for Weak<R> {
    fn reset(&self) -> bool {
        self.upgrade().as_deref().map(Reset::reset).unwrap_or(false)
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
pub(super) struct ThreadLocalQuery<D = DenyAll> {
    delegate: D,
    current_version: u64,
    active_version: Arc<AtomicVersion>,
}

impl<D> ThreadLocalQuery<D> {
    /// Constructs a [`ThreadLocalQuery`] that delegates to `delegate`.
    pub fn new(active_version: Arc<AtomicVersion>, delegate: D) -> Self {
        Self { delegate, current_version: Default::default(), active_version }
    }
}

impl<D: QueryMut + ResetMut> QueryMut for ThreadLocalQuery<D> {
    fn compute_access_decision(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessDecision {
        let version = self.active_version.as_ref().version();
        if self.current_version != version {
            self.current_version = version;
            self.delegate.reset();
        }

        // Allow `self.delegate` to implement caching strategy and prepare response.
        self.delegate.compute_access_decision(source_sid, target_sid, target_class)
    }

    fn compute_new_fs_node_sid(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
    ) -> Result<SecurityId, anyhow::Error> {
        let version = self.active_version.as_ref().version();
        if self.current_version != version {
            self.current_version = version;
            self.delegate.reset();
        }

        // Allow `self.delegate` to implement caching strategy and prepare response.
        self.delegate.compute_new_fs_node_sid(source_sid, target_sid, fs_node_class)
    }

    fn compute_new_fs_node_sid_with_name(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        fs_node_class: FsNodeClass,
        fs_node_name: NullessByteStr<'_>,
    ) -> Option<SecurityId> {
        // Allow `self.delegate` to implement caching strategy and prepare response.
        self.delegate.compute_new_fs_node_sid_with_name(
            source_sid,
            target_sid,
            fs_node_class,
            fs_node_name,
        )
    }

    fn compute_ioctl_access_decision(
        &mut self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
        ioctl_prefix: u8,
    ) -> IoctlAccessDecision {
        self.delegate.compute_ioctl_access_decision(
            source_sid,
            target_sid,
            target_class,
            ioctl_prefix,
        )
    }
}

/// Default size of an access vector cache shared by all threads in the system.
const DEFAULT_SHARED_SIZE: usize = 1000;

/// Default size of a thread-local access vector cache.
const DEFAULT_THREAD_LOCAL_SIZE: usize = 10;

/// Composite access vector cache manager that delegates queries to security server type, `SS`, and
/// owns a shared cache of size `DEFAULT_SHARED_SIZE`, and can produce thread-local caches of size
/// `DEFAULT_THREAD_LOCAL_SIZE`.
pub(super) struct Manager<SS> {
    shared_cache: Locked<FifoQueryCache<Weak<SS>>>,
    thread_local_version: Arc<AtomicVersion>,
}

impl<SS> Manager<SS> {
    /// Constructs a [`Manager`] that initially has no security server delegate (i.e., will default
    /// to deny all requests).
    pub fn new() -> Self {
        Self {
            shared_cache: Locked::new(FifoQueryCache::new(Weak::<SS>::new(), DEFAULT_SHARED_SIZE)),
            thread_local_version: Arc::new(AtomicVersion::default()),
        }
    }

    /// Sets the security server delegate that is consulted when there is no cache hit on a query.
    pub fn set_security_server(&self, security_server: Weak<SS>) -> Weak<SS> {
        self.shared_cache.set_stateful_cache_delegate(security_server)
    }

    /// Returns a shared reference to the shared cache managed by this manager. This operation does
    /// not copy the cache, but it does perform an atomic operation to update a reference count.
    pub fn get_shared_cache(&self) -> &Locked<FifoQueryCache<Weak<SS>>> {
        &self.shared_cache
    }

    /// Constructs a new thread-local cache that will delegate to the shared cache managed by this
    /// manager (which, in turn, delegates to its security server).
    pub fn new_thread_local_cache(
        &self,
    ) -> ThreadLocalQuery<FifoQueryCache<Locked<FifoQueryCache<Weak<SS>>>>> {
        ThreadLocalQuery::new(
            self.thread_local_version.clone(),
            FifoQueryCache::new(self.shared_cache.clone(), DEFAULT_THREAD_LOCAL_SIZE),
        )
    }
}

impl<SS> Reset for Manager<SS> {
    /// Resets caches owned by this manager. If owned caches delegate to a security server that is
    /// reloading its policy, the security server must reload its policy (and start serving the new
    /// policy) *before* invoking `Manager::reset()` on any managers that delegate to that security
    /// server. This is because the [`Manager`]-managed caches are consulted by [`Query`] clients
    /// *before* the security server; performing reload/reset in the reverse order could move stale
    /// queries into reset caches before policy reload is complete.
    fn reset(&self) -> bool {
        // Layered cache stale entries avoided only if shared cache reset first, then thread-local
        // caches are reset. This is because thread-local caches are consulted by `Query` clients
        // before the shared cache; performing reset in the reverse order could move stale queries
        // into reset caches.
        self.shared_cache.reset();
        self.thread_local_version.reset();
        true
    }
}

/// Test constants and helpers shared by `tests` and `starnix_tests`.
#[cfg(test)]
mod testing {
    use crate::SecurityId;

    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::LazyLock;

    /// SID to use where any value will do.
    pub(super) static A_TEST_SID: LazyLock<SecurityId> = LazyLock::new(unique_sid);

    /// Default fixed cache capacity to request in tests.
    pub(super) const TEST_CAPACITY: usize = 10;

    /// Returns a new `SecurityId` with unique id.
    pub(super) fn unique_sid() -> SecurityId {
        static NEXT_ID: AtomicU32 = AtomicU32::new(1000);
        SecurityId(NonZeroU32::new(NEXT_ID.fetch_add(1, Ordering::AcqRel)).unwrap())
    }

    /// Returns a vector of `count` unique `SecurityIds`.
    pub(super) fn unique_sids(count: usize) -> Vec<SecurityId> {
        (0..count).map(|_| unique_sid()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::policy::{AccessVector, XpermsBitmap};
    use crate::KernelClass;

    use std::sync::atomic::AtomicUsize;

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
        fn compute_access_decision(
            &self,
            source_sid: SecurityId,
            target_sid: SecurityId,
            target_class: ObjectClass,
        ) -> AccessDecision {
            self.query_count.fetch_add(1, Ordering::Relaxed);
            self.delegate.compute_access_decision(source_sid, target_sid, target_class)
        }

        fn compute_new_fs_node_sid(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
        ) -> Result<SecurityId, anyhow::Error> {
            unreachable!()
        }

        fn compute_new_fs_node_sid_with_name(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
            _fs_node_name: NullessByteStr<'_>,
        ) -> Option<SecurityId> {
            unreachable!()
        }

        fn compute_ioctl_access_decision(
            &self,
            source_sid: SecurityId,
            target_sid: SecurityId,
            target_class: ObjectClass,
            ioctl_prefix: u8,
        ) -> IoctlAccessDecision {
            self.query_count.fetch_add(1, Ordering::Relaxed);
            self.delegate.compute_ioctl_access_decision(
                source_sid,
                target_sid,
                target_class,
                ioctl_prefix,
            )
        }
    }

    impl<D: Reset> Reset for Counter<D> {
        fn reset(&self) -> bool {
            self.reset_count.fetch_add(1, Ordering::Relaxed);
            self.delegate.reset();
            true
        }
    }

    #[test]
    fn empty_access_vector_cache_default_deny_all() {
        let mut avc = Empty::<DenyAll>::default();
        assert_eq!(
            AccessVector::NONE,
            avc.compute_access_decision(
                A_TEST_SID.clone(),
                A_TEST_SID.clone(),
                KernelClass::Process.into()
            )
            .allow
        );
    }

    #[test]
    fn fixed_access_vector_cache_add_entry() {
        let mut avc = FifoQueryCache::<_>::new(Counter::<DenyAll>::default(), TEST_CAPACITY);
        assert_eq!(0, avc.delegate.query_count());
        assert_eq!(
            AccessVector::NONE,
            avc.compute_access_decision(
                A_TEST_SID.clone(),
                A_TEST_SID.clone(),
                KernelClass::Process.into()
            )
            .allow
        );
        assert_eq!(1, avc.delegate.query_count());
        assert_eq!(
            AccessVector::NONE,
            avc.compute_access_decision(
                A_TEST_SID.clone(),
                A_TEST_SID.clone(),
                KernelClass::Process.into()
            )
            .allow
        );
        assert_eq!(1, avc.delegate.query_count());
        assert_eq!(false, avc.access_cache_is_full());
    }

    #[test]
    fn fixed_access_vector_cache_reset() {
        let mut avc = FifoQueryCache::<_>::new(Counter::<DenyAll>::default(), TEST_CAPACITY);

        avc.reset();
        assert_eq!(false, avc.access_cache_is_full());

        assert_eq!(0, avc.delegate.query_count());
        assert_eq!(
            AccessVector::NONE,
            avc.compute_access_decision(
                A_TEST_SID.clone(),
                A_TEST_SID.clone(),
                KernelClass::Process.into()
            )
            .allow
        );
        assert_eq!(1, avc.delegate.query_count());
        assert_eq!(false, avc.access_cache_is_full());

        avc.reset();
        assert_eq!(false, avc.access_cache_is_full());
    }

    #[test]
    fn fixed_access_vector_cache_fill() {
        let mut avc = FifoQueryCache::<_>::new(Counter::<DenyAll>::default(), TEST_CAPACITY);

        for sid in unique_sids(avc.access_cache.capacity()) {
            avc.compute_access_decision(sid, A_TEST_SID.clone(), KernelClass::Process.into());
        }
        assert_eq!(true, avc.access_cache_is_full());

        avc.reset();
        assert_eq!(false, avc.access_cache_is_full());

        for sid in unique_sids(avc.access_cache.capacity()) {
            avc.compute_access_decision(A_TEST_SID.clone(), sid, KernelClass::Process.into());
        }
        assert_eq!(true, avc.access_cache_is_full());

        avc.reset();
        assert_eq!(false, avc.access_cache_is_full());
    }

    #[test]
    fn fixed_access_vector_cache_full_miss() {
        let mut avc = FifoQueryCache::<_>::new(Counter::<DenyAll>::default(), TEST_CAPACITY);

        // Make the test query, which will trivially miss.
        avc.compute_access_decision(
            A_TEST_SID.clone(),
            A_TEST_SID.clone(),
            KernelClass::Process.into(),
        );
        assert!(!avc.access_cache_is_full());

        // Fill the cache with new queries, which should evict the test query.
        for sid in unique_sids(avc.access_cache.capacity()) {
            avc.compute_access_decision(sid, A_TEST_SID.clone(), KernelClass::Process.into());
        }
        assert!(avc.access_cache_is_full());

        // Making the test query should result in another miss.
        let delegate_query_count = avc.delegate.query_count();
        avc.compute_access_decision(
            A_TEST_SID.clone(),
            A_TEST_SID.clone(),
            KernelClass::Process.into(),
        );
        assert_eq!(delegate_query_count + 1, avc.delegate.query_count());

        // Because the cache is not LRU, making `capacity()` unique queries, each preceded by
        // the test query, will still result in the test query result being evicted.
        // Each test query will hit, and the interleaved queries will miss, with the final of the
        // interleaved queries evicting the test query.
        for sid in unique_sids(avc.access_cache.capacity()) {
            avc.compute_access_decision(
                A_TEST_SID.clone(),
                A_TEST_SID.clone(),
                KernelClass::Process.into(),
            );
            avc.compute_access_decision(sid, A_TEST_SID.clone(), KernelClass::Process.into());
        }

        // The test query should now miss.
        let delegate_query_count = avc.delegate.query_count();
        avc.compute_access_decision(
            A_TEST_SID.clone(),
            A_TEST_SID.clone(),
            KernelClass::Process.into(),
        );
        assert_eq!(delegate_query_count + 1, avc.delegate.query_count());
    }

    #[test]
    fn thread_local_query_access_vector_cache_reset() {
        let cache_version = Arc::new(AtomicVersion::default());
        let mut avc = ThreadLocalQuery::new(cache_version.clone(), Counter::<DenyAll>::default());

        // Reset deferred to next query.
        assert_eq!(0, avc.delegate.reset_count());
        cache_version.reset();
        assert_eq!(0, avc.delegate.reset_count());
        avc.compute_access_decision(
            A_TEST_SID.clone(),
            A_TEST_SID.clone(),
            KernelClass::Process.into(),
        );
        assert_eq!(1, avc.delegate.reset_count());
    }

    #[test]
    fn access_vector_cache_ioctl_hit() {
        let mut avc = FifoQueryCache::<_>::new(Counter::<DenyAll>::default(), TEST_CAPACITY);
        assert_eq!(0, avc.delegate.query_count());
        assert_eq!(
            XpermsBitmap::NONE,
            avc.compute_ioctl_access_decision(
                A_TEST_SID.clone(),
                A_TEST_SID.clone(),
                KernelClass::Process.into(),
                0x0,
            )
            .allow
        );
        assert_eq!(1, avc.delegate.query_count());
        // The second request for the same key is a cache hit.
        assert_eq!(
            XpermsBitmap::NONE,
            avc.compute_ioctl_access_decision(
                A_TEST_SID.clone(),
                A_TEST_SID.clone(),
                KernelClass::Process.into(),
                0x0
            )
            .allow
        );
        assert_eq!(1, avc.delegate.query_count());
    }

    #[test]
    fn access_vector_cache_ioctl_miss() {
        let mut avc = FifoQueryCache::<_>::new(Counter::<DenyAll>::default(), TEST_CAPACITY);

        // Make the test query, which will trivially miss.
        avc.compute_ioctl_access_decision(
            A_TEST_SID.clone(),
            A_TEST_SID.clone(),
            KernelClass::Process.into(),
            0x0,
        );

        // Fill the ioctl cache with new queries, which should evict the test query.
        for ioctl_prefix in 0x1..(1 + avc.ioctl_access_cache.capacity())
            .try_into()
            .expect("assumed that test ioctl cache capacity was < 255")
        {
            avc.compute_ioctl_access_decision(
                A_TEST_SID.clone(),
                A_TEST_SID.clone(),
                KernelClass::Process.into(),
                ioctl_prefix,
            );
        }
        // Make sure that we've fulfilled at least one new cache miss since the original test query,
        // and that the cache is now full.
        assert!(avc.delegate.query_count() > 1);
        assert!(avc.ioctl_access_cache_is_full());
        let delegate_query_count = avc.delegate.query_count();

        // Making the original test query again should result in another miss.
        avc.compute_ioctl_access_decision(
            A_TEST_SID.clone(),
            A_TEST_SID.clone(),
            KernelClass::Process.into(),
            0x0,
        );
        assert_eq!(delegate_query_count + 1, avc.delegate.query_count());
    }
}

/// Async tests that depend on `fuchsia::test` only run in starnix.
#[cfg(test)]
#[cfg(feature = "selinux_starnix")]
mod starnix_tests {
    use super::testing::*;
    use super::*;
    use crate::policy::testing::{ACCESS_VECTOR_0001, ACCESS_VECTOR_0010};
    use crate::policy::AccessVector;
    use crate::KernelClass;

    use rand::distributions::Uniform;
    use rand::{thread_rng, Rng as _};
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::AtomicU32;
    use std::thread::spawn;

    const NO_RIGHTS: u32 = 0;
    const READ_RIGHTS: u32 = 1;
    const WRITE_RIGHTS: u32 = 2;

    const ACCESS_VECTOR_READ: AccessDecision = AccessDecision::allow(ACCESS_VECTOR_0001);
    const ACCESS_VECTOR_WRITE: AccessDecision = AccessDecision::allow(ACCESS_VECTOR_0010);

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
        fn compute_access_decision(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _target_class: ObjectClass,
        ) -> AccessDecision {
            let policy = self.policy.as_ref().load(Ordering::Relaxed);
            if policy == NO_RIGHTS {
                AccessDecision::default()
            } else if policy == READ_RIGHTS {
                ACCESS_VECTOR_READ
            } else if policy == WRITE_RIGHTS {
                ACCESS_VECTOR_WRITE
            } else {
                panic!("compute_access_decision found invalid policy: {}", policy);
            }
        }

        fn compute_new_fs_node_sid(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
        ) -> Result<SecurityId, anyhow::Error> {
            unreachable!()
        }

        fn compute_new_fs_node_sid_with_name(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
            _fs_node_name: NullessByteStr<'_>,
        ) -> Option<SecurityId> {
            unreachable!()
        }

        fn compute_ioctl_access_decision(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _target_class: ObjectClass,
            _ioctl_prefix: u8,
        ) -> IoctlAccessDecision {
            todo!()
        }
    }

    impl Reset for PolicyServer {
        fn reset(&self) -> bool {
            true
        }
    }

    #[fuchsia::test]
    async fn thread_local_query_access_vector_cache_coherence() {
        for _ in 0..TEST_CAPACITY {
            test_thread_local_query_access_vector_cache_coherence().await
        }
    }

    /// Tests cache coherence over two policy changes over a [`ThreadLocalQuery`].
    async fn test_thread_local_query_access_vector_cache_coherence() {
        let active_policy: Arc<AtomicU32> = Arc::new(Default::default());
        let policy_server: Arc<PolicyServer> =
            Arc::new(PolicyServer { policy: active_policy.clone() });
        let cache_version = Arc::new(AtomicVersion::default());

        let fixed_avc = FifoQueryCache::<_>::new(policy_server.clone(), TEST_CAPACITY);
        let cache_version_for_avc = cache_version.clone();
        let mut query_avc = ThreadLocalQuery::new(cache_version_for_avc, fixed_avc);

        policy_server.set_policy(NO_RIGHTS);
        let (tx, rx) = futures::channel::oneshot::channel();
        let query_thread = spawn(move || {
            let mut trace = vec![];

            for _ in 0..2000 {
                trace.push(query_avc.compute_access_decision(
                    A_TEST_SID.clone(),
                    A_TEST_SID.clone(),
                    KernelClass::Process.into(),
                ))
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
            if i != 0 && rights.allow != prev_rights {
                // Return-to-previous-rights => cache incoherence!
                assert!(!observed_rights.contains(&rights.allow));
                observed_rights.insert(rights.allow);
            }

            prev_rights = rights.allow;
        }
    }

    #[fuchsia::test]
    async fn locked_fixed_access_vector_cache_coherence() {
        for _ in 0..10 {
            test_locked_fixed_access_vector_cache_coherence().await
        }
    }

    /// Tests cache coherence over two policy changes over a `Locked<Fixed>`.
    async fn test_locked_fixed_access_vector_cache_coherence() {
        //
        // Test setup
        //

        let active_policy: Arc<AtomicU32> = Arc::new(Default::default());
        let policy_server = Arc::new(PolicyServer { policy: active_policy.clone() });
        let fixed_avc = FifoQueryCache::<_>::new(policy_server.clone(), TEST_CAPACITY);
        let avc = Locked::new(fixed_avc);
        let sids = unique_sids(30);

        // Ensure the initial policy is `NO_RIGHTS`.
        policy_server.set_policy(NO_RIGHTS);

        //
        // Test run: Two threads will query the AVC many times while two other threads make policy
        // changes.
        //

        // Allow both query threads to synchronize on "last policy change has been made". Query
        // threads use this signal to ensure at least some of their queries occur after the last
        // policy change.
        let (tx_last_policy_change_1, rx_last_policy_change_1) =
            futures::channel::oneshot::channel();
        let (tx_last_policy_change_2, rx_last_policy_change_2) =
            futures::channel::oneshot::channel();

        // Set up two querying threads. The number of iterations in each thread is highly likely
        // to perform queries that overlap with the two policy changes, but to be sure, use
        // `rx_last_policy_change_#` to synchronize  before last queries.
        let (tx1, rx1) = futures::channel::oneshot::channel();
        let avc_for_query_1 = avc.clone();
        let sids_for_query_1 = sids.clone();

        let query_thread_1 = spawn(|| async move {
            let sids = sids_for_query_1;
            let mut trace = vec![];

            for i in thread_rng().sample_iter(&Uniform::new(0, 20)).take(2000) {
                trace.push((
                    sids[i].clone(),
                    avc_for_query_1.compute_access_decision(
                        sids[i].clone(),
                        A_TEST_SID.clone(),
                        KernelClass::Process.into(),
                    ),
                ))
            }

            rx_last_policy_change_1.await.expect("receive last-policy-change signal (1)");

            for i in thread_rng().sample_iter(&Uniform::new(0, 20)).take(10) {
                trace.push((
                    sids[i].clone(),
                    avc_for_query_1.compute_access_decision(
                        sids[i].clone(),
                        A_TEST_SID.clone(),
                        KernelClass::Process.into(),
                    ),
                ))
            }

            tx1.send(trace).expect("send trace 1");

            //
            // Test expectations: After `<final-policy-reset>; avc.reset();
            // avc.compute_access_decision();`, all caches (including those that lazily reset on
            // next query) must contain *only* items consistent with the final policy: `(_, _, ) =>
            // WRITE`.
            //

            for (_, result) in avc_for_query_1.delegate.lock().access_cache.iter() {
                assert_eq!(ACCESS_VECTOR_WRITE, result.access_decision);
            }
        });

        let (tx2, rx2) = futures::channel::oneshot::channel();
        let avc_for_query_2 = avc.clone();
        let sids_for_query_2 = sids.clone();

        let query_thread_2 = spawn(|| async move {
            let sids = sids_for_query_2;
            let mut trace = vec![];

            for i in thread_rng().sample_iter(&Uniform::new(10, 30)).take(2000) {
                trace.push((
                    sids[i].clone(),
                    avc_for_query_2.compute_access_decision(
                        sids[i].clone(),
                        A_TEST_SID.clone(),
                        KernelClass::Process.into(),
                    ),
                ))
            }

            rx_last_policy_change_2.await.expect("receive last-policy-change signal (2)");

            for i in thread_rng().sample_iter(&Uniform::new(10, 30)).take(10) {
                trace.push((
                    sids[i].clone(),
                    avc_for_query_2.compute_access_decision(
                        sids[i].clone(),
                        A_TEST_SID.clone(),
                        KernelClass::Process.into(),
                    ),
                ))
            }

            tx2.send(trace).expect("send trace 2");

            //
            // Test expectations: After `<final-policy-reset>; avc.reset();
            // avc.compute_access_decision();`, all caches (including those that lazily reset on
            // next query) must contain *only* items consistent with the final policy: `(_, _, ) =>
            // NONE`.
            //

            for (_, result) in avc_for_query_2.delegate.lock().access_cache.iter() {
                assert_eq!(ACCESS_VECTOR_WRITE, result.access_decision);
            }
        });

        let policy_server_for_set_read = policy_server.clone();
        let avc_for_set_read = avc.clone();
        let (tx_set_read, rx_set_read) = futures::channel::oneshot::channel();
        let set_read_thread = spawn(move || {
            // Allow some queries to accumulate before first policy change.
            std::thread::sleep(std::time::Duration::from_micros(1));

            // Set security server policy *first*, then reset caches. This is normally the
            // responsibility of the security server.
            policy_server_for_set_read.set_policy(READ_RIGHTS);
            avc_for_set_read.reset();

            tx_set_read.send(true).expect("send set-read signal")
        });

        let policy_server_for_set_write = policy_server.clone();
        let avc_for_set_write = avc;
        let set_write_thread = spawn(|| async move {
            // Complete set-read before executing set-write.
            rx_set_read.await.expect("receive set-write signal");
            std::thread::sleep(std::time::Duration::from_micros(1));

            // Set security server policy *first*, then reset caches. This is normally the
            // responsibility of the security server.
            policy_server_for_set_write.set_policy(WRITE_RIGHTS);
            avc_for_set_write.reset();

            tx_last_policy_change_1.send(true).expect("send last-policy-change signal (1)");
            tx_last_policy_change_2.send(true).expect("send last-policy-change signal (2)");
        });

        // Join all threads.
        set_read_thread.join().expect("join set-policy-to-read");
        let _ = set_write_thread.join().expect("join set-policy-to-write").await;
        let _ = query_thread_1.join().expect("join query").await;
        let _ = query_thread_2.join().expect("join query").await;

        // Receive traces from query threads.
        let trace_1 = rx1.await.expect("receive trace 1");
        let trace_2 = rx2.await.expect("receive trace 2");

        //
        // Test expectations: Inspect individual query thread traces separately. For each thread,
        // group `(sid, 0, 0) -> AccessVector` trace items by `sid`, keeping them in chronological
        // order. Every such grouping should observe at most `NONE->READ`, `READ->WRITE`
        // transitions. Any other transitions suggests out-of-order "jitter" from stale cache items.
        //
        // We cannot expect stronger guarantees (e.g., across different queries). For example, the
        // following scheduling is possible:
        //
        // 1. Policy change thread changes policy from NONE to READ;
        // 2. Query thread qt queries q1, which as never been queried before. Result: READ.
        // 3. Query thread qt queries q0, which was cached before policy reload. Result: NONE.
        // 4. All caches reset.
        //
        // Notice that, ignoring query inputs, qt observes trace `..., READ, NONE`. However, such a
        // sequence must not occur when observing qt's trace filtered by query input (q1, q0, etc.).
        //

        for trace in [trace_1, trace_2] {
            let mut trace_by_sid = HashMap::<SecurityId, Vec<AccessVector>>::new();
            for (sid, access_decision) in trace {
                trace_by_sid.entry(sid).or_insert(vec![]).push(access_decision.allow);
            }
            for access_vectors in trace_by_sid.values() {
                let initial_rights = AccessVector::NONE;
                let mut prev_rights = &initial_rights;
                for rights in access_vectors.iter() {
                    // Note: `WRITE > READ > NONE`.
                    assert!(rights >= prev_rights);
                    prev_rights = rights;
                }
            }
        }
    }

    struct SecurityServer {
        manager: Manager<SecurityServer>,
        policy: Arc<AtomicU32>,
    }

    impl SecurityServer {
        fn manager(&self) -> &Manager<SecurityServer> {
            &self.manager
        }
    }

    impl Query for SecurityServer {
        fn compute_access_decision(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _target_class: ObjectClass,
        ) -> AccessDecision {
            let policy = self.policy.as_ref().load(Ordering::Relaxed);
            if policy == NO_RIGHTS {
                AccessDecision::default()
            } else if policy == READ_RIGHTS {
                ACCESS_VECTOR_READ
            } else if policy == WRITE_RIGHTS {
                ACCESS_VECTOR_WRITE
            } else {
                panic!("compute_access_decision found invalid policy: {}", policy);
            }
        }

        fn compute_new_fs_node_sid(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
        ) -> Result<SecurityId, anyhow::Error> {
            unreachable!()
        }

        fn compute_new_fs_node_sid_with_name(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _fs_node_class: FsNodeClass,
            _fs_node_name: NullessByteStr<'_>,
        ) -> Option<SecurityId> {
            unreachable!()
        }

        fn compute_ioctl_access_decision(
            &self,
            _source_sid: SecurityId,
            _target_sid: SecurityId,
            _target_class: ObjectClass,
            _ioctl_prefix: u8,
        ) -> IoctlAccessDecision {
            todo!()
        }
    }

    impl Reset for SecurityServer {
        fn reset(&self) -> bool {
            true
        }
    }

    #[fuchsia::test]
    async fn manager_cache_coherence() {
        for _ in 0..10 {
            test_manager_cache_coherence().await
        }
    }

    /// Tests cache coherence over two policy changes over a `Locked<Fixed>`.
    async fn test_manager_cache_coherence() {
        //
        // Test setup
        //

        let (active_policy, security_server) = {
            // Carefully initialize strong and weak references between security server and its cache
            // manager.

            let manager = Manager::new();

            // Initialize `security_server` to own `manager`.
            let active_policy: Arc<AtomicU32> = Arc::new(Default::default());
            let security_server =
                Arc::new(SecurityServer { manager, policy: active_policy.clone() });

            // Replace `security_server.manager`'s  empty `Weak` with `Weak<security_server>` to
            // start servering `security_server`'s policy out of `security_server.manager`'s cache.
            security_server
                .as_ref()
                .manager()
                .set_security_server(Arc::downgrade(&security_server));

            (active_policy, security_server)
        };
        let sids = unique_sids(30);

        fn set_policy(owner: &Arc<AtomicU32>, policy: u32) {
            if policy > 2 {
                panic!("attempt to set policy to invalid value: {}", policy);
            }
            owner.as_ref().store(policy, Ordering::Relaxed);
        }

        // Ensure the initial policy is `NO_RIGHTS`.
        set_policy(&active_policy, NO_RIGHTS);

        //
        // Test run: Two threads will query the AVC many times while two other threads make policy
        // changes.
        //

        // Allow both query threads to synchronize on "last policy change has been made". Query
        // threads use this signal to ensure at least some of their queries occur after the last
        // policy change.
        let (tx_last_policy_change_1, rx_last_policy_change_1) =
            futures::channel::oneshot::channel();
        let (tx_last_policy_change_2, rx_last_policy_change_2) =
            futures::channel::oneshot::channel();

        // Set up two querying threads. The number of iterations in each thread is highly likely
        // to perform queries that overlap with the two policy changes, but to be sure, use
        // `rx_last_policy_change_#` to synchronize  before last queries.
        let (tx1, rx1) = futures::channel::oneshot::channel();
        let mut avc_for_query_1 = security_server.manager().new_thread_local_cache();
        let sids_for_query_1 = sids.clone();

        let query_thread_1 = spawn(|| async move {
            let sids = sids_for_query_1;
            let mut trace = vec![];

            for i in thread_rng().sample_iter(&Uniform::new(0, 20)).take(2000) {
                trace.push((
                    sids[i].clone(),
                    avc_for_query_1.compute_access_decision(
                        sids[i].clone(),
                        A_TEST_SID.clone(),
                        KernelClass::Process.into(),
                    ),
                ))
            }

            rx_last_policy_change_1.await.expect("receive last-policy-change signal (1)");

            for i in thread_rng().sample_iter(&Uniform::new(0, 20)).take(10) {
                trace.push((
                    sids[i].clone(),
                    avc_for_query_1.compute_access_decision(
                        sids[i].clone(),
                        A_TEST_SID.clone(),
                        KernelClass::Process.into(),
                    ),
                ))
            }

            tx1.send(trace).expect("send trace 1");

            //
            // Test expectations: After `<final-policy-reset>; avc.reset();
            // avc.compute_access_decision();`, all caches (including those that lazily reset on
            // next query) must contain *only* items consistent with the final policy: `(_, _, ) =>
            // WRITE`.
            //

            for (_, result) in avc_for_query_1.delegate.access_cache.iter() {
                assert_eq!(ACCESS_VECTOR_WRITE, result.access_decision);
            }
        });

        let (tx2, rx2) = futures::channel::oneshot::channel();
        let mut avc_for_query_2 = security_server.manager().new_thread_local_cache();
        let sids_for_query_2 = sids.clone();

        let query_thread_2 = spawn(|| async move {
            let sids = sids_for_query_2;
            let mut trace = vec![];

            for i in thread_rng().sample_iter(&Uniform::new(10, 30)).take(2000) {
                trace.push((
                    sids[i].clone(),
                    avc_for_query_2.compute_access_decision(
                        sids[i].clone(),
                        A_TEST_SID.clone(),
                        KernelClass::Process.into(),
                    ),
                ))
            }

            rx_last_policy_change_2.await.expect("receive last-policy-change signal (2)");

            for i in thread_rng().sample_iter(&Uniform::new(10, 30)).take(10) {
                trace.push((
                    sids[i].clone(),
                    avc_for_query_2.compute_access_decision(
                        sids[i].clone(),
                        A_TEST_SID.clone(),
                        KernelClass::Process.into(),
                    ),
                ))
            }

            tx2.send(trace).expect("send trace 2");

            //
            // Test expectations: After `<final-policy-reset>; avc.reset();
            // avc.compute_access_decision();`, all caches (including those that lazily reset on
            // next query) must contain *only* items consistent with the final policy: `(_, _, ) =>
            // WRITE`.
            //

            for (_, result) in avc_for_query_2.delegate.access_cache.iter() {
                assert_eq!(ACCESS_VECTOR_WRITE, result.access_decision);
            }
        });

        // Set up two threads that will update the security policy *first*, then reset caches.
        // The threads synchronize to ensure a policy order of NONE->READ->WRITE.
        let active_policy_for_set_read = active_policy.clone();
        let security_server_for_set_read = security_server.clone();
        let (tx_set_read, rx_set_read) = futures::channel::oneshot::channel();
        let set_read_thread = spawn(move || {
            // Allow some queries to accumulate before first policy change.
            std::thread::sleep(std::time::Duration::from_micros(1));

            // Set security server policy *first*, then reset caches. This is normally the
            // responsibility of the security server.
            set_policy(&active_policy_for_set_read, READ_RIGHTS);
            security_server_for_set_read.manager().reset();

            tx_set_read.send(true).expect("send set-read signal")
        });
        let active_policy_for_set_write = active_policy.clone();
        let security_server_for_set_write = security_server.clone();
        let set_write_thread = spawn(|| async move {
            // Complete set-read before executing set-write.
            rx_set_read.await.expect("receive set-read signal");
            std::thread::sleep(std::time::Duration::from_micros(1));

            // Set security server policy *first*, then reset caches. This is normally the
            // responsibility of the security server.
            set_policy(&active_policy_for_set_write, WRITE_RIGHTS);
            security_server_for_set_write.manager().reset();

            tx_last_policy_change_1.send(true).expect("send last-policy-change signal (1)");
            tx_last_policy_change_2.send(true).expect("send last-policy-change signal (2)");
        });

        // Join all threads.
        set_read_thread.join().expect("join set-policy-to-read");
        let _ = set_write_thread.join().expect("join set-policy-to-write").await;
        let _ = query_thread_1.join().expect("join query").await;
        let _ = query_thread_2.join().expect("join query").await;

        // Receive traces from query threads.
        let trace_1 = rx1.await.expect("receive trace 1");
        let trace_2 = rx2.await.expect("receive trace 2");

        //
        // Test expectations: Inspect individual query thread traces separately. For each thread,
        // group `(sid, 0, 0) -> AccessVector` trace items by `sid`, keeping them in chronological
        // order. Every such grouping should observe at most `NONE->READ`, `READ->WRITE`
        // transitions. Any other transitions suggests out-of-order "jitter" from stale cache items.
        //
        // We cannot expect stronger guarantees (e.g., across different queries). For example, the
        // following scheduling is possible:
        //
        // 1. Policy change thread changes policy from NONE to READ;
        // 2. Query thread qt queries q1, which as never been queried before. Result: READ.
        // 3. Query thread qt queries q0, which was cached before policy reload. Result: NONE.
        // 4. All caches reset.
        //
        // Notice that, ignoring query inputs, qt observes `..., READ, NONE`. However, such a
        // sequence must not occur when observing qt's trace filtered by query input (q1, q0, etc.).
        //
        // Finally, the shared (`Locked`) cache should contain only entries consistent with
        // the final policy: `(_, _, ) => WRITE`.
        //

        for trace in [trace_1, trace_2] {
            let mut trace_by_sid = HashMap::<SecurityId, Vec<AccessVector>>::new();
            for (sid, access_decision) in trace {
                trace_by_sid.entry(sid).or_insert(vec![]).push(access_decision.allow);
            }
            for access_vectors in trace_by_sid.values() {
                let initial_rights = AccessVector::NONE;
                let mut prev_rights = &initial_rights;
                for rights in access_vectors.iter() {
                    // Note: `WRITE > READ > NONE`.
                    assert!(rights >= prev_rights);
                    prev_rights = rights;
                }
            }
        }

        let shared_cache = security_server.manager().shared_cache.delegate.lock();
        for (_, result) in shared_cache.access_cache.iter() {
            assert_eq!(ACCESS_VECTOR_WRITE, result.access_decision);
        }
    }
}
