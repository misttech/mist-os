// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::filesystem::{
    FxFilesystem, FxFilesystemBuilder, JournalingObject, OpenFxFilesystem, SyncOptions,
};
use crate::fsck::errors::{FsckError, FsckFatal, FsckIssue, FsckWarning};
use crate::fsck::{fsck_volume_with_options, fsck_with_options, FsckOptions};
use crate::lsm_tree::persistent_layer::PersistentLayerWriter;
use crate::lsm_tree::types::{Item, ItemRef, Key, LayerIterator, LayerWriter, Value};
use crate::lsm_tree::Query;
use crate::object_handle::{ObjectHandle, ReadObjectHandle, WriteObjectHandle, INVALID_OBJECT_ID};
use crate::object_store::allocator::{AllocatorKey, AllocatorValue, CoalescingIterator};
use crate::object_store::directory::{self, Directory, MutableAttributesInternal};
use crate::object_store::transaction::{self, lock_keys, LockKey, ObjectStoreMutation, Options};
use crate::object_store::volume::root_volume;
use crate::object_store::{
    AttributeKey, ChildValue, EncryptionKeys, ExtentValue, FsverityMetadata, HandleOptions,
    Mutation, ObjectAttributes, ObjectDescriptor, ObjectKey, ObjectKeyData, ObjectKind,
    ObjectStore, ObjectValue, RootDigest, StoreInfo, Timestamp, DEFAULT_DATA_ATTRIBUTE_ID,
    FSVERITY_MERKLE_ATTRIBUTE_ID, VOLUME_DATA_KEY_ID,
};
use crate::round::round_down;
use crate::serialized_types::VersionedLatest;
use crate::testing::writer::Writer;
use anyhow::{Context, Error};
use assert_matches::assert_matches;
use fidl_fuchsia_io as fio;
use futures::join;
use fxfs_crypto::{Crypt, WrappedKeys};
use fxfs_insecure_crypto::InsecureCrypt;
use mundane::hash::{Digest, Hasher, Sha256};
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use storage_device::fake_device::FakeDevice;
use storage_device::DeviceHolder;

const TEST_DEVICE_BLOCK_SIZE: u32 = 512;
const TEST_DEVICE_BLOCK_COUNT: u64 = 8192;

struct FsckTest {
    filesystem: Option<OpenFxFilesystem>,
    errors: Mutex<Vec<FsckIssue>>,
    crypt: Option<Arc<InsecureCrypt>>,
}

#[derive(Default)]
struct TestOptions {
    halt_on_error: bool,
    skip_system_fsck: bool,
    volume_store_id: Option<u64>,
}

impl FsckTest {
    async fn new() -> Self {
        let filesystem = FxFilesystem::new_empty(DeviceHolder::new(FakeDevice::new(
            TEST_DEVICE_BLOCK_COUNT,
            TEST_DEVICE_BLOCK_SIZE,
        )))
        .await
        .expect("new_empty failed");

        Self { filesystem: Some(filesystem), errors: Mutex::new(vec![]), crypt: None }
    }
    async fn remount(&mut self) -> Result<(), Error> {
        let fs = self.filesystem.take().unwrap();
        fs.close().await.expect("Failed to close FS");
        let device = fs.take_device().await;
        device.reopen(true);
        self.filesystem = Some(
            FxFilesystemBuilder::new()
                .read_only(true)
                .open(device)
                .await
                .context("Failed to open FS")?,
        );
        Ok(())
    }
    async fn run(&self, test_options: TestOptions) -> Result<(), Error> {
        let options = FsckOptions {
            fail_on_warning: true,
            halt_on_error: test_options.halt_on_error,
            on_error: Box::new(|err| {
                if err.is_error() {
                    eprintln!("Fsck error: {:?}", &err);
                } else {
                    println!("Fsck warning: {:?}", &err);
                }
                self.errors.lock().unwrap().push(err.clone());
            }),
            ..Default::default()
        };
        if !test_options.skip_system_fsck {
            fsck_with_options(self.filesystem(), &options).await?;
        }
        if let Some(store_id) = test_options.volume_store_id {
            fsck_volume_with_options(
                self.filesystem().as_ref(),
                &options,
                store_id,
                self.crypt.clone().map(|x| x as Arc<dyn Crypt>),
            )
            .await?;
        }
        Ok(())
    }
    fn filesystem(&self) -> Arc<FxFilesystem> {
        self.filesystem.as_ref().unwrap().deref().clone()
    }
    fn errors(&self) -> Vec<FsckIssue> {
        self.errors.lock().unwrap().clone()
    }
    fn get_crypt(&mut self) -> Arc<InsecureCrypt> {
        self.crypt.get_or_insert_with(|| Arc::new(InsecureCrypt::new())).clone()
    }
}

// Creates a new layer file containing |items| and writes them in order into |store|, skipping all
// normal validation.  This allows bad records to be inserted into the object store (although they
// will still be subject to merging).
// Doing this in the root store might cause a variety of unrelated failures.
async fn install_items_in_store<K: Key, V: Value>(
    filesystem: &Arc<FxFilesystem>,
    store: &ObjectStore,
    items: impl AsRef<[Item<K, V>]>,
) {
    let device = filesystem.device();
    let root_store = filesystem.root_store();
    let mut transaction = filesystem
        .clone()
        .new_transaction(lock_keys![], Options::default())
        .await
        .expect("new_transaction failed");
    let layer_handle = ObjectStore::create_object(
        &root_store,
        &mut transaction,
        HandleOptions::default(),
        store.crypt().as_deref(),
        None,
    )
    .await
    .expect("create_object failed");
    transaction.commit().await.expect("commit failed");

    {
        let mut writer = PersistentLayerWriter::<_, K, V>::new(
            Writer::new(&layer_handle).await,
            items.as_ref().len(),
            filesystem.block_size(),
        )
        .await
        .expect("writer new");
        for item in items.as_ref() {
            writer.write(item.as_item_ref()).await.expect("write failed");
        }
        writer.flush().await.expect("flush failed");
    }

    // store.store_info() holds the current state of the store including unflushed mods.
    // The on-disk version should represent the state of the store at the time the layer files
    // were written (i.e. excluding any entries pending in the journal) so we read it and modify
    // it's layer files.
    let store_info_handle = ObjectStore::open_object(
        &root_store,
        store.store_info_handle_object_id().unwrap(),
        HandleOptions::default(),
        None,
    )
    .await
    .expect("open store info handle failed");

    let mut store_info = if store_info_handle.get_size() == 0 {
        StoreInfo::default()
    } else {
        let mut cursor = std::io::Cursor::new(
            store_info_handle.contents(1000).await.expect("error reading content"),
        );
        StoreInfo::deserialize_with_version(&mut cursor).expect("deserialize_error").0
    };
    store_info.layers.push(layer_handle.object_id());
    let mut store_info_vec = vec![];
    store_info.serialize_with_version(&mut store_info_vec).expect("serialize failed");
    let mut buf = device.allocate_buffer(store_info_vec.len()).await;
    buf.as_mut_slice().copy_from_slice(&store_info_vec[..]);

    let mut transaction =
        store_info_handle.new_transaction().await.expect("new_transaction failed");
    store_info_handle.txn_write(&mut transaction, 0, buf.as_ref()).await.expect("txn_write failed");
    transaction.commit().await.expect("commit failed");
}

/* TODO(https://fxbug.dev/42173686): Fix this test
#[fuchsia::test]
async fn test_missing_graveyard() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let root_store = fs.root_store();
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![],
                transaction::Options {
                    skip_journal_checks: true,
                    borrow_metadata_space: true,
                    ..Default::default()
                },
            )
            .await
            .expect("New transaction failed");
        transaction.add(root_store.store_object_id, Mutation::graveyard_directory(u64::MAX - 1));
        transaction.commit().await.expect("Commit transaction failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [
            FsckIssue::Error(FsckError::ExtraAllocations(_)),
            FsckIssue::Error(FsckError::AllocatedBytesMismatch(..))
        ]
    );
}
*/

#[fuchsia::test]
async fn test_bad_graveyard_value() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let root_store = fs.root_store();
        let graveyard_id = root_store.graveyard_directory_object_id();
        transaction.add(
            root_store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::graveyard_entry(graveyard_id, 1000),
                ObjectValue::Attribute { size: 500, has_overwrite_extents: false },
            ),
        );
        transaction.commit().await.expect("commit failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Error(FsckError::BadGraveyardValue(_, object_id))] if object_id == 1000
    );
}

