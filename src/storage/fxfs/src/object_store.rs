// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod allocator;
pub mod caching_object_handle;
mod data_object_handle;
pub mod directory;
mod extent_record;
mod flush;
pub mod graveyard;
pub mod journal;
mod key_manager;
pub(crate) mod merge;
pub mod object_manager;
pub mod object_record;
pub mod project_id;
mod store_object_handle;
pub mod transaction;
mod tree;
mod tree_cache;
pub mod volume;

pub use data_object_handle::{
    DataObjectHandle, DirectWriter, FsverityState, FsverityStateInner, RangeType,
};
pub use directory::Directory;
pub use object_record::{ChildValue, ObjectDescriptor, PosixAttributes, Timestamp};
pub use store_object_handle::{
    SetExtendedAttributeMode, StoreObjectHandle, EXTENDED_ATTRIBUTE_RANGE_END,
    EXTENDED_ATTRIBUTE_RANGE_START,
};

use crate::errors::FxfsError;
use crate::filesystem::{
    ApplyContext, ApplyMode, FxFilesystem, JournalingObject, SyncOptions, MAX_FILE_SIZE,
};
use crate::log::*;
use crate::lsm_tree::cache::{NullCache, ObjectCache};
use crate::lsm_tree::types::{Item, ItemRef, LayerIterator};
use crate::lsm_tree::{LSMTree, Query};
use crate::object_handle::{ObjectHandle, ReadObjectHandle, INVALID_OBJECT_ID};
use crate::object_store::allocator::Allocator;
use crate::object_store::graveyard::Graveyard;
use crate::object_store::journal::{JournalCheckpoint, JournaledTransaction};
use crate::object_store::key_manager::KeyManager;
use crate::object_store::transaction::{
    lock_keys, AssocObj, AssociatedObject, LockKey, ObjectStoreMutation, Operation, Options,
    Transaction,
};
use crate::range::RangeExt;
use crate::round::round_up;
use crate::serialized_types::{migrate_to_version, Migrate, Version, Versioned, VersionedLatest};
use anyhow::{anyhow, bail, ensure, Context, Error};
use async_trait::async_trait;
use fidl_fuchsia_io as fio;
use fprint::TypeFingerprint;
use fuchsia_inspect::ArrayProperty;
use fxfs_crypto::ff1::Ff1;
use fxfs_crypto::{Crypt, KeyPurpose, StreamCipher, WrappedKeyV32, WrappedKeyV40, WrappedKeys};
use once_cell::sync::OnceCell;
use scopeguard::ScopeGuard;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use storage_device::Device;
use uuid::Uuid;

pub use extent_record::{
    ExtentKey, ExtentMode, ExtentValue, BLOB_MERKLE_ATTRIBUTE_ID, DEFAULT_DATA_ATTRIBUTE_ID,
    FSVERITY_MERKLE_ATTRIBUTE_ID,
};
pub use object_record::{
    AttributeKey, EncryptionKeys, ExtendedAttributeValue, FsverityMetadata, ObjectAttributes,
    ObjectKey, ObjectKeyData, ObjectKind, ObjectValue, ProjectProperty, RootDigest,
};
pub use transaction::Mutation;

// For encrypted stores, the lower 32 bits of the object ID are encrypted to make side-channel
// attacks more difficult. This mask can be used to extract the hi part of the object ID.
const OBJECT_ID_HI_MASK: u64 = 0xffffffff00000000;

// At time of writing, this threshold limits transactions that delete extents to about 10,000 bytes.
const TRANSACTION_MUTATION_THRESHOLD: usize = 200;

// Encrypted files and directories use the fscrypt key (identified by `FSCRYPT_KEY_ID`) to encrypt
// file contents and filenames respectively. All non-fscrypt encrypted files otherwise default to
// using the `VOLUME_DATA_KEY_ID` key. Note, the filesystem always uses the `VOLUME_DATA_KEY_ID`
// key to encrypt large extended attributes. Thus, encrypted files and directories with large
// xattrs will have both an fscrypt and volume data key.
pub const VOLUME_DATA_KEY_ID: u64 = 0;
pub const FSCRYPT_KEY_ID: u64 = 1;

/// DataObjectHandle stores an owner that must implement this trait, which allows the handle to get
/// back to an ObjectStore.
pub trait HandleOwner: AsRef<ObjectStore> + Send + Sync + 'static {}

/// StoreInfo stores information about the object store.  This is stored within the parent object
/// store, and is used, for example, to get the persistent layer objects.
pub type StoreInfo = StoreInfoV40;

#[derive(Clone, Debug, Default, Serialize, Deserialize, TypeFingerprint, Versioned)]
pub struct StoreInfoV40 {
    /// The globally unique identifier for the associated object store. If unset, will be all zero.
    guid: [u8; 16],

    /// The last used object ID.  Note that this field is not accurate in memory; ObjectStore's
    /// last_object_id field is the one to use in that case.  Technically, this might not be the
    /// last object ID used for the latest transaction that created an object because we use this at
    /// the point of creating the object but before we commit the transaction.  Transactions can
    /// then get committed in an arbitrary order (or not at all).
    last_object_id: u64,

    /// Object ids for layers.  TODO(https://fxbug.dev/42178036): need a layer of indirection here
    /// so we can support snapshots.
    pub layers: Vec<u64>,

    /// The object ID for the root directory.
    root_directory_object_id: u64,

    /// The object ID for the graveyard.
    graveyard_directory_object_id: u64,

    /// The number of live objects in the store.  This should *not* be trusted; it can be invalid
    /// due to filesystem inconsistencies.
    object_count: u64,

    /// The (wrapped) key that encrypted mutations should use.
    mutations_key: Option<WrappedKeyV40>,

    /// Mutations for the store are encrypted using a stream cipher.  To decrypt the mutations, we
    /// need to know the offset in the cipher stream to start it.
    mutations_cipher_offset: u64,

    /// If we have to flush the store whilst we do not have the key, we need to write the encrypted
    /// mutations to an object. This is the object ID of that file if it exists.
    pub encrypted_mutations_object_id: u64,

    /// Object IDs are encrypted to reduce the amount of information that sequential object IDs
    /// reveal (such as the number of files in the system and the ordering of their creation in
    /// time).  Only the bottom 32 bits of the object ID are encrypted whilst the top 32 bits will
    /// increment after 2^32 object IDs have been used and this allows us to roll the key.
    object_id_key: Option<WrappedKeyV40>,

    /// A directory for storing internal files in a directory structure. Holds INVALID_OBJECT_ID
    /// when the directory doesn't yet exist.
    internal_directory_object_id: u64,
}

#[derive(Default, Migrate, Serialize, Deserialize, TypeFingerprint, Versioned)]
#[migrate_to_version(StoreInfoV40)]
pub struct StoreInfoV36 {
    guid: [u8; 16],
    last_object_id: u64,
    pub layers: Vec<u64>,
    root_directory_object_id: u64,
    graveyard_directory_object_id: u64,
    object_count: u64,
    mutations_key: Option<WrappedKeyV32>,
    mutations_cipher_offset: u64,
    pub encrypted_mutations_object_id: u64,
    object_id_key: Option<WrappedKeyV32>,
    internal_directory_object_id: u64,
}

#[derive(Migrate, Serialize, Deserialize, TypeFingerprint, Versioned)]
#[migrate_to_version(StoreInfoV36)]
pub struct StoreInfoV32 {
    guid: [u8; 16],
    last_object_id: u64,
    pub layers: Vec<u64>,
    root_directory_object_id: u64,
    graveyard_directory_object_id: u64,
    object_count: u64,
    mutations_key: Option<WrappedKeyV32>,
    mutations_cipher_offset: u64,
    pub encrypted_mutations_object_id: u64,
    object_id_key: Option<WrappedKeyV32>,
}

impl StoreInfo {
    /// Create a new/default [`StoreInfo`] but with a newly generated GUID.
    fn new_with_guid() -> Self {
        let guid = Uuid::new_v4();
        Self { guid: *guid.as_bytes(), ..Default::default() }
    }

    /// Returns the parent objects for this store.
    pub fn parent_objects(&self) -> Vec<u64> {
        // We should not include the ID of the store itself, since that should be referred to in the
        // volume directory.
        let mut objects = self.layers.to_vec();
        if self.encrypted_mutations_object_id != INVALID_OBJECT_ID {
            objects.push(self.encrypted_mutations_object_id);
        }
        objects
    }
}

// TODO(https://fxbug.dev/42178037): We should test or put checks in place to ensure this limit isn't exceeded.
// It will likely involve placing limits on the maximum number of layers.
pub const MAX_STORE_INFO_SERIALIZED_SIZE: usize = 131072;

// This needs to be large enough to accommodate the maximum amount of unflushed data (data that is
// in the journal but hasn't yet been written to layer files) for a store.  We set a limit because
// we want to limit the amount of memory use in the case the filesystem is corrupt or under attack.
pub const MAX_ENCRYPTED_MUTATIONS_SIZE: usize = 8 * journal::DEFAULT_RECLAIM_SIZE as usize;

#[derive(Default)]
pub struct HandleOptions {
    /// If true, transactions used by this handle will skip journal space checks.
    pub skip_journal_checks: bool,
    /// If true, data written to any attribute of this handle will not have per-block checksums
    /// computed.
    pub skip_checksums: bool,
}

#[derive(Default)]
pub struct NewChildStoreOptions {
    /// The store is unencrypted if store is none.
    pub crypt: Option<Arc<dyn Crypt>>,

    /// Specifies the object ID in the root store to be used for the store.  If set to
    /// INVALID_OBJECT_ID (the default and typical case), a suitable ID will be chosen.
    pub object_id: u64,
}

pub type EncryptedMutations = EncryptedMutationsV40;

#[derive(Clone, Default, Deserialize, Serialize, TypeFingerprint)]
pub struct EncryptedMutationsV40 {
    // Information about the mutations are held here, but the actual encrypted data is held within
    // data.  For each transaction, we record the checkpoint and the count of mutations within the
    // transaction.  The checkpoint is required for the log file offset (which we need to apply the
    // mutations), and the version so that we can correctly decode the mutation after it has been
    // decrypted. The count specifies the number of serialized mutations encoded in |data|.
    transactions: Vec<(JournalCheckpoint, u64)>,

    // The encrypted mutations.
    data: Vec<u8>,

    // If the mutations key was rolled, this holds the offset in `data` where the new key should
    // apply.
    mutations_key_roll: Vec<(usize, WrappedKeyV40)>,
}

#[derive(Deserialize, Serialize, TypeFingerprint, Versioned)]
pub struct EncryptedMutationsV32 {
    transactions: Vec<(JournalCheckpoint, u64)>,
    data: Vec<u8>,
    mutations_key_roll: Vec<(usize, WrappedKeyV32)>,
}

impl From<EncryptedMutationsV32> for EncryptedMutationsV40 {
    fn from(item: EncryptedMutationsV32) -> Self {
        Self {
            transactions: item.transactions,
            data: item.data,
            mutations_key_roll: item
                .mutations_key_roll
                .into_iter()
                .map(|(a, b)| (a, b.into()))
                .collect(),
        }
    }
}

impl std::fmt::Debug for EncryptedMutations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        f.debug_struct("EncryptedMutations")
            .field("transactions", &self.transactions)
            .field("len", &self.data.len())
            .field(
                "mutations_key_roll",
                &self.mutations_key_roll.iter().map(|k| k.0).collect::<Vec<usize>>(),
            )
            .finish()
    }
}

impl Versioned for EncryptedMutations {
    fn max_serialized_size() -> u64 {
        MAX_ENCRYPTED_MUTATIONS_SIZE as u64
    }
}

impl EncryptedMutations {
    fn from_replayed_mutations(
        store_object_id: u64,
        transactions: Vec<JournaledTransaction>,
    ) -> Self {
        let mut this = Self::default();
        for JournaledTransaction { checkpoint, non_root_mutations, .. } in transactions {
            for (object_id, mutation) in non_root_mutations {
                if store_object_id == object_id {
                    if let Mutation::EncryptedObjectStore(data) = mutation {
                        this.push(&checkpoint, data);
                    } else if let Mutation::UpdateMutationsKey(key) = mutation {
                        this.mutations_key_roll.push((this.data.len(), key.into()));
                    }
                }
            }
        }
        this
    }

    fn extend(&mut self, other: &EncryptedMutations) {
        self.transactions.extend_from_slice(&other.transactions[..]);
        self.mutations_key_roll.extend(
            other
                .mutations_key_roll
                .iter()
                .map(|(offset, key)| (offset + self.data.len(), key.clone())),
        );
        self.data.extend_from_slice(&other.data[..]);
    }

    fn push(&mut self, checkpoint: &JournalCheckpoint, data: Box<[u8]>) {
        self.data.append(&mut data.into());
        // If the checkpoint is the same as the last mutation we pushed, increment the count.
        if let Some((last_checkpoint, count)) = self.transactions.last_mut() {
            if last_checkpoint.file_offset == checkpoint.file_offset {
                *count += 1;
                return;
            }
        }
        self.transactions.push((checkpoint.clone(), 1));
    }
}

pub enum LockState {
    Locked,
    Unencrypted,
    Unlocked(Arc<dyn Crypt>),

    // The store is unlocked, but in a read-only state, and no flushes or other operations will be
    // performed on the store.
    UnlockedReadOnly(Arc<dyn Crypt>),

