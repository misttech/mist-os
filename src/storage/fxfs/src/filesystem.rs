// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::errors::FxfsError;
use crate::fsck::{fsck_volume_with_options, fsck_with_options, FsckOptions};
use crate::log::*;
use crate::object_store::allocator::{Allocator, Hold, Reservation};
use crate::object_store::directory::Directory;
use crate::object_store::graveyard::Graveyard;
use crate::object_store::journal::super_block::SuperBlockHeader;
use crate::object_store::journal::{self, Journal, JournalCheckpoint, JournalOptions};
use crate::object_store::object_manager::ObjectManager;
use crate::object_store::transaction::{
    self, lock_keys, AssocObj, LockKey, LockKeys, LockManager, MetadataReservation, Mutation,
    ReadGuard, Transaction, TRANSACTION_METADATA_MAX_AMOUNT,
};
use crate::object_store::volume::{root_volume, VOLUMES_DIRECTORY};
use crate::object_store::ObjectStore;
use crate::range::RangeExt;
use crate::serialized_types::Version;
use crate::{debug_assert_not_too_long, metrics};
use anyhow::{bail, Context, Error};
use async_trait::async_trait;
use event_listener::Event;
use fuchsia_async as fasync;
use fuchsia_inspect::{NumericProperty as _, UintProperty};
use futures::FutureExt;
use fxfs_crypto::Crypt;
use once_cell::sync::OnceCell;
use static_assertions::const_assert;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use storage_device::{Device, DeviceHolder};

pub const MIN_BLOCK_SIZE: u64 = 4096;
pub const MAX_BLOCK_SIZE: u64 = u16::MAX as u64 + 1;

// Whilst Fxfs could support up to u64::MAX, off_t is i64 so allowing files larger than that becomes
// difficult to deal with via the POSIX APIs. Additionally, PagedObjectHandle only sees data get
// modified in page chunks so to prevent writes at i64::MAX the entire page containing i64::MAX
// needs to be excluded.
pub const MAX_FILE_SIZE: u64 = i64::MAX as u64 - 4095;
const_assert!(9223372036854771712 == MAX_FILE_SIZE);

// The maximum number of transactions that can be in-flight at any time.
const MAX_IN_FLIGHT_TRANSACTIONS: u64 = 4;

// Start trimming 1 hour after boot.  The idea here is to wait until the initial flurry of
// activity during boot is finished.  This is a rough heuristic and may need to change later if
// performance is affected.
const TRIM_AFTER_BOOT_TIMER: std::time::Duration = std::time::Duration::from_secs(60 * 60);

// After the initial trim, perform another trim every 24 hours.
const TRIM_INTERVAL_TIMER: std::time::Duration = std::time::Duration::from_secs(60 * 60 * 24);

/// Holds information on an Fxfs Filesystem
pub struct Info {
    pub total_bytes: u64,
    pub used_bytes: u64,
}

pub type PostCommitHook =
    Option<Box<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>>;

pub type PreCommitHook = Option<Box<dyn Fn(&Transaction<'_>) -> Result<(), Error> + Send + Sync>>;

pub struct Options {
    /// True if the filesystem is read-only.
    pub read_only: bool,

    /// The metadata keys will be rolled after this many bytes.  This must be large enough such that
    /// we can't end up with more than two live keys (so it must be bigger than the maximum possible
    /// size of unflushed journal contents).  This is exposed for testing purposes.
    pub roll_metadata_key_byte_count: u64,

    /// A callback that runs before every transaction is committed.  If this callback returns an
    /// error then the transaction is failed with that error.
    pub pre_commit_hook: PreCommitHook,

    /// A callback that runs after every transaction has been committed.  This will be called whilst
    /// a lock is held which will block more transactions from being committed.
    pub post_commit_hook: PostCommitHook,

    /// If true, don't do an initial reap of the graveyard at mount time.  This is useful for
    /// testing.
    pub skip_initial_reap: bool,

    // The first duration is how long after the filesystem has been mounted to perform an initial
    // trim.  The second is the interval to repeat trimming thereafter.  If set to None, no trimming
    // is done.
    // Default values are (5 minutes, 24 hours).
    pub trim_config: Option<(std::time::Duration, std::time::Duration)>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            roll_metadata_key_byte_count: 128 * 1024 * 1024,
            read_only: false,
            pre_commit_hook: None,
            post_commit_hook: None,
            skip_initial_reap: false,
            trim_config: Some((TRIM_AFTER_BOOT_TIMER, TRIM_INTERVAL_TIMER)),
        }
    }
}

/// The context in which a transaction is being applied.
pub struct ApplyContext<'a, 'b> {
    /// The mode indicates whether the transaction is being replayed.
    pub mode: ApplyMode<'a, 'b>,

    /// The transaction checkpoint for this mutation.
    pub checkpoint: JournalCheckpoint,
}

/// A transaction can be applied during replay or on a live running system (in which case a
/// transaction object will be available).
pub enum ApplyMode<'a, 'b> {
    Replay,
    Live(&'a Transaction<'b>),
}

impl ApplyMode<'_, '_> {
    pub fn is_replay(&self) -> bool {
        matches!(self, ApplyMode::Replay)
    }

    pub fn is_live(&self) -> bool {
        matches!(self, ApplyMode::Live(_))
    }
}

/// Objects that use journaling to track mutations (`Allocator` and `ObjectStore`) implement this.
/// This is primarily used by `ObjectManager` and `SuperBlock` with flush calls used in a few tests.
#[async_trait]
pub trait JournalingObject: Send + Sync {
    /// This method get called when the transaction commits, which can either be during live
    /// operation (See `ObjectManager::apply_mutation`) or during journal replay, in which case
    /// transaction will be None (See `super_block::read`).
    fn apply_mutation(
        &self,
        mutation: Mutation,
        context: &ApplyContext<'_, '_>,
        assoc_obj: AssocObj<'_>,
    ) -> Result<(), Error>;

