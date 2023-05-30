// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        debug_assert_not_too_long,
        errors::FxfsError,
        fsck::{fsck_volume_with_options, fsck_with_options, FsckOptions},
        log::*,
        metrics,
        object_store::{
            allocator::{Allocator, Hold, Reservation, SimpleAllocator},
            directory::Directory,
            graveyard::Graveyard,
            journal::{
                self, super_block::SuperBlockHeader, Journal, JournalCheckpoint, JournalOptions,
            },
            object_manager::ObjectManager,
            transaction::{
                self, AssocObj, LockKey, LockManager, MetadataReservation, Mutation, ReadGuard,
                Transaction, TransactionHandler, TransactionLocks, WriteGuard,
                TRANSACTION_METADATA_MAX_AMOUNT,
            },
            volume::{root_volume, VOLUMES_DIRECTORY},
            ObjectStore,
        },
        serialized_types::Version,
        trace_duration,
    },
    anyhow::{bail, Context, Error},
    async_trait::async_trait,
    event_listener::Event,
    fuchsia_async as fasync,
    fuchsia_inspect::{NumericProperty as _, UintProperty},
    futures::{
        channel::oneshot::{channel, Sender},
        FutureExt,
    },
    fxfs_crypto::Crypt,
    once_cell::sync::OnceCell,
    scopeguard::ScopeGuard,
    static_assertions::const_assert,
    std::sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
    storage_device::{Device, DeviceHolder},
};

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

/// Holds information on an Fxfs Filesystem
pub struct Info {
    pub total_bytes: u64,
    pub used_bytes: u64,
}

pub type PostCommitHook =
    Option<Box<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>>;

pub struct Options {
    /// True if the filesystem is read-only.
    pub read_only: bool,

    /// The metadata keys will be rolled after this many bytes.  This must be large enough such that
    /// we can't end up with more than two live keys (so it must be bigger than the maximum possible
    /// size of unflushed journal contents).  This is exposed for testing purposes.
    pub roll_metadata_key_byte_count: u64,

    /// A callback that runs after every transaction has been committed.  This will be called whilst
    /// a lock is held which will block more transactions from being committed.
    pub post_commit_hook: PostCommitHook,

    /// If true, don't do an initial reap of the graveyard at mount time.  This is useful for
    /// testing.
    pub skip_initial_reap: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            roll_metadata_key_byte_count: 128 * 1024 * 1024,
            read_only: false,
            post_commit_hook: None,
            skip_initial_reap: false,
        }
    }
}

#[async_trait]
pub trait Filesystem: TransactionHandler {
    /// Returns access to the undeyling device.
    fn device(&self) -> Arc<dyn Device>;

    /// Returns the root store or panics if it is not available.
    fn root_store(&self) -> Arc<ObjectStore>;

    /// Returns the allocator or panics if it is not available.
    fn allocator(&self) -> Arc<SimpleAllocator>;

    /// Returns the object manager for the filesystem.
    fn object_manager(&self) -> &Arc<ObjectManager>;

    /// Returns the journal for the filesystem.
    fn journal(&self) -> &Arc<Journal>;