    // The store is encrypted but is now in an unusable state (due to a failure to sync the journal
    // after locking the store).  The store cannot be unlocked.
    Invalid,

    // Before we've read the StoreInfo we might not know whether the store is Locked or Unencrypted.
    // This can happen when lazily opening stores (ObjectManager::lazy_open_store).
    Unknown,

    // The store is in the process of being locked.  Whilst the store is being locked, the store
    // isn't usable; assertions will trip if any mutations are applied.
    Locking,

    // Whilst we're unlocking, we will replay encrypted mutations.  The store isn't usable until
    // it's in the Unlocked state.
    Unlocking,
}

impl fmt::Debug for LockState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            LockState::Locked => "Locked",
            LockState::Unencrypted => "Unencrypted",
            LockState::Unlocked(_) => "Unlocked",
            LockState::UnlockedReadOnly(..) => "UnlockedReadOnly",
            LockState::Invalid => "Invalid",
            LockState::Unknown => "Unknown",
            LockState::Locking => "Locking",
            LockState::Unlocking => "Unlocking",
        })
    }
}

#[derive(Default)]
struct LastObjectId {
    // The *unencrypted* value of the last object ID.
    id: u64,

    // Encrypted stores will use a cipher to obfuscate the object ID.
    cipher: Option<Ff1>,
}

impl LastObjectId {
    // Returns true if a cipher is needed to generate new object IDs.
    fn should_create_cipher(&self) -> bool {
        self.id as u32 == u32::MAX
    }

    fn get_next_object_id(&mut self) -> u64 {
        self.id += 1;
        if let Some(cipher) = &self.cipher {
            let hi = self.id & OBJECT_ID_HI_MASK;
            assert_ne!(hi, INVALID_OBJECT_ID);
            assert_ne!(self.id as u32, 0); // This would indicate the ID wrapped.
            hi | cipher.encrypt(self.id as u32) as u64
        } else {
            self.id
        }
    }
}

/// An object store supports a file like interface for objects.  Objects are keyed by a 64 bit
/// identifier.  And object store has to be backed by a parent object store (which stores metadata
/// for the object store).  The top-level object store (a.k.a. the root parent object store) is
/// in-memory only.
pub struct ObjectStore {
    parent_store: Option<Arc<ObjectStore>>,
    store_object_id: u64,
    device: Arc<dyn Device>,
    block_size: u64,
    filesystem: Weak<FxFilesystem>,
    // Lock ordering: This must be taken before `lock_state`.
    store_info: Mutex<Option<StoreInfo>>,
    tree: LSMTree<ObjectKey, ObjectValue>,

    // When replaying the journal, the store cannot read StoreInfo until the whole journal
    // has been replayed, so during that time, store_info_handle will be None and records
    // just get sent to the tree. Once the journal has been replayed, we can open the store
    // and load all the other layer information.
    store_info_handle: OnceCell<DataObjectHandle<ObjectStore>>,

    // The cipher to use for encrypted mutations, if this store is encrypted.
    mutations_cipher: Mutex<Option<StreamCipher>>,

    // Current lock state of the store.
    // Lock ordering: This must be taken after `store_info`.
    lock_state: Mutex<LockState>,
    key_manager: KeyManager,

    // Enable/disable tracing.
    trace: AtomicBool,

    // Informational counters for events occurring within the store.
    counters: Mutex<ObjectStoreCounters>,

    // These are updated in performance-sensitive code paths so we use atomics instead of counters.
    device_read_ops: AtomicU64,
    device_write_ops: AtomicU64,
    logical_read_ops: AtomicU64,
    logical_write_ops: AtomicU64,

    // Contains the last object ID and, optionally, a cipher to be used when generating new object
    // IDs.
    last_object_id: Mutex<LastObjectId>,

    // An optional callback to be invoked each time the ObjectStore flushes.  The callback is
    // invoked at the end of flush, while the write lock is still held.
    flush_callback: Mutex<Option<Box<dyn Fn(&ObjectStore) + Send + Sync + 'static>>>,
}

#[derive(Clone, Default)]
struct ObjectStoreCounters {
    mutations_applied: u64,
    mutations_dropped: u64,
    num_flushes: u64,
    last_flush_time: Option<std::time::SystemTime>,
    persistent_layer_file_sizes: Vec<u64>,
}

impl ObjectStore {
    fn new(
        parent_store: Option<Arc<ObjectStore>>,
        store_object_id: u64,
        filesystem: Arc<FxFilesystem>,
        store_info: Option<StoreInfo>,
        object_cache: Box<dyn ObjectCache<ObjectKey, ObjectValue>>,
        mutations_cipher: Option<StreamCipher>,
        lock_state: LockState,
        last_object_id: LastObjectId,
    ) -> Arc<ObjectStore> {
        let device = filesystem.device();
        let block_size = filesystem.block_size();
        Arc::new(ObjectStore {
            parent_store,
            store_object_id,
            device,
            block_size,
            filesystem: Arc::downgrade(&filesystem),
            store_info: Mutex::new(store_info),
            tree: LSMTree::new(merge::merge, object_cache),
            store_info_handle: OnceCell::new(),
            mutations_cipher: Mutex::new(mutations_cipher),
            lock_state: Mutex::new(lock_state),
            key_manager: KeyManager::new(),
            trace: AtomicBool::new(false),
            counters: Mutex::new(ObjectStoreCounters::default()),
            device_read_ops: AtomicU64::new(0),
            device_write_ops: AtomicU64::new(0),
            logical_read_ops: AtomicU64::new(0),
            logical_write_ops: AtomicU64::new(0),
            last_object_id: Mutex::new(last_object_id),
            flush_callback: Mutex::new(None),
        })
    }

    fn new_empty(
        parent_store: Option<Arc<ObjectStore>>,
        store_object_id: u64,
        filesystem: Arc<FxFilesystem>,
        object_cache: Box<dyn ObjectCache<ObjectKey, ObjectValue>>,
    ) -> Arc<Self> {
        Self::new(
            parent_store,
            store_object_id,
            filesystem,
            Some(StoreInfo::default()),
            object_cache,
            None,
            LockState::Unencrypted,
            LastObjectId::default(),
        )
    }

    /// Cycle breaker constructor that returns an ObjectStore without a filesystem.
    /// This should only be used from super block code.
    pub fn new_root_parent(device: Arc<dyn Device>, block_size: u64, store_object_id: u64) -> Self {
        ObjectStore {
            parent_store: None,
            store_object_id,
            device,
            block_size,
            filesystem: Weak::<FxFilesystem>::new(),
            store_info: Mutex::new(Some(StoreInfo::default())),
            tree: LSMTree::new(merge::merge, Box::new(NullCache {})),
            store_info_handle: OnceCell::new(),
            mutations_cipher: Mutex::new(None),
            lock_state: Mutex::new(LockState::Unencrypted),
            key_manager: KeyManager::new(),
            trace: AtomicBool::new(false),
            counters: Mutex::new(ObjectStoreCounters::default()),
            device_read_ops: AtomicU64::new(0),
            device_write_ops: AtomicU64::new(0),
            logical_read_ops: AtomicU64::new(0),
            logical_write_ops: AtomicU64::new(0),
            last_object_id: Mutex::new(LastObjectId::default()),
            flush_callback: Mutex::new(None),
        }
    }

    /// Used to set filesystem on root_parent stores at bootstrap time after the filesystem has
    /// been created.
    pub fn attach_filesystem(mut this: ObjectStore, filesystem: Arc<FxFilesystem>) -> ObjectStore {
        this.filesystem = Arc::downgrade(&filesystem);
        this
    }

    /// Create a child store. It is a multi-step process:
    ///
    ///   1. Call `ObjectStore::new_child_store`.
    ///   2. Register the store with the object-manager.
    ///   3. Call `ObjectStore::create` to write the store-info.
    ///
    /// If the procedure fails, care must be taken to unregister store with the object-manager.
    ///
    /// The steps have to be separate because of lifetime issues when working with a transaction.
    async fn new_child_store(
        self: &Arc<Self>,
        transaction: &mut Transaction<'_>,
        options: NewChildStoreOptions,
        object_cache: Box<dyn ObjectCache<ObjectKey, ObjectValue>>,
    ) -> Result<Arc<Self>, Error> {
        let handle = if options.object_id != INVALID_OBJECT_ID {
            ObjectStore::create_object_with_id(
                self,
                transaction,
                options.object_id,
                HandleOptions::default(),
                None,
                None,
            )
            .await?
        } else {
            ObjectStore::create_object(self, transaction, HandleOptions::default(), None, None)
                .await?
        };
        let filesystem = self.filesystem();
        let store = if let Some(crypt) = options.crypt {
            let (wrapped_key, unwrapped_key) =
                crypt.create_key(handle.object_id(), KeyPurpose::Metadata).await?;
            let (object_id_wrapped, object_id_unwrapped) =
                crypt.create_key(handle.object_id(), KeyPurpose::Metadata).await?;
            Self::new(
                Some(self.clone()),
                handle.object_id(),
                filesystem.clone(),
                Some(StoreInfo {
                    mutations_key: Some(wrapped_key),
                    object_id_key: Some(object_id_wrapped),
                    ..StoreInfo::new_with_guid()
                }),
                object_cache,
                Some(StreamCipher::new(&unwrapped_key, 0)),
                LockState::Unlocked(crypt),
                LastObjectId {
                    // We need to avoid accidentally getting INVALID_OBJECT_ID, so we set
                    // the top 32 bits to a non-zero value.
                    id: 1 << 32,
                    cipher: Some(Ff1::new(&object_id_unwrapped)),
                },
            )
        } else {
            Self::new(
                Some(self.clone()),
                handle.object_id(),
                filesystem.clone(),
                Some(StoreInfo::new_with_guid()),
                object_cache,
                None,
                LockState::Unencrypted,
                LastObjectId::default(),
            )
        };
        assert!(store.store_info_handle.set(handle).is_ok());
        Ok(store)
    }

