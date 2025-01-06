// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::errors::FxfsError;
use crate::log::*;
use crate::lsm_tree::merge::{Merger, MergerIterator};
use crate::lsm_tree::types::{ItemRef, LayerIterator};
use crate::lsm_tree::Query;
use crate::object_handle::INVALID_OBJECT_ID;
use crate::object_store::object_manager::ObjectManager;
use crate::object_store::object_record::{
    ObjectAttributes, ObjectKey, ObjectKeyData, ObjectKind, ObjectValue, Timestamp,
};
use crate::object_store::transaction::{Mutation, Options, Transaction};
use crate::object_store::ObjectStore;
use anyhow::{anyhow, bail, Context, Error};
use fuchsia_async::{self as fasync};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::channel::oneshot;
use futures::StreamExt;
use std::sync::{Arc, Mutex};

enum ReaperTask {
    None,
    Pending(UnboundedReceiver<Message>),
    Running(fasync::Task<()>),
}

/// A graveyard exists as a place to park objects that should be deleted when they are no longer in
/// use.  How objects enter and leave the graveyard is up to the caller to decide.  The intention is
/// that at mount time, any objects in the graveyard will get removed.  Each object store has a
/// directory like object that contains a list of the objects within that store that are part of the
/// graveyard.  A single instance of this Graveyard struct manages *all* stores.
pub struct Graveyard {
    object_manager: Arc<ObjectManager>,
    reaper_task: Mutex<ReaperTask>,
    channel: UnboundedSender<Message>,
}

enum Message {
    // Tombstone the object identified by <store-id>, <object-id>, Option<attribute-id>. If
    // <attribute-id> is Some, tombstone just the attribute instead of the entire object.
    Tombstone(u64, u64, Option<u64>),

    // Trims the identified object.
    Trim(u64, u64),

    // When the flush message is processed, notifies sender.  This allows the receiver to know
    // that all preceding tombstone messages have been processed.
    Flush(oneshot::Sender<()>),
}

#[fxfs_trace::trace]
impl Graveyard {
    /// Creates a new instance of the graveyard manager.
    pub fn new(object_manager: Arc<ObjectManager>) -> Arc<Self> {
        let (sender, receiver) = unbounded();
        Arc::new(Graveyard {
            object_manager,
            reaper_task: Mutex::new(ReaperTask::Pending(receiver)),
            channel: sender,
        })
    }

    /// Creates a graveyard object in `store`.  Returns the object ID for the graveyard object.
    pub fn create(transaction: &mut Transaction<'_>, store: &ObjectStore) -> u64 {
        let object_id = store.maybe_get_next_object_id();
        // This is OK because we only ever create a graveyard as we are creating a new store so
        // maybe_get_next_object_id will never fail here due to a lack of an object ID cipher.
        assert_ne!(object_id, INVALID_OBJECT_ID);
        let now = Timestamp::now();
        transaction.add(
            store.store_object_id,
            Mutation::insert_object(
                ObjectKey::object(object_id),
                ObjectValue::Object {
                    kind: ObjectKind::Graveyard,
                    attributes: ObjectAttributes {
                        creation_time: now.clone(),
                        modification_time: now,
                        project_id: 0,
                        ..Default::default()
                    },
                },
            ),
        );
        object_id
    }

    /// Starts an asynchronous task to reap the graveyard for all entries older than
    /// |journal_offset| (exclusive).
    /// If a task is already started, this has no effect, even if that task was targeting an older
    /// |journal_offset|.
    pub fn reap_async(self: Arc<Self>) {
        let mut reaper_task = self.reaper_task.lock().unwrap();
        if let ReaperTask::Pending(_) = &*reaper_task {
            if let ReaperTask::Pending(receiver) =
                std::mem::replace(&mut *reaper_task, ReaperTask::None)
            {
                *reaper_task =
                    ReaperTask::Running(fasync::Task::spawn(self.clone().reap_task(receiver)));
            } else {
                unreachable!();
            }
        }
    }

