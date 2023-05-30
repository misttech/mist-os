// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        errors::FxfsError,
        lsm_tree::{
            merge::{Merger, MergerIterator},
            types::{ItemRef, LayerIterator},
        },
        object_handle::{ObjectHandle, ObjectProperties, INVALID_OBJECT_ID},
        object_store::{
            object_record::{
                ObjectAttributes, ObjectItem, ObjectKey, ObjectKeyData, ObjectKind, ObjectValue,
                PosixAttributes, Timestamp,
            },
            transaction::{LockKey, Mutation, Options, Transaction},
            BasicObjectHandle, HandleOptions, HandleOwner, ObjectStore, StoreObjectHandle,
        },
        trace_duration,
    },
    anyhow::{anyhow, bail, ensure, Error},
    std::{
        fmt,
        ops::Bound,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    },
};

// ObjectDescriptor is exposed in Directory::lookup.
pub use crate::object_store::object_record::ObjectDescriptor;

/// A directory stores name to child object mappings.
pub struct Directory<S> {
    owner: Arc<S>,
    object_id: u64,
    is_deleted: AtomicBool,
}

impl<S: HandleOwner> Directory<S> {
    fn new(owner: Arc<S>, object_id: u64) -> Self {
        Directory { owner, object_id, is_deleted: AtomicBool::new(false) }
    }

    pub fn object_id(&self) -> u64 {
        return self.object_id;
    }

    pub fn owner(&self) -> &Arc<S> {
        &self.owner
    }

    pub fn store(&self) -> &ObjectStore {
        self.owner.as_ref().as_ref()
    }

    pub fn is_deleted(&self) -> bool {
        self.is_deleted.load(Ordering::Relaxed)
    }

    pub fn set_deleted(&self) {
        self.is_deleted.store(true, Ordering::Relaxed);
    }

    pub async fn create(
        transaction: &mut Transaction<'_>,
        owner: &Arc<S>,
        posix_attributes: Option<PosixAttributes>,
    ) -> Result<Directory<S>, Error> {
        let store = owner.as_ref().as_ref();
        let object_id = store.get_next_object_id().await?;
        let now = Timestamp::now();
        transaction.add(
            store.store_object_id,
            Mutation::insert_object(
                ObjectKey::object(object_id),
                ObjectValue::Object {
                    kind: ObjectKind::Directory { sub_dirs: 0 },
                    attributes: ObjectAttributes {
                        creation_time: now.clone(),
                        modification_time: now,
                        project_id: 0,
                        posix_attributes,
                    },
                },
            ),
        );
        Ok(Directory::new(owner.clone(), object_id))
    }

    pub async fn open(owner: &Arc<S>, object_id: u64) -> Result<Directory<S>, Error> {
        trace_duration!("Directory::open");
        let store = owner.as_ref().as_ref();
        match store.tree.find(&ObjectKey::object(object_id)).await?.ok_or(FxfsError::NotFound)? {
            ObjectItem {
                value: ObjectValue::Object { kind: ObjectKind::Directory { .. }, .. },
                ..
            } => Ok(Directory::new(owner.clone(), object_id)),
            ObjectItem { value: ObjectValue::None, .. } => bail!(FxfsError::NotFound),
            _ => bail!(FxfsError::NotDir),
        }
    }

    /// Opens a directory. The caller is responsible for ensuring that the object exists and is a
    /// directory.
    pub fn open_unchecked(owner: Arc<S>, object_id: u64) -> Self {
        Self::new(owner, object_id)
    }

