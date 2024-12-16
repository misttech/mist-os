// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_device::ControllerProxy;
use fidl_fuchsia_io as fio;
use fuchsia_component::client::connect_to_named_protocol_at_dir_root;
use ramdevice_client::RamdiskClient;
use storage_benchmarks::block_device::BlockDevice;
use storage_benchmarks::{BlockDeviceConfig, BlockDeviceFactory};
use storage_isolated_driver_manager::{fvm, zxcrypt};

use crate::block_devices::set_up_fvm_volume;

const RAMDISK_FVM_SLICE_SIZE: usize = 1024 * 1024;

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
    volume_dir: fio::DirectoryProxy,
    volume_controller: ControllerProxy,
}

impl Ramdisk {
    async fn new(block_size: u64, block_count: u64, config: &BlockDeviceConfig) -> Self {
        let ramdisk = RamdiskClient::builder(block_size, block_count)
            .use_v2()
            .build()
            .await
            .expect("Failed to create RamdiskClient");

        // TODO switch to component FVM
        let volume_manager = fvm::set_up_fvm(
            ramdisk.as_controller().expect("invalid controller"),
            ramdisk.as_dir().expect("invalid directory proxy"),
            RAMDISK_FVM_SLICE_SIZE,
        )
        .await
        .expect("Failed to set up FVM");
        let volume_dir = set_up_fvm_volume(&volume_manager, config.fvm_volume_size).await;
        let volume_dir = if config.use_zxcrypt {
            zxcrypt::set_up_insecure_zxcrypt(&volume_dir).await.expect("Failed to set up zxcrypt")
        } else {
            volume_dir
        };

        let volume_controller = connect_to_named_protocol_at_dir_root::<ControllerMarker>(
            &volume_dir,
            "device_controller",
        )
        .expect("failed to connect to the device controller");

        Self { _ramdisk: ramdisk, volume_dir, volume_controller: volume_controller }
    }
}

impl BlockDevice for Ramdisk {
    fn dir(&self) -> &fio::DirectoryProxy {
        &self.volume_dir
    }

    fn connector(&self) -> &ControllerProxy {
        &self.volume_controller
    }
}