    /// Returns a future which completes when the ongoing reap task (if it exists) completes.
    pub async fn wait_for_reap(&self) {
        self.channel.close_channel();
        let task = std::mem::replace(&mut *self.reaper_task.lock().unwrap(), ReaperTask::None);
        if let ReaperTask::Running(task) = task {
            task.await;
        }
    }

    async fn reap_task(self: Arc<Self>, mut receiver: UnboundedReceiver<Message>) {
        // Wait and process reap requests.
        while let Some(message) = receiver.next().await {
            match message {
                Message::Tombstone(store_id, object_id, attribute_id) => {
                    let res = if let Some(attribute_id) = attribute_id {
                        self.tombstone_attribute(store_id, object_id, attribute_id).await
                    } else {
                        self.tombstone_object(store_id, object_id).await
                    };
                    if let Err(e) = res {
                        error!(error:? = e, store_id, oid = object_id, attribute_id; "Tombstone error");
                    }
                }
                Message::Trim(store_id, object_id) => {
                    if let Err(e) = self.trim(store_id, object_id).await {
                        error!(error:? = e, store_id, oid = object_id; "Tombstone error");
                    }
                }
                Message::Flush(sender) => {
                    let _ = sender.send(());
                }
            }
        }
    }

    /// Performs the initial mount-time reap for the given store.  This will queue all items in the
    /// graveyard.  Concurrently adding more entries to the graveyard will lead to undefined
    /// behaviour: the entries might or might not be immediately tombstoned, so callers should wait
    /// for this to return before changing to a state where more entries can be added.  Once this
    /// has returned, entries will be tombstoned in the background.
    #[trace]
    pub async fn initial_reap(self: &Arc<Self>, store: &ObjectStore) -> Result<usize, Error> {
        if store.filesystem().options().skip_initial_reap {
            return Ok(0);
        }
        let mut count = 0;
        let layer_set = store.tree().layer_set();
        let mut merger = layer_set.merger();
        let graveyard_object_id = store.graveyard_directory_object_id();
        let mut iter = Self::iter(graveyard_object_id, &mut merger).await?;
        let store_id = store.store_object_id();
        // TODO(https://fxbug.dev/355246066): This iteration can sometimes attempt to reap
        // attributes after they've been reaped as part of the parent object.
        while let Some(GraveyardEntryInfo { object_id, attribute_id, sequence: _, value }) =
            iter.get()
        {
            match value {
                ObjectValue::Some => {
                    if let Some(attribute_id) = attribute_id {
                        self.queue_tombstone_attribute(store_id, object_id, attribute_id)
                    } else {
                        self.queue_tombstone_object(store_id, object_id)
                    }
                }
                ObjectValue::Trim => {
                    if attribute_id.is_some() {
                        return Err(anyhow!(
                            "Trim is not currently supported for a single attribute"
                        ));
                    }
                    self.queue_trim(store_id, object_id)
                }
                _ => bail!(anyhow!(FxfsError::Inconsistent).context("Bad graveyard value")),
            }
            count += 1;
            iter.advance().await?;
        }
        Ok(count)
    }

    /// Queues an object for tombstoning.
    pub fn queue_tombstone_object(&self, store_id: u64, object_id: u64) {
        let _ = self.channel.unbounded_send(Message::Tombstone(store_id, object_id, None));
    }

    /// Queues an object's attribute for tombstoning.
    pub fn queue_tombstone_attribute(&self, store_id: u64, object_id: u64, attribute_id: u64) {
        let _ = self.channel.unbounded_send(Message::Tombstone(
            store_id,
            object_id,
            Some(attribute_id),
        ));
    }

    fn queue_trim(&self, store_id: u64, object_id: u64) {
        let _ = self.channel.unbounded_send(Message::Trim(store_id, object_id));
    }

    /// Waits for all preceding queued tombstones to finish.
    pub async fn flush(&self) {
        let (sender, receiver) = oneshot::channel::<()>();
        self.channel.unbounded_send(Message::Flush(sender)).unwrap();
        receiver.await.unwrap();
    }

