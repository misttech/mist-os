// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! # The Allocator
//!
//! The allocator in Fxfs is filesystem-wide entity responsible for managing the allocation of
//! regions of the device to "owners" (which are `ObjectStore`).
//!
//! Allocations are tracked in an LSMTree with coalescing used to merge neighboring allocations
//! with the same properties (owner and reference count). As of writing, reference counting is not
//! used. (Reference counts are intended for future use if/when snapshot support is implemented.)
//!
//! There are a number of important implementation features of the allocator that warrant further
//! documentation.
//!
//! ## Byte limits
//!
//! Fxfs is a multi-volume filesystem. Fuchsia with fxblob currently uses two primary volumes -
//! an unencrypted volume for blob storage and an encrypted volume for data storage.
//! Byte limits ensure that no one volume can consume all available storage. This is important
//! as software updates must always be possible (blobs) and conversely configuration data should
//! always be writable (data).
//!
//! ## Reservation tracking
//!
//! Fxfs on Fuchsia leverages write-back caching which allows us to buffer writes in RAM until
//! memory pressure, an explicit flush or a file close requires us to persist data to storage.
//!
//! To ensure that we do not over-commit in such cases (e.g. by writing many files to RAM only to
//! find out tha there is insufficient disk to store them all), the allocator includes a simple
//! reservation tracking mechanism.
//!
//! Reservation tracking is implemented hierarchically such that a reservation can portion out
//! sub-reservations, each of which may be "reserved" or "forgotten" when it comes time to actually
//! allocate space.
//!
//! ## Fixed locations
//!
//! The only fixed location files in Fxfs are the first 512kiB extents of the two superblocks that
//! exist as the first things on the disk (i.e. The first 1MiB). The allocator manages these
//! locations, but does so using a 'mark_allocated' method distinct from all other allocations in
//! which the location is left up to the allocator.
//!
//! ## Deallocated unusable regions
//!
//! It is not legal to reuse a deallocated disk region until after a flush. Transactions
//! are not guaranteed to be persisted until after a successful flush so any reuse of
//! a region before then followed by power loss may lead to corruption.
//!
//! e.g. Consider if we deallocate a file, reuse its extent for another file, then crash after
//! writing to the new file but not yet flushing the journal. At next mount we will have lost the
//! transaction despite overwriting the original file's data, leading to inconsistency errors (data
//! corruption).
//!
//! These regions are currently tracked in RAM in the allocator.
//!
//! ## TRIMed unusable regions
//!
//! TRIM notifies the SSD controller of regions of the disk that are not used. This helps with SSD
//! performance and longevity because wear leveling and ECC need not be performed on logical blocks
//! that don't contain useful data. Fxfs performs batch TRIM passes periodically in the background
//! by walking over unallocated regions of the disk and performing trim() operations on unused
//! regions. These operations are asynchronous and therefore these regions are unusable until
//! the operation completes.
//!
//! We allocate these free ranges using a monotonically increasing
//! `LocationHint::NextAvailable(range)` hint to allocate(). When the trim completes, we free the
//! range, returning it to the pool of available storage.
//!
//! ## Volume deletion
//!
//! We make use of an optimisation in the case where an entire volume is deleted. In such cases,
//! rather than individually deallocate all disk regions associated with that volume, we make
//! note of the deletion and perform special merge operation on the next LSMTree compaction that
//! filters out allocations for the deleted volume.
//!
//! This is designed to make dropping of volumes significantly cheaper, but it does add some
//! additional complexity if implementing an allocator that implements data structures to track
//! free space (rather than just allocated space).
//!
//! ## Image generation
//!
//! The Fuchsia build process requires building an initial filesystem image. In the case of
//! fxblob-based boards, this is an Fxfs filesystem containing a volume with the base set of
//! blobs required to bootstrap the system. When we build such an image, we want it to be as compact
//! as possible as we're potentially packaging it up for distribution. To that end, our allocation
//! strategy should differ between image generation and "live" use cases.

pub mod merge;
pub mod strategy;

use {
    crate::{
        debug_assert_not_too_long,
        drop_event::DropEvent,
        errors::FxfsError,
        filesystem::{ApplyContext, ApplyMode, FxFilesystem, JournalingObject, SyncOptions},
        log::*,
        lsm_tree::{
            cache::NullCache,
            layers_from_handles,
            types::{
                Item, ItemRef, LayerIterator, LayerKey, MergeType, OrdLowerBound, OrdUpperBound,
                RangeKey, SortByU64,
            },
            LSMTree,
        },
        object_handle::{ObjectHandle, ReadObjectHandle, INVALID_OBJECT_ID},
        object_store::{
            allocator::strategy::{AllocatorStrategy, LocationHint},
            object_manager::ReservationUpdate,
            transaction::{
                lock_keys, AllocatorMutation, AssocObj, LockKey, Mutation, Options, Transaction,
            },
            tree, DirectWriter, HandleOptions, ObjectStore,
        },
        range::RangeExt,
        round::{round_div, round_down},
        serialized_types::{
            Version, Versioned, VersionedLatest, DEFAULT_MAX_SERIALIZED_RECORD_SIZE,
        },
    },
    anyhow::{anyhow, bail, ensure, Context, Error},
    async_trait::async_trait,
    either::Either::{Left, Right},
    event_listener::EventListener,
    fprint::TypeFingerprint,
    fuchsia_inspect::ArrayProperty,
    futures::FutureExt,
    merge::{filter_marked_for_deletion, filter_tombstones, merge},
    serde::{Deserialize, Serialize},
    std::{
        borrow::Borrow,
        collections::{BTreeMap, HashSet, VecDeque},
        hash::Hash,
        marker::PhantomData,
        ops::{Bound, Range},
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc, Mutex, Weak,
        },
    },
};

/// This trait is implemented by things that own reservations.
pub trait ReservationOwner: Send + Sync {
    /// Report that bytes are being released from the reservation back to the the |ReservationOwner|
    /// where |owner_object_id| is the owner under the root object store associated with the
    /// reservation.
    fn release_reservation(&self, owner_object_id: Option<u64>, amount: u64);
}

/// A reservation guarantees that when it comes time to actually allocate, it will not fail due to
/// lack of space.  Sub-reservations (a.k.a. holds) are possible which effectively allows part of a
/// reservation to be set aside until it's time to commit.  Reservations do offer some
/// thread-safety, but some responsibility is born by the caller: e.g. calling `forget` and
/// `reserve` at the same time from different threads is unsafe. Reservations are have an
/// |owner_object_id| which associates it with an object under the root object store that the
/// reservation is accounted against.
pub struct ReservationImpl<T: Borrow<U>, U: ReservationOwner + ?Sized> {
    owner: T,
    owner_object_id: Option<u64>,
    inner: Mutex<ReservationInner>,
    phantom: PhantomData<U>,
}

#[derive(Debug, Default)]
struct ReservationInner {
    // Amount currently held by this reservation.
    amount: u64,

    // Amount reserved by sub-reservations.
    reserved: u64,
}

impl<T: Borrow<U>, U: ReservationOwner + ?Sized> std::fmt::Debug for ReservationImpl<T, U> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.lock().unwrap().fmt(f)
    }
}

impl<T: Borrow<U> + Clone + Send + Sync, U: ReservationOwner + ?Sized> ReservationImpl<T, U> {
    pub fn new(owner: T, owner_object_id: Option<u64>, amount: u64) -> Self {
        Self {
            owner,
            owner_object_id,
            inner: Mutex::new(ReservationInner { amount, reserved: 0 }),
            phantom: PhantomData,
        }
    }

    pub fn owner_object_id(&self) -> Option<u64> {
        self.owner_object_id
    }

    /// Returns the total amount of the reservation, not accounting for anything that might be held.
    pub fn amount(&self) -> u64 {
        self.inner.lock().unwrap().amount
    }

    /// Adds more to the reservation.
    pub fn add(&self, amount: u64) {
        self.inner.lock().unwrap().amount += amount;
    }

    /// Returns the entire amount of the reservation.  The caller is responsible for maintaining
    /// consistency, i.e. updating counters, etc, and there can be no sub-reservations (an assert
    /// will fire otherwise).
    pub fn forget(&self) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        assert_eq!(inner.reserved, 0);
        std::mem::take(&mut inner.amount)
    }

    /// Takes some of the reservation.  The caller is responsible for maintaining consistency,
    /// i.e. updating counters, etc.  This will assert that the amount being forgotten does not
    /// exceed the available reservation amount; the caller should ensure that this is the case.
    pub fn forget_some(&self, amount: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.amount -= amount;
        assert!(inner.reserved <= inner.amount);
    }

    /// Returns a partial amount of the reservation.  If the reservation is smaller than |amount|,
    /// returns less than the requested amount, and this can be *zero*.
    fn reserve_at_most(&self, amount: u64) -> ReservationImpl<&Self, Self> {
        let mut inner = self.inner.lock().unwrap();
        let taken = std::cmp::min(amount, inner.amount - inner.reserved);
        inner.reserved += taken;
        ReservationImpl::new(self, self.owner_object_id, taken)
    }

    /// Reserves *exactly* amount if possible.
    pub fn reserve(&self, amount: u64) -> Option<ReservationImpl<&Self, Self>> {
        let mut inner = self.inner.lock().unwrap();
        if inner.amount - inner.reserved < amount {
            None
        } else {
            inner.reserved += amount;
            Some(ReservationImpl::new(self, self.owner_object_id, amount))
        }
    }

    /// Commits a previously reserved amount from this reservation.  The caller is responsible for
    /// ensuring the amount was reserved.
    pub fn commit(&self, amount: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.reserved -= amount;
        inner.amount -= amount;
    }

    /// Returns some of the reservation.
    pub fn give_back(&self, amount: u64) {
        self.owner.borrow().release_reservation(self.owner_object_id, amount);
        let mut inner = self.inner.lock().unwrap();
        inner.amount -= amount;
        assert!(inner.reserved <= inner.amount);
    }

    /// Moves `amount` from this reservation to another reservation.
    pub fn move_to<V: Borrow<W> + Clone + Send + Sync, W: ReservationOwner + ?Sized>(
        &self,
        other: &ReservationImpl<V, W>,
        amount: u64,
    ) {
        assert_eq!(self.owner_object_id, other.owner_object_id());
        let mut inner = self.inner.lock().unwrap();
        if let Some(amount) = inner.amount.checked_sub(amount) {
            inner.amount = amount;
        } else {
            std::mem::drop(inner);
            panic!("Insufficient reservation space");
        }
        other.add(amount);
    }
}

impl<T: Borrow<U>, U: ReservationOwner + ?Sized> Drop for ReservationImpl<T, U> {
    fn drop(&mut self) {
        let inner = self.inner.get_mut().unwrap();
        assert_eq!(inner.reserved, 0);
        let owner_object_id = self.owner_object_id;
        if inner.amount > 0 {
            self.owner
                .borrow()
                .release_reservation(owner_object_id, std::mem::take(&mut inner.amount));
        }
    }
}

impl<T: Borrow<U> + Send + Sync, U: ReservationOwner + ?Sized> ReservationOwner
    for ReservationImpl<T, U>
{
    fn release_reservation(&self, owner_object_id: Option<u64>, amount: u64) {
        // Sub-reservations should belong to the same owner (or lack thereof).
        assert_eq!(owner_object_id, self.owner_object_id);
        let mut inner = self.inner.lock().unwrap();
        assert!(inner.reserved >= amount, "{} >= {}", inner.reserved, amount);
        inner.reserved -= amount;
    }
}

pub type Reservation = ReservationImpl<Arc<dyn ReservationOwner>, dyn ReservationOwner>;

pub type Hold<'a> = ReservationImpl<&'a Reservation, Reservation>;

// Our allocator implementation tracks extents with a reference count.  At time of writing, these
// reference counts should never exceed 1, but that might change with snapshots and clones.

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, TypeFingerprint, Versioned)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub struct AllocatorKey {
    pub device_range: Range<u64>,
}

impl SortByU64 for AllocatorKey {
    fn get_leading_u64(&self) -> u64 {
        self.device_range.start
    }
}

