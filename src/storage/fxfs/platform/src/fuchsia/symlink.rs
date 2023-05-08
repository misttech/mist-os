// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::fuchsia::{
        directory::FxDirectory, errors::map_to_status, node::FxNode, volume::FxVolume,
    },
    anyhow::{bail, Error},
    async_trait::async_trait,
    fuchsia_zircon as zx,
    fxfs::{
        errors::FxfsError,
        object_handle::ObjectProperties,
        object_store::{
            transaction::LockKey, ObjectAttributes, ObjectKey, ObjectKind, ObjectValue,
        },
    },
    std::sync::Arc,
    vfs::symlink::Symlink,
};

pub struct FxSymlink {
    volume: Arc<FxVolume>,
    object_id: u64,
}

impl FxSymlink {
    pub fn new(volume: Arc<FxVolume>, object_id: u64) -> Self {
        Self { volume, object_id }
    }
}

#[async_trait]
impl Symlink for FxSymlink {
    async fn read_target(&self) -> Result<Vec<u8>, zx::Status> {
        self.volume.store().read_symlink(self.object_id).await.map_err(map_to_status)
    }
}

#[async_trait]
impl FxNode for FxSymlink {
    fn object_id(&self) -> u64 {
        self.object_id
    }

    fn parent(&self) -> Option<Arc<FxDirectory>> {
        None
    }

    fn set_parent(&self, _parent: Arc<FxDirectory>) {}
    fn open_count_add_one(&self) {}
    fn open_count_sub_one(&self) {}

    async fn get_properties(&self) -> Result<ObjectProperties, Error> {
        let store = self.volume.store();
        let fs = store.filesystem();
        let _guard =
            fs.read_lock(&[LockKey::object(store.store_object_id(), self.object_id)]).await;
        let item = store
            .tree()
            .find(&ObjectKey::object(self.object_id))
            .await?
            .expect("Unable to find object record");
        match item.value {
            ObjectValue::Object {
                kind: ObjectKind::Symlink { refs, .. },
                attributes:
                    ObjectAttributes { creation_time, modification_time, posix_attributes, .. },
            } => Ok(ObjectProperties {
                refs,
                allocated_size: 0,
                data_attribute_size: 0,
                creation_time,
                modification_time,
                sub_dirs: 0,
                posix_attributes,
            }),
            _ => bail!(FxfsError::NotFile),
        }
    }
}
