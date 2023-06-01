// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! We currently store two of these super-blocks (A/B) located in two logical consecutive
//! 512kiB extents at the start of the device.
//!
//! Immediately following the serialized `SuperBlockHeader` structure below is a stream of
//! serialized operations that are replayed into the root parent `ObjectStore`. Note that the root
//! parent object store exists entirely in RAM until serialized back into the super-block.
//!
//! Super-blocks are updated alternately with a monotonically increasing generation number.
//! At mount time, the super-block used is the valid `SuperBlock` with the highest generation
//! number.
//!
//! Note the asymmetry here regarding load/save:
//!   * We load a superblock from a Device/SuperBlockInstance and return a
//!     (SuperBlockHeader, ObjectStore) pair. The ObjectStore is populated directly from device.
//!   * We save a superblock from a (SuperBlockHeader, Vec<ObjectItem>) pair to a WriteObjectHandle.
//!
//! This asymmetry is required for consistency.
//! The Vec<ObjectItem> is produced by scanning the root_parent_store. This is the responsibility
//! of the journal code, which must hold a lock to avoid concurrent updates. However, this lock
//! must NOT be held when saving the superblock as additional extents may need to be allocated as
//! part of the save process.
use {
    crate::{
        errors::FxfsError,
        filesystem::{ApplyContext, ApplyMode, Filesystem, JournalingObject},
        log::*,
        lsm_tree::{types::MutableLayer, LSMTree, LayerSet},
        metrics,
        object_store::{
            allocator::Reservation,
            journal::{
                bootstrap_handle::BootstrapObjectHandle,
                reader::{JournalReader, ReadResult},
                writer::JournalWriter,
                JournalCheckpoint, JournalHandle as _, BLOCK_SIZE,
            },
            object_record::{ObjectItem, ObjectItemV25, ObjectItemV29, ObjectItemV5},
            transaction::{AssocObj, Options},
            tree::MajorCompactable,
            HandleOptions, Mutation, ObjectKey, ObjectStore, ObjectValue, StoreObjectHandle,
        },
        range::RangeExt,
        serialized_types::{
            migrate_to_version, Migrate, Version, Versioned, VersionedLatest,
            EARLIEST_SUPPORTED_VERSION,
        },
    },
    anyhow::{bail, ensure, Context, Error},
    fprint::TypeFingerprint,
    fuchsia_inspect::{Property as _, UintProperty},
    serde::{Deserialize, Serialize},
    std::{
        collections::HashMap,
        convert::TryInto,
        fmt,
        io::{Read, Write},
        ops::{Bound, Range},
        sync::{Arc, Mutex},
        time::SystemTime,
    },
    storage_device::Device,
    uuid::Uuid,
};

// These only exist in the root store.
const SUPER_BLOCK_A_OBJECT_ID: u64 = 1;
const SUPER_BLOCK_B_OBJECT_ID: u64 = 2;

/// The superblock is extended in units of `SUPER_BLOCK_CHUNK_SIZE` as required.
const SUPER_BLOCK_CHUNK_SIZE: u64 = 65536;

/// The first 2 * 512 KiB on the disk are reserved for two A/B super-blocks.
const MIN_SUPER_BLOCK_SIZE: u64 = 524_288;

/// All superblocks start with the magic bytes "FxfsSupr".
const SUPER_BLOCK_MAGIC: &[u8; 8] = b"FxfsSupr";

/// An enum representing one of our super-block instances.
///
/// This provides hard-coded constants related to the location and properties of the super-blocks
/// that are required to bootstrap the filesystem.
#[derive(Copy, Clone, Debug)]
pub enum SuperBlockInstance {
    A,
    B,
}

impl SuperBlockInstance {
    /// Returns the next [SuperBlockInstance] for use in round-robining writes across super-blocks.
    pub fn next(&self) -> SuperBlockInstance {
        match self {
            SuperBlockInstance::A => SuperBlockInstance::B,
            SuperBlockInstance::B => SuperBlockInstance::A,
        }
    }

    pub fn object_id(&self) -> u64 {
        match self {
            SuperBlockInstance::A => SUPER_BLOCK_A_OBJECT_ID,
            SuperBlockInstance::B => SUPER_BLOCK_B_OBJECT_ID,
        }
    }

