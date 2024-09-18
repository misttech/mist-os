// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{
    Bus, Class, Device, DeviceMetadata, KObject, KObjectBased, KObjectHandle, UEventAction,
    UEventContext,
};
use crate::fs::devtmpfs::{devtmpfs_create_device, devtmpfs_remove_node};
use crate::fs::sysfs::{
    BusCollectionDirectory, KObjectDirectory, KObjectSymlinkDirectory, SYSFS_BLOCK, SYSFS_BUS,
    SYSFS_CLASS, SYSFS_DEVICES,
};
use crate::task::CurrentTask;
use crate::vfs::{FileOps, FsNode, FsNodeOps, FsStr};
use starnix_logging::{log_error, log_warn, track_stub};
use starnix_uapi::device_type::{DeviceType, DYN_MAJOR};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error};

use starnix_sync::{
    DeviceOpen, FileOpsCore, LockBefore, Locked, MappedMutexGuard, Mutex, MutexGuard, RwLock,
};
use std::collections::btree_map::{BTreeMap, Entry};
use std::ops::{Deref, Range};
use std::sync::{Arc, Weak};

use dyn_clone::{clone_trait_object, DynClone};
use range_map::RangeMap;

use super::kobject::Collection;

const CHRDEV_MINOR_MAX: u32 = 256;
const BLKDEV_MINOR_MAX: u32 = 2u32.pow(20);

/// The mode or category of the device driver.
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
pub enum DeviceMode {
    Char,
    Block,
}

impl DeviceMode {
    fn minor_count(&self) -> u32 {
        match self {
            Self::Char => CHRDEV_MINOR_MAX,
            Self::Block => BLKDEV_MINOR_MAX,
        }
    }

    fn minor_range(&self) -> Range<u32> {
        0..self.minor_count()
    }
}

pub trait DeviceOps: DynClone + Send + Sync + 'static {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno>;
}

impl<T: DeviceOps> DeviceOps for Arc<T> {
    fn open(
        &self,
        locked: &mut Locked<'_, DeviceOpen>,
        current_task: &CurrentTask,
        id: DeviceType,
        node: &FsNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        self.deref().open(locked, current_task, id, node, flags)
    }
}

clone_trait_object!(DeviceOps);

/// Allows directly using a function or closure as an implementation of DeviceOps, avoiding having
/// to write a zero-size struct and an impl for it.
impl<F> DeviceOps for F
where
    F: Clone
        + Send
        + Sync
        + Clone
        + Fn(
            &mut Locked<'_, DeviceOpen>,
            &CurrentTask,
            DeviceType,
            &FsNode,
            OpenFlags,
        ) -> Result<Box<dyn FileOps>, Errno>
        + 'static,
{
    fn open(
        &self,
        locked: &mut Locked<'_, DeviceOpen>,
        current_task: &CurrentTask,
        id: DeviceType,
        node: &FsNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        self(locked, current_task, id, node, flags)
    }
}

/// A simple `DeviceOps` function for any device that implements `FileOps + Default`.
pub fn simple_device_ops<T: Default + FileOps + 'static>(
    _locked: &mut Locked<'_, DeviceOpen>,
    _current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(Box::new(T::default()))
}

/// Keys returned by the registration method for `DeviceListener`s that allows to unregister a
/// listener.
pub type DeviceListenerKey = u64;

/// A listener for uevents on devices.
pub trait DeviceListener: Send + Sync {
    fn on_device_event(&self, action: UEventAction, device: Device, context: UEventContext);
}

#[derive(Clone)]
struct MinorDevice {
    // Will be used for /proc/misc.
    #[allow(dead_code)]
    kobject: Weak<KObject>,
    ops: Arc<dyn DeviceOps>,
}

impl MinorDevice {
    fn from(device: &Device, ops: impl DeviceOps) -> Self {
        Self { kobject: device.kobject.clone(), ops: Arc::new(ops) }
    }

    fn from_ops(ops: impl DeviceOps) -> Self {
        Self { kobject: Default::default(), ops: Arc::new(ops) }
    }
}

impl PartialEq for MinorDevice {
    fn eq(&self, _other: &Self) -> bool {
        false
    }
}
impl Eq for MinorDevice {}

#[derive(Default)]
struct MajorDevice {
    minor_devices: RangeMap<u32, MinorDevice>,
}

impl MajorDevice {
    fn get(&self, minor: u32) -> Option<&MinorDevice> {
        self.minor_devices.get(&minor).map(|(_, minor_device)| minor_device)
    }