#[fuchsia::test]
async fn test_extra_allocation() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        // We need a discontiguous allocation, and some blocks will have been used up by other
        // things, so allocate the very last block.  Note that changing our allocation strategy
        // might break this test.
        let end =
            round_down(TEST_DEVICE_BLOCK_SIZE as u64 * TEST_DEVICE_BLOCK_COUNT, fs.block_size());
        fs.allocator()
            .mark_allocated(&mut transaction, 4, end - fs.block_size()..end)
            .await
            .expect("mark_allocated failed");
        transaction.commit().await.expect("commit failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Error(FsckError::ExtraAllocations(_)), ..]);
}

#[fuchsia::test]
async fn test_misaligned_allocation() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        // We need a discontiguous allocation, and some blocks will have been used up by other
        // things, so allocate the very last block.  Note that changing our allocation strategy
        // might break this test.
        let end =
            round_down(TEST_DEVICE_BLOCK_SIZE as u64 * TEST_DEVICE_BLOCK_COUNT, fs.block_size());
        fs.allocator()
            .mark_allocated(&mut transaction, 99, end - fs.block_size() + 1..end)
            .await
            .expect("mark_allocated failed");
        transaction.commit().await.expect("commit failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { halt_on_error: true, ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Error(FsckError::MisalignedAllocation(..))]);
}

#[fuchsia::test]
async fn test_malformed_allocation() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let root_store = fs.root_store();
        let device = fs.device();
        // We need to manually insert the record into the allocator's LSM tree directly, since the
        // allocator code checks range validity.

        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let layer_handle = ObjectStore::create_object(
            &root_store,
            &mut transaction,
            HandleOptions::default(),
            None,
            None,
        )
        .await
        .expect("create_object failed");
        transaction.commit().await.expect("commit failed");

        {
            let mut writer = PersistentLayerWriter::<_, AllocatorKey, AllocatorValue>::new(
                Writer::new(&layer_handle).await,
                1,
                fs.block_size(),
            )
            .await
            .expect("writer new");
            // We also need a discontiguous allocation, and some blocks will have been used up by
            // other things, so allocate the very last block.  Note that changing our allocation
            // strategy might break this test.
            let end = round_down(
                TEST_DEVICE_BLOCK_SIZE as u64 * TEST_DEVICE_BLOCK_COUNT,
                fs.block_size(),
            );
            let item = Item::new(
                AllocatorKey { device_range: end..end },
                AllocatorValue::Abs { count: 2, owner_object_id: 9 },
            );
            writer.write(item.as_item_ref()).await.expect("write failed");
            writer.flush().await.expect("flush failed");
        }
        let mut allocator_info = fs.allocator().info();
        allocator_info.layers.push(layer_handle.object_id());
        let mut allocator_info_vec = vec![];
        allocator_info.serialize_with_version(&mut allocator_info_vec).expect("serialize failed");
        let mut buf = device.allocate_buffer(allocator_info_vec.len()).await;
        buf.as_mut_slice().copy_from_slice(&allocator_info_vec[..]);

        let handle = ObjectStore::open_object(
            &root_store,
            fs.allocator().object_id(),
            HandleOptions::default(),
            None,
        )
        .await
        .expect("open allocator handle failed");
        let mut transaction = handle.new_transaction().await.expect("new_transaction failed");
        handle.txn_write(&mut transaction, 0, buf.as_ref()).await.expect("txn_write failed");
        transaction.commit().await.expect("commit failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { halt_on_error: true, ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Error(FsckError::MalformedAllocation(..))]);
}

#[fuchsia::test]
async fn test_misaligned_extent_in_child_store() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();

        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        transaction.add(
            store.store_object_id(),
            Mutation::insert_object(
                ObjectKey::extent(555, 0, 1..fs.block_size()),
                ObjectValue::Extent(ExtentValue::new_raw(1, VOLUME_DATA_KEY_ID)),
            ),
        );
        transaction.commit().await.expect("commit failed");
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions {
        halt_on_error: true,
        volume_store_id: Some(store_id),
        ..Default::default()
    })
    .await
    .expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Error(FsckError::MisalignedExtent(..))]);
}

#[fuchsia::test]
async fn test_malformed_extent_in_child_store() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();

        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        transaction.add(
            store.store_object_id(),
            Mutation::insert_object(
                ObjectKey::extent(555, 0, fs.block_size()..0),
                ObjectValue::Extent(ExtentValue::new_raw(1, VOLUME_DATA_KEY_ID)),
            ),
        );
        transaction.commit().await.expect("commit failed");
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions {
        halt_on_error: true,
        volume_store_id: Some(store_id),
        ..Default::default()
    })
    .await
    .expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Error(FsckError::MalformedExtent(..))]);
}

#[fuchsia::test]
async fn test_allocation_mismatch() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let allocator = fs.allocator();
        let range = {
            let layer_set = allocator.tree().layer_set();
            let mut merger = layer_set.merger();
            let iter = allocator
                .filter(merger.query(Query::FullScan).await.expect("seek failed"), false)
                .await
                .expect("iter failed");
            let ItemRef { key: AllocatorKey { device_range }, .. } =
                iter.get().expect("missing item");
            device_range.clone()
        };
        // Replace owner_object_id with a different owner and bump count to something impossible.
        allocator.tree().replace_or_insert(Item::new(
            AllocatorKey { device_range: range.clone() },
            AllocatorValue::Abs { count: 2, owner_object_id: 10 },
        ));
        allocator.flush().await.expect("flush failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [
            FsckIssue::Error(FsckError::AllocationForNonexistentOwner(..)),
            FsckIssue::Error(FsckError::MissingAllocation(..)),
            FsckIssue::Error(FsckError::AllocatedBytesMismatch(..)),
        ]
    );
}

#[fuchsia::test]
async fn test_volume_allocation_mismatch() {
    let mut test = FsckTest::new().await;
    let store_id = {
        let fs = test.filesystem();
        let device = fs.device();
        let store_id = {
            let root_volume = root_volume(fs.clone()).await.unwrap();
            let volume = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
            let root_directory = Directory::open(&volume, volume.root_directory_object_id())
                .await
                .expect("open failed");

            let mut transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(
                        volume.store_object_id(),
                        root_directory.object_id()
                    )],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");
            let handle = root_directory
                .create_child_file(&mut transaction, "child_file")
                .await
                .expect("create_child_file failed");
            transaction.commit().await.expect("commit failed");
            let mut transaction = fs
                .clone()
                .new_transaction(
                    lock_keys![LockKey::object(volume.store_object_id(), handle.object_id())],
                    Options::default(),
                )
                .await
                .expect("new_transaction failed");
            let buf = device.allocate_buffer(1).await;
            handle
                .txn_write(&mut transaction, 1_048_576, buf.as_ref())
                .await
                .expect("write failed");
            transaction.commit().await.expect("commit failed");
            volume.flush().await.expect("Flush store failed");
            volume.store_object_id()
        };

        // Find and break first allocation record for the child store.
        let allocator = fs.allocator();
        let range = {
            let layer_set = allocator.tree().layer_set();
            let mut merger = layer_set.merger();
            let mut iter = allocator
                .filter(merger.query(Query::FullScan).await.expect("seek failed"), false)
                .await
                .expect("iter failed");
            loop {
                if let ItemRef {
                    key: AllocatorKey { device_range },
                    value: AllocatorValue::Abs { owner_object_id, .. },
                    ..
                } = iter.get().expect("no allocations found")
                {
                    if *owner_object_id == store_id {
                        break device_range.clone();
                    }
                }
                iter.advance().await.expect("advance failed");
            }
        };
        allocator.tree().replace_or_insert(Item::new(
            AllocatorKey { device_range: range },
            AllocatorValue::Abs { count: 2, owner_object_id: 42 },
        ));
        allocator.flush().await.expect("flush failed");
        store_id
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions {
        skip_system_fsck: true,
        volume_store_id: Some(store_id),
        ..Default::default()
    })
    .await
    .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [
            FsckIssue::Error(FsckError::AllocationForNonexistentOwner(..)),
            FsckIssue::Error(FsckError::MissingAllocation(..)),
            FsckIssue::Error(FsckError::AllocatedBytesMismatch(..)),
        ]
    );
}

#[fuchsia::test]
async fn test_missing_allocation() {
    let test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let allocator = fs.allocator();
        let key = {
            let layer_set = allocator.tree().layer_set();
            let mut merger = layer_set.merger();
            let iter = allocator
                .filter(merger.query(Query::FullScan).await.expect("seek failed"), false)
                .await
                .expect("iter failed");
            let iter = CoalescingIterator::new(iter).await.expect("filter failed");
            let ItemRef { key, .. } = iter.get().expect("missing item");
            // 'key' points at the first allocation record, which will be for the super blocks.
            key.clone()
        };
        let lower_bound = key.lower_bound_for_merge_into();
        allocator.tree().merge_into(Item::new(key, AllocatorValue::None), &lower_bound);
    }
    // We intentionally don't remount here, since the above tree mutation wouldn't persist
    // otherwise.
    // Structuring this test to actually persist a bad allocation layer file is possible but tricky
    // since flushing or committing transactions might itself perform allocations, and it isn't that
    // important.
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [
            FsckIssue::Error(FsckError::MissingAllocation(..)),
            FsckIssue::Error(FsckError::AllocatedBytesMismatch(..)),
        ]
    );
}

