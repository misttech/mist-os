// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuchsia::{
        directory::FxDirectory, errors::map_to_status, node::FxNode, volume::FxVolume,
    },
    anyhow::{bail, Error},
    async_trait::async_trait,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    fxfs::{
        errors::FxfsError,
        object_handle::{ObjectHandle, ObjectProperties},
        object_store::{
            transaction::{LockKey, Options},
            HandleOptions, ObjectAttributes, ObjectDescriptor, ObjectKey, ObjectKind, ObjectValue,
            StoreObjectHandle,
        },
    },
    std::sync::Arc,
    vfs::{
        attributes, directory::entry_container::MutableDirectory, name::Name, node::Node,
        symlink::Symlink,
    },
};

pub struct FxSymlink {
    handle: StoreObjectHandle<FxVolume>,
}

impl FxSymlink {
    pub fn new(volume: Arc<FxVolume>, object_id: u64) -> Self {
        Self {
            handle: StoreObjectHandle::new(
                volume,
                object_id,
                /* permanent_keys: */ false,
                HandleOptions::default(),
                /* trace: */ false,
            ),
        }
    }
}

#[async_trait]
impl Symlink for FxSymlink {
    async fn read_target(&self) -> Result<Vec<u8>, zx::Status> {
        self.handle.store().read_symlink(self.object_id()).await.map_err(map_to_status)
    }

    async fn list_extended_attributes(&self) -> Result<Vec<Vec<u8>>, zx::Status> {
        self.handle.list_extended_attributes().await.map_err(map_to_status)
    }
    async fn get_extended_attribute(&self, name: Vec<u8>) -> Result<Vec<u8>, zx::Status> {
        self.handle.get_extended_attribute(name).await.map_err(map_to_status)
    }
    async fn set_extended_attribute(
        &self,
        name: Vec<u8>,
        value: Vec<u8>,
        mode: fio::SetExtendedAttributeMode,
    ) -> Result<(), zx::Status> {
        self.handle.set_extended_attribute(name, value, mode.into()).await.map_err(map_to_status)
    }
    async fn remove_extended_attribute(&self, name: Vec<u8>) -> Result<(), zx::Status> {
        self.handle.remove_extended_attribute(name).await.map_err(map_to_status)
    }
}

#[async_trait]
impl Node for FxSymlink {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        let props = self.get_properties().await.map_err(map_to_status)?;
        Ok(attributes!(
            requested_attributes,
            Mutable {
                creation_time: props.creation_time.as_nanos(),
                modification_time: props.modification_time.as_nanos(),
                mode: props.posix_attributes.map(|a| a.mode).unwrap_or(0),
                uid: props.posix_attributes.map(|a| a.uid).unwrap_or(0),
                gid: props.posix_attributes.map(|a| a.gid).unwrap_or(0),
                rdev: props.posix_attributes.map(|a| a.rdev).unwrap_or(0),
            },
            Immutable {
                protocols: fio::NodeProtocolKinds::SYMLINK,
                abilities: fio::Operations::GET_ATTRIBUTES | fio::Operations::UPDATE_ATTRIBUTES,
                content_size: props.data_attribute_size,
                storage_size: props.allocated_size,
                link_count: props.refs,
                id: self.object_id(),
            }
        ))
    }

    async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn link_into(
        self: Arc<Self>,
        destination_dir: Arc<dyn MutableDirectory>,
        name: Name,
    ) -> Result<(), zx::Status> {
        let dir = destination_dir.into_any().downcast::<FxDirectory>().unwrap();
        let store = self.handle.store();
        let transaction = store
            .filesystem()
            .clone()
            .new_transaction(
                &[
                    LockKey::object(store.store_object_id(), self.object_id()),
                    LockKey::object(store.store_object_id(), dir.object_id()),
                ],
                Options::default(),
            )
            .await
            .map_err(map_to_status)?;
        dir.link_object(transaction, &name, self.object_id(), ObjectDescriptor::Symlink).await
    }
}

#[async_trait]
impl FxNode for FxSymlink {
    fn object_id(&self) -> u64 {
        self.handle.object_id()
    }

    fn parent(&self) -> Option<Arc<FxDirectory>> {
        None
    }

    fn set_parent(&self, _parent: Arc<FxDirectory>) {}
    fn open_count_add_one(&self) {}
    fn open_count_sub_one(self: Arc<Self>) {}

    async fn get_properties(&self) -> Result<ObjectProperties, Error> {
        let store = self.handle.store();
        let fs = store.filesystem();
        let _guard =
            fs.read_lock(&[LockKey::object(store.store_object_id(), self.object_id())]).await;
        let item = store
            .tree()
            .find(&ObjectKey::object(self.object_id()))
            .await?
            .expect("Unable to find object record");
        match item.value {
            ObjectValue::Object {
                kind: ObjectKind::Symlink { refs, .. },
                attributes:
                    ObjectAttributes {
                        creation_time,
                        modification_time,
                        posix_attributes,
                        access_time,
                        change_time,
                        ..
                    },
            } => Ok(ObjectProperties {
                refs,
                allocated_size: 0,
                data_attribute_size: 0,
                creation_time,
                modification_time,
                access_time,
                change_time,
                sub_dirs: 0,
                posix_attributes,
            }),
            _ => bail!(FxfsError::NotFile),
        }
    }
}