    fn insert(&mut self, minor: u32, minor_device: MinorDevice) -> Result<(), Errno> {
        let range = minor..(minor + 1);
        self.insert_range(range, minor_device)
    }

    fn insert_range(&mut self, range: Range<u32>, minor_device: MinorDevice) -> Result<(), Errno> {
        if self.minor_devices.intersection(range.clone()).count() == 0 {
            self.minor_devices.insert(range, minor_device);
            Ok(())
        } else {
            log_error!("Range {:?} overlaps with existing entries in the map.", range);
            error!(EINVAL)
        }
    }

    fn remove_range(&mut self, range: &Range<u32>) {
        self.minor_devices.remove(range);
    }
}

#[derive(Default)]
struct MajorDeviceRegistry {
    major_devices: BTreeMap<u32, MajorDevice>,
}

impl MajorDeviceRegistry {
    fn get_major(&mut self, major: u32) -> &mut MajorDevice {
        self.major_devices.entry(major).or_default()
    }

    fn get(&self, device_type: DeviceType) -> Result<Arc<dyn DeviceOps>, Errno> {
        match self.major_devices.get(&device_type.major()) {
            Some(major_device) => match major_device.get(device_type.minor()) {
                Some(minor_device) => Ok(Arc::clone(&minor_device.ops)),
                None => error!(ENODEV),
            },
            None => error!(ENODEV),
        }
    }

    fn register_device(&mut self, device: &Device, ops: impl DeviceOps) -> Result<(), Errno> {
        let major = device.metadata.device_type.major();
        let minor = device.metadata.device_type.minor();
        self.get_major(major).insert(minor, MinorDevice::from(device, ops))
    }

    fn register(
        &mut self,
        major: u32,
        minor_range: Range<u32>,
        ops: impl DeviceOps,
    ) -> Result<(), Errno> {
        self.get_major(major).insert_range(minor_range, MinorDevice::from_ops(ops))
    }

    fn unregister(&mut self, major: u32, minor_range: Range<u32>) -> Result<(), Errno> {
        match self.major_devices.entry(major) {
            Entry::Occupied(major_device) => {
                major_device.into_mut().remove_range(&minor_range);
                Ok(())
            }
            Entry::Vacant(_) => {
                log_error!("No major {} entry registered in the map", major);
                error!(EINVAL)
            }
        }
    }
}

pub struct KernelObjects {
    pub devices: KObjectHandle,
    pub class: KObjectHandle,
    pub block: KObjectHandle,
    pub bus: KObjectHandle,
}

impl KernelObjects {
    /// Returns the virtual bus kobject where all virtual and pseudo devices are stored.
    pub fn virtual_bus(&self) -> Bus {
        Bus::new(self.devices.get_or_create_child("virtual".into(), KObjectDirectory::new), None)
    }

    pub fn get_or_create_bus(&self, name: &FsStr) -> Bus {
        let collection =
            Collection::new(self.bus.get_or_create_child(name, BusCollectionDirectory::new));
        Bus::new(self.devices.get_or_create_child(name, KObjectDirectory::new), Some(collection))
    }

    pub fn get_or_create_class(&self, name: &FsStr, bus: Bus) -> Class {
        let collection =
            Collection::new(self.class.get_or_create_child(name, KObjectSymlinkDirectory::new));
        Class::new(bus.kobject().get_or_create_child(name, KObjectDirectory::new), bus, collection)
    }

    fn create_device<F, N>(
        &self,
        name: &FsStr,
        metadata: DeviceMetadata,
        class: Class,
        create_device_sysfs_ops: F,
    ) -> Device
    where
        F: Fn(Device) -> N + Send + Sync + 'static,
        N: FsNodeOps,
    {
        let class_cloned = class.clone();
        let metadata_cloned = metadata.clone();
        let device_kobject = class.kobject().get_or_create_child(name, move |kobject| {
            create_device_sysfs_ops(Device::new(
                kobject.upgrade().unwrap(),
                class_cloned.clone(),
                metadata_cloned.clone(),
            ))
        });

        // Insert the created device kobject into its subsystems.
        class.collection.kobject().insert_child(device_kobject.clone());
        if metadata.mode == DeviceMode::Block {
            self.block.insert_child(device_kobject.clone());
        }
        if let Some(bus_collection) = &class.bus.collection {
            bus_collection.kobject().insert_child(device_kobject.clone());
        }

        Device::new(device_kobject, class, metadata)
    }
}