#[fuchsia::test]
async fn test_too_many_object_refs() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();

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
        let child_file = root_directory
            .create_child_file(&mut transaction, "child_file")
            .await
            .expect("create_child_file failed");
        let child_dir = root_directory
            .create_child_dir(&mut transaction, "child_dir")
            .await
            .expect("create_child_directory failed");

        // Add an extra reference to the child file.
        child_dir
            .insert_child(&mut transaction, "test", child_file.object_id(), ObjectDescriptor::File)
            .await
            .expect("insert_child failed");
        transaction.commit().await.expect("commit failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Error(FsckError::RefCountMismatch(..)),]);
}

#[fuchsia::test]
async fn test_too_few_object_refs() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let root_store = fs.root_store();

        // Create an object but no directory entry referencing that object, so it will end up with a
        // reference count of one, but zero references.
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
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

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Warning(FsckWarning::OrphanedObject(..))]);
}

#[fuchsia::test]
async fn test_missing_object_tree_layer_file() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let volume = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        ObjectStore::create_object(&volume, &mut transaction, HandleOptions::default(), None, None)
            .await
            .expect("create_object failed");
        transaction.commit().await.expect("commit failed");
        volume.flush().await.expect("Flush store failed");
        let id = {
            let layers = volume.tree().immutable_layer_set();
            assert!(!layers.layers.is_empty());
            layers.layers[0].handle().unwrap().object_id()
        };
        fs.root_store()
            .tombstone_object(id, transaction::Options::default())
            .await
            .expect("tombstone failed");
    }

    test.remount().await.expect_err("Remount succeeded");
}

#[fuchsia::test]
async fn test_missing_object_store_handle() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store_id = {
            let volume = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
            volume.store_object_id()
        };
        fs.root_store()
            .tombstone_object(store_id, transaction::Options::default())
            .await
            .expect("tombstone failed");
    }

    test.remount().await.expect_err("Remount succeeded");
}

#[fuchsia::test]
async fn test_misordered_layer_file() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![
                Item::new(ObjectKey::extent(5, 0, 10..20), ObjectValue::deleted_extent()),
                Item::new(ObjectKey::extent(1, 0, 0..5), ObjectValue::deleted_extent()),
            ],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Fatal(FsckFatal::MisOrderedLayerFile(..))]);
}

#[fuchsia::test]
async fn test_overlapping_keys_in_layer_file() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![
                Item::new(ObjectKey::extent(1, 0, 0..20), ObjectValue::deleted_extent()),
                Item::new(ObjectKey::extent(1, 0, 10..30), ObjectValue::deleted_extent()),
                Item::new(ObjectKey::extent(1, 0, 15..40), ObjectValue::deleted_extent()),
            ],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Fatal(FsckFatal::OverlappingKeysInLayerFile(..))]
    );
}

impl Value for u32 {
    const DELETED_MARKER: Self = 0;
}

#[fuchsia::test]
async fn test_unexpected_record_in_layer_file() {
    let mut test = FsckTest::new().await;
    // This test relies on the value below being something that doesn't deserialize to a valid
    // ObjectValue.
    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![Item::new(ObjectKey::object(0), 0xffffffffu32)],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Fatal(FsckFatal::MalformedLayerFile(..))]);
}

#[fuchsia::test]
async fn test_mismatched_key_and_value() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![Item::new(
                ObjectKey::object(10),
                ObjectValue::Attribute { size: 100, has_overwrite_extents: false },
            )],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Error(FsckError::MalformedObjectRecord(..)), ..]
    );
}

#[fuchsia::test]
async fn test_link_to_root_directory() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let store = fs.root_store();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        root_directory
            .insert_child(
                &mut transaction,
                "a",
                store.root_directory_object_id(),
                ObjectDescriptor::Directory,
            )
            .await
            .expect("insert_child failed");
        transaction.commit().await.expect("commit transaction failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Error(FsckError::RootObjectHasParent(..)), ..]);
}

#[fuchsia::test]
async fn test_multiple_links_to_directory() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let child_dir = root_directory
            .create_child_dir(&mut transaction, "a")
            .await
            .expect("create_child_dir failed");
        root_directory
            .insert_child(&mut transaction, "b", child_dir.object_id(), ObjectDescriptor::Directory)
            .await
            .expect("insert_child failed");
        transaction.commit().await.expect("commit transaction failed");
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Error(FsckError::MultipleLinksToDirectory(..)), ..]
    );
}

#[fuchsia::test]
async fn test_conflicting_link_types() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let store = fs.root_store();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let child_dir = root_directory
            .create_child_dir(&mut transaction, "a")
            .await
            .expect("create_child_dir failed");
        root_directory
            .insert_child(&mut transaction, "b", child_dir.object_id(), ObjectDescriptor::File)
            .await
            .expect("insert_child failed");
        transaction.commit().await.expect("commit transaction failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Error(FsckError::ConflictingTypeForLink(..)), ..]
    );
}

#[fuchsia::test]
async fn test_volume_in_child_store() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        root_directory
            .insert_child(&mut transaction, "a", 10, ObjectDescriptor::Volume)
            .await
            .expect("Create child failed");
        transaction.commit().await.expect("commit transaction failed");
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Error(FsckError::VolumeInChildStore(..)), ..]);
}

#[fuchsia::test]
async fn test_children_on_file() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let object_id = root_directory
            .create_child_file(&mut transaction, "a'")
            .await
            .expect("Create child failed")
            .object_id();
        transaction.commit().await.expect("commit transaction failed");

        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![Item::new(
                ObjectKey::child(object_id, "foo", false),
                ObjectValue::Child(ChildValue {
                    object_id,
                    object_descriptor: ObjectDescriptor::File,
                }),
            )],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Error(FsckError::ObjectHasChildren(..)), ..]);
}

#[fuchsia::test]
async fn test_non_file_marked_as_verified() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", None).await.unwrap();

        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![
                Item::new(
                    ObjectKey::object(10),
                    ObjectValue::Object {
                        kind: ObjectKind::Directory {
                            sub_dirs: 0,
                            wrapping_key_id: None,
                            casefold: false,
                        },
                        attributes: ObjectAttributes { ..Default::default() },
                    },
                ),
                Item::new(
                    ObjectKey::attribute(10, DEFAULT_DATA_ATTRIBUTE_ID, AttributeKey::Attribute),
                    ObjectValue::verified_attribute(
                        0,
                        FsverityMetadata { root_digest: RootDigest::Sha256([0; 32]), salt: vec![] },
                    ),
                ),
            ],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Error(FsckError::NonFileMarkedAsVerified(..)), ..]
    );
}

#[fuchsia::test]
async fn test_verified_file_merkle_attribute_missing() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", None).await.unwrap();

        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![
                Item::new(
                    ObjectKey::object(10),
                    ObjectValue::Object {
                        kind: ObjectKind::File { refs: 1 },
                        attributes: ObjectAttributes { ..Default::default() },
                    },
                ),
                Item::new(
                    ObjectKey::attribute(10, DEFAULT_DATA_ATTRIBUTE_ID, AttributeKey::Attribute),
                    ObjectValue::verified_attribute(
                        0,
                        FsverityMetadata { root_digest: RootDigest::Sha256([0; 32]), salt: vec![] },
                    ),
                ),
            ],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Error(FsckError::VerifiedFileDoesNotHaveAMerkleAttribute(..)), ..]
    );
}

#[fuchsia::test]
async fn test_orphaned_extended_attribute_record() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();

        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![Item::new(
                ObjectKey::extended_attribute(10, b"foo".to_vec()),
                ObjectValue::inline_extended_attribute(b"value".to_vec()),
            )],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Warning(FsckWarning::OrphanedExtendedAttributeRecord(..)), ..]
    );
}

#[fuchsia::test]
async fn test_orphaned_large_extended_attribute_record() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();

        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![Item::new(
                ObjectKey::extended_attribute(10, b"foo".to_vec()),
                ObjectValue::extended_attribute(64),
            )],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Warning(FsckWarning::OrphanedExtendedAttributeRecord(..)), ..]
    );
}

#[fuchsia::test]
async fn test_large_extended_attribute_nonexistent_attribute() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();

        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![Item::new(
                ObjectKey::extended_attribute(store.root_directory_object_id(), b"foo".to_vec()),
                ObjectValue::extended_attribute(64),
            )],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [.., FsckIssue::Error(FsckError::MissingAttributeForExtendedAttribute(..))]
    );
}