    /// Acquires a transaction with the appropriate locks to replace |name|. Returns the
    /// transaction, as well as the ID and type of the child. If the child doesn't exist then a
    /// transaction is returned with a lock only on the parent and None for the target info so that
    /// the transaction can be executed with the confidence that the target doesn't exist.
    ///
    /// We need to lock |self|, but also the child if it exists. When it is a directory the lock
    /// prevents entries being added at the same time. When it is a file needs to be able to
    /// decrement the reference count.
    pub async fn acquire_transaction_for_replace<'a>(
        &self,
        extra_keys: &[LockKey],
        name: &str,
        borrow_metadata_space: bool,
    ) -> Result<(Transaction<'a>, Option<(u64, ObjectDescriptor)>), Error> {
        // Since we don't know the child object ID until we've looked up the child, we need to loop
        // until we have acquired a lock on a child whose ID is the same as it was in the last
        // iteration.
        //
        // Note that the returned transaction may lock more objects than is necessary (for example,
        // if the child "foo" was first a directory, then was renamed to "bar" and a file "foo" was
        // created, we might acquire a lock on both the parent and "bar").
        let store = self.store();
        let mut child_object_id = INVALID_OBJECT_ID;
        loop {
            let mut lock_keys = match child_object_id {
                INVALID_OBJECT_ID => vec![LockKey::object(store.store_object_id(), self.object_id)],
                _ => vec![
                    LockKey::object(store.store_object_id(), self.object_id),
                    LockKey::object(store.store_object_id(), child_object_id),
                ],
            };
            lock_keys.extend_from_slice(extra_keys);
            let fs = store.filesystem().clone();
            let transaction = fs
                .new_transaction(
                    &lock_keys,
                    Options { borrow_metadata_space, ..Default::default() },
                )
                .await?;

            match self.lookup(name).await? {
                Some((object_id, object_descriptor)) => match object_descriptor {
                    ObjectDescriptor::File
                    | ObjectDescriptor::Directory
                    | ObjectDescriptor::Symlink => {
                        if object_id == child_object_id {
                            return Ok((transaction, Some((object_id, object_descriptor))));
                        }
                        child_object_id = object_id
                    }
                    _ => bail!(FxfsError::Inconsistent),
                },
                None => {
                    if child_object_id == INVALID_OBJECT_ID {
                        return Ok((transaction, None));
                    }
                    child_object_id = INVALID_OBJECT_ID;
                }
            };
        }
    }

    async fn has_children(&self) -> Result<bool, Error> {
        if self.is_deleted() {
            return Ok(false);
        }
        let layer_set = self.store().tree().layer_set();
        let mut merger = layer_set.merger();
        Ok(self.iter(&mut merger).await?.get().is_some())
    }

    /// Returns the object ID and descriptor for the given child, or None if not found.
    pub async fn lookup(&self, name: &str) -> Result<Option<(u64, ObjectDescriptor)>, Error> {
        trace_duration!("Directory::lookup");
        if self.is_deleted() {
            return Ok(None);
        }
        match self.store().tree().find(&ObjectKey::child(self.object_id, name)).await? {
            None | Some(ObjectItem { value: ObjectValue::None, .. }) => Ok(None),
            Some(ObjectItem {
                value: ObjectValue::Child { object_id, object_descriptor }, ..
            }) => Ok(Some((object_id, object_descriptor))),
            Some(item) => Err(anyhow!(FxfsError::Inconsistent)
                .context(format!("Unexpected item in lookup: {:?}", item))),
        }
    }

    pub async fn create_child_dir(
        &self,
        transaction: &mut Transaction<'_>,
        name: &str,
        posix_attributes: Option<PosixAttributes>,
    ) -> Result<Directory<S>, Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let handle = Directory::create(transaction, &self.owner, posix_attributes).await?;
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(self.object_id, name),
                ObjectValue::child(handle.object_id(), ObjectDescriptor::Directory),
            ),
        );
        self.update_attributes(transaction, None, Some(Timestamp::now()), 1, posix_attributes)
            .await?;
        self.copy_project_id_to_object_in_txn(transaction, handle.object_id())?;
        Ok(handle)
    }

    pub async fn add_child_file<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        name: &str,
        handle: &StoreObjectHandle<S>,
    ) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(self.object_id, name),
                ObjectValue::child(handle.object_id(), ObjectDescriptor::File),
            ),
        );
        self.update_attributes(transaction, None, Some(Timestamp::now()), 0, None).await?;
        Ok(())
    }

    // This applies the project id of this directory (if nonzero) to an object. The method assumes
    // both this and child objects are already present in the mutations of the provided
    // transactions and that the child is of of zero size. This is meant for use inside
    // `create_child_file()` and `create_child_dir()` only, where such assumptions are safe.
    fn copy_project_id_to_object_in_txn<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        object_id: u64,
    ) -> Result<(), Error> {
        let store_id = self.store().store_object_id();
        // This mutation must already be in here as we've just modified the mtime.
        let ObjectValue::Object{ attributes: ObjectAttributes{ project_id, .. }, .. } =
            transaction.get_object_mutation(
                store_id,
                ObjectKey::object(self.object_id)
        ).unwrap().item.value else {
            return Err(anyhow!(FxfsError::Inconsistent));
        };
        if project_id > 0 {
            // This mutation must be present as well since we've just created the object. So this
            // replaces it.
            let mut mutation = transaction
                .get_object_mutation(store_id, ObjectKey::object(object_id))
                .unwrap()
                .clone();
            if let ObjectValue::Object {
                attributes: ObjectAttributes { project_id: child_project_id, .. },
                ..
            } = &mut mutation.item.value
            {
                *child_project_id = project_id;
            } else {
                return Err(anyhow!(FxfsError::Inconsistent));
            }
            transaction.add(store_id, Mutation::ObjectStore(mutation));
            transaction.add(
                store_id,
                Mutation::merge_object(
                    ObjectKey::project_usage(self.store().root_directory_object_id(), project_id),
                    ObjectValue::BytesAndNodes { bytes: 0, nodes: 1 },
                ),
            );
        }
        Ok(())
    }

    pub async fn create_child_file<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        name: &str,
    ) -> Result<StoreObjectHandle<S>, Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let handle =
            ObjectStore::create_object(&self.owner, transaction, HandleOptions::default(), None)
                .await?;
        self.add_child_file(transaction, name, &handle).await?;
        self.copy_project_id_to_object_in_txn(transaction, handle.object_id())?;
        Ok(handle)
    }

    pub async fn create_symlink(
        &self,
        transaction: &mut Transaction<'_>,
        link: &[u8],
        name: &str,
    ) -> Result<u64, Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        // Limit the length of link that might be too big to put in the tree.
        // https://pubs.opengroup.org/onlinepubs/9699919799/basedefs/limits.h.html.
        // See _POSIX_SYMLINK_MAX.
        ensure!(link.len() <= 256, FxfsError::BadPath);
        let symlink_id = self.store().get_next_object_id().await?;
        transaction.add(
            self.store().store_object_id(),
            Mutation::insert_object(
                ObjectKey::object(symlink_id),
                ObjectValue::symlink(link, Timestamp::now(), Timestamp::now(), 0),
            ),
        );
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(self.object_id, name),
                ObjectValue::child(symlink_id, ObjectDescriptor::Symlink),
            ),
        );
        self.update_attributes(transaction, None, Some(Timestamp::now()), 0, None).await?;
        Ok(symlink_id)
    }

    pub async fn add_child_volume(
        &self,
        transaction: &mut Transaction<'_>,
        volume_name: &str,
        store_object_id: u64,
    ) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(self.object_id, volume_name),
                ObjectValue::child(store_object_id, ObjectDescriptor::Volume),
            ),
        );
        self.update_attributes(transaction, None, Some(Timestamp::now()), 0, None).await
    }

    pub async fn delete_child_volume<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        volume_name: &str,
        store_object_id: u64,
    ) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(self.object_id, volume_name),
                ObjectValue::None,
            ),
        );
        // We note in the journal that we've deleted the volume. ObjectManager applies this
        // mutation by forgetting the store. We do it this way to ensure that the store is removed
        // during replay where there may be mutations to the store prior to its deletion. Without
        // this, we will try (and fail) to open the store after replay.
        transaction.add(store_object_id, Mutation::DeleteVolume);
        Ok(())
    }

    /// Inserts a child into the directory.
    ///
    /// Requires transaction locks on |self|.
    pub async fn insert_child<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        name: &str,
        object_id: u64,
        descriptor: ObjectDescriptor,
    ) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let sub_dirs_delta = if descriptor == ObjectDescriptor::Directory { 1 } else { 0 };
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(self.object_id, name),
                ObjectValue::child(object_id, descriptor),
            ),
        );
        self.update_attributes(transaction, None, Some(Timestamp::now()), sub_dirs_delta, None)
            .await
    }

    /// Updates attributes for the directory.
    pub async fn update_attributes<'a>(
        &self,
        transaction: &mut Transaction<'a>,
        crtime: Option<Timestamp>,
        mtime: Option<Timestamp>,
        sub_dirs_delta: i64,
        posix_attributes: Option<PosixAttributes>,
    ) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let mut mutation =
            self.store().txn_get_object_mutation(&transaction, self.object_id).await?;
        if let ObjectValue::Object { attributes, kind: ObjectKind::Directory { sub_dirs } } =
            &mut mutation.item.value
        {
            if let Some(time) = crtime {
                attributes.creation_time = time;
            }
            if let Some(time) = mtime {
                attributes.modification_time = time;
            }
            attributes.posix_attributes = posix_attributes;
            *sub_dirs = sub_dirs.saturating_add_signed(sub_dirs_delta);
        } else {
            bail!(anyhow!(FxfsError::Inconsistent)
                .context("update_attributes: expected directory object"));
        };
        transaction.add(self.store().store_object_id(), Mutation::ObjectStore(mutation));
        Ok(())
    }

    pub async fn get_properties(&self) -> Result<ObjectProperties, Error> {
        let item = self
            .store()
            .tree()
            .find(&ObjectKey::object(self.object_id))
            .await?
            .ok_or(anyhow!(FxfsError::NotFound))?;
        match item.value {
            ObjectValue::Object {
                kind: ObjectKind::Directory { sub_dirs },
                attributes:
                    ObjectAttributes { creation_time, modification_time, posix_attributes, .. },
            } => Ok(ObjectProperties {
                refs: 1,
                allocated_size: 0,
                data_attribute_size: 0,
                creation_time,
                modification_time,
                sub_dirs,
                posix_attributes,
            }),
            _ => {
                bail!(anyhow!(FxfsError::Inconsistent)
                    .context("get_properties: Expected object value"))
            }
        }
    }

    pub async fn list_extended_attributes(&self) -> Result<Vec<Vec<u8>>, Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let handle = BasicObjectHandle::new(self.owner.clone(), self.object_id);
        handle.list_extended_attributes().await
    }

    pub async fn get_extended_attribute(&self, name: Vec<u8>) -> Result<Vec<u8>, Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let handle = BasicObjectHandle::new(self.owner.clone(), self.object_id);
        handle.get_extended_attribute(name).await
    }

    pub async fn set_extended_attribute(&self, name: Vec<u8>, value: Vec<u8>) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let handle = BasicObjectHandle::new(self.owner.clone(), self.object_id);
        handle.set_extended_attribute(name, value).await
    }

    pub async fn remove_extended_attribute(&self, name: Vec<u8>) -> Result<(), Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let handle = BasicObjectHandle::new(self.owner.clone(), self.object_id);
        handle.remove_extended_attribute(name).await
    }

    /// Returns an iterator that will return directory entries skipping deleted ones.  Example
    /// usage:
    ///
    ///   let layer_set = dir.store().tree().layer_set();
    ///   let mut merger = layer_set.merger();
    ///   let mut iter = dir.iter(&mut merger).await?;
    ///
    pub async fn iter<'a, 'b>(
        &self,
        merger: &'a mut Merger<'b, ObjectKey, ObjectValue>,
    ) -> Result<DirectoryIterator<'a, 'b>, Error> {
        self.iter_from(merger, "").await
    }

    /// Like "iter", but seeks from a specific filename (inclusive).  Example usage:
    ///
    ///   let layer_set = dir.store().tree().layer_set();
    ///   let mut merger = layer_set.merger();
    ///   let mut iter = dir.iter_from(&mut merger, "foo").await?;
    ///
    pub async fn iter_from<'a, 'b>(
        &self,
        merger: &'a mut Merger<'b, ObjectKey, ObjectValue>,
        from: &str,
    ) -> Result<DirectoryIterator<'a, 'b>, Error> {
        ensure!(!self.is_deleted(), FxfsError::Deleted);
        let mut iter =
            merger.seek(Bound::Included(&ObjectKey::child(self.object_id, from))).await?;
        // Skip deleted entries.
        loop {
            match iter.get() {
                Some(ItemRef {
                    key: ObjectKey { object_id, .. },
                    value: ObjectValue::None,
                    ..
                }) if *object_id == self.object_id => {}
                _ => break,
            }
            iter.advance().await?;
        }
        Ok(DirectoryIterator { object_id: self.object_id, iter })
    }
}