impl AllocatorKey {
    /// Returns a new key that is a lower bound suitable for use with merge_into.
    pub fn lower_bound_for_merge_into(self: &AllocatorKey) -> AllocatorKey {
        AllocatorKey { device_range: 0..self.device_range.start }
    }
}

impl LayerKey for AllocatorKey {
    fn merge_type(&self) -> MergeType {
        MergeType::OptimizedMerge
    }
}

impl OrdUpperBound for AllocatorKey {
    fn cmp_upper_bound(&self, other: &AllocatorKey) -> std::cmp::Ordering {
        self.device_range.end.cmp(&other.device_range.end)
    }
}

impl OrdLowerBound for AllocatorKey {
    fn cmp_lower_bound(&self, other: &AllocatorKey) -> std::cmp::Ordering {
        // The ordering over range.end is significant here as it is used in
        // the heap ordering that feeds into our merge function and
        // a total ordering over range lets us remove a symmetry case from
        // the allocator merge function.
        self.device_range
            .start
            .cmp(&other.device_range.start)
            .then(self.device_range.end.cmp(&other.device_range.end))
    }
}

impl Ord for AllocatorKey {
    fn cmp(&self, other: &AllocatorKey) -> std::cmp::Ordering {
        self.device_range
            .start
            .cmp(&other.device_range.start)
            .then(self.device_range.end.cmp(&other.device_range.end))
    }
}

impl PartialOrd for AllocatorKey {
    fn partial_cmp(&self, other: &AllocatorKey) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl RangeKey for AllocatorKey {
    fn overlaps(&self, other: &Self) -> bool {
        self.device_range.start < other.device_range.end
            && self.device_range.end > other.device_range.start
    }
}

/// Allocations are "owned" by a single ObjectStore and are reference counted
/// (for future snapshot/clone support).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TypeFingerprint, Versioned)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub enum AllocatorValue {
    // Tombstone variant indicating an extent is no longer allocated.
    None,
    // Used when we know there are no possible allocations below us in the stack.
    // This is currently all the time. We used to have a related Delta type but
    // it has been removed due to correctness issues (https://fxbug.dev/42179428).
    Abs { count: u64, owner_object_id: u64 },
}

pub type AllocatorItem = Item<AllocatorKey, AllocatorValue>;

#[derive(Debug, Default, Clone, Deserialize, Serialize, TypeFingerprint, Versioned)]
pub struct AllocatorInfo {
    /// Holds the set of layer file object_id for the LSM tree (newest first).
    pub layers: Vec<u64>,
    /// Maps from owner_object_id to bytes allocated.
    pub allocated_bytes: BTreeMap<u64, u64>,
    /// Set of owner_object_id that we should ignore if found in layer files.
    pub marked_for_deletion: HashSet<u64>,
    // The limit for the number of allocates bytes per `owner_object_id` whereas the value. If there
    // is no limit present here for an `owner_object_id` assume it is max u64.
    pub limit_bytes: BTreeMap<u64, u64>,
}

const MAX_ALLOCATOR_INFO_SERIALIZED_SIZE: usize = 131_072;

/// Computes the target maximum extent size based on the block size of the allocator.
pub fn max_extent_size_for_block_size(block_size: u64) -> u64 {
    // Each block in an extent contains an 8-byte checksum (which due to varint encoding is 9
    // bytes), and a given extent record must be no larger DEFAULT_MAX_SERIALIZED_RECORD_SIZE.  We
    // also need to leave a bit of room (arbitrarily, 64 bytes) for the rest of the extent's
    // metadata.
    block_size * (DEFAULT_MAX_SERIALIZED_RECORD_SIZE - 64) / 9
}

#[derive(Default)]
struct AllocatorCounters {
    num_flushes: u64,
    last_flush_time: Option<std::time::SystemTime>,
    persistent_layer_file_sizes: Vec<u64>,
}

pub struct Allocator {
    filesystem: Weak<FxFilesystem>,
    block_size: u64,
    device_size: u64,
    object_id: u64,
    max_extent_size_bytes: u64,
    tree: LSMTree<AllocatorKey, AllocatorValue>,
    inner: Mutex<Inner>,
    allocation_mutex: futures::lock::Mutex<()>,
    counters: Mutex<AllocatorCounters>,
    maximum_offset: AtomicU64,
}

/// Tracks the different stages of byte allocations for an individual owner.
#[derive(Debug, Default, PartialEq)]
struct ByteTracking {
    /// This value is the up-to-date count of the number of allocated bytes per owner_object_id
    /// whereas the value in `Info::allocated_bytes` is the value as it was when we last flushed.
    /// This is i64 because it can be negative during replay.
    allocated_bytes: i64,
    /// This value is the number of bytes allocated to uncommitted allocations.
    uncommitted_allocated_bytes: u64,
    /// This value is the number of bytes allocated to reservations.
    reserved_bytes: u64,
    /// Committed deallocations that we cannot use until they are flushed to the device.  Each entry
    /// in this list is the log file offset at which it was committed and an array of deallocations
    /// that occurred at that time.
    committed_deallocated_bytes: u64,
}

impl ByteTracking {
    // Returns the total number of bytes that are taken either from reservations, allocations or
    // uncommitted allocations.
    fn used_bytes(&self) -> u64 {
        self.allocated_bytes as u64 + self.uncommitted_allocated_bytes + self.reserved_bytes
    }

    // Returns the amount that is not available to be allocated, which includes actually allocated
    // bytes, bytes that have been allocated for a transaction but the transaction hasn't committed
    // yet, and bytes that have been deallocated, but the device hasn't been flushed yet so we can't
    // reuse those bytes yet.
    fn unavailable_bytes(&self) -> u64 {
        self.allocated_bytes as u64
            + self.uncommitted_allocated_bytes
            + self.committed_deallocated_bytes
    }

    // Like `unavailable_bytes`, but treats as available the bytes which have been deallocated and
    // require a device flush to be reused.
    fn unavailable_after_sync_bytes(&self) -> u64 {
        self.allocated_bytes as u64 + self.uncommitted_allocated_bytes
    }
}

#[derive(Debug)]
struct CommittedDeallocation {
    // The offset at which this deallocation was committed.
    log_file_offset: u64,
    // The device range being deallocated.
    range: Range<u64>,
    // The owning object id which originally allocated it.
    owner_object_id: u64,
}

struct Inner {
    info: AllocatorInfo,
    /// The allocator can only be opened if there have been no allocations and it has not already
    /// been opened or initialized.
    opened: bool,
    /// The per-owner counters for bytes at various stages of the data life-cycle. From initial
    /// reservation through until the bytes are unallocated and eventually uncommitted.
    owner_bytes: BTreeMap<u64, ByteTracking>,
    /// This value is the number of bytes allocated to reservations but not tracked as part of a
    /// particular volume.
    unattributed_reserved_bytes: u64,
    /// Committed deallocations that we cannot use until they are flushed to the device.
    committed_deallocated: VecDeque<CommittedDeallocation>,
    /// Maps |owner_object_id| to log offset and bytes allocated to a deleted encryption volume.
    /// Once the journal has been flushed beyond 'log_offset', we replace entries here with
    /// an entry in AllocatorInfo to have all iterators ignore owner_object_id. That entry is
    /// then cleaned up at next (major) compaction time.
    committed_encrypted_volume_deletions: BTreeMap<u64, (/*log_offset:*/ u64, /*bytes:*/ u64)>,
    /// Bytes which are currently being trimmed.  These can still be allocated from, but the
    /// allocation will block until the current batch of trimming is finished.
    trim_reserved_bytes: u64,
    /// While a trim is being performed, this listener is set.  When the current batch of extents
    /// being trimmed have been released (and trim_reserved_bytes is 0), this is signaled.
    /// This should only be listened to while the allocation_mutex is held.
    trim_listener: Option<EventListener>,
    /// This controls how we allocate our free space to manage fragmentation.
    strategy: Box<dyn strategy::AllocatorStrategy>,
    /// Tracks the number of allocations of size 1,2,...63,>=64.
    histogram: [u64; 64],
}

impl Inner {
    fn allocated_bytes(&self) -> i64 {
        self.owner_bytes.values().map(|x| &x.allocated_bytes).sum()
    }

    fn uncommitted_allocated_bytes(&self) -> u64 {
        self.owner_bytes.values().map(|x| &x.uncommitted_allocated_bytes).sum()
    }

    fn reserved_bytes(&self) -> u64 {
        self.owner_bytes.values().map(|x| &x.reserved_bytes).sum::<u64>()
            + self.unattributed_reserved_bytes
    }

    fn owner_id_limit_bytes(&self, owner_object_id: u64) -> u64 {
        match self.info.limit_bytes.get(&owner_object_id) {
            Some(v) => *v,
            None => u64::MAX,
        }
    }

    fn owner_id_bytes_left(&self, owner_object_id: u64) -> u64 {
        let limit = self.owner_id_limit_bytes(owner_object_id);
        let used = match self.owner_bytes.get(&owner_object_id) {
            Some(b) => b.used_bytes(),
            None => 0,
        };
        if limit > used {
            limit - used
        } else {
            0
        }
    }

    // Returns the amount that is not available to be allocated, which includes actually allocated
    // bytes, bytes that have been allocated for a transaction but the transaction hasn't committed
    // yet, and bytes that have been deallocated, but the device hasn't been flushed yet so we can't
    // reuse those bytes yet.
    fn unavailable_bytes(&self) -> u64 {
        self.owner_bytes.values().map(|x| x.unavailable_bytes()).sum::<u64>()
            + self.committed_encrypted_volume_deletions.values().map(|(_, x)| x).sum::<u64>()
    }

    // Returns the total number of bytes that are taken either from reservations, allocations or
    // uncommitted allocations.
    fn used_bytes(&self) -> u64 {
        self.owner_bytes.values().map(|x| x.used_bytes()).sum::<u64>()
            + self.unattributed_reserved_bytes
    }

    // Like `unavailable_bytes`, but treats as available the bytes which have been deallocated and
    // require a device flush to be reused.
    fn unavailable_after_sync_bytes(&self) -> u64 {
        self.owner_bytes.values().map(|x| x.unavailable_after_sync_bytes()).sum::<u64>()
    }

    // Returns the number of bytes which will be available after the current batch of trimming
    // completes.
    fn bytes_available_not_being_trimmed(&self, device_size: u64) -> Result<u64, Error> {
        device_size
            .checked_sub(self.unavailable_after_sync_bytes() + self.trim_reserved_bytes)
            .ok_or(anyhow!(FxfsError::Inconsistent))
    }

    fn add_reservation(&mut self, owner_object_id: Option<u64>, amount: u64) {
        match owner_object_id {
            Some(owner) => self.owner_bytes.entry(owner).or_default().reserved_bytes += amount,
            None => self.unattributed_reserved_bytes += amount,
        };
    }

    fn remove_reservation(&mut self, owner_object_id: Option<u64>, amount: u64) {
        match owner_object_id {
            Some(owner) => {
                let owner_entry = self.owner_bytes.entry(owner).or_default();
                assert!(
                    owner_entry.reserved_bytes >= amount,
                    "{} >= {}",
                    owner_entry.reserved_bytes,
                    amount
                );
                owner_entry.reserved_bytes -= amount;
            }
            None => {
                assert!(
                    self.unattributed_reserved_bytes >= amount,
                    "{} >= {}",
                    self.unattributed_reserved_bytes,
                    amount
                );
                self.unattributed_reserved_bytes -= amount
            }
        };
    }
}

/// A container for a set of extents which are known to be free and can be trimmed.  Returned by
/// `take_for_trimming`.
pub struct TrimmableExtents<'a> {
    allocator: &'a Allocator,
    extents: Vec<Range<u64>>,
    // The allocator can subscribe to this event to wait until these extents are dropped.  This way,
    // we don't fail an allocation attempt if blocks are tied up for trimming; rather, we just wait
    // until the batch is finished with and then proceed.
    _drop_event: DropEvent,
}

impl<'a> TrimmableExtents<'a> {
    pub fn extents(&self) -> &Vec<Range<u64>> {
        &self.extents
    }

    // Also returns an EventListener which is signaled when this is dropped.
    fn new(allocator: &'a Allocator) -> (Self, EventListener) {
        let drop_event = DropEvent::new();
        let listener = drop_event.listen();
        (Self { allocator, extents: vec![], _drop_event: drop_event }, listener)
    }