#[fuchsia::test]
async fn test_orphaned_extended_attribute() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();

        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![Item::new(
                ObjectKey::attribute(store.root_directory_object_id(), 64, AttributeKey::Attribute),
                ObjectValue::attribute(100, false),
            )],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Warning(FsckWarning::OrphanedExtendedAttribute(..)), ..]
    );
}

#[fuchsia::test]
async fn test_orphaned_attribute() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();

        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![Item::new(
                ObjectKey::attribute(10, 1, AttributeKey::Attribute),
                ObjectValue::attribute(100, false),
            )],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Warning(FsckWarning::OrphanedAttribute(..)), ..]
    );
}

#[fuchsia::test]
async fn test_records_for_tombstoned_object() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();

        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![
                Item::new(ObjectKey::object(10), ObjectValue::None),
                Item::new(
                    ObjectKey::attribute(10, 1, AttributeKey::Attribute),
                    ObjectValue::attribute(100, false),
                ),
            ],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Error(FsckError::TombstonedObjectHasRecords(..)), ..]
    );
}

#[fuchsia::test]
async fn test_invalid_value_graveyard_attribute_entry() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();

        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![
                Item::new(
                    ObjectKey::object(10),
                    ObjectValue::Object {
                        kind: ObjectKind::File { refs: 1 },
                        attributes: ObjectAttributes { ..Default::default() },
                    },
                ),
                Item::new(
                    ObjectKey::attribute(10, 1, AttributeKey::Attribute),
                    ObjectValue::attribute(100, false),
                ),
                Item::new(
                    ObjectKey::graveyard_attribute_entry(
                        store.graveyard_directory_object_id(),
                        10,
                        1,
                    ),
                    ObjectValue::Trim,
                ),
            ],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [
            FsckIssue::Error(FsckError::MissingEncryptionKeys(..)),
            FsckIssue::Error(FsckError::TrimValueForGraveyardAttributeEntry(.., 10, 1,)),
            ..
        ]
    );
}

#[fuchsia::test]
async fn test_tombstoned_attribute_does_not_exist() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();

        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![
                Item::new(
                    ObjectKey::object(10),
                    ObjectValue::Object {
                        kind: ObjectKind::Directory {
                            sub_dirs: 0,
                            wrapping_key_id: None,
                            casefold: false,
                        },
                        attributes: ObjectAttributes { ..Default::default() },
                    },
                ),
                Item::new(
                    ObjectKey::graveyard_attribute_entry(
                        store.graveyard_directory_object_id(),
                        10,
                        1,
                    ),
                    ObjectValue::Some,
                ),
            ],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Error(FsckError::TombstonedAttributeDoesNotExist(.., 10, 1)), ..]
    );
}

#[fuchsia::test]
async fn test_invalid_object_in_store() {
    let mut test = FsckTest::new().await;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();

        install_items_in_store(
            &fs,
            store.as_ref(),
            vec![Item::new(ObjectKey::object(INVALID_OBJECT_ID), ObjectValue::Some)],
        )
        .await;
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Warning(FsckWarning::InvalidObjectIdInStore(..)), ..]
    );
}

#[fuchsia::test]
async fn test_invalid_child_in_store() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let store = fs.root_store();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        root_directory
            .insert_child(&mut transaction, "a", INVALID_OBJECT_ID, ObjectDescriptor::File)
            .await
            .expect("Insert child failed");
        transaction.commit().await.expect("commit transaction failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Warning(FsckWarning::InvalidObjectIdInStore(..)), ..]
    );
}

#[fuchsia::test]
async fn test_link_cycle() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let store = fs.root_store();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let parent = root_directory
            .create_child_dir(&mut transaction, "a")
            .await
            .expect("Create child failed");
        let child =
            parent.create_child_dir(&mut transaction, "b").await.expect("Create child failed");
        child
            .insert_child(&mut transaction, "c", parent.object_id(), ObjectDescriptor::Directory)
            .await
            .expect("Insert child failed");
        transaction.commit().await.expect("commit transaction failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [
            FsckIssue::Error(FsckError::MultipleLinksToDirectory(..)),
            FsckIssue::Error(FsckError::LinkCycle(..)),
            ..
        ]
    );
}

#[fuchsia::test]
async fn test_orphaned_link_cycle() {
    // This checks we catch a cycle where two directories refer to each other as children.
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let store = fs.root_store();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");

        let dir1 = Directory::create(&mut transaction, &store, None).await.expect("create failed");
        let dir2 = Directory::create(&mut transaction, &store, None).await.expect("create failed");

        dir1.insert_child(&mut transaction, "dir2", dir2.object_id(), ObjectDescriptor::Directory)
            .await
            .expect("insert_child failed");
        dir2.insert_child(&mut transaction, "dir1", dir1.object_id(), ObjectDescriptor::Directory)
            .await
            .expect("insert_child failed");

        transaction.commit().await.expect("commit transaction failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(test.errors()[..], [FsckIssue::Error(FsckError::LinkCycle(..)), ..]);
}

#[fuchsia::test]
async fn test_incorrect_merkle_tree_size_empty_file() {
    let mut test = FsckTest::new().await;
    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let object = root_directory
            .create_child_file(&mut transaction, "verified file")
            .await
            .expect("Create child failed");
        transaction.commit_and_continue().await.expect("commit_and_continue transaction failed");

        object
            .enable_verity(fio::VerificationOptions {
                hash_algorithm: Some(fio::HashAlgorithm::Sha256),
                salt: Some(vec![]),
                ..Default::default()
            })
            .await
            .expect("set verified file metadata failed");
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
        transaction.commit().await.expect("commit transaction failed");
        store.store_object_id()
    };
    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should have failed");
    assert_matches!(
        test.errors()[..],
        [.., FsckIssue::Error(FsckError::IncorrectMerkleTreeSize(.., expected_size, 0))]
            if expected_size == <Sha256 as Hasher>::Digest::DIGEST_LEN as u64
    );
}

#[fuchsia::test]
async fn test_incorrect_merkle_tree_size_one_data_block() {
    let mut test = FsckTest::new().await;
    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let object = root_directory
            .create_child_file(&mut transaction, "verified file")
            .await
            .expect("Create child failed");
        transaction.commit_and_continue().await.expect("commit_and_continue transaction failed");

        let mut buf = object.allocate_buffer(fs.block_size() as usize).await;
        buf.as_mut_slice().fill(1);
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
        object
            .enable_verity(fio::VerificationOptions {
                hash_algorithm: Some(fio::HashAlgorithm::Sha256),
                salt: Some(vec![]),
                ..Default::default()
            })
            .await
            .expect("set verified file metadata failed");
        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(
                    object.object_id(),
                    FSVERITY_MERKLE_ATTRIBUTE_ID,
                    AttributeKey::Attribute,
                ),
                ObjectValue::attribute(2 * <Sha256 as Hasher>::Digest::DIGEST_LEN as u64, false),
            ),
        );
        transaction.commit().await.expect("commit transaction failed");
        store.store_object_id()
    };
    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should have failed");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Error(FsckError::IncorrectMerkleTreeSize(.., expected_size, actual_size)), ..]
            if expected_size == <Sha256 as Hasher>::Digest::DIGEST_LEN as u64
                && actual_size == 2 * <Sha256 as Hasher>::Digest::DIGEST_LEN as u64
    );
}

#[fuchsia::test]
async fn test_incorrect_merkle_tree_size_data_unaligned() {
    let mut test = FsckTest::new().await;
    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let object = root_directory
            .create_child_file(&mut transaction, "verified file")
            .await
            .expect("Create child failed");
        transaction.commit_and_continue().await.expect("commit_and_continue transaction failed");

        let mut buf = object.allocate_buffer(1 + 5 * fs.block_size() as usize).await;
        buf.as_mut_slice().fill(1);
        object.write_or_append(Some(0), buf.as_ref()).await.expect("write failed");
        object
            .enable_verity(fio::VerificationOptions {
                hash_algorithm: Some(fio::HashAlgorithm::Sha256),
                salt: Some(vec![]),
                ..Default::default()
            })
            .await
            .expect("set verified file metadata failed");
        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(
                    object.object_id(),
                    FSVERITY_MERKLE_ATTRIBUTE_ID,
                    AttributeKey::Attribute,
                ),
                ObjectValue::attribute(10 * <Sha256 as Hasher>::Digest::DIGEST_LEN as u64, false),
            ),
        );
        transaction.commit().await.expect("commit transaction failed");
        store.store_object_id()
    };
    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should have failed");
    assert_matches!(
        test.errors()[..],
        [
            FsckIssue::Error(FsckError::IncorrectMerkleTreeSize(
                ..,
                expected_size,
                actual_size,
            )),
            ..
        ] if expected_size ==  6 * <Sha256 as Hasher>::Digest::DIGEST_LEN as u64
            && actual_size == 10 * <Sha256 as Hasher>::Digest::DIGEST_LEN as u64
    );
}