    /// Actually creates the store in a transaction.  This will also create a root directory and
    /// graveyard directory for the store.  See `new_child_store` above.
    async fn create<'a>(
        self: &'a Arc<Self>,
        transaction: &mut Transaction<'a>,
    ) -> Result<(), Error> {
        let buf = {
            // Create a root directory and graveyard directory.
            let graveyard_directory_object_id = Graveyard::create(transaction, &self);
            let root_directory = Directory::create(transaction, &self, None).await?;

            let serialized_info = {
                let mut store_info = self.store_info.lock().unwrap();
                let store_info = store_info.as_mut().unwrap();

                store_info.graveyard_directory_object_id = graveyard_directory_object_id;
                store_info.root_directory_object_id = root_directory.object_id();

                let mut serialized_info = Vec::new();
                store_info.serialize_with_version(&mut serialized_info)?;
                serialized_info
            };
            let mut buf = self.device.allocate_buffer(serialized_info.len()).await;
            buf.as_mut_slice().copy_from_slice(&serialized_info[..]);
            buf
        };

        self.store_info_handle.get().unwrap().txn_write(transaction, 0u64, buf.as_ref()).await
    }

    pub fn set_trace(&self, trace: bool) {
        let old_value = self.trace.swap(trace, Ordering::Relaxed);
        if trace != old_value {
            info!(store_id = self.store_object_id(), trace; "OS: trace",);
        }
    }

    /// Sets a callback to be invoked each time the ObjectStore flushes.  The callback is invoked at
    /// the end of flush, while the write lock is still held.
    pub fn set_flush_callback<F: Fn(&ObjectStore) + Send + Sync + 'static>(&self, callback: F) {
        let mut flush_callback = self.flush_callback.lock().unwrap();
        *flush_callback = Some(Box::new(callback));
    }

    pub fn is_root(&self) -> bool {
        if let Some(parent) = &self.parent_store {
            parent.parent_store.is_none()
        } else {
            // The root parent store isn't the root store.
            false
        }
    }

    /// Populates an inspect node with store statistics.
    pub fn record_data(self: &Arc<Self>, root: &fuchsia_inspect::Node) {
        // TODO(https://fxbug.dev/42069513): Push-back or rate-limit to prevent DoS.
        let counters = self.counters.lock().unwrap();
        if let Some(store_info) = self.store_info() {
            root.record_string("guid", Uuid::from_bytes(store_info.guid).to_string());
        } else {
            warn!("Can't access store_info; store is locked.");
        };
        root.record_uint("store_object_id", self.store_object_id);
        root.record_uint("mutations_applied", counters.mutations_applied);
        root.record_uint("mutations_dropped", counters.mutations_dropped);
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
        root.record_uint("device_read_ops", self.device_read_ops.load(Ordering::Relaxed));
        root.record_uint("device_write_ops", self.device_write_ops.load(Ordering::Relaxed));
        root.record_uint("logical_read_ops", self.logical_read_ops.load(Ordering::Relaxed));
        root.record_uint("logical_write_ops", self.logical_write_ops.load(Ordering::Relaxed));

        root.record(sizes);

        let this = self.clone();
        root.record_child("lsm_tree", move |node| this.tree().record_inspect_data(node));
    }

    pub fn device(&self) -> &Arc<dyn Device> {
        &self.device
    }

    pub fn block_size(&self) -> u64 {
        self.block_size
    }

    pub fn filesystem(&self) -> Arc<FxFilesystem> {
        self.filesystem.upgrade().unwrap()
    }

    pub fn store_object_id(&self) -> u64 {
        self.store_object_id
    }

    pub fn tree(&self) -> &LSMTree<ObjectKey, ObjectValue> {
        &self.tree
    }

    pub fn root_directory_object_id(&self) -> u64 {
        self.store_info.lock().unwrap().as_ref().unwrap().root_directory_object_id
    }

    pub fn graveyard_directory_object_id(&self) -> u64 {
        self.store_info.lock().unwrap().as_ref().unwrap().graveyard_directory_object_id
    }

    fn set_graveyard_directory_object_id(&self, oid: u64) {
        assert_eq!(
            std::mem::replace(
                &mut self
                    .store_info
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .graveyard_directory_object_id,
                oid
            ),
            INVALID_OBJECT_ID
        );
    }

    pub fn object_count(&self) -> u64 {
        self.store_info.lock().unwrap().as_ref().unwrap().object_count
    }

    pub fn key_manager(&self) -> &KeyManager {
        &self.key_manager
    }

    pub fn parent_store(&self) -> Option<&Arc<ObjectStore>> {
        self.parent_store.as_ref()
    }

    /// Returns the crypt object for the store. Returns None if the store is unencrypted. This will
    /// panic if the store is locked.
    pub fn crypt(&self) -> Option<Arc<dyn Crypt>> {
        match &*self.lock_state.lock().unwrap() {
            LockState::Locked => panic!("Store is locked"),
            LockState::Invalid
            | LockState::Unencrypted
            | LockState::Locking
            | LockState::Unlocking => None,
            LockState::Unlocked(crypt) => Some(crypt.clone()),
            LockState::UnlockedReadOnly(crypt) => Some(crypt.clone()),
            LockState::Unknown => {
                panic!("Store is of unknown lock state; has the journal been replayed yet?")
            }
        }
    }

    pub async fn get_or_create_internal_directory_id(self: &Arc<Self>) -> Result<u64, Error> {
        // Create the transaction first to use the object store lock.
        let mut transaction = self
            .filesystem()
            .new_transaction(
                lock_keys![LockKey::object(
                    self.parent_store.as_ref().unwrap().store_object_id,
                    self.store_object_id,
                )],
                Options::default(),
            )
            .await?;
        let obj_id = self.store_info.lock().unwrap().as_ref().unwrap().internal_directory_object_id;
        if obj_id != INVALID_OBJECT_ID {
            return Ok(obj_id);
        }

        // Need to create an internal directory.
        let directory = Directory::create(&mut transaction, self, None).await?;

        transaction.add(self.store_object_id, Mutation::CreateInternalDir(directory.object_id()));
        transaction.commit().await?;
        Ok(directory.object_id())
    }

    /// Returns the file size for the object without opening the object.
    async fn get_file_size(&self, object_id: u64) -> Result<u64, Error> {
        let item = self
            .tree
            .find(&ObjectKey::attribute(
                object_id,
                DEFAULT_DATA_ATTRIBUTE_ID,
                AttributeKey::Attribute,
            ))
            .await?
            .ok_or(FxfsError::NotFound)?;
        if let ObjectValue::Attribute { size, .. } = item.value {
            Ok(size)
        } else {
            bail!(FxfsError::NotFile);
        }
    }

    /// `crypt` can be provided if the crypt service should be different to the default; see the
    /// comment on create_object.  Users should avoid having more than one handle open for the same
    /// object at the same time because they might get out-of-sync; there is no code that will
    /// prevent this.  One example where this can cause an issue is if the object ends up using a
    /// permanent key (which is the case if a value is passed for `crypt`), the permanent key is
    /// dropped when a handle is dropped, which will impact any other handles for the same object.
    pub async fn open_object<S: HandleOwner>(
        owner: &Arc<S>,
        obj_id: u64,
        options: HandleOptions,
        crypt: Option<Arc<dyn Crypt>>,
    ) -> Result<DataObjectHandle<S>, Error> {
        let store = owner.as_ref().as_ref();
        let mut fsverity_descriptor = None;
        let mut overwrite_ranges = Vec::new();
        let item = store
            .tree
            .find(&ObjectKey::attribute(obj_id, DEFAULT_DATA_ATTRIBUTE_ID, AttributeKey::Attribute))
            .await?
            .ok_or(FxfsError::NotFound)?;

        let (size, track_overwrite_extents) = match item.value {
            ObjectValue::Attribute { size, has_overwrite_extents } => (size, has_overwrite_extents),
            ObjectValue::VerifiedAttribute { size, fsverity_metadata } => {
                fsverity_descriptor = Some(fsverity_metadata);
                // We only track the overwrite extents in memory for writes, reads handle them
                // implicitly, which means verified files (where the data won't change anymore)
                // don't need to track them.
                (size, false)
            }
            _ => bail!(anyhow!(FxfsError::Inconsistent).context("open_object: Expected attibute")),
        };

        ensure!(size <= MAX_FILE_SIZE, FxfsError::Inconsistent);

        if track_overwrite_extents {
            let layer_set = store.tree.layer_set();
            let mut merger = layer_set.merger();
            let mut iter = merger
                .query(Query::FullRange(&ObjectKey::attribute(
                    obj_id,
                    DEFAULT_DATA_ATTRIBUTE_ID,
                    AttributeKey::Extent(ExtentKey::search_key_from_offset(0)),
                )))
                .await?;
            loop {
                match iter.get() {
                    Some(ItemRef {
                        key:
                            ObjectKey {
                                object_id,
                                data:
                                    ObjectKeyData::Attribute(
                                        attribute_id,
                                        AttributeKey::Extent(ExtentKey { range }),
                                    ),
                            },
                        value,
                        ..
                    }) if *object_id == obj_id && *attribute_id == DEFAULT_DATA_ATTRIBUTE_ID => {
                        match value {
                            ObjectValue::Extent(ExtentValue::None)
                            | ObjectValue::Extent(ExtentValue::Some {
                                mode: ExtentMode::Raw,
                                ..
                            })
                            | ObjectValue::Extent(ExtentValue::Some {
                                mode: ExtentMode::Cow(_),
                                ..
                            }) => (),
                            ObjectValue::Extent(ExtentValue::Some {
                                mode: ExtentMode::OverwritePartial(_),
                                ..
                            })
                            | ObjectValue::Extent(ExtentValue::Some {
                                mode: ExtentMode::Overwrite,
                                ..
                            }) => overwrite_ranges.push(range.clone()),
                            _ => bail!(anyhow!(FxfsError::Inconsistent)
                                .context("open_object: Expected extent")),
                        }
                        iter.advance().await?;
                    }
                    _ => break,
                }
            }
        }

        // If a crypt service has been specified, it needs to be a permanent key because cached
        // keys can only use the store's crypt service.
        let permanent = if let Some(crypt) = crypt {
            store
                .key_manager
                .get_keys(
                    obj_id,
                    crypt.as_ref(),
                    &mut Some(async || store.get_keys(obj_id).await),
                    /* permanent= */ true,
                    /* force= */ false,
                )
                .await?;
            true
        } else {
            false
        };
        let data_object_handle = DataObjectHandle::new(
            owner.clone(),
            obj_id,
            permanent,
            DEFAULT_DATA_ATTRIBUTE_ID,
            size,
            FsverityState::None,
            options,
            false,
            overwrite_ranges,
        );
        if let Some(descriptor) = fsverity_descriptor {
            match data_object_handle.read_attr(FSVERITY_MERKLE_ATTRIBUTE_ID).await? {
                None => {
                    return Err(anyhow!(FxfsError::NotFound));
                }
                Some(data) => {
                    data_object_handle.set_fsverity_state_some(descriptor, data);
                }
            }
        }
        Ok(data_object_handle)
    }

    // See the comment on create_object for the semantics of the `crypt` argument.  If object_id ==
    // INVALID_OBJECT_ID (which should usually be the case), an object ID will be chosen.
    async fn create_object_with_id<S: HandleOwner>(
        owner: &Arc<S>,
        transaction: &mut Transaction<'_>,
        mut object_id: u64,
        options: HandleOptions,
        mut crypt: Option<&dyn Crypt>,
        wrapping_key_id: Option<u128>,
    ) -> Result<DataObjectHandle<S>, Error> {
        let store = owner.as_ref().as_ref();
        if object_id == INVALID_OBJECT_ID {
            object_id = store.get_next_object_id(transaction).await?;
        } else {
            store.update_last_object_id(object_id);
        }
        let store_crypt;
        // See comment for equivalent in open_object.
        let permanent = crypt.is_some();
        if crypt.is_none() {
            store_crypt = store.crypt();
            crypt = store_crypt.as_deref();
        }
        let now = Timestamp::now();
        transaction.add(
            store.store_object_id(),
            Mutation::insert_object(
                ObjectKey::object(object_id),
                ObjectValue::file(1, 0, now.clone(), now.clone(), now.clone(), now, 0, None),
            ),
        );
        let key_id = if wrapping_key_id.is_some() { FSCRYPT_KEY_ID } else { VOLUME_DATA_KEY_ID };
        if let Some(crypt) = crypt {
            let (key, unwrapped_key) = if let Some(wrapping_key_id) = wrapping_key_id {
                crypt.create_key_with_id(object_id, wrapping_key_id).await?
            } else {
                crypt.create_key(object_id, KeyPurpose::Data).await?
            };
            transaction.add(
                store.store_object_id(),
                Mutation::insert_object(
                    ObjectKey::keys(object_id),
                    ObjectValue::keys(EncryptionKeys::AES256XTS(WrappedKeys::from(vec![(
                        key_id, key,
                    )]))),
                ),
            );
            store.key_manager.insert(object_id, &vec![(key_id, Some(unwrapped_key))], permanent);
        }
        transaction.add(
            store.store_object_id(),
            Mutation::insert_object(
                ObjectKey::attribute(object_id, DEFAULT_DATA_ATTRIBUTE_ID, AttributeKey::Attribute),
                // This is a new object so nothing has pre-allocated overwrite extents yet.
                ObjectValue::attribute(0, false),
            ),
        );
        Ok(DataObjectHandle::new(
            owner.clone(),
            object_id,
            permanent,
            DEFAULT_DATA_ATTRIBUTE_ID,
            0,
            FsverityState::None,
            options,
            false,
            Vec::new(),
        ))
    }

    /// There are instances where a store might not be an encrypted store, but the object should
    /// still be encrypted.  For example, the layer files for child stores should be encrypted using
    /// the crypt service of the child store even though the root store doesn't have encryption.  If
    /// `crypt` is None, the default for the store is used (and prefer to pass None over passing the
    /// store's crypt service directly because the latter will end up using a permanent key that
    /// isn't purged when inactive).
    /// TODO(https://fxbug.dev/364428824): Condense all optional arguments into a separate struct.
    pub async fn create_object<S: HandleOwner>(
        owner: &Arc<S>,
        mut transaction: &mut Transaction<'_>,
        options: HandleOptions,
        crypt: Option<&dyn Crypt>,
        wrapping_key_id: Option<u128>,
    ) -> Result<DataObjectHandle<S>, Error> {
        ObjectStore::create_object_with_id(
            owner,
            &mut transaction,
            INVALID_OBJECT_ID,
            options,
            crypt,
            wrapping_key_id,
        )
        .await
    }

    /// Adjusts the reference count for a given object.  If the reference count reaches zero, the
    /// object is moved into the graveyard and true is returned.
    pub async fn adjust_refs(
        &self,
        transaction: &mut Transaction<'_>,
        oid: u64,
        delta: i64,
    ) -> Result<bool, Error> {
        let mut mutation = self.txn_get_object_mutation(transaction, oid).await?;
        let refs = if let ObjectValue::Object {
            kind: ObjectKind::File { refs, .. } | ObjectKind::Symlink { refs, .. },
            ..
        } = &mut mutation.item.value
        {
            *refs =
                refs.checked_add_signed(delta).ok_or_else(|| anyhow!("refs underflow/overflow"))?;
            refs
        } else {
            bail!(FxfsError::NotFile);
        };
        if *refs == 0 {
            self.add_to_graveyard(transaction, oid);

            // We might still need to adjust the reference count if delta was something other than
            // -1.
            if delta != -1 {
                *refs = 1;
                transaction.add(self.store_object_id, Mutation::ObjectStore(mutation));
            }
            Ok(true)
        } else {
            transaction.add(self.store_object_id, Mutation::ObjectStore(mutation));
            Ok(false)
        }
    }

    // Purges an object that is in the graveyard.
    pub async fn tombstone_object(
        &self,
        object_id: u64,
        txn_options: Options<'_>,
    ) -> Result<(), Error> {
        self.key_manager.remove(object_id).await;
        self.trim_or_tombstone(object_id, true, txn_options).await
    }

    /// Trim extents beyond the end of a file for all attributes.  This will remove the entry from
    /// the graveyard when done.
    pub async fn trim(&self, object_id: u64) -> Result<(), Error> {
        // For the root and root parent store, we would need to use the metadata reservation which
        // we don't currently support, so assert that we're not those stores.
        assert!(self.parent_store.as_ref().unwrap().parent_store.is_some());

        self.trim_or_tombstone(
            object_id,
            false,
            Options { borrow_metadata_space: true, ..Default::default() },
        )
        .await
    }

    async fn trim_or_tombstone(
        &self,
        object_id: u64,
        for_tombstone: bool,
        txn_options: Options<'_>,
    ) -> Result<(), Error> {
        let fs = self.filesystem();
        let mut next_attribute = Some(0);
        while let Some(attribute_id) = next_attribute.take() {
            let mut transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![
                        LockKey::object_attribute(self.store_object_id, object_id, attribute_id),
                        LockKey::object(self.store_object_id, object_id),
                    ],
                    txn_options,
                )
                .await?;

            match self
                .trim_some(
                    &mut transaction,
                    object_id,
                    attribute_id,
                    if for_tombstone {
                        TrimMode::Tombstone(TombstoneMode::Object)
                    } else {
                        TrimMode::UseSize
                    },
                )
                .await?
            {
                TrimResult::Incomplete => next_attribute = Some(attribute_id),
                TrimResult::Done(None) => {
                    if for_tombstone
                        || matches!(
                            self.tree
                                .find(&ObjectKey::graveyard_entry(
                                    self.graveyard_directory_object_id(),
                                    object_id,
                                ))
                                .await?,
                            Some(Item { value: ObjectValue::Trim, .. })
                        )
                    {
                        self.remove_from_graveyard(&mut transaction, object_id);
                    }
                }
                TrimResult::Done(id) => next_attribute = id,
            }

            if !transaction.mutations().is_empty() {
                transaction.commit().await?;
            }
        }
        Ok(())
    }

    // Purges an object's attribute that is in the graveyard.
    pub async fn tombstone_attribute(
        &self,
        object_id: u64,
        attribute_id: u64,
        txn_options: Options<'_>,
    ) -> Result<(), Error> {
        let fs = self.filesystem();
        let mut trim_result = TrimResult::Incomplete;
        while matches!(trim_result, TrimResult::Incomplete) {
            let mut transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![
                        LockKey::object_attribute(self.store_object_id, object_id, attribute_id),
                        LockKey::object(self.store_object_id, object_id),
                    ],
                    txn_options,
                )
                .await?;
            trim_result = self
                .trim_some(
                    &mut transaction,
                    object_id,
                    attribute_id,
                    TrimMode::Tombstone(TombstoneMode::Attribute),
                )
                .await?;
            if let TrimResult::Done(..) = trim_result {
                self.remove_attribute_from_graveyard(&mut transaction, object_id, attribute_id)
            }
            if !transaction.mutations().is_empty() {
                transaction.commit().await?;
            }
        }
        Ok(())
    }

    /// Deletes extents for attribute `attribute_id` in object `object_id`.  Also see the comments
    /// for TrimMode and TrimResult. Should hold a lock on the attribute, and the object as it
    /// performs a read-modify-write on the sizes.
    pub async fn trim_some(
        &self,
        transaction: &mut Transaction<'_>,
        object_id: u64,
        attribute_id: u64,
        mode: TrimMode,
    ) -> Result<TrimResult, Error> {
        let layer_set = self.tree.layer_set();
        let mut merger = layer_set.merger();

        let aligned_offset = match mode {
            TrimMode::FromOffset(offset) => {
                round_up(offset, self.block_size).ok_or(FxfsError::Inconsistent)?
            }
            TrimMode::Tombstone(..) => 0,
            TrimMode::UseSize => {
                let iter = merger
                    .query(Query::FullRange(&ObjectKey::attribute(
                        object_id,
                        attribute_id,
                        AttributeKey::Attribute,
                    )))
                    .await?;
                if let Some(item_ref) = iter.get() {
                    if item_ref.key.object_id != object_id {
                        return Ok(TrimResult::Done(None));
                    }

                    if let ItemRef {
                        key:
                            ObjectKey {
                                data:
                                    ObjectKeyData::Attribute(size_attribute_id, AttributeKey::Attribute),
                                ..
                            },
                        value: ObjectValue::Attribute { size, .. },
                        ..
                    } = item_ref
                    {
                        // If we found a different attribute_id, return so we can get the
                        // right lock.
                        if *size_attribute_id != attribute_id {
                            return Ok(TrimResult::Done(Some(*size_attribute_id)));
                        }
                        round_up(*size, self.block_size).ok_or(FxfsError::Inconsistent)?
                    } else {
                        // At time of writing, we should always see a size record or None here, but
                        // asserting here would be brittle so just skip to the the next attribute
                        // instead.
                        return Ok(TrimResult::Done(Some(attribute_id + 1)));
                    }
                } else {
                    // End of the tree.
                    return Ok(TrimResult::Done(None));
                }
            }
        };

        // Loop over the extents and deallocate them.
        let mut iter = merger
            .query(Query::FullRange(&ObjectKey::from_extent(
                object_id,
                attribute_id,
                ExtentKey::search_key_from_offset(aligned_offset),
            )))
            .await?;
        let mut end = 0;
        let allocator = self.allocator();
        let mut result = TrimResult::Done(None);
        let mut deallocated = 0;
        let block_size = self.block_size;

        while let Some(item_ref) = iter.get() {
            if item_ref.key.object_id != object_id {
                break;
            }
            if let ObjectKey {
                data: ObjectKeyData::Attribute(extent_attribute_id, attribute_key),
                ..
            } = item_ref.key
            {
                if *extent_attribute_id != attribute_id {
                    result = TrimResult::Done(Some(*extent_attribute_id));
                    break;
                }
                if let (
                    AttributeKey::Extent(ExtentKey { range }),
                    ObjectValue::Extent(ExtentValue::Some { device_offset, .. }),
                ) = (attribute_key, item_ref.value)
                {
                    let start = std::cmp::max(range.start, aligned_offset);
                    ensure!(start < range.end, FxfsError::Inconsistent);
                    let device_offset = device_offset
                        .checked_add(start - range.start)
                        .ok_or(FxfsError::Inconsistent)?;
                    end = range.end;
                    let len = end - start;
                    let device_range = device_offset..device_offset + len;
                    ensure!(device_range.is_aligned(block_size), FxfsError::Inconsistent);
                    allocator.deallocate(transaction, self.store_object_id, device_range).await?;
                    deallocated += len;
                    // Stop if the transaction is getting too big.
                    if transaction.mutations().len() >= TRANSACTION_MUTATION_THRESHOLD {
                        result = TrimResult::Incomplete;
                        break;
                    }
                }
            }
            iter.advance().await?;
        }

        let finished_tombstone_object = matches!(mode, TrimMode::Tombstone(TombstoneMode::Object))
            && matches!(result, TrimResult::Done(None));
        let finished_tombstone_attribute =
            matches!(mode, TrimMode::Tombstone(TombstoneMode::Attribute))
                && !matches!(result, TrimResult::Incomplete);
        let mut object_mutation = None;
        let nodes = if finished_tombstone_object { -1 } else { 0 };
        if nodes != 0 || deallocated != 0 {
            let mutation = self.txn_get_object_mutation(transaction, object_id).await?;
            if let ObjectValue::Object { attributes: ObjectAttributes { project_id, .. }, .. } =
                mutation.item.value
            {
                if project_id != 0 {
                    transaction.add(
                        self.store_object_id,
                        Mutation::merge_object(
                            ObjectKey::project_usage(self.root_directory_object_id(), project_id),
                            ObjectValue::BytesAndNodes {
                                bytes: -i64::try_from(deallocated).unwrap(),
                                nodes,
                            },
                        ),
                    );
                }
                object_mutation = Some(mutation);
            } else {
                panic!("Inconsistent object type.");
            }
        }

        // Deletion marker records *must* be merged so as to consume all other records for the
        // object.
        if finished_tombstone_object {
            transaction.add(
                self.store_object_id,
                Mutation::merge_object(ObjectKey::object(object_id), ObjectValue::None),
            );
        } else {
            if finished_tombstone_attribute {
                transaction.add(
                    self.store_object_id,
                    Mutation::merge_object(
                        ObjectKey::attribute(object_id, attribute_id, AttributeKey::Attribute),
                        ObjectValue::None,
                    ),
                );
            }
            if deallocated > 0 {
                let mut mutation = match object_mutation {
                    Some(mutation) => mutation,
                    None => self.txn_get_object_mutation(transaction, object_id).await?,
                };
                transaction.add(
                    self.store_object_id,
                    Mutation::merge_object(
                        ObjectKey::extent(object_id, attribute_id, aligned_offset..end),
                        ObjectValue::deleted_extent(),
                    ),
                );
                // Update allocated size.
                if let ObjectValue::Object {
                    attributes: ObjectAttributes { allocated_size, .. },
                    ..
                } = &mut mutation.item.value
                {
                    // The only way for these to fail are if the volume is inconsistent.
                    *allocated_size = allocated_size.checked_sub(deallocated).ok_or_else(|| {
                        anyhow!(FxfsError::Inconsistent).context("Allocated size overflow")
                    })?;
                } else {
                    panic!("Unexpected object value");
                }
                transaction.add(self.store_object_id, Mutation::ObjectStore(mutation));
            }
        }
        Ok(result)
    }

    /// Returns all objects that exist in the parent store that pertain to this object store.
    /// Note that this doesn't include the object_id of the store itself which is generally
    /// referenced externally.
    pub fn parent_objects(&self) -> Vec<u64> {
        assert!(self.store_info_handle.get().is_some());
        self.store_info.lock().unwrap().as_ref().unwrap().parent_objects()
    }

    /// Returns root objects for this store.
    pub fn root_objects(&self) -> Vec<u64> {
        let mut objects = Vec::new();
        let store_info = self.store_info.lock().unwrap();
        let info = store_info.as_ref().unwrap();
        if info.root_directory_object_id != INVALID_OBJECT_ID {
            objects.push(info.root_directory_object_id);
        }
        if info.graveyard_directory_object_id != INVALID_OBJECT_ID {
            objects.push(info.graveyard_directory_object_id);
        }
        if info.internal_directory_object_id != INVALID_OBJECT_ID {
            objects.push(info.internal_directory_object_id);
        }
        objects
    }

    pub fn store_info(&self) -> Option<StoreInfo> {
        self.store_info.lock().unwrap().as_ref().cloned()
    }

    /// Returns None if called during journal replay.
    pub fn store_info_handle_object_id(&self) -> Option<u64> {
        self.store_info_handle.get().map(|h| h.object_id())
    }

    /// Called to open a store, before replay of this store's mutations.
    async fn open(
        parent_store: &Arc<ObjectStore>,
        store_object_id: u64,
        object_cache: Box<dyn ObjectCache<ObjectKey, ObjectValue>>,
    ) -> Result<Arc<ObjectStore>, Error> {
        let handle =
            ObjectStore::open_object(parent_store, store_object_id, HandleOptions::default(), None)
                .await?;

        let info = load_store_info(parent_store, store_object_id).await?;
        let is_encrypted = info.mutations_key.is_some();

        let mut total_layer_size = 0;
        let last_object_id;

        // TODO(https://fxbug.dev/42178043): the layer size here could be bad and cause overflow.

        // If the store is encrypted, we can't open the object tree layers now, but we need to
        // compute the size of the layers.
        if is_encrypted {
            for &oid in &info.layers {
                total_layer_size += parent_store.get_file_size(oid).await?;
            }
            if info.encrypted_mutations_object_id != INVALID_OBJECT_ID {
                total_layer_size += layer_size_from_encrypted_mutations_size(
                    parent_store.get_file_size(info.encrypted_mutations_object_id).await?,
                );
            }
            last_object_id = LastObjectId::default();
        } else {
            last_object_id = LastObjectId { id: info.last_object_id, cipher: None };
        }

        let fs = parent_store.filesystem();

        let store = ObjectStore::new(
            Some(parent_store.clone()),
            store_object_id,
            fs.clone(),
            if is_encrypted { None } else { Some(info) },
            object_cache,
            None,
            if is_encrypted { LockState::Locked } else { LockState::Unencrypted },
            last_object_id,
        );

        assert!(store.store_info_handle.set(handle).is_ok(), "Failed to set store_info_handle!");

        if !is_encrypted {
            let object_tree_layer_object_ids =
                store.store_info.lock().unwrap().as_ref().unwrap().layers.clone();
            let object_layers = store.open_layers(object_tree_layer_object_ids, None).await?;
            total_layer_size = object_layers.iter().map(|h| h.get_size()).sum();
            store
                .tree
                .append_layers(object_layers)
                .await
                .context("Failed to read object store layers")?;
        }

        fs.object_manager().update_reservation(
            store_object_id,
            tree::reservation_amount_from_layer_size(total_layer_size),
        );

        Ok(store)
    }

    async fn load_store_info(&self) -> Result<StoreInfo, Error> {
        load_store_info(self.parent_store.as_ref().unwrap(), self.store_object_id).await
    }

    async fn open_layers(
        &self,
        object_ids: impl std::iter::IntoIterator<Item = u64>,
        crypt: Option<Arc<dyn Crypt>>,
    ) -> Result<Vec<DataObjectHandle<ObjectStore>>, Error> {
        let parent_store = self.parent_store.as_ref().unwrap();
        let mut handles = Vec::new();
        let mut sizes = Vec::new();
        for object_id in object_ids {
            let handle = ObjectStore::open_object(
                &parent_store,
                object_id,
                HandleOptions::default(),
                crypt.clone(),
            )
            .await
            .with_context(|| format!("Failed to open layer file {}", object_id))?;
            sizes.push(handle.get_size());
            handles.push(handle);
        }
        self.counters.lock().unwrap().persistent_layer_file_sizes = sizes;
        Ok(handles)
    }

    /// Unlocks a store so that it is ready to be used.
    /// This is not thread-safe.
    pub async fn unlock(self: &Arc<Self>, crypt: Arc<dyn Crypt>) -> Result<(), Error> {
        self.unlock_inner(crypt, /*read_only=*/ false).await
    }

    /// Unlocks a store so that it is ready to be read from.
    /// The store will generally behave like it is still locked: when flushed, the store will
    /// write out its mutations into the encrypted mutations file, rather than directly updating
    /// the layer files of the object store.
    /// Re-locking the store (which *must* be done with `Self::lock_read_only` will not trigger a
    /// flush, although the store might still be flushed during other operations.
    /// This is not thread-safe.
    pub async fn unlock_read_only(self: &Arc<Self>, crypt: Arc<dyn Crypt>) -> Result<(), Error> {
        self.unlock_inner(crypt, /*read_only=*/ true).await
    }

    async fn unlock_inner(
        self: &Arc<Self>,
        crypt: Arc<dyn Crypt>,
        read_only: bool,
    ) -> Result<(), Error> {
        match &*self.lock_state.lock().unwrap() {
            LockState::Locked => {}
            LockState::Unencrypted => bail!(FxfsError::InvalidArgs),
            LockState::Invalid => bail!(FxfsError::Internal),
            LockState::Unlocked(_) | LockState::UnlockedReadOnly(..) => {
                bail!(FxfsError::AlreadyBound)
            }
            LockState::Unknown => panic!("Store was unlocked before replay"),
            LockState::Locking => panic!("Store is being locked"),
            LockState::Unlocking => panic!("Store is being unlocked"),
        }
        // We must lock flushing since that can modify store_info and the encrypted mutations file.
        let keys = lock_keys![LockKey::flush(self.store_object_id())];
        let fs = self.filesystem();
        let guard = fs.lock_manager().write_lock(keys).await;

        let store_info = self.load_store_info().await?;

        self.tree
            .append_layers(
                self.open_layers(store_info.layers.iter().cloned(), Some(crypt.clone())).await?,
            )
            .await
            .context("Failed to read object tree layer file contents")?;

        let unwrapped_key = crypt
            .unwrap_key(store_info.mutations_key.as_ref().unwrap(), self.store_object_id)
            .await
            .context("Failed to unwrap mutations keys")?;
        // The ChaCha20 stream cipher we use supports up to 64 GiB.  By default we'll roll the key
        // after every 128 MiB.  Here we just need to pick a number that won't cause issues if it
        // wraps, so we just use u32::MAX (the offset is u64).
        ensure!(store_info.mutations_cipher_offset <= u32::MAX as u64, FxfsError::Inconsistent);
        let mut mutations_cipher =
            StreamCipher::new(&unwrapped_key, store_info.mutations_cipher_offset);

        let wrapped_key = store_info.object_id_key.as_ref().ok_or(FxfsError::Inconsistent)?;
        let object_id_cipher =
            Ff1::new(&crypt.unwrap_key(wrapped_key, self.store_object_id).await?);
        {
            let mut last_object_id = self.last_object_id.lock().unwrap();
            last_object_id.cipher = Some(object_id_cipher);
        }
        self.update_last_object_id(store_info.last_object_id);

        // Apply the encrypted mutations.
        let mut mutations = {
            if store_info.encrypted_mutations_object_id == INVALID_OBJECT_ID {
                EncryptedMutations::default()
            } else {
                let parent_store = self.parent_store.as_ref().unwrap();
                let handle = ObjectStore::open_object(
                    &parent_store,
                    store_info.encrypted_mutations_object_id,
                    HandleOptions::default(),
                    None,
                )
                .await?;
                let mut cursor = std::io::Cursor::new(
                    handle
                        .contents(MAX_ENCRYPTED_MUTATIONS_SIZE)
                        .await
                        .context(FxfsError::Inconsistent)?,
                );
                let mut mutations = EncryptedMutations::deserialize_with_version(&mut cursor)
                    .context("Failed to deserialize EncryptedMutations")?
                    .0;
                let len = cursor.get_ref().len() as u64;
                while cursor.position() < len {
                    mutations.extend(
                        &EncryptedMutations::deserialize_with_version(&mut cursor)
                            .context("Failed to deserialize EncryptedMutations")?
                            .0,
                    );
                }
                mutations
            }
        };

        // This assumes that the journal has no buffered mutations for this store (see Self::lock).
        let journaled = EncryptedMutations::from_replayed_mutations(
            self.store_object_id,
            fs.journal()
                .read_transactions_for_object(self.store_object_id)
                .await
                .context("Failed to read encrypted mutations from journal")?,
        );
        mutations.extend(&journaled);

        let _ = std::mem::replace(&mut *self.lock_state.lock().unwrap(), LockState::Unlocking);
        *self.store_info.lock().unwrap() = Some(store_info);

        // If we fail, clean up.
        let clean_up = scopeguard::guard((), |_| {
            *self.lock_state.lock().unwrap() = LockState::Locked;
            *self.store_info.lock().unwrap() = None;
            // Make sure we don't leave unencrypted data lying around in memory.
            self.tree.reset();
        });

        let EncryptedMutations { transactions, mut data, mutations_key_roll } = mutations;

        let mut slice = &mut data[..];
        let mut last_offset = 0;
        for (offset, key) in mutations_key_roll {
            let split_offset = offset
                .checked_sub(last_offset)
                .ok_or(FxfsError::Inconsistent)
                .context("Invalid mutation key roll offset")?;
            last_offset = offset;
            ensure!(split_offset <= slice.len(), FxfsError::Inconsistent);
            let (old, new) = slice.split_at_mut(split_offset);
            mutations_cipher.decrypt(old);
            let unwrapped_key = crypt
                .unwrap_key(&key, self.store_object_id)
                .await
                .context("Failed to unwrap mutations keys")?;
            mutations_cipher = StreamCipher::new(&unwrapped_key, 0);
            slice = new;
        }
        mutations_cipher.decrypt(slice);

        // Always roll the mutations key when we unlock which guarantees we won't reuse a
        // previous key and nonce.
        self.roll_mutations_key(crypt.as_ref()).await?;

        let mut cursor = std::io::Cursor::new(data);
        for (checkpoint, count) in transactions {
            let context = ApplyContext { mode: ApplyMode::Replay, checkpoint };
            for _ in 0..count {
                let mutation =
                    Mutation::deserialize_from_version(&mut cursor, context.checkpoint.version)
                        .context("failed to deserialize encrypted mutation")?;
                self.apply_mutation(mutation, &context, AssocObj::None)
                    .context("failed to apply encrypted mutation")?;
            }
        }

        *self.lock_state.lock().unwrap() =
            if read_only { LockState::UnlockedReadOnly(crypt) } else { LockState::Unlocked(crypt) };

        // To avoid unbounded memory growth, we should flush the encrypted mutations now. Otherwise
        // it's possible for more writes to be queued and for the store to be locked before we can
        // flush anything and that can repeat.
        std::mem::drop(guard);

        if !read_only && !self.filesystem().options().read_only {
            self.flush_with_reason(flush::Reason::Unlock).await?;

            // Reap purged files within this store.
            let _ = self.filesystem().graveyard().initial_reap(&self).await?;
        }

        // Return and cancel the clean up.
        Ok(ScopeGuard::into_inner(clean_up))
    }

    pub fn is_locked(&self) -> bool {
        matches!(
            *self.lock_state.lock().unwrap(),
            LockState::Locked | LockState::Locking | LockState::Unknown
        )
    }

    pub fn is_unknown(&self) -> bool {
        matches!(*self.lock_state.lock().unwrap(), LockState::Unknown)
    }

    pub fn is_encrypted(&self) -> bool {
        self.store_info.lock().unwrap().as_ref().unwrap().mutations_key.is_some()
    }

    // Locks a store.  This assumes no other concurrent access to the store (other than flushing
    // which is guarded for here).  Whilst this can return an error, the store will be placed into
    // an unusable but safe state (i.e. no lingering unencrypted data) if an error is encountered.
    pub async fn lock(&self) -> Result<(), Error> {
        // We must lock flushing since it is not safe for that to be happening whilst we are locking
        // the store.
        let keys = lock_keys![LockKey::flush(self.store_object_id())];
        let fs = self.filesystem();
        let _guard = fs.lock_manager().write_lock(keys).await;

        {
            let mut lock_state = self.lock_state.lock().unwrap();
            if let LockState::Unlocked(_) = &*lock_state {
                *lock_state = LockState::Locking;
            } else {
                panic!("Unexpected lock state: {:?}", &*lock_state);
            }
        }

        // Sync the journal now to ensure that any buffered mutations for this store make it out to
        // disk.  This is necessary to be able to unlock the store again.
        // We need to establish a barrier at this point (so that the journaled writes are observable
        // by any future attempts to unlock the store), hence the flush_device.
        let sync_result =
            self.filesystem().sync(SyncOptions { flush_device: true, ..Default::default() }).await;

        *self.lock_state.lock().unwrap() = if let Err(error) = &sync_result {
            error!(error:?; "Failed to sync journal; store will no longer be usable");
            LockState::Invalid
        } else {
            LockState::Locked
        };
        self.key_manager.clear();
        *self.store_info.lock().unwrap() = None;
        self.tree.reset();

        sync_result
    }

    // Locks a store which was previously unlocked read-only (see `Self::unlock_read_only`).  Data
    // is not flushed, and instead any journaled mutations are buffered back into the ObjectStore
    // and will be replayed next time the store is unlocked.
    pub fn lock_read_only(&self) {
        *self.lock_state.lock().unwrap() = LockState::Locked;
        *self.store_info.lock().unwrap() = None;
        self.tree.reset();
    }

    // Returns INVALID_OBJECT_ID if the object ID cipher needs to be created or rolled.
    fn maybe_get_next_object_id(&self) -> u64 {
        let mut last_object_id = self.last_object_id.lock().unwrap();
        if last_object_id.should_create_cipher() {
            INVALID_OBJECT_ID
        } else {
            last_object_id.get_next_object_id()
        }
    }

    // Returns a new object ID that can be used.  This will create an object ID cipher if necessary.
    // We require a transaction to guarantee that a guard is held because if we create a transaction
    // to roll the object ID key, we skip taking the filesystem lock.
    pub async fn get_next_object_id(&self, transaction: &Transaction<'_>) -> Result<u64, Error> {
        let object_id = self.maybe_get_next_object_id();
        if object_id != INVALID_OBJECT_ID {
            return Ok(object_id);
        }

        // Create a transaction (which has a lock) and then check again.
        let mut transaction = self
            .filesystem()
            .new_transaction(
                lock_keys![LockKey::object(
                    self.parent_store.as_ref().unwrap().store_object_id,
                    self.store_object_id,
                )],
                Options {
                    // We must skip journal checks because this transaction might be needed to
                    // compact.
                    skip_journal_checks: true,
                    borrow_metadata_space: true,
                    txn_guard: Some(transaction.txn_guard()),
                    ..Default::default()
                },
            )
            .await?;

        {
            let mut last_object_id = self.last_object_id.lock().unwrap();
            if !last_object_id.should_create_cipher() {
                // We lost a race.
                return Ok(last_object_id.get_next_object_id());
            }
            // It shouldn't be possible for last_object_id to wrap within our lifetime, so if this
            // happens, it's most likely due to corruption.
            ensure!(
                last_object_id.id & OBJECT_ID_HI_MASK != OBJECT_ID_HI_MASK,
                FxfsError::Inconsistent
            );
        }

        // Create a key.
        let (object_id_wrapped, object_id_unwrapped) =
            self.crypt().unwrap().create_key(self.store_object_id, KeyPurpose::Metadata).await?;

        // Update StoreInfo.
        let buf = {
            let serialized_info = {
                let mut store_info = self.store_info.lock().unwrap();
                let store_info = store_info.as_mut().unwrap();
                store_info.object_id_key = Some(object_id_wrapped);
                let mut serialized_info = Vec::new();
                store_info.serialize_with_version(&mut serialized_info)?;
                serialized_info
            };
            let mut buf = self.device.allocate_buffer(serialized_info.len()).await;
            buf.as_mut_slice().copy_from_slice(&serialized_info[..]);
            buf
        };

        self.store_info_handle
            .get()
            .unwrap()
            .txn_write(&mut transaction, 0u64, buf.as_ref())
            .await?;
        transaction.commit().await?;

        let mut last_object_id = self.last_object_id.lock().unwrap();
        last_object_id.cipher = Some(Ff1::new(&object_id_unwrapped));
        last_object_id.id = (last_object_id.id + (1 << 32)) & OBJECT_ID_HI_MASK;

        Ok((last_object_id.id & OBJECT_ID_HI_MASK)
            | last_object_id.cipher.as_ref().unwrap().encrypt(last_object_id.id as u32) as u64)
    }

    fn allocator(&self) -> Arc<Allocator> {
        self.filesystem().allocator()
    }

    // If |transaction| has an impending mutation for the underlying object, returns that.
    // Otherwise, looks up the object from the tree and returns a suitable mutation for it.  The
    // mutation is returned here rather than the item because the mutation includes the operation
    // which has significance: inserting an object implies it's the first of its kind unlike
    // replacing an object.
    async fn txn_get_object_mutation(
        &self,
        transaction: &Transaction<'_>,
        object_id: u64,
    ) -> Result<ObjectStoreMutation, Error> {
        if let Some(mutation) =
            transaction.get_object_mutation(self.store_object_id, ObjectKey::object(object_id))
        {
            Ok(mutation.clone())
        } else {
            Ok(ObjectStoreMutation {
                item: self
                    .tree
                    .find(&ObjectKey::object(object_id))
                    .await?
                    .ok_or(FxfsError::Inconsistent)
                    .context("Object id missing")?,
                op: Operation::ReplaceOrInsert,
            })
        }
    }

    fn update_last_object_id(&self, mut object_id: u64) {
        let mut last_object_id = self.last_object_id.lock().unwrap();
        // For encrypted stores, object_id will be encrypted here, so we must decrypt first.
        if let Some(cipher) = &last_object_id.cipher {
            // If the object ID cipher has been rolled, then it's possible we might see object IDs
            // that were generated using a different cipher so the decrypt here will return the
            // wrong value, but that won't matter because the hi part of the object ID should still
            // discriminate.
            object_id = object_id & OBJECT_ID_HI_MASK | cipher.decrypt(object_id as u32) as u64;
        }
        if object_id > last_object_id.id {
            last_object_id.id = object_id;
        }
    }

    /// Adds the specified object to the graveyard.
    pub fn add_to_graveyard(&self, transaction: &mut Transaction<'_>, object_id: u64) {
        let graveyard_id = self.graveyard_directory_object_id();
        assert_ne!(graveyard_id, INVALID_OBJECT_ID);
        transaction.add(
            self.store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::graveyard_entry(graveyard_id, object_id),
                ObjectValue::Some,
            ),
        );
    }

    /// Removes the specified object from the graveyard.  NB: Care should be taken when calling
    /// this because graveyard entries are used for purging deleted files *and* for trimming
    /// extents.  For example, consider the following sequence:
    ///
    ///     1. Add Trim graveyard entry.
    ///     2. Replace with Some graveyard entry (see above).
    ///     3. Remove graveyard entry.
    ///
    /// If the desire in #3 is just to cancel the effect of the Some entry, then #3 should
    /// actually be:
    ///
    ///     3. Replace with Trim graveyard entry.
    pub fn remove_from_graveyard(&self, transaction: &mut Transaction<'_>, object_id: u64) {
        transaction.add(
            self.store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::graveyard_entry(self.graveyard_directory_object_id(), object_id),
                ObjectValue::None,
            ),
        );
    }

    /// Removes the specified attribute from the graveyard. Unlike object graveyard entries,
    /// attribute graveyard entries only have one functionality (i.e. to purge deleted attributes)
    /// so the caller does not need to be concerned about replacing the graveyard attribute entry
    /// with its prior state when cancelling it. See comment on `remove_from_graveyard()`.
    pub fn remove_attribute_from_graveyard(
        &self,
        transaction: &mut Transaction<'_>,
        object_id: u64,
        attribute_id: u64,
    ) {
        transaction.add(
            self.store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::graveyard_attribute_entry(
                    self.graveyard_directory_object_id(),
                    object_id,
                    attribute_id,
                ),
                ObjectValue::None,
            ),
        );
    }

    // Roll the mutations key.  The new key will be written for the next encrypted mutation.
    async fn roll_mutations_key(&self, crypt: &dyn Crypt) -> Result<(), Error> {
        let (wrapped_key, unwrapped_key) =
            crypt.create_key(self.store_object_id, KeyPurpose::Metadata).await?;

        // The mutations_cipher lock must be held for the duration so that mutations_cipher and
        // store_info are updated atomically.  Otherwise, write_mutation could find a new cipher but
        // end up writing the wrong wrapped key.
        let mut cipher = self.mutations_cipher.lock().unwrap();
        *cipher = Some(StreamCipher::new(&unwrapped_key, 0));
        self.store_info.lock().unwrap().as_mut().unwrap().mutations_key = Some(wrapped_key);
        // mutations_cipher_offset is updated by flush.
        Ok(())
    }

    /// Returns the link of a symlink object.
    pub async fn read_symlink(&self, object_id: u64) -> Result<Vec<u8>, Error> {
        match self.tree.find(&ObjectKey::object(object_id)).await? {
            None => bail!(FxfsError::NotFound),
            Some(Item {
                value: ObjectValue::Object { kind: ObjectKind::Symlink { link, .. }, .. },
                ..
            }) => Ok(link),
            Some(item) => Err(anyhow!(FxfsError::Inconsistent)
                .context(format!("Unexpected item in lookup: {item:?}"))),
        }
    }

    /// Retrieves the wrapped keys for the given object.  The keys *should* be known to exist and it
    /// will be considered an inconsistency if they don't.
    pub async fn get_keys(&self, object_id: u64) -> Result<WrappedKeys, Error> {
        match self.tree.find(&ObjectKey::keys(object_id)).await?.ok_or(FxfsError::Inconsistent)? {
            Item { value: ObjectValue::Keys(EncryptionKeys::AES256XTS(keys)), .. } => Ok(keys),
            _ => Err(anyhow!(FxfsError::Inconsistent).context("open_object: Expected keys")),
        }
    }

    pub async fn update_attributes<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        object_id: u64,
        node_attributes: Option<&fio::MutableNodeAttributes>,
        change_time: Option<Timestamp>,
    ) -> Result<(), Error> {
        if change_time.is_none() {
            if let Some(attributes) = node_attributes {
                let empty_attributes = fio::MutableNodeAttributes { ..Default::default() };
                if *attributes == empty_attributes {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }
        let mut mutation = self.txn_get_object_mutation(transaction, object_id).await?;
        if let ObjectValue::Object { ref mut attributes, .. } = mutation.item.value {
            if let Some(time) = change_time {
                attributes.change_time = time;
            }
            if let Some(node_attributes) = node_attributes {
                if let Some(time) = node_attributes.creation_time {
                    attributes.creation_time = Timestamp::from_nanos(time);
                }
                if let Some(time) = node_attributes.modification_time {
                    attributes.modification_time = Timestamp::from_nanos(time);
                }
                if let Some(time) = node_attributes.access_time {
                    attributes.access_time = Timestamp::from_nanos(time);
                }
                if node_attributes.mode.is_some()
                    || node_attributes.uid.is_some()
                    || node_attributes.gid.is_some()
                    || node_attributes.rdev.is_some()
                {
                    if let Some(a) = &mut attributes.posix_attributes {
                        if let Some(mode) = node_attributes.mode {
                            a.mode = mode;
                        }
                        if let Some(uid) = node_attributes.uid {
                            a.uid = uid;
                        }
                        if let Some(gid) = node_attributes.gid {
                            a.gid = gid;
                        }
                        if let Some(rdev) = node_attributes.rdev {
                            a.rdev = rdev;
                        }
                    } else {
                        attributes.posix_attributes = Some(PosixAttributes {
                            mode: node_attributes.mode.unwrap_or_default(),
                            uid: node_attributes.uid.unwrap_or_default(),
                            gid: node_attributes.gid.unwrap_or_default(),
                            rdev: node_attributes.rdev.unwrap_or_default(),
                        });
                    }
                }
            }
        } else {
            bail!(anyhow!(FxfsError::Inconsistent)
                .context("ObjectStore.update_attributes: Expected object value"));
        };
        transaction.add(self.store_object_id(), Mutation::ObjectStore(mutation));
        Ok(())
    }
}

#[async_trait]
impl JournalingObject for ObjectStore {
    fn apply_mutation(
        &self,
        mutation: Mutation,
        context: &ApplyContext<'_, '_>,
        _assoc_obj: AssocObj<'_>,
    ) -> Result<(), Error> {
        match &*self.lock_state.lock().unwrap() {
            LockState::Locked | LockState::Locking => {
                ensure!(
                    matches!(mutation, Mutation::BeginFlush | Mutation::EndFlush)
                        || matches!(
                            mutation,
                            Mutation::EncryptedObjectStore(_) | Mutation::UpdateMutationsKey(_)
                                if context.mode.is_replay()
                        ),
                    anyhow!(FxfsError::Inconsistent)
                        .context(format!("Unexpected mutation for encrypted store: {mutation:?}"))
                );
            }
            LockState::Invalid
            | LockState::Unlocking
            | LockState::Unencrypted
            | LockState::Unlocked(_)
            | LockState::UnlockedReadOnly(..) => {}
            lock_state @ _ => panic!("Unexpected lock state: {lock_state:?}"),
        }
        match mutation {
            Mutation::ObjectStore(ObjectStoreMutation { mut item, op }) => {
                item.sequence = context.checkpoint.file_offset;
                match op {
                    Operation::Insert => {
                        // If we are inserting an object record for the first time, it signifies the
                        // birth of the object so we need to adjust the object count.
                        if matches!(item.value, ObjectValue::Object { .. }) {
                            {
                                let info = &mut self.store_info.lock().unwrap();
                                let object_count = &mut info.as_mut().unwrap().object_count;
                                *object_count = object_count.saturating_add(1);
                            }
                            if context.mode.is_replay() {
                                self.update_last_object_id(item.key.object_id);
                            }
                        }
                        self.tree.insert(item)?;
                    }
                    Operation::ReplaceOrInsert => {
                        self.tree.replace_or_insert(item);
                    }
                    Operation::Merge => {
                        if item.is_tombstone() {
                            let info = &mut self.store_info.lock().unwrap();
                            let object_count = &mut info.as_mut().unwrap().object_count;
                            *object_count = object_count.saturating_sub(1);
                        }
                        let lower_bound = item.key.key_for_merge_into();
                        self.tree.merge_into(item, &lower_bound);
                    }
                }
            }
            Mutation::BeginFlush => {
                ensure!(self.parent_store.is_some(), FxfsError::Inconsistent);
                self.tree.seal();
            }
            Mutation::EndFlush => ensure!(self.parent_store.is_some(), FxfsError::Inconsistent),
            Mutation::EncryptedObjectStore(_) | Mutation::UpdateMutationsKey(_) => {
                // We will process these during Self::unlock.
                ensure!(
                    !matches!(&*self.lock_state.lock().unwrap(), LockState::Unencrypted),
                    FxfsError::Inconsistent
                );
            }
            Mutation::CreateInternalDir(object_id) => {
                ensure!(object_id != INVALID_OBJECT_ID, FxfsError::Inconsistent);
                self.store_info.lock().unwrap().as_mut().unwrap().internal_directory_object_id =
                    object_id;
            }
            _ => bail!("unexpected mutation: {:?}", mutation),
        }
        self.counters.lock().unwrap().mutations_applied += 1;
        Ok(())
    }

    fn drop_mutation(&self, _mutation: Mutation, _transaction: &Transaction<'_>) {
        self.counters.lock().unwrap().mutations_dropped += 1;
    }

    /// Push all in-memory structures to the device. This is not necessary for sync since the
    /// journal will take care of it.  This is supposed to be called when there is either memory or
    /// space pressure (flushing the store will persist in-memory data and allow the journal file to
    /// be trimmed).
    ///
    /// Also returns the earliest version of a struct in the filesystem (when known).
    async fn flush(&self) -> Result<Version, Error> {
        return self.flush_with_reason(flush::Reason::Journal).await;
    }

    fn write_mutation(&self, mutation: &Mutation, mut writer: journal::Writer<'_>) {
        // Intentionally enumerating all variants to force a decision on any new variants. Encrypt
        // all mutations that could affect an encrypted object store contents or the `StoreInfo` of
        // the encrypted object store. During `unlock()` any mutations which haven't been encrypted
        // won't be replayed after reading `StoreInfo`.
        match mutation {
            // Whilst CreateInternalDir is a mutation for `StoreInfo`, which isn't encrypted, we
            // still choose to encrypt the mutation because it makes it easier to deal with replay.
            // When we replay mutations for an encrypted store, the only thing we keep in memory are
            // the encrypted mutations; we don't keep `StoreInfo` or changes to it in memory. So, by
            // encrypting the CreateInternalDir mutation here, it means we don't have to track both
            // encrypted mutations bound for the LSM tree and unencrypted mutations for `StoreInfo`
            // to use in `unlock()`. It'll just bundle CreateInternalDir mutations with the other
            // encrypted mutations and handled them all in sequence during `unlock()`.
            Mutation::ObjectStore(_) | Mutation::CreateInternalDir(_) => {
                let mut cipher = self.mutations_cipher.lock().unwrap();
                if let Some(cipher) = cipher.as_mut() {
                    // If this is the first time we've used this key, we must write the key out.
                    if cipher.offset() == 0 {
                        writer.write(Mutation::update_mutations_key(
                            self.store_info
                                .lock()
                                .unwrap()
                                .as_ref()
                                .unwrap()
                                .mutations_key
                                .as_ref()
                                .unwrap()
                                .clone(),
                        ));
                    }
                    let mut buffer = Vec::new();
                    mutation.serialize_into(&mut buffer).unwrap();
                    cipher.encrypt(&mut buffer);
                    writer.write(Mutation::EncryptedObjectStore(buffer.into()));
                    return;
                }
            }
            // `EncryptedObjectStore` and `UpdateMutationsKey` are both obviously associated with
            // encrypted object stores, but are either the encrypted mutation data itself or
            // metadata governing how the data will be encrypted. They should only be produced here.
            Mutation::EncryptedObjectStore(_) | Mutation::UpdateMutationsKey(_) => {
                debug_assert!(false, "Only this method should generate encrypted mutations");
            }
            // `BeginFlush` and `EndFlush` are not needed during `unlock()` and are needed during
            // the initial journal replay, so should not be encrypted. `Allocator`, `DeleteVolume`,
            // `UpdateBorrowed` mutations are never associated with an encrypted store as we do not
            // encrypt the allocator or root/root-parent stores so we can avoid the locking.
            Mutation::Allocator(_)
            | Mutation::BeginFlush
            | Mutation::EndFlush
            | Mutation::DeleteVolume
            | Mutation::UpdateBorrowed(_) => {}
        }
        writer.write(mutation.clone());
    }
}

impl HandleOwner for ObjectStore {}

impl AsRef<ObjectStore> for ObjectStore {
    fn as_ref(&self) -> &ObjectStore {
        self
    }
}

fn layer_size_from_encrypted_mutations_size(size: u64) -> u64 {
    // This is similar to reserved_space_from_journal_usage. It needs to be a worst case estimate of
    // the amount of metadata space that might need to be reserved to allow the encrypted mutations
    // to be written to layer files.  It needs to be >= than reservation_amount_from_layer_size will
    // return once the data has been written to layer files and <= than
    // reserved_space_from_journal_usage would use.  We can't just use
    // reserved_space_from_journal_usage because the encrypted mutations file includes some extra
    // data (it includes the checkpoints) that isn't written in the same way to the journal.
    size * 3
}

impl AssociatedObject for ObjectStore {}

/// Argument to the trim_some method.
#[derive(Debug)]
pub enum TrimMode {
    /// Trim extents beyond the current size.
    UseSize,

    /// Trim extents beyond the supplied offset.
    FromOffset(u64),

    /// Remove the object (or attribute) from the store once it is fully trimmed.
    Tombstone(TombstoneMode),
}

/// Sets the mode for tombstoning (either at the object or attribute level).
#[derive(Debug)]
pub enum TombstoneMode {
    Object,
    Attribute,
}

/// Result of the trim_some method.
#[derive(Debug)]
pub enum TrimResult {
    /// We reached the limit of the transaction and more extents might follow.
    Incomplete,

    /// We finished this attribute.  Returns the ID of the next attribute for the same object if
    /// there is one.
    Done(Option<u64>),
}

/// Loads store info.
pub async fn load_store_info(
    parent: &Arc<ObjectStore>,
    store_object_id: u64,
) -> Result<StoreInfo, Error> {
    let handle =
        ObjectStore::open_object(parent, store_object_id, HandleOptions::default(), None).await?;

    Ok(if handle.get_size() > 0 {
        let serialized_info = handle.contents(MAX_STORE_INFO_SERIALIZED_SIZE).await?;
        let mut cursor = std::io::Cursor::new(serialized_info);
        let (store_info, _) = StoreInfo::deserialize_with_version(&mut cursor)
            .context("Failed to deserialize StoreInfo")?;
        store_info
    } else {
        // The store_info will be absent for a newly created and empty object store.
        StoreInfo::default()
    })
}

#[cfg(test)]
mod tests {
    use super::{
        StoreInfo, DEFAULT_DATA_ATTRIBUTE_ID, FSVERITY_MERKLE_ATTRIBUTE_ID,
        MAX_STORE_INFO_SERIALIZED_SIZE, OBJECT_ID_HI_MASK,
    };
    use crate::errors::FxfsError;
    use crate::filesystem::{FxFilesystem, JournalingObject, OpenFxFilesystem, SyncOptions};
    use crate::fsck::fsck;
    use crate::lsm_tree::types::{ItemRef, LayerIterator};
    use crate::lsm_tree::Query;
    use crate::object_handle::{
        ObjectHandle, ReadObjectHandle, WriteObjectHandle, INVALID_OBJECT_ID,
    };
    use crate::object_store::directory::Directory;
    use crate::object_store::object_record::{AttributeKey, ObjectKey, ObjectKind, ObjectValue};
    use crate::object_store::transaction::{lock_keys, Options};
    use crate::object_store::volume::root_volume;
    use crate::object_store::{
        FsverityMetadata, HandleOptions, LockKey, Mutation, ObjectStore, RootDigest,
    };
    use crate::serialized_types::VersionedLatest;
    use assert_matches::assert_matches;
    use fuchsia_async as fasync;
    use futures::join;
    use fxfs_crypto::{Crypt, WrappedKey, WrappedKeyBytes, WRAPPED_KEY_SIZE};
    use fxfs_insecure_crypto::InsecureCrypt;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use storage_device::fake_device::FakeDevice;
    use storage_device::DeviceHolder;

    const TEST_DEVICE_BLOCK_SIZE: u32 = 512;

    async fn test_filesystem() -> OpenFxFilesystem {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        FxFilesystem::new_empty(device).await.expect("new_empty failed")
    }

    #[fuchsia::test]
    async fn test_item_sequences() {
        let fs = test_filesystem().await;
        let object1;
        let object2;
        let object3;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let store = fs.root_store();
        object1 = Arc::new(
            ObjectStore::create_object(
                &store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed"),
        );
        transaction.commit().await.expect("commit failed");
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        object2 = Arc::new(
            ObjectStore::create_object(
                &store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed"),
        );
        transaction.commit().await.expect("commit failed");

        fs.sync(SyncOptions::default()).await.expect("sync failed");

        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        object3 = Arc::new(
            ObjectStore::create_object(
                &store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed"),
        );
        transaction.commit().await.expect("commit failed");

        let layer_set = store.tree.layer_set();
        let mut merger = layer_set.merger();
        let mut iter = merger.query(Query::FullScan).await.expect("seek failed");
        let mut sequences = [0u64; 3];
        while let Some(ItemRef { key: ObjectKey { object_id, .. }, sequence, .. }) = iter.get() {
            if *object_id == object1.object_id() {
                sequences[0] = sequence;
            } else if *object_id == object2.object_id() {
                sequences[1] = sequence;
            } else if *object_id == object3.object_id() {
                sequences[2] = sequence;
            }
            iter.advance().await.expect("advance failed");
        }

        assert!(sequences[0] <= sequences[1], "sequences: {:?}", sequences);
        // The last item came after a sync, so should be strictly greater.
        assert!(sequences[1] < sequences[2], "sequences: {:?}", sequences);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_verified_file_with_verified_attribute() {
        let fs: OpenFxFilesystem = test_filesystem().await;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let store = fs.root_store();
        let object = Arc::new(
            ObjectStore::create_object(
                &store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed"),
        );

        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(
                    object.object_id(),
                    DEFAULT_DATA_ATTRIBUTE_ID,
                    AttributeKey::Attribute,
                ),
                ObjectValue::verified_attribute(
                    0,
                    FsverityMetadata { root_digest: RootDigest::Sha256([0; 32]), salt: vec![] },
                ),
            ),
        );

        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(
                    object.object_id(),
                    FSVERITY_MERKLE_ATTRIBUTE_ID,
                    AttributeKey::Attribute,
                ),
                ObjectValue::attribute(0, false),
            ),
        );

        transaction.commit().await.unwrap();

        let handle =
            ObjectStore::open_object(&store, object.object_id(), HandleOptions::default(), None)
                .await
                .expect("open_object failed");

        assert!(handle.is_verified_file());

        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_verified_file_without_verified_attribute() {
        let fs: OpenFxFilesystem = test_filesystem().await;
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let store = fs.root_store();
        let object = Arc::new(
            ObjectStore::create_object(
                &store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed"),
        );

        transaction.commit().await.unwrap();

        let handle =
            ObjectStore::open_object(&store, object.object_id(), HandleOptions::default(), None)
                .await
                .expect("open_object failed");

        assert!(!handle.is_verified_file());

        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_create_and_open_store() {
        let fs = test_filesystem().await;
        let store_id = {
            let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
            root_volume
                .new_volume("test", Some(Arc::new(InsecureCrypt::new())))
                .await
                .expect("new_volume failed")
                .store_object_id()
        };

        fs.close().await.expect("close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");

        {
            let store = fs.object_manager().store(store_id).expect("store not found");
            store.unlock(Arc::new(InsecureCrypt::new())).await.expect("unlock failed");
        }
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_create_and_open_internal_dir() {
        let fs = test_filesystem().await;
        let dir_id;
        let store_id;
        {
            let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
            let store = root_volume
                .new_volume("test", Some(Arc::new(InsecureCrypt::new())))
                .await
                .expect("new_volume failed");
            dir_id =
                store.get_or_create_internal_directory_id().await.expect("Create internal dir");
            store_id = store.store_object_id();
        }

        fs.close().await.expect("close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");

        {
            let store = fs.object_manager().store(store_id).expect("store not found");
            store.unlock(Arc::new(InsecureCrypt::new())).await.expect("unlock failed");
            assert_eq!(
                dir_id,
                store.get_or_create_internal_directory_id().await.expect("Retrieving dir")
            );
            let obj = store
                .tree()
                .find(&ObjectKey::object(dir_id))
                .await
                .expect("Searching tree for dir")
                .unwrap();
            assert_matches!(
                obj.value,
                ObjectValue::Object { kind: ObjectKind::Directory { .. }, .. }
            );
        }
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_create_and_open_internal_dir_unencrypted() {
        let fs = test_filesystem().await;
        let dir_id;
        let store_id;
        {
            let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
            let store = root_volume.new_volume("test", None).await.expect("new_volume failed");
            dir_id =
                store.get_or_create_internal_directory_id().await.expect("Create internal dir");
            store_id = store.store_object_id();
        }

        fs.close().await.expect("close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");

        {
            let store = fs.object_manager().store(store_id).expect("store not found");
            assert_eq!(
                dir_id,
                store.get_or_create_internal_directory_id().await.expect("Retrieving dir")
            );
            let obj = store
                .tree()
                .find(&ObjectKey::object(dir_id))
                .await
                .expect("Searching tree for dir")
                .unwrap();
            assert_matches!(
                obj.value,
                ObjectValue::Object { kind: ObjectKind::Directory { .. }, .. }
            );
        }
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_old_layers_are_purged() {
        let fs = test_filesystem().await;

        let store = fs.root_store();
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let object = Arc::new(
            ObjectStore::create_object(
                &store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed"),
        );
        transaction.commit().await.expect("commit failed");

        store.flush().await.expect("flush failed");

        let mut buf = object.allocate_buffer(5).await;
        buf.as_mut_slice().copy_from_slice(b"hello");
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");

        // Getting the layer-set should cause the flush to stall.
        let layer_set = store.tree().layer_set();

        let done = Mutex::new(false);
        let mut object_id = 0;

        join!(
            async {
                store.flush().await.expect("flush failed");
                assert!(*done.lock().unwrap());
            },
            async {
                // This is a halting problem so all we can do is sleep.
                fasync::Timer::new(Duration::from_secs(1)).await;
                *done.lock().unwrap() = true;
                object_id = layer_set.layers.last().unwrap().handle().unwrap().object_id();
                std::mem::drop(layer_set);
            }
        );

        if let Err(e) = ObjectStore::open_object(
            &store.parent_store.as_ref().unwrap(),
            object_id,
            HandleOptions::default(),
            store.crypt(),
        )
        .await
        {
            assert!(FxfsError::NotFound.matches(&e));
        } else {
            panic!("open_object succeeded");
        }
    }

    #[fuchsia::test]
    async fn test_tombstone_deletes_data() {
        let fs = test_filesystem().await;
        let root_store = fs.root_store();
        let child_id = {
            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new_transaction failed");
            let child = ObjectStore::create_object(
                &root_store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed");
            transaction.commit().await.expect("commit failed");

            // Allocate an extent in the file.
            let mut buffer = child.allocate_buffer(8192).await;
            buffer.as_mut_slice().fill(0xaa);
            child.write_or_append(Some(0), buffer.as_ref()).await.expect("write failed");

            child.object_id()
        };

        root_store.tombstone_object(child_id, Options::default()).await.expect("tombstone failed");

        // Let fsck check allocations.
        fsck(fs.clone()).await.expect("fsck failed");
    }

    #[fuchsia::test]
    async fn test_tombstone_purges_keys() {
        let fs = test_filesystem().await;
        let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
        let store = root_volume
            .new_volume("test", Some(Arc::new(InsecureCrypt::new())))
            .await
            .expect("new_volume failed");
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let child = ObjectStore::create_object(
            &store,
            &mut transaction,
            HandleOptions::default(),
            None,
            None,
        )
        .await
        .expect("create_object failed");
        transaction.commit().await.expect("commit failed");
        assert!(store.key_manager.get(child.object_id()).await.unwrap().is_some());
        store
            .tombstone_object(child.object_id(), Options::default())
            .await
            .expect("tombstone_object failed");
        assert!(store.key_manager.get(child.object_id()).await.unwrap().is_none());
        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn test_major_compaction_discards_unnecessary_records() {
        let fs = test_filesystem().await;
        let root_store = fs.root_store();
        let child_id = {
            let mut transaction = fs
                .clone()
                .new_transaction(lock_keys![], Options::default())
                .await
                .expect("new_transaction failed");
            let child = ObjectStore::create_object(
                &root_store,
                &mut transaction,
                HandleOptions::default(),
                None,
                None,
            )
            .await
            .expect("create_object failed");
            transaction.commit().await.expect("commit failed");

            // Allocate an extent in the file.
            let mut buffer = child.allocate_buffer(8192).await;
            buffer.as_mut_slice().fill(0xaa);
            child.write_or_append(Some(0), buffer.as_ref()).await.expect("write failed");

            child.object_id()
        };

        root_store.tombstone_object(child_id, Options::default()).await.expect("tombstone failed");
        {
            let layers = root_store.tree.layer_set();
            let mut merger = layers.merger();
            let iter = merger
                .query(Query::FullRange(&ObjectKey::object(child_id)))
                .await
                .expect("seek failed");
            // Find at least one object still in the tree.
            match iter.get() {
                Some(ItemRef { key: ObjectKey { object_id, .. }, .. })
                    if *object_id == child_id => {}
                _ => panic!("Objects should still be in the tree."),
            }
        }
        root_store.flush().await.expect("flush failed");

        // There should be no records for the object.
        let layers = root_store.tree.layer_set();
        let mut merger = layers.merger();
        let iter = merger
            .query(Query::FullRange(&ObjectKey::object(child_id)))
            .await
            .expect("seek failed");
        match iter.get() {
            None => {}
            Some(ItemRef { key: ObjectKey { object_id, .. }, .. }) => {
                assert_ne!(*object_id, child_id)
            }
        }
    }

    #[fuchsia::test]
    async fn test_overlapping_extents_in_different_layers() {
        let fs = test_filesystem().await;
        let store = fs.root_store();

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
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        let object = root_directory
            .create_child_file(&mut transaction, "test")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        let buf = object.allocate_buffer(16384).await;
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");

        store.flush().await.expect("flush failed");

        object.write_or_append(Some(0), buf.subslice(0..4096)).await.expect("write failed");

        // At this point, we should have an extent for 0..16384 in a layer that has been flushed,
        // and an extent for 0..4096 that partially overwrites it.  Writing to 0..16384 should
        // overwrite both of those extents.
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");

        fsck(fs.clone()).await.expect("fsck failed");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_encrypted_mutations() {
        async fn one_iteration(
            fs: OpenFxFilesystem,
            crypt: Arc<dyn Crypt>,
            iteration: u64,
        ) -> OpenFxFilesystem {
            async fn reopen(fs: OpenFxFilesystem) -> OpenFxFilesystem {
                fs.close().await.expect("Close failed");
                let device = fs.take_device().await;
                device.reopen(false);
                FxFilesystem::open(device).await.expect("FS open failed")
            }

            let fs = reopen(fs).await;

            let (store_object_id, object_id) = {
                let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
                let store =
                    root_volume.volume("test", Some(crypt.clone())).await.expect("volume failed");

                let mut transaction = fs
                    .clone()
                    .new_transaction(
                        lock_keys![LockKey::object(
                            store.store_object_id(),
                            store.root_directory_object_id(),
                        )],
                        Options::default(),
                    )
                    .await
                    .expect("new_transaction failed");
                let root_directory = Directory::open(&store, store.root_directory_object_id())
                    .await
                    .expect("open failed");
                let object = root_directory
                    .create_child_file(&mut transaction, &format!("test {}", iteration))
                    .await
                    .expect("create_child_file failed");
                transaction.commit().await.expect("commit failed");

                let mut buf = object.allocate_buffer(1000).await;
                for i in 0..buf.len() {
                    buf.as_mut_slice()[i] = i as u8;
                }
                object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");

                (store.store_object_id(), object.object_id())
            };

            let fs = reopen(fs).await;

            let check_object = |fs: Arc<FxFilesystem>| {
                let crypt = crypt.clone();
                async move {
                    let root_volume = root_volume(fs).await.expect("root_volume failed");
                    let volume =
                        root_volume.volume("test", Some(crypt)).await.expect("volume failed");

                    let object = ObjectStore::open_object(
                        &volume,
                        object_id,
                        HandleOptions::default(),
                        None,
                    )
                    .await
                    .expect("open_object failed");
                    let mut buf = object.allocate_buffer(1000).await;
                    assert_eq!(object.read(0, buf.as_mut()).await.expect("read failed"), 1000);
                    for i in 0..buf.len() {
                        assert_eq!(buf.as_slice()[i], i as u8);
                    }
                }
            };

            check_object(fs.clone()).await;

            let fs = reopen(fs).await;

            // At this point the "test" volume is locked.  Before checking the object, flush the
            // filesystem.  This should leave a file with encrypted mutations.
            fs.object_manager().flush().await.expect("flush failed");

            assert_ne!(
                fs.object_manager()
                    .store(store_object_id)
                    .unwrap()
                    .load_store_info()
                    .await
                    .expect("load_store_info failed")
                    .encrypted_mutations_object_id,
                INVALID_OBJECT_ID
            );

            check_object(fs.clone()).await;

            // Checking the object should have triggered a flush and so now there should be no
            // encrypted mutations object.
            assert_eq!(
                fs.object_manager()
                    .store(store_object_id)
                    .unwrap()
                    .load_store_info()
                    .await
                    .expect("load_store_info failed")
                    .encrypted_mutations_object_id,
                INVALID_OBJECT_ID
            );

            let fs = reopen(fs).await;

            fsck(fs.clone()).await.expect("fsck failed");

            let fs = reopen(fs).await;

            check_object(fs.clone()).await;

            fs
        }

        let mut fs = test_filesystem().await;
        let crypt = Arc::new(InsecureCrypt::new());

        {
            let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
            let _store = root_volume
                .new_volume("test", Some(crypt.clone()))
                .await
                .expect("new_volume failed");
        }

        // Run a few iterations so that we test changes with the stream cipher offset.
        for i in 0..5 {
            fs = one_iteration(fs, crypt.clone(), i).await;
        }
    }

    #[fuchsia::test(threads = 10)]
    async fn test_object_id_cipher_roll() {
        let fs = test_filesystem().await;
        let crypt = Arc::new(InsecureCrypt::new());

        {
            let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
            let store = root_volume
                .new_volume("test", Some(crypt.clone()))
                .await
                .expect("new_volume failed");

            let store_info = store.store_info().unwrap();

            // Hack the last object ID to force a roll of the object ID cipher.
            {
                let mut last_object_id = store.last_object_id.lock().unwrap();
                assert_eq!(last_object_id.id & OBJECT_ID_HI_MASK, 1u64 << 32);
                last_object_id.id |= 0xffffffff;
            }

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
            let root_directory = Directory::open(&store, store.root_directory_object_id())
                .await
                .expect("open failed");
            let object = root_directory
                .create_child_file(&mut transaction, "test")
                .await
                .expect("create_child_file failed");
            transaction.commit().await.expect("commit failed");

            assert_eq!(object.object_id() & OBJECT_ID_HI_MASK, 2u64 << 32);

            // Check that the key has been changed.
            assert_ne!(store.store_info().unwrap().object_id_key, store_info.object_id_key);

            assert_eq!(store.last_object_id.lock().unwrap().id, 2u64 << 32);
        };

        fs.close().await.expect("Close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");
        let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
        let store = root_volume.volume("test", Some(crypt.clone())).await.expect("volume failed");

        assert_eq!(store.last_object_id.lock().unwrap().id, 2u64 << 32);
    }

    #[fuchsia::test(threads = 10)]
    async fn test_lock_store() {
        let fs = test_filesystem().await;
        let crypt = Arc::new(InsecureCrypt::new());

        let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
        let store =
            root_volume.new_volume("test", Some(crypt.clone())).await.expect("new_volume failed");
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
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        root_directory
            .create_child_file(&mut transaction, "test")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");
        store.lock().await.expect("lock failed");

        store.unlock(crypt).await.expect("unlock failed");
        root_directory.lookup("test").await.expect("lookup failed").expect("not found");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_unlock_read_only() {
        let fs = test_filesystem().await;
        let crypt = Arc::new(InsecureCrypt::new());

        let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
        let store =
            root_volume.new_volume("test", Some(crypt.clone())).await.expect("new_volume failed");
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
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        root_directory
            .create_child_file(&mut transaction, "test")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");
        store.lock().await.expect("lock failed");

        store.unlock_read_only(crypt.clone()).await.expect("unlock failed");
        root_directory.lookup("test").await.expect("lookup failed").expect("not found");
        store.lock_read_only();
        store.unlock_read_only(crypt).await.expect("unlock failed");
        root_directory.lookup("test").await.expect("lookup failed").expect("not found");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_key_rolled_when_unlocked() {
        let fs = test_filesystem().await;
        let crypt = Arc::new(InsecureCrypt::new());

        let object_id;
        {
            let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
            let store = root_volume
                .new_volume("test", Some(crypt.clone()))
                .await
                .expect("new_volume failed");
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
            let root_directory = Directory::open(&store, store.root_directory_object_id())
                .await
                .expect("open failed");
            object_id = root_directory
                .create_child_file(&mut transaction, "test")
                .await
                .expect("create_child_file failed")
                .object_id();
            transaction.commit().await.expect("commit failed");
        }

        fs.close().await.expect("Close failed");
        let mut device = fs.take_device().await;

        // Repeatedly remount so that we can be sure that we can remount when there are many
        // mutations keys.
        for _ in 0..100 {
            device.reopen(false);
            let fs = FxFilesystem::open(device).await.expect("open failed");
            {
                let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
                let store = root_volume
                    .volume("test", Some(crypt.clone()))
                    .await
                    .expect("open_volume failed");

                // The key should get rolled every time we unlock.
                assert_eq!(store.mutations_cipher.lock().unwrap().as_ref().unwrap().offset(), 0);

                // Make sure there's an encrypted mutation.
                let handle =
                    ObjectStore::open_object(&store, object_id, HandleOptions::default(), None)
                        .await
                        .expect("open_object failed");
                let buffer = handle.allocate_buffer(100).await;
                handle
                    .write_or_append(Some(0), buffer.as_ref())
                    .await
                    .expect("write_or_append failed");
            }
            fs.close().await.expect("Close failed");
            device = fs.take_device().await;
        }
    }

    #[test]
    fn test_store_info_max_serialized_size() {
        let info = StoreInfo {
            guid: [0xff; 16],
            last_object_id: 0x1234567812345678,
            // Worst case, each layer should be 3/4 the size of the layer below it (because of the
            // compaction policy we're using).  If the smallest layer is 8,192 bytes, then 120
            // layers would take up a size that exceeds a 64 bit unsigned integer, so if this fits,
            // any size should fit.
            layers: vec![0x1234567812345678; 120],
            root_directory_object_id: 0x1234567812345678,
            graveyard_directory_object_id: 0x1234567812345678,
            object_count: 0x1234567812345678,
            mutations_key: Some(WrappedKey {
                wrapping_key_id: 0x1234567812345678,
                key: WrappedKeyBytes::from([0xff; WRAPPED_KEY_SIZE]),
            }),
            mutations_cipher_offset: 0x1234567812345678,
            encrypted_mutations_object_id: 0x1234567812345678,
            object_id_key: Some(WrappedKey {
                wrapping_key_id: 0x1234567812345678,
                key: WrappedKeyBytes::from([0xff; WRAPPED_KEY_SIZE]),
            }),
            internal_directory_object_id: INVALID_OBJECT_ID,
        };
        let mut serialized_info = Vec::new();
        info.serialize_with_version(&mut serialized_info).unwrap();
        assert!(
            serialized_info.len() <= MAX_STORE_INFO_SERIALIZED_SIZE,
            "{}",
            serialized_info.len()
        );
    }

    async fn reopen_after_crypt_failure_inner(read_only: bool) {
        let fs = test_filesystem().await;
        let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");

        let store = {
            let crypt = Arc::new(InsecureCrypt::new());
            let store = root_volume
                .new_volume("vol", Some(crypt.clone()))
                .await
                .expect("new_volume failed");
            let root_directory = Directory::open(&store, store.root_directory_object_id())
                .await
                .expect("open failed");
            let mut transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(
                        store.store_object_id(),
                        root_directory.object_id()
                    )],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");
            root_directory
                .create_child_file(&mut transaction, "test")
                .await
                .expect("create_child_file failed");
            transaction.commit().await.expect("commit failed");

            crypt.shutdown();
            let mut transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(
                        store.store_object_id(),
                        root_directory.object_id()
                    )],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");
            root_directory
                .create_child_file(&mut transaction, "test2")
                .await
                .map(|_| ())
                .expect_err("create_child_file should fail");
            store.lock().await.expect("lock failed");
            store
        };

        let crypt = Arc::new(InsecureCrypt::new());
        if read_only {
            store.unlock_read_only(crypt).await.expect("unlock failed");
        } else {
            store.unlock(crypt).await.expect("unlock failed");
        }
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        root_directory.lookup("test").await.expect("lookup failed").expect("not found");
    }

    #[fuchsia::test(threads = 10)]
    async fn test_reopen_after_crypt_failure() {
        reopen_after_crypt_failure_inner(false).await;
    }

    #[fuchsia::test(threads = 10)]
    async fn test_reopen_read_only_after_crypt_failure() {
        reopen_after_crypt_failure_inner(true).await;
    }

    #[fuchsia::test(threads = 10)]
    #[should_panic(expected = "Insufficient reservation space")]
    #[cfg(debug_assertions)]
    async fn large_transaction_causes_panic_in_debug_builds() {
        let fs = test_filesystem().await;
        let root_volume = root_volume(fs.clone()).await.expect("root_volume failed");
        let store = root_volume.new_volume("vol", None).await.expect("new_volume failed");
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("transaction");
        for i in 0..500 {
            root_directory
                .create_symlink(&mut transaction, b"link", &format!("{}", i))
                .await
                .expect("symlink");
        }
        assert_eq!(transaction.commit().await.expect("commit"), 0);
    }
}
