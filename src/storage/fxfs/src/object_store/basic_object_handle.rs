// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        errors::FxfsError,
        lsm_tree::types::LayerIterator,
        object_store::{
            object_record::{ExtendedAttributeValue, ObjectKey, ObjectKeyData, ObjectValue},
            transaction::{LockKey, Mutation, Options},
            ObjectStore,
        },
    },
    anyhow::{anyhow, bail, ensure, Error},
    std::{ops::Bound, sync::Arc},
};

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
                bail!(anyhow!(FxfsError::NotSupported))
            }
            // If an extended attribute has a value of None, it means it was deleted but hasn't
            // been cleaned up yet.
            ObjectValue::None => {
                bail!(anyhow!(FxfsError::NotFound))
            }
            _ => {
                bail!(anyhow!(FxfsError::Inconsistent)
                    .context("get_extended_attribute: Expected ExtendedAttribute value"))
            }
        }
    }

    pub async fn set_extended_attribute(&self, name: Vec<u8>, value: Vec<u8>) -> Result<(), Error> {
        // TODO(fxbug.dev/122123): support large extended attributes that shouldn't be stored inline
        ensure!(value.len() < 256, FxfsError::TooBig);
        let store = self.store();
        let keys = [LockKey::object(store.store_object_id(), self.object_id)];
        let fs = store.filesystem();
        let mut transaction = fs.new_transaction(&keys, Options::default()).await?;
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::extended_attribute(self.object_id, name),
                ObjectValue::inline_extended_attribute(value),
            ),
        );
        transaction.commit().await?;
        Ok(())
    }

    pub async fn remove_extended_attribute(&self, name: Vec<u8>) -> Result<(), Error> {
        let store = self.store();
        let keys = [LockKey::object(store.store_object_id(), self.object_id)];
        let fs = store.filesystem();
        let mut transaction = fs.new_transaction(&keys, Options::default()).await?;
        // TODO(fxbug.dev/122123): support trimming extents for large attributes.
        transaction.add(
            self.store().store_object_id(),
            Mutation::replace_or_insert_object(
                ObjectKey::extended_attribute(self.object_id, name),
                ObjectValue::None,
            ),
        );
        transaction.commit().await?;
        Ok(())
    }
}
