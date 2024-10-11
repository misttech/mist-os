// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::Device;
use anyhow::{anyhow, Error};
use fidl::endpoints::Proxy as _;
use fidl_fuchsia_component::{self as fcomponent, RealmMarker};
use fuchsia_component::client::{
    connect_to_protocol, connect_to_protocol_at_dir_root, open_childs_exposed_directory,
};
use std::sync::atomic::{AtomicU64, Ordering};
use {fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_io as fio};

static COLLECTION_NAME: &str = "storage-host";

/// Handle to a running instance of storage-host bound to a block device.
pub struct StorageHostInstance {
    component_name: String,
    exposed_dir: fio::DirectoryProxy,
}

impl StorageHostInstance {
    /// Creates a new instance of storage-host bound to `device`.
    pub async fn new(device: &mut dyn Device, url: &str) -> Result<Self, Error> {
        static INSTANCE: AtomicU64 = AtomicU64::new(1);

        let collection_ref = fdecl::CollectionRef { name: COLLECTION_NAME.to_string() };

        let component_name = format!("storage-host.{}", INSTANCE.fetch_add(1, Ordering::SeqCst));

        let child_decl = fdecl::Child {
            name: Some(component_name.clone()),
            url: Some(url.to_string()),
            startup: Some(fdecl::StartupMode::Lazy),
            ..Default::default()
        };

        let realm_proxy = connect_to_protocol::<RealmMarker>()?;

        realm_proxy
            .create_child(&collection_ref, &child_decl, fcomponent::CreateChildArgs::default())
            .await?
            .map_err(|e| anyhow!("create_child failed: {:?}", e))?;

        let exposed_dir = open_childs_exposed_directory(
            component_name.clone(),
            Some(COLLECTION_NAME.to_string()),
        )
        .await?;

        let proxy = connect_to_protocol_at_dir_root::<fidl_fuchsia_storagehost::StorageHostMarker>(
            &exposed_dir,
        )?;
        proxy
            .start(device.device_controller()?.into_client_end().unwrap())
            .await?
            .map_err(zx::Status::from_raw)?;

        Ok(StorageHostInstance { component_name, exposed_dir })
    }

    pub async fn partitions_dir(&self) -> Result<fio::DirectoryProxy, Error> {
        Ok(fuchsia_fs::directory::open_directory_deprecated(
            &self.exposed_dir,
            "partitions",
            fio::OpenFlags::RIGHT_READABLE,
        )
        .await?)
    }
}

impl Drop for StorageHostInstance {
    fn drop(&mut self) {
        if let Ok(realm_proxy) = connect_to_protocol::<RealmMarker>() {
            let _ = realm_proxy.destroy_child(&fdecl::ChildRef {
                name: self.component_name.clone(),
                collection: Some(COLLECTION_NAME.to_string()),
            });
        }
    }
}