    /// Called when a transaction fails to commit.
    fn drop_mutation(&self, mutation: Mutation, transaction: &Transaction<'_>);

    /// Flushes in-memory changes to the device (to allow journal space to be freed).
    ///
    /// Also returns the earliest version of a struct in the filesystem.
    async fn flush(&self) -> Result<Version, Error>;

    /// Writes a mutation to the journal.  This allows objects to encrypt or otherwise modify what
    /// gets written to the journal.
    fn write_mutation(&self, mutation: &Mutation, mut writer: journal::Writer<'_>) {
        writer.write(mutation.clone());
    }
}

#[derive(Default)]
pub struct SyncOptions<'a> {
    /// If set, the journal will be flushed, as well as the underlying block device.  This is much
    /// more expensive, but ensures the contents of the journal are persisted (which also acts as a
    /// barrier, ensuring all previous journal writes are observable by future operations).
    /// Note that when this is not set, the journal is *not* synchronously flushed by the sync call,
    /// and it will return before the journal flush completes.  In other words, some journal
    /// mutations may still be buffered in memory after this call returns.
    pub flush_device: bool,

    /// A precondition that is evaluated whilst a lock is held that determines whether or not the
    /// sync needs to proceed.
    pub precondition: Option<Box<dyn FnOnce() -> bool + 'a + Send>>,
}

pub struct OpenFxFilesystem(Arc<FxFilesystem>);

impl OpenFxFilesystem {
    /// Waits for filesystem to be dropped (so callers should ensure all direct and indirect
    /// references are dropped) and returns the device.  No attempt is made at a graceful shutdown.
    pub async fn take_device(self) -> DeviceHolder {
        let fut = self.device.take_when_dropped();
        std::mem::drop(self);
        debug_assert_not_too_long!(fut)
    }
}

impl From<Arc<FxFilesystem>> for OpenFxFilesystem {
    fn from(fs: Arc<FxFilesystem>) -> Self {
        Self(fs)
    }
}

impl Drop for OpenFxFilesystem {
    fn drop(&mut self) {
        if !self.options.read_only && !self.closed.load(Ordering::SeqCst) {
            error!("OpenFxFilesystem dropped without first being closed. Data loss may occur.");
        }
    }
}

impl std::ops::Deref for OpenFxFilesystem {
    type Target = Arc<FxFilesystem>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct FxFilesystemBuilder {
    format: bool,
    trace: bool,
    options: Options,
    journal_options: JournalOptions,
    on_new_allocator: Option<Box<dyn Fn(Arc<Allocator>) + Send + Sync>>,
    on_new_store: Option<Box<dyn Fn(&ObjectStore) + Send + Sync>>,
    fsck_after_every_transaction: bool,
}

impl FxFilesystemBuilder {
    pub fn new() -> Self {
        Self {
            format: false,
            trace: false,
            options: Options::default(),
            journal_options: JournalOptions::default(),
            on_new_allocator: None,
            on_new_store: None,
            fsck_after_every_transaction: false,
        }
    }

    /// Sets whether the block device should be formatted when opened. Defaults to `false`.
    pub fn format(mut self, format: bool) -> Self {
        self.format = format;
        self
    }

    /// Enables or disables trace level logging. Defaults to `false`.
    pub fn trace(mut self, trace: bool) -> Self {
        self.trace = trace;
        self
    }

    /// Sets whether the filesystem will be opened in read-only mode. Defaults to `false`.
    /// Incompatible with `format`.
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.options.read_only = read_only;
        self
    }

    /// Sets how often the metadata keys are rolled. See `Options::roll_metadata_key_byte_count`.
    pub fn roll_metadata_key_byte_count(mut self, roll_metadata_key_byte_count: u64) -> Self {
        self.options.roll_metadata_key_byte_count = roll_metadata_key_byte_count;
        self
    }