impl<S: HandleOwner> fmt::Debug for Directory<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Directory")
            .field("store_id", &self.store().store_object_id())
            .field("object_id", &self.object_id)
            .finish()
    }
}

pub struct DirectoryIterator<'a, 'b> {
    object_id: u64,
    iter: MergerIterator<'a, 'b, ObjectKey, ObjectValue>,
}

impl DirectoryIterator<'_, '_> {
    pub fn get(&self) -> Option<(&str, u64, &ObjectDescriptor)> {
        match self.iter.get() {
            Some(ItemRef {
                key: ObjectKey { object_id: oid, data: ObjectKeyData::Child { name } },
                value: ObjectValue::Child { object_id, object_descriptor },
                ..
            }) if *oid == self.object_id => Some((name, *object_id, object_descriptor)),
            _ => None,
        }
    }

    pub async fn advance(&mut self) -> Result<(), Error> {
        loop {
            self.iter.advance().await?;
            // Skip deleted entries.
            match self.iter.get() {
                Some(ItemRef {
                    key: ObjectKey { object_id, .. },
                    value: ObjectValue::None,
                    ..
                }) if *object_id == self.object_id => {}
                _ => return Ok(()),
            }
        }
    }
}

/// Return type for |replace_child| describing the object which was replaced. The u64 fields are all
/// object_ids.
#[derive(Debug)]
pub enum ReplacedChild {
    None,
    File(u64),
    FileWithRemainingLinks(u64),
    Directory(u64),
}

