// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod constants;

use anyhow::{anyhow, Context, Error};
use async_trait::async_trait;
use fidl::endpoints::{create_proxy, ClientEnd};
use fidl_fuchsia_device::{ControllerMarker, ControllerProxy};
use fidl_fuchsia_hardware_block::{BlockMarker, BlockProxy};
use fidl_fuchsia_hardware_block_partition::{PartitionMarker, PartitionProxy};
use fidl_fuchsia_hardware_block_volume::{VolumeMarker, VolumeProxy};
use fidl_fuchsia_io::{self as fio};
use fs_management::filesystem::{BlockConnector, DirBasedBlockConnector};
use fs_management::format::{detect_disk_format, DiskFormat};
use fuchsia_async::condition::Condition;
use fuchsia_component::client::connect_to_protocol_at_path;
use ramdevice_client::RamdiskClient;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;

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

    /// Returns the path in the local namespace. This path is absolute, e.g. /dev/class/block/000.
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

    /// Returns the connection to the Controller interface of the device.  Panics if the device
    /// isn't a DF-managed device.
    fn controller(&self) -> &ControllerProxy {
        panic!("Device isn't managed by Driver Framework.")
    }

    /// Establishes a new connection to the Controller interface of the device.  Panics if the
    /// device isn't a DF-managed device.
    fn device_controller(&self) -> Result<ControllerProxy, Error> {
        panic!("Device isn't managed by Driver Framework.")
    }

    /// Returns a new controller for the block device.
    fn block_connector(&self) -> Result<Box<dyn BlockConnector>, Error>;

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

    /// True if this is the ramdisk device that fshost has created or a child of the ramdisk device.
    /// NOTE: This is *only* true for the ramdisk device that fshost creates and will not be true
    /// for other ramdisks.
    fn is_fshost_ramdisk(&self) -> bool;

    /// Marks the device as being backed by an fshost ramdisk.
    fn set_fshost_ramdisk(&mut self, v: bool);
}

/// A nand device.
#[derive(Debug)]
pub struct NandDevice {
    // Nand devices don't actually support the block protocol, but we re-use the BlockDevice type
    // for convenience.
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
            fuchsia_fs::directory::open_in_namespace(DEV_CLASS_NAND, fio::PERM_READABLE)?;
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
        Err(anyhow!("not supported by nand device"))
    }

    async fn resize(&mut self, _target_size_bytes: u64) -> Result<u64, Error> {
        Err(anyhow!("not supported by nand device"))
    }

    async fn set_partition_max_bytes(&mut self, _max_bytes: u64) -> Result<(), Error> {
        Err(anyhow!("not supported by nand device"))
    }

    fn controller(&self) -> &ControllerProxy {
        self.block_device.controller()
    }

    fn device_controller(&self) -> Result<ControllerProxy, Error> {
        self.block_device.device_controller()
    }

    fn block_connector(&self) -> Result<Box<dyn BlockConnector>, Error> {
        Err(anyhow!("not supported by nand device"))
    }

    fn block_proxy(&self) -> Result<BlockProxy, Error> {
        Err(anyhow!("not supported by nand device"))
    }

    fn volume_proxy(&self) -> Result<VolumeProxy, Error> {
        Err(anyhow!("not supported by nand device"))
    }

    fn is_fshost_ramdisk(&self) -> bool {
        false
    }

    fn set_fshost_ramdisk(&mut self, _v: bool) {}
}