    fn add_extent(&mut self, extent: Range<u64>) {
        self.extents.push(extent);
    }
}

impl<'a> Drop for TrimmableExtents<'a> {
    fn drop(&mut self) {
        let mut inner = self.allocator.inner.lock().unwrap();
        for device_range in std::mem::take(&mut self.extents) {
            inner.strategy.free(device_range);
        }
        inner.trim_reserved_bytes = 0;
    }
}

impl Allocator {
    pub fn new(filesystem: Arc<FxFilesystem>, object_id: u64) -> Allocator {
        let block_size = filesystem.block_size();
        // We expect device size to be a multiple of block size. Throw away any tail.
        let device_size = round_down(filesystem.device().size(), block_size);
        if device_size != filesystem.device().size() {
            tracing::warn!("Device size is not block aligned. Rounding down.");
        }
        let max_extent_size_bytes = max_extent_size_for_block_size(filesystem.block_size());
        // Note that we use BestFit strategy for new filesystems to favour dense packing of
        // data and 'FirstFit' for existing filesystems for better fragmentation.
        let mut strategy: Box<dyn strategy::AllocatorStrategy> =
            Box::new(strategy::best_fit::BestFit::default());
        strategy.free(0..filesystem.device().size());
        Allocator {
            filesystem: Arc::downgrade(&filesystem),
            block_size,
            device_size,
            object_id,
            max_extent_size_bytes,
            tree: LSMTree::new(merge, Box::new(NullCache {})),
            inner: Mutex::new(Inner {
                info: AllocatorInfo::default(),
                opened: false,
                owner_bytes: BTreeMap::new(),
                unattributed_reserved_bytes: 0,
                committed_deallocated: VecDeque::new(),
                committed_encrypted_volume_deletions: BTreeMap::new(),
                trim_reserved_bytes: 0,
                trim_listener: None,
                strategy,
                histogram: [0; 64],
            }),
            allocation_mutex: futures::lock::Mutex::new(()),
            counters: Mutex::new(AllocatorCounters::default()),
            maximum_offset: AtomicU64::new(0),
        }
    }

    pub fn tree(&self) -> &LSMTree<AllocatorKey, AllocatorValue> {
        &self.tree
    }

    /// Returns an iterator that yields all allocations, filtering out tombstones and any
    /// owner_object_id that have been marked as deleted.
    pub async fn filter(
        &self,
        iter: impl LayerIterator<AllocatorKey, AllocatorValue>,
    ) -> Result<impl LayerIterator<AllocatorKey, AllocatorValue>, Error> {
        let marked_for_deletion = self.inner.lock().unwrap().info.marked_for_deletion.clone();
        let iter =
            filter_marked_for_deletion(filter_tombstones(iter).await?, marked_for_deletion).await?;
        Ok(iter)
    }

    pub fn objects_pending_deletion(&self) -> Vec<u64> {
        self.inner
            .lock()
            .unwrap()
            .committed_encrypted_volume_deletions
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    }

    /// A histogram of allocation request sizes.
    /// The index into the array is 'number of blocks'.
    /// The last bucket is a catch-all for larger allocation requests.
    pub fn histogram(&self) -> [u64; 64] {
        self.inner.lock().unwrap().histogram
    }

    /// Creates a new (empty) allocator.
    pub async fn create(&self, transaction: &mut Transaction<'_>) -> Result<(), Error> {
        // Mark the allocator as opened before creating the file because creating a new
        // transaction requires a reservation.
        assert_eq!(std::mem::replace(&mut self.inner.lock().unwrap().opened, true), false);

        let filesystem = self.filesystem.upgrade().unwrap();
        let root_store = filesystem.root_store();
        ObjectStore::create_object_with_id(
            &root_store,
            transaction,
            self.object_id(),
            HandleOptions::default(),
            None,
            None,
        )
        .await?;
        Ok(())
    }

    // Ensures the allocator is open.  If empty, create the object in the root object store,
    // otherwise load and initialise the LSM tree.  This is not thread-safe; this should be called
    // after the journal has been replayed. Any entries in the LSMTree mutable layer
    // that were written during replay will be preserved.
    pub async fn open(&self) -> Result<(), Error> {
        let filesystem = self.filesystem.upgrade().unwrap();
        let root_store = filesystem.root_store();
        let mut strategy: Box<dyn AllocatorStrategy>;

        let handle =
            ObjectStore::open_object(&root_store, self.object_id, HandleOptions::default(), None)
                .await
                .context("Failed to open allocator object")?;

        if handle.get_size() > 0 {
            let serialized_info = handle
                .contents(MAX_ALLOCATOR_INFO_SERIALIZED_SIZE)
                .await
                .context("Failed to read AllocatorInfo")?;
            let mut cursor = std::io::Cursor::new(serialized_info);
            let (mut info, _version) = AllocatorInfo::deserialize_with_version(&mut cursor)
                .context("Failed to deserialize AllocatorInfo")?;

            // Make sure the allocated_bytes don't exceed the size of the device.
            let mut device_bytes = self.device_size;
            for bytes in info.allocated_bytes.values() {
                ensure!(*bytes <= device_bytes, FxfsError::Inconsistent);
                device_bytes -= bytes;
            }

            let mut handles = Vec::new();
            let mut total_size = 0;
            for object_id in &info.layers {
                let handle = ObjectStore::open_object(
                    &root_store,
                    *object_id,
                    HandleOptions::default(),
                    None,
                )
                .await
                .context("Failed to open allocator layer file")?;
                total_size += handle.get_size();
                handles.push(handle);
            }
            {
                let mut inner = self.inner.lock().unwrap();
                // After replaying, allocated_bytes should include all the deltas since the time
                // the allocator was last flushed, so here we just need to add whatever is
                // recorded in info.
                for (owner_object_id, bytes) in &info.allocated_bytes {
                    let amount: i64 = (*bytes).try_into().map_err(|_| {
                        anyhow!(FxfsError::Inconsistent).context("Allocated bytes inconsistent")
                    })?;
                    let entry = inner.owner_bytes.entry(*owner_object_id).or_default();
                    match entry.allocated_bytes.checked_add(amount) {
                        None => {
                            bail!(anyhow!(FxfsError::Inconsistent)
                                .context("Allocated bytes overflow"));
                        }
                        Some(value) if value < 0 || value as u64 > self.device_size => {
                            bail!(anyhow!(FxfsError::Inconsistent)
                                .context("Allocated bytes inconsistent"));
                        }
                        Some(value) => {
                            entry.allocated_bytes = value;
                        }
                    };
                }
                // Merge in current data which has picked up the deltas on top of the snapshot.
                info.limit_bytes.extend(inner.info.limit_bytes.iter());
                // Don't continue tracking bytes for anything that has been marked for deletion.
                for k in &inner.info.marked_for_deletion {
                    info.limit_bytes.remove(k);
                }
                inner.info = info;
            }
            let layer_file_sizes =
                handles.iter().map(ReadObjectHandle::get_size).collect::<Vec<u64>>();
            self.counters.lock().unwrap().persistent_layer_file_sizes = layer_file_sizes;
            self.tree.append_layers(handles).await.context("Failed to append allocator layers")?;
            self.filesystem.upgrade().unwrap().object_manager().update_reservation(
                self.object_id,
                tree::reservation_amount_from_layer_size(total_size),
            );
            // Use FirstFit for existing filesystems to minimise fragmentation in the face of heavy
            // filesystem churn.
            strategy = Box::new(strategy::first_fit::FirstFit::default());
        } else {
            // Use BestFit for new filesystems so we pack files together without creating gaps.
            // This can help us produce minimal sized filesystem images.
            strategy = Box::new(strategy::best_fit::BestFit::default());
        }
        // Walk all allocations to generate the set of free regions between allocations.
        // This may take some time and consume a potentially large chunk of RAM on on very large,
        // fragmented filesystems. We may circle back here to support caching of free space maps
        // and/or tracking only partial free-space (if the chosen allocation strategy allows it).
        {
            let layer_set = self.tree.layer_set();
            let mut merger = layer_set.merger();
            let mut iter = self.filter(merger.seek(Bound::Unbounded).await?).await?;
            let mut last_offset = 0;
            loop {
                match iter.get() {
                    None => {
                        ensure!(last_offset <= self.device_size, FxfsError::Inconsistent);
                        strategy.free(last_offset..self.device_size);
                        break;
                    }
                    Some(ItemRef { key: AllocatorKey { device_range, .. }, .. }) => {
                        if device_range.start > last_offset {
                            strategy.free(last_offset..device_range.start);
                        }
                        last_offset = device_range.end;
                    }
                }
                iter.advance().await?;
            }
            self.inner.lock().unwrap().strategy = strategy;
        }

        assert_eq!(std::mem::replace(&mut self.inner.lock().unwrap().opened, true), false);
        Ok(())
    }

    /// Collects up to `extents_per_batch` free extents of size up to `max_extent_size` from
    /// `offset`.  The extents will be reserved.
    /// Note that only one `FreeExtents` can exist for the allocator at any time.
    pub async fn take_for_trimming(
        &self,
        offset: u64,
        max_extent_size: usize,
        extents_per_batch: usize,
    ) -> Result<TrimmableExtents<'_>, Error> {
        let _guard = self.allocation_mutex.lock().await;

