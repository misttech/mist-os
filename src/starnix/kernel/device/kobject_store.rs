// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{
    Bus, Class, Collection, Device, DeviceMetadata, KObject, KObjectBased, KObjectHandle,
};
use crate::device::DeviceMode;
use crate::fs::sysfs::{
    get_sysfs, BusCollectionDirectory, KObjectDirectory, SYSFS_BLOCK, SYSFS_BUS, SYSFS_CLASS,
    SYSFS_DEV, SYSFS_DEVICES,
};
use crate::task::Kernel;
use crate::vfs::fs_node_cache::FsNodeCache;
use crate::vfs::pseudo::simple_directory::SimpleDirectoryMutator;
use crate::vfs::{FileSystemHandle, FsNodeOps, FsStr, FsString};
use std::sync::{Arc, OnceLock};

/// The owner of all the KObjects in sysfs.
///
/// This structure holds strong references to the KObjects that are visible in sysfs. These
/// objects are organized into hierarchies that make it easier to implement sysfs.
pub struct KObjectStore {
    /// The node cache used to allocate inode numbers for the kobjects in this store.
    pub node_cache: Arc<FsNodeCache>,

    /// The sysfs filesystem in which the KObjects are stored.
    fs: OnceLock<FileSystemHandle>,

    /// All of the devices added to the system.
    ///
    /// Used to populate the /sys/devices directory.
    pub devices: KObjectHandle,

    /// All of the device classes known to the system.
    ///
    /// Used to populate the /sys/class directory.
    pub class: KObjectHandle,

    /// All of the block devices known to the system.
    ///
    /// Used to populate the /sys/block directory.
    pub block: KObjectHandle,

    /// All of the buses known to the system.
    ///
    /// Devices are organized first by bus and then by class. The more relevant bus for our
    /// purposes is the "virtual" bus, which is accessible via the `virtial_bus` method.
    pub bus: KObjectHandle,

    /// The devices in the system, organized by `DeviceMode` and `DeviceType`.
    ///
    /// Used to populate the /sys/dev directory. The KObjects descended from this KObject are
    /// the same objects referenced through the `devices` KObject. They are just organized in
    /// a different hierarchy. When populated in /sys/dev, they appear as symlinks to the
    /// canonical names in /sys/devices.
    pub dev: KObjectHandle,
}

impl KObjectStore {
    pub fn init(&self, kernel: &Arc<Kernel>) {
        self.fs.set(get_sysfs(kernel)).unwrap();
    }

    fn fs(&self) -> &FileSystemHandle {
        self.fs.get().expect("sysfs should be initialized")
    }

    /// The virtual bus kobject where all virtual and pseudo devices are stored.
    pub fn virtual_bus(&self) -> Bus {
        Bus::new(
            self.devices.get_or_create_child_with_ops("virtual".into(), KObjectDirectory::new),
            None,
        )
    }

    /// The device class used for virtual block devices.
    pub fn virtual_block_class(&self) -> Class {
        self.get_or_create_class("block".into(), self.virtual_bus())
    }

    /// The device class used for virtual thermal devices.
    pub fn virtual_thermal_class(&self) -> Class {
        self.get_or_create_class("thermal".into(), self.virtual_bus())
    }

    /// The device class used for virtual graphics devices.
    pub fn graphics_class(&self) -> Class {
        self.get_or_create_class("graphics".into(), self.virtual_bus())
    }

    /// The device class used for virtual input devices.
    pub fn input_class(&self) -> Class {
        self.get_or_create_class("input".into(), self.virtual_bus())
    }

    /// The device class used for virtual mem devices.
    pub fn mem_class(&self) -> Class {
        self.get_or_create_class("mem".into(), self.virtual_bus())
    }

    /// The device class used for virtual net devices.
    pub fn net_class(&self) -> Class {
        self.get_or_create_class("net".into(), self.virtual_bus())
    }

    /// The device class used for virtual misc devices.
    pub fn misc_class(&self) -> Class {
        self.get_or_create_class("misc".into(), self.virtual_bus())
    }

    /// The device class used for virtual tty devices.
    pub fn tty_class(&self) -> Class {
        self.get_or_create_class("tty".into(), self.virtual_bus())
    }

    /// The device class used for virtual dma_heap devices.
    pub fn dma_heap_class(&self) -> Class {
        self.get_or_create_class("dma_heap".into(), self.virtual_bus())
    }

    /// An incorrect device class.
    ///
    /// This class exposes the name "starnix" to userspace, which is incorrect. Instead, devices
    /// should use a class that represents their usage rather than their implementation.
    ///
    /// This class exists because a number of devices incorrectly use this class. We should fix
    /// those devices to report their proper class.
    pub fn starnix_class(&self) -> Class {
        self.get_or_create_class("starnix".into(), self.virtual_bus())
    }

    /// Get a bus by name.
    ///
    /// If the bus does not exist, this function will create it.
    pub fn get_or_create_bus(&self, name: &FsStr) -> Bus {
        let collection = Collection::new(
            self.bus.get_or_create_child_with_ops(name, BusCollectionDirectory::new),
        );
        Bus::new(
            self.devices.get_or_create_child_with_ops(name, KObjectDirectory::new),
            Some(collection),
        )
    }

    /// Get a class by name.
    ///
    /// If the bus does not exist, this function will create it.
    pub fn get_or_create_class(&self, name: &FsStr, bus: Bus) -> Class {
        let collection = self.class.dir().subdir(self.fs(), name, 0o755);
        Class::new(
            bus.kobject().get_or_create_child_with_ops(name, KObjectDirectory::new),
            bus,
            collection,
        )
    }

