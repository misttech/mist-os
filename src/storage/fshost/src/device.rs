// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod constants;

use {
    anyhow::{anyhow, Context, Error},
    async_trait::async_trait,
    fidl::endpoints::create_proxy,
    fidl_fuchsia_device::{ControllerMarker, ControllerProxy},
    fidl_fuchsia_hardware_block::{BlockMarker, BlockProxy},
    fidl_fuchsia_hardware_block_partition::{PartitionMarker, PartitionProxy},
    fidl_fuchsia_hardware_block_volume::{VolumeMarker, VolumeProxy},
    fidl_fuchsia_io::OpenFlags,
    fs_management::format::{detect_disk_format, DiskFormat},
    fuchsia_component::client::connect_to_protocol_at_path,
    fuchsia_zircon as zx,
};

#[async_trait]
pub trait Device: Send + Sync {
    /// Returns BlockInfo (the result of calling fuchsia.hardware.block/Block.Query).
    async fn get_block_info(&self) -> Result<fidl_fuchsia_hardware_block::BlockInfo, Error>;

    /// True if this is a NAND device.
    fn is_nand(&self) -> bool;

    /// Returns the format as determined by content sniffing. This should be used sparingly when
    /// other means of determining the format are not possible.
    async fn content_format(&mut self) -> Result<DiskFormat, Error>;

    /// Returns the topological path.
    fn topological_path(&self) -> &str;

    /// Returns the path in /dev/class. This path is absolute and includes the /dev/class prefix.
    fn path(&self) -> &str;

    /// If this device is a partition, this returns the label. Otherwise, an error is returned.
    async fn partition_label(&mut self) -> Result<&str, Error>;

    /// If this device is a partition, this returns the type GUID. Otherwise, an error is returned.
    async fn partition_type(&mut self) -> Result<&[u8; 16], Error>;

    /// If this device is a partition, this returns the instance GUID.
    /// Otherwise, an error is returned.
    async fn partition_instance(&mut self) -> Result<&[u8; 16], Error>;

    /// If this device is a volume, this allows resizing the device.
    /// Returns actual byte size assuming success, or error.
    async fn resize(&mut self, _target_size_bytes: u64) -> Result<u64, Error> {
        Err(anyhow!("Unimplemented"))
    }

    /// Sets the maximum size of an partition.
    /// Attempts to resize above this value will fail.
    async fn set_partition_max_bytes(&mut self, _max_bytes: u64) -> Result<(), Error> {
        Err(anyhow!("Unimplemented"))
    }

    /// Returns the Controller interface for the device.
    fn controller(&self) -> &ControllerProxy;

    /// Establish a new connection to the Controller interface for the device.
    fn reopen_controller(&self) -> Result<ControllerProxy, Error>;

    /// Establish a new connection to the Block interface of the device.
    fn block_proxy(&self) -> Result<BlockProxy, Error>;

    /// Establish a new connection to the Volume interface of the device.
    fn volume_proxy(&self) -> Result<VolumeProxy, Error>;

    /// If device is backed by FVM, returns the topological path to FVM, otherwise None.
    fn fvm_path(&self) -> Option<String> {
        // The 4 is from the 4 characters in "/fvm"
        self.topological_path()
            .rfind("/fvm")
            .map(|index| (self.topological_path()[..index + 4]).to_string())
    }

    /// Returns a new Device, which is a child of this device with the specified suffix. This
    /// function will return when the device is available. This function assumes the child device
    /// will show up in /dev/class/block.
    async fn get_child(&self, suffix: &str) -> Result<Box<dyn Device>, Error>;
}

/// A nand device.
#[derive(Clone, Debug)]
pub struct NandDevice {
    block_device: BlockDevice,
}

impl NandDevice {
    pub async fn new(path: impl ToString) -> Result<Self, Error> {
        Ok(NandDevice { block_device: BlockDevice::new(path).await? })
    }
}

#[async_trait]
impl Device for NandDevice {
    fn is_nand(&self) -> bool {
        true
    }

    async fn get_block_info(&self) -> Result<fidl_fuchsia_hardware_block::BlockInfo, Error> {
        Err(anyhow!("not supported by nand device"))
    }