    /// Returns the byte range where the first extent of the [SuperBlockInstance] is stored.
    /// (Note that a [SuperBlockInstance] may still have multiple extents.)
    pub fn first_extent(&self) -> Range<u64> {
        match self {
            SuperBlockInstance::A => 0..MIN_SUPER_BLOCK_SIZE,
            SuperBlockInstance::B => MIN_SUPER_BLOCK_SIZE..2 * MIN_SUPER_BLOCK_SIZE,
        }
    }
}

#[derive(
    Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TypeFingerprint, Versioned,
)]
pub struct SuperBlockHeader {
    /// The globally unique identifier for the filesystem.
    guid: UuidWrapper,

    /// There are two super-blocks which are used in an A/B configuration. The super-block with the
    /// greatest generation number is what is used when mounting an Fxfs image; the other is
    /// discarded.
    pub generation: u64,

    /// The root parent store is an in-memory only store and serves as the backing store for the
    /// root store and the journal.  The records for this store are serialized into the super-block
    /// and mutations are also recorded in the journal.
    pub root_parent_store_object_id: u64,

    /// The root parent needs a graveyard and there's nowhere else to store it other than in the
    /// super-block.
    pub root_parent_graveyard_directory_object_id: u64,

    /// The root object store contains all other metadata objects (including the allocator, the
    /// journal and the super-blocks) and is the parent for all other object stores.
    pub root_store_object_id: u64,

    /// This is in the root object store.
    pub allocator_object_id: u64,

    /// This is in the root parent object store.
    pub journal_object_id: u64,

    /// Start checkpoint for the journal file.
    pub journal_checkpoint: JournalCheckpoint,

    /// Offset of the journal file when the super-block was written.  If no entry is present in
    /// journal_file_offsets for a particular object, then an object might have dependencies on the
    /// journal from super_block_journal_file_offset onwards, but not earlier.
    pub super_block_journal_file_offset: u64,

    /// object id -> journal file offset. Indicates where each object has been flushed to.
    pub journal_file_offsets: HashMap<u64, u64>,

    /// Records the amount of borrowed metadata space as applicable at
    /// `super_block_journal_file_offset`.
    pub borrowed_metadata_space: u64,

    /// The earliest version of Fxfs used to create any still-existing struct in the filesystem.
    ///
    /// Note: structs in the filesystem may had been made with various different versions of Fxfs.
    pub earliest_version: Version,
}

#[derive(Clone, Default, Eq, PartialEq)]
struct UuidWrapper(Uuid);

impl fmt::Debug for UuidWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The UUID uniquely identifies the filesystem, so we should redact it so that we don't leak
        // it in logs.
        f.write_str("<redacted>")
    }
}

impl TypeFingerprint for UuidWrapper {
    fn fingerprint() -> String {
        "<[u8;16]>".to_owned()
    }
}

// Uuid serializes like a slice, but SuperBlockHeader used to contain [u8; 16] and we want to remain
// compatible.
impl Serialize for UuidWrapper {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.as_bytes().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for UuidWrapper {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        <[u8; 16]>::deserialize(deserializer).map(|bytes| UuidWrapper(Uuid::from_bytes(bytes)))
    }
}

#[derive(Debug, Serialize, Deserialize, TypeFingerprint, Versioned)]
pub enum SuperBlockRecord {
    // When reading the super-block we know the initial extent, but not subsequent extents, so these
    // records need to exist to allow us to completely read the super-block.
    Extent(Range<u64>),

    // Following the super-block header are ObjectItem records that are to be replayed into the root
    // parent object store.
    ObjectItem(ObjectItem),

    // Marks the end of the full super-block.
    End,
}

#[derive(Debug, Deserialize, Migrate, Serialize, Versioned, TypeFingerprint)]
pub enum SuperBlockRecordV29 {
    Extent(Range<u64>),
    ObjectItem(ObjectItemV29),
    End,
}

#[derive(Debug, Deserialize, Migrate, Serialize, Versioned, TypeFingerprint)]
#[migrate_to_version(SuperBlockRecordV29)]
pub enum SuperBlockRecordV25 {
    Extent(Range<u64>),
    ObjectItem(ObjectItemV25),
    End,
}

#[derive(Debug, Deserialize, Migrate, Serialize, Versioned, TypeFingerprint)]
#[migrate_to_version(SuperBlockRecordV25)]
pub enum SuperBlockRecordV5 {
    Extent(Range<u64>),
    ObjectItem(ObjectItemV5),
    End,
}

struct SuperBlockMetrics {
    /// Time we wrote the most recent superblock in milliseconds since [`std::time::UNIX_EPOCH`].
    /// Uses [`std::time::SystemTime`] as the clock source.
    last_super_block_update_time_ms: UintProperty,