    /// Sets a callback that runs before every transaction. See `Options::pre_commit_hook`.
    pub fn pre_commit_hook(
        mut self,
        hook: impl Fn(&Transaction<'_>) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Self {
        self.options.pre_commit_hook = Some(Box::new(hook));
        self
    }

    /// Sets a callback that runs after every transaction has been committed. See
    /// `Options::post_commit_hook`.
    pub fn post_commit_hook(
        mut self,
        hook: impl Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync + 'static,
    ) -> Self {
        self.options.post_commit_hook = Some(Box::new(hook));
        self
    }

    /// Sets whether to do an initial reap of the graveyard at mount time. See
    /// `Options::skip_initial_reap`. Defaults to `false`.
    pub fn skip_initial_reap(mut self, skip_initial_reap: bool) -> Self {
        self.options.skip_initial_reap = skip_initial_reap;
        self
    }

    /// Sets the options for the journal.
    pub fn journal_options(mut self, journal_options: JournalOptions) -> Self {
        self.journal_options = journal_options;
        self
    }

    /// Sets a method to be called immediately after creating the allocator.
    pub fn on_new_allocator(
        mut self,
        on_new_allocator: impl Fn(Arc<Allocator>) + Send + Sync + 'static,
    ) -> Self {
        self.on_new_allocator = Some(Box::new(on_new_allocator));
        self
    }

    /// Sets a method to be called each time a new store is registered with `ObjectManager`.
    pub fn on_new_store(
        mut self,
        on_new_store: impl Fn(&ObjectStore) + Send + Sync + 'static,
    ) -> Self {
        self.on_new_store = Some(Box::new(on_new_store));
        self
    }

    /// Enables or disables running fsck after every transaction. Defaults to `false`.
    pub fn fsck_after_every_transaction(mut self, fsck_after_every_transaction: bool) -> Self {
        self.fsck_after_every_transaction = fsck_after_every_transaction;
        self
    }

    pub fn trim_config(
        mut self,
        delay_and_interval: Option<(std::time::Duration, std::time::Duration)>,
    ) -> Self {
        self.options.trim_config = delay_and_interval;
        self
    }

    /// Constructs an `FxFilesystem` object with the specified settings.
    pub async fn open(self, device: DeviceHolder) -> Result<OpenFxFilesystem, Error> {
        let read_only = self.options.read_only;
        if self.format && read_only {
            bail!("Cannot initialize a filesystem as read-only");
        }

        let objects = Arc::new(ObjectManager::new(self.on_new_store));
        let journal = Arc::new(Journal::new(objects.clone(), self.journal_options));

        let block_size = std::cmp::max(device.block_size().into(), MIN_BLOCK_SIZE);
        assert_eq!(block_size % MIN_BLOCK_SIZE, 0);
        assert!(block_size <= MAX_BLOCK_SIZE, "Max supported block size is 64KiB");

        let mut fsck_after_every_transaction = None;
        let mut filesystem_options = self.options;
        if self.fsck_after_every_transaction {
            let instance =
                FsckAfterEveryTransaction::new(filesystem_options.post_commit_hook.take());
            fsck_after_every_transaction = Some(instance.clone());
            filesystem_options.post_commit_hook =
                Some(Box::new(move || instance.clone().run().boxed()));
        }

        if !read_only && !self.format {
            // See comment in JournalRecord::DidFlushDevice for why we need to flush the device
            // before replay.
            device.flush().await.context("Device flush failed")?;
        }

        let filesystem = Arc::new(FxFilesystem {
            device,
            block_size,
            objects: objects.clone(),
            journal,
            commit_mutex: futures::lock::Mutex::new(()),
            lock_manager: LockManager::new(),
            flush_task: Mutex::new(None),
            trim_task: Mutex::new(None),
            closed: AtomicBool::new(true),
            shutdown_event: Event::new(),
            trace: self.trace,
            graveyard: Graveyard::new(objects.clone()),
            completed_transactions: metrics::detail().create_uint("completed_transactions", 0),
            options: filesystem_options,
            in_flight_transactions: AtomicU64::new(0),
            transaction_limit_event: Event::new(),
        });

        filesystem.journal.set_trace(self.trace);
        if self.format {
            filesystem.journal.init_empty(filesystem.clone()).await?;
            // Start the graveyard's background reaping task.
            filesystem.graveyard.clone().reap_async();

            // Create the root volume directory.
            let root_store = filesystem.root_store();
            root_store.set_trace(self.trace);
            let root_directory =
                Directory::open(&root_store, root_store.root_directory_object_id())
                    .await
                    .context("Unable to open root volume directory")?;
            let mut transaction = filesystem
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(
                        root_store.store_object_id(),
                        root_directory.object_id()
                    )],
                    transaction::Options::default(),
                )
                .await?;
            let volume_directory =
                root_directory.create_child_dir(&mut transaction, VOLUMES_DIRECTORY).await?;
            transaction.commit().await?;
            objects.set_volume_directory(volume_directory);
        } else {
            filesystem
                .journal
                .replay(filesystem.clone(), self.on_new_allocator)
                .await
                .context("Journal replay failed")?;
            filesystem.root_store().set_trace(self.trace);

            if !read_only {
                // Queue all purged entries for tombstoning.  Don't start the reaper yet because
                // that can trigger a flush which can add more entries to the graveyard which might
                // get caught in the initial reap and cause objects to be prematurely tombstoned.
                for store in objects.unlocked_stores() {
                    filesystem.graveyard.initial_reap(&store).await?;
                }
            }
        }

        // This must be after we've formatted the filesystem; it will fail during format otherwise.
        if let Some(fsck_after_every_transaction) = fsck_after_every_transaction {
            fsck_after_every_transaction
                .fs
                .set(Arc::downgrade(&filesystem))
                .unwrap_or_else(|_| unreachable!());
        }

        filesystem.closed.store(false, Ordering::SeqCst);

        if !read_only {
            // Start the background tasks.
            filesystem.graveyard.clone().reap_async();

            if let Some((delay, interval)) = filesystem.options.trim_config.clone() {
                filesystem.start_trim_task(delay, interval);
            }
        }

        Ok(filesystem.into())
    }
}

pub struct FxFilesystem {
    block_size: u64,
    objects: Arc<ObjectManager>,
    journal: Arc<Journal>,
    commit_mutex: futures::lock::Mutex<()>,
    lock_manager: LockManager,
    flush_task: Mutex<Option<fasync::Task<()>>>,
    trim_task: Mutex<Option<fasync::Task<()>>>,
    closed: AtomicBool,
    // An event that is signalled when the filesystem starts to shut down.
    shutdown_event: Event,
    trace: bool,
    graveyard: Arc<Graveyard>,
    completed_transactions: UintProperty,
    options: Options,

    // The number of in-flight transactions which we will limit to MAX_IN_FLIGHT_TRANSACTIONS.
    in_flight_transactions: AtomicU64,

    // An event that is used to wake up tasks that are blocked due to the in-flight transaction
    // limit.
    transaction_limit_event: Event,

    // NOTE: This *must* go last so that when users take the device from a closed filesystem, the
    // filesystem has dropped all other members first (Rust drops members in declaration order).
    device: DeviceHolder,
}

#[fxfs_trace::trace]
impl FxFilesystem {
    pub async fn new_empty(device: DeviceHolder) -> Result<OpenFxFilesystem, Error> {
        FxFilesystemBuilder::new().format(true).open(device).await
    }

    pub async fn open(device: DeviceHolder) -> Result<OpenFxFilesystem, Error> {
        FxFilesystemBuilder::new().open(device).await
    }

    pub fn root_parent_store(&self) -> Arc<ObjectStore> {
        self.objects.root_parent_store()
    }

