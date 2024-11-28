// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::checksum::{fletcher64, Checksum, Checksums};
use crate::errors::FxfsError;
use crate::log::*;
use crate::lsm_tree::types::{Item, ItemRef, LayerIterator};
use crate::lsm_tree::Query;
use crate::object_handle::ObjectHandle;
use crate::object_store::extent_record::{ExtentKey, ExtentMode, ExtentValue};
use crate::object_store::object_manager::ObjectManager;
use crate::object_store::object_record::{
    AttributeKey, EncryptionKeys, ExtendedAttributeValue, ObjectAttributes, ObjectItem, ObjectKey,
    ObjectKeyData, ObjectValue, Timestamp,
};
use crate::object_store::transaction::{
    lock_keys, AssocObj, AssociatedObject, LockKey, Mutation, ObjectStoreMutation, Options,
    ReadGuard, Transaction,
};
use crate::object_store::{
    HandleOptions, HandleOwner, ObjectStore, TrimMode, TrimResult, VOLUME_DATA_KEY_ID,
};
use crate::range::RangeExt;
use crate::round::{round_down, round_up};
use anyhow::{anyhow, bail, ensure, Context, Error};
use assert_matches::assert_matches;
use bit_vec::BitVec;
use futures::stream::{FuturesOrdered, FuturesUnordered};
use futures::{try_join, TryStreamExt};
use fxfs_crypto::{Cipher, CipherSet, FindKeyResult, Key, KeyPurpose};
use fxfs_trace::trace;
use static_assertions::const_assert;
use std::cmp::min;
use std::future::Future;
use std::ops::Range;
use std::sync::atomic::{self, AtomicBool, Ordering};
use std::sync::Arc;
use storage_device::buffer::{Buffer, BufferFuture, BufferRef, MutableBufferRef};
use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

/// Maximum size for an extended attribute name.
pub const MAX_XATTR_NAME_SIZE: usize = 255;
/// Maximum size an extended attribute can be before it's stored in an object attribute instead of
/// inside the record directly.
pub const MAX_INLINE_XATTR_SIZE: usize = 256;
/// Maximum size for an extended attribute value. NB: the maximum size for an extended attribute is
/// 64kB, which we rely on for correctness when deleting attributes, so ensure it's always
/// enforced.
pub const MAX_XATTR_VALUE_SIZE: usize = 64000;
/// The range of fxfs attribute ids which are reserved for extended attribute values. Whenever a
/// new attribute is needed, the first unused id will be chosen from this range. It's technically
/// safe to change these values, but it has potential consequences - they are only used during id
/// selection, so any existing extended attributes keep their ids, which means any past or present
/// selected range here could potentially have used attributes unless they are explicitly migrated,
/// which isn't currently done.
pub const EXTENDED_ATTRIBUTE_RANGE_START: u64 = 64;
pub const EXTENDED_ATTRIBUTE_RANGE_END: u64 = 512;

/// When writing, often the logic should be generic over whether or not checksums are generated.
/// This provides that and a handy way to convert to the more general ExtentMode that eventually
/// stores it on disk.
#[derive(Debug, Clone, PartialEq)]
pub enum MaybeChecksums {
    None,
    Fletcher(Vec<Checksum>),
}

impl MaybeChecksums {
    pub fn maybe_as_ref(&self) -> Option<&[Checksum]> {
        match self {
            Self::None => None,
            Self::Fletcher(sums) => Some(&sums),
        }
    }

    pub fn split_off(&mut self, at: usize) -> Self {
        match self {
            Self::None => Self::None,
            Self::Fletcher(sums) => Self::Fletcher(sums.split_off(at)),
        }
    }

    pub fn to_mode(self) -> ExtentMode {
        match self {
            Self::None => ExtentMode::Raw,
            Self::Fletcher(sums) => ExtentMode::Cow(Checksums::fletcher(sums)),
        }
    }

    pub fn into_option(self) -> Option<Vec<Checksum>> {
        match self {
            Self::None => None,
            Self::Fletcher(sums) => Some(sums),
        }
    }
}

/// The mode of operation when setting extended attributes. This is the same as the fidl definition
/// but is replicated here so we don't have fuchsia.io structures in the api, so this can be used
/// on host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetExtendedAttributeMode {
    /// Create the extended attribute if it doesn't exist, replace the value if it does.
    Set,
    /// Create the extended attribute if it doesn't exist, fail if it does.
    Create,
    /// Replace the extended attribute value if it exists, fail if it doesn't.
    Replace,
}

impl From<fio::SetExtendedAttributeMode> for SetExtendedAttributeMode {
    fn from(other: fio::SetExtendedAttributeMode) -> SetExtendedAttributeMode {
        match other {
            fio::SetExtendedAttributeMode::Set => SetExtendedAttributeMode::Set,
            fio::SetExtendedAttributeMode::Create => SetExtendedAttributeMode::Create,
            fio::SetExtendedAttributeMode::Replace => SetExtendedAttributeMode::Replace,
        }
    }
}

enum Encryption {
    /// The object doesn't use encryption.
    None,

    /// The object has keys that are cached (which means unwrapping occurs on-demand) with
    /// KeyManager.
    CachedKeys,

    /// The object has permanent keys registered with KeyManager.
    PermanentKeys,
}

/// StoreObjectHandle is the lowest-level, untyped handle to an object with the id [`object_id`] in
/// a particular store, [`owner`]. It provides functionality shared across all objects, such as
/// reading and writing attributes and managing encryption keys.
///
/// Since it's untyped, it doesn't do any object kind validation, and is generally meant to
/// implement higher-level typed handles.
///
/// For file-like objects with a data attribute, DataObjectHandle implements traits and helpers for
/// doing more complex extent management and caches the content size.
///
/// For directory-like objects, Directory knows how to add and remove child objects and enumerate
/// its children.
pub struct StoreObjectHandle<S: HandleOwner> {
    owner: Arc<S>,
    object_id: u64,
    options: HandleOptions,
    trace: AtomicBool,
    encryption: Encryption,
}

impl<S: HandleOwner> ObjectHandle for StoreObjectHandle<S> {
    fn set_trace(&self, v: bool) {
        info!(store_id = self.store().store_object_id, oid = self.object_id(), trace = v, "trace");
        self.trace.store(v, atomic::Ordering::Relaxed);
    }

    fn object_id(&self) -> u64 {
        return self.object_id;
    }

    fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
        self.store().device.allocate_buffer(size)
    }

    fn block_size(&self) -> u64 {
        self.store().block_size()
    }
}

struct Watchdog {
    _task: fasync::Task<()>,
}

impl Watchdog {
    fn new(increment_seconds: u64, cb: impl Fn(u64) + Send + 'static) -> Self {
        Self {
            _task: fasync::Task::spawn(async move {
                let increment = increment_seconds.try_into().unwrap();
                let mut fired_counter = 0;
                let mut next_wake = fasync::MonotonicInstant::now();
                loop {
                    next_wake += std::time::Duration::from_secs(increment).into();
                    // If this isn't being scheduled this will purposely result in fast looping when
                    // it does. This will be insightful about the state of the thread and task
                    // scheduling.
                    if fasync::MonotonicInstant::now() < next_wake {
                        fasync::Timer::new(next_wake).await;
                    }
                    fired_counter += 1;
                    cb(fired_counter);
                }
            }),
        }
    }
}

impl<S: HandleOwner> StoreObjectHandle<S> {
    /// Make a new StoreObjectHandle for the object with id [`object_id`] in store [`owner`].
    pub fn new(
        owner: Arc<S>,
        object_id: u64,
        permanent_keys: bool,
        options: HandleOptions,
        trace: bool,
    ) -> Self {
        let encryption = if permanent_keys {
            Encryption::PermanentKeys
        } else if owner.as_ref().as_ref().is_encrypted() {
            Encryption::CachedKeys
        } else {
            Encryption::None
        };
        Self { owner, object_id, encryption, options, trace: AtomicBool::new(trace) }
    }

    pub fn owner(&self) -> &Arc<S> {
        &self.owner
    }

    pub fn store(&self) -> &ObjectStore {
        self.owner.as_ref().as_ref()
    }

    pub fn trace(&self) -> bool {
        self.trace.load(atomic::Ordering::Relaxed)
    }

    pub fn is_encrypted(&self) -> bool {
        !matches!(self.encryption, Encryption::None)
    }

    /// Get the default set of transaction options for this object. This is mostly the overall
    /// default, modified by any [`HandleOptions`] held by this handle.
    pub fn default_transaction_options<'b>(&self) -> Options<'b> {
        Options { skip_journal_checks: self.options.skip_journal_checks, ..Default::default() }
    }

