// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::Guid,
    anyhow::{Context, Result},
    device_watcher::recursive_wait_and_open,
    fidl::endpoints::Proxy as _,
    fidl_fuchsia_device::ControllerProxy,
    fidl_fuchsia_hardware_block::BlockMarker,
    fidl_fuchsia_hardware_block_partition::Guid as FidlGuid,
    fidl_fuchsia_hardware_block_volume::{VolumeManagerMarker, VolumeManagerProxy},
    fidl_fuchsia_io as fio,
    fuchsia_component::client::connect_to_named_protocol_at_dir_root,
    fuchsia_zircon::{self as zx, sys::zx_handle_t, zx_status_t, AsHandleRef},
};

const FVM_DRIVER_PATH: &str = "fvm.cm";

#[link(name = "fvm")]
extern "C" {
    // This function initializes FVM on a fuchsia.hardware.block.Block device
    // with a given slice size.
    fn fvm_init(device: zx_handle_t, slice_size: usize) -> zx_status_t;
}

/// Formats the block device at `block_device` to be an empty FVM instance.
pub fn format_for_fvm(block_device: &fio::DirectoryProxy, fvm_slice_size: usize) -> Result<()> {
    // TODO(https://fxbug.dev/121896): In order to remove multiplexing, callers of this function
    // should directly pass in a BlockProxy. Callers holding onto a ramdisk should replace as_dir()
    // with a connect_to_device_fidl() call. This requires work downstream.
    let device = connect_to_named_protocol_at_dir_root::<BlockMarker>(block_device, ".")?;
    let device_raw = device.as_channel().raw_handle();
    let status = unsafe { fvm_init(device_raw, fvm_slice_size) };
    zx::ok(status).context("fvm_init failed")
}

/// Binds the FVM driver to the device at `controller`. Does not wait for the driver to be ready.
pub async fn bind_fvm_driver(controller: &ControllerProxy) -> Result<()> {
    controller
        .bind(FVM_DRIVER_PATH)
        .await
        .context("fvm driver bind call failed")?
        .map_err(zx::Status::from_raw)
        .context("fvm driver bind returned error")?;
    Ok(())
}

/// Binds the fvm driver and returns a connection to the newly created FVM instance.
pub async fn start_fvm_driver(
    controller: &ControllerProxy,
    block_device: &fio::DirectoryProxy,
) -> Result<VolumeManagerProxy> {
    bind_fvm_driver(controller).await?;
    const FVM_DEVICE_NAME: &str = "fvm";
    recursive_wait_and_open::<VolumeManagerMarker>(block_device, FVM_DEVICE_NAME)
        .await
        .context("wait_for_fvm_driver wait failed")
}

/// Sets up an FVM instance on `block_device`. Returns a connection to the newly created FVM
/// instance.
pub async fn set_up_fvm(
    controller: &ControllerProxy,
    block_device: &fio::DirectoryProxy,
    fvm_slice_size: usize,
) -> Result<VolumeManagerProxy> {
    format_for_fvm(block_device, fvm_slice_size)?;
    start_fvm_driver(controller, block_device).await
}