    /// Flushes buffered data to the underlying device.
    async fn sync(&self, options: SyncOptions<'_>) -> Result<(), Error>;

    /// Returns the filesystem block size.
    fn block_size(&self) -> u64;

    /// Returns filesystem information.
    fn get_info(&self) -> Info;

    /// Returns the super-block header.
    fn super_block_header(&self) -> SuperBlockHeader;

    /// Returns the graveyard manager (whilst there exists a graveyard per store, the manager
    /// handles all of them).
    fn graveyard(&self) -> &Arc<Graveyard>;

    /// Whether to enable verbose logging.
    fn trace(&self) -> bool {
        false
    }

    /// Returns the filesystem options.
    fn options(&self) -> &Options;

    /// Spawns a new task that runs `future` to completion in the background.
    fn spawn_background_task(&self, future: futures::future::BoxFuture<'static, ()>);
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

#[async_trait]
pub trait JournalingObject: Send + Sync {
    /// Objects that use the journaling system to track mutations should implement this trait.  This
    /// method will get called when the transaction commits, which can either be during live
    /// operation or during journal replay, in which case transaction will be None.  Also see
    /// ObjectManager's apply_mutation method.
    async fn apply_mutation(
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

    // A precondition that is evaluated whilst a lock is held that determines whether or not the
    // sync needs to proceed.
    pub precondition: Option<Box<dyn FnOnce() -> bool + 'a + Send>>,
}

pub struct OpenFxFilesystem(Arc<FxFilesystem>);

impl OpenFxFilesystem {
    /// Waits for filesystem to be dropped (so callers should ensure all direct and indirect
    /// references are dropped) and returns the device.  No attempt is made at a graceful shutdown.
    pub async fn take_device(self) -> DeviceHolder {
        let (sender, receiver) = channel::<DeviceHolder>();
        self.device_sender
            .set(sender)
            .unwrap_or_else(|_| panic!("take_device should only be called once"));
        std::mem::drop(self);
        debug_assert_not_too_long!(receiver).unwrap()
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
    on_new_allocator: Option<Box<dyn Fn(Arc<SimpleAllocator>) + Send + Sync>>,
    on_new_store: Option<Box<dyn Fn(&ObjectStore) + Send + Sync>>,
    fsck_after_every_transaction: bool,
    background_task_spawner: Box<dyn Fn(futures::future::BoxFuture<'static, ()>) + Send + Sync>,
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
            background_task_spawner: Box::new(Self::default_background_task_spawner),
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
        on_new_allocator: impl Fn(Arc<SimpleAllocator>) + Send + Sync + 'static,
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

    /// Sets the function to use to spawn background tasks.
    pub fn background_task_spawner(
        mut self,
        background_task_spawner: impl Fn(futures::future::BoxFuture<'static, ()>)
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.background_task_spawner = Box::new(background_task_spawner);
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

        let filesystem = Arc::new(FxFilesystem {
            device: OnceCell::new(),
            block_size,
            objects: objects.clone(),
            journal,
            commit_mutex: futures::lock::Mutex::new(()),
            lock_manager: LockManager::new(),
            flush_task: Mutex::new(None),
            device_sender: OnceCell::new(),
            closed: AtomicBool::new(true),
            trace: self.trace,
            graveyard: Graveyard::new(objects.clone()),
            completed_transactions: metrics::detail().create_uint("completed_transactions", 0),
            options: filesystem_options,
            in_flight_transactions: AtomicU64::new(0),
            event: Event::new(),
            background_task_spawner: self.background_task_spawner,
        });

        if let Some(fsck_after_every_transaction) = fsck_after_every_transaction {
            fsck_after_every_transaction
                .fs
                .set(Arc::downgrade(&filesystem))
                .unwrap_or_else(|_| unreachable!());
        }

        if !read_only && !self.format {
            // See comment in JournalRecord::DidFlushDevice for why we need to flush the device
            // before replay.
            device.flush().await.context("Device flush failed")?;
        }
        filesystem.device.set(device).unwrap_or_else(|_| unreachable!());

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
                    &[LockKey::object(root_store.store_object_id(), root_directory.object_id())],
                    transaction::Options::default(),
                )
                .await?;
            let volume_directory = root_directory
                .create_child_dir(&mut transaction, VOLUMES_DIRECTORY, Default::default())
                .await?;
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
                // Now start the async reaper.
                filesystem.graveyard.clone().reap_async();
            }
        }

        filesystem.closed.store(false, Ordering::SeqCst);
        Ok(filesystem.into())
    }

    fn default_background_task_spawner(future: futures::future::BoxFuture<'static, ()>) {
        fasync::Task::spawn(future).detach();
    }
}

pub struct FxFilesystem {
    device: OnceCell<DeviceHolder>,
    block_size: u64,
    objects: Arc<ObjectManager>,
    journal: Arc<Journal>,
    commit_mutex: futures::lock::Mutex<()>,
    lock_manager: LockManager,
    flush_task: Mutex<Option<fasync::Task<()>>>,
    device_sender: OnceCell<Sender<DeviceHolder>>,
    closed: AtomicBool,
    trace: bool,
    graveyard: Arc<Graveyard>,
    completed_transactions: UintProperty,
    options: Options,

    // The number of in-flight transactions which we will limit to MAX_IN_FLIGHT_TRANSACTIONS.
    in_flight_transactions: AtomicU64,

    // An event that is used to wake up tasks that are blocked due to the in-flight transaction
    // limit.
    event: Event,

    background_task_spawner: Box<dyn Fn(futures::future::BoxFuture<'static, ()>) + Send + Sync>,
}

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

    fn root_store(&self) -> Arc<ObjectStore> {
        self.objects.root_store()
    }

    pub async fn close(&self) -> Result<(), Error> {
        assert_eq!(self.closed.swap(true, Ordering::SeqCst), false);
        debug_assert_not_too_long!(self.graveyard.wait_for_reap());
        self.journal.stop_compactions().await;
        let sync_status =
            self.journal.sync(SyncOptions { flush_device: true, ..Default::default() }).await;
        if let Err(e) = &sync_status {
            error!(error = e.as_value(), "Failed to sync filesystem; data may be lost");
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

    async fn reservation_for_transaction<'a>(
        self: &Arc<Self>,
        options: transaction::Options<'a>,
    ) -> Result<(MetadataReservation, Option<&'a Reservation>, Option<Hold<'a>>), Error> {
        if !options.skip_journal_checks {
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

    async fn add_transaction(&self, skip_journal_checks: bool) {
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
                let listener = self.event.listen();
                if inc() {
                    break;
                }
                listener.await;
            }
        }
    }

    fn sub_transaction(&self) {
        let old = self.in_flight_transactions.fetch_sub(1, Ordering::Relaxed);
        assert!(old != 0);
        if old <= MAX_IN_FLIGHT_TRANSACTIONS {
            self.event.notify(usize::MAX);
        }
    }
}

impl Drop for FxFilesystem {
    fn drop(&mut self) {
        if let Some(sender) = self.device_sender.take() {
            // We don't care if this fails to send.
            let _ = sender.send(self.device.take().unwrap());
        }
    }
}

#[async_trait]
impl Filesystem for FxFilesystem {
    fn device(&self) -> Arc<dyn Device> {
        Arc::clone(self.device.get().unwrap())
    }

    fn root_store(&self) -> Arc<ObjectStore> {
        self.objects.root_store()
    }

    fn allocator(&self) -> Arc<SimpleAllocator> {
        self.objects.allocator()
    }

    fn object_manager(&self) -> &Arc<ObjectManager> {
        &self.objects
    }

    fn journal(&self) -> &Arc<Journal> {
        &self.journal
    }

    async fn sync(&self, options: SyncOptions<'_>) -> Result<(), Error> {
        self.journal.sync(options).await.map(|_| ())
    }

    fn block_size(&self) -> u64 {
        self.block_size
    }

    fn get_info(&self) -> Info {
        Info {
            total_bytes: self.device.get().unwrap().size(),
            used_bytes: self.object_manager().allocator().get_used_bytes(),
        }
    }

    fn super_block_header(&self) -> SuperBlockHeader {
        self.journal.super_block_header()
    }

    fn graveyard(&self) -> &Arc<Graveyard> {
        &self.graveyard
    }

    fn trace(&self) -> bool {
        self.trace
    }

    fn options(&self) -> &Options {
        &self.options
    }

    fn spawn_background_task(&self, future: futures::future::BoxFuture<'static, ()>) {
        (self.background_task_spawner)(future);
    }
}

#[async_trait]
impl TransactionHandler for FxFilesystem {
    async fn new_transaction<'a>(
        self: Arc<Self>,
        locks: &[LockKey],
        options: transaction::Options<'a>,
    ) -> Result<Transaction<'a>, Error> {
        self.add_transaction(options.skip_journal_checks).await;
        let guard = scopeguard::guard((), |_| self.sub_transaction());
        let (metadata_reservation, allocator_reservation, hold) =
            self.reservation_for_transaction(options).await?;
        let mut transaction =
            Transaction::new(self.clone(), metadata_reservation, &[LockKey::Filesystem], locks)
                .await;

        ScopeGuard::into_inner(guard);
        hold.map(|h| h.forget()); // Transaction takes ownership from here on.
        transaction.allocator_reservation = allocator_reservation;
        Ok(transaction)
    }

    async fn transaction_lock<'a>(&'a self, lock_keys: &[LockKey]) -> TransactionLocks<'a> {
        let lock_manager: &LockManager = self.as_ref();
        TransactionLocks(debug_assert_not_too_long!(lock_manager.txn_lock(lock_keys)))
    }

    async fn commit_transaction(
        self: Arc<Self>,
        transaction: &mut Transaction<'_>,
        callback: &mut (dyn FnMut(u64) + Send),
    ) -> Result<u64, Error> {
        trace_duration!("FxFilesystem::commit_transaction");
        debug_assert_not_too_long!(self.lock_manager.commit_prepare(&transaction));
        {
            let mut flush_task = self.flush_task.lock().unwrap();
            if flush_task.is_none() {
                let this = self.clone();
                *flush_task = Some(fasync::Task::spawn(async move {
                    this.journal.flush_task().await;
                }));
            }
        }
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

    fn drop_transaction(&self, transaction: &mut Transaction<'_>) {
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

    async fn read_lock<'a>(&'a self, lock_keys: &[LockKey]) -> ReadGuard<'a> {
        debug_assert_not_too_long!(self.lock_manager.read_lock(lock_keys))
    }

    async fn write_lock<'a>(&'a self, lock_keys: &[LockKey]) -> WriteGuard<'a> {
        debug_assert_not_too_long!(self.lock_manager.write_lock(lock_keys))
    }
}