    pub async fn close(&self) -> Result<(), Error> {
        assert_eq!(self.closed.swap(true, Ordering::SeqCst), false);
        self.shutdown_event.notify(usize::MAX);
        debug_assert_not_too_long!(self.graveyard.wait_for_reap());
        let trim_task = self.trim_task.lock().unwrap().take();
        if let Some(task) = trim_task {
            debug_assert_not_too_long!(task);
        }
        self.journal.stop_compactions().await;
        let sync_status =
            self.journal.sync(SyncOptions { flush_device: true, ..Default::default() }).await;
        match &sync_status {
            Ok(checkpoint) => info!(
                "Filesystem closed (checkpoint: {})",
                checkpoint.as_ref().unwrap().0.file_offset
            ),
            Err(e) => error!(error = ?e, "Failed to sync filesystem; data may be lost"),
        }
        self.journal.terminate();
        let flush_task = self.flush_task.lock().unwrap().take();
        if let Some(task) = flush_task {
            debug_assert_not_too_long!(task);
        }
        // Regardless of whether sync succeeds, we should close the device, since otherwise we will
        // crash instead of exiting gracefully.
        self.device().close().await.context("Failed to close device")?;
        sync_status.map(|_| ())
    }

    pub fn device(&self) -> Arc<dyn Device> {
        Arc::clone(&self.device)
    }

    pub fn root_store(&self) -> Arc<ObjectStore> {
        self.objects.root_store()
    }

    pub fn allocator(&self) -> Arc<Allocator> {
        self.objects.allocator()
    }

    pub fn object_manager(&self) -> &Arc<ObjectManager> {
        &self.objects
    }

    pub fn journal(&self) -> &Arc<Journal> {
        &self.journal
    }

    pub async fn sync(&self, options: SyncOptions<'_>) -> Result<(), Error> {
        self.journal.sync(options).await.map(|_| ())
    }

    pub fn block_size(&self) -> u64 {
        self.block_size
    }

    pub fn get_info(&self) -> Info {
        Info {
            total_bytes: self.device.size(),
            used_bytes: self.object_manager().allocator().get_used_bytes().0,
        }
    }

    pub fn super_block_header(&self) -> SuperBlockHeader {
        self.journal.super_block_header()
    }

    pub fn graveyard(&self) -> &Arc<Graveyard> {
        &self.graveyard
    }

    pub fn trace(&self) -> bool {
        self.trace
    }

    pub fn options(&self) -> &Options {
        &self.options
    }