    pub async fn new_transaction_with_options<'b>(
        &self,
        attribute_id: u64,
        options: Options<'b>,
    ) -> Result<Transaction<'b>, Error> {
        Ok(self
            .store()
            .filesystem()
            .new_transaction(
                lock_keys![
                    LockKey::object_attribute(
                        self.store().store_object_id(),
                        self.object_id(),
                        attribute_id,
                    ),
                    LockKey::object(self.store().store_object_id(), self.object_id()),
                ],
                options,
            )
            .await?)
    }

    pub async fn new_transaction<'b>(&self, attribute_id: u64) -> Result<Transaction<'b>, Error> {
        self.new_transaction_with_options(attribute_id, self.default_transaction_options()).await
    }

    // If |transaction| has an impending mutation for the underlying object, returns that.
    // Otherwise, looks up the object from the tree.
    async fn txn_get_object_mutation(
        &self,
        transaction: &Transaction<'_>,
    ) -> Result<ObjectStoreMutation, Error> {
        self.store().txn_get_object_mutation(transaction, self.object_id()).await
    }

    // Returns the amount deallocated.
    async fn deallocate_old_extents(
        &self,
        transaction: &mut Transaction<'_>,
        attribute_id: u64,
        range: Range<u64>,
    ) -> Result<u64, Error> {
        let block_size = self.block_size();
        assert_eq!(range.start % block_size, 0);
        assert_eq!(range.end % block_size, 0);
        if range.start == range.end {
            return Ok(0);
        }
        let tree = &self.store().tree;
        let layer_set = tree.layer_set();
        let key = ExtentKey { range };
        let lower_bound = ObjectKey::attribute(
            self.object_id(),
            attribute_id,
            AttributeKey::Extent(key.search_key()),
        );
        let mut merger = layer_set.merger();
        let mut iter = merger.query(Query::FullRange(&lower_bound)).await?;
        let allocator = self.store().allocator();
        let mut deallocated = 0;
        let trace = self.trace();
        while let Some(ItemRef {
            key:
                ObjectKey {
                    object_id,
                    data: ObjectKeyData::Attribute(attr_id, AttributeKey::Extent(extent_key)),
                },
            value: ObjectValue::Extent(value),
            ..
        }) = iter.get()
        {
            if *object_id != self.object_id() || *attr_id != attribute_id {
                break;
            }
            if let ExtentValue::Some { device_offset, .. } = value {
                if let Some(overlap) = key.overlap(extent_key) {
                    let range = device_offset + overlap.start - extent_key.range.start
                        ..device_offset + overlap.end - extent_key.range.start;
                    ensure!(range.is_aligned(block_size), FxfsError::Inconsistent);
                    if trace {
                        info!(
                            store_id = self.store().store_object_id(),
                            oid = self.object_id(),
                            device_range = ?range,
                            len = range.end - range.start,
                            ?extent_key,
                            "D",
                        );
                    }
                    allocator
                        .deallocate(transaction, self.store().store_object_id(), range)
                        .await?;
                    deallocated += overlap.end - overlap.start;
                } else {
                    break;
                }
            }
            iter.advance().await?;
        }
        Ok(deallocated)
    }

    // Writes aligned data (that should already be encrypted) to the given offset and computes
    // checksums if requested.
    async fn write_aligned(
        &self,
        buf: BufferRef<'_>,
        device_offset: u64,
    ) -> Result<MaybeChecksums, Error> {
        if self.trace() {
            info!(
                store_id = self.store().store_object_id(),
                oid = self.object_id(),
                device_range = ?(device_offset..device_offset + buf.len() as u64),
                len = buf.len(),
                "W",
            );
        }
        let store = self.store();
        store.device_write_ops.fetch_add(1, Ordering::Relaxed);
        let mut checksums = Vec::new();
        let _watchdog = Watchdog::new(10, |count| {
            warn!("Write has been stalled for {} seconds", count * 10);
        });
        try_join!(store.device.write(device_offset, buf), async {
            if !self.options.skip_checksums {
                let block_size = self.block_size();
                for chunk in buf.as_slice().chunks_exact(block_size as usize) {
                    checksums.push(fletcher64(chunk, 0));
                }
            }
            Ok(())
        })?;
        Ok(if !self.options.skip_checksums {
            MaybeChecksums::Fletcher(checksums)
        } else {
            MaybeChecksums::None
        })
    }

    /// Flushes the underlying device.  This is expensive and should be used sparingly.
    pub async fn flush_device(&self) -> Result<(), Error> {
        self.store().device().flush().await
    }

    pub async fn update_allocated_size(
        &self,
        transaction: &mut Transaction<'_>,
        allocated: u64,
        deallocated: u64,
    ) -> Result<(), Error> {
        if allocated == deallocated {
            return Ok(());
        }
        let mut mutation = self.txn_get_object_mutation(transaction).await?;
        if let ObjectValue::Object {
            attributes: ObjectAttributes { project_id, allocated_size, .. },
            ..
        } = &mut mutation.item.value
        {
            // The only way for these to fail are if the volume is inconsistent.
            *allocated_size = allocated_size
                .checked_add(allocated)
                .ok_or_else(|| anyhow!(FxfsError::Inconsistent).context("Allocated size overflow"))?
                .checked_sub(deallocated)
                .ok_or_else(|| {
                    anyhow!(FxfsError::Inconsistent).context("Allocated size underflow")
                })?;

            if *project_id != 0 {
                // The allocated and deallocated shouldn't exceed the max size of the file which is
                // bound within i64.
                let diff = i64::try_from(allocated).unwrap() - i64::try_from(deallocated).unwrap();
                transaction.add(
                    self.store().store_object_id(),
                    Mutation::merge_object(
                        ObjectKey::project_usage(
                            self.store().root_directory_object_id(),
                            *project_id,
                        ),
                        ObjectValue::BytesAndNodes { bytes: diff, nodes: 0 },
                    ),
                );
            }
        } else {
            // This can occur when the object mutation is created from an object in the tree which
            // was corrupt.
            bail!(anyhow!(FxfsError::Inconsistent).context("Unexpected object value"));
        }
        transaction.add(self.store().store_object_id, Mutation::ObjectStore(mutation));
        Ok(())
    }

    pub async fn update_attributes<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        node_attributes: Option<&fio::MutableNodeAttributes>,
        change_time: Option<Timestamp>,
    ) -> Result<(), Error> {
        if let Some(&fio::MutableNodeAttributes { selinux_context: Some(ref context), .. }) =
            node_attributes
        {
            if let fio::SelinuxContext::Data(context) = context {
                self.set_extended_attribute_impl(
                    "security.selinux".into(),
                    context.clone(),
                    SetExtendedAttributeMode::Set,
                    transaction,
                )
                .await?;
            } else {
                return Err(anyhow!(FxfsError::InvalidArgs)
                    .context("Only set SELinux context with `data` member."));
            }
        }
        self.store()
            .update_attributes(transaction, self.object_id, node_attributes, change_time)
            .await
    }

    /// Zeroes the given range.  The range must be aligned.  Returns the amount of data deallocated.
    pub async fn zero(
        &self,
        transaction: &mut Transaction<'_>,
        attribute_id: u64,
        range: Range<u64>,
    ) -> Result<(), Error> {
        let deallocated =
            self.deallocate_old_extents(transaction, attribute_id, range.clone()).await?;
        if deallocated > 0 {
            self.update_allocated_size(transaction, 0, deallocated).await?;
            transaction.add(
                self.store().store_object_id,
                Mutation::merge_object(
                    ObjectKey::extent(self.object_id(), attribute_id, range),
                    ObjectValue::Extent(ExtentValue::deleted_extent()),
                ),
            );
        }
        Ok(())
    }

    // Returns a new aligned buffer (reading the head and tail blocks if necessary) with a copy of
    // the data from `buf`.
    pub async fn align_buffer(
        &self,
        attribute_id: u64,
        offset: u64,
        buf: BufferRef<'_>,
    ) -> Result<(std::ops::Range<u64>, Buffer<'_>), Error> {
        let block_size = self.block_size();
        let end = offset + buf.len() as u64;
        let aligned =
            round_down(offset, block_size)..round_up(end, block_size).ok_or(FxfsError::TooBig)?;

        let mut aligned_buf =
            self.store().device.allocate_buffer((aligned.end - aligned.start) as usize).await;

        // Deal with head alignment.
        if aligned.start < offset {
            let mut head_block = aligned_buf.subslice_mut(..block_size as usize);
            let read = self.read(attribute_id, aligned.start, head_block.reborrow()).await?;
            head_block.as_mut_slice()[read..].fill(0);
        }

        // Deal with tail alignment.
        if aligned.end > end {
            let end_block_offset = aligned.end - block_size;
            // There's no need to read the tail block if we read it as part of the head block.
            if offset <= end_block_offset {
                let mut tail_block =
                    aligned_buf.subslice_mut(aligned_buf.len() - block_size as usize..);
                let read = self.read(attribute_id, end_block_offset, tail_block.reborrow()).await?;
                tail_block.as_mut_slice()[read..].fill(0);
            }
        }

        aligned_buf.as_mut_slice()
            [(offset - aligned.start) as usize..(end - aligned.start) as usize]
            .copy_from_slice(buf.as_slice());

        Ok((aligned, aligned_buf))
    }

    /// Trim an attribute's extents, potentially adding a graveyard trim entry if more trimming is
    /// needed, so the transaction can be committed without worrying about leaking data.
    ///
    /// This doesn't update the size stored in the attribute value - the caller is responsible for
    /// doing that to keep the size up to date.
    pub async fn shrink(
        &self,
        transaction: &mut Transaction<'_>,
        attribute_id: u64,
        size: u64,
    ) -> Result<NeedsTrim, Error> {
        let store = self.store();
        let needs_trim = matches!(
            store
                .trim_some(transaction, self.object_id(), attribute_id, TrimMode::FromOffset(size))
                .await?,
            TrimResult::Incomplete
        );
        if needs_trim {
            // Add the object to the graveyard in case the following transactions don't get
            // replayed.
            let graveyard_id = store.graveyard_directory_object_id();
            match store
                .tree
                .find(&ObjectKey::graveyard_entry(graveyard_id, self.object_id()))
                .await?
            {
                Some(ObjectItem { value: ObjectValue::Some, .. })
                | Some(ObjectItem { value: ObjectValue::Trim, .. }) => {
                    // This object is already in the graveyard so we don't need to do anything.
                }
                _ => {
                    transaction.add(
                        store.store_object_id,
                        Mutation::replace_or_insert_object(
                            ObjectKey::graveyard_entry(graveyard_id, self.object_id()),
                            ObjectValue::Trim,
                        ),
                    );
                }
            }
        }
        Ok(NeedsTrim(needs_trim))
    }

    pub async fn read_and_decrypt(
        &self,
        device_offset: u64,
        file_offset: u64,
        mut buffer: MutableBufferRef<'_>,
        key_id: u64,
        // If provided, blocks in the bitmap that are zero will have their contents zeroed out. The
        // bitmap should be exactly the size of the buffer and aligned to the offset in the extent
        // the read is starting at.
        block_bitmap: Option<bit_vec::BitVec>,
    ) -> Result<(), Error> {
        let store = self.store();
        store.device_read_ops.fetch_add(1, Ordering::Relaxed);
        let ((), key) = {
            let _watchdog = Watchdog::new(10, |count| {
                warn!("Read has been stalled for {} seconds", count * 10);
            });
            futures::future::try_join(
                store.device.read(device_offset, buffer.reborrow()),
                self.get_key(Some(key_id)),
            )
            .await?
        };
        if let Some(key) = key {
            key.decrypt(file_offset, buffer.as_mut_slice())?;
        }
        if let Some(bitmap) = block_bitmap {
            let block_size = self.block_size() as usize;
            let buf = buffer.as_mut_slice();
            debug_assert_eq!(bitmap.len() * block_size, buf.len());
            for (i, block) in bitmap.iter().enumerate() {
                if !block {
                    let start = i * block_size;
                    buf[start..start + block_size].fill(0);
                }
            }
        }
        Ok(())
    }

    /// Returns the specified key. If `key_id` is None, it will try and return the fscrypt key if
    /// it is present, or the volume key if it isn't. If the fscrypt key is present, but the key
    /// cannot be unwrapped, then this will return `FxfsError::NoKey`. If the volume is not
    /// encrypted, this returns None.
    pub async fn get_key(&self, key_id: Option<u64>) -> Result<Option<Key>, Error> {
        let store = self.store();
        Ok(match self.encryption {
            Encryption::None => None,
            Encryption::CachedKeys => {
                if let Some(key_id) = key_id {
                    Some(
                        store
                            .key_manager
                            .get_key(
                                self.object_id,
                                store.crypt().ok_or_else(|| anyhow!("No crypt!"))?.as_ref(),
                                store.get_keys(self.object_id),
                                key_id,
                            )
                            .await?
                            .ok_or(FxfsError::NotFound)?,
                    )
                } else {
                    Some(
                        store
                            .key_manager
                            .get_fscrypt_key_if_present(
                                self.object_id,
                                store.crypt().ok_or_else(|| anyhow!("No crypt!"))?.as_ref(),
                                store.get_keys(self.object_id),
                            )
                            .await?,
                    )
                }
            }
            Encryption::PermanentKeys => {
                Some(store.key_manager.get(self.object_id).await?.unwrap())
            }
        })
    }

    /// This will only work for a non-permanent volume data key. This is designed to be used with
    /// extended attributes where we'll only create the key on demand for directories and encrypted
    /// files.
    async fn get_or_create_key(&self, transaction: &mut Transaction<'_>) -> Result<Key, Error> {
        let store = self.store();

        // Fast path: try and get keys from the cache.
        if let Some(key) = store.key_manager.get(self.object_id).await.context("get failed")? {
            return Ok(key);
        }

        let crypt = store.crypt().ok_or_else(|| anyhow!("No crypt!"))?;

        // Next, see if the keys are already created.
        let (mut wrapped_keys, mut unwrapped_keys) = if let Some(item) =
            store.tree.find(&ObjectKey::keys(self.object_id)).await.context("find failed")?
        {
            if let ObjectValue::Keys(EncryptionKeys::AES256XTS(wrapped_keys)) = item.value {
                let unwrapped_keys = store
                    .key_manager
                    .get_keys(
                        self.object_id,
                        crypt.as_ref(),
                        &mut Some(async { Ok(wrapped_keys.clone()) }),
                        /* permanent= */ false,
                        /* force= */ false,
                    )
                    .await
                    .context("get_keys failed")?;
                match unwrapped_keys.find_key(VOLUME_DATA_KEY_ID) {
                    FindKeyResult::NotFound => {}
                    FindKeyResult::Unavailable => return Err(FxfsError::NoKey.into()),
                    FindKeyResult::Key(key) => return Ok(key),
                }
                (wrapped_keys, unwrapped_keys.ciphers().to_vec())
            } else {
                return Err(anyhow!(FxfsError::Inconsistent));
            }
        } else {
            Default::default()
        };

        // Proceed to create the key.  The transaction holds the required locks.
        let (key, unwrapped_key) = crypt.create_key(self.object_id, KeyPurpose::Data).await?;

        // Merge in unwrapped_key.
        unwrapped_keys.push(Cipher::new(VOLUME_DATA_KEY_ID, &unwrapped_key));
        let unwrapped_keys = Arc::new(CipherSet::from(unwrapped_keys));

        // Arrange for the key to be added to the cache when (and if) the transaction
        // commits.
        struct UnwrappedKeys {
            object_id: u64,
            new_keys: Arc<CipherSet>,
        }

        impl AssociatedObject for UnwrappedKeys {
            fn will_apply_mutation(
                &self,
                _mutation: &Mutation,
                object_id: u64,
                manager: &ObjectManager,
            ) {
                manager.store(object_id).unwrap().key_manager.insert(
                    self.object_id,
                    self.new_keys.clone(),
                    /* permanent= */ false,
                );
            }
        }

        wrapped_keys.push((VOLUME_DATA_KEY_ID, key));

        transaction.add_with_object(
            store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::keys(self.object_id),
                ObjectValue::keys(EncryptionKeys::AES256XTS(wrapped_keys)),
            ),
            AssocObj::Owned(Box::new(UnwrappedKeys {
                object_id: self.object_id,
                new_keys: unwrapped_keys.clone(),
            })),
        );

        let FindKeyResult::Key(key) = unwrapped_keys.find_key(VOLUME_DATA_KEY_ID) else {
            unreachable!()
        };
        Ok(key)
    }

    pub async fn read(
        &self,
        attribute_id: u64,
        offset: u64,
        mut buf: MutableBufferRef<'_>,
    ) -> Result<usize, Error> {
        let fs = self.store().filesystem();
        let guard = fs
            .lock_manager()
            .read_lock(lock_keys![LockKey::object_attribute(
                self.store().store_object_id(),
                self.object_id(),
                attribute_id,
            )])
            .await;

        let key = ObjectKey::attribute(self.object_id(), attribute_id, AttributeKey::Attribute);
        let item = self.store().tree().find(&key).await?;
        let size = match item {
            Some(item) if item.key == key => match item.value {
                ObjectValue::Attribute { size, .. } => size,
                _ => bail!(FxfsError::Inconsistent),
            },
            _ => return Ok(0),
        };
        if offset >= size {
            return Ok(0);
        }
        let length = min(buf.len() as u64, size - offset) as usize;
        buf = buf.subslice_mut(0..length);
        self.read_unchecked(attribute_id, offset, buf, &guard).await?;
        Ok(length)
    }

    /// Read `buf.len()` bytes from the attribute `attribute_id`, starting at `offset`, into `buf`.
    /// It's required that a read lock on this attribute id is taken before this is called.
    ///
    /// This function doesn't do any size checking - any portion of `buf` past the end of the file
    /// will be filled with zeros. The caller is responsible for enforcing the file size on reads.
    /// This is because, just looking at the extents, we can't tell the difference between the file
    /// actually ending and there just being a section at the end with no data (since attributes
    /// are sparse).
    pub(super) async fn read_unchecked(
        &self,
        attribute_id: u64,
        mut offset: u64,
        mut buf: MutableBufferRef<'_>,
        _guard: &ReadGuard<'_>,
    ) -> Result<(), Error> {
        if buf.len() == 0 {
            return Ok(());
        }
        let end_offset = offset + buf.len() as u64;

        self.store().logical_read_ops.fetch_add(1, Ordering::Relaxed);

        // Whilst the read offset must be aligned to the filesystem block size, the buffer need only
        // be aligned to the device's block size.
        let block_size = self.block_size() as u64;
        let device_block_size = self.store().device.block_size() as u64;
        assert_eq!(offset % block_size, 0);
        assert_eq!(buf.range().start as u64 % device_block_size, 0);
        let tree = &self.store().tree;
        let layer_set = tree.layer_set();
        let mut merger = layer_set.merger();
        let mut iter = merger
            .query(Query::LimitedRange(&ObjectKey::extent(
                self.object_id(),
                attribute_id,
                offset..end_offset,
            )))
            .await?;
        let end_align = ((offset + buf.len() as u64) % block_size) as usize;
        let trace = self.trace();
        let reads = FuturesUnordered::new();
        while let Some(ItemRef {
            key:
                ObjectKey {
                    object_id,
                    data: ObjectKeyData::Attribute(attr_id, AttributeKey::Extent(extent_key)),
                },
            value: ObjectValue::Extent(extent_value),
            ..
        }) = iter.get()
        {
            if *object_id != self.object_id() || *attr_id != attribute_id {
                break;
            }
            ensure!(
                extent_key.range.is_valid() && extent_key.range.is_aligned(block_size),
                FxfsError::Inconsistent
            );
            if extent_key.range.start > offset {
                // Zero everything up to the start of the extent.
                let to_zero = min(extent_key.range.start - offset, buf.len() as u64) as usize;
                for i in &mut buf.as_mut_slice()[..to_zero] {
                    *i = 0;
                }
                buf = buf.subslice_mut(to_zero..);
                if buf.is_empty() {
                    break;
                }
                offset += to_zero as u64;
            }

            if let ExtentValue::Some { device_offset, key_id, mode } = extent_value {
                let mut device_offset = device_offset + (offset - extent_key.range.start);
                let key_id = *key_id;

                let to_copy = min(buf.len() - end_align, (extent_key.range.end - offset) as usize);
                if to_copy > 0 {
                    if trace {
                        info!(
                            store_id = self.store().store_object_id(),
                            oid = self.object_id(),
                            device_range = ?(device_offset..device_offset + to_copy as u64),
                            offset,
                            range = ?extent_key.range,
                            block_size,
                            "R",
                        );
                    }
                    let (head, tail) = buf.split_at_mut(to_copy);
                    let maybe_bitmap = match mode {
                        ExtentMode::OverwritePartial(bitmap) => {
                            let mut read_bitmap = bitmap.clone().split_off(
                                ((offset - extent_key.range.start) / block_size) as usize,
                            );
                            read_bitmap.truncate(to_copy / block_size as usize);
                            Some(read_bitmap)
                        }
                        _ => None,
                    };
                    reads.push(self.read_and_decrypt(
                        device_offset,
                        offset,
                        head,
                        key_id,
                        maybe_bitmap,
                    ));
                    buf = tail;
                    if buf.is_empty() {
                        break;
                    }
                    offset += to_copy as u64;
                    device_offset += to_copy as u64;
                }

                // Deal with end alignment by reading the existing contents into an alignment
                // buffer.
                if offset < extent_key.range.end && end_align > 0 {
                    if let ExtentMode::OverwritePartial(bitmap) = mode {
                        let bitmap_offset = (offset - extent_key.range.start) / block_size;
                        if !bitmap.get(bitmap_offset as usize).ok_or(FxfsError::Inconsistent)? {
                            // If this block isn't actually initialized, skip it.
                            break;
                        }
                    }
                    let mut align_buf =
                        self.store().device.allocate_buffer(block_size as usize).await;
                    if trace {
                        info!(
                            store_id = self.store().store_object_id(),
                            oid = self.object_id(),
                            device_range = ?(device_offset..device_offset + align_buf.len() as u64),
                            "RT",
                        );
                    }
                    self.read_and_decrypt(device_offset, offset, align_buf.as_mut(), key_id, None)
                        .await?;
                    buf.as_mut_slice().copy_from_slice(&align_buf.as_slice()[..end_align]);
                    buf = buf.subslice_mut(0..0);
                    break;
                }
            } else if extent_key.range.end >= offset + buf.len() as u64 {
                // Deleted extent covers remainder, so we're done.
                break;
            }

            iter.advance().await?;
        }
        reads.try_collect::<()>().await?;
        buf.as_mut_slice().fill(0);
        Ok(())
    }

    /// Reads an entire attribute.
    pub async fn read_attr(&self, attribute_id: u64) -> Result<Option<Box<[u8]>>, Error> {
        let store = self.store();
        let tree = &store.tree;
        let layer_set = tree.layer_set();
        let mut merger = layer_set.merger();
        let key = ObjectKey::attribute(self.object_id(), attribute_id, AttributeKey::Attribute);
        let mut iter = merger.query(Query::FullRange(&key)).await?;
        let (mut buffer, size) = match iter.get() {
            Some(item) if item.key == &key => match item.value {
                ObjectValue::Attribute { size, .. } => {
                    // TODO(https://fxbug.dev/42073113): size > max buffer size
                    (
                        store
                            .device
                            .allocate_buffer(round_up(*size, self.block_size()).unwrap() as usize)
                            .await,
                        *size as usize,
                    )
                }
                // Attribute was deleted.
                ObjectValue::None => return Ok(None),
                _ => bail!(FxfsError::Inconsistent),
            },
            _ => return Ok(None),
        };
        store.logical_read_ops.fetch_add(1, Ordering::Relaxed);
        let mut last_offset = 0;
        loop {
            iter.advance().await?;
            match iter.get() {
                Some(ItemRef {
                    key:
                        ObjectKey {
                            object_id,
                            data:
                                ObjectKeyData::Attribute(attr_id, AttributeKey::Extent(extent_key)),
                        },
                    value: ObjectValue::Extent(extent_value),
                    ..
                }) if *object_id == self.object_id() && *attr_id == attribute_id => {
                    if let ExtentValue::Some { device_offset, key_id, mode } = extent_value {
                        let offset = extent_key.range.start as usize;
                        buffer.as_mut_slice()[last_offset..offset].fill(0);
                        let end = std::cmp::min(extent_key.range.end as usize, buffer.len());
                        let maybe_bitmap = match mode {
                            ExtentMode::OverwritePartial(bitmap) => {
                                // The caller has to adjust the bitmap if necessary, but we always
                                // start from the beginning of any extent, so we only truncate.
                                let mut read_bitmap = bitmap.clone();
                                read_bitmap.truncate(
                                    (end - extent_key.range.start as usize)
                                        / self.block_size() as usize,
                                );
                                Some(read_bitmap)
                            }
                            _ => None,
                        };
                        self.read_and_decrypt(
                            *device_offset,
                            extent_key.range.start,
                            buffer.subslice_mut(offset..end as usize),
                            *key_id,
                            maybe_bitmap,
                        )
                        .await?;
                        last_offset = end;
                        if last_offset >= size {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        buffer.as_mut_slice()[std::cmp::min(last_offset, size)..].fill(0);
        Ok(Some(buffer.as_slice()[..size].into()))
    }

    /// Writes potentially unaligned data at `device_offset` and returns checksums if requested.
    /// The data will be encrypted if necessary.  `buf` is mutable as an optimization, since the
    /// write may require encryption, we can encrypt the buffer in-place rather than copying to
    /// another buffer if the write is already aligned.
    ///
    /// NOTE: This will not create keys if they are missing (it will fail with an error if that
    /// happens to be the case).
    pub async fn write_at(
        &self,
        attribute_id: u64,
        offset: u64,
        buf: MutableBufferRef<'_>,
        key_id: Option<u64>,
        device_offset: u64,
    ) -> Result<MaybeChecksums, Error> {
        let mut transfer_buf;
        let block_size = self.block_size();
        let (range, mut transfer_buf_ref) =
            if offset % block_size == 0 && buf.len() as u64 % block_size == 0 {
                (offset..offset + buf.len() as u64, buf)
            } else {
                let (range, buf) = self.align_buffer(attribute_id, offset, buf.as_ref()).await?;
                transfer_buf = buf;
                (range, transfer_buf.as_mut())
            };

        if let Some(key) = self.get_key(key_id).await? {
            key.encrypt(range.start, transfer_buf_ref.as_mut_slice())?;
        }

        self.write_aligned(transfer_buf_ref.as_ref(), device_offset - (offset - range.start)).await
    }

    /// Writes to multiple ranges with data provided in `buf`.  The buffer can be modified in place
    /// if encryption takes place.  The ranges must all be aligned and no change to content size is
    /// applied; the caller is responsible for updating size if required.  If `key_id` is None, it
    /// means pick the default key for the object which is the fscrypt key if present, or the volume
    /// data key, or no key if it's an unencrypted file.
    pub async fn multi_write(
        &self,
        transaction: &mut Transaction<'_>,
        attribute_id: u64,
        key_id: Option<u64>,
        ranges: &[Range<u64>],
        mut buf: MutableBufferRef<'_>,
    ) -> Result<(), Error> {
        if buf.is_empty() {
            return Ok(());
        }
        let block_size = self.block_size();
        let store = self.store();
        let store_id = store.store_object_id();

        // The only key we allow to be created on-the-fly is a non permanent key wrapped with the
        // volume data key.
        let key = if key_id == Some(VOLUME_DATA_KEY_ID)
            && matches!(self.encryption, Encryption::CachedKeys)
        {
            Some(self.get_or_create_key(transaction).await.context("get_or_create_key failed")?)
        } else {
            self.get_key(key_id).await?
        };
        let key_id = if let Some(key) = key {
            let mut slice = buf.as_mut_slice();
            for r in ranges {
                let l = r.end - r.start;
                let (head, tail) = slice.split_at_mut(l as usize);
                key.encrypt(r.start, head)?;
                slice = tail;
            }
            key.key_id()
        } else {
            0
        };

        let mut allocated = 0;
        let allocator = store.allocator();
        let trace = self.trace();
        let mut writes = FuturesOrdered::new();
        while !buf.is_empty() {
            let device_range = allocator
                .allocate(transaction, store_id, buf.len() as u64)
                .await
                .context("allocation failed")?;
            if trace {
                info!(
                    store_id,
                    oid = self.object_id(),
                    ?device_range,
                    len = device_range.end - device_range.start,
                    "A",
                );
            }
            let device_range_len = device_range.end - device_range.start;
            allocated += device_range_len;

            let (head, tail) = buf.split_at_mut(device_range_len as usize);
            buf = tail;

            writes.push_back(async move {
                let len = head.len() as u64;
                Result::<_, Error>::Ok((
                    device_range.start,
                    len,
                    self.write_aligned(head.as_ref(), device_range.start).await?,
                ))
            });
        }

        self.store().logical_write_ops.fetch_add(1, Ordering::Relaxed);
        let ((mutations, checksums), deallocated) = try_join!(
            async {
                let mut current_range = 0..0;
                let mut mutations = Vec::new();
                let mut out_checksums = Vec::new();
                let mut ranges = ranges.iter();
                while let Some((mut device_offset, mut len, mut checksums)) =
                    writes.try_next().await?
                {
                    while len > 0 {
                        if current_range.end <= current_range.start {
                            current_range = ranges.next().unwrap().clone();
                        }
                        let chunk_len = std::cmp::min(len, current_range.end - current_range.start);
                        let tail = checksums.split_off((chunk_len / block_size) as usize);
                        if let Some(checksums) = checksums.maybe_as_ref() {
                            out_checksums.push((
                                device_offset..device_offset + chunk_len,
                                checksums.to_owned(),
                            ));
                        }
                        mutations.push(Mutation::merge_object(
                            ObjectKey::extent(
                                self.object_id(),
                                attribute_id,
                                current_range.start..current_range.start + chunk_len,
                            ),
                            ObjectValue::Extent(ExtentValue::new(
                                device_offset,
                                checksums.to_mode(),
                                key_id,
                            )),
                        ));
                        checksums = tail;
                        device_offset += chunk_len;
                        len -= chunk_len;
                        current_range.start += chunk_len;
                    }
                }
                Result::<_, Error>::Ok((mutations, out_checksums))
            },
            async {
                let mut deallocated = 0;
                for r in ranges {
                    deallocated +=
                        self.deallocate_old_extents(transaction, attribute_id, r.clone()).await?;
                }
                Result::<_, Error>::Ok(deallocated)
            }
        )?;
        for m in mutations {
            transaction.add(store_id, m);
        }
        for (r, c) in checksums {
            transaction.add_checksum(r, c, true);
        }
        self.update_allocated_size(transaction, allocated, deallocated).await
    }

    /// Write data to overwrite extents with the provided set of ranges. This makes a strong
    /// assumption that the ranges are actually going to be already allocated overwrite extents and
    /// will error out or do something wrong if they aren't. It also assumes the ranges passed to
    /// it are sorted.
    pub async fn multi_overwrite<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        attr_id: u64,
        ranges: &[Range<u64>],
        mut buf: MutableBufferRef<'_>,
    ) -> Result<(), Error> {
        if buf.is_empty() {
            return Ok(());
        }
        let block_size = self.block_size();
        let store = self.store();
        let tree = store.tree();
        let store_id = store.store_object_id();

        let key = self.get_key(None).await?;
        let key_id = if let Some(key) = &key {
            let mut slice = buf.as_mut_slice();
            for r in ranges {
                let l = r.end - r.start;
                let (head, tail) = slice.split_at_mut(l as usize);
                key.encrypt(r.start, head)?;
                slice = tail;
            }
            key.key_id()
        } else {
            0
        };

        let mut range_iter = ranges.into_iter();
        // There should be at least one range if the buffer has data in it
        let mut target_range = range_iter.next().unwrap().clone();
        let mut mutations = Vec::new();
        let writes = FuturesUnordered::new();

        let layer_set = tree.layer_set();
        let mut merger = layer_set.merger();
        let mut iter = merger
            .query(Query::FullRange(&ObjectKey::attribute(
                self.object_id(),
                attr_id,
                AttributeKey::Extent(ExtentKey::search_key_from_offset(target_range.start)),
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
                    value: ObjectValue::Extent(ExtentValue::Some { device_offset, mode, .. }),
                    ..
                }) if *object_id == self.object_id() && *attribute_id == attr_id => {
                    // If this extent ends before the target range starts (not possible on the
                    // first loop because of the query parameters but possible on further loops),
                    // advance until we find a the next one we care about.
                    if range.end <= target_range.start {
                        iter.advance().await?;
                        continue;
                    }
                    // The ranges passed to this function should already by allocated, so
                    // extent records should exist for them.
                    if range.start > target_range.start {
                        return Err(anyhow!(FxfsError::Inconsistent)).with_context(|| {
                            format!(
                            "multi_overwrite failed: target range ({}, {}) starts before first \
                            extent found at ({}, {})",
                            target_range.start,
                            target_range.end,
                            range.start,
                            range.end,
                        )
                        });
                    }
                    let mut bitmap = match mode {
                        ExtentMode::Raw | ExtentMode::Cow(_) => {
                            return Err(anyhow!(FxfsError::Inconsistent)).with_context(|| {
                                format!(
                                    "multi_overwrite failed: \
                            extent from ({}, {}) which overlaps target range ({}, {}) had the \
                            wrong extent mode",
                                    range.start, range.end, target_range.start, target_range.end,
                                )
                            })
                        }
                        ExtentMode::OverwritePartial(bitmap) => {
                            Some((bitmap.clone(), BitVec::from_elem(bitmap.len(), false)))
                        }
                        ExtentMode::Overwrite => None,
                    };
                    loop {
                        let offset_within_extent = target_range.start - range.start;
                        let write_device_offset = *device_offset + offset_within_extent;
                        let block_offset_within_extent = offset_within_extent / block_size;
                        let write_end = min(range.end, target_range.end);
                        let write_length = write_end - target_range.start;
                        let (current_buf, remaining_buf) = buf.split_at_mut(write_length as usize);
                        let checksum_ranges = if let Some((bitmap, write_bitmap)) = &mut bitmap {
                            let mut checksum_range_start = 0;
                            let mut checksum_range_type =
                                !bitmap.get(block_offset_within_extent as usize).unwrap();
                            let mut checksum_ranges = Vec::new();
                            let mut write_device_offset_start = write_device_offset;
                            for i in block_offset_within_extent
                                ..block_offset_within_extent + write_length / block_size
                            {
                                write_bitmap.set(i as usize, true);
                                if checksum_range_type == bitmap.get(i as usize).unwrap() {
                                    checksum_ranges.push((
                                        checksum_range_start as usize
                                            ..(i - block_offset_within_extent) as usize,
                                        write_device_offset_start
                                            ..write_device_offset + i * block_size,
                                        checksum_range_type,
                                    ));
                                    checksum_range_start = i - block_offset_within_extent;
                                    checksum_range_type = !checksum_range_type;
                                    write_device_offset_start =
                                        write_device_offset + i * block_size;
                                }
                            }
                            checksum_ranges
                        } else {
                            // If there is no bitmap, then the overwrite range is fully written to.
                            // However, we could still be within the journal flush window where one
                            // of the blocks was written to for the first time to put it in this
                            // state, so we still need to emit the checksums in case replay needs
                            // them.
                            vec![(
                                0..(write_length / block_size) as usize,
                                write_device_offset..(write_device_offset + write_length),
                                false,
                            )]
                        };
                        writes.push(async move {
                            let maybe_checksums = self
                                .write_aligned(current_buf.as_ref(), write_device_offset)
                                .await?;
                            Ok::<Vec<_>, Error>(
                                maybe_checksums
                                    .into_option()
                                    .into_iter()
                                    .flat_map(|checksums| {
                                        let mut out_checksums = Vec::new();
                                        for (checksum_range, write_range, first_write) in
                                            &checksum_ranges
                                        {
                                            out_checksums.push((
                                                write_range.clone(),
                                                checksums[checksum_range.clone()].to_vec(),
                                                *first_write,
                                            ))
                                        }
                                        out_checksums
                                    })
                                    .collect(),
                            )
                        });
                        buf = remaining_buf;
                        target_range.start += write_length;
                        if target_range.start == target_range.end {
                            match range_iter.next() {
                                None => break,
                                Some(next_range) => target_range = next_range.clone(),
                            }
                        }
                        if range.end <= target_range.start {
                            break;
                        }
                    }
                    if let Some((mut bitmap, write_bitmap)) = bitmap {
                        if bitmap.or(&write_bitmap) {
                            let mode = if bitmap.all() {
                                ExtentMode::Overwrite
                            } else {
                                ExtentMode::OverwritePartial(bitmap)
                            };
                            mutations.push(Mutation::merge_object(
                                ObjectKey::extent(self.object_id(), attr_id, range.clone()),
                                ObjectValue::Extent(ExtentValue::new(*device_offset, mode, key_id)),
                            ))
                        }
                    }
                    if target_range.start == target_range.end {
                        break;
                    }
                    iter.advance().await?;
                }
                // We've either run past the end of the existing extents or something is wrong with
                // the tree. The main section should break if it finishes the ranges, so either
                // case, this is an error.
                _ => bail!(anyhow!(FxfsError::Internal).context(
                    "found a non-extent object record while there were still ranges to process"
                )),
            }
        }

        let checksums = writes.try_collect::<Vec<_>>().await?;
        for (r, c, first_write) in checksums.into_iter().flatten() {
            transaction.add_checksum(r, c, first_write);
        }

        for m in mutations {
            transaction.add(store_id, m);
        }

        Ok(())
    }

    /// Writes an attribute that should not already exist and therefore does not require trimming.
    /// Breaks up the write into multiple transactions if `data.len()` is larger than `batch_size`.
    /// If writing the attribute requires multiple transactions, adds the attribute to the
    /// graveyard. The caller is responsible for removing the attribute from the graveyard when it
    /// commits the last transaction.  This always writes using a key wrapped with the volume data
    /// key.
    #[trace]
    pub async fn write_new_attr_in_batches<'a>(
        &'a self,
        transaction: &mut Transaction<'a>,
        attribute_id: u64,
        data: &[u8],
        batch_size: usize,
    ) -> Result<(), Error> {
        transaction.add(
            self.store().store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(self.object_id(), attribute_id, AttributeKey::Attribute),
                ObjectValue::attribute(data.len() as u64, false),
            ),
        );
        let chunks = data.chunks(batch_size);
        let num_chunks = chunks.len();
        if num_chunks > 1 {
            transaction.add(
                self.store().store_object_id,
                Mutation::replace_or_insert_object(
                    ObjectKey::graveyard_attribute_entry(
                        self.store().graveyard_directory_object_id(),
                        self.object_id(),
                        attribute_id,
                    ),
                    ObjectValue::Some,
                ),
            );
        }
        let mut start_offset = 0;
        for (i, chunk) in chunks.enumerate() {
            let rounded_len = round_up(chunk.len() as u64, self.block_size()).unwrap();
            let mut buffer = self.store().device.allocate_buffer(rounded_len as usize).await;
            let slice = buffer.as_mut_slice();
            slice[..chunk.len()].copy_from_slice(chunk);
            slice[chunk.len()..].fill(0);
            self.multi_write(
                transaction,
                attribute_id,
                Some(VOLUME_DATA_KEY_ID),
                &[start_offset..start_offset + rounded_len],
                buffer.as_mut(),
            )
            .await?;
            start_offset += rounded_len;
            // Do not commit the last chunk.
            if i < num_chunks - 1 {
                transaction.commit_and_continue().await?;
            }
        }
        Ok(())
    }

    /// Writes an entire attribute. Returns whether or not the attribute needs to continue being
    /// trimmed - if the new data is shorter than the old data, this will trim any extents beyond
    /// the end of the new size, but if there were too many for a single transaction, a commit
    /// needs to be made before trimming again, so the responsibility is left to the caller so as
    /// to not accidentally split the transaction when it's not in a consistent state.  This will
    /// write using the volume data key; the fscrypt key is not supported.
    pub async fn write_attr(
        &self,
        transaction: &mut Transaction<'_>,
        attribute_id: u64,
        data: &[u8],
    ) -> Result<NeedsTrim, Error> {
        let rounded_len = round_up(data.len() as u64, self.block_size()).unwrap();
        let store = self.store();
        let tree = store.tree();
        let should_trim = if let Some(item) = tree
            .find(&ObjectKey::attribute(self.object_id(), attribute_id, AttributeKey::Attribute))
            .await?
        {
            match item.value {
                ObjectValue::Attribute { size: _, has_overwrite_extents: true } => {
                    bail!(anyhow!(FxfsError::Inconsistent)
                        .context("write_attr on an attribute with overwrite extents"))
                }
                ObjectValue::Attribute { size, .. } => (data.len() as u64) < size,
                _ => bail!(FxfsError::Inconsistent),
            }
        } else {
            false
        };
        let mut buffer = self.store().device.allocate_buffer(rounded_len as usize).await;
        let slice = buffer.as_mut_slice();
        slice[..data.len()].copy_from_slice(data);
        slice[data.len()..].fill(0);
        self.multi_write(
            transaction,
            attribute_id,
            Some(VOLUME_DATA_KEY_ID),
            &[0..rounded_len],
            buffer.as_mut(),
        )
        .await?;
        transaction.add(
            self.store().store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(self.object_id(), attribute_id, AttributeKey::Attribute),
                ObjectValue::attribute(data.len() as u64, false),
            ),
        );
        if should_trim {
            self.shrink(transaction, attribute_id, data.len() as u64).await
        } else {
            Ok(NeedsTrim(false))
        }
    }

    pub async fn list_extended_attributes(&self) -> Result<Vec<Vec<u8>>, Error> {
        let layer_set = self.store().tree().layer_set();
        let mut merger = layer_set.merger();
        // Seek to the first extended attribute key for this object.
        let mut iter = merger
            .query(Query::FullRange(&ObjectKey::extended_attribute(self.object_id(), Vec::new())))
            .await?;
        let mut out = Vec::new();
        while let Some(item) = iter.get() {
            // Skip deleted extended attributes.
            if item.value != &ObjectValue::None {
                match item.key {
                    ObjectKey { object_id, data: ObjectKeyData::ExtendedAttribute { name } } => {
                        if self.object_id() != *object_id {
                            bail!(anyhow!(FxfsError::Inconsistent)
                                .context("list_extended_attributes: wrong object id"))
                        }
                        out.push(name.clone());
                    }
                    // Once we hit something that isn't an extended attribute key, we've gotten to
                    // the end.
                    _ => break,
                }
            }
            iter.advance().await?;
        }
        Ok(out)
    }

    /// Looks up the values for the extended attribute `fio::SELINUX_CONTEXT_NAME`, returning it
    /// if it is found inline. If it is not inline, it will request use of the
    /// `get_extended_attributes` method. If the entry doesn't exist at all, returns None.
    pub async fn get_inline_selinux_context(&self) -> Result<Option<fio::SelinuxContext>, Error> {
        // This optimization is only useful as long as the attribute is smaller than inline sizes.
        // Avoid reading the data out of the attributes.
        const_assert!(fio::MAX_SELINUX_CONTEXT_ATTRIBUTE_LEN as usize <= MAX_INLINE_XATTR_SIZE);
        let item = match self
            .store()
            .tree()
            .find(&ObjectKey::extended_attribute(
                self.object_id(),
                fio::SELINUX_CONTEXT_NAME.into(),
            ))
            .await?
        {
            Some(item) => item,
            None => return Ok(None),
        };
        match item.value {
            ObjectValue::ExtendedAttribute(ExtendedAttributeValue::Inline(value)) => {
                Ok(Some(fio::SelinuxContext::Data(value)))
            }
            ObjectValue::ExtendedAttribute(ExtendedAttributeValue::AttributeId(_)) => {
                Ok(Some(fio::SelinuxContext::UseExtendedAttributes(fio::EmptyStruct {})))
            }
            _ => {
                bail!(anyhow!(FxfsError::Inconsistent)
                    .context("get_inline_extended_attribute: Expected ExtendedAttribute value"))
            }
        }
    }

    pub async fn get_extended_attribute(&self, name: Vec<u8>) -> Result<Vec<u8>, Error> {
        let item = self
            .store()
            .tree()
            .find(&ObjectKey::extended_attribute(self.object_id(), name))
            .await?
            .ok_or(FxfsError::NotFound)?;
        match item.value {
            ObjectValue::ExtendedAttribute(ExtendedAttributeValue::Inline(value)) => Ok(value),
            ObjectValue::ExtendedAttribute(ExtendedAttributeValue::AttributeId(id)) => {
                Ok(self.read_attr(id).await?.ok_or(FxfsError::Inconsistent)?.into_vec())
            }
            _ => {
                bail!(anyhow!(FxfsError::Inconsistent)
                    .context("get_extended_attribute: Expected ExtendedAttribute value"))
            }
        }
    }

    pub async fn set_extended_attribute(
        &self,
        name: Vec<u8>,
        value: Vec<u8>,
        mode: SetExtendedAttributeMode,
    ) -> Result<(), Error> {
        let store = self.store();
        let fs = store.filesystem();
        // NB: We need to take this lock before we potentially look up the value to prevent racing
        // with another set.
        let keys = lock_keys![LockKey::object(store.store_object_id(), self.object_id())];
        let mut transaction = fs.new_transaction(keys, Options::default()).await?;
        self.set_extended_attribute_impl(name, value, mode, &mut transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn set_extended_attribute_impl(
        &self,
        name: Vec<u8>,
        value: Vec<u8>,
        mode: SetExtendedAttributeMode,
        transaction: &mut Transaction<'_>,
    ) -> Result<(), Error> {
        ensure!(name.len() <= MAX_XATTR_NAME_SIZE, FxfsError::TooBig);
        ensure!(value.len() <= MAX_XATTR_VALUE_SIZE, FxfsError::TooBig);
        let tree = self.store().tree();
        let object_key = ObjectKey::extended_attribute(self.object_id(), name);

        let existing_attribute_id = {
            let (found, existing_attribute_id) = match tree.find(&object_key).await? {
                None => (false, None),
                Some(Item { value, .. }) => (
                    true,
                    match value {
                        ObjectValue::ExtendedAttribute(ExtendedAttributeValue::Inline(..)) => None,
                        ObjectValue::ExtendedAttribute(ExtendedAttributeValue::AttributeId(id)) => {
                            Some(id)
                        }
                        _ => bail!(anyhow!(FxfsError::Inconsistent)
                            .context("expected extended attribute value")),
                    },
                ),
            };
            match mode {
                SetExtendedAttributeMode::Create if found => {
                    bail!(FxfsError::AlreadyExists)
                }
                SetExtendedAttributeMode::Replace if !found => {
                    bail!(FxfsError::NotFound)
                }
                _ => (),
            }
            existing_attribute_id
        };

        if let Some(attribute_id) = existing_attribute_id {
            // If we already have an attribute id allocated for this extended attribute, we always
            // use it, even if the value has shrunk enough to be stored inline. We don't need to
            // worry about trimming here for the same reason we don't need to worry about it when
            // we delete xattrs - they simply aren't large enough to ever need more than one
            // transaction.
            let _ = self.write_attr(transaction, attribute_id, &value).await?;
        } else if value.len() <= MAX_INLINE_XATTR_SIZE {
            transaction.add(
                self.store().store_object_id(),
                Mutation::replace_or_insert_object(
                    object_key,
                    ObjectValue::inline_extended_attribute(value),
                ),
            );
        } else {
            // If there isn't an existing attribute id and we are going to store the value in
            // an attribute, find the next empty attribute id in the range. We search for fxfs
            // attribute records specifically, instead of the extended attribute records, because
            // even if the extended attribute record is removed the attribute may not be fully
            // trimmed yet.
            let mut attribute_id = EXTENDED_ATTRIBUTE_RANGE_START;
            let layer_set = tree.layer_set();
            let mut merger = layer_set.merger();
            let key = ObjectKey::attribute(self.object_id(), attribute_id, AttributeKey::Attribute);
            let mut iter = merger.query(Query::FullRange(&key)).await?;
            loop {
                match iter.get() {
                    // None means the key passed to seek wasn't found. That means the first
                    // attribute is available and we can just stop right away.
                    None => break,
                    Some(ItemRef {
                        key: ObjectKey { object_id, data: ObjectKeyData::Attribute(attr_id, _) },
                        value,
                        ..
                    }) if *object_id == self.object_id() => {
                        if matches!(value, ObjectValue::None) {
                            // This attribute was once used but is now deleted, so it's safe to use
                            // again.
                            break;
                        }
                        if attribute_id < *attr_id {
                            // We found a gap - use it.
                            break;
                        } else if attribute_id == *attr_id {
                            // This attribute id is in use, try the next one.
                            attribute_id += 1;
                            if attribute_id == EXTENDED_ATTRIBUTE_RANGE_END {
                                bail!(FxfsError::NoSpace);
                            }
                        }
                        // If we don't hit either of those cases, we are still moving through the
                        // extent keys for the current attribute, so just keep advancing until the
                        // attribute id changes.
                    }
                    // As we are working our way through the iterator, if we hit anything that
                    // doesn't have our object id or attribute key data, we've gone past the end of
                    // this section and can stop.
                    _ => break,
                }
                iter.advance().await?;
            }

            // We know this won't need trimming because it's a new attribute.
            let _ = self.write_attr(transaction, attribute_id, &value).await?;
            transaction.add(
                self.store().store_object_id(),
                Mutation::replace_or_insert_object(
                    object_key,
                    ObjectValue::extended_attribute(attribute_id),
                ),
            );
        }

        Ok(())
    }

    pub async fn remove_extended_attribute(&self, name: Vec<u8>) -> Result<(), Error> {
        let store = self.store();
        let tree = store.tree();
        let object_key = ObjectKey::extended_attribute(self.object_id(), name);

        // NB: The API says we have to return an error if the attribute doesn't exist, so we have
        // to look it up first to make sure we have a record of it before we delete it. Make sure
        // we take a lock and make a transaction before we do so we don't race with other
        // operations.
        let keys = lock_keys![LockKey::object(store.store_object_id(), self.object_id())];
        let mut transaction = store.filesystem().new_transaction(keys, Options::default()).await?;

        let attribute_to_delete =
            match tree.find(&object_key).await?.ok_or(FxfsError::NotFound)?.value {
                ObjectValue::ExtendedAttribute(ExtendedAttributeValue::AttributeId(id)) => Some(id),
                ObjectValue::ExtendedAttribute(ExtendedAttributeValue::Inline(..)) => None,
                _ => {
                    bail!(anyhow!(FxfsError::Inconsistent)
                        .context("remove_extended_attribute: Expected ExtendedAttribute value"))
                }
            };

        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(object_key, ObjectValue::None),
        );

        // If the attribute wasn't stored inline, we need to deallocate all the extents too. This
        // would normally need to interact with the graveyard for correctness - if there are too
        // many extents to delete to fit in a single transaction then we could potentially have
        // consistency issues. However, the maximum size of an extended attribute is small enough
        // that it will never come close to that limit even in the worst case, so we just delete
        // everything in one shot.
        if let Some(attribute_id) = attribute_to_delete {
            let trim_result = store
                .trim_some(
                    &mut transaction,
                    self.object_id(),
                    attribute_id,
                    TrimMode::FromOffset(0),
                )
                .await?;
            // In case you didn't read the comment above - this should not be used to delete
            // arbitrary attributes!
            assert_matches!(trim_result, TrimResult::Done(_));
            transaction.add(
                store.store_object_id(),
                Mutation::replace_or_insert_object(
                    ObjectKey::attribute(self.object_id, attribute_id, AttributeKey::Attribute),
                    ObjectValue::None,
                ),
            );
        }

        transaction.commit().await?;
        Ok(())
    }

    /// Returns a future that will pre-fetches the keys so as to avoid paying the performance
    /// penalty later. Must ensure that the object is not removed before the future completes.
    pub fn pre_fetch_keys(&self) -> Option<impl Future<Output = ()>> {
        if let Encryption::CachedKeys = self.encryption {
            let owner = self.owner.clone();
            let object_id = self.object_id;
            Some(async move {
                let store = owner.as_ref().as_ref();
                if let Some(crypt) = store.crypt() {
                    let _ = store
                        .key_manager
                        .get_keys(
                            object_id,
                            crypt.as_ref(),
                            &mut Some(store.get_keys(object_id)),
                            /* permanent= */ false,
                            /* force= */ false,
                        )
                        .await;
                }
            })
        } else {
            None
        }
    }
}