        let (mut result, listener) = TrimmableExtents::new(self);
        let mut bytes = 0;
        {
            let mut inner = self.inner.lock().unwrap();
            for _ in 0..extents_per_batch {
                if let Some(range) = inner
                    .strategy
                    .allocate(max_extent_size as u64, LocationHint::NextAvailable(offset))
                {
                    result.add_extent(range.clone());
                    bytes += range.length()?;
                } else {
                    break;
                }
            }

            assert!(inner.trim_reserved_bytes == 0, "Multiple trims ongoing");
            inner.trim_listener = Some(listener);
            inner.trim_reserved_bytes = bytes;
            debug_assert!(
                inner.trim_reserved_bytes + inner.unavailable_bytes() <= self.device_size
            );
        }
        Ok(result)
    }

    /// Returns all objects that exist in the parent store that pertain to this allocator.
    pub fn parent_objects(&self) -> Vec<u64> {
        // The allocator tree needs to store a file for each of the layers in the tree, so we return
        // those, since nothing else references them.
        self.inner.lock().unwrap().info.layers.clone()
    }

    /// Returns all the current owner byte limits (in pairs of `(owner_object_id, bytes)`).
    pub fn owner_byte_limits(&self) -> Vec<(u64, u64)> {
        self.inner.lock().unwrap().info.limit_bytes.iter().map(|(k, v)| (*k, *v)).collect()
    }

    /// Returns (allocated_bytes, byte_limit) for the given owner.
    pub fn owner_allocation_info(&self, owner_object_id: u64) -> (u64, Option<u64>) {
        let inner = self.inner.lock().unwrap();
        (
            inner.owner_bytes.get(&owner_object_id).map(|b| b.used_bytes()).unwrap_or(0u64),
            inner.info.limit_bytes.get(&owner_object_id).copied(),
        )
    }

    fn needs_sync(&self) -> bool {
        // TODO(https://fxbug.dev/42178048): This will only trigger if *all* free space is taken up with
        // committed deallocated bytes, but we might want to trigger a sync if we're low and there
        // happens to be a lot of deallocated bytes as that might mean we can fully satisfy
        // allocation requests.
        self.inner.lock().unwrap().unavailable_bytes() >= self.device_size
    }

    fn is_system_store(&self, owner_object_id: u64) -> bool {
        let fs = self.filesystem.upgrade().unwrap();
        owner_object_id == fs.object_manager().root_store_object_id()
            || owner_object_id == fs.object_manager().root_parent_store_object_id()
    }

    /// Updates the accounting to track that a byte reservation has been moved out of an owner to
    /// the unattributed pool.
    pub fn disown_reservation(&self, old_owner_object_id: Option<u64>, amount: u64) {
        if old_owner_object_id.is_none() || amount == 0 {
            return;
        }
        // These 2 mutations should behave as though they're a single atomic mutation.
        let mut inner = self.inner.lock().unwrap();
        inner.remove_reservation(old_owner_object_id, amount);
        inner.add_reservation(None, amount);
    }

    /// Creates a lazy inspect node named `str` under `parent` which will yield statistics for the
    /// allocator when queried.
    pub fn track_statistics(self: &Arc<Self>, parent: &fuchsia_inspect::Node, name: &str) {
        let this = Arc::downgrade(self);
        parent.record_lazy_child(name, move || {
            let this_clone = this.clone();
            async move {
                let inspector = fuchsia_inspect::Inspector::default();
                if let Some(this) = this_clone.upgrade() {
                    let counters = this.counters.lock().unwrap();
                    let root = inspector.root();
                    root.record_uint("max_extent_size_bytes", this.max_extent_size_bytes);
                    root.record_uint("bytes_total", this.device_size);
                    let (allocated, reserved, used, unavailable) = {
                        // TODO(https://fxbug.dev/42069513): Push-back or rate-limit to prevent DoS.
                        let inner = this.inner.lock().unwrap();
                        (
                            inner.allocated_bytes().try_into().unwrap_or(0u64),
                            inner.reserved_bytes(),
                            inner.used_bytes(),
                            inner.unavailable_bytes(),
                        )
                    };
                    root.record_uint("bytes_allocated", allocated);
                    root.record_uint("bytes_reserved", reserved);
                    root.record_uint("bytes_used", used);
                    root.record_uint("bytes_unavailable", unavailable);

                    // TODO(https://fxbug.dev/42068224): Post-compute rather than manually computing
                    // metrics.
                    if let Some(x) = round_div(100 * allocated, this.device_size) {
                        root.record_uint("bytes_allocated_percent", x);
                    }
                    if let Some(x) = round_div(100 * reserved, this.device_size) {
                        root.record_uint("bytes_reserved_percent", x);
                    }
                    if let Some(x) = round_div(100 * used, this.device_size) {
                        root.record_uint("bytes_used_percent", x);
                    }
                    if let Some(x) = round_div(100 * unavailable, this.device_size) {
                        root.record_uint("bytes_unavailable_percent", x);
                    }

                    root.record_uint("num_flushes", counters.num_flushes);
                    if let Some(last_flush_time) = counters.last_flush_time.as_ref() {
                        root.record_uint(
                            "last_flush_time_ms",
                            last_flush_time
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or(std::time::Duration::ZERO)
                                .as_millis()
                                .try_into()
                                .unwrap_or(0u64),
                        );
                    }
                    let sizes = root.create_uint_array(
                        "persistent_layer_file_sizes",
                        counters.persistent_layer_file_sizes.len(),
                    );
                    for i in 0..counters.persistent_layer_file_sizes.len() {
                        sizes.set(i, counters.persistent_layer_file_sizes[i]);
                    }
                    root.record(sizes);

                    let allocator_data = this.histogram();
                    let alloc_sizes =
                        root.create_uint_array("allocation_size_histogram", allocator_data.len());
                    for (i, count) in allocator_data.iter().enumerate() {
                        alloc_sizes.set(i, *count);
                    }
                    root.record(alloc_sizes);
                }
                Ok(inspector)
            }
            .boxed()
        });
    }

    /// Returns the offset of the first byte which has not been used by the allocator since its
    /// creation.
    /// NB: This does *not* take into account existing allocations.  This is only reliable when the
    /// allocator was created from scratch, without any pre-existing allocations.
    pub fn maximum_offset(&self) -> u64 {
        self.maximum_offset.load(Ordering::Relaxed)
    }
}

impl Drop for Allocator {
    fn drop(&mut self) {
        let inner = self.inner.lock().unwrap();
        // Uncommitted and reserved should be released back using RAII, so they should be zero.
        assert_eq!(inner.uncommitted_allocated_bytes(), 0);
        assert_eq!(inner.reserved_bytes(), 0);
    }
}

#[fxfs_trace::trace]
impl Allocator {
    /// Returns the object ID for the allocator.
    pub fn object_id(&self) -> u64 {
        self.object_id
    }

    /// Returns information about the allocator such as the layer files storing persisted
    /// allocations.
    pub fn info(&self) -> AllocatorInfo {
        self.inner.lock().unwrap().info.clone()
    }

    /// Tries to allocate enough space for |object_range| in the specified object and returns the
    /// device range allocated.
    /// The allocated range may be short (e.g. due to fragmentation), in which case the caller can
    /// simply call allocate again until they have enough blocks.
    ///
    /// We also store the object store ID of the store that the allocation should be assigned to so
    /// that we have a means to delete encrypted stores without needing the encryption key.
    #[trace]
    pub async fn allocate(
        &self,
        transaction: &mut Transaction<'_>,
        owner_object_id: u64,
        mut len: u64,
    ) -> Result<Range<u64>, Error> {
        assert_eq!(len % self.block_size, 0);
        len = std::cmp::min(len, self.max_extent_size_bytes);
        debug_assert_ne!(owner_object_id, INVALID_OBJECT_ID);

        // Make sure we have space reserved before we try and find the space.
        let reservation = if let Some(reservation) = transaction.allocator_reservation {
            match reservation.owner_object_id {
                // If there is no owner, this must be a system store that we're allocating for.
                None => assert!(self.is_system_store(owner_object_id)),
                // If it has an owner, it should not be different than the allocating owner.
                Some(res_owner_object_id) => assert_eq!(owner_object_id, res_owner_object_id),
            };
            let r = reservation.reserve_at_most(len);
            len = r.amount();
            Left(r)
        } else {
            let mut inner = self.inner.lock().unwrap();
            assert!(inner.opened);
            // Do not exceed the limit for the owner or the device.
            let device_used = inner.used_bytes();
            let owner_bytes_left = inner.owner_id_bytes_left(owner_object_id);
            // We must take care not to use up space that might be reserved.
            let limit = std::cmp::min(owner_bytes_left, self.device_size - device_used);
            len = round_down(std::cmp::min(len, limit), self.block_size);
            let owner_entry = inner.owner_bytes.entry(owner_object_id).or_default();
            owner_entry.reserved_bytes += len;
            Right(ReservationImpl::<_, Self>::new(self, Some(owner_object_id), len))
        };

        ensure!(len > 0, FxfsError::NoSpace);

        #[allow(clippy::never_loop)] // Loop used as a for {} else {}.
        let _guard = 'sync: loop {
            // Cap number of sync attempts before giving up on finding free space.
            for _ in 0..10 {
                {
                    let guard = self.allocation_mutex.lock().await;

                    if !self.needs_sync() {
                        break 'sync guard;
                    }
                }

                // All the free space is currently tied up with deallocations, so we need to sync
                // and flush the device to free that up.
                //
                // We can't hold the allocation lock whilst we sync here because the allocation lock
                // is also taken in apply_mutations, which is called when journal locks are held,
                // and we call sync here which takes those same locks, so it would have the
                // potential to result in a deadlock.  Sync holds its own lock to guard against
                // multiple syncs occurring at the same time, and we can supply a precondition that
                // is evaluated under that lock to ensure we don't sync twice if we don't need to.
                self.filesystem
                    .upgrade()
                    .unwrap()
                    .sync(SyncOptions {
                        flush_device: true,
                        precondition: Some(Box::new(|| self.needs_sync())),
                        ..Default::default()
                    })
                    .await?;
            }
            bail!(
                anyhow!(FxfsError::NoSpace).context("Sync failed to yield sufficient free space.")
            );
        };

        let mut trim_listener = None;
        {
            let mut inner = self.inner.lock().unwrap();
            inner.histogram[std::cmp::min(63, len / self.block_size) as usize] += 1;

            // If trimming would be the reason that this allocation gets cut short, wait for
            // trimming to complete before proceeding.
            let avail = self
                .device_size
                .checked_sub(inner.unavailable_bytes())
                .ok_or(FxfsError::Inconsistent)?;
            let free_and_not_being_trimmed =
                inner.bytes_available_not_being_trimmed(self.device_size)?;
            if free_and_not_being_trimmed < std::cmp::min(len, avail) {
                debug_assert!(inner.trim_reserved_bytes > 0);
                trim_listener = std::mem::take(&mut inner.trim_listener);
            }
        }

        if let Some(listener) = trim_listener {
            listener.await;
        }

        let result =
            self.inner.lock().unwrap().strategy.allocate(len, LocationHint::None).ok_or_else(
                || {
                    let err = anyhow!(FxfsError::NoSpace)
                        .context("Unexpectedly found no space after search");
                    tracing::error!(%err, "Likely filesystem corruption.");
                    err
                },
            )?;

        debug!(device_range = ?result, "allocate");

        let len = result.length().unwrap();
        let reservation_owner = reservation.either(
            // Left means we got an outside reservation.
            |l| {
                l.forget_some(len);
                l.owner_object_id()
            },
            |r| {
                r.forget_some(len);
                r.owner_object_id()
            },
        );

        {
            let mut inner = self.inner.lock().unwrap();
            let owner_entry = inner.owner_bytes.entry(owner_object_id).or_default();
            owner_entry.uncommitted_allocated_bytes += len;
            // If the reservation has an owner, ensure they are the same.
            assert_eq!(owner_object_id, reservation_owner.unwrap_or(owner_object_id));
            inner.remove_reservation(reservation_owner, len);
        }

        let mutation =
            AllocatorMutation::Allocate { device_range: result.clone().into(), owner_object_id };
        assert!(transaction.add(self.object_id(), Mutation::Allocator(mutation)).is_none());

