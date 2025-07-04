// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The journal is implemented as an ever extending file which contains variable length records
//! that describe mutations to be applied to various objects.  The journal file consists of
//! blocks, with a checksum at the end of each block, but otherwise it can be considered a
//! continuous stream.
//!
//! The checksum is seeded with the checksum from the previous block.  To free space in the
//! journal, records are replaced with sparse extents when it is known they are no longer
//! needed to mount.
//!
//! At mount time, the journal is replayed: the mutations are applied into memory.
//! Eventually, a checksum failure will indicate no more records exist to be replayed,
//! at which point the mount can continue and the journal will be extended from that point with
//! further mutations as required.

mod bootstrap_handle;
mod checksum_list;
mod reader;
pub mod super_block;
mod writer;

use crate::checksum::{Checksum, Checksums, ChecksumsV38};
use crate::debug_assert_not_too_long;
use crate::errors::FxfsError;
use crate::filesystem::{ApplyContext, ApplyMode, FxFilesystem, SyncOptions};
use crate::log::*;
use crate::lsm_tree::cache::NullCache;
use crate::lsm_tree::types::Layer;
use crate::object_handle::{ObjectHandle as _, ReadObjectHandle};
use crate::object_store::allocator::Allocator;
use crate::object_store::data_object_handle::OverwriteOptions;
use crate::object_store::extent_record::{
    ExtentKey, ExtentMode, ExtentValue, DEFAULT_DATA_ATTRIBUTE_ID,
};
use crate::object_store::graveyard::Graveyard;
use crate::object_store::journal::bootstrap_handle::BootstrapObjectHandle;
use crate::object_store::journal::checksum_list::ChecksumList;
use crate::object_store::journal::reader::{JournalReader, ReadResult};
use crate::object_store::journal::super_block::{
    SuperBlockHeader, SuperBlockInstance, SuperBlockManager,
};
use crate::object_store::journal::writer::JournalWriter;
use crate::object_store::object_manager::ObjectManager;
use crate::object_store::object_record::{AttributeKey, ObjectKey, ObjectKeyData, ObjectValue};
use crate::object_store::transaction::{
    lock_keys, AllocatorMutation, LockKey, Mutation, MutationV40, MutationV41, MutationV43,
    MutationV46, MutationV47, ObjectStoreMutation, Options, Transaction, TxnMutation,
    TRANSACTION_MAX_JOURNAL_USAGE,
};
use crate::object_store::{
    AssocObj, DataObjectHandle, HandleOptions, HandleOwner, Item, ItemRef, NewChildStoreOptions,
    ObjectStore, INVALID_OBJECT_ID,
};
use crate::range::RangeExt;
use crate::round::{round_div, round_down};
use crate::serialized_types::{migrate_to_version, Migrate, Version, Versioned, LATEST_VERSION};
use anyhow::{anyhow, bail, ensure, Context, Error};
use event_listener::Event;
use fprint::TypeFingerprint;
use fuchsia_sync::Mutex;
use futures::future::poll_fn;
use futures::FutureExt as _;
use once_cell::sync::OnceCell;
use rand::Rng;
use rustc_hash::FxHashMap as HashMap;
use serde::{Deserialize, Serialize};
use static_assertions::const_assert;
use std::clone::Clone;
use std::collections::HashSet;
use std::ops::{Bound, Range};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Poll, Waker};

// The journal file is written to in blocks of this size.
pub const BLOCK_SIZE: u64 = 4096;

// The journal file is extended by this amount when necessary.
const CHUNK_SIZE: u64 = 131_072;
const_assert!(CHUNK_SIZE > TRANSACTION_MAX_JOURNAL_USAGE);

// See the comment for the `reclaim_size` member of Inner.
pub const DEFAULT_RECLAIM_SIZE: u64 = 262_144;

// Temporary space that should be reserved for the journal.  For example: space that is currently
// used in the journal file but cannot be deallocated yet because we are flushing.
pub const RESERVED_SPACE: u64 = 1_048_576;

// Whenever the journal is replayed (i.e. the system is unmounted and remounted), we reset the
// journal stream, at which point any half-complete transactions are discarded.  We indicate a
// journal reset by XORing the previous block's checksum with this mask, and using that value as a
// seed for the next journal block.
const RESET_XOR: u64 = 0xffffffffffffffff;

// To keep track of offsets within a journal file, we need both the file offset and the check-sum of
// the preceding block, since the check-sum of the preceding block is an input to the check-sum of
// every block.
pub type JournalCheckpoint = JournalCheckpointV32;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TypeFingerprint)]
pub struct JournalCheckpointV32 {
    pub file_offset: u64,

    // Starting check-sum for block that contains file_offset i.e. the checksum for the previous
    // block.
    pub checksum: Checksum,

    // If versioned, the version of elements stored in the journal. e.g. JournalRecord version.
    // This can change across reset events so we store it along with the offset and checksum to
    // know which version to deserialize.
    pub version: Version,
}

pub type JournalRecord = JournalRecordV47;

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Serialize, Deserialize, TypeFingerprint, Versioned)]
#[cfg_attr(fuzz, derive(arbitrary::Arbitrary))]
pub enum JournalRecordV47 {
    // Indicates no more records in this block.
    EndBlock,
    // Mutation for a particular object.  object_id here is for the collection i.e. the store or
    // allocator.
    Mutation { object_id: u64, mutation: MutationV47 },
    // Commits records in the transaction.
    Commit,
    // Discard all mutations with offsets greater than or equal to the given offset.
    Discard(u64),
    // Indicates the device was flushed at the given journal offset.
    // Note that this really means that at this point in the journal offset, we can be certain that
    // there's no remaining buffered data in the block device; the buffers and the disk contents are
    // consistent.
    // We insert one of these records *after* a flush along with the *next* transaction to go
    // through.  If that never comes (either due to graceful or hard shutdown), the journal reset
    // on the next mount will serve the same purpose and count as a flush, although it is necessary
    // to defensively flush the device before replaying the journal (if possible, i.e. not
    // read-only) in case the block device connection was reused.
    DidFlushDevice(u64),
    // Checksums for a data range written by this transaction. A transaction is only valid if these
    // checksums are right. The range is the device offset the checksums are for.
    //
    // A boolean indicates whether this range is being written to for the first time. For overwrite
    // extents, we only check the checksums for a block if it has been written to for the first
    // time since the last flush, because otherwise we can't roll it back anyway so it doesn't
    // matter. For copy-on-write extents, the bool is always true.
    DataChecksums(Range<u64>, ChecksumsV38, bool),
}

#[allow(clippy::large_enum_variant)]
#[derive(Migrate, Serialize, Deserialize, TypeFingerprint, Versioned)]
#[migrate_to_version(JournalRecordV47)]
pub enum JournalRecordV46 {
    EndBlock,
    Mutation { object_id: u64, mutation: MutationV46 },
    Commit,
    Discard(u64),
    DidFlushDevice(u64),
    DataChecksums(Range<u64>, ChecksumsV38, bool),
}

#[allow(clippy::large_enum_variant)]
#[derive(Migrate, Serialize, Deserialize, TypeFingerprint, Versioned)]
#[migrate_to_version(JournalRecordV46)]
pub enum JournalRecordV43 {
    EndBlock,
    Mutation { object_id: u64, mutation: MutationV43 },
    Commit,
    Discard(u64),
    DidFlushDevice(u64),
    DataChecksums(Range<u64>, ChecksumsV38, bool),
}

#[derive(Migrate, Serialize, Deserialize, TypeFingerprint, Versioned)]
#[migrate_to_version(JournalRecordV43)]
pub enum JournalRecordV42 {
    EndBlock,
    Mutation { object_id: u64, mutation: MutationV41 },
    Commit,
    Discard(u64),
    DidFlushDevice(u64),
    DataChecksums(Range<u64>, ChecksumsV38, bool),
}

#[derive(Serialize, Deserialize, TypeFingerprint, Versioned)]
pub enum JournalRecordV41 {
    EndBlock,
    Mutation { object_id: u64, mutation: MutationV41 },
    Commit,
    Discard(u64),
    DidFlushDevice(u64),
    DataChecksums(Range<u64>, ChecksumsV38),
}

impl From<JournalRecordV41> for JournalRecordV42 {
    fn from(record: JournalRecordV41) -> Self {
        match record {
            JournalRecordV41::EndBlock => Self::EndBlock,
            JournalRecordV41::Mutation { object_id, mutation } => {
                Self::Mutation { object_id, mutation: mutation.into() }
            }
            JournalRecordV41::Commit => Self::Commit,
            JournalRecordV41::Discard(offset) => Self::Discard(offset),
            JournalRecordV41::DidFlushDevice(offset) => Self::DidFlushDevice(offset),
            JournalRecordV41::DataChecksums(range, sums) => {
                // At the time of writing the only extents written by real systems are CoW extents
                // so the new bool is always true.
                Self::DataChecksums(range, sums, true)
            }
        }
    }
}

#[derive(Migrate, Serialize, Deserialize, TypeFingerprint, Versioned)]
#[migrate_to_version(JournalRecordV41)]
pub enum JournalRecordV40 {
    EndBlock,
    Mutation { object_id: u64, mutation: MutationV40 },
    Commit,
    Discard(u64),
    DidFlushDevice(u64),
    DataChecksums(Range<u64>, ChecksumsV38),
}

