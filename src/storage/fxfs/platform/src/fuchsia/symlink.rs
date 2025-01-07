// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fuchsia::directory::FxDirectory;
use crate::fuchsia::errors::map_to_status;
use crate::fuchsia::node::FxNode;
use crate::fuchsia::volume::{info_to_filesystem_info, FxVolume};
use anyhow::Error;
use fidl_fuchsia_io as fio;
use fxfs::errors::FxfsError;
use fxfs::object_handle::{ObjectHandle, ObjectProperties};
use fxfs::object_store::transaction::{lock_keys, LockKey, Options};
use fxfs::object_store::{
    HandleOptions, ObjectAttributes, ObjectDescriptor, ObjectKey, ObjectKind, ObjectValue,
    StoreObjectHandle,
};
use fxfs_macros::ToWeakNode;
use std::sync::Arc;
use vfs::attributes;
use vfs::directory::entry::{EntryInfo, GetEntryInfo};
use vfs::directory::entry_container::MutableDirectory;
use vfs::name::Name;
use vfs::node::Node;
use vfs::symlink::Symlink;

#[derive(ToWeakNode)]
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

    async fn get_properties(&self) -> Result<ObjectProperties, Error> {
        let store = self.handle.store();
        let fs = store.filesystem();
        let _guard = fs
            .lock_manager()
            .read_lock(lock_keys![LockKey::object(store.store_object_id(), self.object_id())])
            .await;
        let item = store
            .tree()
            .find(&ObjectKey::object(self.object_id()))
            .await?
            .ok_or(FxfsError::NotFound)?;
        match item.value {
            ObjectValue::Object {
                kind: ObjectKind::Symlink { refs, link },
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
                // For POSIX compatibility we report the target length as file size.
                data_attribute_size: link.len() as u64,
                creation_time,
                modification_time,
                access_time,
                change_time,
                sub_dirs: 0,
                posix_attributes,
                casefold: false,
                // TODO: https://fxbug.dev/360171961: Add fscrypt support for symlinks
                wrapping_key_id: None,
            }),
            ObjectValue::None => Err(FxfsError::NotFound.into()),
            _ => Err(FxfsError::NotFile.into()),
        }
    }
}

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

impl GetEntryInfo for FxSymlink {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(self.object_id(), fio::DirentType::Symlink)
    }
}

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
                mode: props.posix_attributes.map(|a| a.mode),
                uid: props.posix_attributes.map(|a| a.uid),
                gid: props.posix_attributes.map(|a| a.gid),
                rdev: props.posix_attributes.map(|a| a.rdev),
                selinux_context: self
                    .handle
                    .get_inline_selinux_context()
                    .await
                    .map_err(map_to_status)?,
            },
            Immutable {
                protocols: fio::NodeProtocolKinds::SYMLINK,
                abilities: fio::Operations::GET_ATTRIBUTES | fio::Operations::UPDATE_ATTRIBUTES,
                content_size: props.data_attribute_size,
                storage_size: props.allocated_size,
                link_count: props.refs,
                id: self.object_id(),
                verity_enabled: false,
            }
        ))
    }

    async fn link_into(
        self: Arc<Self>,
        destination_dir: Arc<dyn MutableDirectory>,
        name: Name,
    ) -> Result<(), zx::Status> {
        let dir = destination_dir.into_any().downcast::<FxDirectory>().unwrap();
        // TODO(https://fxbug.dev/360171961): Add fscrypt symlink support.
        if dir.directory().wrapping_key_id().is_some() {
            return Err(zx::Status::INVALID_ARGS);
        }
        let store = self.handle.store();
        let transaction = store
            .filesystem()
            .clone()
            .new_transaction(
                lock_keys![
                    LockKey::object(store.store_object_id(), self.object_id()),
                    LockKey::object(store.store_object_id(), dir.object_id()),
                ],
                Options::default(),
            )
            .await
            .map_err(map_to_status)?;
        dir.link_object(transaction, &name, self.object_id(), ObjectDescriptor::Symlink).await
    }

    fn query_filesystem(&self) -> Result<fio::FilesystemInfo, zx::Status> {
        let store = self.handle.store();
        Ok(info_to_filesystem_info(
            store.filesystem().get_info(),
            store.filesystem().block_size(),
            store.object_count(),
            self.handle.owner().id(),
        ))
    }
}

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

    fn object_descriptor(&self) -> ObjectDescriptor {
        ObjectDescriptor::Symlink
    }
}