        Ok(result)
    }

    /// Marks the given device range as allocated.  The main use case for this at this time is for
    /// the super-block which needs to be at a fixed location on the device.
    #[trace]
    pub async fn mark_allocated(
        &self,
        transaction: &mut Transaction<'_>,
        owner_object_id: u64,
        device_range: Range<u64>,
    ) -> Result<(), Error> {
        debug_assert_ne!(owner_object_id, INVALID_OBJECT_ID);
        {
            let len = device_range.length().map_err(|_| FxfsError::InvalidArgs)?;

            let mut inner = self.inner.lock().unwrap();
            let device_used = inner.used_bytes();
            let owner_id_bytes_left = inner.owner_id_bytes_left(owner_object_id);
            let owner_entry = inner.owner_bytes.entry(owner_object_id).or_default();
            ensure!(
                device_range.end <= self.device_size
                    && self.device_size - device_used >= len
                    && owner_id_bytes_left >= len,
                FxfsError::NoSpace
            );
            if let Some(reservation) = &mut transaction.allocator_reservation {
                // The transaction takes ownership of this hold.
                reservation.reserve(len).ok_or(FxfsError::NoSpace)?.forget();
            }
            owner_entry.uncommitted_allocated_bytes += len;
            // Done last to avoid leaking free list entries if we error out.
            ensure!(
                inner
                    .strategy
                    .allocate(
                        device_range.end - device_range.start,
                        LocationHint::Require(device_range.start),
                    )
                    .ok_or(FxfsError::NoSpace)?
                    == device_range,
                FxfsError::Inconsistent
            );
        }
        let mutation =
            AllocatorMutation::Allocate { device_range: device_range.into(), owner_object_id };
        transaction.add(self.object_id(), Mutation::Allocator(mutation));
        Ok(())
    }

    /// Sets the limits for an owner object in terms of usage.
    pub async fn set_bytes_limit(
        &self,
        transaction: &mut Transaction<'_>,
        owner_object_id: u64,
        bytes: u64,
    ) -> Result<(), Error> {
        // System stores cannot be given limits.
        assert!(!self.is_system_store(owner_object_id));
        transaction.add(
            self.object_id(),
            Mutation::Allocator(AllocatorMutation::SetLimit { owner_object_id, bytes }),
        );
        Ok(())
    }

    /// Gets the bytes limit for an owner object.
    pub async fn get_bytes_limit(&self, owner_object_id: u64) -> Option<u64> {
        self.inner.lock().unwrap().info.limit_bytes.get(&owner_object_id).copied()
    }

    /// Deallocates the given device range for the specified object.
    #[trace]
    pub async fn deallocate(
        &self,
        transaction: &mut Transaction<'_>,
        owner_object_id: u64,
        dealloc_range: Range<u64>,
    ) -> Result<u64, Error> {
        debug!(device_range = ?dealloc_range, "deallocate");
        ensure!(dealloc_range.is_valid(), FxfsError::InvalidArgs);
        // We don't currently support sharing of allocations (value.count always equals 1), so as
        // long as we can assume the deallocated range is actually allocated, we can avoid device
        // access.
        let deallocated = dealloc_range.end - dealloc_range.start;
        let mutation =
            AllocatorMutation::Deallocate { device_range: dealloc_range.into(), owner_object_id };
        transaction.add(self.object_id(), Mutation::Allocator(mutation));
        Ok(deallocated)
    }

    /// Marks allocations associated with a given |owner_object_id| for deletion.
    ///
    /// This is used as part of deleting encrypted volumes (ObjectStore) without having the keys.
    ///
    /// MarkForDeletion mutations eventually manipulates allocator metadata (AllocatorInfo) instead
    /// of the mutable layer but we must be careful not to do this too early and risk premature
    /// reuse of extents.
    ///
    /// Applying the mutation moves byte count for the owner_object_id from 'allocated_bytes' to
    /// 'committed_encrypted_volume_deletions'.
    ///
    /// Replay is not guaranteed until the *device* gets flushed, so we cannot reuse the deleted
    /// extents until we receive a `did_flush_device` callback.
    ///
    /// At this point, the mutation is guaranteed so the 'committed_encrypted_volume_deletions'
    /// entry is removed and the owner_object_id is added to the 'marked_for_deletion' set. This set
    /// of owner_object_id are filtered out of all iterators used by the allocator.
    ///
    /// Since this was first written, an allocator free list has been introduced which muddies this
    /// otherwise clean implementation. Because we now need to add all the extents that are filtered
    /// out to the free list, we don't get away without at least a read over the LSMTree at this
    /// point in order to add the newly freed extents.
    ///
    /// TODO(b/316827348): Consider removing the use of mark_for_deletion in AllocatorInfo and
    /// just compacting?
    ///
    /// After an allocator.flush() (i.e. a major compaction), we know that there is no data left
    /// in the layer files for this owner_object_id and we are able to clear `marked_for_deletion`.
    pub async fn mark_for_deletion(&self, transaction: &mut Transaction<'_>, owner_object_id: u64) {
        // Note that because the actual time of deletion (the next major compaction) is undefined,
        // |owner_object_id| should not be reused after this call.
        transaction.add(
            self.object_id(),
            Mutation::Allocator(AllocatorMutation::MarkForDeletion(owner_object_id)),
        );
    }

    /// Called when the device has been flush and indicates what the journal log offset was when
    /// that happened.
    pub async fn did_flush_device(&self, flush_log_offset: u64) {
        // First take out the deallocations that we now know to be flushed.  The list is maintained
        // in order, so we can stop on the first entry that we find that should not be unreserved
        // yet.
        #[allow(clippy::never_loop)] // Loop used as a for {} else {}.
        let deallocs = 'deallocs_outer: loop {
            let mut inner = self.inner.lock().unwrap();
            for (index, dealloc) in inner.committed_deallocated.iter().enumerate() {
                if dealloc.log_file_offset >= flush_log_offset {
                    let mut deallocs = inner.committed_deallocated.split_off(index);
                    // Swap because we want the opposite of what split_off does.
                    std::mem::swap(&mut inner.committed_deallocated, &mut deallocs);
                    break 'deallocs_outer deallocs;
                }
            }
            break std::mem::take(&mut inner.committed_deallocated);
        };

        // We also have to scan and add to this set any ranges that should be added to our free list
        // (inner.strategy).
        let committed_encrypted_volume_deletions =
            std::mem::take(&mut self.inner.lock().unwrap().committed_encrypted_volume_deletions);
        if committed_encrypted_volume_deletions.len() != 0 {
            let mut ranges = Vec::new();
            let layer_set = self.tree.layer_set();
            let mut merger = layer_set.merger();
            let mut iter = self.filter(merger.seek(Bound::Unbounded).await.unwrap()).await.unwrap();
            loop {
                match iter.get() {
                    None => {
                        break;
                    }
                    Some(ItemRef {
                        key: AllocatorKey { device_range },
                        value: AllocatorValue::Abs { owner_object_id, .. },
                        ..
                    }) => {
                        if let Some((log_offset, _bytes)) =
                            committed_encrypted_volume_deletions.get(&owner_object_id)
                        {
                            if *log_offset < flush_log_offset {
                                ranges.push(device_range.clone());
                            }
                        }
                    }
                    _ => unreachable!(), // self.filter() should ensure we don't see this.
                }
                iter.advance().await.unwrap();
            }
            let mut inner = self.inner.lock().unwrap();
            for range in ranges {
                inner.strategy.free(range);
            }
        }

        // Now we can free those elements.
        let mut inner = self.inner.lock().unwrap();
        let mut totals = BTreeMap::<u64, u64>::new();
        for dealloc in deallocs {
            *(totals.entry(dealloc.owner_object_id).or_default()) +=
                dealloc.range.length().unwrap();
            inner.strategy.free(dealloc.range);
        }

        // This *must* come after we've removed the records from reserved reservations because the
        // allocator uses this value to decide whether or not a device-flush is required and it must
        // be possible to find free space if it thinks no device-flush is required.
        for (owner_object_id, total) in totals {
            match inner.owner_bytes.get_mut(&owner_object_id) {
                Some(counters) => counters.committed_deallocated_bytes -= total,
                None => panic!("Failed to decrement for unknown owner: {}", owner_object_id),
            }
        }

        // We can now reuse any marked_for_deletion extents that have been committed to journal.
        for (owner_object_id, (log_offset, bytes)) in committed_encrypted_volume_deletions {
            if log_offset >= flush_log_offset {
                inner
                    .committed_encrypted_volume_deletions
                    .insert(owner_object_id, (log_offset, bytes));
            } else {
                inner.info.marked_for_deletion.insert(owner_object_id);
            }
            // After the journal is fulled replayed, anything marked_for_deletion should stop being
            // tracked in memory so that it will not show up in the next write of AllocatorInfo.
            inner.owner_bytes.remove(&owner_object_id);
        }
    }

    /// Returns a reservation that can be used later, or None if there is insufficient space. The
    /// |owner_object_id| indicates which object in the root object store the reservation is for.
    pub fn reserve(
        self: Arc<Self>,
        owner_object_id: Option<u64>,
        amount: u64,
    ) -> Option<Reservation> {
        {
            let mut inner = self.inner.lock().unwrap();

            let limit = match owner_object_id {
                Some(id) => std::cmp::min(
                    inner.owner_id_bytes_left(id),
                    self.device_size - inner.used_bytes(),
                ),
                None => self.device_size - inner.used_bytes(),
            };
            if limit < amount {
                return None;
            }
            inner.add_reservation(owner_object_id, amount);
        }
        Some(Reservation::new(self, owner_object_id, amount))
    }

    /// Like reserve, but returns as much as available if not all of amount is available, which
    /// could be zero bytes.
    pub fn reserve_at_most(
        self: Arc<Self>,
        owner_object_id: Option<u64>,
        mut amount: u64,
    ) -> Reservation {
        {
            let mut inner = self.inner.lock().unwrap();
            let limit = match owner_object_id {
                Some(id) => std::cmp::min(
                    inner.owner_id_bytes_left(id),
                    self.device_size - inner.used_bytes(),
                ),
                None => self.device_size - inner.used_bytes(),
            };
            amount = std::cmp::min(limit, amount);
            inner.add_reservation(owner_object_id, amount);
        }
        Reservation::new(self, owner_object_id, amount)
    }

    /// Returns the total number of allocated bytes.
    pub fn get_allocated_bytes(&self) -> u64 {
        self.inner.lock().unwrap().allocated_bytes() as u64
    }

    /// Returns the size of bytes available to allocate.
    pub fn get_disk_bytes(&self) -> u64 {
        self.device_size
    }

    /// Returns the total number of allocated bytes per owner_object_id.
    /// Note that this is quite an expensive operation as it copies the collection.
    /// This is intended for use in fsck() and friends, not general use code.
    pub fn get_owner_allocated_bytes(&self) -> BTreeMap<u64, i64> {
        self.inner
            .lock()
            .unwrap()
            .owner_bytes
            .iter()
            .map(|(k, v)| (*k, v.allocated_bytes))
            .collect()
    }

    /// Returns the number of allocated and reserved bytes.
    pub fn get_used_bytes(&self) -> u64 {
        let inner = self.inner.lock().unwrap();
        inner.used_bytes()
    }
}

impl ReservationOwner for Allocator {
    fn release_reservation(&self, owner_object_id: Option<u64>, amount: u64) {
        self.inner.lock().unwrap().remove_reservation(owner_object_id, amount);
    }
}

#[async_trait]
impl JournalingObject for Allocator {
    async fn apply_mutation(
        &self,
        mutation: Mutation,
        context: &ApplyContext<'_, '_>,
        _assoc_obj: AssocObj<'_>,
    ) -> Result<(), Error> {
        match mutation {
            Mutation::Allocator(AllocatorMutation::MarkForDeletion(owner_object_id)) => {
                let mut inner = self.inner.lock().unwrap();
                // If live, we haven't serialized this yet so we track the commitment in RAM.
                // If we're replaying the journal, we know this is already on storage and
                // MUST happen so this will be cleared out of AllocatorInfo at next allocator flush.
                let old_allocated_bytes =
                    inner.owner_bytes.remove(&owner_object_id).map(|v| v.allocated_bytes);
                if let Some(old_bytes) = old_allocated_bytes {
                    inner.committed_encrypted_volume_deletions.insert(
                        owner_object_id,
                        (context.checkpoint.file_offset, old_bytes as u64),
                    );
                }
                inner.info.limit_bytes.remove(&owner_object_id);
            }
            Mutation::Allocator(AllocatorMutation::Allocate { device_range, owner_object_id }) => {
                self.maximum_offset.fetch_max(device_range.end, Ordering::Relaxed);
                let item = AllocatorItem {
                    key: AllocatorKey { device_range: device_range.clone().into() },
                    value: AllocatorValue::Abs { count: 1, owner_object_id },
                    sequence: context.checkpoint.file_offset,
                };
                let len = item.key.device_range.length().unwrap();
                let lower_bound = item.key.lower_bound_for_merge_into();
                self.tree.merge_into(item, &lower_bound).await;
                let mut inner = self.inner.lock().unwrap();
                let entry = inner.owner_bytes.entry(owner_object_id).or_default();
                entry.allocated_bytes = entry.allocated_bytes.saturating_add(len as i64);
                if let ApplyMode::Live(transaction) = context.mode {
                    entry.uncommitted_allocated_bytes -= len;
                    if let Some(reservation) = transaction.allocator_reservation {
                        reservation.commit(len);
                    }
                }
            }
            Mutation::Allocator(AllocatorMutation::Deallocate {
                device_range,
                owner_object_id,
            }) => {
                let item = AllocatorItem {
                    key: AllocatorKey { device_range: device_range.clone().into() },
                    value: AllocatorValue::None,
                    sequence: context.checkpoint.file_offset,
                };
                let len = item.key.device_range.length().unwrap();

                {
                    let mut inner = self.inner.lock().unwrap();
                    {
                        let entry = inner.owner_bytes.entry(owner_object_id).or_default();
                        entry.allocated_bytes = entry.allocated_bytes.saturating_sub(len as i64);
                        if context.mode.is_live() {
                            entry.committed_deallocated_bytes += len;
                        }
                    }
                    if context.mode.is_live() {
                        inner.committed_deallocated.push_back(CommittedDeallocation {
                            log_file_offset: context.checkpoint.file_offset,
                            range: item.key.device_range.clone(),
                            owner_object_id,
                        });
                    }
                    if let ApplyMode::Live(Transaction {
                        allocator_reservation: Some(reservation),
                        ..
                    }) = context.mode
                    {
                        inner.add_reservation(reservation.owner_object_id(), len);
                        reservation.add(len);
                    }
                }
                let lower_bound = item.key.lower_bound_for_merge_into();
                self.tree.merge_into(item, &lower_bound).await;
            }
            Mutation::Allocator(AllocatorMutation::SetLimit { owner_object_id, bytes }) => {
                // Journal replay is ordered and each of these calls is idempotent. So the last one
                // will be respected, it doesn't matter if the value is already set, or gets changed
                // multiple times during replay. When it gets opened it will be merged in with the
                // snapshot.
                self.inner.lock().unwrap().info.limit_bytes.insert(owner_object_id, bytes);
            }
            Mutation::BeginFlush => {
                {
                    // After we seal the tree, we will start adding mutations to the new mutable
                    // layer, but we cannot safely do that whilst we are attempting to allocate
                    // because there is a chance it might miss an allocation and also not see the
                    // allocation in temporary_allocations.
                    let _guard = debug_assert_not_too_long!(self.allocation_mutex.lock());
                    self.tree.seal().await;
                }
                // Transfer our running count for allocated_bytes so that it gets written to the new
                // info file when flush completes.
                let mut inner = self.inner.lock().unwrap();
                let allocated_bytes =
                    inner.owner_bytes.iter().map(|(k, v)| (*k, v.allocated_bytes as u64)).collect();
                inner.info.allocated_bytes = allocated_bytes;
            }
            Mutation::EndFlush => {
                if context.mode.is_replay() {
                    self.tree.reset_immutable_layers();

                    // AllocatorInfo is written in the same transaction and will contain the count
                    // at the point BeginFlush was applied, so we need to adjust allocated_bytes so
                    // that it just covers the delta from that point.  Later, when we properly open
                    // the allocator, we'll add this back.
                    let mut inner = self.inner.lock().unwrap();
                    let allocated_bytes: Vec<(u64, i64)> =
                        inner.info.allocated_bytes.iter().map(|(k, v)| (*k, *v as i64)).collect();
                    for (k, v) in allocated_bytes {
                        let entry = inner.owner_bytes.entry(k).or_default();
                        entry.allocated_bytes -= v as i64;
                    }
                }
            }
            _ => bail!("unexpected mutation: {:?}", mutation),
        }
        Ok(())
    }