#[fuchsia::test]
async fn test_file_length_mismatch() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let device = fs.device();
        let store = fs.root_store();

        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let handle = ObjectStore::create_object(
            &store,
            &mut transaction,
            HandleOptions::default(),
            None,
            None,
        )
        .await
        .expect("create object failed");
        transaction.commit().await.expect("commit transaction failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), handle.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let buf = device.allocate_buffer(1).await;
        handle.txn_write(&mut transaction, 1_048_576, buf.as_ref()).await.expect("write failed");
        transaction.commit().await.expect("commit transaction failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), handle.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(
                    handle.object_id(),
                    handle.attribute_id(),
                    AttributeKey::Attribute,
                ),
                ObjectValue::attribute(123, false),
            ),
        );
        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::object(handle.object_id()),
                ObjectValue::Object {
                    kind: ObjectKind::File { refs: 1 },
                    attributes: ObjectAttributes {
                        creation_time: Timestamp::now(),
                        modification_time: Timestamp::now(),
                        project_id: 0,
                        allocated_size: 123,
                        ..Default::default()
                    },
                },
            ),
        );
        transaction.commit().await.expect("commit transaction failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [
            FsckIssue::Error(FsckError::ExtentExceedsLength(..)),
            FsckIssue::Error(FsckError::AllocatedSizeMismatch(..)),
            ..
        ]
    );
}

#[fuchsia::test]
async fn test_spurious_extents() {
    let mut test = FsckTest::new().await;
    const SPURIOUS_OFFSET: u64 = 100 << 20;

    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();

        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        transaction.add(
            store.store_object_id(),
            Mutation::insert_object(
                ObjectKey::extent(555, 0, 0..4096),
                ObjectValue::Extent(ExtentValue::new_raw(SPURIOUS_OFFSET, VOLUME_DATA_KEY_ID)),
            ),
        );
        transaction.add(
            store.store_object_id(),
            Mutation::insert_object(
                ObjectKey::extent(store.root_directory_object_id(), 0, 0..4096),
                ObjectValue::Extent(ExtentValue::new_raw(SPURIOUS_OFFSET, VOLUME_DATA_KEY_ID)),
            ),
        );
        transaction.commit().await.expect("commit failed");
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    let mut found = 0;
    for e in test.errors() {
        match e {
            FsckIssue::Warning(FsckWarning::ExtentForMissingAttribute(..)) => found |= 1,
            FsckIssue::Warning(FsckWarning::ExtentForNonexistentObject(..)) => found |= 2,
            _ => {}
        }
    }
    assert_eq!(found, 3, "Missing expected errors: {:?}", test.errors());
}

#[fuchsia::test]
async fn test_missing_encryption_key() {
    let mut test = FsckTest::new().await;

    let (store_id, object_id) = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let handle;
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        handle = root_directory
            .create_child_file(&mut transaction, "child_file")
            .await
            .expect("create_child_file failed");
        let buf = handle.allocate_buffer(1).await;
        handle.txn_write(&mut transaction, 1_048_576, buf.as_ref()).await.expect("write failed");

        let txn_mutation = transaction
            .mutations()
            .iter()
            .find(|m| {
                matches!(
                    m.mutation,
                    Mutation::ObjectStore(ObjectStoreMutation {
                        item: Item {
                            key: ObjectKey {
                                data: ObjectKeyData::Attribute(_, AttributeKey::Extent(_)),
                                ..
                            },
                            ..
                        },
                        ..
                    })
                )
            })
            .expect("find failed");

        let mut mutation = txn_mutation.mutation.clone();

        let store_id = store.store_object_id();
        transaction.remove(store_id, mutation.clone());

        if let Mutation::ObjectStore(ObjectStoreMutation {
            item: Item { value: ObjectValue::Extent(ExtentValue::Some { key_id, .. }), .. },
            ..
        }) = &mut mutation
        {
            *key_id += 1;
        } else {
            unreachable!();
        }

        transaction.add(store_id, mutation);

        transaction.commit().await.expect("commit failed");

        (store_id, handle.object_id())
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");

    assert_matches!(&test.errors()[..], [ FsckIssue::Error(FsckError::MissingKey(sid, oid, 1)) ] if *sid == store_id && *oid == object_id);
}

#[fuchsia::test]
async fn test_orphaned_keys() {
    let mut test = FsckTest::new().await;

    let store_id;
    {
        let fs = test.filesystem();
        let root_store = fs.root_store();
        store_id = root_store.store_object_id();

        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![LockKey::object(store_id, 1000)], Options::default())
            .await
            .expect("new_transaction failed");
        transaction.add(
            store_id,
            Mutation::insert_object(
                ObjectKey::keys(1000),
                ObjectValue::Keys(EncryptionKeys::AES256XTS(WrappedKeys::from(vec![]))),
            ),
        );
        transaction.commit().await.expect("commit failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");

    assert_matches!(&test.errors()[..], [FsckIssue::Warning(FsckWarning::OrphanedKeys(sid, 1000))] if *sid == store_id);
}

#[fuchsia::test]
async fn test_missing_encryption_keys() {
    let mut test = FsckTest::new().await;

    let (store_id, object_id) = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let handle;
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        handle = root_directory
            .create_child_file(&mut transaction, "child_file")
            .await
            .expect("create_child_file failed");

        let txn_mutation = transaction
            .mutations()
            .iter()
            .find(|m| {
                matches!(
                    m.mutation,
                    Mutation::ObjectStore(ObjectStoreMutation {
                        item: Item { key: ObjectKey { data: ObjectKeyData::Keys, .. }, .. },
                        ..
                    })
                )
            })
            .expect("find failed");

        let mutation = txn_mutation.mutation.clone();
        let store_id = store.store_object_id();
        transaction.remove(store_id, mutation);

        transaction.commit().await.expect("commit failed");

        (store_id, handle.object_id())
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");

    assert_matches!(&test.errors()[..], [ FsckIssue::Error(FsckError::MissingEncryptionKeys(sid, oid)) ] if *sid == store_id && *oid == object_id);
}

#[fuchsia::test]
async fn test_encrypted_directory_has_unencrypted_child() {
    let mut test = FsckTest::new().await;

    let (store_id, parent_oid, child_oid) = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let crypt = test.get_crypt();
        let store = root_volume.new_volume("vol", Some(crypt.clone())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let handle;
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        handle = root_directory
            .create_child_dir(&mut transaction, "dir")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");
        let transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), handle.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");

        crypt.add_wrapping_key(2, [1; 32]);
        handle
            .update_attributes(
                transaction,
                Some(&fio::MutableNodeAttributes {
                    wrapping_key_id: Some(u128::to_le_bytes(2)),
                    ..Default::default()
                }),
                0,
                None,
            )
            .await
            .expect("update attributes failed");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), handle.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let subdir = handle
            .create_child_dir(&mut transaction, "subdir")
            .await
            .expect("create_child_file failed");

        let txn_mutation = transaction
            .mutations()
            .iter()
            .find(|m| {
                match m.mutation {
                    Mutation::ObjectStore(ObjectStoreMutation {
                        item: Item {
                            key: ObjectKey {
                                object_id,
                                data: ObjectKeyData::EncryptedChild { .. },
                            },
                            ..
                        },
                        ..
                    }) if object_id == handle.object_id() => true,
                    _ => false
                }
            })
            .expect("find failed");

        let mutation = txn_mutation.mutation.clone();
        let store_id = store.store_object_id();
        transaction.remove(store_id, mutation.clone());

        if let Mutation::ObjectStore(ObjectStoreMutation {
            item: Item { value, key: ObjectKey { object_id, .. }, .. },
            ..
        }) = &mutation
        {
            let mutation = Mutation::replace_or_insert_object(
                ObjectKey::child(*object_id, "subdir", false),
                value.clone(),
            );
            transaction.add(store_id, mutation);
            transaction.commit().await.expect("commit failed");
        } else {
            unreachable!();
        }

        (store_id, handle.object_id(), subdir.object_id())
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");

    let expected = [FsckIssue::Error(FsckError::EncryptedDirectoryHasUnencryptedChild(
        store_id, parent_oid, child_oid,
    ))];
    assert_eq!(&test.errors()[..], &expected);
}