impl AsRef<LockManager> for FxFilesystem {
    fn as_ref(&self) -> &LockManager {
        &self.lock_manager
    }
}

/// Helper method for making a new filesystem.
pub async fn mkfs(device: DeviceHolder) -> Result<(), Error> {
    let fs = FxFilesystem::new_empty(device).await?;
    fs.close().await
}

/// Helper method for making a new filesystem with a default volume.
/// This shouldn't be used in production; instead volumes should be created with the Volumes
/// protocol.
pub async fn mkfs_with_default(
    device: DeviceHolder,
    crypt: Option<Arc<dyn Crypt>>,
) -> Result<(), Error> {
    let fs = FxFilesystem::new_empty(device).await?;
    {
        // expect instead of propagating errors here, since otherwise we could drop |fs| before
        // close is called, which leads to confusing and unrelated error messages.
        let root_volume = root_volume(fs.clone()).await.expect("Open root_volume failed");
        root_volume.new_volume("default", crypt).await.expect("Create volume failed");
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
    use {
        super::{Filesystem, FxFilesystem, FxFilesystemBuilder, SyncOptions},
        crate::{
            fsck::fsck,
            lsm_tree::{types::Item, Operation},
            object_handle::{ObjectHandle, WriteObjectHandle},
            object_store::{
                directory::replace_child,
                directory::Directory,
                journal::JournalOptions,
                transaction::{LockKey, Options, TransactionHandler},
            },
        },
        fuchsia_async as fasync,
        futures::future::join_all,
        std::{
            collections::HashMap,
            sync::{Arc, Mutex},
        },
        storage_device::{fake_device::FakeDevice, DeviceHolder},
    };

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
                    &[LockKey::object(root_store.store_object_id(), root_directory.object_id())],
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
                let mut buf = handle.allocate_buffer(TEST_DATA.len());
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
        let object_mutations = Arc::new(Mutex::new(HashMap::new()));
        let fs = open_fs(device, object_mutations.clone(), allocator_mutations.clone()).await;

        let root_store = fs.root_store();
        let root_directory = Directory::open(&root_store, root_store.root_directory_object_id())
            .await
            .expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(root_store.store_object_id(), root_directory.object_id())],
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
        let buf = object.allocate_buffer(10000);
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");

        // Overwrite some data.
        object.write_or_append(Some(5000), buf.as_ref()).await.expect("write failed");

        // Truncate.
        (&object as &dyn WriteObjectHandle).truncate(3000).await.expect("truncate failed");

        // Delete the object.
        let mut transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(root_store.store_object_id(), root_directory.object_id())],
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
            .tombstone(object.object_id(), Options::default())
            .await
            .expect("tombstone failed");

        // Now reopen and check that replay produces the same set of mutations.
        fs.close().await.expect("close failed");

        let metadata_reservation_amount = fs.object_manager().metadata_reservation().amount();

        let device = fs.take_device().await;
        device.reopen(false);

        let replayed_object_mutations = Arc::new(Mutex::new(HashMap::new()));
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

        let mut transactions = Vec::new();
        for _ in 0..super::MAX_IN_FLIGHT_TRANSACTIONS {
            transactions.push(fs.clone().new_transaction(&[], Options::default()).await);
        }

        // Trying to create another one should be blocked.
        let mut fut = fs.clone().new_transaction(&[], Options::default());
        assert!(futures::poll!(&mut fut).is_pending());

        // Dropping one should allow it to proceed.
        transactions.pop();

        assert!(futures::poll!(&mut fut).is_ready());
    }
}