    fn drop_mutation(&self, mutation: Mutation, transaction: &Transaction<'_>) {
        match mutation {
            Mutation::Allocator(AllocatorMutation::Allocate { device_range, owner_object_id }) => {
                let len = device_range.length().unwrap();
                let mut inner = self.inner.lock().unwrap();
                inner
                    .owner_bytes
                    .entry(owner_object_id)
                    .or_default()
                    .uncommitted_allocated_bytes -= len;
                if let Some(reservation) = transaction.allocator_reservation {
                    let res_owner = reservation.owner_object_id();
                    inner.add_reservation(res_owner, len);
                    reservation.release_reservation(res_owner, len);
                }
                inner.strategy.free(device_range.clone().into());
            }
            _ => {}
        }
    }

    async fn flush(&self) -> Result<Version, Error> {
        let filesystem = self.filesystem.upgrade().unwrap();
        let object_manager = filesystem.object_manager();
        if !object_manager.needs_flush(self.object_id()) {
            // Early exit, but still return the earliest version used by a struct in the tree
            return Ok(self.tree.get_earliest_version());
        }

        let keys = lock_keys![LockKey::flush(self.object_id())];
        let _guard = filesystem.lock_manager().write_lock(keys).await;

        let reservation = object_manager.metadata_reservation();
        let txn_options = Options {
            skip_journal_checks: true,
            borrow_metadata_space: true,
            allocator_reservation: Some(reservation),
            ..Default::default()
        };
        let mut transaction = filesystem.clone().new_transaction(lock_keys![], txn_options).await?;

        let root_store = self.filesystem.upgrade().unwrap().root_store();
        let layer_object_handle = ObjectStore::create_object(
            &root_store,
            &mut transaction,
            HandleOptions { skip_journal_checks: true, ..Default::default() },
            None,
            None,
        )
        .await?;
        let object_id = layer_object_handle.object_id();
        root_store.add_to_graveyard(&mut transaction, object_id);
        // It's important that this transaction does not include any allocations because we use
        // BeginFlush as a snapshot point for mutations to the tree: other allocator mutations
        // within this transaction might get applied before seal (which would be OK), but they could
        // equally get applied afterwards (since Transaction makes no guarantees about the order in
        // which mutations are applied whilst committing), in which case they'd get lost on replay
        // because the journal will only send mutations that follow this transaction.
        transaction.add(self.object_id(), Mutation::BeginFlush);
        transaction.commit().await?;

        let layer_set = self.tree.immutable_layer_set();
        {
            let mut merger = layer_set.merger();
            let iter = self.filter(merger.seek(Bound::Unbounded).await?).await?;
            let iter = CoalescingIterator::new(iter).await?;
            self.tree
                .compact_with_iterator(
                    iter,
                    DirectWriter::new(&layer_object_handle, txn_options).await,
                    layer_object_handle.block_size(),
                )
                .await?;
        }

        // Both of these forward-declared variables need to outlive the transaction.
        let object_handle;
        let reservation_update;
        let mut transaction = filesystem
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(root_store.store_object_id(), self.object_id())],
                txn_options,
            )
            .await?;
        let mut serialized_info = Vec::new();

        debug!(oid = object_id, "new allocator layer file");
        object_handle =
            ObjectStore::open_object(&root_store, self.object_id(), HandleOptions::default(), None)
                .await?;

        // We must be careful to take a copy AllocatorInfo here rather than manipulate the
        // live one. If we remove marked_for_deletion entries prematurely, we may fail any
        // allocate() calls that are performed before the new version makes it to disk.
        // Specifically, txn_write() below must allocate space and may fail if we prematurely
        // clear marked_for_deletion.
        let new_info = {
            let mut info = self.inner.lock().unwrap().info.clone();

            // After compaction, all new layers have marked_for_deletion objects removed.
            info.marked_for_deletion.clear();

            // Move all the existing layers to the graveyard.
            for object_id in &info.layers {
                root_store.add_to_graveyard(&mut transaction, *object_id);
            }

            info.layers = vec![object_id];
            info
        };
        new_info.serialize_with_version(&mut serialized_info)?;

        let mut buf = object_handle.allocate_buffer(serialized_info.len()).await;
        buf.as_mut_slice()[..serialized_info.len()].copy_from_slice(&serialized_info[..]);
        object_handle.txn_write(&mut transaction, 0u64, buf.as_ref()).await?;

        reservation_update = ReservationUpdate::new(tree::reservation_amount_from_layer_size(
            layer_object_handle.get_size(),
        ));
        let layer_file_sizes = vec![layer_object_handle.get_size()];

        // It's important that EndFlush is in the same transaction that we write AllocatorInfo,
        // because we use EndFlush to make the required adjustments to allocated_bytes.
        transaction.add_with_object(
            self.object_id(),
            Mutation::EndFlush,
            AssocObj::Borrowed(&reservation_update),
        );
        root_store.remove_from_graveyard(&mut transaction, object_id);

        let layers = layers_from_handles([layer_object_handle]).await?;
        transaction
            .commit_with_callback(|_| {
                self.tree.set_layers(layers);

                // At this point we've committed the new layers to disk so we can start using them.
                // This means we can also switch to the new AllocatorInfo which clears
                // marked_for_deletion.
                self.inner.lock().unwrap().info = new_info;
            })
            .await?;

        // Now close the layers and purge them.
        for layer in layer_set.layers {
            let object_id = layer.handle().map(|h| h.object_id());
            layer.close_layer().await;
            if let Some(object_id) = object_id {
                root_store.tombstone_object(object_id, txn_options).await?;
            }
        }

        let mut counters = self.counters.lock().unwrap();
        counters.num_flushes += 1;
        counters.last_flush_time = Some(std::time::SystemTime::now());
        counters.persistent_layer_file_sizes = layer_file_sizes;
        // Return the earliest version used by a struct in the tree
        Ok(self.tree.get_earliest_version())
    }
}

// The merger is unable to merge extents that exist like the following:
//
//     |----- +1 -----|
//                    |----- -1 -----|
//                    |----- +2 -----|
//
// It cannot coalesce them because it has to emit the +1 record so that it can move on and merge the
// -1 and +2 records. To address this, we add another stage that applies after merging which
// coalesces records after they have been emitted.  This is a bit simpler than merging because the
// records cannot overlap, so it's just a question of merging adjacent records if they happen to
// have the same delta and object_id.

pub struct CoalescingIterator<I> {
    iter: I,
    item: Option<AllocatorItem>,
}

impl<I: LayerIterator<AllocatorKey, AllocatorValue>> CoalescingIterator<I> {
    pub async fn new(iter: I) -> Result<CoalescingIterator<I>, Error> {
        let mut iter = Self { iter, item: None };
        iter.advance().await?;
        Ok(iter)
    }
}