impl Default for KernelObjects {
    fn default() -> Self {
        Self {
            devices: KObject::new_root(SYSFS_DEVICES.into()),
            class: KObject::new_root(SYSFS_CLASS.into()),
            block: KObject::new_root_with_dir(SYSFS_BLOCK.into(), KObjectSymlinkDirectory::new),
            bus: KObject::new_root(SYSFS_BUS.into()),
        }
    }
}

/// The kernel's registry of drivers.
pub struct DeviceRegistry {
    pub objects: KernelObjects,
    state: Mutex<DeviceRegistryState>,
}

struct DeviceRegistryState {
    char_devices: MajorDeviceRegistry,
    block_devices: MajorDeviceRegistry,
    dyn_devices: Arc<RwLock<DynRegistry>>,
    next_anon_minor: u32,
    listeners: BTreeMap<u64, Box<dyn DeviceListener>>,
    next_listener_id: u64,
    next_event_id: u64,
}

impl DeviceRegistry {
    fn notify_device<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        device: Device,
    ) where
        L: LockBefore<FileOpsCore>,
    {
        if let Err(err) = devtmpfs_create_device(locked, current_task, device.metadata.clone()) {
            log_warn!("Cannot add device {:?} in devtmpfs ({:?})", device.metadata, err);
        }
        self.dispatch_uevent(UEventAction::Add, device);
    }

    pub fn add_and_register_device<F, N, L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        metadata: DeviceMetadata,
        class: Class,
        create_device_sysfs_ops: F,
        dev_ops: impl DeviceOps,
    ) -> Device
    where
        F: Fn(Device) -> N + Send + Sync + 'static,
        N: FsNodeOps,
        L: LockBefore<FileOpsCore>,
    {
        let device = self.objects.create_device(name, metadata, class, create_device_sysfs_ops);
        if let Err(err) = self.major_devices(device.metadata.mode).register_device(&device, dev_ops)
        {
            log_error!("Cannot register device {:?} ({:?})", device.metadata, err);
        }
        self.notify_device(locked, current_task, device.clone());
        device
    }

    pub fn add_device<F, N, L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        metadata: DeviceMetadata,
        class: Class,
        create_device_sysfs_ops: F,
    ) -> Device
    where
        F: Fn(Device) -> N + Send + Sync + 'static,
        N: FsNodeOps,
        L: LockBefore<FileOpsCore>,
    {
        let device = self.objects.create_device(name, metadata, class, create_device_sysfs_ops);
        self.notify_device(locked, current_task, device.clone());
        device
    }

    pub fn remove_device<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        device: Device,
    ) where
        L: LockBefore<FileOpsCore>,
    {
        let kobject = device.kobject();
        let name = kobject.name();
        device.class.collection.kobject().remove_child(name);
        if let Some(bus_collection) = &device.class.bus.collection {
            bus_collection.kobject().remove_child(name);
        }
        device.kobject().remove();
        self.dispatch_uevent(UEventAction::Remove, device.clone());

        if let Err(err) =
            devtmpfs_remove_node(locked, current_task, device.metadata.devname.as_ref())
        {
            log_error!("Cannot remove device {:?} ({:?})", device, err);
        }
    }

    fn major_devices(&self, mode: DeviceMode) -> MappedMutexGuard<'_, MajorDeviceRegistry> {
        MutexGuard::map(self.state.lock(), |state| match mode {
            DeviceMode::Char => &mut state.char_devices,
            DeviceMode::Block => &mut state.block_devices,
        })
    }

    pub fn register_major(
        &self,
        major: u32,
        device: impl DeviceOps,
        mode: DeviceMode,
    ) -> Result<(), Errno> {
        self.major_devices(mode).register(major, mode.minor_range(), device)
    }

    pub fn unregister_major(&self, major: u32, mode: DeviceMode) -> Result<(), Errno> {
        self.major_devices(mode).unregister(major, mode.minor_range())
    }

    pub fn register_dyn_chrdev(&self, device: impl DeviceOps) -> Result<DeviceType, Errno> {
        self.state.lock().dyn_devices.write().register(device)
    }

    pub fn next_anonymous_dev_id(&self) -> DeviceType {
        let mut state = self.state.lock();
        let id = DeviceType::new(0, state.next_anon_minor);
        state.next_anon_minor += 1;
        id
    }

    /// Register a new listener for uevents on devices.
    ///
    /// Returns a key used to unregister the listener.
    pub fn register_listener(&self, listener: impl DeviceListener + 'static) -> DeviceListenerKey {
        let mut state = self.state.lock();
        let key = state.next_listener_id;
        state.next_listener_id += 1;
        state.listeners.insert(key, Box::new(listener));
        key
    }

    /// Unregister a listener previously registered through `register_listener`.
    pub fn unregister_listener(&self, key: &DeviceListenerKey) {
        self.state.lock().listeners.remove(key);
    }

    /// Dispatch an uevent for the given `device`.
    pub fn dispatch_uevent(&self, action: UEventAction, device: Device) {
        let mut state = self.state.lock();
        let event_id = state.next_event_id;
        state.next_event_id += 1;
        let context = UEventContext { seqnum: event_id };
        for listener in state.listeners.values() {
            listener.on_device_event(action, device.clone(), context);
        }
    }

    /// Opens a device file corresponding to the device identifier `dev`.
    pub fn open_device<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        node: &FsNode,
        flags: OpenFlags,
        id: DeviceType,
        mode: DeviceMode,
    ) -> Result<Box<dyn FileOps>, Errno>
    where
        L: LockBefore<DeviceOpen>,
    {
        let device_ops = self.major_devices(mode).get(id)?;
        let mut locked = locked.cast_locked::<DeviceOpen>();
        device_ops.open(&mut locked, current_task, id, node, flags)
    }
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        let mut state = DeviceRegistryState {
            char_devices: Default::default(),
            block_devices: Default::default(),
            dyn_devices: Default::default(),
            next_anon_minor: 1,
            listeners: Default::default(),
            next_listener_id: 0,
            next_event_id: 0,
        };
        state
            .char_devices
            .register(DYN_MAJOR, DeviceMode::Char.minor_range(), Arc::clone(&state.dyn_devices))
            .expect("Failed to register DYN_MAJOR");
        Self { objects: Default::default(), state: Mutex::new(state) }
    }
}

