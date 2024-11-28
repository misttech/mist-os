// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{format_sources, get_policy, unseal_sources, KeyConsumer};
use crate::device::Device;
use anyhow::{anyhow, Context, Error};
use async_trait::async_trait;
use device_watcher::recursive_wait_and_open;
use fidl::endpoints::Proxy as _;
use fidl_fuchsia_device::ControllerProxy;
use fidl_fuchsia_hardware_block::BlockProxy;
use fidl_fuchsia_hardware_block_encrypted::{DeviceManagerMarker, DeviceManagerProxy};
use fidl_fuchsia_hardware_block_volume::VolumeProxy;
use fidl_fuchsia_io as fio;
use fs_management::filesystem::BlockConnector;
use fs_management::format::DiskFormat;

/// Fetches a FIDL proxy for accessing zxcrypt management protocol for a given Device.
async fn device_to_device_manager_proxy(device: &dyn Device) -> Result<DeviceManagerProxy, Error> {
    let controller =
        fuchsia_fs::directory::open_in_namespace(device.topological_path(), fio::Flags::empty())?;
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
    proxy: DeviceManagerProxy,
    inner_device: Box<dyn Device>,
    is_fshost_ramdisk: bool,
}

impl ZxcryptDevice {
    /// Unseals a Zxcrypt BlockDevice and returns it.
    /// If device is not in zxcrypt format, return 'FormatRequired'.
    pub async fn unseal(outer_device: &mut dyn Device) -> Result<UnsealOutcome, Error> {
        if outer_device.content_format().await? != DiskFormat::Zxcrypt {
            return Ok(UnsealOutcome::FormatRequired);
        }
        let proxy = device_to_device_manager_proxy(outer_device).await?;
        ZxcryptDevice::from_proxy(outer_device, proxy).await
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
                    let zxcrypt_device =
                        ZxcryptDevice::from_proxy(outer_device, proxy.clone()).await?;
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
        proxy: DeviceManagerProxy,
    ) -> Result<UnsealOutcome, Error> {
        let policy = get_policy().await?;
        let sources = unseal_sources(policy);

        let mut last_res = Err(anyhow!("no keys?"));
        for source in sources {
            let key = source.get_key(KeyConsumer::Zxcrypt).await?;
            match zx::ok(proxy.unseal(&key, 0).await?) {
                Ok(()) => {
                    let device = ZxcryptDevice {
                        parent_is_nand: outer_device.is_nand(),
                        proxy: proxy.clone(),
                        inner_device: outer_device.get_child("/zxcrypt/unsealed/block").await?,
                        is_fshost_ramdisk: false,
                    };
                    tracing::info!(
                        path = device.path(),
                        topological_path = device.topological_path(),
                        "created zxcryptdevice"
                    );
                    return Ok(UnsealOutcome::Unsealed(device));
                }
                Err(zx::Status::ACCESS_DENIED) => last_res = Ok(UnsealOutcome::FormatRequired),
                Err(status) => last_res = Err(status.into()),
            };
        }
        last_res
    }

    pub async fn seal(self) -> Result<(), Error> {
        zx::ok(self.proxy.seal().await?).map_err(|e| e.into())
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
        // Nb: The zxcrypt device proxies the BlockVolume protocol and
        // changes the extend/shrink offset to account for the
        // zxcrypt header (src/devices/block/drivers/zxcrypt/device.cc:193).
        let volume_proxy = self.volume_proxy()?;
        crate::volume::resize_volume(&volume_proxy, target_size_bytes).await
    }

    async fn set_partition_max_bytes(&mut self, max_bytes: u64) -> Result<(), Error> {
        // Because partition limits are set on the volume manager (not the volume proxy)
        // we have to account fo the zxcrypt overheads ourselves.
        let extra_bytes = if max_bytes > 0 {
            let volume_proxy = self.volume_proxy()?;
            let (status, volume_manager_info, _volume_info) = volume_proxy
                .get_volume_info()
                .await
                .context("Transport error on get_volume_info")?;
            zx::Status::ok(status).context("get_volume_info failed")?;
            let manager =
                volume_manager_info.ok_or_else(|| anyhow!("Expected volume manager info"))?;
            manager.slice_size
        } else {
            0
        };
        // Add an extra slice for zxcrypt metadata.
        self.inner_device.set_partition_max_bytes(max_bytes + extra_bytes).await
    }

    fn device_controller(&self) -> Result<ControllerProxy, Error> {
        self.inner_device.device_controller()
    }

    fn block_connector(&self) -> Result<Box<dyn BlockConnector>, Error> {
        self.inner_device.block_connector()
    }

    fn block_proxy(&self) -> Result<BlockProxy, Error> {
        self.inner_device.block_proxy()
    }

    fn volume_proxy(&self) -> Result<VolumeProxy, Error> {
        self.inner_device.volume_proxy()
    }

    async fn get_child(&self, suffix: &str) -> Result<Box<dyn Device>, Error> {
        self.inner_device.get_child(suffix).await
    }

    fn is_fshost_ramdisk(&self) -> bool {
        self.is_fshost_ramdisk
    }

    fn set_fshost_ramdisk(&mut self, v: bool) {
        self.is_fshost_ramdisk = v;
    }
}