#[async_trait]
impl<I: LayerIterator<AllocatorKey, AllocatorValue>> LayerIterator<AllocatorKey, AllocatorValue>
    for CoalescingIterator<I>
{
    async fn advance(&mut self) -> Result<(), Error> {
        self.item = self.iter.get().map(|x| x.cloned());
        if self.item.is_none() {
            return Ok(());
        }
        let left = self.item.as_mut().unwrap();
        loop {
            self.iter.advance().await?;
            match self.iter.get() {
                None => return Ok(()),
                Some(right) => {
                    // The two records cannot overlap.
                    ensure!(
                        left.key.device_range.end <= right.key.device_range.start,
                        FxfsError::Inconsistent
                    );
                    // We can only coalesce records if they are touching and have the same value.
                    if left.key.device_range.end < right.key.device_range.start
                        || left.value != *right.value
                    {
                        return Ok(());
                    }
                    left.key.device_range.end = right.key.device_range.end;
                }
            }
        }
    }

    fn get(&self) -> Option<ItemRef<'_, AllocatorKey, AllocatorValue>> {
        self.item.as_ref().map(|x| x.as_item_ref())
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::{
            filesystem::{FxFilesystem, FxFilesystemBuilder, JournalingObject, OpenFxFilesystem},
            lsm_tree::{
                cache::NullCache,
                skip_list_layer::SkipListLayer,
                types::{Item, ItemRef, Layer, LayerIterator, MutableLayer},
                LSMTree,
            },
            object_store::{
                allocator::{
                    merge::merge, Allocator, AllocatorKey, AllocatorValue, CoalescingIterator,
                },
                transaction::{lock_keys, Options},
            },
            range::RangeExt,
        },
        fuchsia_async as fasync,
        std::{
            cmp::{max, min},
            ops::{Bound, Range},
            sync::{Arc, Mutex},
        },
        storage_device::{fake_device::FakeDevice, DeviceHolder},
    };

    #[fuchsia::test]
    async fn test_coalescing_iterator() {
        let skip_list = SkipListLayer::new(100);
        let items = [
            Item::new(
                AllocatorKey { device_range: 0..100 },
                AllocatorValue::Abs { count: 1, owner_object_id: 99 },
            ),
            Item::new(
                AllocatorKey { device_range: 100..200 },
                AllocatorValue::Abs { count: 1, owner_object_id: 99 },
            ),
        ];
        skip_list.insert(items[1].clone()).await.expect("insert error");
        skip_list.insert(items[0].clone()).await.expect("insert error");
        let mut iter =
            CoalescingIterator::new(skip_list.seek(Bound::Unbounded).await.expect("seek failed"))
                .await
                .expect("new failed");
        let ItemRef { key, value, .. } = iter.get().expect("get failed");
        assert_eq!(
            (key, value),
            (
                &AllocatorKey { device_range: 0..200 },
                &AllocatorValue::Abs { count: 1, owner_object_id: 99 }
            )
        );
        iter.advance().await.expect("advance failed");
        assert!(iter.get().is_none());
    }

    #[fuchsia::test]
    async fn test_merge_and_coalesce_across_three_layers() {
        let lsm_tree = LSMTree::new(merge, Box::new(NullCache {}));
        lsm_tree
            .insert(Item::new(
                AllocatorKey { device_range: 100..200 },
                AllocatorValue::Abs { count: 1, owner_object_id: 99 },
            ))
            .await
            .expect("insert error");
        lsm_tree.seal().await;
        lsm_tree
            .insert(Item::new(
                AllocatorKey { device_range: 0..100 },
                AllocatorValue::Abs { count: 1, owner_object_id: 99 },
            ))
            .await
            .expect("insert error");

        let layer_set = lsm_tree.layer_set();
        let mut merger = layer_set.merger();
        let mut iter =
            CoalescingIterator::new(merger.seek(Bound::Unbounded).await.expect("seek failed"))
                .await
                .expect("new failed");
        let ItemRef { key, value, .. } = iter.get().expect("get failed");
        assert_eq!(
            (key, value),
            (
                &AllocatorKey { device_range: 0..200 },
                &AllocatorValue::Abs { count: 1, owner_object_id: 99 }
            )
        );
        iter.advance().await.expect("advance failed");
        assert!(iter.get().is_none());
    }

    #[fuchsia::test]
    async fn test_merge_and_coalesce_wont_merge_across_object_id() {
        let lsm_tree = LSMTree::new(merge, Box::new(NullCache {}));
        lsm_tree
            .insert(Item::new(
                AllocatorKey { device_range: 100..200 },
                AllocatorValue::Abs { count: 1, owner_object_id: 99 },
            ))
            .await
            .expect("insert error");
        lsm_tree.seal().await;
        lsm_tree
            .insert(Item::new(
                AllocatorKey { device_range: 0..100 },
                AllocatorValue::Abs { count: 1, owner_object_id: 98 },
            ))
            .await
            .expect("insert error");

        let layer_set = lsm_tree.layer_set();
        let mut merger = layer_set.merger();
        let mut iter =
            CoalescingIterator::new(merger.seek(Bound::Unbounded).await.expect("seek failed"))
                .await
                .expect("new failed");
        let ItemRef { key, value, .. } = iter.get().expect("get failed");
        assert_eq!(
            (key, value),
            (
                &AllocatorKey { device_range: 0..100 },
                &AllocatorValue::Abs { count: 1, owner_object_id: 98 },
            )
        );
        iter.advance().await.expect("advance failed");
        let ItemRef { key, value, .. } = iter.get().expect("get failed");
        assert_eq!(
            (key, value),
            (
                &AllocatorKey { device_range: 100..200 },
                &AllocatorValue::Abs { count: 1, owner_object_id: 99 }
            )
        );
        iter.advance().await.expect("advance failed");
        assert!(iter.get().is_none());
    }

    fn overlap(a: &Range<u64>, b: &Range<u64>) -> u64 {
        if a.end > b.start && a.start < b.end {
            min(a.end, b.end) - max(a.start, b.start)
        } else {
            0
        }
    }

    async fn collect_allocations(allocator: &Allocator) -> Vec<Range<u64>> {
        let layer_set = allocator.tree.layer_set();
        let mut merger = layer_set.merger();
        let mut iter = allocator
            .filter(merger.seek(Bound::Unbounded).await.expect("seek failed"))
            .await
            .expect("build iterator");
        let mut allocations: Vec<Range<u64>> = Vec::new();
        while let Some(ItemRef { key: AllocatorKey { device_range }, .. }) = iter.get() {
            if let Some(r) = allocations.last() {
                assert!(device_range.start >= r.end);
            }
            allocations.push(device_range.clone());
            iter.advance().await.expect("advance failed");
        }
        allocations
    }

    async fn check_allocations(allocator: &Allocator, expected_allocations: &[Range<u64>]) {
        let layer_set = allocator.tree.layer_set();
        let mut merger = layer_set.merger();
        let mut iter = allocator
            .filter(merger.seek(Bound::Unbounded).await.expect("seek failed"))
            .await
            .expect("build iterator");
        let mut found = 0;
        while let Some(ItemRef { key: AllocatorKey { device_range }, .. }) = iter.get() {
            let mut l = device_range.length().expect("Invalid range");
            found += l;
            // Make sure that the entire range we have found completely overlaps with all the
            // allocations we expect to find.
            for range in expected_allocations {
                l -= overlap(range, device_range);
                if l == 0 {
                    break;
                }
            }
            assert_eq!(l, 0, "range {device_range:?} not covered by expectations");
            iter.advance().await.expect("advance failed");
        }
        // Make sure the total we found adds up to what we expect.
        assert_eq!(found, expected_allocations.iter().map(|r| r.length().unwrap()).sum::<u64>());
    }

    async fn test_fs() -> (OpenFxFilesystem, Arc<Allocator>) {
        let device = DeviceHolder::new(FakeDevice::new(4096, 4096));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let allocator = fs.allocator();
        (fs, allocator)
    }

    #[fuchsia::test]
    async fn test_allocations() {
        const STORE_OBJECT_ID: u64 = 99;
        let (fs, allocator) = test_fs().await;
        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        let mut device_ranges = collect_allocations(&allocator).await;
        device_ranges.push(
            allocator
                .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                .await
                .expect("allocate failed"),
        );
        assert_eq!(device_ranges.last().unwrap().length().expect("Invalid range"), fs.block_size(),);
        device_ranges.push(
            allocator
                .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                .await
                .expect("allocate failed"),
        );
        assert_eq!(device_ranges.last().unwrap().length().expect("Invalid range"), fs.block_size(),);
        assert_eq!(overlap(&device_ranges[0], &device_ranges[1]), 0);
        transaction.commit().await.expect("commit failed");
        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        device_ranges.push(
            allocator
                .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                .await
                .expect("allocate failed"),
        );
        assert_eq!(device_ranges[2].length().unwrap(), fs.block_size());
        assert_eq!(overlap(&device_ranges[0], &device_ranges[2]), 0);
        assert_eq!(overlap(&device_ranges[1], &device_ranges[2]), 0);
        transaction.commit().await.expect("commit failed");

        check_allocations(&allocator, &device_ranges).await;
    }

    #[fuchsia::test]
    async fn test_allocate_more_than_max_size() {
        const STORE_OBJECT_ID: u64 = 99;
        let (fs, allocator) = test_fs().await;
        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        let mut device_ranges = collect_allocations(&allocator).await;
        device_ranges.push(
            allocator
                .allocate(&mut transaction, STORE_OBJECT_ID, fs.device().size())
                .await
                .expect("allocate failed"),
        );
        assert_eq!(
            device_ranges.last().unwrap().length().expect("Invalid range"),
            allocator.max_extent_size_bytes
        );
        transaction.commit().await.expect("commit failed");

        check_allocations(&allocator, &device_ranges).await;
    }

    #[fuchsia::test]
    async fn test_deallocations() {
        const STORE_OBJECT_ID: u64 = 99;
        let (fs, allocator) = test_fs().await;
        let initial_allocations = collect_allocations(&allocator).await;

        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        let device_range1 = allocator
            .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
            .await
            .expect("allocate failed");
        assert_eq!(device_range1.length().expect("Invalid range"), fs.block_size());
        transaction.commit().await.expect("commit failed");

        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        allocator
            .deallocate(&mut transaction, STORE_OBJECT_ID, device_range1)
            .await
            .expect("deallocate failed");
        transaction.commit().await.expect("commit failed");

        check_allocations(&allocator, &initial_allocations).await;
    }

    #[fuchsia::test]
    async fn test_mark_allocated() {
        const STORE_OBJECT_ID: u64 = 99;
        let (fs, allocator) = test_fs().await;
        let mut device_ranges = collect_allocations(&allocator).await;
        let range = {
            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new failed");
            // First, allocate 2 blocks.
            allocator
                .allocate(&mut transaction, STORE_OBJECT_ID, 2 * fs.block_size())
                .await
                .expect("allocate failed")
            // Let the transaction drop.
        };

        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");

        // If we allocate 1 block, the two blocks that were allocated earlier should be available,
        // and this should return the first of them.
        device_ranges.push(
            allocator
                .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                .await
                .expect("allocate failed"),
        );

        assert_eq!(device_ranges.last().unwrap().start, range.start);

        // Mark the second block as allocated.
        let mut range2 = range.clone();
        range2.start += fs.block_size();
        allocator
            .mark_allocated(&mut transaction, STORE_OBJECT_ID, range2.clone())
            .await
            .expect("mark_allocated failed");
        device_ranges.push(range2);

        // This should avoid the range we marked as allocated.
        device_ranges.push(
            allocator
                .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                .await
                .expect("allocate failed"),
        );
        let last_range = device_ranges.last().unwrap();
        assert_eq!(last_range.length().expect("Invalid range"), fs.block_size());
        assert_eq!(overlap(last_range, &range), 0);
        transaction.commit().await.expect("commit failed");

        check_allocations(&allocator, &device_ranges).await;
    }

    #[fuchsia::test]
    async fn test_mark_for_deletion() {
        const STORE_OBJECT_ID: u64 = 99;
        let (fs, allocator) = test_fs().await;

        // Allocate some stuff.
        let initial_allocated_bytes = allocator.get_allocated_bytes();
        let mut device_ranges = collect_allocations(&allocator).await;
        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        // Note we have a cap on individual allocation length so we allocate over multiple mutation.
        for _ in 0..15 {
            device_ranges.push(
                allocator
                    .allocate(&mut transaction, STORE_OBJECT_ID, 100 * fs.block_size())
                    .await
                    .expect("allocate failed"),
            );
            device_ranges.push(
                allocator
                    .allocate(&mut transaction, STORE_OBJECT_ID, 100 * fs.block_size())
                    .await
                    .expect("allocate2 failed"),
            );
        }
        transaction.commit().await.expect("commit failed");
        check_allocations(&allocator, &device_ranges).await;

        assert_eq!(
            allocator.get_allocated_bytes(),
            initial_allocated_bytes + fs.block_size() * 3000
        );

        // Mark for deletion.
        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        allocator.mark_for_deletion(&mut transaction, STORE_OBJECT_ID).await;
        transaction.commit().await.expect("commit failed");

        // Expect that allocated bytes is updated immediately but device ranges are still allocated.
        assert_eq!(allocator.get_allocated_bytes(), initial_allocated_bytes);
        check_allocations(&allocator, &device_ranges).await;

        // Allocate more space than we have until we deallocate the mark_for_deletion space.
        // This should force a flush on allocate(). (1500 * 3 > test_fs size of 4096 blocks).
        device_ranges.clear();

        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        let target_bytes = 1500 * fs.block_size();
        while device_ranges.iter().map(|x| x.length().unwrap()).sum::<u64>() != target_bytes {
            let len = std::cmp::min(
                target_bytes - device_ranges.iter().map(|x| x.length().unwrap()).sum::<u64>(),
                100 * fs.block_size(),
            );
            device_ranges.push(
                allocator.allocate(&mut transaction, 100, len).await.expect("allocate failed"),
            );
        }
        transaction.commit().await.expect("commit failed");

        // Have the deleted ranges cleaned up.
        allocator.flush().await.expect("flush failed");

        // The flush above seems to trigger an allocation for the allocator itself.
        // We will just check that we have the right size for the owner we care about.

        assert!(!allocator.get_owner_allocated_bytes().contains_key(&STORE_OBJECT_ID));
        assert_eq!(*allocator.get_owner_allocated_bytes().get(&100).unwrap() as u64, target_bytes,);
    }

    #[fuchsia::test]
    async fn test_allocate_free_reallocate() {
        const STORE_OBJECT_ID: u64 = 99;
        let (fs, allocator) = test_fs().await;

        // Allocate some stuff.
        let mut device_ranges = Vec::new();
        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        for _ in 0..30 {
            device_ranges.push(
                allocator
                    .allocate(&mut transaction, STORE_OBJECT_ID, 100 * fs.block_size())
                    .await
                    .expect("allocate failed"),
            );
        }
        transaction.commit().await.expect("commit failed");

        assert_eq!(
            fs.block_size() * 3000,
            *allocator.get_owner_allocated_bytes().entry(STORE_OBJECT_ID).or_default() as u64
        );

        // Delete it all.
        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        for range in std::mem::replace(&mut device_ranges, Vec::new()) {
            allocator.deallocate(&mut transaction, STORE_OBJECT_ID, range).await.expect("dealloc");
        }
        transaction.commit().await.expect("commit failed");

        assert_eq!(
            0,
            *allocator.get_owner_allocated_bytes().entry(STORE_OBJECT_ID).or_default() as u64
        );

        // Allocate some more stuff. Due to storage pressure, this requires us to flush device
        // before reusing the above space
        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        let target_len = 1500 * fs.block_size();
        while device_ranges.iter().map(|i| i.length().unwrap()).sum::<u64>() != target_len {
            let len = target_len - device_ranges.iter().map(|i| i.length().unwrap()).sum::<u64>();
            device_ranges.push(
                allocator
                    .allocate(&mut transaction, STORE_OBJECT_ID, len)
                    .await
                    .expect("allocate failed"),
            );
        }
        transaction.commit().await.expect("commit failed");

        assert_eq!(
            fs.block_size() * 1500,
            *allocator.get_owner_allocated_bytes().entry(STORE_OBJECT_ID).or_default() as u64
        );
    }

    #[fuchsia::test]
    async fn test_flush() {
        const STORE_OBJECT_ID: u64 = 99;

        let mut device_ranges = Vec::new();
        let device = {
            let (fs, allocator) = test_fs().await;
            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new failed");
            device_ranges.push(
                allocator
                    .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                    .await
                    .expect("allocate failed"),
            );
            device_ranges.push(
                allocator
                    .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                    .await
                    .expect("allocate failed"),
            );
            device_ranges.push(
                allocator
                    .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                    .await
                    .expect("allocate failed"),
            );
            transaction.commit().await.expect("commit failed");

            allocator.flush().await.expect("flush failed");

            fs.close().await.expect("close failed");
            fs.take_device().await
        };

        device.reopen(false);
        let fs = FxFilesystemBuilder::new().open(device).await.expect("open failed");
        let allocator = fs.allocator();

        let allocated = collect_allocations(&allocator).await;

        // Make sure the ranges we allocated earlier are still allocated.
        for i in &device_ranges {
            let mut overlapping = 0;
            for j in &allocated {
                overlapping += overlap(i, j);
            }
            assert_eq!(overlapping, i.length().unwrap(), "Range {i:?} not allocated");
        }

        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        let range = allocator
            .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
            .await
            .expect("allocate failed");

        // Make sure the range just allocated doesn't overlap any other allocated ranges.
        for r in &allocated {
            assert_eq!(overlap(r, &range), 0);
        }
        transaction.commit().await.expect("commit failed");
    }

    #[fuchsia::test]
    async fn test_dropped_transaction() {
        const STORE_OBJECT_ID: u64 = 99;
        let (fs, allocator) = test_fs().await;
        let allocated_range = {
            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new_transaction failed");
            allocator
                .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                .await
                .expect("allocate failed")
        };
        // After dropping the transaction and attempting to allocate again, we should end up with
        // the same range because the reservation should have been released.
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        assert_eq!(
            allocator
                .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                .await
                .expect("allocate failed"),
            allocated_range
        );
    }

    #[fuchsia::test]
    async fn test_cleanup_removed_owner() {
        const STORE_OBJECT_ID: u64 = 99;
        let device = {
            let (fs, allocator) = test_fs().await;

            assert!(!allocator.get_owner_allocated_bytes().contains_key(&STORE_OBJECT_ID));
            {
                let mut transaction =
                    fs.clone().new_transaction(lock_keys![], Options::default()).await.unwrap();
                allocator
                    .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                    .await
                    .expect("Allocating");
                transaction.commit().await.expect("Committing.");
            }
            allocator.flush().await.expect("Flushing");
            assert!(allocator.get_owner_allocated_bytes().contains_key(&STORE_OBJECT_ID));
            {
                let mut transaction =
                    fs.clone().new_transaction(lock_keys![], Options::default()).await.unwrap();
                allocator.mark_for_deletion(&mut transaction, STORE_OBJECT_ID).await;
                transaction.commit().await.expect("Committing.");
            }
            assert!(!allocator.get_owner_allocated_bytes().contains_key(&STORE_OBJECT_ID));
            fs.close().await.expect("Closing");
            fs.take_device().await
        };

        device.reopen(false);
        let fs = FxFilesystemBuilder::new().open(device).await.expect("open failed");
        let allocator = fs.allocator();
        assert!(!allocator.get_owner_allocated_bytes().contains_key(&STORE_OBJECT_ID));
    }

    #[fuchsia::test]
    async fn test_allocated_bytes() {
        const STORE_OBJECT_ID: u64 = 99;
        let (fs, allocator) = test_fs().await;

        let initial_allocated_bytes = allocator.get_allocated_bytes();

        // Verify allocated_bytes reflects allocation changes.
        let allocated_bytes = initial_allocated_bytes + fs.block_size();
        let allocated_range = {
            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new_transaction failed");
            let range = allocator
                .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                .await
                .expect("allocate failed");
            transaction.commit().await.expect("commit failed");
            assert_eq!(allocator.get_allocated_bytes(), allocated_bytes);
            range
        };

        {
            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new_transaction failed");
            allocator
                .allocate(&mut transaction, STORE_OBJECT_ID, fs.block_size())
                .await
                .expect("allocate failed");

            // Prior to committing, the count of allocated bytes shouldn't change.
            assert_eq!(allocator.get_allocated_bytes(), allocated_bytes);
        }

        // After dropping the prior transaction, the allocated bytes still shouldn't have changed.
        assert_eq!(allocator.get_allocated_bytes(), allocated_bytes);

        // Verify allocated_bytes reflects deallocations.
        let deallocate_range = allocated_range.start + 20..allocated_range.end - 20;
        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");
        allocator
            .deallocate(&mut transaction, STORE_OBJECT_ID, deallocate_range)
            .await
            .expect("deallocate failed");

        // Before committing, there should be no change.
        assert_eq!(allocator.get_allocated_bytes(), allocated_bytes);

        transaction.commit().await.expect("commit failed");

        // After committing, all but 40 bytes should remain allocated.
        assert_eq!(allocator.get_allocated_bytes(), initial_allocated_bytes + 40);
    }

    #[fuchsia::test]
    async fn test_persist_bytes_limit() {
        const LIMIT: u64 = 12345;
        const OWNER_ID: u64 = 12;

        let (fs, allocator) = test_fs().await;
        {
            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new_transaction failed");
            allocator
                .set_bytes_limit(&mut transaction, OWNER_ID, LIMIT)
                .await
                .expect("Failed to set limit.");
            assert!(allocator.inner.lock().unwrap().info.limit_bytes.get(&OWNER_ID).is_none());
            transaction.commit().await.expect("Failed to commit transaction");
            let bytes: u64 = *allocator
                .inner
                .lock()
                .unwrap()
                .info
                .limit_bytes
                .get(&OWNER_ID)
                .expect("Failed to find limit");
            assert_eq!(LIMIT, bytes);
        }
    }

    /// Given a sorted list of non-overlapping ranges, this will coalesce adjacent ranges.
    /// This allows comparison of equivalent sets of ranges which may occur due to differences
    /// across allocator strategies.
    fn coalesce_ranges(ranges: Vec<Range<u64>>) -> Vec<Range<u64>> {
        let mut coalesced = Vec::new();
        let mut prev: Option<Range<u64>> = None;
        for range in ranges {
            if let Some(prev_range) = &mut prev {
                if range.start == prev_range.end {
                    prev_range.end = range.end;
                } else {
                    coalesced.push(prev_range.clone());
                    prev = Some(range);
                }
            } else {
                prev = Some(range);
            }
        }
        if let Some(prev_range) = prev {
            coalesced.push(prev_range);
        }
        coalesced
    }

    #[fuchsia::test]
    async fn test_take_for_trimming() {
        const STORE_OBJECT_ID: u64 = 99;

        // Allocate a large chunk, then free a few bits of it, so we have free chunks interleaved
        // with allocated chunks.
        let allocated_range;
        let expected_free_ranges;
        let device = {
            let (fs, allocator) = test_fs().await;
            let bs = fs.block_size();
            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new failed");
            allocated_range = allocator
                .allocate(&mut transaction, STORE_OBJECT_ID, 32 * bs)
                .await
                .expect("allocate failed");
            transaction.commit().await.expect("commit failed");

            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new failed");
            let base = allocated_range.start;
            expected_free_ranges = vec![
                base..(base + (bs * 1)),
                (base + (bs * 2))..(base + (bs * 3)),
                // Note that the next three ranges are adjacent and will be treated as one free
                // range once applied.  We separate them here to exercise the handling of "large"
                // free ranges.
                (base + (bs * 4))..(base + (bs * 8)),
                (base + (bs * 8))..(base + (bs * 12)),
                (base + (bs * 12))..(base + (bs * 13)),
                (base + (bs * 29))..(base + (bs * 30)),
            ];
            for range in &expected_free_ranges {
                allocator
                    .deallocate(&mut transaction, STORE_OBJECT_ID, range.clone())
                    .await
                    .expect("deallocate failed");
            }
            transaction.commit().await.expect("commit failed");

            allocator.flush().await.expect("flush failed");

            fs.close().await.expect("close failed");
            fs.take_device().await
        };

        device.reopen(false);
        let fs = FxFilesystemBuilder::new().open(device).await.expect("open failed");
        let allocator = fs.allocator();

        // These values were picked so that each of them would be the reason why
        // collect_free_extents finished, and so we would return after partially processing one of
        // the free extents.
        let max_extent_size = fs.block_size() as usize * 4;
        const EXTENTS_PER_BATCH: usize = 2;
        let mut free_ranges = vec![];
        let mut offset = allocated_range.start;
        while offset < allocated_range.end {
            let free = allocator
                .take_for_trimming(offset, max_extent_size, EXTENTS_PER_BATCH)
                .await
                .expect("take_for_trimming failed");
            free_ranges.extend(
                free.extents().iter().filter(|range| range.end <= allocated_range.end).cloned(),
            );
            offset = free.extents().last().expect("Unexpectedly hit the end of free extents").end;
        }
        // Coalesce adjacent free ranges because the buddy allocator will
        // return smaller aligned chunks but the overall range will be
        // equivalent.
        let coalesced_free_ranges = coalesce_ranges(free_ranges);
        let coalesced_expected_free_ranges = coalesce_ranges(expected_free_ranges);

        assert_eq!(coalesced_free_ranges, coalesced_expected_free_ranges);
    }

    #[fuchsia::test]
    async fn test_allocations_wait_for_free_extents() {
        const STORE_OBJECT_ID: u64 = 99;
        let (fs, allocator) = test_fs().await;
        let allocator_clone = allocator.clone();

        let mut transaction =
            fs.clone().new_transaction(lock_keys![], Options::default()).await.expect("new failed");

        // Tie up all of the free extents on the device, and make sure allocations block.
        let max_extent_size = fs.device().size() as usize;
        const EXTENTS_PER_BATCH: usize = usize::MAX;

        // HACK: Treat `trimmable_extents` as being locked by `trim_done` (i.e. it should only be
        // accessed whilst `trim_done` is locked). We can't combine them into the same mutex,
        // because the inner type would be "poisoned" by the lifetime parameter of
        // `trimmable_extents` (which is in the lifetime of `allocator`), and then we can't move it
        // into `alloc_task` which would require a `'static` lifetime.
        let trim_done = Arc::new(Mutex::new(false));
        let trimmable_extents = allocator
            .take_for_trimming(0, max_extent_size, EXTENTS_PER_BATCH)
            .await
            .expect("take_for_trimming failed");

        let trim_done_clone = trim_done.clone();
        let bs = fs.block_size();
        let alloc_task = fasync::Task::spawn(async move {
            allocator_clone
                .allocate(&mut transaction, STORE_OBJECT_ID, bs)
                .await
                .expect("allocate should fail");
            {
                assert!(
                    *trim_done_clone.lock().unwrap(),
                    "Allocation finished before trim completed"
                );
            }
            transaction.commit().await.expect("commit failed");
        });

        // Add a small delay to simulate the trim taking some nonzero amount of time.  Otherwise,
        // this will almost certainly always beat the allocation attempt.
        fasync::Timer::new(std::time::Duration::from_millis(100)).await;

        // Once the free extents are released, the task should unblock.
        {
            let mut trim_done = trim_done.lock().unwrap();
            std::mem::drop(trimmable_extents);
            *trim_done = true;
        }

        alloc_task.await;
    }
}