/// Creates an FVM volume in `volume_manager`.
///
/// If `volume_size` is not provided then the volume will start with 1 slice. If `volume_size` is
/// provided then the volume will start with the minimum number of slices required to have
/// `volume_size` bytes.
///
/// `wait_for_block_device` can be used to find the volume after its created.
pub async fn create_fvm_volume(
    volume_manager: &VolumeManagerProxy,
    name: &str,
    type_guid: &Guid,
    instance_guid: &Guid,
    volume_size: Option<u64>,
    flags: u32,
) -> Result<()> {
    let slice_count = match volume_size {
        Some(volume_size) => {
            let (status, info) =
                volume_manager.get_info().await.context("Failed to get FVM info")?;
            zx::ok(status).context("Get Info Error")?;
            let slice_size = info.unwrap().slice_size;
            assert!(slice_size > 0);
            // Number of slices needed to satisfy volume_size.
            (volume_size + slice_size - 1) / slice_size
        }
        None => 1,
    };
    let type_guid = FidlGuid { value: type_guid.clone() };
    let instance_guid = FidlGuid { value: instance_guid.clone() };

    let status = volume_manager
        .allocate_partition(slice_count, &type_guid, &instance_guid, name, flags)
        .await?;
    zx::ok(status).context("error allocating partition")
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{wait_for_block_device, BlockDeviceMatcher},
        fidl_fuchsia_hardware_block_volume::VolumeMarker,
        fidl_fuchsia_hardware_block_volume::ALLOCATE_PARTITION_FLAG_INACTIVE,
        fuchsia_component::client::connect_to_protocol_at_path,
        ramdevice_client::RamdiskClient,
    };

    const BLOCK_SIZE: u64 = 512;
    const BLOCK_COUNT: u64 = 64 * 1024 * 1024 / BLOCK_SIZE;
    const FVM_SLICE_SIZE: usize = 1024 * 1024;
    const INSTANCE_GUID: Guid = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const TYPE_GUID: Guid = [
        0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xa0, 0xb0, 0xc0, 0xd0, 0xe0,
        0xf0,
    ];
    const VOLUME_NAME: &str = "volume-name";

    #[fuchsia::test]
    async fn set_up_fvm_test() {
        let ramdisk = RamdiskClient::create(BLOCK_SIZE, BLOCK_COUNT).await.unwrap();
        let fvm = set_up_fvm(
            ramdisk.as_controller().expect("invalid controller"),
            ramdisk.as_dir().expect("invalid directory proxy"),
            FVM_SLICE_SIZE,
        )
        .await
        .expect("Failed to set up FVM");

        let fvm_info = fvm.get_info().await.unwrap();
        zx::ok(fvm_info.0).unwrap();
        let fvm_info = fvm_info.1.unwrap();
        assert_eq!(fvm_info.slice_size, FVM_SLICE_SIZE as u64);
        assert_eq!(fvm_info.assigned_slice_count, 0);
    }

    #[fuchsia::test]
    async fn create_fvm_volume_without_volume_size_has_one_slice() {
        let ramdisk = RamdiskClient::create(BLOCK_SIZE, BLOCK_COUNT).await.unwrap();
        let fvm = set_up_fvm(
            ramdisk.as_controller().expect("invalid controller"),
            ramdisk.as_dir().expect("invalid directory proxy"),
            FVM_SLICE_SIZE,
        )
        .await
        .expect("Failed to set up FVM");

        create_fvm_volume(
            &fvm,
            VOLUME_NAME,
            &TYPE_GUID,
            &INSTANCE_GUID,
            None,
            ALLOCATE_PARTITION_FLAG_INACTIVE,
        )
        .await
        .expect("Failed to create fvm volume");
        let block_device_path = wait_for_block_device(&[
            BlockDeviceMatcher::TypeGuid(&TYPE_GUID),
            BlockDeviceMatcher::InstanceGuid(&INSTANCE_GUID),
            BlockDeviceMatcher::Name(VOLUME_NAME),
        ])
        .await
        .expect("Failed to find block device");

        let volume =
            connect_to_protocol_at_path::<VolumeMarker>(block_device_path.to_str().unwrap())
                .unwrap();
        let volume_info = volume.get_volume_info().await.unwrap();
        zx::ok(volume_info.0).unwrap();
        let volume_info = volume_info.2.unwrap();
        assert_eq!(volume_info.partition_slice_count, 1);
    }

    #[fuchsia::test]
    async fn create_fvm_volume_with_unaligned_volume_size_rounds_up_to_slice_multiple() {
        let ramdisk = RamdiskClient::create(BLOCK_SIZE, BLOCK_COUNT).await.unwrap();
        let fvm = set_up_fvm(
            ramdisk.as_controller().expect("invalid controller"),
            ramdisk.as_dir().expect("invalid directory proxy"),
            FVM_SLICE_SIZE,
        )
        .await
        .expect("Failed to set up FVM");

        create_fvm_volume(
            &fvm,
            VOLUME_NAME,
            &TYPE_GUID,
            &INSTANCE_GUID,
            Some((FVM_SLICE_SIZE * 5 + 4) as u64),
            ALLOCATE_PARTITION_FLAG_INACTIVE,
        )
        .await
        .expect("Failed to create fvm volume");

        let block_device_path = wait_for_block_device(&[
            BlockDeviceMatcher::TypeGuid(&TYPE_GUID),
            BlockDeviceMatcher::InstanceGuid(&INSTANCE_GUID),
            BlockDeviceMatcher::Name(VOLUME_NAME),
        ])
        .await
        .expect("Failed to find block device");

        let volume =
            connect_to_protocol_at_path::<VolumeMarker>(block_device_path.to_str().unwrap())
                .unwrap();
        let volume_info = volume.get_volume_info().await.unwrap();
        zx::ok(volume_info.0).unwrap();
        let volume_info = volume_info.2.unwrap();
        assert_eq!(volume_info.partition_slice_count, 6);
    }
}