    async fn content_format(&mut self) -> Result<DiskFormat, Error> {
        Ok(DiskFormat::Unknown)
    }

    async fn get_child(&self, suffix: &str) -> Result<Box<dyn Device>, Error> {
        const DEV_CLASS_NAND: &str = "/dev/class/nand";
        let dev_class_nand =
            fuchsia_fs::directory::open_in_namespace(DEV_CLASS_NAND, OpenFlags::RIGHT_READABLE)?;
        let child_path = device_watcher::wait_for_device_with(
            &dev_class_nand,
            |device_watcher::DeviceInfo { filename, topological_path }| {
                topological_path.strip_suffix(suffix).and_then(|topological_path| {
                    (topological_path == self.topological_path())
                        .then(|| format!("{}/{}", DEV_CLASS_NAND, filename))
                })
            },
        )
        .await?;
        let nand_device = NandDevice::new(child_path).await?;
        Ok(Box::new(nand_device))
    }

    fn topological_path(&self) -> &str {
        self.block_device.topological_path()
    }

    fn path(&self) -> &str {
        self.block_device.path()
    }

    async fn partition_label(&mut self) -> Result<&str, Error> {
        self.block_device.partition_label().await
    }

    async fn partition_type(&mut self) -> Result<&[u8; 16], Error> {
        self.block_device.partition_type().await
    }

    async fn partition_instance(&mut self) -> Result<&[u8; 16], Error> {
        self.block_device.partition_instance().await
    }

    async fn resize(&mut self, target_size_bytes: u64) -> Result<u64, Error> {
        self.block_device.resize(target_size_bytes).await
    }

    async fn set_partition_max_bytes(&mut self, max_bytes: u64) -> Result<(), Error> {
        self.block_device.set_partition_max_bytes(max_bytes).await
    }

    fn controller(&self) -> &ControllerProxy {
        self.block_device.controller()
    }

    fn reopen_controller(&self) -> Result<ControllerProxy, Error> {
        self.block_device.reopen_controller()
    }

    fn block_proxy(&self) -> Result<BlockProxy, Error> {
        self.block_device.block_proxy()
    }

    fn volume_proxy(&self) -> Result<VolumeProxy, Error> {
        self.block_device.volume_proxy()
    }
}

/// A block device.
#[derive(Clone, Debug)]
pub struct BlockDevice {
    // The canonical path of the device in /dev. Most of the time it's from /dev/class/, but it
    // just needs to be able to be opened as a fuchsia.device/Controller.
    //
    // Eventually we should consider moving towards opening the device as a directory, which will
    // let us re-open the controller and protocol connections, and be generally more future proof
    // with respect to devfs.
    path: String,

    // The topological path.
    topological_path: String,

    // The proxy for the device's controller, through which the Block/Volume/... protocols can be
    // accessed (see Controller.ConnectToDeviceFidl).
    controller_proxy: ControllerProxy,

    // Cache a proxy to the device's Partition interface so we can use it internally.  (This assumes
    // that devices speak Partition, which is currently always true).
    partition_proxy: PartitionProxy,

    // Memoized fields.
    content_format: Option<DiskFormat>,
    partition_label: Option<String>,
    partition_type: Option<[u8; 16]>,
    partition_instance: Option<[u8; 16]>,
}

impl BlockDevice {
    pub async fn new(path: impl ToString) -> Result<Self, Error> {
        let path = path.to_string();
        let controller =
            connect_to_protocol_at_path::<ControllerMarker>(&format!("{path}/device_controller"))?;
        Self::from_proxy(controller, path).await
    }

    pub async fn from_proxy(
        controller_proxy: ControllerProxy,
        path: impl ToString,
    ) -> Result<Self, Error> {
        let topological_path =
            controller_proxy.get_topological_path().await?.map_err(zx::Status::from_raw)?;
        let (partition_proxy, server) = create_proxy::<PartitionMarker>()?;
        controller_proxy.connect_to_device_fidl(server.into_channel())?;
        Ok(Self {
            path: path.to_string(),
            topological_path: topological_path.to_string(),
            controller_proxy,
            partition_proxy,
            content_format: None,
            partition_label: None,
            partition_type: None,
            partition_instance: None,
        })
    }
}