    /// Immediately tombstones (discards) an object in the graveyard.
    /// NB: Code should generally use |queue_tombstone| instead.
    pub async fn tombstone_object(&self, store_id: u64, object_id: u64) -> Result<(), Error> {
        let store = self
            .object_manager
            .store(store_id)
            .with_context(|| format!("Failed to get store {}", store_id))?;
        // For now, it's safe to assume that all objects in the root parent and root store should
        // return space to the metadata reservation, but we might have to revisit that if we end up
        // with objects that are in other stores.
        let options = if store_id == self.object_manager.root_parent_store_object_id()
            || store_id == self.object_manager.root_store_object_id()
        {
            Options {
                skip_journal_checks: true,
                borrow_metadata_space: true,
                allocator_reservation: Some(self.object_manager.metadata_reservation()),
                ..Default::default()
            }
        } else {
            Options { skip_journal_checks: true, borrow_metadata_space: true, ..Default::default() }
        };
        store.tombstone_object(object_id, options).await
    }

    /// Immediately tombstones (discards) and attribute in the graveyard.
    /// NB: Code should generally use |queue_tombstone| instead.
    pub async fn tombstone_attribute(
        &self,
        store_id: u64,
        object_id: u64,
        attribute_id: u64,
    ) -> Result<(), Error> {
        let store = self
            .object_manager
            .store(store_id)
            .with_context(|| format!("Failed to get store {}", store_id))?;
        // For now, it's safe to assume that all objects in the root parent and root store should
        // return space to the metadata reservation, but we might have to revisit that if we end up
        // with objects that are in other stores.
        let options = if store_id == self.object_manager.root_parent_store_object_id()
            || store_id == self.object_manager.root_store_object_id()
        {
            Options {
                skip_journal_checks: true,
                borrow_metadata_space: true,
                allocator_reservation: Some(self.object_manager.metadata_reservation()),
                ..Default::default()
            }
        } else {
            Options { skip_journal_checks: true, borrow_metadata_space: true, ..Default::default() }
        };
        store.tombstone_attribute(object_id, attribute_id, options).await
    }

    async fn trim(&self, store_id: u64, object_id: u64) -> Result<(), Error> {
        let store = self
            .object_manager
            .store(store_id)
            .with_context(|| format!("Failed to get store {}", store_id))?;
        store.trim(object_id).await.context("Failed to trim object")
    }

    /// Returns an iterator that will return graveyard entries skipping deleted ones.  Example
    /// usage:
    ///
    ///   let layer_set = graveyard.store().tree().layer_set();
    ///   let mut merger = layer_set.merger();
    ///   let mut iter = graveyard.iter(&mut merger).await?;
    ///
    pub async fn iter<'a, 'b>(
        graveyard_object_id: u64,
        merger: &'a mut Merger<'b, ObjectKey, ObjectValue>,
    ) -> Result<GraveyardIterator<'a, 'b>, Error> {
        Self::iter_from(merger, graveyard_object_id, 0).await
    }

    /// Like "iter", but seeks from a specific (store-id, object-id) tuple.  Example usage:
    ///
    ///   let layer_set = graveyard.store().tree().layer_set();
    ///   let mut merger = layer_set.merger();
    ///   let mut iter = graveyard.iter_from(&mut merger, (2, 3)).await?;
    ///
    async fn iter_from<'a, 'b>(
        merger: &'a mut Merger<'b, ObjectKey, ObjectValue>,
        graveyard_object_id: u64,
        from: u64,
    ) -> Result<GraveyardIterator<'a, 'b>, Error> {
        GraveyardIterator::new(
            graveyard_object_id,
            merger
                .query(Query::FullRange(&ObjectKey::graveyard_entry(graveyard_object_id, from)))
                .await?,
        )
        .await
    }
}

pub struct GraveyardIterator<'a, 'b> {
    object_id: u64,
    iter: MergerIterator<'a, 'b, ObjectKey, ObjectValue>,
}