    /// Returns a guard that must be taken before any transaction can commence.  This guard takes a
    /// shared lock on the filesystem.  `fsck` will take an exclusive lock so that it can get a
    /// consistent picture of the filesystem that it can verify.  It is important that this lock is
    /// acquired before *all* other locks.  It is also important that this lock is not taken twice
    /// by the same task since that can lead to deadlocks if another task tries to take a write
    /// lock.
    pub async fn txn_guard(self: Arc<Self>) -> TxnGuard<'static> {
        TxnGuard::Owned(
            self.lock_manager
                .read_lock(lock_keys!(LockKey::Filesystem))
                .await
                .into_owned(self.clone()),
        )
    }

    pub async fn new_transaction<'a>(
        self: Arc<Self>,
        locks: LockKeys,
        options: transaction::Options<'a>,
    ) -> Result<Transaction<'a>, Error> {
        let guard = if let Some(guard) = options.txn_guard.as_ref() {
            TxnGuard::Borrowed(guard)
        } else {
            self.txn_guard().await
        };
        Transaction::new(guard, options, locks).await
    }

    #[trace]
    pub async fn commit_transaction(
        &self,
        transaction: &mut Transaction<'_>,
        callback: &mut (dyn FnMut(u64) + Send),
    ) -> Result<u64, Error> {
        if let Some(hook) = self.options.pre_commit_hook.as_ref() {
            hook(transaction)?;
        }
        debug_assert_not_too_long!(self.lock_manager.commit_prepare(&transaction));
        self.maybe_start_flush_task();
        let _guard = debug_assert_not_too_long!(self.commit_mutex.lock());
        let journal_offset = self.journal.commit(transaction).await?;
        self.completed_transactions.add(1);

        // For now, call the callback whilst holding the lock.  Technically, we don't need to do
        // that except if there's a post-commit-hook (which there usually won't be).  We can
        // consider changing this if we need to for performance, but we'd need to double check that
        // callers don't depend on this.
        callback(journal_offset);

        if let Some(hook) = self.options.post_commit_hook.as_ref() {
            hook().await;
        }

        Ok(journal_offset)
    }

    pub fn lock_manager(&self) -> &LockManager {
        &self.lock_manager
    }

    pub(crate) fn drop_transaction(&self, transaction: &mut Transaction<'_>) {
        if !matches!(transaction.metadata_reservation, MetadataReservation::None) {
            self.sub_transaction();
        }
        // If we placed a hold for metadata space, return it now.
        if let MetadataReservation::Hold(hold_amount) =
            std::mem::replace(&mut transaction.metadata_reservation, MetadataReservation::None)
        {
            let hold = transaction
                .allocator_reservation
                .unwrap()
                .reserve(0)
                .expect("Zero should always succeed.");
            hold.add(hold_amount);
        }
        self.objects.drop_transaction(transaction);
        self.lock_manager.drop_transaction(transaction);
    }

    fn maybe_start_flush_task(&self) {
        let mut flush_task = self.flush_task.lock().unwrap();
        if flush_task.is_none() {
            let journal = self.journal.clone();
            *flush_task = Some(fasync::Task::spawn(journal.flush_task()));
        }
    }

    // Returns the number of bytes trimmed.
    async fn do_trim(&self) -> Result<usize, Error> {
        const MAX_EXTENTS_PER_BATCH: usize = 8;
        const MAX_EXTENT_SIZE: usize = 256 * 1024;
        let mut offset = 0;
        let mut bytes_trimmed = 0;
        loop {
            if self.closed.load(Ordering::Relaxed) {
                info!("Filesystem is closed, nothing to trim");
                return Ok(bytes_trimmed);
            }
            let allocator = self.allocator();
            let trimmable_extents =
                allocator.take_for_trimming(offset, MAX_EXTENT_SIZE, MAX_EXTENTS_PER_BATCH).await?;
            for device_range in trimmable_extents.extents() {
                self.device.trim(device_range.clone()).await?;
                bytes_trimmed += device_range.length()? as usize;
            }
            if let Some(device_range) = trimmable_extents.extents().last() {
                offset = device_range.end;
            } else {
                break;
            }
        }
        Ok(bytes_trimmed)
    }

    fn start_trim_task(
        self: &Arc<Self>,
        delay: std::time::Duration,
        interval: std::time::Duration,
    ) {
        if !self.device.supports_trim() {
            info!("Device does not support trim; not scheduling trimming");
            return;
        }
        let this = self.clone();
        let mut next_timer = delay;
        *self.trim_task.lock().unwrap() = Some(fasync::Task::spawn(async move {
            loop {
                let shutdown_listener = this.shutdown_event.listen();
                // Note that we need to check if the filesystem was closed after we start listening
                // to the shutdown event, but before we start waiting on `timer`, because otherwise
                // we might start listening on `shutdown_event` *after* the event was signaled, and
                // so `shutdown_listener` will never fire, and this task will get stuck until
                // `timer` expires.
                if this.closed.load(Ordering::SeqCst) {
                    return;
                }
                futures::select!(
                    () = fasync::Timer::new(next_timer.clone()).fuse() => {},
                    () = shutdown_listener.fuse() => return,
                );
                let start_time = std::time::Instant::now();
                let res = this.do_trim().await;
                let duration = std::time::Instant::now() - start_time;
                next_timer = interval.clone();
                match res {
                    Ok(bytes_trimmed) => info!(
                        "Trimmed {bytes_trimmed} bytes in {duration:?}.  Next trim in \
                        {next_timer:?}",
                    ),
                    Err(e) => error!(?e, "Failed to trim"),
                }
            }
        }));
    }

    pub(crate) async fn reservation_for_transaction<'a>(
        self: &Arc<Self>,
        options: transaction::Options<'a>,
    ) -> Result<(MetadataReservation, Option<&'a Reservation>, Option<Hold<'a>>), Error> {
        if !options.skip_journal_checks {
            self.maybe_start_flush_task();
            self.journal.check_journal_space().await?;
        }

        // We support three options for metadata space reservation:
        //
        //   1. We can borrow from the filesystem's metadata reservation.  This should only be
        //      be used on the understanding that eventually, potentially after a full compaction,
        //      there should be no net increase in space used.  For example, unlinking an object
        //      should eventually decrease the amount of space used and setting most attributes
        //      should not result in any change.
        //
        //   2. A reservation is provided in which case we'll place a hold on some of it for
        //      metadata.
        //
        //   3. No reservation is supplied, so we try and reserve space with the allocator now,
        //      and will return NoSpace if that fails.
        let mut hold = None;
        let metadata_reservation = if options.borrow_metadata_space {
            MetadataReservation::Borrowed
        } else {
            match options.allocator_reservation {
                Some(reservation) => {
                    hold = Some(
                        reservation
                            .reserve(TRANSACTION_METADATA_MAX_AMOUNT)
                            .ok_or(FxfsError::NoSpace)?,
                    );
                    MetadataReservation::Hold(TRANSACTION_METADATA_MAX_AMOUNT)
                }
                None => {
                    let reservation = self
                        .allocator()
                        .reserve(None, TRANSACTION_METADATA_MAX_AMOUNT)
                        .ok_or(FxfsError::NoSpace)?;
                    MetadataReservation::Reservation(reservation)
                }
            }
        };
        Ok((metadata_reservation, options.allocator_reservation, hold))
    }

    pub(crate) async fn add_transaction(&self, skip_journal_checks: bool) {
        if skip_journal_checks {
            self.in_flight_transactions.fetch_add(1, Ordering::Relaxed);
        } else {
            let inc = || {
                let mut in_flights = self.in_flight_transactions.load(Ordering::Relaxed);
                while in_flights < MAX_IN_FLIGHT_TRANSACTIONS {
                    match self.in_flight_transactions.compare_exchange_weak(
                        in_flights,
                        in_flights + 1,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => return true,
                        Err(x) => in_flights = x,
                    }
                }
                return false;
            };
            while !inc() {
                let listener = self.transaction_limit_event.listen();
                if inc() {
                    break;
                }
                listener.await;
            }
        }
    }

    pub(crate) fn sub_transaction(&self) {
        let old = self.in_flight_transactions.fetch_sub(1, Ordering::Relaxed);
        assert!(old != 0);
        if old <= MAX_IN_FLIGHT_TRANSACTIONS {
            self.transaction_limit_event.notify(usize::MAX);
        }
    }
}

pub enum TxnGuard<'a> {
    Borrowed(&'a TxnGuard<'a>),
    Owned(ReadGuard<'static>),
}

impl TxnGuard<'_> {
    pub fn fs(&self) -> &Arc<FxFilesystem> {
        match self {
            TxnGuard::Borrowed(b) => b.fs(),
            TxnGuard::Owned(o) => o.fs().unwrap(),
        }
    }
}

/// Helper method for making a new filesystem.
pub async fn mkfs(device: DeviceHolder) -> Result<(), Error> {
    let fs = FxFilesystem::new_empty(device).await?;
    fs.close().await
}

/// Helper method for making a new filesystem with a single named volume.
/// This shouldn't be used in production; instead volumes should be created with the Volumes
/// protocol.
pub async fn mkfs_with_volume(
    device: DeviceHolder,
    volume_name: &str,
    crypt: Option<Arc<dyn Crypt>>,
) -> Result<(), Error> {
    let fs = FxFilesystem::new_empty(device).await?;
    {
        // expect instead of propagating errors here, since otherwise we could drop |fs| before
        // close is called, which leads to confusing and unrelated error messages.
        let root_volume = root_volume(fs.clone()).await.expect("Open root_volume failed");
        root_volume.new_volume(volume_name, crypt).await.expect("Create volume failed");
    }
    fs.close().await?;
    Ok(())
}