    /// Offset of the most recent superblock we wrote in the journal.
    last_super_block_offset: UintProperty,
}

impl Default for SuperBlockMetrics {
    fn default() -> Self {
        SuperBlockMetrics {
            last_super_block_update_time_ms: metrics::detail()
                .create_uint("last_super_block_update_time_ms", 0),
            last_super_block_offset: metrics::detail().create_uint("last_super_block_offset", 0),
        }
    }
}

/// Reads an individual (A/B) super-block instance and root_parent_store from device.
/// Users should use SuperBlockManager::load() instead.
async fn read(
    device: Arc<dyn Device>,
    block_size: u64,
    instance: SuperBlockInstance,
) -> Result<(SuperBlockHeader, SuperBlockInstance, ObjectStore), Error> {
    let (super_block_header, mut reader) = SuperBlockHeader::read_header(device.clone(), instance)
        .await
        .context("failed to read superblock")?;
    let root_parent = ObjectStore::new_root_parent(
        device,
        block_size,
        super_block_header.root_parent_store_object_id,
    );
    root_parent.set_graveyard_directory_object_id(
        super_block_header.root_parent_graveyard_directory_object_id,
    );

    loop {
        // TODO: Flatten a layer and move reader here?
        let (mutation, sequence) = match reader.next_item().await? {
            // RecordReader should filter out extent records.
            SuperBlockRecord::Extent(_) => bail!("Unexpected extent record"),
            SuperBlockRecord::ObjectItem(item) => {
                (Mutation::insert_object(item.key, item.value), item.sequence)
            }
            SuperBlockRecord::End => break,
        };
        root_parent
            .apply_mutation(
                mutation,
                &ApplyContext {
                    mode: ApplyMode::Replay,
                    checkpoint: JournalCheckpoint { file_offset: sequence, ..Default::default() },
                },
                AssocObj::None,
            )
            .await?;
    }
    Ok((super_block_header, instance, root_parent))
}

/// Write a super-block to the given file handle.
/// Requires that the filesystem is fully loaded and writable as this may require allocation.
async fn write<S: AsRef<ObjectStore> + Send + Sync + 'static>(
    super_block_header: &SuperBlockHeader,
    items: LayerSet<ObjectKey, ObjectValue>,
    handle: StoreObjectHandle<S>,
) -> Result<(), Error> {
    let object_manager = handle.store().filesystem().object_manager().clone();
    // TODO(fxbug.dev/95404): Don't use the same code here for Journal and SuperBlock. They
    // aren't the same things and it is already getting convoluted. e.g of diff stream content:
    //   Superblock:  (Magic, Ver, Header(Ver), SuperBlockRecord(Ver)*, ...)
    //   Journal:     (Ver, JournalRecord(Ver)*, RESET, Ver2, JournalRecord(Ver2)*, ...)
    // We should abstract away the checksum code and implement these separately.
    let mut writer = SuperBlockWriter::new(handle, object_manager.metadata_reservation());

    writer.writer.write_all(SUPER_BLOCK_MAGIC)?;
    super_block_header.serialize_with_version(&mut writer.writer)?;

    let mut merger = items.merger();
    let mut iter = LSMTree::major_iter(Box::new(merger.seek(Bound::Unbounded).await?)).await?;
    while let Some(item) = iter.get() {
        writer.maybe_extend().await?;
        SuperBlockRecord::ObjectItem(item.cloned()).serialize_into(&mut writer.writer)?;
        iter.advance().await?;
    }

    SuperBlockRecord::End.serialize_into(&mut writer.writer)?;
    writer.writer.pad_to_block()?;
    writer.flush_buffer().await?;
    let len =
        std::cmp::max(MIN_SUPER_BLOCK_SIZE, writer.writer.journal_file_checkpoint().file_offset);
    writer
        .handle
        .truncate_with_options(
            Options {
                skip_journal_checks: true,
                borrow_metadata_space: true,
                ..Default::default()
            },
            len,
        )
        .await?;
    Ok(())
}

// Compacts and returns the *old* snapshot of the root_parent store.
// Must be performed whilst holding a writer lock.
pub async fn compact_root_parent(
    root_parent_store: &ObjectStore,
) -> Result<LayerSet<ObjectKey, ObjectValue>, Error> {
    let tree = root_parent_store.tree();
    let layer_set = tree.layer_set();
    {
        let mut merger = layer_set.merger();
        let mut iter = LSMTree::major_iter(Box::new(merger.seek(Bound::Unbounded).await?)).await?;
        let new_layer = LSMTree::new_mutable_layer();
        while let Some(item_ref) = iter.get() {
            new_layer.insert(item_ref.cloned()).await?;
            iter.advance().await?;
        }
        tree.set_mutable_layer(new_layer);
    }
    Ok(layer_set)
}