/// Moves src.0/src.1 to dst.0/dst.1.
///
/// If |dst.0| already has a child |dst.1|, it is removed from dst.0.  For files, if this was their
/// last reference, the file is moved to the graveyard.  For directories, the removed directory will
/// be deleted permanently (and must be empty).
///
/// If |src| is None, this is effectively the same as unlink(dst.0/dst.1).
pub async fn replace_child<'a, S: HandleOwner>(
    transaction: &mut Transaction<'a>,
    src: Option<(&'a Directory<S>, &str)>,
    dst: (&'a Directory<S>, &str),
) -> Result<ReplacedChild, Error> {
    let mut sub_dirs_delta: i64 = 0;
    let now = Timestamp::now();

    let src = if let Some((src_dir, src_name)) = src {
        let store_id = dst.0.store().store_object_id();
        assert_eq!(store_id, src_dir.store().store_object_id());
        transaction.add(
            store_id,
            Mutation::replace_or_insert_object(
                ObjectKey::child(src_dir.object_id, src_name),
                ObjectValue::None,
            ),
        );
        let (id, descriptor) = src_dir.lookup(src_name).await?.ok_or(FxfsError::NotFound)?;
        if src_dir.object_id() != dst.0.object_id() {
            sub_dirs_delta = if descriptor == ObjectDescriptor::Directory { 1 } else { 0 };
            src_dir
                .update_attributes(transaction, None, Some(now.clone()), -sub_dirs_delta, None)
                .await?;
        }
        Some((id, descriptor))
    } else {
        None
    };

    replace_child_with_object(transaction, src, dst, sub_dirs_delta, now).await
}

/// Replaces dst.0/dst.1 with the given object, or unlinks if `src` is None.
///
/// If |dst.0| already has a child |dst.1|, it is removed from dst.0.  For files, if this was their
/// last reference, the file is moved to the graveyard.  For directories, the removed directory will
/// be deleted permanently (and must be empty).
///
/// `sub_dirs_delta` can be used if `src` is a directory and happened to already be a child of
/// `dst`.
pub async fn replace_child_with_object<'a, S: HandleOwner>(
    transaction: &mut Transaction<'a>,
    src: Option<(u64, ObjectDescriptor)>,
    dst: (&'a Directory<S>, &str),
    mut sub_dirs_delta: i64,
    timestamp: Timestamp,
) -> Result<ReplacedChild, Error> {
    let deleted_id_and_descriptor = dst.0.lookup(dst.1).await?;
    let result = match deleted_id_and_descriptor {
        Some((old_id, ObjectDescriptor::File)) => {
            let was_last_ref = dst.0.store().adjust_refs(transaction, old_id, -1).await?;
            if was_last_ref {
                ReplacedChild::File(old_id)
            } else {
                ReplacedChild::FileWithRemainingLinks(old_id)
            }
        }
        Some((old_id, ObjectDescriptor::Directory)) => {
            let dir = Directory::open(&dst.0.owner(), old_id).await?;
            if dir.has_children().await? {
                bail!(FxfsError::NotEmpty);
            }
            remove(transaction, &dst.0.store(), old_id);
            sub_dirs_delta -= 1;
            ReplacedChild::Directory(old_id)
        }
        Some((_, ObjectDescriptor::Volume)) => {
            bail!(anyhow!(FxfsError::Inconsistent).context("Unexpected volume child"))
        }
        None => {
            if src.is_none() {
                // Neither src nor dst exist
                bail!(FxfsError::NotFound);
            }
            ReplacedChild::None
        }
        Some((old_id, ObjectDescriptor::Symlink)) => {
            transaction.add(
                dst.0.store().store_object_id(),
                Mutation::replace_or_insert_object(ObjectKey::object(old_id), ObjectValue::None),
            );
            ReplacedChild::None
        }
    };
    let store_id = dst.0.store().store_object_id();
    let new_value = match src {
        Some((id, descriptor)) => ObjectValue::child(id, descriptor),
        None => ObjectValue::None,
    };
    transaction.add(
        store_id,
        Mutation::replace_or_insert_object(ObjectKey::child(dst.0.object_id, dst.1), new_value),
    );
    dst.0.update_attributes(transaction, None, Some(timestamp), sub_dirs_delta, None).await?;
    Ok(result)
}

fn remove(transaction: &mut Transaction<'_>, store: &ObjectStore, object_id: u64) {
    transaction.add(
        store.store_object_id(),
        Mutation::merge_object(ObjectKey::object(object_id), ObjectValue::None),
    );
}

#[cfg(test)]
mod tests {
    use {
        super::remove,
        crate::{
            errors::FxfsError,
            filesystem::{Filesystem, FxFilesystem, JournalingObject, SyncOptions},
            object_handle::{ObjectHandle, ReadObjectHandle, WriteObjectHandle},
            object_store::{
                directory::{replace_child, Directory, ReplacedChild},
                object_record::Timestamp,
                transaction::{Options, TransactionHandler},
                HandleOptions, LockKey, ObjectDescriptor, ObjectStore,
            },
        },
        assert_matches::assert_matches,
        storage_device::{fake_device::FakeDevice, DeviceHolder},
    };

    const TEST_DEVICE_BLOCK_SIZE: u32 = 512;

    #[fuchsia::test]
    async fn test_create_directory() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let object_id = {
            let mut transaction = fs
                .clone()
                .new_transaction(&[], Options::default())
                .await
                .expect("new_transaction failed");
            let dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
                .await
                .expect("create failed");

            let child_dir = dir
                .create_child_dir(&mut transaction, "foo", Default::default())
                .await
                .expect("create_child_dir failed");
            let _child_dir_file = child_dir
                .create_child_file(&mut transaction, "bar")
                .await
                .expect("create_child_file failed");
            let _child_file = dir
                .create_child_file(&mut transaction, "baz")
                .await
                .expect("create_child_file failed");
            dir.add_child_volume(&mut transaction, "corge", 100)
                .await
                .expect("add_child_volume failed");
            transaction.commit().await.expect("commit failed");
            fs.sync(SyncOptions::default()).await.expect("sync failed");
            dir.object_id()
        };
        fs.close().await.expect("Close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");
        {
            let dir = Directory::open(&fs.root_store(), object_id).await.expect("open failed");
            let (object_id, object_descriptor) =
                dir.lookup("foo").await.expect("lookup failed").expect("not found");
            assert_eq!(object_descriptor, ObjectDescriptor::Directory);
            let child_dir =
                Directory::open(&fs.root_store(), object_id).await.expect("open failed");
            let (object_id, object_descriptor) =
                child_dir.lookup("bar").await.expect("lookup failed").expect("not found");
            assert_eq!(object_descriptor, ObjectDescriptor::File);
            let _child_dir_file = ObjectStore::open_object(
                &fs.root_store(),
                object_id,
                HandleOptions::default(),
                None,
            )
            .await
            .expect("open object failed");
            let (object_id, object_descriptor) =
                dir.lookup("baz").await.expect("lookup failed").expect("not found");
            assert_eq!(object_descriptor, ObjectDescriptor::File);
            let _child_file = ObjectStore::open_object(
                &fs.root_store(),
                object_id,
                HandleOptions::default(),
                None,
            )
            .await
            .expect("open object failed");
            let (object_id, object_descriptor) =
                dir.lookup("corge").await.expect("lookup failed").expect("not found");
            assert_eq!(object_id, 100);
            if let ObjectDescriptor::Volume = object_descriptor {
            } else {
                panic!("wrong ObjectDescriptor");
            }

            assert_eq!(dir.lookup("qux").await.expect("lookup failed"), None);
        }
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_delete_child() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
            .await
            .expect("create failed");

        dir.create_child_file(&mut transaction, "foo").await.expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, None, (&dir, "foo"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::File(..)
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_delete_child_with_children_fails() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child;
        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
            .await
            .expect("create failed");

        child = dir
            .create_child_dir(&mut transaction, "foo", Default::default())
            .await
            .expect("create_child_dir failed");
        child.create_child_file(&mut transaction, "bar").await.expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_eq!(
            replace_child(&mut transaction, None, (&dir, "foo"))
                .await
                .expect_err("replace_child succeeded")
                .downcast::<FxfsError>()
                .expect("wrong error"),
            FxfsError::NotEmpty
        );

        transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(fs.root_store().store_object_id(), child.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, None, (&child, "bar"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::File(..)
        );
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                &[
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, None, (&dir, "foo"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::Directory(..)
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_delete_and_reinsert_child() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
            .await
            .expect("create failed");

        dir.create_child_file(&mut transaction, "foo").await.expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, None, (&dir, "foo"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::File(..)
        );
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        dir.create_child_file(&mut transaction, "foo").await.expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        dir.lookup("foo").await.expect("lookup failed");
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_delete_child_persists() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let object_id = {
            let dir;
            let mut transaction = fs
                .clone()
                .new_transaction(&[], Options::default())
                .await
                .expect("new_transaction failed");
            dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
                .await
                .expect("create failed");

            dir.create_child_file(&mut transaction, "foo").await.expect("create_child_file failed");
            transaction.commit().await.expect("commit failed");
            dir.lookup("foo").await.expect("lookup failed");

            transaction = fs
                .clone()
                .new_transaction(
                    &[LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");
            assert_matches!(
                replace_child(&mut transaction, None, (&dir, "foo"))
                    .await
                    .expect("replace_child failed"),
                ReplacedChild::File(..)
            );
            transaction.commit().await.expect("commit failed");

            fs.sync(SyncOptions::default()).await.expect("sync failed");
            dir.object_id()
        };

        fs.close().await.expect("Close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");
        let dir = Directory::open(&fs.root_store(), object_id).await.expect("open failed");
        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_replace_child() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child_dir1;
        let child_dir2;
        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
            .await
            .expect("create failed");

        child_dir1 = dir
            .create_child_dir(&mut transaction, "dir1", Default::default())
            .await
            .expect("create_child_dir failed");
        child_dir2 = dir
            .create_child_dir(&mut transaction, "dir2", Default::default())
            .await
            .expect("create_child_dir failed");
        child_dir1
            .create_child_file(&mut transaction, "foo")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                &[
                    LockKey::object(fs.root_store().store_object_id(), child_dir1.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child_dir2.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, Some((&child_dir1, "foo")), (&child_dir2, "bar"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::None
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(child_dir1.lookup("foo").await.expect("lookup failed"), None);
        child_dir2.lookup("bar").await.expect("lookup failed");
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_replace_child_overwrites_dst() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child_dir1;
        let child_dir2;
        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
            .await
            .expect("create failed");

        child_dir1 = dir
            .create_child_dir(&mut transaction, "dir1", Default::default())
            .await
            .expect("create_child_dir failed");
        child_dir2 = dir
            .create_child_dir(&mut transaction, "dir2", Default::default())
            .await
            .expect("create_child_dir failed");
        let foo = child_dir1
            .create_child_file(&mut transaction, "foo")
            .await
            .expect("create_child_file failed");
        let bar = child_dir2
            .create_child_file(&mut transaction, "bar")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        {
            let mut buf = foo.allocate_buffer(TEST_DEVICE_BLOCK_SIZE as usize);
            buf.as_mut_slice().fill(0xaa);
            foo.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
            buf.as_mut_slice().fill(0xbb);
            bar.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
        }
        std::mem::drop(bar);
        std::mem::drop(foo);

        transaction = fs
            .clone()
            .new_transaction(
                &[
                    LockKey::object(fs.root_store().store_object_id(), child_dir1.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child_dir2.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, Some((&child_dir1, "foo")), (&child_dir2, "bar"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::File(..)
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(child_dir1.lookup("foo").await.expect("lookup failed"), None);

        // Check the contents to ensure that the file was replaced.
        let (oid, object_descriptor) =
            child_dir2.lookup("bar").await.expect("lookup failed").expect("not found");
        assert_eq!(object_descriptor, ObjectDescriptor::File);
        let bar = ObjectStore::open_object(&child_dir2.owner, oid, HandleOptions::default(), None)
            .await
            .expect("Open failed");
        let mut buf = bar.allocate_buffer(TEST_DEVICE_BLOCK_SIZE as usize);
        bar.read(0, buf.as_mut()).await.expect("read failed");
        assert_eq!(buf.as_slice(), vec![0xaa; TEST_DEVICE_BLOCK_SIZE as usize]);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_replace_child_fails_if_would_overwrite_nonempty_dir() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child_dir1;
        let child_dir2;
        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
            .await
            .expect("create failed");

        child_dir1 = dir
            .create_child_dir(&mut transaction, "dir1", Default::default())
            .await
            .expect("create_child_dir failed");
        child_dir2 = dir
            .create_child_dir(&mut transaction, "dir2", Default::default())
            .await
            .expect("create_child_dir failed");
        child_dir1
            .create_child_file(&mut transaction, "foo")
            .await
            .expect("create_child_file failed");
        let nested_child = child_dir2
            .create_child_dir(&mut transaction, "bar", Default::default())
            .await
            .expect("create_child_file failed");
        nested_child
            .create_child_file(&mut transaction, "baz")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                &[
                    LockKey::object(fs.root_store().store_object_id(), child_dir1.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child_dir2.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_eq!(
            replace_child(&mut transaction, Some((&child_dir1, "foo")), (&child_dir2, "bar"))
                .await
                .expect_err("replace_child succeeded")
                .downcast::<FxfsError>()
                .expect("wrong error"),
            FxfsError::NotEmpty
        );
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_replace_child_within_dir() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
            .await
            .expect("create failed");
        dir.create_child_file(&mut transaction, "foo").await.expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, Some((&dir, "foo")), (&dir, "bar"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::None
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);
        dir.lookup("bar").await.expect("lookup new name failed");
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_iterate() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
            .await
            .expect("create failed");
        let _cat =
            dir.create_child_file(&mut transaction, "cat").await.expect("create_child_file failed");
        let _ball = dir
            .create_child_file(&mut transaction, "ball")
            .await
            .expect("create_child_file failed");
        let _apple = dir
            .create_child_file(&mut transaction, "apple")
            .await
            .expect("create_child_file failed");
        let _dog =
            dir.create_child_file(&mut transaction, "dog").await.expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");
        transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, None, (&dir, "apple"))
            .await
            .expect("rereplace_child failed");
        transaction.commit().await.expect("commit failed");
        let layer_set = dir.store().tree().layer_set();
        let mut merger = layer_set.merger();
        let mut iter = dir.iter(&mut merger).await.expect("iter failed");
        let mut entries = Vec::new();
        while let Some((name, _, _)) = iter.get() {
            entries.push(name.to_string());
            iter.advance().await.expect("advance failed");
        }
        assert_eq!(&entries, &["ball", "cat", "dog"]);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_sub_dir_count() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let child_dir;
        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
            .await
            .expect("create failed");
        child_dir = dir
            .create_child_dir(&mut transaction, "foo", Default::default())
            .await
            .expect("create_child_dir failed");
        transaction.commit().await.expect("commit failed");
        assert_eq!(dir.get_properties().await.expect("get_properties failed").sub_dirs, 1);

        // Moving within the same directory should not change the sub_dir count.
        transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, Some((&dir, "foo")), (&dir, "bar"))
            .await
            .expect("replace_child failed");
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.get_properties().await.expect("get_properties failed").sub_dirs, 1);
        assert_eq!(child_dir.get_properties().await.expect("get_properties failed").sub_dirs, 0);

        // Moving between two different directories should update source and destination.
        transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(fs.root_store().store_object_id(), child_dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let second_child = child_dir
            .create_child_dir(&mut transaction, "baz", Default::default())
            .await
            .expect("create_child_dir failed");
        transaction.commit().await.expect("commit failed");

        assert_eq!(child_dir.get_properties().await.expect("get_properties failed").sub_dirs, 1);

        transaction = fs
            .clone()
            .new_transaction(
                &[
                    LockKey::object(fs.root_store().store_object_id(), child_dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, Some((&child_dir, "baz")), (&dir, "foo"))
            .await
            .expect("replace_child failed");
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.get_properties().await.expect("get_properties failed").sub_dirs, 2);
        assert_eq!(child_dir.get_properties().await.expect("get_properties failed").sub_dirs, 0);

        // Moving over a directory.
        transaction = fs
            .clone()
            .new_transaction(
                &[
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), second_child.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, Some((&dir, "bar")), (&dir, "foo"))
            .await
            .expect("replace_child failed");
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.get_properties().await.expect("get_properties failed").sub_dirs, 1);

        // Unlinking a directory.
        transaction = fs
            .clone()
            .new_transaction(
                &[
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child_dir.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, None, (&dir, "foo")).await.expect("replace_child failed");
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.get_properties().await.expect("get_properties failed").sub_dirs, 0);
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_deleted_dir() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
            .await
            .expect("create failed");
        let child = dir
            .create_child_dir(&mut transaction, "foo", Default::default())
            .await
            .expect("create_child_dir failed");
        dir.create_child_dir(&mut transaction, "bar", Default::default())
            .await
            .expect("create_child_dir failed");
        transaction.commit().await.expect("commit failed");

        // Flush the tree so that we end up with records in different layers.
        dir.store().flush().await.expect("flush failed");

        // Unlink the child directory.
        transaction = fs
            .clone()
            .new_transaction(
                &[
                    LockKey::object(fs.root_store().store_object_id(), dir.object_id()),
                    LockKey::object(fs.root_store().store_object_id(), child.object_id()),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        replace_child(&mut transaction, None, (&dir, "foo")).await.expect("replace_child failed");
        transaction.commit().await.expect("commit failed");

        // Finding the child should fail now.
        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);

        // But finding "bar" should succeed.
        assert!(dir.lookup("bar").await.expect("lookup failed").is_some());

        // Remove dir now.
        transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(fs.root_store().store_object_id(), dir.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        remove(&mut transaction, dir.store(), dir.object_id());
        transaction.commit().await.expect("commit failed");

        // If we mark dir as deleted, any further operations should fail.
        dir.set_deleted();

        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);
        assert_eq!(dir.lookup("bar").await.expect("lookup failed"), None);
        assert!(!dir.has_children().await.expect("has_children failed"));

        transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");

        let assert_access_denied = |result| {
            if let Err(e) = result {
                assert!(FxfsError::Deleted.matches(&e));
            } else {
                panic!();
            }
        };
        assert_access_denied(
            dir.create_child_dir(&mut transaction, "baz", Default::default()).await.map(|_| {}),
        );
        assert_access_denied(dir.create_child_file(&mut transaction, "baz").await.map(|_| {}));
        assert_access_denied(dir.add_child_volume(&mut transaction, "baz", 1).await);
        assert_access_denied(
            dir.insert_child(&mut transaction, "baz", 1, ObjectDescriptor::File).await,
        );
        assert_access_denied(
            dir.update_attributes(&mut transaction, Some(Timestamp::zero()), None, 0, None).await,
        );
        let layer_set = dir.store().tree().layer_set();
        let mut merger = layer_set.merger();
        assert_access_denied(dir.iter(&mut merger).await.map(|_| {}));
    }

    #[fuchsia::test]
    async fn test_create_symlink() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let (dir_id, symlink_id) = {
            let mut transaction = fs
                .clone()
                .new_transaction(&[], Options::default())
                .await
                .expect("new_transaction failed");
            let dir = Directory::create(&mut transaction, &fs.root_store(), Default::default())
                .await
                .expect("create failed");

            let symlink_id = dir
                .create_symlink(&mut transaction, b"link", "foo")
                .await
                .expect("create_symlink failed");
            transaction.commit().await.expect("commit failed");

            fs.sync(SyncOptions::default()).await.expect("sync failed");
            (dir.object_id(), symlink_id)
        };
        fs.close().await.expect("Close failed");
        let device = fs.take_device().await;
        device.reopen(false);
        let fs = FxFilesystem::open(device).await.expect("open failed");
        {
            let dir = Directory::open(&fs.root_store(), dir_id).await.expect("open failed");
            assert_eq!(
                dir.lookup("foo").await.expect("lookup failed").expect("not found"),
                (symlink_id, ObjectDescriptor::Symlink)
            );
        }
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_read_symlink() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        let store = fs.root_store();
        let dir = Directory::create(&mut transaction, &store, Default::default())
            .await
            .expect("create failed");

        let symlink_id = dir
            .create_symlink(&mut transaction, b"link", "foo")
            .await
            .expect("create_symlink failed");
        transaction.commit().await.expect("commit failed");

        let link = store.read_symlink(symlink_id).await.expect("read_symlink failed");
        assert_eq!(&link, b"link");
        fs.close().await.expect("Close failed");
    }

    #[fuchsia::test]
    async fn test_unlink_symlink() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let dir;
        let mut transaction = fs
            .clone()
            .new_transaction(&[], Options::default())
            .await
            .expect("new_transaction failed");
        let store = fs.root_store();
        dir = Directory::create(&mut transaction, &store, Default::default())
            .await
            .expect("create failed");

        let symlink_id = dir
            .create_symlink(&mut transaction, b"link", "foo")
            .await
            .expect("create_symlink failed");
        transaction.commit().await.expect("commit failed");
        transaction = fs
            .clone()
            .new_transaction(
                &[
                    LockKey::object(store.store_object_id(), dir.object_id()),
                    LockKey::object(store.store_object_id(), symlink_id),
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        assert_matches!(
            replace_child(&mut transaction, None, (&dir, "foo"))
                .await
                .expect("replace_child failed"),
            ReplacedChild::None
        );
        transaction.commit().await.expect("commit failed");

        assert_eq!(dir.lookup("foo").await.expect("lookup failed"), None);
        assert!(FxfsError::NotFound
            .matches(&store.read_symlink(symlink_id).await.expect_err("read_symlink succeeded")));
        fs.close().await.expect("Close failed");
    }
}