/// A block device.
#[derive(Debug)]
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

    is_fshost_ramdisk: bool,
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
        let (partition_proxy, server) = create_proxy::<PartitionMarker>();
        controller_proxy.connect_to_device_fidl(server.into_channel())?;
        Ok(Self {
            path: path.to_string(),
            topological_path: topological_path,
            controller_proxy,
            partition_proxy,
            content_format: None,
            partition_label: None,
            partition_type: None,
            partition_instance: None,
            is_fshost_ramdisk: false,
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
            self.partition_label = Some(name.ok_or_else(|| anyhow!("Expected name"))?);
        }
        Ok(self.partition_label.as_ref().unwrap())
    }

    async fn partition_type(&mut self) -> Result<&[u8; 16], Error> {
        if self.partition_type.is_none() {
            let (status, partition_type) = self.partition_proxy.get_type_guid().await?;
            zx::Status::ok(status)?;
            self.partition_type =
                Some(partition_type.ok_or_else(|| anyhow!("Expected type"))?.value);
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
                Some(instance_guid.ok_or_else(|| anyhow!("Expected instance guid"))?.value);
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

    fn device_controller(&self) -> Result<ControllerProxy, Error> {
        Ok(connect_to_protocol_at_path::<ControllerMarker>(&format!(
            "{}/device_controller",
            self.path
        ))?)
    }

    fn block_connector(&self) -> Result<Box<dyn BlockConnector>, Error> {
        self.device_controller().map(|c| Box::new(c) as Box<dyn BlockConnector>)
    }

    fn block_proxy(&self) -> Result<BlockProxy, Error> {
        let (proxy, server) = create_proxy::<BlockMarker>();
        self.controller_proxy.connect_to_device_fidl(server.into_channel())?;
        Ok(proxy)
    }

    fn volume_proxy(&self) -> Result<VolumeProxy, Error> {
        let (proxy, server) = create_proxy::<VolumeMarker>();
        self.controller_proxy.connect_to_device_fidl(server.into_channel())?;
        Ok(proxy)
    }

    async fn get_child(&self, suffix: &str) -> Result<Box<dyn Device>, Error> {
        const DEV_CLASS_BLOCK: &str = "/dev/class/block";
        let dev_class_block =
            fuchsia_fs::directory::open_in_namespace(DEV_CLASS_BLOCK, fio::PERM_READABLE)?;
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

    fn is_fshost_ramdisk(&self) -> bool {
        self.is_fshost_ramdisk
    }

    fn set_fshost_ramdisk(&mut self, v: bool) {
        self.is_fshost_ramdisk = v;
    }
}

/// A device that only serves the volume protocol at the specified path.
#[derive(Debug)]
pub struct VolumeProtocolDevice {
    connector: Box<DirBasedBlockConnector>,

    // Cache a proxy to the device's Volume interface so we can use it internally.  (This assumes
    // that devices speak Volume, which is currently always true).
    volume_proxy: VolumeProxy,

    // Memoized fields.
    content_format: Option<DiskFormat>,
    partition_label: Option<String>,
    partition_type: Option<[u8; 16]>,
    partition_instance: Option<[u8; 16]>,
}

impl VolumeProtocolDevice {
    pub fn new(dir: fio::DirectoryProxy, path: impl ToString) -> Result<Self, Error> {
        let connector = Box::new(DirBasedBlockConnector::new(dir, path.to_string() + "/volume"));
        let volume_proxy = connector.connect_volume()?.into_proxy();
        Ok(Self {
            connector,
            volume_proxy,
            content_format: None,
            partition_label: None,
            partition_type: None,
            partition_instance: None,
        })
    }
}

#[async_trait]
impl Device for VolumeProtocolDevice {
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

    fn path(&self) -> &str {
        self.connector.path()
    }

    fn topological_path(&self) -> &str {
        self.connector.path()
    }

    async fn partition_label(&mut self) -> Result<&str, Error> {
        if self.partition_label.is_none() {
            let (status, name) = self.volume_proxy.get_name().await?;
            zx::Status::ok(status)?;
            self.partition_label = Some(name.ok_or_else(|| anyhow!("Expected name"))?);
        }
        Ok(self.partition_label.as_ref().unwrap())
    }

    async fn partition_type(&mut self) -> Result<&[u8; 16], Error> {
        if self.partition_type.is_none() {
            let (status, partition_type) = self.volume_proxy.get_type_guid().await?;
            zx::Status::ok(status)?;
            self.partition_type =
                Some(partition_type.ok_or_else(|| anyhow!("Expected type"))?.value);
        }
        Ok(self.partition_type.as_ref().unwrap())
    }

    async fn partition_instance(&mut self) -> Result<&[u8; 16], Error> {
        if self.partition_instance.is_none() {
            let (status, instance_guid) = self
                .volume_proxy
                .get_instance_guid()
                .await
                .context("Transport error get_instance_guid")?;
            zx::Status::ok(status).context("get_instance_guid failed")?;
            self.partition_instance =
                Some(instance_guid.ok_or_else(|| anyhow!("Expected instance guid"))?.value);
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

    fn block_connector(&self) -> Result<Box<dyn BlockConnector>, Error> {
        Ok(self.connector.clone())
    }

    fn block_proxy(&self) -> Result<BlockProxy, Error> {
        self.connector.connect_block().and_then(|c| Ok(c.into_proxy()))
    }

    fn volume_proxy(&self) -> Result<VolumeProxy, Error> {
        self.connector.connect_volume().and_then(|c| Ok(c.into_proxy()))
    }

    async fn get_child(&self, _suffix: &str) -> Result<Box<dyn Device>, Error> {
        unimplemented!()
    }

    fn is_fshost_ramdisk(&self) -> bool {
        false
    }

    fn set_fshost_ramdisk(&mut self, _v: bool) {}
}

/// A device that represents the fshost Ramdisk.
pub struct RamdiskDevice {
    ramdisk: Arc<RamdiskClient>,

    // Cache a proxy to the device's Volume interface so we can use it internally.  (This assumes
    // that devices speak Volume, which is currently always true).
    volume_proxy: VolumeProxy,

    // Memoized fields.
    content_format: Option<DiskFormat>,
}

impl RamdiskDevice {
    pub fn new(ramdisk: RamdiskClient) -> Result<Self, Error> {
        let volume_proxy =
            ClientEnd::<VolumeMarker>::new(ramdisk.open()?.into_channel()).into_proxy();
        Ok(Self { ramdisk: Arc::new(ramdisk), volume_proxy, content_format: None })
    }
}

#[async_trait]
impl Device for RamdiskDevice {
    async fn get_block_info(&self) -> Result<fidl_fuchsia_hardware_block::BlockInfo, Error> {
        let info = self.volume_proxy.get_info().await?.map_err(zx::Status::from_raw)?;
        Ok(info)
    }

    fn is_nand(&self) -> bool {
        false
    }

    async fn content_format(&mut self) -> Result<DiskFormat, Error> {
        if let Some(format) = self.content_format {
            return Ok(format);
        }
        return Ok(detect_disk_format(&self.volume_proxy).await);
    }

    fn path(&self) -> &str {
        ""
    }

    fn topological_path(&self) -> &str {
        ""
    }

    async fn partition_label(&mut self) -> Result<&str, Error> {
        Err(anyhow!("partition_label not supported for ramdisk"))
    }

    async fn partition_type(&mut self) -> Result<&[u8; 16], Error> {
        Err(anyhow!("partition_type not supported for ramdisk"))
    }

    async fn partition_instance(&mut self) -> Result<&[u8; 16], Error> {
        Err(anyhow!("partition_instance not supported for ramdisk"))
    }

    async fn resize(&mut self, _target_size_bytes: u64) -> Result<u64, Error> {
        Err(anyhow!("resize not supported for ramdisk"))
    }

    async fn set_partition_max_bytes(&mut self, _max_bytes: u64) -> Result<(), Error> {
        Err(anyhow!("set_partition_max_bytes not supported for ramdisk"))
    }

    fn block_connector(&self) -> Result<Box<dyn BlockConnector>, Error> {
        Ok(Box::new(RamdiskDeviceBlockConnector { ramdisk: self.ramdisk.clone() }))
    }

    fn block_proxy(&self) -> Result<BlockProxy, Error> {
        Ok(self.ramdisk.open()?.into_proxy())
    }

    fn volume_proxy(&self) -> Result<VolumeProxy, Error> {
        Ok(ClientEnd::<VolumeMarker>::new(self.ramdisk.open()?.into_channel()).into_proxy())
    }

    async fn get_child(&self, _suffix: &str) -> Result<Box<dyn Device>, Error> {
        unimplemented!()
    }

    fn is_fshost_ramdisk(&self) -> bool {
        true
    }

    fn set_fshost_ramdisk(&mut self, _v: bool) {}
}

struct RamdiskDeviceBlockConnector {
    ramdisk: Arc<RamdiskClient>,
}

impl BlockConnector for RamdiskDeviceBlockConnector {
    fn connect_volume(
        &self,
    ) -> Result<ClientEnd<fidl_fuchsia_hardware_block_volume::VolumeMarker>, Error> {
        Ok(ClientEnd::new(self.ramdisk.open()?.into_channel()))
    }
}

/// RegisteredDevices keeps track of significant devices so that they can be found later as
/// required.  Devices can be associated with a tag.
pub struct RegisteredDevices(Condition<HashMap<DeviceTag, Box<dyn Device>>>);

impl Default for RegisteredDevices {
    fn default() -> Self {
        Self(Condition::new(HashMap::default()))
    }
}

impl RegisteredDevices {
    /// Registers a device with the specified tag.  This *only* registers the first device with the
    /// tag.
    pub fn register_device(&self, tag: DeviceTag, device: Box<dyn Device>) {
        let mut map = self.0.lock();
        if let Entry::Vacant(v) = map.entry(tag) {
            v.insert(device);
        }
        for waker in map.drain_wakers() {
            waker.wake();
        }
    }

    /// Returns the topological path for the device with the specified tag, if registered.
    pub fn get_topological_path(&self, tag: DeviceTag) -> Option<String> {
        self.0.lock().get(&tag).map(|d| d.topological_path().to_string())
    }

    /// Returns a block_connector for the device with the specified tag.  This will wait till the
    /// device is registered.
    pub async fn get_block_connector(
        &self,
        tag: DeviceTag,
    ) -> Result<Box<dyn BlockConnector>, Error> {
        self.0
            .when(|map| map.get(&tag).map_or(Poll::Pending, |d| Poll::Ready(d.block_connector())))
            .await
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DeviceTag {
    /// The fshost ramdisk device.
    Ramdisk,

    /// The block device containing the partition table in which the Fuchsia system resides.
    SystemPartitionTable,

    /// The non-ramdisk block device in which the Fuchsia system resides (which is either an FVM
    /// instance, or an Fxblob instance).  Only set on recovery (and volumes within the container
    /// will not be bound).
    SystemContainerOnRecovery,
}