#[derive(Default)]
struct DynRegistry {
    dyn_devices: BTreeMap<u32, Arc<Box<dyn DeviceOps>>>,
    next_dynamic_minor: u32,
}

impl DynRegistry {
    fn register(&mut self, device: impl DeviceOps) -> Result<DeviceType, Errno> {
        track_stub!(TODO("https://fxbug.dev/322873632"), "emit uevent for dynamic registration");

        let minor = self.next_dynamic_minor;
        if minor > 255 {
            return error!(ENOMEM);
        }
        self.next_dynamic_minor += 1;
        self.dyn_devices.insert(minor, Arc::new(Box::new(device)));
        Ok(DeviceType::new(DYN_MAJOR, minor))
    }
}

impl DeviceOps for Arc<RwLock<DynRegistry>> {
    fn open(
        &self,
        locked: &mut Locked<'_, DeviceOpen>,
        current_task: &CurrentTask,
        id: DeviceType,
        node: &FsNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let device =
            Arc::clone(self.read().dyn_devices.get(&id.minor()).ok_or_else(|| errno!(ENODEV))?);
        device.open(locked, current_task, id, node, flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::mem::{mem_device_init, DevNull};
    use crate::fs::sysfs::DeviceDirectory;
    use crate::testing::*;
    use crate::vfs::*;
    use starnix_uapi::device_type::{INPUT_MAJOR, MEM_MAJOR};

    #[::fuchsia::test]
    fn registry_fails_to_add_duplicate_device() {
        let registry = DeviceRegistry::default();
        registry
            .register_major(MEM_MAJOR, simple_device_ops::<DevNull>, DeviceMode::Char)
            .expect("registers once");
        registry
            .register_major(123, simple_device_ops::<DevNull>, DeviceMode::Char)
            .expect("registers unique");
        registry
            .register_major(MEM_MAJOR, simple_device_ops::<DevNull>, DeviceMode::Char)
            .expect_err("fail to register duplicate");
    }

    #[::fuchsia::test]
    async fn registry_opens_device() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let registry = DeviceRegistry::default();
        registry
            .register_major(MEM_MAJOR, simple_device_ops::<DevNull>, DeviceMode::Char)
            .expect("registers unique");

        let node = FsNode::new_root(PanickingFsNode);

        // Fail to open non-existent device.
        assert!(registry
            .open_device(
                &mut locked,
                &current_task,
                &node,
                OpenFlags::RDONLY,
                DeviceType::NONE,
                DeviceMode::Char
            )
            .is_err());

        // Fail to open in wrong mode.
        assert!(registry
            .open_device(
                &mut locked,
                &current_task,
                &node,
                OpenFlags::RDONLY,
                DeviceType::NULL,
                DeviceMode::Block
            )
            .is_err());

        // Open in correct mode.
        let _ = registry
            .open_device(
                &mut locked,
                &current_task,
                &node,
                OpenFlags::RDONLY,
                DeviceType::NULL,
                DeviceMode::Char,
            )
            .expect("opens device");
    }

    #[::fuchsia::test]
    async fn registry_dynamic_misc() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        fn create_test_device(
            _locked: &mut Locked<'_, DeviceOpen>,
            _current_task: &CurrentTask,
            _id: DeviceType,
            _node: &FsNode,
            _flags: OpenFlags,
        ) -> Result<Box<dyn FileOps>, Errno> {
            Ok(Box::new(PanickingFile))
        }

        let registry = DeviceRegistry::default();
        let device_type = registry.register_dyn_chrdev(create_test_device).unwrap();
        assert_eq!(device_type.major(), DYN_MAJOR);

        let node = FsNode::new_root(PanickingFsNode);
        let _ = registry
            .open_device(
                &mut locked,
                &current_task,
                &node,
                OpenFlags::RDONLY,
                device_type,
                DeviceMode::Char,
            )
            .expect("opens device");
    }