/// This encapsulates the A/B alternating super-block logic.
/// All super-block load/save operations should be via the methods on this type.
pub struct SuperBlockManager {
    next_instance: Arc<Mutex<SuperBlockInstance>>,
    metrics: SuperBlockMetrics,
}

impl SuperBlockManager {
    pub fn new() -> Self {
        Self {
            next_instance: Arc::new(Mutex::new(SuperBlockInstance::A)),
            metrics: Default::default(),
        }
    }

    /// Loads both A/B super-blocks and root_parent ObjectStores and and returns the newest valid
    /// pair. Also ensures the next superblock updated via |save| will be the other instance.
    pub async fn load(
        &self,
        device: Arc<dyn Device>,
        block_size: u64,
    ) -> Result<(SuperBlockHeader, ObjectStore), Error> {
        let (super_block, current_super_block, root_parent) = match futures::join!(
            read(device.clone(), block_size, SuperBlockInstance::A),
            read(device.clone(), block_size, SuperBlockInstance::B)
        ) {
            (Err(e1), Err(e2)) => {
                bail!("Failed to load both superblocks due to {:?}\nand\n{:?}", e1, e2)
            }
            (Ok(result), Err(_)) => result,
            (Err(_), Ok(result)) => result,
            (Ok(result1), Ok(result2)) => {
                // Break the tie by taking the super-block with the greatest generation.
                if result2.0.generation > result1.0.generation {
                    result2
                } else {
                    result1
                }
            }
        };
        info!(?super_block, ?current_super_block, "loaded super-block");
        *self.next_instance.lock().unwrap() = current_super_block.next();
        Ok((super_block, root_parent))
    }

    /// Writes the provided superblock and root_parent ObjectStore to the device.
    /// Requires that the filesystem is fully loaded and writable as this may require allocation.
    pub async fn save(
        &self,
        super_block_header: SuperBlockHeader,
        filesystem: Arc<dyn Filesystem>,
        root_parent: LayerSet<ObjectKey, ObjectValue>,
    ) -> Result<(), Error> {
        let root_store = filesystem.root_store();
        let object_id = {
            let mut next_instance = self.next_instance.lock().unwrap();
            let object_id = next_instance.object_id();
            *next_instance = next_instance.next();
            object_id
        };
        let handle = ObjectStore::open_object(
            &root_store,
            object_id,
            HandleOptions { skip_journal_checks: true, ..Default::default() },
            None,
        )
        .await?;
        write(&super_block_header, root_parent, handle).await?;
        self.metrics
            .last_super_block_offset
            .set(super_block_header.super_block_journal_file_offset);
        self.metrics.last_super_block_update_time_ms.set(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis()
                .try_into()
                .unwrap_or(0u64),
        );
        Ok(())
    }
}

impl SuperBlockHeader {
    /// Creates a new instance with random GUID.
    pub fn new(
        root_parent_store_object_id: u64,
        root_parent_graveyard_directory_object_id: u64,
        root_store_object_id: u64,
        allocator_object_id: u64,
        journal_object_id: u64,
        journal_checkpoint: JournalCheckpoint,
        earliest_version: Version,
    ) -> Self {
        SuperBlockHeader {
            guid: UuidWrapper(Uuid::new_v4()),
            generation: 1u64,
            root_parent_store_object_id,
            root_parent_graveyard_directory_object_id,
            root_store_object_id,
            allocator_object_id,
            journal_object_id,
            journal_checkpoint,
            earliest_version,
            ..Default::default()
        }
    }