#[fuchsia::test]
async fn test_unencrypted_directory_has_encrypted_child() {
    let mut test = FsckTest::new().await;

    let (store_id, parent_oid, child_oid) = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let crypt = test.get_crypt();
        let store = root_volume.new_volume("vol", Some(crypt.clone())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let handle;
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        handle = root_directory
            .create_child_dir(&mut transaction, "dir")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), handle.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");

        let subdir = handle
            .create_child_dir(&mut transaction, "subdir")
            .await
            .expect("create_child_file failed");

        let txn_mutation = transaction
            .mutations()
            .iter()
            .find(|m| match m.mutation {
                Mutation::ObjectStore(ObjectStoreMutation {
                    item:
                        Item {
                            key: ObjectKey { object_id, data: ObjectKeyData::Child { .. } },
                            value: ObjectValue::Child(..),
                            ..
                        },
                    ..
                }) if object_id == handle.object_id() => true,
                _ => false,
            })
            .expect("find failed");

        let mutation = txn_mutation.mutation.clone();
        let store_id = store.store_object_id();
        transaction.remove(store_id, mutation.clone());

        if let Mutation::ObjectStore(ObjectStoreMutation {
            item: Item { value, key: ObjectKey { object_id, .. }, .. },
            ..
        }) = &mutation
        {
            let mutation = Mutation::replace_or_insert_object(
                ObjectKey::encrypted_child(*object_id, [1, 2, 3].to_vec(), 0),
                value.clone(),
            );
            transaction.add(store_id, mutation);
            transaction.commit().await.expect("commit failed");
        } else {
            unreachable!();
        }

        (store_id, handle.object_id(), subdir.object_id())
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");

    assert_matches!(&test.errors()[..], [ FsckIssue::Error(FsckError::UnencryptedDirectoryHasEncryptedChild(sid, oid, oid_child)), .. ] if *sid == store_id && *oid == parent_oid && *oid_child == child_oid);
}

#[fuchsia::test]
async fn test_parent_and_child_encrypted_with_different_wrapping_keys() {
    let mut test = FsckTest::new().await;

    let (store_id, parent_oid, child_oid, parent_wrapping_key_id, child_wrapping_key_id) = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let crypt = test.get_crypt();
        let store = root_volume.new_volume("vol", Some(crypt.clone())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let handle;
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        handle = root_directory
            .create_child_dir(&mut transaction, "dir")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");
        let transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), handle.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");

        crypt.add_wrapping_key(2, [1; 32]);
        handle
            .update_attributes(
                transaction,
                Some(&fio::MutableNodeAttributes {
                    wrapping_key_id: Some(u128::to_le_bytes(2)),
                    ..Default::default()
                }),
                0,
                None,
            )
            .await
            .expect("update attributes failed");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), handle.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");

        let subdir = handle
            .create_child_dir(&mut transaction, "subdir")
            .await
            .expect("create_child_file failed");

        let txn_mutation = transaction
            .mutations()
            .iter()
            .find(|m| match m.mutation {
                Mutation::ObjectStore(ObjectStoreMutation {
                    item:
                        Item {
                            key: ObjectKey { object_id, data: ObjectKeyData::Object },
                            value:
                                ObjectValue::Object {
                                    kind: ObjectKind::Directory { wrapping_key_id, .. },
                                    ..
                                },
                            ..
                        },
                    ..
                }) if object_id == subdir.object_id() && wrapping_key_id.is_some() => true,
                _ => false,
            })
            .expect("find failed");

        let mut mutation = txn_mutation.mutation.clone();
        let store_id = store.store_object_id();
        transaction.remove(store_id, mutation.clone());

        if let Mutation::ObjectStore(ObjectStoreMutation {
            item:
                Item {
                    value:
                        ObjectValue::Object {
                            kind: ObjectKind::Directory { wrapping_key_id, .. }, ..
                        },
                    ..
                },
            ..
        }) = &mut mutation
        {
            *wrapping_key_id = Some(3);
        } else {
            unreachable!();
        }
        transaction.add(store_id, mutation);

        transaction.commit().await.expect("commit failed");

        (store_id, handle.object_id(), subdir.object_id(), 2, 3)
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");

    assert_matches!(&test.errors()[..], [ FsckIssue::Error(FsckError::ChildEncryptedWithDifferentWrappingKeyThanParent(sid, oid, oid_child, parent_id, child_id)) ]
    if *sid == store_id && *oid == parent_oid && *oid_child == child_oid && *parent_id == parent_wrapping_key_id && *child_id == child_wrapping_key_id);
}

#[fuchsia::test]
async fn test_encrypted_directory_no_wrapping_key() {
    let mut test = FsckTest::new().await;

    let (store_id, child_oid) = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let crypt = test.get_crypt();
        let store = root_volume.new_volume("vol", Some(crypt.clone())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let handle;
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        handle = root_directory
            .create_child_dir(&mut transaction, "dir")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");
        let transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), handle.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");

        crypt.add_wrapping_key(2, [1; 32]);
        handle
            .update_attributes(
                transaction,
                Some(&fio::MutableNodeAttributes {
                    wrapping_key_id: Some(u128::to_le_bytes(2)),
                    ..Default::default()
                }),
                0,
                None,
            )
            .await
            .expect("update attributes failed");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), handle.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let subdir = handle
            .create_child_dir(&mut transaction, "subdir")
            .await
            .expect("create_child_file failed");

        let txn_mutation = transaction
            .mutations()
            .iter()
            .find(|m| match m.mutation {
                Mutation::ObjectStore(ObjectStoreMutation {
                    item:
                        Item {
                            key: ObjectKey { object_id, data: ObjectKeyData::Object },
                            value:
                                ObjectValue::Object {
                                    kind: ObjectKind::Directory { wrapping_key_id, .. },
                                    ..
                                },
                            ..
                        },
                    ..
                }) if object_id == subdir.object_id() && wrapping_key_id.is_some() => true,
                _ => false,
            })
            .expect("find failed");

        let mut mutation = txn_mutation.mutation.clone();
        let store_id = store.store_object_id();
        transaction.remove(store_id, mutation.clone());

        if let Mutation::ObjectStore(ObjectStoreMutation {
            item:
                Item {
                    value:
                        ObjectValue::Object {
                            kind: ObjectKind::Directory { wrapping_key_id, .. }, ..
                        },
                    ..
                },
            ..
        }) = &mut mutation
        {
            *wrapping_key_id = None;
        } else {
            unreachable!();
        }
        transaction.add(store_id, mutation);

        transaction.commit().await.expect("commit failed");

        (store_id, subdir.object_id())
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");

    assert_matches!(&test.errors()[..], [FsckIssue::Error(FsckError::EncryptedChildDirectoryNoWrappingKey(sid, oid)) ]
    if *sid == store_id && *oid == child_oid);
}

#[fuchsia::test]
async fn test_directory_missing_encryption_key_for_large_extended_attribute() {
    let mut test = FsckTest::new().await;

    let (store_id, object_id) = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let crypt = test.get_crypt();
        let store = root_volume.new_volume("vol", Some(crypt.clone())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let handle;
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        handle = root_directory
            .create_child_dir(&mut transaction, "dir")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), handle.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(handle.object_id(), 10, AttributeKey::Attribute),
                ObjectValue::attribute(300, false),
            ),
        );
        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::extended_attribute(handle.object_id(), b"foo".to_vec()),
                ObjectValue::extended_attribute(10),
            ),
        );
        transaction.commit().await.expect("commit failed");
        (store.store_object_id(), handle.object_id())
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");

    assert_matches!(&test.errors()[..], [ FsckIssue::Error(FsckError::MissingKey(sid, oid, 0)) ] if *sid == store_id && *oid == object_id);
}

#[fuchsia::test]
async fn test_directory_missing_encryption_key_for_fscrypt() {
    let mut test = FsckTest::new().await;

    let (store_id, object_id) = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let crypt = test.get_crypt();
        let store = root_volume.new_volume("vol", Some(crypt.clone())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let handle;
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        handle = root_directory
            .create_child_dir(&mut transaction, "dir")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), handle.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");

        crypt.add_wrapping_key(2, [1; 32]);
        handle.set_wrapping_key(&mut transaction, 2).await.expect("failed to set wrapping key");

        let txn_mutation = transaction
            .mutations()
            .iter()
            .find(|m| match &m.mutation {
                Mutation::ObjectStore(ObjectStoreMutation {
                    item:
                        Item {
                            key: ObjectKey { data: ObjectKeyData::Keys, .. },
                            value: ObjectValue::Keys(EncryptionKeys::AES256XTS(keys)),
                            ..
                        },
                    ..
                }) => {
                    assert!(keys.iter().find(|x| x.0 == 1).is_some());
                    true
                }
                _ => false,
            })
            .expect("find failed");

        let mut mutation = txn_mutation.mutation.clone();
        let store_id = store.store_object_id();
        transaction.remove(store_id, mutation.clone());

        if let Mutation::ObjectStore(ObjectStoreMutation {
            item: Item { value: ObjectValue::Keys(EncryptionKeys::AES256XTS(keys)), .. },
            ..
        }) = &mut mutation
        {
            use std::ops::DerefMut as _;
            let keys = keys.deref_mut();
            let idx = keys.iter().position(|x| x.0 == 1).unwrap();
            let (_, wrapped_key) = keys.remove(idx);
            keys.push((0, wrapped_key));
        } else {
            unreachable!();
        }

        transaction.add(store_id, mutation);

        transaction.commit().await.expect("commit failed");

        (store_id, handle.object_id())
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");

    assert_matches!(&test.errors()[..], [ FsckIssue::Error(FsckError::MissingKey(sid, oid, 1)) ] if *sid == store_id && *oid == object_id);
}

