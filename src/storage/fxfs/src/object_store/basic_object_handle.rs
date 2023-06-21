// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        errors::FxfsError,
        lsm_tree::types::{ItemRef, LayerIterator},
        object_store::{
            object_record::{ExtendedAttributeValue, ObjectKey, ObjectKeyData, ObjectValue},
            transaction::{LockKey, Mutation, Options},
            ObjectStore,
        },
    },
    anyhow::{anyhow, bail, ensure, Error},
    fidl_fuchsia_io as fio,
    std::{ops::Bound, sync::Arc},
};

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

/// An ObjectHandle implementation that can represent any object capable of having attributes. The
/// attributes are written and read wholesale, so it's not suitable for reading and writing the
/// data attribute for files.
// TODO(sdemos): extend this with extent reading and writing, right now it just handles inline
// extended attributes.
pub struct BasicObjectHandle<S: AsRef<ObjectStore> + Send + Sync + 'static> {
    owner: Arc<S>,
    object_id: u64,
}

impl<S: AsRef<ObjectStore> + Send + Sync + 'static> BasicObjectHandle<S> {
    /// Make a new BasicObjectHandle for the object with id [`object_id`] in store [`owner`].
    pub fn new(owner: Arc<S>, object_id: u64) -> Self {
        Self { owner, object_id }
    }

    pub fn owner(&self) -> &Arc<S> {
        &self.owner
    }

    pub fn store(&self) -> &ObjectStore {
        self.owner.as_ref().as_ref()
    }

    pub async fn list_extended_attributes(&self) -> Result<Vec<Vec<u8>>, Error> {
        let layer_set = self.store().tree().layer_set();
        let mut merger = layer_set.merger();
        // Seek to the first extended attribute key for this object.
        let mut iter = merger
            .seek(Bound::Included(&ObjectKey::extended_attribute(self.object_id, Vec::new())))
            .await?;
        let mut out = Vec::new();
        while let Some(item) = iter.get() {
            // Skip deleted extended attributes.
            if item.value != &ObjectValue::None {
                match item.key {
                    ObjectKey { object_id, data: ObjectKeyData::ExtendedAttribute { name } } => {
                        if self.object_id != *object_id {
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

    pub async fn get_extended_attribute(&self, name: Vec<u8>) -> Result<Vec<u8>, Error> {
        let item = self
            .store()
            .tree()
            .find(&ObjectKey::extended_attribute(self.object_id, name))
            .await?
            .ok_or(anyhow!(FxfsError::NotFound))?;
        match item.value {
            ObjectValue::ExtendedAttribute(ExtendedAttributeValue::Inline(value)) => Ok(value),
            // TODO(fxbug.dev/122123): support reading from an attribute for large values.
            ObjectValue::ExtendedAttribute(ExtendedAttributeValue::AttributeId(_id)) => {
                bail!(FxfsError::NotSupported)
            }
            // If an extended attribute has a value of None, it means it was deleted but hasn't
            // been cleaned up yet.
            ObjectValue::None => {
                bail!(FxfsError::NotFound)
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
        // TODO(fxbug.dev/122123): support large extended attributes that shouldn't be stored inline
        ensure!(value.len() < 256, FxfsError::TooBig);

        let store = self.store();
        let fs = store.filesystem();
        let tree = store.tree();
        let object_key = ObjectKey::extended_attribute(self.object_id, name);

        // NB: We need to take this lock before we potentially look up the value to prevent racing
        // with another set.
        let keys = [LockKey::object(store.store_object_id(), self.object_id)];
        let mut transaction = fs.new_transaction(&keys, Options::default()).await?;

        if mode != SetExtendedAttributeMode::Set {
            let layer_set = tree.layer_set();
            let mut merger = layer_set.merger();
            let iter = merger.seek(Bound::Included(&object_key)).await?;
            let found = match iter.get() {
                Some(ItemRef { key, value: _, sequence: _ }) => key == &object_key,
                _ => false,
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
        }

        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                object_key,
                ObjectValue::inline_extended_attribute(value),
            ),
        );
        transaction.commit().await?;
        Ok(())
    }

    pub async fn remove_extended_attribute(&self, name: Vec<u8>) -> Result<(), Error> {
        let store = self.store();
        let fs = store.filesystem();
        let tree = store.tree();
        let object_key = ObjectKey::extended_attribute(self.object_id, name);

        // NB: The API says we have to return an error if the attribute doesn't exist, so we have
        // to look it up first to make sure we have a record of it before we delete it. Make sure
        // we take a lock and make a transaction before we do so we don't race with other
        // operations.
        let keys = [LockKey::object(store.store_object_id(), self.object_id)];
        let mut transaction = fs.new_transaction(&keys, Options::default()).await?;

        {
            let layer_set = tree.layer_set();
            let mut merger = layer_set.merger();
            let iter = merger.seek(Bound::Included(&object_key)).await?;
            let value = match iter.get() {
                Some(ItemRef { key, value, sequence: _ }) if key == &object_key => value,
                _ => bail!(FxfsError::NotFound),
            };
            match value {
                // We don't care what kind of attribute value it is.
                ObjectValue::ExtendedAttribute(_) => (),
                // If an extended attribute has a value of None, it means it was deleted already,
                // but hasn't been compacted away yet.
                ObjectValue::None => bail!(FxfsError::NotFound),
                _ => {
                    bail!(anyhow!(FxfsError::Inconsistent)
                        .context("remove_extended_attribute: Expected ExtendedAttribute value"))
                }
            }
        }

        // TODO(fxbug.dev/122123): support trimming extents for large attributes.
        transaction.add(
            store.store_object_id(),
            Mutation::replace_or_insert_object(object_key, ObjectValue::None),
        );
        transaction.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::{
            errors::FxfsError,
            filesystem::{Filesystem, FxFilesystem, OpenFxFilesystem},
            object_handle::ObjectHandle,
            object_store::{
                transaction::{Options, TransactionHandler},
                BasicObjectHandle, Directory, HandleOptions, LockKey, ObjectStore,
                SetExtendedAttributeMode,
            },
        },
        fuchsia_async as fasync,
        futures::join,
        fxfs_insecure_crypto::InsecureCrypt,
        std::sync::Arc,
        storage_device::{fake_device::FakeDevice, DeviceHolder},
    };

    const TEST_DEVICE_BLOCK_SIZE: u32 = 512;
    const TEST_OBJECT_NAME: &str = "foo";

    fn is_error(actual: anyhow::Error, expected: FxfsError) {
        assert_eq!(*actual.root_cause().downcast_ref::<FxfsError>().unwrap(), expected)
    }

    async fn test_filesystem() -> OpenFxFilesystem {
        let device = DeviceHolder::new(FakeDevice::new(8192, TEST_DEVICE_BLOCK_SIZE));
        FxFilesystem::new_empty(device).await.expect("new_empty failed")
    }

    async fn test_filesystem_and_empty_object(
    ) -> (OpenFxFilesystem, Arc<BasicObjectHandle<ObjectStore>>) {
        let fs = test_filesystem().await;
        let store = fs.root_store();
        let object;

        let mut transaction = fs
            .clone()
            .new_transaction(
                &[LockKey::object(store.store_object_id(), store.root_directory_object_id())],
                Options::default(),
            )
            .await
            .expect("new_transaction failed");

        object = ObjectStore::create_object(
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

        (fs, Arc::new(BasicObjectHandle::new(object.owner().clone(), object.object_id())))
    }

    #[fuchsia::test(threads = 3)]
    async fn extended_attribute_double_remove() {
        // This test is intended to trip a potential race condition in remove. Removing an
        // attribute that doesn't exist is an error, so we need to check before we remove, but if
        // we aren't careful, two parallel removes might both succeed in the check and then both
        // remove the value.
        let (fs, basic) = test_filesystem_and_empty_object().await;
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
        let (fs, basic) = test_filesystem_and_empty_object().await;
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
}