impl<S: HandleOwner> Drop for StoreObjectHandle<S> {
    fn drop(&mut self) {
        if self.is_encrypted() {
            let _ = self.store().key_manager.remove(self.object_id);
        }
    }
}

/// When truncating an object, sometimes it might not be possible to complete the transaction in a
/// single transaction, in which case the caller needs to finish trimming the object in subsequent
/// transactions (by calling ObjectStore::trim).
#[must_use]
pub struct NeedsTrim(pub bool);

#[cfg(test)]
mod tests {
    use crate::errors::FxfsError;
    use crate::filesystem::{FxFilesystem, OpenFxFilesystem};
    use crate::object_handle::ObjectHandle;
    use crate::object_store::data_object_handle::WRITE_ATTR_BATCH_SIZE;
    use crate::object_store::transaction::{lock_keys, Mutation, Options};
    use crate::object_store::{
        AttributeKey, DataObjectHandle, Directory, HandleOptions, LockKey, ObjectKey, ObjectStore,
        ObjectValue, SetExtendedAttributeMode, StoreObjectHandle, FSVERITY_MERKLE_ATTRIBUTE_ID,
    };
    use fuchsia_async as fasync;
    use futures::join;
    use fxfs_insecure_crypto::InsecureCrypt;
    use std::sync::Arc;
    use storage_device::fake_device::FakeDevice;
    use storage_device::DeviceHolder;