    #[::fuchsia::test]
    async fn registery_add_class() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;

        let input_class =
            registry.objects.get_or_create_class("input".into(), registry.objects.virtual_bus());
        registry.add_device(
            &mut locked,
            &current_task,
            "mice".into(),
            DeviceMetadata::new("mice".into(), DeviceType::new(INPUT_MAJOR, 0), DeviceMode::Char),
            input_class,
            DeviceDirectory::new,
        );

        assert!(registry.objects.class.has_child("input".into()));
        assert!(registry
            .objects
            .class
            .get_child("input".into())
            .and_then(|collection| collection.get_child("mice".into()))
            .is_some());
    }

    #[::fuchsia::test]
    async fn registry_add_bus() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;

        let bus = registry.objects.get_or_create_bus("bus".into());
        let class = registry.objects.get_or_create_class("class".into(), bus);
        registry.add_device(
            &mut locked,
            &current_task,
            "device".into(),
            DeviceMetadata::new("device".into(), DeviceType::new(0, 0), DeviceMode::Char),
            class,
            DeviceDirectory::new,
        );
        assert!(registry.objects.bus.has_child("bus".into()));
        assert!(registry
            .objects
            .bus
            .get_child("bus".into())
            .and_then(|collection| collection.get_child("device".into()))
            .is_some());
    }

    #[::fuchsia::test]
    async fn registry_remove_device() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;

        let pci_bus = registry.objects.get_or_create_bus("pci".into());
        let input_class = registry.objects.get_or_create_class("input".into(), pci_bus);
        let mice_dev = registry.add_device(
            &mut locked,
            &current_task,
            "mice".into(),
            DeviceMetadata::new("mice".into(), DeviceType::new(INPUT_MAJOR, 0), DeviceMode::Char),
            input_class.clone(),
            DeviceDirectory::new,
        );

        registry.remove_device(&mut locked, &current_task, mice_dev);
        assert!(!input_class.kobject().has_child("mice".into()));
        assert!(!registry
            .objects
            .bus
            .get_child("pci".into())
            .expect("get pci collection")
            .has_child("mice".into()));
        assert!(!registry
            .objects
            .class
            .get_child("input".into())
            .expect("get input collection")
            .has_child("mice".into()));
    }

    #[::fuchsia::test]
    async fn registery_unregister_device() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        mem_device_init(&mut locked, &current_task);

        let registry = &kernel.device_registry;
        let node = FsNode::new_root(PanickingFsNode);
        let _ = registry
            .open_device(
                &mut locked,
                &current_task,
                &node,
                OpenFlags::RDONLY,
                DeviceType::NULL,
                DeviceMode::Char,
            )
            .expect("opens device");

        let _ = registry.unregister_major(MEM_MAJOR, DeviceMode::Char);
        assert!(registry
            .open_device(
                &mut locked,
                &current_task,
                &node,
                OpenFlags::RDONLY,
                DeviceType::NULL,
                DeviceMode::Char,
            )
            .is_err());
    }
}