pub(super) fn journal_handle_options() -> HandleOptions {
    HandleOptions { skip_journal_checks: true, ..Default::default() }
}

/// The journal records a stream of mutations that are to be applied to other objects.  At mount
/// time, these records can be replayed into memory.  It provides a way to quickly persist changes
/// without having to make a large number of writes; they can be deferred to a later time (e.g.
/// when a sufficient number have been queued).  It also provides support for transactions, the
/// ability to have mutations that are to be applied atomically together.
pub struct Journal {
    objects: Arc<ObjectManager>,
    handle: OnceCell<DataObjectHandle<ObjectStore>>,
    super_block_manager: SuperBlockManager,
    inner: Mutex<Inner>,
    writer_mutex: Mutex<()>,
    sync_mutex: futures::lock::Mutex<()>,
    trace: AtomicBool,

    // This event is used when we are waiting for a compaction to free up journal space.
    reclaim_event: Event,
}

struct Inner {
    super_block_header: SuperBlockHeader,

    // The offset that we can zero the journal up to now that it is no longer needed.
    zero_offset: Option<u64>,

    // The journal offset that we most recently flushed to the device.
    device_flushed_offset: u64,

    // If true, indicates a DidFlushDevice record is pending.
    needs_did_flush_device: bool,

    // The writer for the journal.
    writer: JournalWriter,

    // Set when a reset is encountered during a read.
    // Used at write pre_commit() time to ensure we write a version first thing after a reset.
    output_reset_version: bool,

    // Waker for the flush task.
    flush_waker: Option<Waker>,

    // Indicates the journal has been terminated.
    terminate: bool,

    // Latched error indicating reason for journal termination if not graceful.
    terminate_reason: Option<Error>,

    // Disable compactions.
    disable_compactions: bool,

    // True if compactions are running.
    compaction_running: bool,

    // Waker for the sync task for when it's waiting for the flush task to finish.
    sync_waker: Option<Waker>,

    // The last offset we flushed to the journal file.
    flushed_offset: u64,

    // The last offset that should be considered valid in the journal file.  Most of the time, this
    // will be the same as `flushed_offset` but at mount time, this could be less and will only be
    // up to the end of the last valid transaction; it won't include transactions that follow that
    // have been discarded.
    valid_to: u64,

    // If, after replaying, we have to discard a number of mutations (because they don't validate),
    // this offset specifies where we need to discard back to.  This is so that when we next replay,
    // we ignore those mutations and continue with new good mutations.
    discard_offset: Option<u64>,

    // In the steady state, the journal should fluctuate between being approximately half of this
    // number and this number.  New super-blocks will be written every time about half of this
    // amount is written to the journal.
    reclaim_size: u64,

    image_builder_mode: Option<SuperBlockInstance>,

    // If true and `needs_barrier`, issue a pre-barrier on the first device write of each journal
    // write (which happens in multiples of `BLOCK_SIZE`). This ensures that all the corresponding
    // data writes make it to disk before the journal gets written to.
    barriers_enabled: bool,

    // If true, indicates that data write requests have been made to the device since the last
    // journal write.
    needs_barrier: bool,
}

impl Inner {
    fn terminate(&mut self, reason: Option<Error>) {
        self.terminate = true;

        if let Some(err) = reason {
            error!(error:? = err; "Terminating journal");
            // Log previous error if one was already set, otherwise latch the error.
            if let Some(prev_err) = self.terminate_reason.as_ref() {
                error!(error:? = prev_err; "Journal previously terminated");
            } else {
                self.terminate_reason = Some(err);
            }
        }

        if let Some(waker) = self.flush_waker.take() {
            waker.wake();
        }
        if let Some(waker) = self.sync_waker.take() {
            waker.wake();
        }
    }
}

pub struct JournalOptions {
    /// In the steady state, the journal should fluctuate between being approximately half of this
    /// number and this number.  New super-blocks will be written every time about half of this
    /// amount is written to the journal.
    pub reclaim_size: u64,

    // If true, issue a pre-barrier on the first device write of each journal write (which happens
    // in multiples of `BLOCK_SIZE`). This ensures that all the corresponding data writes make it
    // to disk before the journal gets written to.
    pub barriers_enabled: bool,
}

impl Default for JournalOptions {
    fn default() -> Self {
        JournalOptions { reclaim_size: DEFAULT_RECLAIM_SIZE, barriers_enabled: false }
    }
}

struct JournaledTransactions {
    transactions: Vec<JournaledTransaction>,
    device_flushed_offset: u64,
}

#[derive(Debug, Default)]
pub struct JournaledTransaction {
    pub checkpoint: JournalCheckpoint,
    pub root_parent_mutations: Vec<Mutation>,
    pub root_mutations: Vec<Mutation>,
    // List of (store_object_id, mutation).
    pub non_root_mutations: Vec<(u64, Mutation)>,
    pub end_offset: u64,
    pub checksums: Vec<JournaledChecksums>,

    // Records offset + 1 of the matching begin_flush transaction.  If the object is deleted, the
    // offset will be STORE_DELETED.  The +1 is because we want to ignore the begin flush
    // transaction; we don't need or want to replay it.
    pub end_flush: Option<(/* store_id: */ u64, /* begin offset: */ u64)>,
}

impl JournaledTransaction {
    fn new(checkpoint: JournalCheckpoint) -> Self {
        Self { checkpoint, ..Default::default() }
    }
}

const STORE_DELETED: u64 = u64::MAX;

#[derive(Debug)]
pub struct JournaledChecksums {
    pub device_range: Range<u64>,
    pub checksums: Checksums,
    pub first_write: bool,
}

/// Handles for journal-like objects have some additional functionality to manage their extents,
/// since during replay we need to add extents as we find them.
pub trait JournalHandle: ReadObjectHandle {
    /// The end offset of the last extent in the JournalHandle.  Used only for validating extents
    /// (which will be skipped if None is returned).
    /// Note this is equivalent in value to ReadObjectHandle::get_size, when present.
    fn end_offset(&self) -> Option<u64>;
    /// Adds an extent to the current end of the journal stream.
    /// `added_offset` is the offset into the journal of the transaction which added this extent,
    /// used for discard_extents.
    fn push_extent(&mut self, added_offset: u64, device_range: Range<u64>);
    /// Discards all extents which were added in a transaction at offset >= |discard_offset|.
    fn discard_extents(&mut self, discard_offset: u64);
}

// Provide a stub implementation for DataObjectHandle so we can use it in
// Journal::read_transactions.  Manual extent management is a NOP (which is OK since presumably the
// DataObjectHandle already knows where its extents live).
impl<S: HandleOwner> JournalHandle for DataObjectHandle<S> {
    fn end_offset(&self) -> Option<u64> {
        None
    }
    fn push_extent(&mut self, _added_offset: u64, _device_range: Range<u64>) {
        // NOP
    }
    fn discard_extents(&mut self, _discard_offset: u64) {
        // NOP
    }
}

#[fxfs_trace::trace]
impl Journal {
    pub fn new(objects: Arc<ObjectManager>, options: JournalOptions) -> Journal {
        let starting_checksum = rand::thread_rng().gen_range(1..u64::MAX);
        Journal {
            objects: objects,
            handle: OnceCell::new(),
            super_block_manager: SuperBlockManager::new(),
            inner: Mutex::new(Inner {
                super_block_header: SuperBlockHeader::default(),
                zero_offset: None,
                device_flushed_offset: 0,
                needs_did_flush_device: false,
                writer: JournalWriter::new(BLOCK_SIZE as usize, starting_checksum),
                output_reset_version: false,
                flush_waker: None,
                terminate: false,
                terminate_reason: None,
                disable_compactions: false,
                compaction_running: false,
                sync_waker: None,
                flushed_offset: 0,
                valid_to: 0,
                discard_offset: None,
                reclaim_size: options.reclaim_size,
                image_builder_mode: None,
                barriers_enabled: options.barriers_enabled,
                needs_barrier: false,
            }),
            writer_mutex: Mutex::new(()),
            sync_mutex: futures::lock::Mutex::new(()),
            trace: AtomicBool::new(false),
            reclaim_event: Event::new(),
        }
    }

    pub fn set_trace(&self, trace: bool) {
        let old_value = self.trace.swap(trace, Ordering::Relaxed);
        if trace != old_value {
            info!(trace; "J: trace");
        }
    }

    pub fn set_image_builder_mode(&self, mode: Option<SuperBlockInstance>) {
        self.inner.lock().image_builder_mode = mode;
        if let Some(instance) = mode {
            *self.super_block_manager.next_instance.lock() = instance;
        }
    }

    pub fn image_builder_mode(&self) -> Option<SuperBlockInstance> {
        self.inner.lock().image_builder_mode
    }

    #[cfg(feature = "migration")]
    pub fn set_filesystem_uuid(&self, uuid: &[u8; 16]) -> Result<(), Error> {
        ensure!(
            self.inner.lock().image_builder_mode.is_some(),
            "Can only set filesystem uuid in image builder mode."
        );
        self.inner.lock().super_block_header.guid.0 = uuid::Uuid::from_bytes(*uuid);
        Ok(())
    }