struct FsckAfterEveryTransaction {
    fs: OnceCell<Weak<FxFilesystem>>,
    old_hook: PostCommitHook,
}

impl FsckAfterEveryTransaction {
    fn new(old_hook: PostCommitHook) -> Arc<Self> {
        Arc::new(Self { fs: OnceCell::new(), old_hook })
    }

    async fn run(self: Arc<Self>) {
        if let Some(fs) = self.fs.get().and_then(Weak::upgrade) {
            let options = FsckOptions {
                fail_on_warning: true,
                no_lock: true,
                quiet: true,
                ..Default::default()
            };
            fsck_with_options(fs.clone(), &options).await.expect("fsck failed");
            let object_manager = fs.object_manager();
            for store in object_manager.unlocked_stores() {
                let store_id = store.store_object_id();
                if !object_manager.is_system_store(store_id) {
                    fsck_volume_with_options(fs.as_ref(), &options, store_id, None)
                        .await
                        .expect("fsck_volume_with_options failed");
                }
            }
        }
        if let Some(old_hook) = self.old_hook.as_ref() {
            old_hook().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FxFilesystem, FxFilesystemBuilder, SyncOptions};
    use crate::fsck::{fsck, fsck_volume};
    use crate::log::*;
    use crate::lsm_tree::types::Item;
    use crate::lsm_tree::Operation;
    use crate::object_handle::{
        ObjectHandle, ReadObjectHandle, WriteObjectHandle, INVALID_OBJECT_ID,
    };
    use crate::object_store::directory::{replace_child, Directory};
    use crate::object_store::journal::JournalOptions;
    use crate::object_store::transaction::{lock_keys, LockKey, Options};
    use crate::object_store::volume::root_volume;
    use crate::object_store::{HandleOptions, ObjectStore};
    use fuchsia_async as fasync;
    use futures::future::join_all;
    use futures::stream::{FuturesUnordered, TryStreamExt};
    use fxfs_insecure_crypto::InsecureCrypt;
    use rustc_hash::FxHashMap as HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use storage_device::fake_device::FakeDevice;
    use storage_device::DeviceHolder;

    const TEST_DEVICE_BLOCK_SIZE: u32 = 512;

    #[fuchsia::test(threads = 10)]
    async fn test_compaction() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));

        // If compaction is not working correctly, this test will run out of space.
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let root_store = fs.root_store();
        let root_directory = Directory::open(&root_store, root_store.root_directory_object_id())
            .await
            .expect("open failed");

        let mut tasks = Vec::new();
        for i in 0..2 {
            let mut transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(
                        root_store.store_object_id(),
                        root_directory.object_id()
                    )],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");
            let handle = root_directory
                .create_child_file(&mut transaction, &format!("{}", i))
                .await
                .expect("create_child_file failed");
            transaction.commit().await.expect("commit failed");
            tasks.push(fasync::Task::spawn(async move {
                const TEST_DATA: &[u8] = b"hello";
                let mut buf = handle.allocate_buffer(TEST_DATA.len()).await;
                buf.as_mut_slice().copy_from_slice(TEST_DATA);
                for _ in 0..1500 {
                    handle.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
                }
            }));
        }
        join_all(tasks).await;
        fs.sync(SyncOptions::default()).await.expect("sync failed");

        fsck(fs.clone()).await.expect("fsck failed");
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_replay_is_identical() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");

        // Reopen the store, but set reclaim size to a very large value which will effectively
        // stop the journal from flushing and allows us to track all the mutations to the store.
        fs.close().await.expect("close failed");
        let device = fs.take_device().await;
        device.reopen(false);

        struct Mutations<K, V>(Mutex<Vec<(Operation, Item<K, V>)>>);

        impl<K: Clone, V: Clone> Mutations<K, V> {
            fn new() -> Self {
                Mutations(Mutex::new(Vec::new()))
            }

            fn push(&self, operation: Operation, item: &Item<K, V>) {
                self.0.lock().unwrap().push((operation, item.clone()));
            }
        }

        let open_fs = |device,
                       object_mutations: Arc<Mutex<HashMap<_, _>>>,
                       allocator_mutations: Arc<Mutations<_, _>>| async {
            FxFilesystemBuilder::new()
                .journal_options(JournalOptions { reclaim_size: u64::MAX, ..Default::default() })
                .on_new_allocator(move |allocator| {
                    let allocator_mutations = allocator_mutations.clone();
                    allocator.tree().set_mutation_callback(Some(Box::new(move |op, item| {
                        allocator_mutations.push(op, item)
                    })));
                })
                .on_new_store(move |store| {
                    let mutations = Arc::new(Mutations::new());
                    object_mutations
                        .lock()
                        .unwrap()
                        .insert(store.store_object_id(), mutations.clone());
                    store.tree().set_mutation_callback(Some(Box::new(move |op, item| {
                        mutations.push(op, item)
                    })));
                })
                .open(device)
                .await
                .expect("open failed")
        };

        let allocator_mutations = Arc::new(Mutations::new());
        let object_mutations = Arc::new(Mutex::new(HashMap::default()));
        let fs = open_fs(device, object_mutations.clone(), allocator_mutations.clone()).await;

        let root_store = fs.root_store();
        let root_directory = Directory::open(&root_store, root_store.root_directory_object_id())
            .await
            .expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(
                    root_store.store_object_id(),
                    root_directory.object_id()
                )],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let object = root_directory
            .create_child_file(&mut transaction, "test")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        // Append some data.
        let buf = object.allocate_buffer(10000).await;
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");

        // Overwrite some data.
        object.write_or_append(Some(5000), buf.as_ref()).await.expect("write failed");

        // Truncate.
        object.truncate(3000).await.expect("truncate failed");

        // Delete the object.
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(root_store.store_object_id(), root_directory.object_id()),
                    LockKey::object(root_store.store_object_id(), object.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");

        replace_child(&mut transaction, None, (&root_directory, "test"))
            .await
            .expect("replace_child failed");

        transaction.commit().await.expect("commit failed");

        // Finally tombstone the object.
        root_store
            .tombstone_object(object.object_id(), Options::default())
            .await
            .expect("tombstone failed");

        // Now reopen and check that replay produces the same set of mutations.
        fs.close().await.expect("close failed");

        let metadata_reservation_amount = fs.object_manager().metadata_reservation().amount();

        let device = fs.take_device().await;
        device.reopen(false);

        let replayed_object_mutations = Arc::new(Mutex::new(HashMap::default()));
        let replayed_allocator_mutations = Arc::new(Mutations::new());
        let fs = open_fs(
            device,
            replayed_object_mutations.clone(),
            replayed_allocator_mutations.clone(),
        )
        .await;

        let m1 = object_mutations.lock().unwrap();
        let m2 = replayed_object_mutations.lock().unwrap();
        assert_eq!(m1.len(), m2.len());
        for (store_id, mutations) in &*m1 {
            let mutations = mutations.0.lock().unwrap();
            let replayed = m2.get(&store_id).expect("Found unexpected store").0.lock().unwrap();
            assert_eq!(mutations.len(), replayed.len());
            for ((op1, i1), (op2, i2)) in mutations.iter().zip(replayed.iter()) {
                assert_eq!(op1, op2);
                assert_eq!(i1.key, i2.key);
                assert_eq!(i1.value, i2.value);
                assert_eq!(i1.sequence, i2.sequence);
            }
        }

        let a1 = allocator_mutations.0.lock().unwrap();
        let a2 = replayed_allocator_mutations.0.lock().unwrap();
        assert_eq!(a1.len(), a2.len());
        for ((op1, i1), (op2, i2)) in a1.iter().zip(a2.iter()) {
            assert_eq!(op1, op2);
            assert_eq!(i1.key, i2.key);
            assert_eq!(i1.value, i2.value);
            assert_eq!(i1.sequence, i2.sequence);
        }

        assert_eq!(
            fs.object_manager().metadata_reservation().amount(),
            metadata_reservation_amount
        );
    }

    #[fuchsia::test]
    async fn test_max_in_flight_transactions() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");

        let transactions = FuturesUnordered::new();
        for _ in 0..super::MAX_IN_FLIGHT_TRANSACTIONS {
            transactions.push(fs.clone().new_transaction(lock_keys![], Options::default()));
        }
        let mut transactions: Vec<_> = transactions.try_collect().await.unwrap();

        // Trying to create another one should be blocked.
        let mut fut = std::pin::pin!(fs.clone().new_transaction(lock_keys![], Options::default()));
        assert!(futures::poll!(&mut fut).is_pending());

        // Dropping one should allow it to proceed.
        transactions.pop();

        assert!(futures::poll!(&mut fut).is_ready());
    }

    // If run on a single thread, the trim tasks starve out other work.
    #[fuchsia::test(threads = 10)]
    async fn test_continuously_trim() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystemBuilder::new()
            .trim_config(Some((Duration::ZERO, Duration::ZERO)))
            .format(true)
            .open(device)
            .await
            .expect("open failed");
        // Do a small sleep so trim has time to get going.
        fasync::Timer::new(Duration::from_millis(10)).await;

        // Create and delete a bunch of files whilst trim is ongoing.  This just ensures that
        // regular usage isn't affected by trim.
        let root_store = fs.root_store();
        let root_directory = Directory::open(&root_store, root_store.root_directory_object_id())
            .await
            .expect("open failed");
        for _ in 0..100 {
            let mut transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(
                        root_store.store_object_id(),
                        root_directory.object_id()
                    )],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");
            let object = root_directory
                .create_child_file(&mut transaction, "test")
                .await
                .expect("create_child_file failed");
            transaction.commit().await.expect("commit failed");

            {
                let buf = object.allocate_buffer(1024).await;
                object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
            }
            std::mem::drop(object);

            let mut transaction = root_directory
                .acquire_context_for_replace(None, "test", true)
                .await
                .expect("acquire_context_for_replace failed")
                .transaction;
            replace_child(&mut transaction, None, (&root_directory, "test"))
                .await
                .expect("replace_child failed");
            transaction.commit().await.expect("commit failed");
        }
        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn test_power_fail() {
        // This test randomly discards blocks, so we run it a few times to increase the chances
        // of catching an issue in a single run.
        for _ in 0..10 {
            let (store_id, device, test_file_object_id) = {
                let device = DeviceHolder::new(FakeDevice::new(8192, 4096));
                let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
                let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");

                fs.sync(SyncOptions { flush_device: true, ..SyncOptions::default() })
                    .await
                    .expect("sync failed");

                let store = root_volume
                    .new_volume("test", Some(Arc::new(InsecureCrypt::new())))
                    .await
                    .expect("new_volume failed");
                let root_directory = Directory::open(&store, store.root_directory_object_id())
                    .await
                    .expect("open failed");

                // Create a number of files with the goal of using up more than one journal block.
                async fn create_files(store: &Arc<ObjectStore>, prefix: &str) {
                    let fs = store.filesystem();
                    let root_directory = Directory::open(store, store.root_directory_object_id())
                        .await
                        .expect("open failed");
                    for i in 0..100 {
                        let mut transaction = fs
                            .clone()
                            .new_transaction(
                                lock_keys![LockKey::object(
                                    store.store_object_id(),
                                    store.root_directory_object_id()
                                )],
                                Options::default(),
                            )
                            .await
                            .expect("new_transaction failed");
                        root_directory
                            .create_child_file(&mut transaction, &format!("{prefix} {i}"))
                            .await
                            .expect("create_child_file failed");
                        transaction.commit().await.expect("commit failed");
                    }
                }

                // Create one batch of files.
                create_files(&store, "A").await;

                // Create a file and write something to it.  This will make sure there's a
                // transaction present that includes a checksum.
                let mut transaction = fs
                    .clone()
                    .new_transaction(
                        lock_keys![LockKey::object(
                            store.store_object_id(),
                            store.root_directory_object_id()
                        )],
                        Options::default(),
                    )
                    .await
                    .expect("new_transaction failed");
                let object = root_directory
                    .create_child_file(&mut transaction, "test")
                    .await
                    .expect("create_child_file failed");
                transaction.commit().await.expect("commit failed");

                let mut transaction =
                    object.new_transaction().await.expect("new_transaction failed");
                let mut buffer = object.allocate_buffer(4096).await;
                buffer.as_mut_slice().fill(0xed);
                object
                    .txn_write(&mut transaction, 0, buffer.as_ref())
                    .await
                    .expect("txn_write failed");
                transaction.commit().await.expect("commit failed");

                // Create another batch of files.
                create_files(&store, "B").await;

                // Sync the device, but don't flush the device. We want to do this so we can
                // randomly discard blocks below.
                fs.sync(SyncOptions::default()).await.expect("sync failed");

                // When we call `sync` above on the filesystem, it will pad the journal so that it
                // will get written, but it doesn't wait for the write to occur.  We wait for a
                // short time here to give allow time for the journal to be written.  Adding timers
                // isn't great, but this test already isn't deterministic since we randomly discard
                // blocks.
                fasync::Timer::new(Duration::from_millis(10)).await;

                (
                    store.store_object_id(),
                    fs.device().snapshot().expect("snapshot failed"),
                    object.object_id(),
                )
            };

            // Randomly discard blocks since the last flush.  This simulates what might happen in
            // the case of power-loss.  This will be an uncontrolled unmount.
            device
                .discard_random_since_last_flush()
                .expect("discard_random_since_last_flush failed");

            let fs = FxFilesystem::open(device).await.expect("open failed");
            fsck(fs.clone()).await.expect("fsck failed");

            let mut check_test_file = false;

            // If we replayed and the store exists (i.e. the transaction that created the store
            // made it out), start by running fsck on it.
            let object_id = if fs.object_manager().store(store_id).is_some() {
                fsck_volume(&fs, store_id, Some(Arc::new(InsecureCrypt::new())))
                    .await
                    .expect("fsck_volume failed");

                // Now we want to create another file, unmount cleanly, and then finally check that
                // the new file exists.  This checks that we can continue to use the filesystem
                // after an unclean unmount.
                let store = root_volume(fs.clone())
                    .await
                    .expect("root_volume failed")
                    .volume("test", Some(Arc::new(InsecureCrypt::new())))
                    .await
                    .expect("volume failed");

                let root_directory = Directory::open(&store, store.root_directory_object_id())
                    .await
                    .expect("open failed");

                let mut transaction = fs
                    .clone()
                    .new_transaction(
                        lock_keys![LockKey::object(
                            store.store_object_id(),
                            store.root_directory_object_id()
                        )],
                        Options::default(),
                    )
                    .await
                    .expect("new_transaction failed");
                let object = root_directory
                    .create_child_file(&mut transaction, &format!("C"))
                    .await
                    .expect("create_child_file failed");
                transaction.commit().await.expect("commit failed");

                // Write again to the test file if it exists.
                if let Ok(test_file) = ObjectStore::open_object(
                    &store,
                    test_file_object_id,
                    HandleOptions::default(),
                    None,
                )
                .await
                {
                    // Check it has the contents we expect.
                    let mut buffer = test_file.allocate_buffer(4096).await;
                    let bytes = test_file.read(0, buffer.as_mut()).await.expect("read failed");
                    if bytes == 4096 {
                        let expected = [0xed; 4096];
                        assert_eq!(buffer.as_slice(), &expected);
                    } else {
                        // If the write didn't make it, the file should have zero bytes.
                        assert_eq!(bytes, 0);
                    }

                    // Modify the test file.
                    let mut transaction =
                        test_file.new_transaction().await.expect("new_transaction failed");
                    buffer.as_mut_slice().fill(0x37);
                    test_file
                        .txn_write(&mut transaction, 0, buffer.as_ref())
                        .await
                        .expect("txn_write failed");
                    transaction.commit().await.expect("commit failed");
                    check_test_file = true;
                }

                object.object_id()
            } else {
                INVALID_OBJECT_ID
            };

            // This will do a controlled unmount.
            fs.close().await.expect("close failed");
            let device = fs.take_device().await;
            device.reopen(false);

            let fs = FxFilesystem::open(device).await.expect("open failed");
            fsck(fs.clone()).await.expect("fsck failed");

            // As mentioned above, make sure that the object we created before the clean unmount
            // exists.
            if object_id != INVALID_OBJECT_ID {
                fsck_volume(&fs, store_id, Some(Arc::new(InsecureCrypt::new())))
                    .await
                    .expect("fsck_volume failed");

                let store = root_volume(fs.clone())
                    .await
                    .expect("root_volume failed")
                    .volume("test", Some(Arc::new(InsecureCrypt::new())))
                    .await
                    .expect("volume failed");
                // We should be able to open the C object.
                ObjectStore::open_object(&store, object_id, HandleOptions::default(), None)
                    .await
                    .expect("open_object failed");

                // If we made the modification to the test file, check it.
                if check_test_file {
                    info!("Checking test file for modification");
                    let test_file = ObjectStore::open_object(
                        &store,
                        test_file_object_id,
                        HandleOptions::default(),
                        None,
                    )
                    .await
                    .expect("open_object failed");
                    let mut buffer = test_file.allocate_buffer(4096).await;
                    assert_eq!(
                        test_file.read(0, buffer.as_mut()).await.expect("read failed"),
                        4096
                    );
                    let expected = [0x37; 4096];
                    assert_eq!(buffer.as_slice(), &expected);
                }
            }

            fs.close().await.expect("close failed");
        }
    }
}