#[async_trait]
impl Device for BlockDevice {
    async fn get_block_info(&self) -> Result<fidl_fuchsia_hardware_block::BlockInfo, Error> {
        let block_proxy = self.block_proxy()?;
        let info = block_proxy.get_info().await?.map_err(zx::Status::from_raw)?;
        Ok(info)
    }

    fn is_nand(&self) -> bool {
        false
    }

    async fn content_format(&mut self) -> Result<DiskFormat, Error> {
        if let Some(format) = self.content_format {
            return Ok(format);
        }
        let block = self.block_proxy()?;
        return Ok(detect_disk_format(&block).await);
    }

    fn topological_path(&self) -> &str {
        &self.topological_path
    }

    fn path(&self) -> &str {
        &self.path
    }

    async fn partition_label(&mut self) -> Result<&str, Error> {
        if self.partition_label.is_none() {
            let (status, name) = self.partition_proxy.get_name().await?;
            zx::Status::ok(status)?;
            self.partition_label = Some(name.ok_or(anyhow!("Expected name"))?);
        }
        Ok(self.partition_label.as_ref().unwrap())
    }

    async fn partition_type(&mut self) -> Result<&[u8; 16], Error> {
        if self.partition_type.is_none() {
            let (status, partition_type) = self.partition_proxy.get_type_guid().await?;
            zx::Status::ok(status)?;
            self.partition_type = Some(partition_type.ok_or(anyhow!("Expected type"))?.value);
        }
        Ok(self.partition_type.as_ref().unwrap())
    }

    async fn partition_instance(&mut self) -> Result<&[u8; 16], Error> {
        if self.partition_instance.is_none() {
            let (status, instance_guid) = self
                .partition_proxy
                .get_instance_guid()
                .await
                .context("Transport error get_instance_guid")?;
            zx::Status::ok(status).context("get_instance_guid failed")?;
            self.partition_instance =
                Some(instance_guid.ok_or(anyhow!("Expected instance guid"))?.value);
        }
        Ok(self.partition_instance.as_ref().unwrap())
    }

    async fn resize(&mut self, target_size_bytes: u64) -> Result<u64, Error> {
        let volume_proxy = self.volume_proxy()?;
        crate::volume::resize_volume(&volume_proxy, target_size_bytes).await
    }

    async fn set_partition_max_bytes(&mut self, max_bytes: u64) -> Result<(), Error> {
        crate::volume::set_partition_max_bytes(self, max_bytes).await
    }

    fn controller(&self) -> &ControllerProxy {
        &self.controller_proxy
    }

    fn reopen_controller(&self) -> Result<ControllerProxy, Error> {
        Ok(connect_to_protocol_at_path::<ControllerMarker>(&format!(
            "{}/device_controller",
            self.path
        ))?)
    }

    fn block_proxy(&self) -> Result<BlockProxy, Error> {
        let (proxy, server) = create_proxy::<BlockMarker>()?;
        self.controller_proxy.connect_to_device_fidl(server.into_channel())?;
        Ok(proxy)
    }

    fn volume_proxy(&self) -> Result<VolumeProxy, Error> {
        let (proxy, server) = create_proxy::<VolumeMarker>()?;
        self.controller_proxy.connect_to_device_fidl(server.into_channel())?;
        Ok(proxy)
    }

    async fn get_child(&self, suffix: &str) -> Result<Box<dyn Device>, Error> {
        const DEV_CLASS_BLOCK: &str = "/dev/class/block";
        let dev_class_block =
            fuchsia_fs::directory::open_in_namespace(DEV_CLASS_BLOCK, OpenFlags::RIGHT_READABLE)?;
        let child_path = device_watcher::wait_for_device_with(
            &dev_class_block,
            |device_watcher::DeviceInfo { filename, topological_path }| {
                topological_path.strip_suffix(suffix).and_then(|topological_path| {
                    (topological_path == self.topological_path)
                        .then(|| format!("{}/{}", DEV_CLASS_BLOCK, filename))
                })
            },
        )
        .await?;

        let block_device = BlockDevice::new(child_path).await?;
        Ok(Box::new(block_device))
    }
}