    const TEST_DEVICE_BLOCK_SIZE: u32 = 512;
    const TEST_OBJECT_NAME: &str = "foo";

    fn is_error(actual: anyhow::Error, expected: FxfsError) {
        assert_eq!(*actual.root_cause().downcast_ref::<FxfsError>().unwrap(), expected)
    }

    async fn test_filesystem() -> OpenFxFilesystem {
        let device = DeviceHolder::new(FakeDevice::new(16384, TEST_DEVICE_BLOCK_SIZE));
        FxFilesystem::new_empty(device).await.expect("new_empty failed")
    }

    async fn test_filesystem_and_empty_object() -> (OpenFxFilesystem, DataObjectHandle<ObjectStore>)
    {
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

        let object = ObjectStore::create_object(
            &store,
            &mut transaction,
            HandleOptions::default(),
            Some(&InsecureCrypt::new()),
            None,
        )
        .await
        .expect("create_object failed");

        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        root_directory
            .add_child_file(&mut transaction, TEST_OBJECT_NAME, &object)
            .await
            .expect("add_child_file failed");

        transaction.commit().await.expect("commit failed");

        (fs, object)
    }

    #[fuchsia::test(threads = 3)]
    async fn extended_attribute_double_remove() {
        // This test is intended to trip a potential race condition in remove. Removing an
        // attribute that doesn't exist is an error, so we need to check before we remove, but if
        // we aren't careful, two parallel removes might both succeed in the check and then both
        // remove the value.
        let (fs, object) = test_filesystem_and_empty_object().await;
        let basic = Arc::new(StoreObjectHandle::new(
            object.owner().clone(),
            object.object_id(),
            /* permanent_keys: */ false,
            HandleOptions::default(),
            false,
        ));
        let basic_a = basic.clone();
        let basic_b = basic.clone();

        basic
            .set_extended_attribute(
                b"security.selinux".to_vec(),
                b"bar".to_vec(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .expect("failed to set attribute");

        // Try to remove the attribute twice at the same time. One should succeed in the race and
        // return Ok, and the other should fail the race and return NOT_FOUND.
        let a_task = fasync::Task::spawn(async move {
            basic_a.remove_extended_attribute(b"security.selinux".to_vec()).await
        });
        let b_task = fasync::Task::spawn(async move {
            basic_b.remove_extended_attribute(b"security.selinux".to_vec()).await
        });
        match join!(a_task, b_task) {
            (Ok(()), Ok(())) => panic!("both remove calls succeeded"),
            (Err(_), Err(_)) => panic!("both remove calls failed"),

            (Ok(()), Err(e)) => is_error(e, FxfsError::NotFound),
            (Err(e), Ok(())) => is_error(e, FxfsError::NotFound),
        }

        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test(threads = 3)]
    async fn extended_attribute_double_create() {
        // This test is intended to trip a potential race in set when using the create flag,
        // similar to above. If the create mode is set, we need to check that the attribute isn't
        // already created, but if two parallel creates both succeed in that check, and we aren't
        // careful with locking, they will both succeed and one will overwrite the other.
        let (fs, object) = test_filesystem_and_empty_object().await;
        let basic = Arc::new(StoreObjectHandle::new(
            object.owner().clone(),
            object.object_id(),
            /* permanent_keys: */ false,
            HandleOptions::default(),
            false,
        ));
        let basic_a = basic.clone();
        let basic_b = basic.clone();

        // Try to set the attribute twice at the same time. One should succeed in the race and
        // return Ok, and the other should fail the race and return ALREADY_EXISTS.
        let a_task = fasync::Task::spawn(async move {
            basic_a
                .set_extended_attribute(
                    b"security.selinux".to_vec(),
                    b"one".to_vec(),
                    SetExtendedAttributeMode::Create,
                )
                .await
        });
        let b_task = fasync::Task::spawn(async move {
            basic_b
                .set_extended_attribute(
                    b"security.selinux".to_vec(),
                    b"two".to_vec(),
                    SetExtendedAttributeMode::Create,
                )
                .await
        });
        match join!(a_task, b_task) {
            (Ok(()), Ok(())) => panic!("both set calls succeeded"),
            (Err(_), Err(_)) => panic!("both set calls failed"),

            (Ok(()), Err(e)) => {
                assert_eq!(
                    basic
                        .get_extended_attribute(b"security.selinux".to_vec())
                        .await
                        .expect("failed to get xattr"),
                    b"one"
                );
                is_error(e, FxfsError::AlreadyExists);
            }
            (Err(e), Ok(())) => {
                assert_eq!(
                    basic
                        .get_extended_attribute(b"security.selinux".to_vec())
                        .await
                        .expect("failed to get xattr"),
                    b"two"
                );
                is_error(e, FxfsError::AlreadyExists);
            }
        }

        fs.close().await.expect("Close failed");
    }

    struct TestAttr {
        name: Vec<u8>,
        value: Vec<u8>,
    }

    impl TestAttr {
        fn new(name: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Self {
            Self { name: name.as_ref().to_vec(), value: value.as_ref().to_vec() }
        }
        fn name(&self) -> Vec<u8> {
            self.name.clone()
        }
        fn value(&self) -> Vec<u8> {
            self.value.clone()
        }
    }

    #[fuchsia::test]
    async fn extended_attributes() {
        let (fs, object) = test_filesystem_and_empty_object().await;

        let test_attr = TestAttr::new(b"security.selinux", b"foo");

        assert_eq!(object.list_extended_attributes().await.unwrap(), Vec::<Vec<u8>>::new());
        is_error(
            object.get_extended_attribute(test_attr.name()).await.unwrap_err(),
            FxfsError::NotFound,
        );

        object
            .set_extended_attribute(
                test_attr.name(),
                test_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(object.list_extended_attributes().await.unwrap(), vec![test_attr.name()]);
        assert_eq!(
            object.get_extended_attribute(test_attr.name()).await.unwrap(),
            test_attr.value()
        );

        object.remove_extended_attribute(test_attr.name()).await.unwrap();
        assert_eq!(object.list_extended_attributes().await.unwrap(), Vec::<Vec<u8>>::new());
        is_error(
            object.get_extended_attribute(test_attr.name()).await.unwrap_err(),
            FxfsError::NotFound,
        );

        // Make sure we can object the same attribute being set again.
        object
            .set_extended_attribute(
                test_attr.name(),
                test_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(object.list_extended_attributes().await.unwrap(), vec![test_attr.name()]);
        assert_eq!(
            object.get_extended_attribute(test_attr.name()).await.unwrap(),
            test_attr.value()
        );

        object.remove_extended_attribute(test_attr.name()).await.unwrap();
        assert_eq!(object.list_extended_attributes().await.unwrap(), Vec::<Vec<u8>>::new());
        is_error(
            object.get_extended_attribute(test_attr.name()).await.unwrap_err(),
            FxfsError::NotFound,
        );

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn large_extended_attribute() {
        let (fs, object) = test_filesystem_and_empty_object().await;

        let test_attr = TestAttr::new(b"security.selinux", vec![3u8; 300]);

        object
            .set_extended_attribute(
                test_attr.name(),
                test_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            object.get_extended_attribute(test_attr.name()).await.unwrap(),
            test_attr.value()
        );

        // Probe the fxfs attributes to make sure it did the expected thing. This relies on inside
        // knowledge of how the attribute id is chosen.
        assert_eq!(
            object
                .read_attr(64)
                .await
                .expect("read_attr failed")
                .expect("read_attr returned none")
                .into_vec(),
            test_attr.value()
        );

        object.remove_extended_attribute(test_attr.name()).await.unwrap();
        is_error(
            object.get_extended_attribute(test_attr.name()).await.unwrap_err(),
            FxfsError::NotFound,
        );

        // Make sure we can object the same attribute being set again.
        object
            .set_extended_attribute(
                test_attr.name(),
                test_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            object.get_extended_attribute(test_attr.name()).await.unwrap(),
            test_attr.value()
        );
        object.remove_extended_attribute(test_attr.name()).await.unwrap();
        is_error(
            object.get_extended_attribute(test_attr.name()).await.unwrap_err(),
            FxfsError::NotFound,
        );

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn multiple_extended_attributes() {
        let (fs, object) = test_filesystem_and_empty_object().await;

        let attrs = [
            TestAttr::new(b"security.selinux", b"foo"),
            TestAttr::new(b"large.attribute", vec![3u8; 300]),
            TestAttr::new(b"an.attribute", b"asdf"),
            TestAttr::new(b"user.big", vec![5u8; 288]),
            TestAttr::new(b"user.tiny", b"smol"),
            TestAttr::new(b"this string doesn't matter", b"the quick brown fox etc"),
            TestAttr::new(b"also big", vec![7u8; 500]),
            TestAttr::new(b"all.ones", vec![1u8; 11111]),
        ];

        for i in 0..attrs.len() {
            object
                .set_extended_attribute(
                    attrs[i].name(),
                    attrs[i].value(),
                    SetExtendedAttributeMode::Set,
                )
                .await
                .unwrap();
            assert_eq!(
                object.get_extended_attribute(attrs[i].name()).await.unwrap(),
                attrs[i].value()
            );
        }

        for i in 0..attrs.len() {
            // Make sure expected attributes are still available.
            let mut found_attrs = object.list_extended_attributes().await.unwrap();
            let mut expected_attrs: Vec<Vec<u8>> = attrs.iter().skip(i).map(|a| a.name()).collect();
            found_attrs.sort();
            expected_attrs.sort();
            assert_eq!(found_attrs, expected_attrs);
            for j in i..attrs.len() {
                assert_eq!(
                    object.get_extended_attribute(attrs[j].name()).await.unwrap(),
                    attrs[j].value()
                );
            }

            object.remove_extended_attribute(attrs[i].name()).await.expect("failed to remove");
            is_error(
                object.get_extended_attribute(attrs[i].name()).await.unwrap_err(),
                FxfsError::NotFound,
            );
        }

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn multiple_extended_attributes_delete() {
        let (fs, object) = test_filesystem_and_empty_object().await;
        let store = object.owner().clone();

        let attrs = [
            TestAttr::new(b"security.selinux", b"foo"),
            TestAttr::new(b"large.attribute", vec![3u8; 300]),
            TestAttr::new(b"an.attribute", b"asdf"),
            TestAttr::new(b"user.big", vec![5u8; 288]),
            TestAttr::new(b"user.tiny", b"smol"),
            TestAttr::new(b"this string doesn't matter", b"the quick brown fox etc"),
            TestAttr::new(b"also big", vec![7u8; 500]),
            TestAttr::new(b"all.ones", vec![1u8; 11111]),
        ];

        for i in 0..attrs.len() {
            object
                .set_extended_attribute(
                    attrs[i].name(),
                    attrs[i].value(),
                    SetExtendedAttributeMode::Set,
                )
                .await
                .unwrap();
            assert_eq!(
                object.get_extended_attribute(attrs[i].name()).await.unwrap(),
                attrs[i].value()
            );
        }

        // Unlink the file
        let root_directory =
            Directory::open(object.owner(), object.store().root_directory_object_id())
                .await
                .expect("open failed");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(store.store_object_id(), store.root_directory_object_id()),
                    LockKey::object(store.store_object_id(), object.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        crate::object_store::directory::replace_child(
            &mut transaction,
            None,
            (&root_directory, TEST_OBJECT_NAME),
        )
        .await
        .expect("replace_child failed");
        transaction.commit().await.unwrap();
        store.tombstone_object(object.object_id(), Options::default()).await.unwrap();

        crate::fsck::fsck(fs.clone()).await.unwrap();

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn extended_attribute_changing_sizes() {
        let (fs, object) = test_filesystem_and_empty_object().await;

        let test_name = b"security.selinux";
        let test_small_attr = TestAttr::new(test_name, b"smol");
        let test_large_attr = TestAttr::new(test_name, vec![3u8; 300]);

        object
            .set_extended_attribute(
                test_small_attr.name(),
                test_small_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            object.get_extended_attribute(test_small_attr.name()).await.unwrap(),
            test_small_attr.value()
        );

        // With a small attribute, we don't expect it to write to an fxfs attribute.
        assert!(object.read_attr(64).await.expect("read_attr failed").is_none());

        crate::fsck::fsck(fs.clone()).await.unwrap();

        object
            .set_extended_attribute(
                test_large_attr.name(),
                test_large_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            object.get_extended_attribute(test_large_attr.name()).await.unwrap(),
            test_large_attr.value()
        );

        // Once the value is above the threshold, we expect it to get upgraded to an fxfs
        // attribute.
        assert_eq!(
            object
                .read_attr(64)
                .await
                .expect("read_attr failed")
                .expect("read_attr returned none")
                .into_vec(),
            test_large_attr.value()
        );

        crate::fsck::fsck(fs.clone()).await.unwrap();

        object
            .set_extended_attribute(
                test_small_attr.name(),
                test_small_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            object.get_extended_attribute(test_small_attr.name()).await.unwrap(),
            test_small_attr.value()
        );

        // Even though we are back under the threshold, we still expect it to be stored in an fxfs
        // attribute, because we don't downgrade to inline once we've allocated one.
        assert_eq!(
            object
                .read_attr(64)
                .await
                .expect("read_attr failed")
                .expect("read_attr returned none")
                .into_vec(),
            test_small_attr.value()
        );

        crate::fsck::fsck(fs.clone()).await.unwrap();

        object.remove_extended_attribute(test_small_attr.name()).await.expect("failed to remove");

        crate::fsck::fsck(fs.clone()).await.unwrap();

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn extended_attribute_max_size() {
        let (fs, object) = test_filesystem_and_empty_object().await;

        let test_attr = TestAttr::new(
            vec![3u8; super::MAX_XATTR_NAME_SIZE],
            vec![1u8; super::MAX_XATTR_VALUE_SIZE],
        );

        object
            .set_extended_attribute(
                test_attr.name(),
                test_attr.value(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();
        assert_eq!(
            object.get_extended_attribute(test_attr.name()).await.unwrap(),
            test_attr.value()
        );
        assert_eq!(object.list_extended_attributes().await.unwrap(), vec![test_attr.name()]);
        object.remove_extended_attribute(test_attr.name()).await.unwrap();

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn extended_attribute_remove_then_create() {
        let (fs, object) = test_filesystem_and_empty_object().await;

        let test_attr = TestAttr::new(
            vec![3u8; super::MAX_XATTR_NAME_SIZE],
            vec![1u8; super::MAX_XATTR_VALUE_SIZE],
        );

        object
            .set_extended_attribute(
                test_attr.name(),
                test_attr.value(),
                SetExtendedAttributeMode::Create,
            )
            .await
            .unwrap();
        fs.journal().compact().await.unwrap();
        object.remove_extended_attribute(test_attr.name()).await.unwrap();
        object
            .set_extended_attribute(
                test_attr.name(),
                test_attr.value(),
                SetExtendedAttributeMode::Create,
            )
            .await
            .unwrap();

        assert_eq!(
            object.get_extended_attribute(test_attr.name()).await.unwrap(),
            test_attr.value()
        );

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn large_extended_attribute_max_number() {
        let (fs, object) = test_filesystem_and_empty_object().await;

        let max_xattrs =
            super::EXTENDED_ATTRIBUTE_RANGE_END - super::EXTENDED_ATTRIBUTE_RANGE_START;
        for i in 0..max_xattrs {
            let test_attr = TestAttr::new(format!("{}", i).as_bytes(), vec![0x3; 300]);
            object
                .set_extended_attribute(
                    test_attr.name(),
                    test_attr.value(),
                    SetExtendedAttributeMode::Set,
                )
                .await
                .unwrap_or_else(|_| panic!("failed to set xattr number {}", i));
        }

        // That should have taken up all the attributes we've allocated to extended attributes, so
        // this one should return ERR_NO_SPACE.
        match object
            .set_extended_attribute(
                b"one.too.many".to_vec(),
                vec![0x3; 300],
                SetExtendedAttributeMode::Set,
            )
            .await
        {
            Ok(()) => panic!("set should not succeed"),
            Err(e) => is_error(e, FxfsError::NoSpace),
        }

        // But inline attributes don't need an attribute number, so it should work fine.
        object
            .set_extended_attribute(
                b"this.is.okay".to_vec(),
                b"small value".to_vec(),
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();

        // And updating existing ones should be okay.
        object
            .set_extended_attribute(b"11".to_vec(), vec![0x4; 300], SetExtendedAttributeMode::Set)
            .await
            .unwrap();
        object
            .set_extended_attribute(
                b"12".to_vec(),
                vec![0x1; 300],
                SetExtendedAttributeMode::Replace,
            )
            .await
            .unwrap();

        // And we should be able to remove an attribute and set another one.
        object.remove_extended_attribute(b"5".to_vec()).await.unwrap();
        object
            .set_extended_attribute(
                b"new attr".to_vec(),
                vec![0x3; 300],
                SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap();

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn write_attr_trims_beyond_new_end() {
        // When writing, multi_write will deallocate old extents that overlap with the new data,
        // but it doesn't trim anything beyond that, since it doesn't know what the total size will
        // be. write_attr does know, because it writes the whole attribute at once, so we need to
        // make sure it cleans up properly.
        let (fs, object) = test_filesystem_and_empty_object().await;

        let block_size = fs.block_size();
        let buf_size = block_size * 2;
        let attribute_id = 10;

        let mut transaction = (*object).new_transaction(attribute_id).await.unwrap();
        let mut buffer = object.allocate_buffer(buf_size as usize).await;
        buffer.as_mut_slice().fill(3);
        // Writing two separate ranges, even if they are contiguous, forces them to be separate
        // extent records.
        object
            .multi_write(
                &mut transaction,
                attribute_id,
                &[0..block_size, block_size..block_size * 2],
                buffer.as_mut(),
            )
            .await
            .unwrap();
        transaction.add(
            object.store().store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(object.object_id(), attribute_id, AttributeKey::Attribute),
                ObjectValue::attribute(block_size * 2, false),
            ),
        );
        transaction.commit().await.unwrap();

        crate::fsck::fsck(fs.clone()).await.unwrap();

        let mut transaction = (*object).new_transaction(attribute_id).await.unwrap();
        let needs_trim = (*object)
            .write_attr(&mut transaction, attribute_id, &vec![3u8; block_size as usize])
            .await
            .unwrap();
        assert!(!needs_trim.0);
        transaction.commit().await.unwrap();

        crate::fsck::fsck(fs.clone()).await.unwrap();

        fs.close().await.expect("close failed");
    }

    #[fuchsia::test]
    async fn write_new_attr_in_batches_multiple_txns() {
        let (fs, object) = test_filesystem_and_empty_object().await;
        let merkle_tree = vec![1; 3 * WRITE_ATTR_BATCH_SIZE];
        let mut transaction =
            (*object).new_transaction(FSVERITY_MERKLE_ATTRIBUTE_ID).await.unwrap();
        object
            .write_new_attr_in_batches(
                &mut transaction,
                FSVERITY_MERKLE_ATTRIBUTE_ID,
                &merkle_tree,
                WRITE_ATTR_BATCH_SIZE,
            )
            .await
            .expect("failed to write merkle attribute");

        transaction.add(
            object.store().store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::graveyard_attribute_entry(
                    object.store().graveyard_directory_object_id(),
                    object.object_id(),
                    FSVERITY_MERKLE_ATTRIBUTE_ID,
                ),
                ObjectValue::None,
            ),
        );
        transaction.commit().await.unwrap();
        assert_eq!(
            object.read_attr(FSVERITY_MERKLE_ATTRIBUTE_ID).await.expect("read_attr failed"),
            Some(merkle_tree.into())
        );

        fs.close().await.expect("close failed");
    }

    // Running on target only, to use fake time features in the executor.
    #[cfg(target_os = "fuchsia")]
    #[fuchsia::test(allow_stalls = false)]
    async fn test_watchdog() {
        use super::Watchdog;
        use fuchsia_async::{MonotonicDuration, MonotonicInstant, TestExecutor};
        use std::sync::mpsc::channel;

        TestExecutor::advance_to(make_time(0)).await;
        let (sender, receiver) = channel();

        fn make_time(time_secs: i64) -> MonotonicInstant {
            MonotonicInstant::from_nanos(0) + MonotonicDuration::from_seconds(time_secs)
        }

        {
            let _watchdog = Watchdog::new(10, move |count| {
                sender.send(count).expect("Sending value");
            });

            // Too early.
            TestExecutor::advance_to(make_time(5)).await;
            receiver.try_recv().expect_err("Should not have message");

            // First message.
            TestExecutor::advance_to(make_time(10)).await;
            assert_eq!(1, receiver.recv().expect("Receiving"));

            // Too early for the next.
            TestExecutor::advance_to(make_time(15)).await;
            receiver.try_recv().expect_err("Should not have message");

            // Missed one. They'll be spooled up.
            TestExecutor::advance_to(make_time(30)).await;
            assert_eq!(2, receiver.recv().expect("Receiving"));
            assert_eq!(3, receiver.recv().expect("Receiving"));
        }

        // Watchdog is dropped, nothing should trigger.
        TestExecutor::advance_to(make_time(100)).await;
        receiver.recv().expect_err("Watchdog should be gone");
    }
}