    /// Shreds the super-block, rendering it unreadable.  This is used in mkfs to ensure that we
    /// wipe out any stale super-blocks when rewriting Fxfs.
    /// This isn't a secure shred in any way, it just ensures the super-block is not recognized as a
    /// super-block.
    pub async fn shred<S: AsRef<ObjectStore> + Send + Sync + 'static>(
        handle: StoreObjectHandle<S>,
    ) -> Result<(), Error> {
        let mut buf =
            handle.store().device().allocate_buffer(handle.store().device().block_size() as usize);
        buf.as_mut_slice().fill(0u8);
        handle.overwrite(0, buf.as_mut(), false).await
    }

    /// Read the super-block header, and return it and a reader that produces the records that are
    /// to be replayed in to the root parent object store.
    async fn read_header(
        device: Arc<dyn Device>,
        target_super_block: SuperBlockInstance,
    ) -> Result<(SuperBlockHeader, RecordReader), Error> {
        let mut handle = BootstrapObjectHandle::new(target_super_block.object_id(), device);
        handle.push_extent(target_super_block.first_extent());
        let mut reader = JournalReader::new(handle, &JournalCheckpoint::default());

        reader.fill_buf().await?;

        if reader.buffer().len() == 0 {
            // Try with the old block size.
            reader.set_version(EARLIEST_SUPPORTED_VERSION);
            reader.fill_buf().await?;
        }

        let mut super_block_header;
        let super_block_version;
        reader.consume({
            let mut cursor = std::io::Cursor::new(reader.buffer());
            // Validate magic bytes.
            let mut magic_bytes: [u8; 8] = [0; 8];
            cursor.read_exact(&mut magic_bytes)?;
            if magic_bytes.as_slice() != SUPER_BLOCK_MAGIC.as_slice() {
                bail!(format!("Invalid magic: {:?}", magic_bytes));
            }
            (super_block_header, super_block_version) =
                SuperBlockHeader::deserialize_with_version(&mut cursor)?;

            if super_block_version < EARLIEST_SUPPORTED_VERSION {
                bail!(format!("Unsupported SuperBlock version: {:?}", super_block_version));
            }

            // NOTE: It is possible that data was written to the journal with an old version
            // but no compaction ever happened, so the journal version could potentially be older
            // than the layer file versions.
            if super_block_header.journal_checkpoint.version < EARLIEST_SUPPORTED_VERSION {
                bail!(format!(
                    "Unsupported JournalCheckpoint version: {:?}",
                    super_block_header.journal_checkpoint.version
                ));
            }

            if super_block_header.earliest_version < EARLIEST_SUPPORTED_VERSION {
                bail!(format!(
                    "Filesystem contains struct with unsupported version: {:?}",
                    super_block_header.earliest_version
                ));
            }

            cursor.position() as usize
        });
        // If guid is zeroed (e.g. in a newly imaged system), assign one randomly.
        if super_block_header.guid.0.is_nil() {
            super_block_header.guid = UuidWrapper(Uuid::new_v4());
        }
        reader.set_version(super_block_version);
        Ok((super_block_header, RecordReader { reader }))
    }
}

struct SuperBlockWriter<'a, S: AsRef<ObjectStore> + Send + Sync + 'static> {
    handle: StoreObjectHandle<S>,
    writer: JournalWriter,
    next_extent_offset: u64,
    reservation: &'a Reservation,
}

impl<'a, S: AsRef<ObjectStore> + Send + Sync + 'static> SuperBlockWriter<'a, S> {
    fn new(handle: StoreObjectHandle<S>, reservation: &'a Reservation) -> Self {
        Self {
            handle,
            writer: JournalWriter::new(BLOCK_SIZE as usize, 0),
            next_extent_offset: MIN_SUPER_BLOCK_SIZE,
            reservation,
        }
    }

    async fn maybe_extend(&mut self) -> Result<(), Error> {
        if self.writer.journal_file_checkpoint().file_offset
            < self.next_extent_offset - SUPER_BLOCK_CHUNK_SIZE
        {
            return Ok(());
        }
        let mut transaction = self
            .handle
            .new_transaction_with_options(Options {
                skip_journal_checks: true,
                borrow_metadata_space: true,
                allocator_reservation: Some(self.reservation),
                ..Default::default()
            })
            .await?;
        let allocated = self
            .handle
            .preallocate_range(
                &mut transaction,
                self.next_extent_offset..self.next_extent_offset + SUPER_BLOCK_CHUNK_SIZE,
            )
            .await?;
        transaction.commit().await?;
        for device_range in allocated {
            self.next_extent_offset += device_range.end - device_range.start;
            SuperBlockRecord::Extent(device_range).serialize_into(&mut self.writer)?;
        }
        Ok(())
    }

    async fn flush_buffer(&mut self) -> Result<(), Error> {
        let (offset, mut buf) = self.writer.take_buffer(&self.handle).unwrap();
        self.handle.overwrite(offset, buf.as_mut(), false).await
    }
}

pub struct RecordReader {
    reader: JournalReader,
}