    /// Used during replay to validate a mutation.  This should return false if the mutation is not
    /// valid and should not be applied.  This could be for benign reasons: e.g. the device flushed
    /// data out-of-order, or because of a malicious actor.
    fn validate_mutation(&self, mutation: &Mutation, block_size: u64, device_size: u64) -> bool {
        match mutation {
            Mutation::ObjectStore(ObjectStoreMutation {
                item:
                    Item {
                        key:
                            ObjectKey {
                                data:
                                    ObjectKeyData::Attribute(
                                        _,
                                        AttributeKey::Extent(ExtentKey { range }),
                                    ),
                                ..
                            },
                        value: ObjectValue::Extent(ExtentValue::Some { device_offset, mode, .. }),
                        ..
                    },
                ..
            }) => {
                if range.is_empty() || !range.is_aligned(block_size) {
                    return false;
                }
                let len = range.length().unwrap();
                if let ExtentMode::Cow(checksums) = mode {
                    if checksums.len() > 0 {
                        if len % checksums.len() as u64 != 0 {
                            return false;
                        }
                        if (len / checksums.len() as u64) % block_size != 0 {
                            return false;
                        }
                    }
                }
                if *device_offset % block_size != 0
                    || *device_offset >= device_size
                    || device_size - *device_offset < len
                {
                    return false;
                }
            }
            Mutation::ObjectStore(_) => {}
            Mutation::EncryptedObjectStore(_) => {}
            Mutation::Allocator(AllocatorMutation::Allocate { device_range, owner_object_id }) => {
                return !device_range.is_empty()
                    && *owner_object_id != INVALID_OBJECT_ID
                    && device_range.end <= device_size;
            }
            Mutation::Allocator(AllocatorMutation::Deallocate {
                device_range,
                owner_object_id,
            }) => {
                return !device_range.is_empty()
                    && *owner_object_id != INVALID_OBJECT_ID
                    && device_range.end <= device_size;
            }
            Mutation::Allocator(AllocatorMutation::MarkForDeletion(owner_object_id)) => {
                return *owner_object_id != INVALID_OBJECT_ID;
            }
            Mutation::Allocator(AllocatorMutation::SetLimit { owner_object_id, .. }) => {
                return *owner_object_id != INVALID_OBJECT_ID;
            }
            Mutation::BeginFlush => {}
            Mutation::EndFlush => {}
            Mutation::DeleteVolume => {}
            Mutation::UpdateBorrowed(_) => {}
            Mutation::UpdateMutationsKey(_) => {}
            Mutation::CreateInternalDir(owner_object_id) => {
                return *owner_object_id != INVALID_OBJECT_ID;
            }
        }
        true
    }

    // Assumes that `mutation` has been validated.
    fn update_checksum_list(
        &self,
        journal_offset: u64,
        mutation: &Mutation,
        checksum_list: &mut ChecksumList,
    ) -> Result<(), Error> {
        match mutation {
            Mutation::ObjectStore(_) => {}
            Mutation::Allocator(AllocatorMutation::Deallocate { device_range, .. }) => {
                checksum_list.mark_deallocated(journal_offset, device_range.clone().into());
            }
            _ => {}
        }
        Ok(())
    }

    /// Reads the latest super-block, and then replays journaled records.
    #[trace]
    pub async fn replay(
        &self,
        filesystem: Arc<FxFilesystem>,
        on_new_allocator: Option<Box<dyn Fn(Arc<Allocator>) + Send + Sync>>,
    ) -> Result<(), Error> {
        let block_size = filesystem.block_size();

        let (super_block, root_parent) =
            self.super_block_manager.load(filesystem.device(), block_size).await?;

        let root_parent = Arc::new(ObjectStore::attach_filesystem(root_parent, filesystem.clone()));

        self.objects.set_root_parent_store(root_parent.clone());
        let allocator =
            Arc::new(Allocator::new(filesystem.clone(), super_block.allocator_object_id));
        if let Some(on_new_allocator) = on_new_allocator {
            on_new_allocator(allocator.clone());
        }
        self.objects.set_allocator(allocator.clone());
        self.objects.set_borrowed_metadata_space(super_block.borrowed_metadata_space);
        self.objects.set_last_end_offset(super_block.super_block_journal_file_offset);
        {
            let mut inner = self.inner.lock();
            inner.super_block_header = super_block.clone();
        }

        let device = filesystem.device();

        let mut handle;
        {
            let root_parent_layer = root_parent.tree().mutable_layer();
            let mut iter = root_parent_layer
                .seek(Bound::Included(&ObjectKey::attribute(
                    super_block.journal_object_id,
                    DEFAULT_DATA_ATTRIBUTE_ID,
                    AttributeKey::Extent(ExtentKey::search_key_from_offset(round_down(
                        super_block.journal_checkpoint.file_offset,
                        BLOCK_SIZE,
                    ))),
                )))
                .await
                .context("Failed to seek root parent store")?;
            let start_offset = if let Some(ItemRef {
                key:
                    ObjectKey {
                        data:
                            ObjectKeyData::Attribute(
                                DEFAULT_DATA_ATTRIBUTE_ID,
                                AttributeKey::Extent(ExtentKey { range }),
                            ),
                        ..
                    },
                ..
            }) = iter.get()
            {
                range.start
            } else {
                0
            };
            handle = BootstrapObjectHandle::new_with_start_offset(
                super_block.journal_object_id,
                device.clone(),
                start_offset,
            );
            while let Some(item) = iter.get() {
                if !match item.into() {
                    Some((
                        object_id,
                        DEFAULT_DATA_ATTRIBUTE_ID,
                        ExtentKey { range },
                        ExtentValue::Some { device_offset, .. },
                    )) if object_id == super_block.journal_object_id => {
                        if let Some(end_offset) = handle.end_offset() {
                            if range.start != end_offset {
                                bail!(anyhow!(FxfsError::Inconsistent).context(format!(
                                    "Unexpected journal extent {:?}, expected start: {}",
                                    item, end_offset
                                )));
                            }
                        }
                        handle.push_extent(
                            0, // We never discard extents from the root parent store.
                            *device_offset
                                ..*device_offset + range.length().context("Invalid extent")?,
                        );
                        true
                    }
                    _ => false,
                } {
                    break;
                }
                iter.advance().await.context("Failed to advance root parent store iterator")?;
            }
        }

        let mut reader = JournalReader::new(handle, &super_block.journal_checkpoint);
        let JournaledTransactions { mut transactions, device_flushed_offset } = self
            .read_transactions(&mut reader, None, INVALID_OBJECT_ID)
            .await
            .context("Reading transactions for replay")?;

        // Validate all the mutations.
        let mut checksum_list = ChecksumList::new(device_flushed_offset);
        let mut valid_to = reader.journal_file_checkpoint().file_offset;
        let device_size = device.size();
        'bad_replay: for JournaledTransaction {
            checkpoint,
            root_parent_mutations,
            root_mutations,
            non_root_mutations,
            checksums,
            ..
        } in &transactions
        {
            for JournaledChecksums { device_range, checksums, first_write } in checksums {
                checksum_list
                    .push(
                        checkpoint.file_offset,
                        device_range.clone(),
                        checksums.maybe_as_ref().context("Malformed checksums")?,
                        *first_write,
                    )
                    .context("Pushing journal checksum records to checksum list")?;
            }
            for mutation in root_parent_mutations
                .iter()
                .chain(root_mutations)
                .chain(non_root_mutations.iter().map(|(_, m)| m))
            {
                if !self.validate_mutation(mutation, block_size, device_size) {
                    info!(mutation:?; "Stopping replay at bad mutation");
                    valid_to = checkpoint.file_offset;
                    break 'bad_replay;
                }
                self.update_checksum_list(checkpoint.file_offset, &mutation, &mut checksum_list)?;
            }
        }

        // Validate the checksums. Note if barriers are enabled, there will be no checksums in
        // practice to verify.
        let valid_to = checksum_list
            .verify(device.as_ref(), valid_to)
            .await
            .context("Failed to validate checksums")?;

        // Apply the mutations...

        let mut last_checkpoint = reader.journal_file_checkpoint();
        let mut journal_offsets = super_block.journal_file_offsets.clone();

        // Start with the root-parent mutations, and also determine the journal offsets for all
        // other objects.
        for (index, JournaledTransaction { checkpoint, root_parent_mutations, end_flush, .. }) in
            transactions.iter_mut().enumerate()
        {
            if checkpoint.file_offset >= valid_to {
                last_checkpoint = checkpoint.clone();

                // Truncate the transactions so we don't need to worry about them on the next pass.
                transactions.truncate(index);
                break;
            }

            let context = ApplyContext { mode: ApplyMode::Replay, checkpoint: checkpoint.clone() };
            for mutation in root_parent_mutations.drain(..) {
                self.objects
                    .apply_mutation(
                        super_block.root_parent_store_object_id,
                        mutation,
                        &context,
                        AssocObj::None,
                    )
                    .context("Failed to replay root parent store mutations")?;
            }

            if let Some((object_id, journal_offset)) = end_flush {
                journal_offsets.insert(*object_id, *journal_offset);
            }
        }

        // Now we can open the root store.
        let root_store = ObjectStore::open(
            &root_parent,
            super_block.root_store_object_id,
            Box::new(NullCache {}),
        )
        .await
        .context("Unable to open root store")?;

