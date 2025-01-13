// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use fidl::endpoints::DiscoverableProtocolMarker as _;
use fidl_fuchsia_hardware_block_volume::VolumeMarker;
use fs_management::filesystem::{
    BlockConnector, DirBasedBlockConnector, ServingMultiVolumeFilesystem,
};
use fs_management::Fvm;
use fuchsia_component::client::connect_to_protocol_at_dir_root;
use ramdevice_client::RamdiskClient;
use storage_benchmarks::block_device::BlockDevice;
use storage_benchmarks::{BlockDeviceConfig, BlockDeviceFactory};
use storage_isolated_driver_manager::{create_random_guid, fvm};
use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

use crate::block_devices::create_fvm_volume;

pub const RAMDISK_FVM_SLICE_SIZE: usize = 1024 * 1024;

/// Creates block devices on ramdisks.
pub struct RamdiskFactory {
    block_size: u64,
    block_count: u64,
}

impl RamdiskFactory {
    #[allow(dead_code)]
    pub async fn new(block_size: u64, block_count: u64) -> Self {
        Self { block_size, block_count }
    }
}

#[async_trait]
impl BlockDeviceFactory for RamdiskFactory {
    async fn create_block_device(&self, config: &BlockDeviceConfig) -> Box<dyn BlockDevice> {
        Box::new(Ramdisk::new(self.block_size, self.block_count, config).await)
    }
}

/// A ramdisk backed block device.
pub struct Ramdisk {
    _ramdisk: RamdiskClient,
    _fvm: ServingMultiVolumeFilesystem,
    volume_dir: fio::DirectoryProxy,
    _crypt_task: Option<fasync::Task<()>>,
}

impl Ramdisk {
    async fn new(block_size: u64, block_count: u64, config: &BlockDeviceConfig) -> Self {
        let ramdisk = RamdiskClient::builder(block_size, block_count)
            .use_v2()
            .build()
            .await
            .expect("Failed to create RamdiskClient");

        let block_device = ramdisk.open().expect("Failed to connect to block").into_proxy();
        fvm::format_for_fvm(&block_device, RAMDISK_FVM_SLICE_SIZE).expect("Failed to format FVM");

        let fvm = fs_management::filesystem::Filesystem::from_boxed_config(
            ramdisk.connector().unwrap(),
            Box::new(Fvm::default()),
        )
        .serve_multi_volume()
        .await
        .expect("Failed to serve FVM");
        let volumes = connect_to_protocol_at_dir_root::<fidl_fuchsia_fs_startup::VolumesMarker>(
            fvm.exposed_dir(),
        )
        .unwrap();
        let (volume_dir, crypt_task) =
            create_fvm_volume(&volumes, create_random_guid(), config).await;

        Self { _ramdisk: ramdisk, _fvm: fvm, volume_dir, _crypt_task: crypt_task }
    }
}

impl BlockDevice for Ramdisk {
    fn dir(&self) -> &fio::DirectoryProxy {
        &self.volume_dir
    }

    fn connector(&self) -> Box<dyn BlockConnector> {
        let volume_dir = fuchsia_fs::directory::clone(&self.volume_dir).unwrap();
        Box::new(DirBasedBlockConnector::new(
            volume_dir,
            format!("svc/{}", VolumeMarker::PROTOCOL_NAME),
        ))
    }
}