    pub fn class_with_dir(
        &self,
        name: &FsStr,
        bus: Bus,
        build_collection: impl FnOnce(&SimpleDirectoryMutator),
    ) -> Class {
        let class = self.get_or_create_class(name, bus);
        let mutator = SimpleDirectoryMutator::new(self.fs().clone(), class.collection.clone());
        build_collection(&mutator);
        class
    }

    fn block(&self) -> SimpleDirectoryMutator {
        SimpleDirectoryMutator::new(self.fs().clone(), self.block.dir())
    }

    fn dev(&self) -> SimpleDirectoryMutator {
        SimpleDirectoryMutator::new(self.fs().clone(), self.dev.dir())
    }

    fn dev_block(&self, build_subdir: impl FnOnce(&SimpleDirectoryMutator)) {
        self.dev().subdir("block", 0o755, build_subdir)
    }

    fn dev_char(&self, build_subdir: impl FnOnce(&SimpleDirectoryMutator)) {
        self.dev().subdir("char", 0o755, build_subdir)
    }

    /// Create a device and add that device to the store.
    ///
    /// Rather than use this function directly, you should register your device with the
    /// `DeviceRegistry`. The `DeviceRegistry` will create the KObject for the device as
    /// part of the registration process.
    ///
    /// If you create the device yourself, userspace will not be able to instantiate the
    /// device because the `DeviceType` will not be registered with the `DeviceRegistry`.
    pub(super) fn create_device_with_ops<F, N>(
        &self,
        _kernel: &Arc<Kernel>,
        name: &FsStr,
        metadata: Option<DeviceMetadata>,
        class: Class,
        create_device_sysfs_ops: F,
    ) -> Device
    where
        F: Fn(Device) -> N + Send + Sync + 'static,
        N: FsNodeOps,
    {
        let class_cloned = class.clone();
        let metadata_cloned = metadata.clone();
        let kobject = class.kobject().get_or_create_child_with_ops(name, move |kobject| {
            create_device_sysfs_ops(Device::new(
                kobject.upgrade().unwrap(),
                class_cloned.clone(),
                metadata_cloned.clone(),
            ))
        });

        let device = Device::new(kobject, class, metadata);
        self.add(&device);
        device
    }

    pub(super) fn create_device(
        &self,
        _kernel: &Arc<Kernel>,
        name: &FsStr,
        metadata: Option<DeviceMetadata>,
        class: Class,
        build_directory: impl FnOnce(&Device, &SimpleDirectoryMutator),
    ) -> Device {
        let kobject = class.kobject().get_or_create_child(name);
        let dir = kobject.dir();
        let device = Device::new(kobject, class, metadata);
        dir.edit(self.fs(), |dir| {
            build_directory(&device, dir);
        });
        self.add(&device);
        device
    }

    fn add(&self, device: &Device) {
        let class = &device.class;
        let kobject = device.kobject();
        let name = kobject.name();

        let path_to_device =
            format!("devices/{}/{}/{}", class.bus.kobject().name(), class.kobject().name(), name);
        let up_device = FsString::from(format!("../{}", &path_to_device));
        let up_up_device = FsString::from(format!("../../{}", &path_to_device));

        // Insert the newly created device into various views.
        class.collection.edit(self.fs(), |dir| {
            dir.symlink(name, up_up_device.as_ref());
        });

        if let Some(metadata) = &device.metadata {
            let device_number = FsString::from(metadata.device_type.to_string());
            match metadata.mode {
                DeviceMode::Block => {
                    self.block().symlink(name, up_device.as_ref());
                    self.dev_block(|dir| {
                        dir.symlink(device_number.as_ref(), up_up_device.as_ref());
                    });
                }
                DeviceMode::Char => {
                    self.dev_char(|dir| {
                        dir.symlink(device_number.as_ref(), up_up_device.as_ref());
                    });
                }
            }
        }

        if let Some(bus_collection) = &class.bus.collection {
            bus_collection.kobject().insert_child(kobject);
        }
    }

    /// Destroy a device.
    ///
    /// This function removes the KObject for the device from the store.
    ///
    /// Most clients hold weak references to KObjects, which means those references will become
    /// invalid shortly after this function is called.
    pub(super) fn remove(&self, device: &Device) {
        let kobject = device.kobject();
        let name = kobject.name();
        // Remove the device from its views in the reverse order in which it was added.
        if let Some(bus_collection) = &device.class.bus.collection {
            bus_collection.kobject().remove_child(name);
        }
        if let Some(metadata) = &device.metadata {
            let device_number: FsString = metadata.device_type.to_string().into();
            match metadata.mode {
                DeviceMode::Block => {
                    let path = FsString::from(format!("block/{}", device_number));
                    self.dev.dir().remove_path(path.as_ref());
                    self.block.dir().remove(name);
                }
                DeviceMode::Char => {
                    let path = FsString::from(format!("char/{}", device_number));
                    self.dev.dir().remove_path(path.as_ref());
                }
            }
        }
        device.class.collection.remove(name);
        // Finally, remove the device from the object store.
        kobject.remove();
    }
}

impl Default for KObjectStore {
    fn default() -> Self {
        let node_cache = Arc::new(FsNodeCache::default());
        let devices = KObject::new_root(SYSFS_DEVICES.into(), node_cache.clone());
        let class = KObject::new_root_with_dir(SYSFS_CLASS.into(), node_cache.clone());
        let block = KObject::new_root_with_dir(SYSFS_BLOCK.into(), node_cache.clone());
        let bus = KObject::new_root(SYSFS_BUS.into(), node_cache.clone());
        let dev = KObject::new_root_with_dir(SYSFS_DEV.into(), node_cache.clone());

        Self { node_cache, fs: OnceLock::new(), devices, class, block, bus, dev }
    }
}