impl RecordReader {
    pub async fn next_item(&mut self) -> Result<SuperBlockRecord, Error> {
        loop {
            match self.reader.deserialize().await? {
                ReadResult::Reset(_) => bail!("Unexpected reset"),
                ReadResult::ChecksumMismatch => bail!("Checksum mismatch"),
                ReadResult::Some(SuperBlockRecord::Extent(extent)) => {
                    ensure!(extent.is_valid(), FxfsError::Inconsistent);
                    self.reader.handle().push_extent(extent)
                }
                ReadResult::Some(x) => return Ok(x),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{
            compact_root_parent, write, SuperBlockHeader, SuperBlockInstance, UuidWrapper,
            MIN_SUPER_BLOCK_SIZE,
        },
        crate::{
            filesystem::{Filesystem, FxFilesystem, OpenFxFilesystem},
            object_store::{
                allocator::Allocator,
                journal::JournalCheckpoint,
                transaction::{Options, TransactionHandler},
                HandleOptions, ObjectHandle, ObjectKey, ObjectStore, StoreObjectHandle,
            },
            serialized_types::LATEST_VERSION,
        },
        storage_device::{fake_device::FakeDevice, DeviceHolder},
        uuid::Uuid,
    };

    // We require 512kiB each for A/B super-blocks, 256kiB for the journal (128kiB before flush)
    // and compactions require double the layer size to complete.
    const TEST_DEVICE_BLOCK_SIZE: u32 = 512;
    const TEST_DEVICE_BLOCK_COUNT: u64 = 16384;

    async fn filesystem_and_super_block_handles(
    ) -> (OpenFxFilesystem, StoreObjectHandle<ObjectStore>, StoreObjectHandle<ObjectStore>) {
        let device =
            DeviceHolder::new(FakeDevice::new(TEST_DEVICE_BLOCK_COUNT, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        fs.close().await.expect("Close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");

        let handle_a = ObjectStore::open_object(
            &fs.object_manager().root_store(),
            SuperBlockInstance::A.object_id(),
            HandleOptions::default(),
            None,
        )
        .await
        .expect("open superblock failed");

        let handle_b = ObjectStore::open_object(
            &fs.object_manager().root_store(),
            SuperBlockInstance::B.object_id(),
            HandleOptions::default(),
            None,
        )
        .await
        .expect("open superblock failed");
        (fs, handle_a, handle_b)
    }

    #[fuchsia::test]
    async fn test_read_written_super_block() {
        let (fs, _handle_a, _handle_b) = filesystem_and_super_block_handles().await;
        const JOURNAL_OBJECT_ID: u64 = 5;

        // Confirm that the (first) super-block is minimum sized to start with.
        assert_eq!(
            ObjectStore::open_object(
                &fs.root_store(),
                SuperBlockInstance::A.object_id(),
                HandleOptions::default(),
                None,
            )
            .await
            .expect("open_object failed")
            .get_size(),
            MIN_SUPER_BLOCK_SIZE
        );

        // Create a large number of objects in the root parent store so that we test growing
        // of the super-block file, requiring us to add extents.
        let mut created_object_ids = vec![];
        const NUM_ENTRIES: u64 = MIN_SUPER_BLOCK_SIZE / 16;
        for _ in 0..NUM_ENTRIES {
            let mut transaction = fs
                .clone()
                .new_transaction(&[], Options::default())
                .await
                .expect("new_transaction failed");
            created_object_ids.push(
                ObjectStore::create_object(
                    &fs.object_manager().root_parent_store(),
                    &mut transaction,
                    HandleOptions::default(),
                    None,
                    None,
                )
                .await
                .expect("create_object failed")
                .object_id(),
            );
            transaction.commit().await.expect("commit failed");
        }

        // Note here that StoreObjectHandle caches the size given to it at construction.
        // If we want to know the true size after a super-block has been written, we need
        // a new handle.
        assert!(
            ObjectStore::open_object(
                &fs.root_store(),
                SuperBlockInstance::A.object_id(),
                HandleOptions::default(),
                None,
            )
            .await
            .expect("open_object failed")
            .get_size()
                > MIN_SUPER_BLOCK_SIZE
        );

        let written_super_block_a =
            SuperBlockHeader::read_header(fs.device(), SuperBlockInstance::A)
                .await
                .expect("read failed");
        let written_super_block_b =
            SuperBlockHeader::read_header(fs.device(), SuperBlockInstance::B)
                .await
                .expect("read failed");

        // Check that a non-zero GUID has been assigned.
        assert!(!written_super_block_a.0.guid.0.is_nil());

        // Depending on specific offsets is fragile so we just validate the fields we believe
        // to be stable.
        assert_eq!(written_super_block_a.0.guid, written_super_block_b.0.guid);
        assert_eq!(written_super_block_a.0.guid, written_super_block_b.0.guid);
        assert!(written_super_block_a.0.generation != written_super_block_b.0.generation);
        assert_eq!(
            written_super_block_a.0.root_parent_store_object_id,
            written_super_block_b.0.root_parent_store_object_id
        );
        assert_eq!(
            written_super_block_a.0.root_parent_graveyard_directory_object_id,
            written_super_block_b.0.root_parent_graveyard_directory_object_id
        );
        assert_eq!(written_super_block_a.0.root_store_object_id, fs.root_store().store_object_id());
        assert_eq!(
            written_super_block_a.0.root_store_object_id,
            written_super_block_b.0.root_store_object_id
        );
        assert_eq!(written_super_block_a.0.allocator_object_id, fs.allocator().object_id());
        assert_eq!(
            written_super_block_a.0.allocator_object_id,
            written_super_block_b.0.allocator_object_id
        );
        assert_eq!(written_super_block_a.0.journal_object_id, JOURNAL_OBJECT_ID);
        assert_eq!(
            written_super_block_a.0.journal_object_id,
            written_super_block_b.0.journal_object_id
        );
        assert!(
            written_super_block_a.0.journal_checkpoint.file_offset
                != written_super_block_b.0.journal_checkpoint.file_offset
        );
        assert!(
            written_super_block_a.0.super_block_journal_file_offset
                != written_super_block_b.0.super_block_journal_file_offset
        );
        // Nb: We skip journal_file_offsets and borrowed metadata space checks.
        assert_eq!(written_super_block_a.0.earliest_version, LATEST_VERSION);
        assert_eq!(
            written_super_block_a.0.earliest_version,
            written_super_block_b.0.earliest_version
        );

        // Nb: Skip comparison of root_parent store contents because we have no way of anticipating
        // the extent offsets and it is reasonable that a/b differ.

        // Delete all the objects we just made.
        for object_id in created_object_ids {
            let mut transaction = fs
                .clone()
                .new_transaction(&[], Options::default())
                .await
                .expect("new_transaction failed");
            fs.object_manager()
                .root_parent_store()
                .adjust_refs(&mut transaction, object_id, -1)
                .await
                .expect("adjust_refs failed");
            transaction.commit().await.expect("commit failed");
            fs.object_manager()
                .root_parent_store()
                .tombstone(object_id, Options::default())
                .await
                .expect("tombstone failed");
        }
        // Write some stuff to the root store to ensure we rotate the journal and produce new
        // super blocks.
        for _ in 0..NUM_ENTRIES {
            let mut transaction = fs
                .clone()
                .new_transaction(&[], Options::default())
                .await
                .expect("new_transaction failed");
            ObjectStore::create_object(
                &fs.object_manager().root_store(),
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed");
            transaction.commit().await.expect("commit failed");
        }

        // TODO(ripper): Bug! We somehow get existing objects remaining. My suspicion is that we've
        // unlocked super-block writing so this code may induce out-of-order writes.

        assert_eq!(
            ObjectStore::open_object(
                &fs.root_store(),
                SuperBlockInstance::A.object_id(),
                HandleOptions::default(),
                None,
            )
            .await
            .expect("open_object failed")
            .get_size(),
            MIN_SUPER_BLOCK_SIZE
        );
    }

    #[fuchsia::test]
    async fn test_guid_assign_on_read() {
        let (fs, handle_a, _handle_b) = filesystem_and_super_block_handles().await;
        const JOURNAL_OBJECT_ID: u64 = 5;
        let mut super_block_header_a = SuperBlockHeader::new(
            fs.object_manager().root_parent_store().store_object_id(),
            /* root_parent_graveyard_directory_object_id: */ 1000,
            fs.root_store().store_object_id(),
            fs.allocator().object_id(),
            JOURNAL_OBJECT_ID,
            JournalCheckpoint { file_offset: 1234, checksum: 5678, version: LATEST_VERSION },
            /* earliest_version: */ LATEST_VERSION,
        );
        // Ensure the superblock has no set GUID.
        super_block_header_a.guid = UuidWrapper(Uuid::nil());
        write(
            &super_block_header_a,
            compact_root_parent(fs.object_manager().root_parent_store().as_ref())
                .await
                .expect("scan failed"),
            handle_a,
        )
        .await
        .expect("write failed");
        let super_block_header = SuperBlockHeader::read_header(fs.device(), SuperBlockInstance::A)
            .await
            .expect("read failed");
        // Ensure a GUID has been assigned.
        assert!(!super_block_header.0.guid.0.is_nil());
    }

    #[fuchsia::test]
    async fn test_init_wipes_superblocks() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));

        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let root_store = fs.root_store();
        // Generate enough work to induce a journal flush and thus a new superblock being written.
        for _ in 0..8000 {
            let mut transaction = fs
                .clone()
                .new_transaction(&[], Options::default())
                .await
                .expect("new_transaction failed");
            ObjectStore::create_object(
                &root_store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed");
            transaction.commit().await.expect("commit failed");
        }
        fs.close().await.expect("Close failed");
        let device = fs.take_device().await;
        device.reopen(false);

        SuperBlockHeader::read_header(device.clone(), SuperBlockInstance::A)
            .await
            .expect("read failed");
        SuperBlockHeader::read_header(device.clone(), SuperBlockInstance::B)
            .await
            .expect("read failed");

        // Re-initialize the filesystem.  The A block should be reset and the B block should be
        // wiped.
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        fs.close().await.expect("Close failed");
        let device = fs.take_device().await;
        device.reopen(false);

        SuperBlockHeader::read_header(device.clone(), SuperBlockInstance::A)
            .await
            .expect("read failed");
        SuperBlockHeader::read_header(device.clone(), SuperBlockInstance::B)
            .await
            .map(|_| ())
            .expect_err("Super-block B was readable after a re-format");
    }

    #[fuchsia::test]
    async fn test_alternating_super_blocks() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));

        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        fs.close().await.expect("Close failed");
        let device = fs.take_device().await;
        device.reopen(false);

        let (super_block_header_a, _) =
            SuperBlockHeader::read_header(device.clone(), SuperBlockInstance::A)
                .await
                .expect("read failed");

        // The second super-block won't be valid at this time so there's no point reading it.

        let fs = FxFilesystem::open(device).await.expect("open failed");
        let root_store = fs.root_store();
        // Generate enough work to induce a journal flush.
        for _ in 0..8000 {
            let mut transaction = fs
                .clone()
                .new_transaction(&[], Options::default())
                .await
                .expect("new_transaction failed");
            ObjectStore::create_object(
                &root_store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed");
            transaction.commit().await.expect("commit failed");
        }
        fs.close().await.expect("Close failed");
        let device = fs.take_device().await;
        device.reopen(false);

        let (super_block_header_a_after, _) =
            SuperBlockHeader::read_header(device.clone(), SuperBlockInstance::A)
                .await
                .expect("read failed");
        let (super_block_header_b_after, _) =
            SuperBlockHeader::read_header(device.clone(), SuperBlockInstance::B)
                .await
                .expect("read failed");

        // It's possible that multiple super-blocks were written, so cater for that.

        // The sequence numbers should be one apart.
        assert_eq!(
            (super_block_header_b_after.generation as i64
                - super_block_header_a_after.generation as i64)
                .abs(),
            1
        );

        // At least one super-block should have been written.
        assert!(
            std::cmp::max(
                super_block_header_a_after.generation,
                super_block_header_b_after.generation
            ) > super_block_header_a.generation
        );

        // They should have the same oddness.
        assert_eq!(super_block_header_a_after.generation & 1, super_block_header_a.generation & 1);
    }

    #[fuchsia::test]
    async fn test_root_parent_is_compacted() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));

        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");

        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        let store = fs.root_parent_store();
        let handle = ObjectStore::create_object(
            &store,
            &mut transaction,
            HandleOptions::default(),
            None,
            None,
        )
        .await
        .expect("create_object failed");
        transaction.commit().await.expect("commit failed");

        store.tombstone(handle.object_id(), Options::default()).await.expect("tombstone failed");

        // Generate enough work to induce a journal flush.
        let root_store = fs.root_store();
        for _ in 0..8000 {
            let mut transaction = fs
                .clone()
                .new_transaction(&[], Options::default())
                .await
                .expect("new_transaction failed");
            ObjectStore::create_object(
                &root_store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed");
            transaction.commit().await.expect("commit failed");
        }

        // The root parent store should have been compacted, so we shouldn't be able to find any
        // record referring to the object we tombstoned.
        assert_eq!(
            store.tree().find(&ObjectKey::object(handle.object_id())).await.expect("find failed"),
            None
        );
    }
}