#[fuchsia::test]
async fn test_duplicate_key() {
    let mut test = FsckTest::new().await;

    let (store_id, object_id, key_id) = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        let handle;
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        handle = root_directory
            .create_child_file(&mut transaction, "child_file")
            .await
            .expect("create_child_file failed");

        let txn_mutation = transaction
            .mutations()
            .iter()
            .find(|m| {
                matches!(
                    m.mutation,
                    Mutation::ObjectStore(ObjectStoreMutation {
                        item: Item { key: ObjectKey { data: ObjectKeyData::Keys, .. }, .. },
                        ..
                    })
                )
            })
            .expect("find failed");

        let mut mutation = txn_mutation.mutation.clone();
        let store_id = store.store_object_id();
        transaction.remove(store_id, mutation.clone());

        let key_id;
        if let Mutation::ObjectStore(ObjectStoreMutation {
            item: Item { value: ObjectValue::Keys(EncryptionKeys::AES256XTS(keys)), .. },
            ..
        }) = &mut mutation
        {
            use std::ops::DerefMut as _;
            let keys = keys.deref_mut();
            let duplicate = keys.first().unwrap().clone();
            key_id = duplicate.0;
            keys.push(duplicate);
        } else {
            unreachable!();
        }

        transaction.add(store_id, mutation);

        transaction.commit().await.expect("commit failed");

        (store_id, handle.object_id(), key_id)
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");

    assert_matches!(&test.errors()[..], [ FsckIssue::Error(FsckError::DuplicateKey(sid, oid, kid)) ] if *sid == store_id && *oid == object_id && *kid == key_id);
}

#[fuchsia::test]
async fn test_project_accounting() {
    let mut test = FsckTest::new().await;

    let store_id;
    let orphaned_object_id;
    {
        let fs = test.filesystem();
        let store = fs.root_store();
        store_id = store.store_object_id();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");

        // Project 3 in use on file, no info on it.
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store_id, root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let object_id = root_directory
            .create_child_file(&mut transaction, "a")
            .await
            .expect("Create child failed")
            .object_id();
        let mut mutation = transaction
            .get_object_mutation(store_id, ObjectKey::object(object_id))
            .unwrap()
            .clone();
        if let ObjectValue::Object { attributes: ObjectAttributes { project_id, .. }, .. } =
            &mut mutation.item.value
        {
            *project_id = 3;
        } else {
            panic!("Unexpected object type");
        }
        orphaned_object_id = object_id;
        transaction.add(store_id, Mutation::ObjectStore(mutation));
        transaction.commit().await.expect("commit failed");

        // Project 4 with mismatched actual and usage.
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(store_id, root_directory.object_id()),
                    LockKey::ProjectId { store_object_id: store_id, project_id: 4 },
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        transaction.add(
            store_id,
            Mutation::merge_object(
                ObjectKey::project_usage(root_directory.object_id(), 4),
                ObjectValue::BytesAndNodes { bytes: 0, nodes: 2 },
            ),
        );
        transaction.add(
            store_id,
            Mutation::insert_object(
                ObjectKey::project_limit(root_directory.object_id(), 4),
                ObjectValue::BytesAndNodes { bytes: 0, nodes: 0 },
            ),
        );
        let object_id = root_directory
            .create_child_file(&mut transaction, "b")
            .await
            .expect("Create child failed")
            .object_id();
        let mut mutation = transaction
            .get_object_mutation(store_id, ObjectKey::object(object_id))
            .unwrap()
            .clone();
        if let ObjectValue::Object { attributes: ObjectAttributes { project_id, .. }, .. } =
            &mut mutation.item.value
        {
            *project_id = 4;
        } else {
            panic!("Unexpected object type");
        }
        transaction.add(store_id, Mutation::ObjectStore(mutation));
        transaction.commit().await.expect("commit failed");

        // Project 5 just fine.
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(store_id, root_directory.object_id()),
                    LockKey::ProjectId { store_object_id: store_id, project_id: 5 },
                ],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        transaction.add(
            store_id,
            Mutation::merge_object(
                ObjectKey::project_usage(root_directory.object_id(), 5),
                ObjectValue::BytesAndNodes { bytes: 0, nodes: 1 },
            ),
        );
        transaction.add(
            store_id,
            Mutation::insert_object(
                ObjectKey::project_limit(root_directory.object_id(), 5),
                ObjectValue::BytesAndNodes { bytes: 0, nodes: 0 },
            ),
        );
        let object_id = root_directory
            .create_child_file(&mut transaction, "c")
            .await
            .expect("Create child failed")
            .object_id();
        let mut mutation = transaction
            .get_object_mutation(store_id, ObjectKey::object(object_id))
            .unwrap()
            .clone();
        if let ObjectValue::Object { attributes: ObjectAttributes { project_id, .. }, .. } =
            &mut mutation.item.value
        {
            *project_id = 5;
        } else {
            panic!("Unexpected object type");
        }
        transaction.add(store_id, Mutation::ObjectStore(mutation));
        transaction.commit().await.expect("commit failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");

    assert!(test.errors().contains(&FsckIssue::Error(FsckError::ProjectUsedWithNoUsageTracking(
        store_id,
        3,
        orphaned_object_id
    ))));
    assert!(test.errors().contains(&FsckIssue::Warning(FsckWarning::ProjectUsageInconsistent(
        store_id,
        4,
        (0, 2),
        (0, 1)
    ))));
    assert_eq!(test.errors().len(), 2);
}

#[fuchsia::test]
async fn test_zombie_file() {
    let mut test = FsckTest::new().await;

    let (store_id, object_id, root_object_id) = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        let handle;
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        handle = root_directory
            .create_child_file(&mut transaction, "child_file")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        store.add_to_graveyard(&mut transaction, handle.object_id());
        transaction.commit().await.expect("commit failed");
        (store.store_object_id(), handle.object_id(), root_directory.object_id())
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        &test.errors()[..],
        [
            FsckIssue::Error(FsckError::RefCountMismatch(object_id_1, 2, 1)),
            FsckIssue::Error(FsckError::ZombieFile(_, object_id_2, root_oids)),
        ] if object_id == *object_id_1 && object_id == *object_id_2 && root_oids == &[root_object_id]
    );
}

#[fuchsia::test]
async fn test_zombie_dir() {
    let mut test = FsckTest::new().await;

    let (store_id, object_id, root_object_id) = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        let handle;
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        handle = root_directory
            .create_child_dir(&mut transaction, "child_dir")
            .await
            .expect("create_child_dir failed");
        transaction.commit().await.expect("commit failed");

        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        store.add_to_graveyard(&mut transaction, handle.object_id());
        transaction.commit().await.expect("commit failed");
        (store.store_object_id(), handle.object_id(), root_directory.object_id())
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [
            FsckIssue::Error(FsckError::ZombieDir(_, object_id_1, root_oid)),
        ] if object_id == object_id_1 && root_oid == root_object_id
    );
}

#[fuchsia::test]
async fn test_zombie_symlink() {
    let mut test = FsckTest::new().await;

    let (store_id, object_id, root_object_id) = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", Some(test.get_crypt())).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let symlink_object_id = root_directory
            .create_symlink(&mut transaction, b"target", "child_symlink")
            .await
            .expect("create_symlink failed");
        transaction.commit().await.expect("commit failed");

        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        store.add_to_graveyard(&mut transaction, symlink_object_id);
        transaction.commit().await.expect("commit failed");
        (store.store_object_id(), symlink_object_id, root_directory.object_id())
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect_err("Fsck should fail");
    assert_matches!(
        &test.errors()[..],
        [
            FsckIssue::Error(FsckError::ZombieSymlink(_, object_id_1, root_oid)),
        ] if object_id == *object_id_1 && root_oid == &[root_object_id]
    );
}