        ensure!(
            !root_store.is_encrypted(),
            anyhow!(FxfsError::Inconsistent).context("Root store is encrypted")
        );
        self.objects.set_root_store(root_store);

        let root_store_offset =
            journal_offsets.get(&super_block.root_store_object_id).copied().unwrap_or(0);

        // Now replay the root store mutations.
        for JournaledTransaction { checkpoint, root_mutations, .. } in &mut transactions {
            if checkpoint.file_offset < root_store_offset {
                continue;
            }

            let context = ApplyContext { mode: ApplyMode::Replay, checkpoint: checkpoint.clone() };
            for mutation in root_mutations.drain(..) {
                self.objects
                    .apply_mutation(
                        super_block.root_store_object_id,
                        mutation,
                        &context,
                        AssocObj::None,
                    )
                    .context("Failed to replay root store mutations")?;
            }
        }

        // Now we can open the allocator.
        allocator.open().await.context("Failed to open allocator")?;

        // Now replay all other mutations.
        for JournaledTransaction { checkpoint, non_root_mutations, end_offset, .. } in transactions
        {
            self.objects
                .replay_mutations(
                    non_root_mutations,
                    &journal_offsets,
                    &ApplyContext { mode: ApplyMode::Replay, checkpoint },
                    end_offset,
                )
                .await
                .context("Failed to replay mutations")?;
        }

        allocator.on_replay_complete().await.context("Failed to complete replay for allocator")?;

        let discarded_to =
            if last_checkpoint.file_offset != reader.journal_file_checkpoint().file_offset {
                Some(reader.journal_file_checkpoint().file_offset)
            } else {
                None
            };

