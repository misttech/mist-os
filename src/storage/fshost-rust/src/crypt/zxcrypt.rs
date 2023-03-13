// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::{format_sources, get_policy, unseal_sources, KeyConsumer},
    crate::device::Device,
    anyhow::{anyhow, Context, Error},
    async_trait::async_trait,
    device_watcher::recursive_wait_and_open,
    fidl::endpoints::{ClientEnd, Proxy},
    fidl_fuchsia_hardware_block::BlockMarker,
    fidl_fuchsia_hardware_block_encrypted::{DeviceManagerMarker, DeviceManagerProxy},
    fidl_fuchsia_io as fio,
    fs_management::format::DiskFormat,
    fuchsia_zircon as zx,
};

/// Fetches a FIDL proxy for accessing zxcrypt management protocol for a given Device.
async fn device_to_device_manager_proxy(device: &dyn Device) -> Result<DeviceManagerProxy, Error> {
    let controller = fuchsia_fs::directory::open_in_namespace(
        device.topological_path(),
        fio::OpenFlags::empty(),
    )?;
    let zxcrypt = recursive_wait_and_open::<DeviceManagerMarker>(&controller, "zxcrypt")
        .await
        .context("waiting for zxcrypt device")?;
    Ok(DeviceManagerProxy::new(zxcrypt.into_channel().unwrap()))
}

/// Holds the outcome of an unseal attempt via ZxcryptDevice::unseal().
pub enum UnsealOutcome {
    Unsealed(ZxcryptDevice),
    FormatRequired,
}

/// A BlockDevice representing a zxcrypt wrapped child device.
pub struct ZxcryptDevice {
    parent_is_nand: bool,
    inner_device: Box<dyn Device>,
}

impl ZxcryptDevice {
    /// Creates a non-functional ZxcryptDevice for use with mock tests.
    #[cfg(test)]
    pub fn new_mock(device: Box<dyn Device>) -> Self {
        ZxcryptDevice { parent_is_nand: false, inner_device: device }
    }

    /// Unseals a Zxcrypt BlockDevice and returns it.
    /// If device is not in zxcrypt format, return 'FormatRequired'.
    pub async fn unseal(outer_device: &mut dyn Device) -> Result<UnsealOutcome, Error> {
        if outer_device.content_format().await? != DiskFormat::Zxcrypt {
            return Ok(UnsealOutcome::FormatRequired);
        }
        let proxy = device_to_device_manager_proxy(outer_device).await?;
        ZxcryptDevice::from_proxy(outer_device, &proxy).await
    }
    /// Formats a BlockDevice as Zxcrypt and returns it.
    pub async fn format(outer_device: &mut dyn Device) -> Result<ZxcryptDevice, Error> {
        let proxy = device_to_device_manager_proxy(outer_device).await?;
        let policy = get_policy().await?;
        let sources = format_sources(policy);

        let mut last_err = anyhow!("no keys?");
        for source in sources {
            let key = source.get_key(KeyConsumer::Zxcrypt).await?;
            match zx::ok(proxy.format(&key, 0).await?) {
                Ok(()) => {
                    let zxcrypt_device = ZxcryptDevice::from_proxy(outer_device, &proxy).await?;
                    if let UnsealOutcome::Unsealed(zxcrypt_device) = zxcrypt_device {
                        return Ok(zxcrypt_device);
                    } else {
                        return Err(anyhow!("zxcrypt format failed"));
                    }
                }
                Err(status) => last_err = status.into(),
            }
        }
        Err(last_err)
    }

    /// Attempts to unseal a zxcrypt device and return it.
    async fn from_proxy(
        outer_device: &mut dyn Device,
        proxy: &DeviceManagerProxy,
    ) -> Result<UnsealOutcome, Error> {
        let policy = get_policy().await?;
        let sources = unseal_sources(policy);

        let mut last_res = Err(anyhow!("no keys?"));
        for source in sources {
            let key = source.get_key(KeyConsumer::Zxcrypt).await?;
            match zx::ok(proxy.unseal(&key, 0).await?) {
                Ok(()) => {
                    last_res = Ok(UnsealOutcome::Unsealed(ZxcryptDevice {
                        parent_is_nand: outer_device.is_nand(),
                        inner_device: outer_device.get_child("/zxcrypt/unsealed/block").await?,
                    }))
                }
                Err(zx::Status::ACCESS_DENIED) => last_res = Ok(UnsealOutcome::FormatRequired),
                Err(status) => last_res = Err(status.into()),
            };
        }
        last_res
    }
}

#[async_trait]
impl Device for ZxcryptDevice {
    async fn get_block_info(&self) -> Result<fidl_fuchsia_hardware_block::BlockInfo, Error> {
        self.inner_device.get_block_info().await
    }

    fn is_nand(&self) -> bool {
        self.parent_is_nand
    }

    async fn content_format(&mut self) -> Result<DiskFormat, Error> {
        self.inner_device.content_format().await
    }

    fn topological_path(&self) -> &str {
        self.inner_device.topological_path()
    }

    fn path(&self) -> &str {
        self.inner_device.path()
    }

    async fn partition_label(&mut self) -> Result<&str, Error> {
        self.inner_device.partition_label().await
    }

    async fn partition_type(&mut self) -> Result<&[u8; 16], Error> {
        self.inner_device.partition_type().await
    }

    async fn partition_instance(&mut self) -> Result<&[u8; 16], Error> {
        self.inner_device.partition_instance().await
    }

    async fn resize(&mut self, target_size_bytes: u64) -> Result<u64, Error> {
        let volume_proxy = {
            let volume: ClientEnd<fidl_fuchsia_hardware_block_volume::VolumeMarker> =
                self.inner_device.client_end()?.into_channel().into();
            volume.into_proxy()?
        };
        crate::volume::resize_volume(&volume_proxy, target_size_bytes, true).await
    }

    fn client_end(&self) -> Result<ClientEnd<BlockMarker>, Error> {
        self.inner_device.client_end()
    }

    async fn get_child(&self, suffix: &str) -> Result<Box<dyn Device>, Error> {
        self.inner_device.get_child(suffix).await
    }
}