/// Contains information about a graveyard entry associated with a particular object or
/// attribute.
#[derive(Debug, PartialEq)]
pub struct GraveyardEntryInfo {
    object_id: u64,
    attribute_id: Option<u64>,
    sequence: u64,
    value: ObjectValue,
}

impl GraveyardEntryInfo {
    pub fn object_id(&self) -> u64 {
        self.object_id
    }

    pub fn attribute_id(&self) -> Option<u64> {
        self.attribute_id
    }

    pub fn value(&self) -> &ObjectValue {
        &self.value
    }
}

impl<'a, 'b> GraveyardIterator<'a, 'b> {
    async fn new(
        object_id: u64,
        iter: MergerIterator<'a, 'b, ObjectKey, ObjectValue>,
    ) -> Result<GraveyardIterator<'a, 'b>, Error> {
        let mut iter = GraveyardIterator { object_id, iter };
        iter.skip_deleted_entries().await?;
        Ok(iter)
    }

    async fn skip_deleted_entries(&mut self) -> Result<(), Error> {
        loop {
            match self.iter.get() {
                Some(ItemRef {
                    key: ObjectKey { object_id, .. },
                    value: ObjectValue::None,
                    ..
                }) if *object_id == self.object_id => {}
                _ => return Ok(()),
            }
            self.iter.advance().await?;
        }
    }

    pub fn get(&self) -> Option<GraveyardEntryInfo> {
        match self.iter.get() {
            Some(ItemRef {
                key: ObjectKey { object_id: oid, data: ObjectKeyData::GraveyardEntry { object_id } },
                value,
                sequence,
                ..
            }) if *oid == self.object_id => Some(GraveyardEntryInfo {
                object_id: *object_id,
                attribute_id: None,
                sequence,
                value: value.clone(),
            }),
            Some(ItemRef {
                key:
                    ObjectKey {
                        object_id: oid,
                        data: ObjectKeyData::GraveyardAttributeEntry { object_id, attribute_id },
                    },
                value,
                sequence,
            }) if *oid == self.object_id => Some(GraveyardEntryInfo {
                object_id: *object_id,
                attribute_id: Some(*attribute_id),
                sequence,
                value: value.clone(),
            }),
            _ => None,
        }
    }

    pub async fn advance(&mut self) -> Result<(), Error> {
        self.iter.advance().await?;
        self.skip_deleted_entries().await
    }
}

#[cfg(test)]
mod tests {
    use super::{Graveyard, GraveyardEntryInfo, ObjectStore};
    use crate::errors::FxfsError;
    use crate::filesystem::{FxFilesystem, FxFilesystemBuilder};
    use crate::fsck::fsck;
    use crate::object_handle::ObjectHandle;
    use crate::object_store::data_object_handle::WRITE_ATTR_BATCH_SIZE;
    use crate::object_store::object_record::ObjectValue;
    use crate::object_store::transaction::{lock_keys, Options};
    use crate::object_store::{HandleOptions, Mutation, ObjectKey, FSVERITY_MERKLE_ATTRIBUTE_ID};
    use assert_matches::assert_matches;
    use storage_device::fake_device::FakeDevice;
    use storage_device::DeviceHolder;

    const TEST_DEVICE_BLOCK_SIZE: u32 = 512;

    #[fuchsia::test]
    async fn test_graveyard() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let root_store = fs.root_store();

        // Create and add two objects to the graveyard.
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");

        root_store.add_to_graveyard(&mut transaction, 3);
        root_store.add_to_graveyard(&mut transaction, 4);
        transaction.commit().await.expect("commit failed");