        // Configure the journal writer so that we can continue.
        {
            if last_checkpoint.file_offset < super_block.super_block_journal_file_offset {
                return Err(anyhow!(FxfsError::Inconsistent).context(format!(
                    "journal replay cut short; journal finishes at {}, but super-block was \
                     written at {}",
                    last_checkpoint.file_offset, super_block.super_block_journal_file_offset
                )));
            }
            let handle = ObjectStore::open_object(
                &root_parent,
                super_block.journal_object_id,
                journal_handle_options(),
                None,
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to open journal file (object id: {})",
                    super_block.journal_object_id
                )
            })?;
            let _ = self.handle.set(handle);
            let mut inner = self.inner.lock();
            reader.skip_to_end_of_block();
            let mut writer_checkpoint = reader.journal_file_checkpoint();

            // Make sure we don't accidentally use the reader from now onwards.
            std::mem::drop(reader);

            // Reset the stream to indicate that we've remounted the journal.
            writer_checkpoint.checksum ^= RESET_XOR;
            writer_checkpoint.version = LATEST_VERSION;
            inner.flushed_offset = writer_checkpoint.file_offset;

            // When we open the filesystem as writable, we flush the device.
            inner.device_flushed_offset = inner.flushed_offset;

            inner.writer.seek(writer_checkpoint);
            inner.output_reset_version = true;
            inner.valid_to = last_checkpoint.file_offset;
            if last_checkpoint.file_offset < inner.flushed_offset {
                inner.discard_offset = Some(last_checkpoint.file_offset);
            }
        }

        self.objects
            .on_replay_complete()
            .await
            .context("Failed to complete replay for object manager")?;

        info!(checkpoint = last_checkpoint.file_offset, discarded_to; "replay complete");
        Ok(())
    }

    async fn read_transactions(
        &self,
        reader: &mut JournalReader,
        end_offset: Option<u64>,
        object_id_filter: u64,
    ) -> Result<JournaledTransactions, Error> {
        let mut transactions = Vec::new();
        let (mut device_flushed_offset, root_parent_store_object_id, root_store_object_id) = {
            let super_block = &self.inner.lock().super_block_header;
            (
                super_block.super_block_journal_file_offset,
                super_block.root_parent_store_object_id,
                super_block.root_store_object_id,
            )
        };
        let mut current_transaction = None;
        let mut begin_flush_offsets = HashMap::default();
        let mut stores_deleted = HashSet::new();
        loop {
            // Cache the checkpoint before we deserialize a record.
            let checkpoint = reader.journal_file_checkpoint();
            if let Some(end_offset) = end_offset {
                if checkpoint.file_offset >= end_offset {
                    break;
                }
            }
            let result =
                reader.deserialize().await.context("Failed to deserialize journal record")?;
            match result {
                ReadResult::Reset(_) => {
                    if current_transaction.is_some() {
                        current_transaction = None;
                        transactions.pop();
                    }
                    let offset = reader.journal_file_checkpoint().file_offset;
                    if offset > device_flushed_offset {
                        device_flushed_offset = offset;
                    }
                }
                ReadResult::Some(record) => {
                    match record {
                        JournalRecord::EndBlock => {
                            reader.skip_to_end_of_block();
                        }
                        JournalRecord::Mutation { object_id, mutation } => {
                            let current_transaction = match current_transaction.as_mut() {
                                None => {
                                    transactions.push(JournaledTransaction::new(checkpoint));
                                    current_transaction = transactions.last_mut();
                                    current_transaction.as_mut().unwrap()
                                }
                                Some(transaction) => transaction,
                            };

                            if stores_deleted.contains(&object_id) {
                                bail!(anyhow!(FxfsError::Inconsistent)
                                    .context("Encountered mutations for deleted store"));
                            }

                            match &mutation {
                                Mutation::BeginFlush => {
                                    begin_flush_offsets.insert(
                                        object_id,
                                        current_transaction.checkpoint.file_offset,
                                    );
                                }
                                Mutation::EndFlush => {
                                    if let Some(offset) = begin_flush_offsets.remove(&object_id) {
                                        // The +1 is because we don't want to replay the transaction
                                        // containing the begin flush.
                                        if current_transaction
                                            .end_flush
                                            .replace((object_id, offset + 1))
                                            .is_some()
                                        {
                                            bail!(anyhow!(FxfsError::Inconsistent).context(
                                                "Multiple EndFlush/DeleteVolume mutations in a \
                                                 single transaction"
                                            ));
                                        }
                                    }
                                }
                                Mutation::DeleteVolume => {
                                    if current_transaction
                                        .end_flush
                                        .replace((object_id, STORE_DELETED))
                                        .is_some()
                                    {
                                        bail!(anyhow!(FxfsError::Inconsistent).context(
                                            "Multiple EndFlush/DeleteVolume mutations in a single \
                                             transaction"
                                        ));
                                    }
                                    stores_deleted.insert(object_id);
                                }
                                _ => {}
                            }

                            // If this mutation doesn't need to be applied, don't bother adding it
                            // to the transaction.
                            if (object_id_filter == INVALID_OBJECT_ID
                                || object_id_filter == object_id)
                                && self.should_apply(object_id, &current_transaction.checkpoint)
                            {
                                if object_id == root_parent_store_object_id {
                                    current_transaction.root_parent_mutations.push(mutation);
                                } else if object_id == root_store_object_id {
                                    current_transaction.root_mutations.push(mutation);
                                } else {
                                    current_transaction
                                        .non_root_mutations
                                        .push((object_id, mutation));
                                }
                            }
                        }
                        JournalRecord::DataChecksums(device_range, checksums, first_write) => {
                            let current_transaction = match current_transaction.as_mut() {
                                None => {
                                    transactions.push(JournaledTransaction::new(checkpoint));
                                    current_transaction = transactions.last_mut();
                                    current_transaction.as_mut().unwrap()
                                }
                                Some(transaction) => transaction,
                            };
                            current_transaction.checksums.push(JournaledChecksums {
                                device_range,
                                checksums,
                                first_write,
                            });
                        }
                        JournalRecord::Commit => {
                            if let Some(JournaledTransaction {
                                ref checkpoint,
                                ref root_parent_mutations,
                                ref mut end_offset,
                                ..
                            }) = current_transaction.take()
                            {
                                for mutation in root_parent_mutations {
                                    // Snoop the mutations for any that might apply to the journal
                                    // file so that we can pass them to the reader so that it can
                                    // read the journal file.
                                    if let Mutation::ObjectStore(ObjectStoreMutation {
                                        item:
                                            Item {
                                                key:
                                                    ObjectKey {
                                                        object_id,
                                                        data:
                                                            ObjectKeyData::Attribute(
                                                                DEFAULT_DATA_ATTRIBUTE_ID,
                                                                AttributeKey::Extent(ExtentKey {
                                                                    range,
                                                                }),
                                                            ),
                                                        ..
                                                    },
                                                value:
                                                    ObjectValue::Extent(ExtentValue::Some {
                                                        device_offset,
                                                        ..
                                                    }),
                                                ..
                                            },
                                        ..
                                    }) = mutation
                                    {
                                        // Add the journal extents we find on the way to our
                                        // reader.
                                        let handle = reader.handle();
                                        if *object_id != handle.object_id() {
                                            continue;
                                        }
                                        if let Some(end_offset) = handle.end_offset() {
                                            if range.start != end_offset {
                                                bail!(anyhow!(FxfsError::Inconsistent).context(
                                                    format!(
                                                        "Unexpected journal extent {:?} -> {}, \
                                                           expected start: {}",
                                                        range, device_offset, end_offset,
                                                    )
                                                ));
                                            }
                                        }
                                        handle.push_extent(
                                            checkpoint.file_offset,
                                            *device_offset
                                                ..*device_offset
                                                    + range.length().context("Invalid extent")?,
                                        );
                                    }
                                }
                                *end_offset = reader.journal_file_checkpoint().file_offset;
                            }
                        }
                        JournalRecord::Discard(offset) => {
                            if offset == 0 {
                                bail!(anyhow!(FxfsError::Inconsistent)
                                    .context("Invalid offset for Discard"));
                            }
                            if let Some(transaction) = current_transaction.as_ref() {
                                if transaction.checkpoint.file_offset < offset {
                                    // Odd, but OK.
                                    continue;
                                }
                            }
                            current_transaction = None;
                            while let Some(transaction) = transactions.last() {
                                if transaction.checkpoint.file_offset < offset {
                                    break;
                                }
                                transactions.pop();
                            }
                            reader.handle().discard_extents(offset);
                        }
                        JournalRecord::DidFlushDevice(offset) => {
                            if offset > device_flushed_offset {
                                device_flushed_offset = offset;
                            }
                        }
                    }
                }
                // This is expected when we reach the end of the journal stream.
                ReadResult::ChecksumMismatch => break,
            }
        }

        // Discard any uncommitted transaction.
        if current_transaction.is_some() {
            transactions.pop();
        }

        Ok(JournaledTransactions { transactions, device_flushed_offset })
    }

    /// Creates an empty filesystem with the minimum viable objects (including a root parent and
    /// root store but no further child stores).
    pub async fn init_empty(&self, filesystem: Arc<FxFilesystem>) -> Result<(), Error> {
        // The following constants are only used at format time. When mounting, the recorded values
        // in the superblock should be used.  The root parent store does not have a parent, but
        // needs an object ID to be registered with ObjectManager, so it cannot collide (i.e. have
        // the same object ID) with any objects in the root store that use the journal to track
        // mutations.
        const INIT_ROOT_PARENT_STORE_OBJECT_ID: u64 = 3;
        const INIT_ROOT_STORE_OBJECT_ID: u64 = 4;
        const INIT_ALLOCATOR_OBJECT_ID: u64 = 5;

        info!(device_size = filesystem.device().size(); "Formatting");

        let checkpoint = JournalCheckpoint {
            version: LATEST_VERSION,
            ..self.inner.lock().writer.journal_file_checkpoint()
        };

        let root_parent = ObjectStore::new_empty(
            None,
            INIT_ROOT_PARENT_STORE_OBJECT_ID,
            filesystem.clone(),
            Box::new(NullCache {}),
        );
        self.objects.set_root_parent_store(root_parent.clone());

        let allocator = Arc::new(Allocator::new(filesystem.clone(), INIT_ALLOCATOR_OBJECT_ID));
        self.objects.set_allocator(allocator.clone());
        self.objects.init_metadata_reservation()?;

        let journal_handle;
        let super_block_a_handle;
        let super_block_b_handle;
        let root_store;
        let mut transaction = filesystem
            .clone()
            .new_transaction(
                lock_keys![],
                Options { skip_journal_checks: true, ..Default::default() },
            )
            .await?;
        root_store = root_parent
            .new_child_store(
                &mut transaction,
                NewChildStoreOptions { object_id: INIT_ROOT_STORE_OBJECT_ID, ..Default::default() },
                Box::new(NullCache {}),
            )
            .await
            .context("new_child_store")?;
        self.objects.set_root_store(root_store.clone());

        allocator.create(&mut transaction).await?;

        // Create the super-block objects...
        super_block_a_handle = ObjectStore::create_object_with_id(
            &root_store,
            &mut transaction,
            SuperBlockInstance::A.object_id(),
            HandleOptions::default(),
            None,
        )
        .await
        .context("create super block")?;
        root_store.update_last_object_id(SuperBlockInstance::A.object_id());
        super_block_a_handle
            .extend(&mut transaction, SuperBlockInstance::A.first_extent())
            .await
            .context("extend super block")?;
        super_block_b_handle = ObjectStore::create_object_with_id(
            &root_store,
            &mut transaction,
            SuperBlockInstance::B.object_id(),
            HandleOptions::default(),
            None,
        )
        .await
        .context("create super block")?;
        root_store.update_last_object_id(SuperBlockInstance::B.object_id());
        super_block_b_handle
            .extend(&mut transaction, SuperBlockInstance::B.first_extent())
            .await
            .context("extend super block")?;

        // the journal object...
        journal_handle = ObjectStore::create_object(
            &root_parent,
            &mut transaction,
            journal_handle_options(),
            None,
        )
        .await
        .context("create journal")?;
        if self.inner.lock().image_builder_mode.is_none() {
            let mut file_range = 0..self.chunk_size();
            journal_handle
                .preallocate_range(&mut transaction, &mut file_range)
                .await
                .context("preallocate journal")?;
            if file_range.start < file_range.end {
                bail!("preallocate_range returned too little space");
            }
        }

        // Write the root store object info.
        root_store.create(&mut transaction).await?;

        // The root parent graveyard.
        root_parent
            .set_graveyard_directory_object_id(Graveyard::create(&mut transaction, &root_parent));

        transaction.commit().await?;

        self.inner.lock().super_block_header = SuperBlockHeader::new(
            root_parent.store_object_id(),
            root_parent.graveyard_directory_object_id(),
            root_store.store_object_id(),
            allocator.object_id(),
            journal_handle.object_id(),
            checkpoint,
            /* earliest_version: */ LATEST_VERSION,
        );

        // Initialize the journal writer.
        let _ = self.handle.set(journal_handle);
        Ok(())
    }

    /// Normally we allocate the journal when creating the filesystem.
    /// This is used image_builder_mode when journal allocation is done last.
    pub async fn allocate_journal(&self) -> Result<(), Error> {
        let handle = self.handle.get().unwrap();
        let filesystem = handle.store().filesystem();
        let mut transaction = filesystem
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(handle.store().store_object_id(), handle.object_id()),],
                Options { skip_journal_checks: true, ..Default::default() },
            )
            .await?;
        let mut file_range = 0..self.chunk_size();
        self.handle
            .get()
            .unwrap()
            .preallocate_range(&mut transaction, &mut file_range)
            .await
            .context("preallocate journal")?;
        if file_range.start < file_range.end {
            bail!("preallocate_range returned too little space");
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn init_superblocks(&self) -> Result<(), Error> {
        // Overwrite both superblocks.
        for _ in 0..2 {
            self.write_super_block().await?;
        }
        Ok(())
    }

    /// Takes a snapshot of all journaled transactions which affect |object_id| since its last
    /// flush.
    /// The caller is responsible for locking; it must ensure that the journal is not trimmed during
    /// this call.  For example, a Flush lock could be held on the object in question (assuming that
    /// object has data to flush and is registered with ObjectManager).
    pub async fn read_transactions_for_object(
        &self,
        object_id: u64,
    ) -> Result<Vec<JournaledTransaction>, Error> {
        let handle = self.handle.get().expect("No journal handle");
        // Reopen the handle since JournalReader needs an owned handle.
        let handle = ObjectStore::open_object(
            handle.owner(),
            handle.object_id(),
            journal_handle_options(),
            None,
        )
        .await?;

        let checkpoint = match self.objects.journal_checkpoint(object_id) {
            Some(checkpoint) => checkpoint,
            None => return Ok(vec![]),
        };
        let mut reader = JournalReader::new(handle, &checkpoint);
        // Record the current end offset and only read to there, so we don't accidentally read any
        // partially flushed blocks.
        let end_offset = self.inner.lock().valid_to;
        Ok(self.read_transactions(&mut reader, Some(end_offset), object_id).await?.transactions)
    }

    /// Commits a transaction.  This is not thread safe; the caller must take appropriate locks.
    pub async fn commit(&self, transaction: &mut Transaction<'_>) -> Result<u64, Error> {
        if transaction.is_empty() {
            return Ok(self.inner.lock().writer.journal_file_checkpoint().file_offset);
        }

        self.pre_commit(transaction).await?;
        Ok(self.write_and_apply_mutations(transaction))
    }

    // Before we commit, we might need to extend the journal or write pending records to the
    // journal.
    async fn pre_commit(&self, transaction: &Transaction<'_>) -> Result<(), Error> {
        let handle;

        let (size, zero_offset) = {
            let mut inner = self.inner.lock();

            // If this is the first write after a RESET, we need to output version first.
            if std::mem::take(&mut inner.output_reset_version) {
                LATEST_VERSION.serialize_into(&mut inner.writer)?;
            }

            if let Some(discard_offset) = inner.discard_offset {
                JournalRecord::Discard(discard_offset).serialize_into(&mut inner.writer)?;
                inner.discard_offset = None;
            }

            if inner.needs_did_flush_device {
                let offset = inner.device_flushed_offset;
                JournalRecord::DidFlushDevice(offset).serialize_into(&mut inner.writer)?;
                inner.needs_did_flush_device = false;
            }

            handle = match self.handle.get() {
                None => return Ok(()),
                Some(x) => x,
            };

            let file_offset = inner.writer.journal_file_checkpoint().file_offset;

            let size = handle.get_size();
            let size = if file_offset + self.chunk_size() > size { Some(size) } else { None };

            if size.is_none()
                && inner.zero_offset.is_none()
                && !self.objects.needs_borrow_for_journal(file_offset)
            {
                return Ok(());
            }

            (size, inner.zero_offset)
        };

        let mut transaction = handle
            .new_transaction_with_options(Options {
                skip_journal_checks: true,
                borrow_metadata_space: true,
                allocator_reservation: Some(self.objects.metadata_reservation()),
                txn_guard: Some(transaction.txn_guard()),
                ..Default::default()
            })
            .await?;
        if let Some(size) = size {
            handle
                .preallocate_range(&mut transaction, &mut (size..size + self.chunk_size()))
                .await?;
        }
        if let Some(zero_offset) = zero_offset {
            handle.zero(&mut transaction, 0..zero_offset).await?;
        }

        // We can't use regular transaction commit, because that can cause re-entrancy issues, so
        // instead we just apply the transaction directly here.
        self.write_and_apply_mutations(&mut transaction);

        let mut inner = self.inner.lock();

        // Make sure the transaction to extend the journal made it to the journal within the old
        // size, since otherwise, it won't be possible to replay.
        if let Some(size) = size {
            assert!(inner.writer.journal_file_checkpoint().file_offset < size);
        }

        if inner.zero_offset == zero_offset {
            inner.zero_offset = None;
        }

        Ok(())
    }

    // Determines whether a mutation at the given checkpoint should be applied.  During replay, not
    // all records should be applied because the object store or allocator might already contain the
    // mutation.  After replay, that obviously isn't the case and we want to apply all mutations.
    fn should_apply(&self, object_id: u64, journal_file_checkpoint: &JournalCheckpoint) -> bool {
        let super_block_header = &self.inner.lock().super_block_header;
        let offset = super_block_header
            .journal_file_offsets
            .get(&object_id)
            .cloned()
            .unwrap_or(super_block_header.super_block_journal_file_offset);
        journal_file_checkpoint.file_offset >= offset
    }

    /// Flushes previous writes to the device and then writes out a new super-block.
    /// Callers must ensure that we do not make concurrent calls.
    async fn write_super_block(&self) -> Result<(), Error> {
        let root_parent_store = self.objects.root_parent_store();

        // We need to flush previous writes to the device since the new super-block we are writing
        // relies on written data being observable, and we also need to lock the root parent store
        // so that no new entries are written to it whilst we are writing the super-block, and for
        // that we use the write lock.
        let old_layers;
        let old_super_block_offset;
        let mut new_super_block_header;
        let checkpoint;
        let borrowed;

        {
            let _sync_guard = debug_assert_not_too_long!(self.sync_mutex.lock());
            {
                let _write_guard = self.writer_mutex.lock();
                (checkpoint, borrowed) = self.pad_to_block()?;
                old_layers = super_block::compact_root_parent(&*root_parent_store)?;
            }
            self.flush_device(checkpoint.file_offset)
                .await
                .context("flush failed when writing superblock")?;
        }

        new_super_block_header = self.inner.lock().super_block_header.clone();

        old_super_block_offset = new_super_block_header.journal_checkpoint.file_offset;

        let (journal_file_offsets, min_checkpoint) = self.objects.journal_file_offsets();

        new_super_block_header.generation =
            new_super_block_header.generation.checked_add(1).ok_or(FxfsError::Inconsistent)?;
        new_super_block_header.super_block_journal_file_offset = checkpoint.file_offset;
        new_super_block_header.journal_checkpoint = min_checkpoint.unwrap_or(checkpoint);
        new_super_block_header.journal_checkpoint.version = LATEST_VERSION;
        new_super_block_header.journal_file_offsets = journal_file_offsets;
        new_super_block_header.borrowed_metadata_space = borrowed;

        self.super_block_manager
            .save(
                new_super_block_header.clone(),
                self.objects.root_parent_store().filesystem(),
                old_layers,
            )
            .await?;
        {
            let mut inner = self.inner.lock();
            inner.super_block_header = new_super_block_header;
            inner.zero_offset = Some(round_down(old_super_block_offset, BLOCK_SIZE));
        }

        Ok(())
    }

    /// Flushes any buffered journal data to the device.  Note that this does not flush the device
    /// unless the flush_device option is set, in which case data should have been persisted to
    /// lower layers.  If a precondition is supplied, it is evaluated and the sync will be skipped
    /// if it returns false.  This allows callers to check a condition whilst a lock is held.  If a
    /// sync is performed, this function returns the checkpoint that was flushed and the amount of
    /// borrowed metadata space at the point it was flushed.
    pub async fn sync(
        &self,
        options: SyncOptions<'_>,
    ) -> Result<Option<(JournalCheckpoint, u64)>, Error> {
        let _guard = debug_assert_not_too_long!(self.sync_mutex.lock());

        let (checkpoint, borrowed) = {
            if let Some(precondition) = options.precondition {
                if !precondition() {
                    return Ok(None);
                }
            }

            // This guard is required so that we don't insert an EndBlock record in the middle of a
            // transaction.
            let _guard = self.writer_mutex.lock();

            self.pad_to_block()?
        };

        if options.flush_device {
            self.flush_device(checkpoint.file_offset).await.context("sync: flush failed")?;
        }

        Ok(Some((checkpoint, borrowed)))
    }

    // Returns the checkpoint as it was prior to padding.  This is done because the super block
    // needs to record where the last transaction ends and it's the next transaction that pays the
    // price of the padding.
    fn pad_to_block(&self) -> Result<(JournalCheckpoint, u64), Error> {
        let mut inner = self.inner.lock();
        let checkpoint = inner.writer.journal_file_checkpoint();
        if checkpoint.file_offset % BLOCK_SIZE != 0 {
            JournalRecord::EndBlock.serialize_into(&mut inner.writer)?;
            inner.writer.pad_to_block()?;
            if let Some(waker) = inner.flush_waker.take() {
                waker.wake();
            }
        }
        Ok((checkpoint, self.objects.borrowed_metadata_space()))
    }

    async fn flush_device(&self, checkpoint_offset: u64) -> Result<(), Error> {
        debug_assert_not_too_long!(poll_fn(|ctx| {
            let mut inner = self.inner.lock();
            if inner.flushed_offset >= checkpoint_offset {
                Poll::Ready(Ok(()))
            } else if inner.terminate {
                let context = inner
                    .terminate_reason
                    .as_ref()
                    .map(|e| format!("Journal closed with error: {:?}", e))
                    .unwrap_or_else(|| "Journal closed".to_string());
                Poll::Ready(Err(anyhow!(FxfsError::JournalFlushError).context(context)))
            } else {
                inner.sync_waker = Some(ctx.waker().clone());
                Poll::Pending
            }
        }))?;

        let needs_flush = self.inner.lock().device_flushed_offset < checkpoint_offset;
        if needs_flush {
            let trace = self.trace.load(Ordering::Relaxed);
            if trace {
                info!("J: start flush device");
            }
            self.handle.get().unwrap().flush_device().await?;
            if trace {
                info!("J: end flush device");
            }

            // We need to write a DidFlushDevice record at some point, but if we are in the
            // process of shutting down the filesystem, we want to leave the journal clean to
            // avoid there being log messages complaining about unwritten journal data, so we
            // queue it up so that the next transaction will trigger this record to be written.
            // If we are shutting down, that will never happen but since the DidFlushDevice
            // message is purely advisory (it reduces the number of checksums we have to verify
            // during replay), it doesn't matter if it isn't written.
            {
                let mut inner = self.inner.lock();
                inner.device_flushed_offset = checkpoint_offset;
                inner.needs_did_flush_device = true;
            }

            // Tell the allocator that we flushed the device so that it can now start using
            // space that was deallocated.
            self.objects.allocator().did_flush_device(checkpoint_offset).await;
            if trace {
                info!("J: did flush device");
            }
        }

        Ok(())
    }

    /// Returns a copy of the super-block header.
    pub fn super_block_header(&self) -> SuperBlockHeader {
        self.inner.lock().super_block_header.clone()
    }

    /// Waits for there to be sufficient space in the journal.
    pub async fn check_journal_space(&self) -> Result<(), Error> {
        loop {
            debug_assert_not_too_long!({
                let inner = self.inner.lock();
                if inner.terminate {
                    // If the flush error is set, this will never make progress, since we can't
                    // extend the journal any more.
                    let context = inner
                        .terminate_reason
                        .as_ref()
                        .map(|e| format!("Journal closed with error: {:?}", e))
                        .unwrap_or_else(|| "Journal closed".to_string());
                    break Err(anyhow!(FxfsError::JournalFlushError).context(context));
                }
                if self.objects.last_end_offset()
                    - inner.super_block_header.journal_checkpoint.file_offset
                    < inner.reclaim_size
                {
                    break Ok(());
                }
                if inner.image_builder_mode.is_some() {
                    break Ok(());
                }
                if inner.disable_compactions {
                    break Err(
                        anyhow!(FxfsError::JournalFlushError).context("Compactions disabled")
                    );
                }
                self.reclaim_event.listen()
            });
        }
    }

    fn chunk_size(&self) -> u64 {
        CHUNK_SIZE
    }

    fn write_and_apply_mutations(&self, transaction: &mut Transaction<'_>) -> u64 {
        let checkpoint_before;
        let checkpoint_after;
        {
            let _guard = self.writer_mutex.lock();
            checkpoint_before = {
                let mut inner = self.inner.lock();
                if transaction.includes_write() {
                    inner.needs_barrier = true;
                }
                let checkpoint = inner.writer.journal_file_checkpoint();
                for TxnMutation { object_id, mutation, .. } in transaction.mutations() {
                    self.objects.write_mutation(
                        *object_id,
                        mutation,
                        Writer(*object_id, &mut inner.writer),
                    );
                }
                checkpoint
            };
            let maybe_mutation =
                self.objects.apply_transaction(transaction, &checkpoint_before).expect(
                    "apply_transaction should not fail in live mode; \
                     filesystem will be in an inconsistent state",
                );
            checkpoint_after = {
                let mut inner = self.inner.lock();
                if let Some(mutation) = maybe_mutation {
                    inner
                        .writer
                        .write_record(&JournalRecord::Mutation { object_id: 0, mutation })
                        .unwrap();
                }
                for (device_range, checksums, first_write) in
                    transaction.take_checksums().into_iter()
                {
                    inner
                        .writer
                        .write_record(&JournalRecord::DataChecksums(
                            device_range,
                            Checksums::fletcher(checksums),
                            first_write,
                        ))
                        .unwrap();
                }
                inner.writer.write_record(&JournalRecord::Commit).unwrap();

                inner.writer.journal_file_checkpoint()
            };
        }
        self.objects.did_commit_transaction(
            transaction,
            &checkpoint_before,
            checkpoint_after.file_offset,
        );

        if let Some(waker) = self.inner.lock().flush_waker.take() {
            waker.wake();
        }

        checkpoint_before.file_offset
    }

    /// This task will flush journal data to the device when there is data that needs flushing, and
    /// trigger compactions when short of journal space.  It will return after the terminate method
    /// has been called, or an error is encountered with either flushing or compaction.
    pub async fn flush_task(self: Arc<Self>) {
        let mut flush_fut = None;
        let mut compact_fut = None;
        let mut flush_error = false;
        poll_fn(|ctx| loop {
            {
                let mut inner = self.inner.lock();
                if flush_fut.is_none() && !flush_error && self.handle.get().is_some() {
                    let flushable = inner.writer.flushable_bytes();
                    if flushable > 0 {
                        flush_fut = Some(self.flush(flushable).boxed());
                    }
                }
                if inner.terminate && flush_fut.is_none() && compact_fut.is_none() {
                    return Poll::Ready(());
                }
                // The / 2 is here because after compacting, we cannot reclaim the space until the
                // _next_ time we flush the device since the super-block is not guaranteed to
                // persist until then.
                if compact_fut.is_none()
                    && !inner.terminate
                    && !inner.disable_compactions
                    && inner.image_builder_mode.is_none()
                    && self.objects.last_end_offset()
                        - inner.super_block_header.journal_checkpoint.file_offset
                        > inner.reclaim_size / 2
                {
                    compact_fut = Some(self.compact().boxed());
                    inner.compaction_running = true;
                }
                inner.flush_waker = Some(ctx.waker().clone());
            }
            let mut pending = true;
            if let Some(fut) = flush_fut.as_mut() {
                if let Poll::Ready(result) = fut.poll_unpin(ctx) {
                    if let Err(e) = result {
                        self.inner.lock().terminate(Some(e.context("Flush error")));
                        self.reclaim_event.notify(usize::MAX);
                        flush_error = true;
                    }
                    flush_fut = None;
                    pending = false;
                }
            }
            if let Some(fut) = compact_fut.as_mut() {
                if let Poll::Ready(result) = fut.poll_unpin(ctx) {
                    let mut inner = self.inner.lock();
                    if let Err(e) = result {
                        inner.terminate(Some(e.context("Compaction error")));
                    }
                    compact_fut = None;
                    inner.compaction_running = false;
                    self.reclaim_event.notify(usize::MAX);
                    pending = false;
                }
            }
            if pending {
                return Poll::Pending;
            }
        })
        .await;
    }

    async fn flush(&self, amount: usize) -> Result<(), Error> {
        let handle = self.handle.get().unwrap();
        let mut buf = handle.allocate_buffer(amount).await;
        let (offset, len, barrier_on_first_write) = {
            let mut inner = self.inner.lock();
            let offset = inner.writer.take_flushable(buf.as_mut());
            let barrier_on_first_write = inner.needs_barrier && inner.barriers_enabled;
            // Reset `needs_barrier` before instead of after the overwrite in case a txn commit
            // that contains data happens during the overwrite.
            inner.needs_barrier = false;
            (offset, buf.len() as u64, barrier_on_first_write)
        };
        self.handle
            .get()
            .unwrap()
            .overwrite(
                offset,
                buf.as_mut(),
                OverwriteOptions { barrier_on_first_write, ..Default::default() },
            )
            .await?;

        let mut inner = self.inner.lock();
        if let Some(waker) = inner.sync_waker.take() {
            waker.wake();
        }
        inner.flushed_offset = offset + len;
        inner.valid_to = inner.flushed_offset;
        Ok(())
    }

    /// This should generally NOT be called externally. It is public to allow use by FIDL service
    /// fxfs.Debug.
    #[trace]
    pub async fn compact(&self) -> Result<(), Error> {
        let trace = self.trace.load(Ordering::Relaxed);
        debug!("Compaction starting");
        if trace {
            info!("J: start compaction");
        }
        let earliest_version = self.objects.flush().await.context("Failed to flush objects")?;
        self.inner.lock().super_block_header.earliest_version = earliest_version;
        self.write_super_block().await.context("Failed to write superblock")?;
        if trace {
            info!("J: end compaction");
        }
        debug!("Compaction finished");
        Ok(())
    }

    pub async fn stop_compactions(&self) {
        loop {
            debug_assert_not_too_long!({
                let mut inner = self.inner.lock();
                inner.disable_compactions = true;
                if !inner.compaction_running {
                    return;
                }
                self.reclaim_event.listen()
            });
        }
    }

    /// Creates a lazy inspect node named `str` under `parent` which will yield statistics for the
    /// journal when queried.
    pub fn track_statistics(self: &Arc<Self>, parent: &fuchsia_inspect::Node, name: &str) {
        let this = Arc::downgrade(self);
        parent.record_lazy_child(name, move || {
            let this_clone = this.clone();
            async move {
                let inspector = fuchsia_inspect::Inspector::default();
                if let Some(this) = this_clone.upgrade() {
                    let (journal_min, journal_max, journal_reclaim_size) = {
                        // TODO(https://fxbug.dev/42069513): Push-back or rate-limit to prevent DoS.
                        let inner = this.inner.lock();
                        (
                            round_down(
                                inner.super_block_header.journal_checkpoint.file_offset,
                                BLOCK_SIZE,
                            ),
                            inner.flushed_offset,
                            inner.reclaim_size,
                        )
                    };
                    let root = inspector.root();
                    root.record_uint("journal_min_offset", journal_min);
                    root.record_uint("journal_max_offset", journal_max);
                    root.record_uint("journal_size", journal_max - journal_min);
                    root.record_uint("journal_reclaim_size", journal_reclaim_size);

                    // TODO(https://fxbug.dev/42068224): Post-compute rather than manually computing metrics.
                    if let Some(x) = round_div(
                        100 * (journal_max - journal_min),
                        this.objects.allocator().get_disk_bytes(),
                    ) {
                        root.record_uint("journal_size_to_disk_size_percent", x);
                    }
                }
                Ok(inspector)
            }
            .boxed()
        });
    }

    /// Terminate all journal activity.
    pub fn terminate(&self) {
        self.inner.lock().terminate(/*reason*/ None);
        self.reclaim_event.notify(usize::MAX);
    }
}

