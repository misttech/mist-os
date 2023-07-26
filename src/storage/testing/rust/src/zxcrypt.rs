// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context as _, Result},
    device_watcher::{recursive_wait_and_open, recursive_wait_and_open_directory},
    fidl_fuchsia_device::{ControllerMarker, ControllerProxy},
    fidl_fuchsia_hardware_block_encrypted::DeviceManagerMarker,
    fidl_fuchsia_io as fio,
    fuchsia_component::client::connect_to_named_protocol_at_dir_root,
    fuchsia_zircon as zx,
};

const ZXCRYPT_DRIVER_PATH: &str = "zxcrypt.cm";

/// Binds the zxcrypt driver to the device at `controller`. Does not wait for the zxcrypt driver to
/// be ready.
pub async fn bind_zxcrypt_driver(controller: &ControllerProxy) -> Result<()> {
    controller
        .bind(ZXCRYPT_DRIVER_PATH)
        .await
        .context("zxcrypt driver bind fidl failure")?
        .map_err(zx::Status::from_raw)
        .context("zxcrypt driver bind returned error")?;
    Ok(())
}

/// Sets up zxcrypt on top of `block_device` using an insecure key. Returns a path to the block
/// device exposed by zxcrypt.
pub async fn set_up_insecure_zxcrypt(
    block_device: &fio::DirectoryProxy,
) -> Result<fio::DirectoryProxy> {
    const UNSEALED_BLOCK_PATH: &str = "unsealed/block";
    let device_controller = connect_to_named_protocol_at_dir_root::<ControllerMarker>(
        block_device,
        "device_controller",
    )?;
    bind_zxcrypt_driver(&device_controller).await.context("zxcrypt driver bind")?;

    const ZXCRYPT_DEVICE_NAME: &str = "zxcrypt";
    let zxcrypt = recursive_wait_and_open::<DeviceManagerMarker>(block_device, ZXCRYPT_DEVICE_NAME)
        .await
        .context("zxcrypt device wait")?;

    zx::ok(zxcrypt.format(&[0u8; 32], 0).await.context("zxcrypt format fidl failure")?)
        .context("zxcrypt format returned error")?;
    zx::ok(zxcrypt.unseal(&[0u8; 32], 0).await.context("zxcrypt unseal fidl failure")?)
        .context("zxcrypt unseal returned error")?;

    let zxcrypt_dir = fuchsia_fs::directory::open_directory_no_describe(
        block_device,
        ZXCRYPT_DEVICE_NAME,
        fio::OpenFlags::empty(),
    )?;

    recursive_wait_and_open_directory(&zxcrypt_dir, UNSEALED_BLOCK_PATH)
        .await
        .context("zxcrypt unsealed dir wait")
}

#[cfg(test)]
mod tests {
    use {
        super::*, fidl_fuchsia_hardware_block::BlockMarker, ramdevice_client::RamdiskClient,
        test_util::assert_lt,
    };

    const BLOCK_SIZE: u64 = 512;
    const BLOCK_COUNT: u64 = 64 * 1024 * 1024 / BLOCK_SIZE;

    #[fuchsia::test]
    async fn set_up_insecure_zxcrypt_test() {
        let ramdisk = RamdiskClient::create(BLOCK_SIZE, BLOCK_COUNT).await.unwrap();
        let ramdisk_dir = ramdisk.as_dir().expect("invalid directory proxy");
        let zxcrypt_block_dir =
            set_up_insecure_zxcrypt(ramdisk_dir).await.expect("Failed to set up zxcrypt");

        let zxcrypt_block_device =
            connect_to_named_protocol_at_dir_root::<BlockMarker>(&zxcrypt_block_dir, ".").unwrap();
        let info = zxcrypt_block_device.get_info().await.unwrap().unwrap();
        assert_lt!(info.block_count, BLOCK_COUNT);
    }
}
