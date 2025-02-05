// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_hardware_block_volume::VolumeManagerProxy;
use fidl_fuchsia_io as fio;
use ramdevice_client::{RamdiskClient, RamdiskClientBuilder};
use std::path::PathBuf;
use storage_isolated_driver_manager::{
    create_random_guid, fvm, wait_for_block_device_devfs, BlockDeviceMatcher,
};
use zx::{AsHandleRef, Rights, Status, Vmo};

pub use storage_isolated_driver_manager::Guid;

async fn create_ramdisk(vmo: &Vmo, ramdisk_block_size: u64) -> RamdiskClient {
    let duplicated_handle = vmo.as_handle_ref().duplicate(Rights::SAME_RIGHTS).unwrap();
    let duplicated_vmo = Vmo::from(duplicated_handle);

    // Create the ramdisks
    RamdiskClientBuilder::new_with_vmo(duplicated_vmo, Some(ramdisk_block_size))
        .build()
        .await
        .unwrap()
}

/// This structs holds processes of component manager, isolated-devmgr
/// and the fvm driver.
///
/// NOTE: The order of fields in this struct is important.
/// Destruction happens top-down. Test must be destroyed last.
pub struct FvmInstance {
    /// A proxy to fuchsia.hardware.block.VolumeManager protocol
    /// Used to create new FVM volumes
    volume_manager: VolumeManagerProxy,

    /// Manages the ramdisk device that is backed by a VMO
    ramdisk: RamdiskClient,
}

impl FvmInstance {
    /// Start an isolated FVM driver against the given VMO.
    /// If `init` is true, initialize the VMO with FVM layout first.
    pub async fn new(init: bool, vmo: &Vmo, fvm_slice_size: u64, ramdisk_block_size: u64) -> Self {
        let ramdisk = create_ramdisk(&vmo, ramdisk_block_size).await;

        if init {
            fvm::format_for_fvm(
                &ramdisk.open().expect("invalid ramdisk").into_proxy(),
                fvm_slice_size as usize,
            )
            .unwrap();
        }

        let volume_manager = fvm::start_fvm_driver(
            ramdisk.as_controller().expect("invalid controller"),
            ramdisk.as_dir().expect("invalid directory proxy"),
        )
        .await
        .expect("failed to start fvm driver");

        Self { ramdisk, volume_manager }
    }

    /// Create a new FVM volume with the given name and type GUID.
    /// Returns the instance GUID used to uniquely identify this volume.
    pub async fn new_volume(
        &mut self,
        name: &str,
        type_guid: &Guid,
        initial_volume_size: Option<u64>,
    ) -> Guid {
        let instance_guid = create_random_guid();

        fvm::create_fvm_volume(
            &self.volume_manager,
            name,
            type_guid,
            &instance_guid,
            initial_volume_size,
            0,
        )
        .await
        .unwrap();

        instance_guid
    }

    /// Returns the number of bytes the FVM partition has available.
    pub async fn free_space(&self) -> u64 {
        let (status, info) = self.volume_manager.get_info().await.unwrap();
        Status::ok(status).unwrap();
        let info = info.unwrap();

        (info.slice_count - info.assigned_slice_count) * info.slice_size
    }

    /// Returns a reference to the ramdisk DirectoryProxy.
    pub fn ramdisk_get_dir(&self) -> Option<&fio::DirectoryProxy> {
        self.ramdisk.as_dir()
    }

    /// Shuts down the FVM instance and ramdisk that's hosting it.
    pub async fn shutdown(self) {
        self.ramdisk.destroy_and_wait_for_removal().await.expect("failed to shutdown ramdisk");
    }
}

/// Gets the full path to a volume matching the given instance GUID at the given
/// /dev/class/block path. This function will wait until a matching volume is found.
pub async fn get_volume_path(instance_guid: &Guid) -> PathBuf {
    wait_for_block_device_devfs(&[BlockDeviceMatcher::InstanceGuid(instance_guid)]).await.unwrap()
}