        // Check that we see the objects we added.
        {
            let layer_set = root_store.tree().layer_set();
            let mut merger = layer_set.merger();
            let mut iter = Graveyard::iter(root_store.graveyard_directory_object_id(), &mut merger)
                .await
                .expect("iter failed");
            assert_matches!(
                iter.get().expect("missing entry"),
                GraveyardEntryInfo {
                    object_id: 3,
                    attribute_id: None,
                    value: ObjectValue::Some,
                    ..
                }
            );
            iter.advance().await.expect("advance failed");
            assert_matches!(
                iter.get().expect("missing entry"),
                GraveyardEntryInfo {
                    object_id: 4,
                    attribute_id: None,
                    value: ObjectValue::Some,
                    ..
                }
            );
            iter.advance().await.expect("advance failed");
            assert_eq!(iter.get(), None);
        }

        // Remove one of the objects.
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        root_store.remove_from_graveyard(&mut transaction, 4);
        transaction.commit().await.expect("commit failed");

        // Check that the graveyard has been updated as expected.
        let layer_set = root_store.tree().layer_set();
        let mut merger = layer_set.merger();
        let mut iter = Graveyard::iter(root_store.graveyard_directory_object_id(), &mut merger)
            .await
            .expect("iter failed");
        assert_matches!(
            iter.get().expect("missing entry"),
            GraveyardEntryInfo { object_id: 3, attribute_id: None, value: ObjectValue::Some, .. }
        );
        iter.advance().await.expect("advance failed");
        assert_eq!(iter.get(), None);
    }

    #[fuchsia::test]
    async fn test_tombstone_attribute() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let root_store = fs.root_store();
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");

        let handle = ObjectStore::create_object(
            &root_store,
            &mut transaction,
            HandleOptions::default(),
            None,
            None,
        )
        .await
        .expect("failed to create object");
        transaction.commit().await.expect("commit failed");

        handle
            .write_attr(FSVERITY_MERKLE_ATTRIBUTE_ID, &[0; 8192])
            .await
            .expect("failed to write merkle attribute");
        let object_id = handle.object_id();
        let mut transaction = handle.new_transaction().await.expect("new_transaction failed");
        transaction.add(
            root_store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::graveyard_attribute_entry(
                    root_store.graveyard_directory_object_id(),
                    object_id,
                    FSVERITY_MERKLE_ATTRIBUTE_ID,
                ),
                ObjectValue::Some,
            ),
        );

        transaction.commit().await.expect("commit failed");

        fs.close().await.expect("failed to close filesystem");
        let device = fs.take_device().await;
        device.reopen(false);

        let fs =
            FxFilesystemBuilder::new().read_only(true).open(device).await.expect("open failed");
        fsck(fs.clone()).await.expect("fsck failed");
        fs.close().await.expect("failed to close filesystem");
        let device = fs.take_device().await;
        device.reopen(false);

        // On open, the filesystem will call initial_reap which will call queue_tombstone().
        let fs = FxFilesystem::open(device).await.expect("open failed");
        // `wait_for_reap` ensures that the Message::Tombstone is actually processed.
        fs.graveyard().wait_for_reap().await;
        let root_store = fs.root_store();

        let handle =
            ObjectStore::open_object(&root_store, object_id, HandleOptions::default(), None)
                .await
                .expect("failed to open object");

        assert_eq!(
            handle.read_attr(FSVERITY_MERKLE_ATTRIBUTE_ID).await.expect("read_attr failed"),
            None
        );
        fsck(fs.clone()).await.expect("fsck failed");
    }

    #[fuchsia::test]
    async fn test_tombstone_attribute_and_object() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let root_store = fs.root_store();
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");

        let handle = ObjectStore::create_object(
            &root_store,
            &mut transaction,
            HandleOptions::default(),
            None,
            None,
        )
        .await
        .expect("failed to create object");
        transaction.commit().await.expect("commit failed");

        handle
            .write_attr(FSVERITY_MERKLE_ATTRIBUTE_ID, &[0; 8192])
            .await
            .expect("failed to write merkle attribute");
        let object_id = handle.object_id();
        let mut transaction = handle.new_transaction().await.expect("new_transaction failed");
        transaction.add(
            root_store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::graveyard_attribute_entry(
                    root_store.graveyard_directory_object_id(),
                    object_id,
                    FSVERITY_MERKLE_ATTRIBUTE_ID,
                ),
                ObjectValue::Some,
            ),
        );
        transaction.commit().await.expect("commit failed");
        let mut transaction = handle.new_transaction().await.expect("new_transaction failed");
        transaction.add(
            root_store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::graveyard_entry(root_store.graveyard_directory_object_id(), object_id),
                ObjectValue::Some,
            ),
        );
        transaction.commit().await.expect("commit failed");

        fs.close().await.expect("failed to close filesystem");
        let device = fs.take_device().await;
        device.reopen(false);

        let fs =
            FxFilesystemBuilder::new().read_only(true).open(device).await.expect("open failed");
        fsck(fs.clone()).await.expect("fsck failed");
        fs.close().await.expect("failed to close filesystem");
        let device = fs.take_device().await;
        device.reopen(false);

        // On open, the filesystem will call initial_reap which will call queue_tombstone().
        let fs = FxFilesystem::open(device).await.expect("open failed");
        // `wait_for_reap` ensures that the two tombstone messages are processed.
        fs.graveyard().wait_for_reap().await;

        let root_store = fs.root_store();
        if let Err(e) =
            ObjectStore::open_object(&root_store, object_id, HandleOptions::default(), None).await
        {
            assert!(FxfsError::NotFound.matches(&e));
        } else {
            panic!("open_object succeeded");
        };
        fsck(fs.clone()).await.expect("fsck failed");
    }

    #[fuchsia::test]
    async fn test_tombstone_large_attribute() {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        let fs = FxFilesystem::new_empty(device).await.expect("new_empty failed");
        let root_store = fs.root_store();
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");

        let handle = ObjectStore::create_object(
            &root_store,
            &mut transaction,
            HandleOptions::default(),
            None,
            None,
        )
        .await
        .expect("failed to create object");
        transaction.commit().await.expect("commit failed");

        let object_id = {
            let mut transaction = handle.new_transaction().await.expect("new_transaction failed");
            transaction.add(
                root_store.store_object_id(),
                Mutation::replace_or_insert_object(
                    ObjectKey::graveyard_attribute_entry(
                        root_store.graveyard_directory_object_id(),
                        handle.object_id(),
                        FSVERITY_MERKLE_ATTRIBUTE_ID,
                    ),
                    ObjectValue::Some,
                ),
            );

            // This write should span three transactions. This test mimics the behavior when the
            // last transaction gets interrupted by a filesystem.close().
            handle
                .write_new_attr_in_batches(
                    &mut transaction,
                    FSVERITY_MERKLE_ATTRIBUTE_ID,
                    &vec![0; 3 * WRITE_ATTR_BATCH_SIZE],
                    WRITE_ATTR_BATCH_SIZE,
                )
                .await
                .expect("failed to write merkle attribute");

            handle.object_id()
            // Drop the transaction to simulate interrupting the merkle tree creation as well as to
            // release the transaction locks.
        };

        fs.close().await.expect("failed to close filesystem");
        let device = fs.take_device().await;
        device.reopen(false);

        let fs =
            FxFilesystemBuilder::new().read_only(true).open(device).await.expect("open failed");
        fsck(fs.clone()).await.expect("fsck failed");
        fs.close().await.expect("failed to close filesystem");
        let device = fs.take_device().await;
        device.reopen(false);

        // On open, the filesystem will call initial_reap which will call queue_tombstone().
        let fs = FxFilesystem::open(device).await.expect("open failed");
        // `wait_for_reap` ensures that the two tombstone messages are processed.
        fs.graveyard().wait_for_reap().await;

        let root_store = fs.root_store();

        let handle =
            ObjectStore::open_object(&root_store, object_id, HandleOptions::default(), None)
                .await
                .expect("failed to open object");

        assert_eq!(
            handle.read_attr(FSVERITY_MERKLE_ATTRIBUTE_ID).await.expect("read_attr failed"),
            None
        );
        fsck(fs.clone()).await.expect("fsck failed");
    }
}