#[fuchsia::test]
async fn test_empty_volume() {
    let mut test = FsckTest::new().await;
    let store_id = {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", None).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        let object_id = {
            let file;
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
            file = root_directory
                .create_child_file(&mut transaction, "child_file")
                .await
                .expect("create_child_file failed");
            let buffer = file.allocate_buffer(1).await;
            file.txn_write(&mut transaction, 0, buffer.as_ref()).await.expect("write failed");
            transaction.commit().await.expect("commit failed");
            file.object_id()
        };
        let mut transaction = root_directory
            .acquire_context_for_replace(None, "child_file", true)
            .await
            .expect("acquire_context_for_replace failed")
            .transaction;

        directory::replace_child(&mut transaction, None, (&root_directory, "child_file"))
            .await
            .expect("failed to unlink");
        transaction.commit().await.expect("commit failed");
        fs.graveyard().queue_tombstone_object(store.store_object_id(), object_id);
        // Make sure the graveyard processes the message so the bytes are deallocated.
        fs.graveyard().flush().await;
        fs.sync(SyncOptions { flush_device: true, ..Default::default() })
            .await
            .expect("sync failed");
        store.store_object_id()
    };

    test.remount().await.expect("Remount failed");
    test.run(TestOptions { volume_store_id: Some(store_id), ..Default::default() })
        .await
        .expect("Fsck should succeed");
}

#[fuchsia::test]
async fn test_full_disk() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let root_store = fs.root_store();
        let device = fs.device();

        let mut transaction = fs
            .clone()
            .new_transaction(lock_keys![], Options::default())
            .await
            .expect("new_transaction failed");
        let layer_handle = ObjectStore::create_object(
            &root_store,
            &mut transaction,
            HandleOptions::default(),
            None,
            None,
        )
        .await
        .expect("create_object failed");
        transaction.commit().await.expect("commit failed");

        // Now write out our 'fill the disk' allocation.
        {
            let mut writer = PersistentLayerWriter::<_, AllocatorKey, AllocatorValue>::new(
                Writer::new(&layer_handle).await,
                1,
                fs.block_size(),
            )
            .await
            .expect("writer new");
            let end = round_down(
                TEST_DEVICE_BLOCK_SIZE as u64 * TEST_DEVICE_BLOCK_COUNT,
                fs.block_size(),
            );
            let item = Item::new(
                AllocatorKey { device_range: 0..end },
                AllocatorValue::Abs { count: 2, owner_object_id: 9 },
            );
            writer.write(item.as_item_ref()).await.expect("write failed");
            writer.flush().await.expect("flush failed");
        }
        // Discard the mutable layer which contains mutations associated with the write itself.
        fs.allocator()
            .tree()
            .set_mutable_layer(crate::lsm_tree::skip_list_layer::SkipListLayer::new(1024));

        fs.sync(SyncOptions { flush_device: true, ..Default::default() }).await.expect("sync");

        let layer_handle_object_id = layer_handle.object_id();
        let allocator_info = fs.allocator().info();
        let mut allocator_info_vec = vec![];
        allocator_info.serialize_with_version(&mut allocator_info_vec).expect("serialize failed");
        allocator_info_vec.resize(4096, 0);
        let mut buf = device.allocate_buffer(allocator_info_vec.len()).await;
        buf.as_mut_slice().copy_from_slice(&allocator_info_vec[..]);

        let handle = ObjectStore::open_object(
            &root_store,
            fs.allocator().object_id(),
            HandleOptions::default(),
            None,
        )
        .await
        .expect("open allocator handle failed");
        handle.overwrite(0, buf.as_mut(), true).await.expect("overwrite failed");

        // Add "layer_handle" to the layer stack for the allocator but be careful not to
        // allocate anything in the process.
        let mut allocator_info = fs.allocator().info();
        allocator_info.layers.push(layer_handle_object_id);
        let mut allocator_info_vec = vec![];
        allocator_info.serialize_with_version(&mut allocator_info_vec).expect("serialize failed");
        allocator_info_vec.resize(4096 * 4, 0);
        let mut buf = device.allocate_buffer(allocator_info_vec.len()).await;
        buf.as_mut_slice().copy_from_slice(&allocator_info_vec[..]);

        let handle = ObjectStore::open_object(
            &root_store,
            fs.allocator().object_id(),
            HandleOptions::default(),
            None,
        )
        .await
        .expect("open allocator handle failed");
        handle.overwrite(0, buf.as_mut(), true).await.expect("overwrite failed");
    }

    test.remount().await.expect_err("Remount succeeded");
}

#[fuchsia::test]
async fn test_delete_volume() {
    let mut test = FsckTest::new().await;
    {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let store = root_volume.new_volume("vol", None).await.unwrap();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        root_directory
            .create_child_file(&mut transaction, "child_file")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");
        let fs_clone = fs.clone();
        // Compact in one task while mutating the new store in another task.  This will ensure that
        // we write out a superblock which referencs the newly created volume in
        // journal_file_offsets.
        join!(
            async move {
                fs_clone.journal().compact().await.expect("compact failed");
            },
            async move {
                for i in 0..50 {
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
                        .create_child_file(&mut transaction, &format!("child_file_{i}"))
                        .await
                        .expect("create_child_file failed");
                    transaction.commit().await.expect("commit failed");
                }
            },
        );
    };
    {
        let fs = test.filesystem();
        let root_volume = root_volume(fs.clone()).await.unwrap();
        let transaction = fs
            .new_transaction(
                lock_keys![LockKey::object(
                    root_volume.volume_directory().store().store_object_id(),
                    root_volume.volume_directory().object_id(),
                )],
                Options { borrow_metadata_space: true, ..Default::default() },
            )
            .await
            .expect("new_transaction failed");
        root_volume.delete_volume("vol", transaction, || {}).await.expect("delete_volume failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect("Fsck should succeed");
}

#[fuchsia::test]
async fn test_casefold() {
    let mut test = FsckTest::new().await;

    for dir_is_casefold in [false, true] {
        let fs = test.filesystem();
        let store = fs.root_store();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        let dirname = if dir_is_casefold { "casefolded" } else { "regular" };

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let child_dir = root_directory
            .create_child_dir(&mut transaction, dirname)
            .await
            .expect("create_child_dir failed");
        transaction.commit().await.expect("commit transaction failed");

        child_dir.set_casefold(dir_is_casefold).await.expect("enable casefold");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), child_dir.object_id()),],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");

        // Manually add a child entry so we can add the wrong ObjectKeyData child type.
        let handle = Directory::create_with_options(&mut transaction, &store, None, true)
            .await
            .expect("create_directory");
        transaction.add(
            child_dir.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::child(child_dir.object_id(), "b", !dir_is_casefold),
                ObjectValue::child(handle.object_id(), ObjectDescriptor::Directory),
            ),
        );
        let now = Timestamp::now();
        child_dir
            .update_dir_attributes_internal(
                &mut transaction,
                child_dir.object_id(),
                MutableAttributesInternal::new(1, Some(now), Some(now.as_nanos()), None),
            )
            .await
            .expect("update attributes");
        transaction.commit().await.expect("commit transaction failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Error(FsckError::CasefoldInconsistency(..)), ..]
    );
}

#[fuchsia::test]
async fn test_missing_overwrite_extents() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let store = fs.root_store();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let file = root_directory
            .create_child_file(&mut transaction, "child_file")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), file.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(file.object_id(), 0, AttributeKey::Attribute),
                ObjectValue::attribute(0, true),
            ),
        );
        transaction.commit().await.expect("commit failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Error(FsckError::MissingOverwriteExtents(..)), ..]
    );
}

#[fuchsia::test]
async fn test_overwrite_extent_flag_not_set() {
    let mut test = FsckTest::new().await;

    {
        let fs = test.filesystem();
        let store = fs.root_store();
        let root_directory =
            Directory::open(&store, store.root_directory_object_id()).await.expect("open failed");
        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), root_directory.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        let file = root_directory
            .create_child_file(&mut transaction, "child_file")
            .await
            .expect("create_child_file failed");
        transaction.commit().await.expect("commit failed");

        file.allocate(0..fs.block_size()).await.expect("allocate failed");

        let mut transaction = fs
            .clone()
            .new_transaction(
                lock_keys![LockKey::object(store.store_object_id(), file.object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");
        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::attribute(file.object_id(), 0, AttributeKey::Attribute),
                ObjectValue::attribute(fs.block_size(), false),
            ),
        );
        transaction.commit().await.expect("commit failed");
    }

    test.remount().await.expect("Remount failed");
    test.run(TestOptions::default()).await.expect_err("Fsck should fail");
    assert_matches!(
        test.errors()[..],
        [FsckIssue::Error(FsckError::OverwriteExtentFlagUnset(..)), ..]
    );
}