/// Wrapper to allow records to be written to the journal.
pub struct Writer<'a>(u64, &'a mut JournalWriter);

impl Writer<'_> {
    pub fn write(&mut self, mutation: Mutation) {
        self.1.write_record(&JournalRecord::Mutation { object_id: self.0, mutation }).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use crate::filesystem::{FxFilesystem, SyncOptions};
    use crate::fsck::fsck;
    use crate::object_handle::{ObjectHandle, ReadObjectHandle, WriteObjectHandle};
    use crate::object_store::directory::Directory;
    use crate::object_store::transaction::Options;
    use crate::object_store::volume::root_volume;
    use crate::object_store::{lock_keys, HandleOptions, LockKey, ObjectStore, NO_OWNER};
    use fuchsia_async as fasync;
    use fuchsia_async::MonotonicDuration;
    use storage_device::fake_device::FakeDevice;
    use storage_device::DeviceHolder;

    const TEST_DEVICE_BLOCK_SIZE: u32 = 512;

    #[fuchsia::test]
    async fn test_replay() {
        const TEST_DATA: &[u8] = b"hello";

        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));

        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");

        let object_id = {
            let root_store = fs.root_store();
            let root_directory =
                Directory::open(&root_store, root_store.root_directory_object_id())
                    .await
                    .expect("open failed");
            let mut transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(
                        root_store.store_object_id(),
                        root_store.root_directory_object_id(),
                    )],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");
            let handle = root_directory
                .create_child_file(&mut transaction, "test")
                .await
                .expect("create_child_file failed");

            transaction.commit().await.expect("commit failed");
            let mut buf = handle.allocate_buffer(TEST_DATA.len()).await;
            buf.as_mut_slice().copy_from_slice(TEST_DATA);
            handle.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
            // As this is the first sync, this will actually trigger a new super-block, but normally
            // this would not be the case.
            fs.sync(SyncOptions::default()).await.expect("sync failed");
            handle.object_id()
        };

        {
            fs.close().await.expect("Close failed");
            let device = fs.take_device().await;
            device.reopen(false);
            let fs = FxFilesystem::open(device).await.expect("open failed");
            let handle = ObjectStore::open_object(
                &fs.root_store(),
                object_id,
                HandleOptions::default(),
                None,
            )
            .await
            .expect("open_object failed");
            let mut buf = handle.allocate_buffer(TEST_DEVICE_BLOCK_SIZE as usize).await;
            assert_eq!(handle.read(0, buf.as_mut()).await.expect("read failed"), TEST_DATA.len());
            assert_eq!(&buf.as_slice()[..TEST_DATA.len()], TEST_DATA);
            fsck(fs.clone()).await.expect("fsck failed");
            fs.close().await.expect("Close failed");
        }
    }

    #[fuchsia::test]
    async fn test_reset() {
        const TEST_DATA: &[u8] = b"hello";

        let device = DeviceHolder::new(FakeDevice::new(32768, TEST_DEVICE_BLOCK_SIZE));

        let mut object_ids = Vec::new();

        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        {
            let root_store = fs.root_store();
            let root_directory =
                Directory::open(&root_store, root_store.root_directory_object_id())
                    .await
                    .expect("open failed");
            let mut transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(
                        root_store.store_object_id(),
                        root_store.root_directory_object_id(),
                    )],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");
            let handle = root_directory
                .create_child_file(&mut transaction, "test")
                .await
                .expect("create_child_file failed");
            transaction.commit().await.expect("commit failed");
            let mut buf = handle.allocate_buffer(TEST_DATA.len()).await;
            buf.as_mut_slice().copy_from_slice(TEST_DATA);
            handle.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
            fs.sync(SyncOptions::default()).await.expect("sync failed");
            object_ids.push(handle.object_id());

            // Create a lot of objects but don't sync at the end. This should leave the filesystem
            // with a half finished transaction that cannot be replayed.
            for i in 0..1000 {
                let mut transaction = fs
                    .clone()
                    .new_transaction(
                        lock_keys![LockKey::object(
                            root_store.store_object_id(),
                            root_store.root_directory_object_id(),
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
                let mut buf = handle.allocate_buffer(TEST_DATA.len()).await;
                buf.as_mut_slice().copy_from_slice(TEST_DATA);
                handle.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
                object_ids.push(handle.object_id());
            }
        }
        fs.close().await.expect("fs close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");
        fsck(fs.clone()).await.expect("fsck failed");
        {
            let root_store = fs.root_store();
            // Check the first two objects which should exist.
            for &object_id in &object_ids[0..1] {
                let handle = ObjectStore::open_object(
                    &root_store,
                    object_id,
                    HandleOptions::default(),
                    None,
                )
                .await
                .expect("open_object failed");
                let mut buf = handle.allocate_buffer(TEST_DEVICE_BLOCK_SIZE as usize).await;
                assert_eq!(
                    handle.read(0, buf.as_mut()).await.expect("read failed"),
                    TEST_DATA.len()
                );
                assert_eq!(&buf.as_slice()[..TEST_DATA.len()], TEST_DATA);
            }

            // Write one more object and sync.
            let root_directory =
                Directory::open(&root_store, root_store.root_directory_object_id())
                    .await
                    .expect("open failed");
            let mut transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(
                        root_store.store_object_id(),
                        root_store.root_directory_object_id(),
                    )],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");
            let handle = root_directory
                .create_child_file(&mut transaction, "test2")
                .await
                .expect("create_child_file failed");
            transaction.commit().await.expect("commit failed");
            let mut buf = handle.allocate_buffer(TEST_DATA.len()).await;
            buf.as_mut_slice().copy_from_slice(TEST_DATA);
            handle.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
            fs.sync(SyncOptions::default()).await.expect("sync failed");
            object_ids.push(handle.object_id());
        }

        fs.close().await.expect("close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");
        {
            fsck(fs.clone()).await.expect("fsck failed");

            // Check the first two and the last objects.
            for &object_id in object_ids[0..1].iter().chain(object_ids.last().cloned().iter()) {
                let handle = ObjectStore::open_object(
                    &fs.root_store(),
                    object_id,
                    HandleOptions::default(),
                    None,
                )
                .await
                .unwrap_or_else(|e| {
                    panic!("open_object failed (object_id: {}): {:?}", object_id, e)
                });
                let mut buf = handle.allocate_buffer(TEST_DEVICE_BLOCK_SIZE as usize).await;
                assert_eq!(
                    handle.read(0, buf.as_mut()).await.expect("read failed"),
                    TEST_DATA.len()
                );
                assert_eq!(&buf.as_slice()[..TEST_DATA.len()], TEST_DATA);
            }
        }
        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn test_discard() {
        let device = {
            let device = DeviceHolder::new(FakeDevice::new(8192, 4096));
            let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
            let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");

            let store =
                root_volume.new_volume("test", NO_OWNER, None).await.expect("new_volume failed");
            let root_directory = Directory::open(&store, store.root_directory_object_id())
                .await
                .expect("open failed");

            // Create enough data so that another journal extent is used.
            let mut i = 0;
            loop {
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
                    .create_child_file(&mut transaction, &format!("a {i}"))
                    .await
                    .expect("create_child_file failed");
                if transaction.commit().await.expect("commit failed") > super::CHUNK_SIZE {
                    break;
                }
                i += 1;
            }

            // Compact and then disable compactions.
            fs.journal().compact().await.expect("compact failed");
            fs.journal().stop_compactions().await;

            // Keep going until we need another journal extent.
            let mut i = 0;
            loop {
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
                    .create_child_file(&mut transaction, &format!("b {i}"))
                    .await
                    .expect("create_child_file failed");
                if transaction.commit().await.expect("commit failed") > 2 * super::CHUNK_SIZE {
                    break;
                }
                i += 1;
            }

            // Allow the journal to flush, but we don't want to sync.
            fasync::Timer::new(MonotonicDuration::from_millis(10)).await;
            // Because we're not gracefully closing the filesystem, a Discard record will be
            // emitted.
            fs.device().snapshot().expect("snapshot failed")
        };

        let fs = FxFilesystem::open(device).await.expect("open failed");

        {
            let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");

            let store = root_volume.volume("test", NO_OWNER, None).await.expect("volume failed");

            let root_directory = Directory::open(&store, store.root_directory_object_id())
                .await
                .expect("open failed");

            // Write one more transaction.
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
                .create_child_file(&mut transaction, &format!("d"))
                .await
                .expect("create_child_file failed");
            transaction.commit().await.expect("commit failed");
        }

        fs.close().await.expect("close failed");
        let device = fs.take_device().await;
        device.reopen(false);

        let fs = FxFilesystem::open(device).await.expect("open failed");
        fsck(fs.clone()).await.expect("fsck failed");
        fs.close().await.expect("close failed");
    }
}

#[cfg(fuzz)]
mod fuzz {
    use fuzz::fuzz;

    #[fuzz]
    fn fuzz_journal_bytes(input: Vec<u8>) {
        use crate::filesystem::FxFilesystem;
        use fuchsia_async as fasync;
        use std::io::Write;
        use storage_device::fake_device::FakeDevice;
        use storage_device::DeviceHolder;

        fasync::SendExecutor::new(4).run(async move {
            let device = DeviceHolder::new(FakeDevice::new(32768, 512));
            let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
            fs.journal().inner.lock().writer.write_all(&input).expect("write failed");
            fs.close().await.expect("close failed");
            let device = fs.take_device().await;
            device.reopen(false);
            if let Ok(fs) = FxFilesystem::open(device).await {
                // `close()` can fail if there were objects to be tombstoned. If the said object is
                // corrupted, there will be an error when we compact the journal.
                let _ = fs.close().await;
            }
        });
    }

    #[fuzz]
    fn fuzz_journal(input: Vec<super::JournalRecord>) {
        use crate::filesystem::FxFilesystem;
        use fuchsia_async as fasync;
        use storage_device::fake_device::FakeDevice;
        use storage_device::DeviceHolder;

        fasync::SendExecutor::new(4).run(async move {
            let device = DeviceHolder::new(FakeDevice::new(32768, 512));
            let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
            {
                let mut inner = fs.journal().inner.lock();
                for record in &input {
                    let _ = inner.writer.write_record(record);
                }
            }
            fs.close().await.expect("close failed");
            let device = fs.take_device().await;
            device.reopen(false);
            if let Ok(fs) = FxFilesystem::open(device).await {
                // `close()` can fail if there were objects to be tombstoned. If the said object is
                // corrupted, there will be an error when we compact the journal.
                let _ = fs.close().await;
            }
        });
    }
}
